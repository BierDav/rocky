use super::{Node, Root};
use crate::{
    AccessError, AdminRevocation, Device, DeviceId, Me, Partition, Person, PersonId, Removal,
    TableRights, Timestamp,
    enrollment::{Revoke, now},
    partitions::StoredPartition,
    registry::{Changes, MAX_PARTITIONS, Registry},
};
use rocky_core::Right;
use rocky_credentials::{PublicKey, RevocationMode};
use std::collections::BTreeMap;

fn includes(wider: &[rocky_core::ResourceGrant], narrower: &[rocky_core::ResourceGrant]) -> bool {
    narrower
        .iter()
        .all(|grant| wider.iter().any(|held| held.includes(grant)))
}

fn includes_partitions(
    wider: &[rocky_credentials::PartitionGrant],
    narrower: &[rocky_credentials::PartitionGrant],
) -> bool {
    narrower.iter().all(|grant| {
        wider.iter().any(|held| {
            held.table == grant.table
                && held.selector == grant.selector
                && grant.rights.is_subset(&held.rights)
        })
    })
}

fn seconds(time: u64) -> Timestamp {
    Timestamp::from_unix_micros(i64::try_from(time.saturating_mul(1_000_000)).unwrap_or(i64::MAX))
}

impl Node {
    pub(super) async fn me(&self) -> Result<Me, crate::Failure> {
        let settings = self.settings_snapshot().await;
        let domain = self.domain();
        let held = settings
            .credentials
            .held()
            .ok_or("the device holds no credential")?;
        let granted = crate::access::granted_of(held)?;
        let whole = domain.expand(&granted);
        let partitioned = domain.expand_partitions(&granted);
        let logical = domain.logical();
        let mut tables = BTreeMap::new();
        for (name, scope) in domain.scopes() {
            if name == crate::domain::REGISTRY {
                continue;
            }
            let table_id = logical.table(name).map(|table| logical.table_id(table));
            let rights: Vec<&Right> = whole
                .iter()
                .filter(|grant| grant.scope == scope)
                .flat_map(|grant| &grant.rights)
                .chain(
                    partitioned
                        .iter()
                        .filter(|grant| Some(grant.table) == table_id)
                        .flat_map(|grant| &grant.rights),
                )
                .collect();
            let holds = |right: Right| rights.contains(&&right);
            let table = TableRights {
                read: holds(Right::Read),
                insert: holds(Right::Insert),
                update: holds(Right::Update),
                delete: holds(Right::Delete),
            };
            if table != TableRights::default() {
                tables.insert(name.to_owned(), table);
            }
        }
        let node = rocky_secure::PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let owner = settings.issuer()
            && self
                .registry(&settings)
                .await?
                .get(&settings.identity)
                .and_then(|record| record.admitted.as_ref())
                .is_some_and(|admitted| admitted.invited_by == settings.identity);
        Ok(Me {
            person: PersonId::new(settings.identity),
            device: DeviceId::new(node),
            access: settings.access.clone(),
            admin: settings.issuer(),
            owner,
            tables,
            expires: seconds(settings.expires()),
        })
    }

    async fn administered(
        &self,
    ) -> Result<(crate::enrollment::Settings, Registry), crate::Failure> {
        let settings = (*self.settings_snapshot().await).clone();
        if !settings.issuer() {
            return Err(Box::new(AccessError::AdminRequired));
        }
        let registry = self.registry(&settings).await?;
        Ok((settings, registry))
    }

    fn member(
        settings: &crate::enrollment::Settings,
        registry: &Registry,
        person: PersonId,
    ) -> Result<(), crate::Failure> {
        let identity = person.key();
        if settings.revoked_identity(&identity, registry) {
            return Err(Box::new(AccessError::Removed));
        }
        if !registry.admitted(&identity) {
            return Err(Box::new(AccessError::UnknownPerson));
        }
        Ok(())
    }

    fn ordinary(
        settings: &crate::enrollment::Settings,
        registry: &Registry,
        person: PersonId,
    ) -> Result<(), crate::Failure> {
        Self::member(settings, registry, person)?;
        let identity = person.key();
        if identity == settings.publisher
            || identity == settings.identity
            || registry.trusted.contains_key(&identity)
        {
            return Err(Box::new(AccessError::TargetIsAdmin));
        }
        Ok(())
    }

