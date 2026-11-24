use super::*;
use crate::groups::{GroupPartition, record_key, secret_role};
use rocky_core::TableClass;
use rocky_credentials::GroupSecretEnvelope;
use std::collections::BTreeSet;
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Naming {
    Known,
    Create,
    Sealed(rocky_core::PartitionName),
}
impl Node {
    async fn offered_group_name(
        &self,
        partition: &GroupPartition,
    ) -> Result<Option<rocky_core::PartitionName>, Failure> {
        let mut name = None;
        for member in &partition.members {
            if let Some((_, offered)) = self
                .replica
                .registered_partition(member.scope.domain, member.scope.table, &member.tuple)
                .await?
            {
                if name.is_some_and(|name| name != offered) {
                    return Err(refused("conflicting offered group names"));
                }
                name = Some(offered);
            }
        }
        Ok(name)
    }
    pub(super) async fn group_partition(
        &self,
        target: &crate::PartitionTarget,
        naming: Naming,
    ) -> Result<GroupPartition, Failure> {
        let (group, table, values) = target.parts();
        let known = self.settings.lock().await.groups.clone();
        let naming_key = crate::groups::naming_key(&self.vault)?;
        let issuer = self.vault.find(crate::groups::NAMING_ROLE)?.is_some();
        let unnamed =
            GroupPartition::resolve(&self.domain(), group, table, values.to_vec(), |_| {
                Ok([0; 32])
            })?;
        let offered = if naming == Naming::Known
            && !issuer
            && !known.contains_key(&record_key(&unnamed.record.address))
        {
            self.offered_group_name(&unnamed).await?
        } else {
            None
        };
        GroupPartition::resolve(&self.domain(), group, table, values.to_vec(), |address| {
            match known.get(&record_key(address)) {
                Some(record) => Ok(record.name),
                None if let Naming::Sealed(name) = naming => Ok(name),
                None if naming == Naming::Create || issuer => {
                    Ok(rocky_secure::created_name(&naming_key, address))
                }
                None => offered.ok_or_else(|| refused("this device does not know the partition: its group secret or a custody offer names it")),
            }
        })
    }
    async fn unknown_partition(
        &self,
        target: &crate::PartitionTarget,
    ) -> Result<Option<GroupPartition>, Failure> {
        let (group, table, values) = target.parts();
        let unnamed =
            GroupPartition::resolve(&self.domain(), group, table, values.to_vec(), |_| {
                Ok([0; 32])
            })?;
        let known = self
            .settings
            .lock()
            .await
            .groups
            .contains_key(&record_key(&unnamed.record.address));
        let offered = if known {
            true
        } else {
            self.offered_group_name(&unnamed).await?.is_some()
        };
        Ok((!offered).then_some(unnamed))
    }
    pub(super) async fn envelope_name(
        &self,
        envelope: &GroupSecretEnvelope,
        sender: Option<&CredentialChain>,
    ) -> Result<rocky_core::PartitionName, Failure> {
        let signer = match sender {
            None => self.settings.lock().await.publisher,
            Some(_) => envelope.issuer(),
        };
        Ok(envelope
            .open(
                &PrivateKey::from_bytes(&self.vault.get("node")?),
                signer,
                envelope.group(),
                envelope.epoch(),
            )?
            .1)
    }
    pub(super) async fn partition_address(
        &self,
        table: &str,
        values: Vec<SqlValue>,
    ) -> Result<crate::PartitionAddress, Failure> {
        self.domain()
            .logical()
            .table(table)
            .filter(|table| table.class == TableClass::Replayed && !table.partition_by.is_empty())
            .ok_or("table is not partitioned")?;
        let partition = self
            .group_partition(
                &crate::PartitionTarget::Table {
                    table: table.into(),
                    values,
                },
                Naming::Known,
            )
            .await?;
        let member = partition.member(table).ok_or("table is not partitioned")?;
        Ok(crate::PartitionAddress {
            scope: member.scope,
            tuple: member.tuple.clone(),
            group: partition.record.group.clone(),
            address: record_key(&partition.record.address),
            name: partition.record.name,
        })
    }
    pub(super) fn reads_group(
        &self,
        settings: &Settings,
        partition: &GroupPartition,
    ) -> Result<(), Failure> {
        let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let time = now()?;
        let domain = self.domain();
        let verified = domain.verified(
            &settings.credentials,
            settings.publisher,
            &settings.revocations,
            time,
        )?;
        for member in &partition.members {
            let grants = verified
                .partition(
                    node,
                    member.scope,
                    &member.tuple,
                    &settings.revocations,
                    time,
                )
                .unwrap_or_default();
            if !grants.iter().any(|grant| {
                grant.rows == rocky_core::RowSelection::All
                    && grant.rights.contains(&rocky_core::Right::Read)
            }) {
                return Err(refused(format!(
                    "a group secret needs whole-partition read on every table of group {}; {} lacks it",
                    partition.record.group, member.table
                )));
            }
        }
        Ok(())
    }
    pub(super) async fn hold_keys(
        &self,
        settings: &Settings,
        partitions: &[GroupPartition],
    ) -> Result<(), Failure> {
        let domain = self.domain();
        for partition in partitions {
            for member in &partition.members {
                if !settings.reads_partition(&domain, member) {
                    continue;
                }
                self.replica
                    .register_partition(member.scope, member.tuple.clone(), partition.record.name)
                    .await?;
                for epoch in partition.record.epochs.iter().rev() {
                    self.replica
                        .install_key_epoch(partition.key(&self.vault, member, *epoch)?)
                        .await?;
                }
                crate::enrollment::fault::check(crate::enrollment::fault::Stage::Table)?;
            }
        }
        Ok(())
    }
    async fn know(&self, partition: &GroupPartition) -> Result<(), Failure> {
        let key = record_key(&partition.record.address);
        if self.settings.lock().await.groups.contains_key(&key) {
            return Ok(());
        }
        let record = partition.record.clone();
        self.configure(|settings| {
            settings.groups.entry(key).or_insert(record);
            Ok(())
        })
        .await
    }
    async fn create_group(
        &self,
        partition: GroupPartition,
    ) -> Result<crate::PreparedPartition, Failure> {
        let mut settings = self.settings.lock().await;
        self.reads_group(&settings, &partition)?;
        let key = record_key(&partition.record.address);
        let mut record = settings
            .groups
            .get(&key)
            .cloned()
            .unwrap_or_else(|| partition.record.clone());
        let epoch = match record.current() {
            Some(epoch) => epoch,
            None => {
                let slot = format!("prepared.partition.{}", record_key(&record.address));
                let secret = match self.vault.find(&slot)? {
                    Some(secret) => secret,
                    None => self.vault.create(&slot)?,
                };
                let epoch = rocky_core::KeyEpoch::new(1, &secret);
                record.admit(epoch)?;
                self.vault
                    .put(&secret_role(&record.address, epoch), &secret)?;
                epoch
            }
        };
        record.hold(epoch)?;
        let mut draft = settings.clone();
        draft.groups.insert(key, record.clone());
        let ids = settings
            .revocations
            .statements()
            .map(|signed| signed.statement.id())
            .collect::<Result<Vec<_>, _>>()?;
        draft
            .epoch_statements
            .entry(secret_role(&record.address, epoch))
            .or_insert(ids);
        settings.commit(draft, &self.replica).await?;
        let granted = settings.granted(&self.domain());
        let partition = GroupPartition::of(&self.domain(), &record)?;
        self.hold_keys(&settings, std::slice::from_ref(&partition))
            .await?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = granted;
        Ok(crate::PreparedPartition {
            address: record_key(&record.address),
            epoch,
            name: data_encoding::HEXLOWER.encode(&record.name),
        })
    }
    pub(super) async fn install_envelope(
        &self,
        partition: GroupPartition,
        envelope: &GroupSecretEnvelope,
        sender: Option<&CredentialChain>,
        bootstrap: bool,
    ) -> Result<bool, Failure> {
        let epoch = envelope.epoch();
        let mut settings = self.settings.lock().await;
        let signer = match sender {
            None => settings.publisher,
            Some(chain) => {
                let signer = envelope.issuer();
                let time = now()?;
                let verified = self.domain().verified(
                    chain,
                    settings.publisher,
                    &settings.revocations,
                    time,
                )?;
                for member in &partition.members {
                    let grants = verified.partition(
                        signer,
                        member.scope,
                        &member.tuple,
                        &settings.revocations,
                        time,
                    )?;
                    if !grants.iter().any(|grant| {
                        grant.rows == rocky_core::RowSelection::All
                            && grant.rights.contains(&rocky_core::Right::Read)
                    }) {
                        return Err(refused("envelope signer lacks whole-group read"));
                    }
                }
                signer
            }
        };
        let (secret, name) = envelope.open(
            &PrivateKey::from_bytes(&self.vault.get("node")?),
            signer,
            partition.record.address,
            epoch,
        )?;
        if name != partition.record.name {
            return Err(refused("the envelope names the partition differently"));
        }
        self.reads_group(&settings, &partition)?;
        let key = record_key(&partition.record.address);
        let mut record = settings
            .groups
            .get(&key)
            .cloned()
            .unwrap_or_else(|| partition.record.clone());
        record.admit(epoch)?;
        let role = secret_role(&record.address, epoch);
        let held = self.vault.find(&role)?;
        if held.is_some_and(|held| held != secret) {
            return Err(refused(
                "group secret epoch already holds different material",
            ));
        }
        if held.is_some()
            && record.epochs.contains(&epoch)
            && settings.epoch_statements.get(&role).map(Vec::as_slice)
                == Some(envelope.statements())
        {
            return Ok(false);
        }
        if bootstrap {
            self.bootstrap_intent
                .add_scopes(
                    &partition
                        .members
                        .iter()
                        .map(|member| member.scope)
                        .collect::<Vec<_>>(),
                )
                .await?;
        }
        crate::delivery::fault::check(
            PrivateKey::from_bytes(&self.vault.get("node")?).public_key(),
            crate::delivery::fault::Stage::Prepared,
        )?;
        if held.is_none() {
            self.vault.put(&role, &secret)?;
        }
        record.hold(epoch)?;
        let mut draft = settings.clone();
        draft.groups.insert(key, record.clone());
        if draft
            .epoch_statements
            .get(&role)
            .is_some_and(|held| held != envelope.statements())
        {
            return Err("group epoch revocation IDs differ from retained metadata".into());
        }
        draft
            .epoch_statements
            .insert(role, envelope.statements().to_vec());
        settings.commit(draft, &self.replica).await?;
        let granted = settings.granted(&self.domain());
        let partition = GroupPartition::of(&self.domain(), &record)?;
        self.hold_keys(&settings, std::slice::from_ref(&partition))
            .await?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = granted;
        Ok(true)
    }
    pub(super) async fn share(
        &self,
        partition: GroupPartition,
        recipients: BTreeSet<PublicKey>,
        epoch: Option<rocky_core::KeyEpoch>,
    ) -> Result<crate::PartitionShare, Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let membership = self.membership().await?;
        let mut settings = self.settings.lock().await;
        if !settings.issuer()
            && !recipients
                .iter()
                .all(|recipient| settings.known_issuer(recipient))
        {
            return Err(refused(
                "only an admin shares a group secret with a member; a member shares only with admins",
            ));
        }
        self.reads_group(&settings, &partition)?;
        if settings
            .authority
            .as_ref()
            .is_some_and(|authority| authority.rotation.is_some())
        {
            return Err(refused("a revocation is unfinished; reopen to complete it"));
        }
        let record = settings
            .groups
            .get(&record_key(&partition.record.address))
            .filter(|record| record.current().is_some())
            .cloned()
            .ok_or_else(|| refused("no group secret is held here; create or install it first"))?;
        let current = record.current().ok_or("no group secret epoch")?;
        let epoch = epoch.unwrap_or(current);
        if epoch != current {
            return Err(refused(format!(
                "only the current epoch {current:?} of a group secret is shared"
            )));
        }
        if recipients.len() > 256 {
            return Err(refused("share with at most 256 recipients at once"));
        }
        let own = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        if let Some(revoked) = recipients.iter().find(|recipient| {
            settings.tombstoned(recipient)
                || membership
                    .identity_of(recipient)
                    .is_some_and(|identity| settings.revoked_identity(&identity, &registry))
        }) {
            return Err(refused(format!(
                "recipient {} was revoked",
                revoked.to_hex()
            )));
        }
        if recipients.contains(&own) {
            return Err(refused("this device holds the group secret already"));
        }
        let statements = settings
            .epoch_statements
            .get(&secret_role(&record.address, epoch))
            .cloned()
            .unwrap_or_default();
        let sender = Some(settings.credentials.clone());
        let signer = PrivateKey::from_bytes(&self.vault.get("node")?);
        let secret = self.vault.get(&secret_role(&record.address, epoch))?;
        let mut draft = settings.clone();
        let mut envelopes = Vec::new();
        for recipient in &recipients {
            let envelope = GroupSecretEnvelope::seal_recorded(
                &signer,
                rocky_credentials::GroupSecrets {
                    group: record.address,
                    epoch: epoch.generation,
                    secret,
                    name: record.name,
                },
                *recipient,
                statements.clone(),
            )?;
            envelopes.push(crate::PartitionEnvelope {
                recipient: envelope.recipient(),
                envelope: envelope.to_bytes()?,
            });
            let delivery = crate::delivery::Contents::seal(
                *recipient,
                crate::delivery::Reason::Share,
                &record,
                envelope.to_bytes()?,
                sender.clone(),
            )?;
            draft.queue(*recipient, &record, epoch, &delivery, true)?;
        }
        settings.commit(draft, &self.replica).await?;
        Ok(crate::PartitionShare {
            epoch,
            recipients,
            envelopes,
            sender,
        })
    }
    async fn group_status(
        &self,
        partition: GroupPartition,
    ) -> Result<crate::PartitionStatus, Failure> {
        let address = record_key(&partition.record.address);
        let membership = if self.settings.lock().await.issuer() {
            Some(self.membership().await?)
        } else {
            None
        };
        let (epochs, recipients) = {
            let settings = self.settings.lock().await;
            let recipients = settings
                .deliveries
                .values()
                .filter(|pending| pending.address == address)
                .map(|pending| crate::PartitionDelivery {
                    node: settings
                        .delivery_nodes
                        .values()
                        .chain(settings.peers.keys())
                        .chain(membership.iter().flat_map(|membership| {
                            membership.all().map(|(_, linked)| &linked.node)
                        }))
                        .find(|node| rocky_credentials::PeerId::of(**node) == pending.recipient)
                        .copied(),
                    reference: pending.recipient,
                    epoch: pending.epoch,
                    state: pending.state(),
                })
                .collect();
            let epochs = settings
                .groups
                .get(&address)
                .map(|record| record.epochs.clone())
                .unwrap_or_default();
            (epochs, recipients)
        };
        let holding: BTreeMap<ResourceScope, rocky_secure::Holding> =
            self.replica.subscriptions().await?.into_iter().collect();
        let mut tables = Vec::new();
        for member in &partition.members {
            let epochs = self.replica.key_epochs(member.scope).await;
            tables.push(crate::PartitionTableStatus {
                table: member.table.clone(),
                scope: Some(member.scope),
                current: epochs.iter().copied().max(),
                epochs,
                holding: holding.get(&member.scope).copied(),
            });
        }
        Ok(crate::PartitionStatus {
            group: partition.record.group,
            address,
            current: epochs.iter().copied().max(),
            epochs,
            tables,
            recipients,
        })
    }
    pub(super) async fn set_replica_role(
        &self,
        role: rocky_secure::RoleConfig,
    ) -> Result<(), Failure> {
        let partial = role.role == rocky_secure::ReplicaRole::Partial;
        self.replica.set_role(role).await?;
        if partial {
            let settings = self.settings.lock().await.clone();
            for (_, scope) in self.domain().scopes() {
                if !self.domain().partitioned_table(&scope)
                    && settings.reads_whole(&self.domain(), &scope)
                {
                    self.replica.subscribe(scope).await?;
                }
            }
        }
        self.activity
            .record(crate::ActivityKind::RoleChanged { partial });
        self.wake_sync();
        Ok(())
    }
    pub(super) async fn next_partition(
        &self,
        table: &str,
    ) -> Result<crate::CreatedPartition, Failure> {
        let id = self
            .replica
            .next_partition_identity(table, &crate::groups::naming_key(&self.vault)?)
            .await?;
        let target = crate::PartitionTarget::Table {
            table: table.into(),
            values: vec![id.sql()],
        };
        let partition = self.group_partition(&target, Naming::Create).await?;
        let scope = partition
            .member(table)
            .ok_or("table is not partitioned")?
            .scope;
        let partition = self.create_group(partition).await?;
        self.activity.record(crate::ActivityKind::PartitionCreated {
            target: target.clone(),
            id: Some(id),
        });
        self.wake_sync();
        Ok(crate::CreatedPartition {
            id,
            scope,
            partition,
        })
    }
    pub(super) async fn share_partition(
        &self,
        target: &crate::PartitionTarget,
        recipients: BTreeSet<PublicKey>,
        epoch: Option<rocky_core::KeyEpoch>,
    ) -> Result<crate::PartitionShare, Failure> {
        let partition = self.group_partition(target, Naming::Known).await?;
        let shared = self.share(partition, recipients, epoch).await?;
        self.activity.record(crate::ActivityKind::PartitionShared {
            target: target.clone(),
            recipients: shared.recipients.len(),
        });
        self.wake_sync();
        Ok(shared)
    }
    pub(super) async fn identity_nodes(
        &self,
        identity: PublicKey,
    ) -> Result<BTreeSet<PublicKey>, Failure> {
        let mut nodes: BTreeSet<_> = self
            .membership()
            .await?
            .nodes(&identity)
            .map(|linked| linked.node)
            .collect();
        if let Some(issuing) = self.settings.lock().await.authority.as_ref() {
            nodes.extend(
                issuing
                    .seen
                    .iter()
                    .filter(|(_, seen)| seen.identity == identity)
                    .map(|(node, _)| *node),
            );
        }
        Ok(nodes)
    }
    pub(super) async fn partition_complete(&self, name: &str) -> Result<bool, Failure> {
        let logical = self.domain().logical().clone();
        let table = logical
            .table(name)
            .filter(|table| !table.partition_by.is_empty())
            .ok_or("table is not partitioned")?;
        Ok(self.replica.table_complete(logical.table_id(table)).await?)
    }
    async fn partition_changed(&self) {
        self.wake_sync();
    }
    pub(super) async fn create_partition(
        &self,
        target: &crate::PartitionTarget,
    ) -> Result<crate::PreparedPartition, Failure> {
        if !self.settings.lock().await.issuer() {
            return Err(refused("explicit partition creation requires an issuer"));
        }
        let partition = self.group_partition(target, Naming::Create).await?;
        let prepared = self.create_group(partition).await?;
        self.activity.record(crate::ActivityKind::PartitionCreated {
            target: target.clone(),
            id: None,
        });
        self.partition_changed().await;
        Ok(prepared)
    }
    pub(super) async fn install_partition(
        &self,
        target: &crate::PartitionTarget,
        envelope: &GroupSecretEnvelope,
        sender: &CredentialChain,
    ) -> Result<(), Failure> {
        let name = self.envelope_name(envelope, Some(sender)).await?;
        let partition = self.group_partition(target, Naming::Sealed(name)).await?;
        self.install_envelope(partition, envelope, Some(sender), false)
            .await?;
        self.activity
            .record(crate::ActivityKind::PartitionKeyInstalled {
                target: target.clone(),
            });
        self.partition_changed().await;
        Ok(())
    }
    pub(super) async fn partition_status(
        &self,
        target: &crate::PartitionTarget,
    ) -> Result<crate::PartitionStatus, Failure> {
        if let Some(unknown) = self.unknown_partition(target).await? {
            return Ok(crate::PartitionStatus {
                group: unknown.record.group,
                address: record_key(&unknown.record.address),
                current: None,
                epochs: Vec::new(),
                tables: unknown
                    .members
                    .into_iter()
                    .map(|member| crate::PartitionTableStatus {
                        table: member.table,
                        scope: None,
                        epochs: Vec::new(),
                        current: None,
                        holding: None,
                    })
                    .collect(),
                recipients: Vec::new(),
            });
        }
        self.group_status(self.group_partition(target, Naming::Known).await?)
            .await
    }
    pub(super) async fn register_partition(
        &self,
        target: &crate::PartitionTarget,
        selection: &crate::PartitionSelection,
    ) -> Result<(), Failure> {
        let partition = self
            .group_partition(target, selection.name.map_or(Naming::Known, Naming::Sealed))
            .await?;
        for member in partition.covered(&selection.omit).map_err(refused)? {
            self.replica
                .register_partition(member.scope, member.tuple.clone(), partition.record.name)
                .await?;
        }
        self.know(&partition).await?;
        self.partition_changed().await;
        Ok(())
    }
    pub(super) async fn hold_partition(
        &self,
        target: &crate::PartitionTarget,
        selection: &crate::PartitionSelection,
        bootstrap: bool,
    ) -> Result<(), Failure> {
        let partition = self
            .group_partition(target, selection.name.map_or(Naming::Known, Naming::Sealed))
            .await?;
        self.hold(&partition, &selection.omit, bootstrap).await?;
        self.activity.record(if bootstrap {
            crate::ActivityKind::PartitionBootstrap {
                target: target.clone(),
            }
        } else {
            crate::ActivityKind::PartitionSubscribed {
                target: target.clone(),
            }
        });
        self.partition_changed().await;
        Ok(())
    }
    pub(super) async fn release_partition(
        &self,
        target: &crate::PartitionTarget,
        omit: &BTreeSet<String>,
    ) -> Result<(), Failure> {
        if self.unknown_partition(target).await?.is_none() {
            let partition = self.group_partition(target, Naming::Known).await?;
            for member in partition.released(omit).map_err(refused)? {
                self.replica.unsubscribe(member.scope).await?;
            }
            self.activity
                .record(crate::ActivityKind::PartitionUnsubscribed {
                    target: target.clone(),
                });
            self.partition_changed().await;
        }
        Ok(())
    }
    pub(super) async fn evict_partition(
        &self,
        target: &crate::PartitionTarget,
        omit: &BTreeSet<String>,
    ) -> Result<crate::PartitionEviction, Failure> {
        let mut evicted = crate::PartitionEviction::default();
        if self.unknown_partition(target).await?.is_none() {
            let partition = self.group_partition(target, Naming::Known).await?;
            for member in partition.released(omit).map_err(refused)? {
                let report = self.replica.evict(member.scope).await?;
                evicted.rows += report.rows as u64;
                evicted.operations += report.operations as u64;
                evicted.checkpoints += report.checkpoints;
                evicted.recovery_unconfirmed |= report.recovery_unconfirmed;
            }
            self.activity.record(crate::ActivityKind::PartitionEvicted {
                target: target.clone(),
            });
            self.partition_changed().await;
        }
        Ok(evicted)
    }
    pub(super) async fn hold(
        &self,
        partition: &GroupPartition,
        omit: &BTreeSet<String>,
        bootstrap: bool,
    ) -> Result<(), Failure> {
        let covered = partition.covered(omit).map_err(refused)?;
        for member in &covered {
            self.replica
                .register_partition(member.scope, member.tuple.clone(), partition.record.name)
                .await?;
        }
        self.know(partition).await?;
        for member in &covered {
            self.replica.subscribe(member.scope).await?;
        }
        for member in partition
            .members
            .iter()
            .filter(|member| omit.contains(&member.table))
        {
            self.replica.unsubscribe(member.scope).await?;
        }
        if bootstrap {
            self.bootstrap_partition(&covered).await
        } else {
            Ok(())
        }
    }
    async fn bootstrap_partition(&self, covered: &[&crate::groups::Member]) -> Result<(), Failure> {
        let domain = self.domain();
        let added: Vec<ResourceScope> = {
            let settings = self.settings.lock().await;
            covered
                .iter()
                .filter(|member| settings.reads_partition(&domain, member))
                .map(|member| member.scope)
                .collect()
        };
        self.bootstrap_intent.add_scopes(&added).await
    }
}
