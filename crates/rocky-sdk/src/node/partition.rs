use super::*;
use crate::groups::{GroupPartition, GroupRecord, record_key, secret_role};
use rocky_credentials::GroupSecretEnvelope;
use std::collections::BTreeSet;
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Naming {
    Known,
    Create,
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
                None if naming == Naming::Create || issuer => {
                    Ok(rocky_secure::created_name(&naming_key, address))
                }
                None => offered.ok_or_else(|| refused("this device does not know the partition: its group secret or a custody offer names it")),
            }
        })
    }
    #[cfg(any(test, feature = "test-support"))]
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
    async fn audience_of(
        &self,
        partition: &GroupPartition,
        excluded: &[PublicKey],
    ) -> Result<BTreeSet<PublicKey>, Failure> {
        let settings = self.settings_snapshot().await;
        let own = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let mut audience: BTreeSet<PublicKey> = settings
            .issuers
            .iter()
            .copied()
            .filter(|node| !settings.tombstoned(node))
            .collect();
        if settings.issuer() {
            let registry = self.registry(&settings).await?;
            let membership = self.membership().await?;
            audience.extend(settings.audience(
                own,
                &self.domain(),
                partition,
                (&registry, &membership),
                &[],
            ));
        }
        audience.retain(|node| *node != own && !excluded.contains(node));
        Ok(audience)
    }
    async fn create_group(
        &self,
        partition: GroupPartition,
        id: Option<rocky_core::RowIdentity>,
    ) -> Result<GroupRecord, Failure> {
        let audience = self.audience_of(&partition, &[]).await?;
        let mut settings = self.settings.lock().await;
        self.reads_group(&settings, &partition)?;
        let key = record_key(&partition.record.address);
        let mut record = settings
            .groups
            .get(&key)
            .cloned()
            .unwrap_or_else(|| partition.record.clone());
        let created = record.current().is_none();
        let mut recipients = 0;
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
        if created {
            let secret = self.vault.get(&secret_role(&record.address, epoch))?;
            recipients = draft.queue_group(
                &PrivateKey::from_bytes(&self.vault.get("node")?),
                (&record, epoch, secret),
                &audience,
                crate::delivery::Reason::Share,
                false,
            )?;
        }
        settings.commit(draft, &self.replica).await?;
        let granted = settings.granted(&self.domain());
        let partition = GroupPartition::of(&self.domain(), &record)?;
        self.hold_keys(&settings, std::slice::from_ref(&partition))
            .await?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = granted;
        drop(settings);
        let named = record.partition()?;
        self.activity.record(crate::ActivityKind::PartitionCreated {
            partition: named.clone(),
            id,
        });
        if recipients > 0 {
            self.activity.record(crate::ActivityKind::PartitionShared {
                partition: named,
                recipients,
            });
        }
        self.wake_sync();
        Ok(record)
    }
    pub(super) async fn install_envelope(
        &self,
        partition: GroupPartition,
        envelope: &GroupSecretEnvelope,
        sender: Option<&CredentialChain>,
        bootstrap: bool,
    ) -> Result<bool, Failure> {
        let epoch = envelope.epoch();
        let created_by_member = sender.is_some_and(|chain| {
            !chain
                .held()
                .and_then(|held| held.extended().ok())
                .is_some_and(|extended| extended.issues)
        });
        let forward = if created_by_member && self.settings_snapshot().await.issuer() {
            self.audience_of(&partition, &[envelope.issuer()]).await?
        } else {
            BTreeSet::new()
        };
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
        let forwarded = draft.queue_group(
            &PrivateKey::from_bytes(&self.vault.get("node")?),
            (&record, epoch, secret),
            &forward,
            crate::delivery::Reason::Share,
            false,
        )?;
        settings.commit(draft, &self.replica).await?;
        let granted = settings.granted(&self.domain());
        let partition = GroupPartition::of(&self.domain(), &record)?;
        self.hold_keys(&settings, std::slice::from_ref(&partition))
            .await?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = granted;
        if forwarded > 0 {
            self.activity.record(crate::ActivityKind::PartitionShared {
                partition: record.partition()?,
                recipients: forwarded,
            });
        }
        Ok(true)
    }
    pub(super) async fn share_with(
        &self,
        partition: &GroupPartition,
        recipients: &BTreeSet<PublicKey>,
    ) -> Result<usize, Failure> {
        let mut settings = self.settings.lock().await;
        self.reads_group(&settings, partition)?;
        let record = settings
            .groups
            .get(&record_key(&partition.record.address))
            .filter(|record| record.current().is_some())
            .cloned()
            .ok_or_else(|| refused("no group secret is held here; create it first"))?;
        let epoch = record.current().ok_or("no group secret epoch")?;
        let secret = self.vault.get(&secret_role(&record.address, epoch))?;
        let mut draft = settings.clone();
        let queued = draft.queue_group(
            &PrivateKey::from_bytes(&self.vault.get("node")?),
            (&record, epoch, secret),
            recipients,
            crate::delivery::Reason::Share,
            true,
        )?;
        settings.commit(draft, &self.replica).await?;
        Ok(queued)
    }
    #[cfg(any(test, feature = "test-support"))]
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
        partition.member(table).ok_or("table is not partitioned")?;
        let partition = self.create_group(partition, Some(id)).await?.partition()?;
        Ok(crate::CreatedPartition { id, partition })
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
    pub(super) async fn create_partition(
        &self,
        target: &crate::PartitionTarget,
    ) -> Result<GroupRecord, Failure> {
        if !self.settings.lock().await.issuer() {
            return Err(refused("explicit partition creation requires an issuer"));
        }
        let partition = self.group_partition(target, Naming::Create).await?;
        self.create_group(partition, None).await
    }
    #[cfg(any(test, feature = "test-support"))]
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
    pub(super) async fn hold(
        &self,
        partition: &GroupPartition,
        bootstrap: bool,
    ) -> Result<(), Failure> {
        for member in &partition.members {
            self.replica
                .register_partition(member.scope, member.tuple.clone(), partition.record.name)
                .await?;
        }
        for member in &partition.members {
            self.replica.subscribe(member.scope).await?;
        }
        if bootstrap {
            self.bootstrap_partition(&partition.members).await
        } else {
            Ok(())
        }
    }
    async fn bootstrap_partition(&self, covered: &[crate::groups::Member]) -> Result<(), Failure> {
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
    pub(super) async fn partition_progress(
        &self,
        settings: &Settings,
        blockers: &[rocky_core::Blocker],
    ) -> Result<Vec<crate::PartitionProgress>, Failure> {
        let domain = self.domain();
        let full = self.replica.role().await?.role == rocky_secure::ReplicaRole::Full;
        let mut progress = Vec::new();
        for partition in settings.partitions(&domain)? {
            let held = partition.record.current().is_some()
                && partition
                    .members
                    .iter()
                    .all(|member| settings.reads_partition(&domain, member));
            let mut complete = held;
            for member in &partition.members {
                if !complete {
                    break;
                }
                let holds = match self.replica.subscription(member.scope).await? {
                    Some(holding) => holding == rocky_secure::Holding::Held,
                    None => full,
                };
                let label = rocky_core::scope_label(&member.scope);
                complete = holds && !blockers.iter().any(|blocker| blocker.scope == label);
            }
            progress.push(crate::PartitionProgress {
                partition: partition.record.partition()?,
                held,
                complete,
            });
        }
        Ok(progress)
    }
}