    pub(super) async fn people(&self) -> Result<Vec<Person>, crate::Failure> {
        let (settings, registry) = self.administered().await?;
        let membership = self.membership().await?;
        let time = now()?;
        let admins = registry.trusted_identities(
            settings.database_context()?,
            &settings.revocations,
            time,
        )?;
        let mut people = Vec::new();
        for (identity, record) in &registry.records {
            let Some(admitted) = &record.admitted else {
                continue;
            };
            if settings.revoked_identity(identity, &registry) {
                continue;
            }
            let devices = membership
                .nodes(identity)
                .filter(|linked| !settings.tombstoned(&linked.node))
                .map(|linked| Device {
                    id: DeviceId::new(linked.node),
                    linked: seconds(linked.linked),
                    last_seen: self.last_seen(linked.node, time).map(seconds),
                })
                .collect();
            let owner = admitted.invited_by == *identity;
            people.push(Person {
                id: PersonId::new(*identity),
                access: admitted.access.clone(),
                admin: owner || admins.contains(identity),
                owner,
                label: record
                    .label
                    .as_ref()
                    .and_then(|label| String::from_utf8(label.clone()).ok()),
                joined: seconds(admitted.joined),
                invited_by: (admitted.invited_by != *identity)
                    .then_some(PersonId::new(admitted.invited_by)),
                invitation: admitted.invitation,
                partitions: record.partitions(),
                devices,
            });
        }
        Ok(people)
    }

    pub(super) async fn set_access(
        &self,
        person: PersonId,
        access: crate::Access,
    ) -> Result<(), crate::Failure> {
        let (settings, registry) = self.administered().await?;
        Self::ordinary(&settings, &registry, person)?;
        let identity = person.key();
        let record = registry.get(&identity).ok_or(AccessError::UnknownPerson)?;
        let admitted = record.admitted.clone().ok_or(AccessError::UnknownPerson)?;
        if admitted.access == access {
            return Ok(());
        }
        let domain = self.domain();
        let held = record.held(&domain)?;
        domain
            .compile(&access, &held)
            .map_err(|error| AccessError::of(&error, &access, &record.partitions()))?;
        if !held.is_empty() && !matches!(access, crate::Access::Role(_)) {
            return Err(Box::new(AccessError::PartitionsHeld));
        }
        let before = domain.compile(&admitted.access, &held)?;
        let after = domain.compile(&access, &held)?;
        let widens = includes(&domain.expand(&after), &domain.expand(&before))
            && includes_partitions(
                &domain.expand_partitions(&after),
                &domain.expand_partitions(&before),
            );
        let mut changes = Changes::default();
        changes.admitted(
            &identity,
            &crate::registry::Admitted { access, ..admitted },
            true,
        )?;
        if widens {
            self.commit(changes).await?;
            self.activity
                .record(crate::ActivityKind::RightsChanged { person });
            return Ok(());
        }
        self.change_rights(identity, changes).await
    }

    pub(super) async fn grant_partition(
        &self,
        person: PersonId,
        partition: &Partition,
    ) -> Result<(), crate::Failure> {
        let (settings, registry) = self.administered().await?;
        Self::ordinary(&settings, &registry, person)?;
        let identity = person.key();
        let record = registry.get(&identity).ok_or(AccessError::UnknownPerson)?;
        let access = record.access().cloned().ok_or(AccessError::UnknownPerson)?;
        let stored = StoredPartition::of(partition);
        if record.grants.contains(&stored) {
            return Ok(());
        }
        if record.grants.len() >= MAX_PARTITIONS {
            return Err(Box::new(AccessError::TooManyPartitions {
                limit: MAX_PARTITIONS,
            }));
        }
        let domain = self.domain();
        let target = partition.target();
        let fresh = domain.partition_targets(std::slice::from_ref(&target))?;
        let mut held = record.held(&domain)?;
        held.extend(fresh.iter().cloned());
        let refusal = |error: crate::Failure| {
            AccessError::of(&error, &access, std::slice::from_ref(partition))
        };
        domain.compile(&access, &held).map_err(refusal)?;
        let granted = domain.compile(&access, &fresh).map_err(refusal)?;
        let group = self
            .group_partition(&target, super::partition::Naming::Known)
            .await?;
        let reads_every_member = group.members.iter().all(|member| {
            granted.partitions.iter().any(|grant| {
                grant.table == member.scope.table && grant.rights.contains(&Right::Read)
            })
        });
        if !reads_every_member {
            return Err(Box::new(AccessError::PartialGroup {
                group: partition.group.clone(),
            }));
        }
        let mut changes = Changes::default();
        changes.grant(&identity, &stored, Some(record))?;
        self.commit(changes).await?;
        let nodes = self.identity_nodes(identity).await?;
        let recipients = self.share_with(&group, &nodes).await?;
        self.activity.record(crate::ActivityKind::PartitionShared {
            partition: partition.clone(),
            recipients,
        });
        self.wake_sync();
        Ok(())
    }

    pub(super) async fn revoke_partition(
        &self,
        person: PersonId,
        partition: &Partition,
    ) -> Result<(), crate::Failure> {
        let (settings, registry) = self.administered().await?;
        Self::ordinary(&settings, &registry, person)?;
        let identity = person.key();
        let record = registry.get(&identity).ok_or(AccessError::UnknownPerson)?;
        let stored = StoredPartition::of(partition);
        if !record.grants.contains(&stored) {
            return Ok(());
        }
        let mut changes = Changes::default();
        changes.ungrant(&identity, &stored)?;
        self.change_rights(identity, changes).await
    }

    pub(super) fn role_holders(
        settings: &crate::enrollment::Settings,
        registry: &Registry,
        role: &crate::RoleName,
    ) -> Vec<PersonId> {
        let access = crate::Access::Role(role.clone());
        registry
            .records
            .iter()
            .filter(|(identity, record)| {
                record
                    .admitted
                    .as_ref()
                    .is_some_and(|admitted| admitted.access == access)
                    && !settings.revoked_identity(identity, registry)
            })
            .map(|(identity, _)| PersonId::new(*identity))
            .collect()
    }

    pub(super) async fn narrow(&self, people: &[PersonId]) -> Result<(), crate::Failure> {
        for person in people {
            self.revoke_with(Revoke::Rights(person.key()), &Root::default())
                .await?;
            self.activity
                .record(crate::ActivityKind::RightsChanged { person: *person });
        }
        if !people.is_empty() {
            self.wake_sync();
        }
        Ok(())
    }

    async fn change_rights(
        &self,
        identity: PublicKey,
        changes: Changes,
    ) -> Result<(), crate::Failure> {
        self.commit(changes).await?;
        self.revoke_with(Revoke::Rights(identity), &Root::default())
            .await?;
        self.activity.record(crate::ActivityKind::RightsChanged {
            person: PersonId::new(identity),
        });
        self.wake_sync();
        Ok(())
    }

    pub(super) async fn remove(
        &self,
        person: PersonId,
        removal: Removal,
        root: &Root,
    ) -> Result<(), crate::Failure> {
        let (settings, registry) = self.administered().await?;
        let identity = person.key();
        if settings.revoked_identity(&identity, &registry) && removal == Removal::Departed {
            return Err(Box::new(AccessError::Removed));
        }
        if registry.trusted.contains_key(&identity) && root.signer().is_none() {
            return Err(Box::new(AccessError::OwnerRequired));
        }
        let mode = match removal {
            Removal::Departed => RevocationMode::Lost,
            Removal::Compromised => RevocationMode::Compromised,
        };
        let target = if registry.trusted.contains_key(&identity) || removal == Removal::Compromised
        {
            Revoke::TrustedIdentity(identity, mode)
        } else {
            Revoke::Identity(identity)
        };
        self.revoke_with(target, root).await?;
        self.activity
            .record(crate::ActivityKind::IdentityRevoked { identity });
        Ok(())
    }

    pub(super) async fn make_admin(
        &self,
        person: PersonId,
        root: &Root,
    ) -> Result<(), crate::Failure> {
        let (settings, registry) = self.administered().await?;
        Self::member(&settings, &registry, person)?;
        if root.signer().is_none() {
            return Err(Box::new(AccessError::OwnerRequired));
        }
        let identity = person.key();
        self.trust_identity(identity, root).await?;
        self.activity
            .record(crate::ActivityKind::RightsChanged { person });
        self.wake_sync();
        Ok(())
    }

    pub(super) async fn revoke_admin(
        &self,
        person: PersonId,
        revocation: AdminRevocation,
        root: &Root,
    ) -> Result<(), crate::Failure> {
        let (_, registry) = self.administered().await?;
        if root.signer().is_none() {
            return Err(Box::new(AccessError::OwnerRequired));
        }
        if !registry.trusted.contains_key(&person.key()) {
            return Err(Box::new(AccessError::NotAdmin));
        }
        match revocation {
            AdminRevocation::Remove(removal) => self.remove(person, removal, root).await,
            AdminRevocation::KeepAsFull => Err(Box::new(AccessError::KeepAsFullUnsupported)),
        }
    }

    pub(super) async fn create_named_partition(
        &self,
        partition: &Partition,
    ) -> Result<Partition, crate::Failure> {
        if !self.settings.lock().await.issuer() {
            return Err(Box::new(AccessError::AdminRequired));
        }
        self.create_partition(&partition.target())
            .await?
            .partition()
    }
}
