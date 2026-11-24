use super::{Node, Root};
use crate::{
    AccessError, Device, DeviceId, Me, Partition, Person, PersonId, Removal, TableRights,
    Timestamp,
    enrollment::{Revoke, RightsChange, now},
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reach {
    Wider,
    Narrower,
}

fn seconds(time: u64) -> Timestamp {
    Timestamp::from_unix_micros(i64::try_from(time.saturating_mul(1_000_000)).unwrap_or(i64::MAX))
}

impl Node {
    fn table_rights(
        &self,
        settings: &crate::enrollment::Settings,
    ) -> Result<BTreeMap<String, TableRights>, crate::Failure> {
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
        Ok(tables)
    }

    pub(crate) async fn statement_refusal(&self, sql: &str) -> Option<crate::Error> {
        use rocky_storage::statement::Kind;
        let analyzed = rocky_storage::statement::analyze(sql, |_| None).ok()?;
        let domain = self.domain();
        let written = match analyzed.kind {
            Kind::Read => None,
            _ => rocky_storage::partition_sql::written_table(sql, &domain.logical().views),
        };
        let mut needed = Vec::new();
        if let Some(written) = &written {
            match analyzed.kind {
                Kind::Insert => needed.push((written.clone(), crate::Right::Append)),
                Kind::Delete => needed.push((written.clone(), crate::Right::Write)),
                Kind::Update => needed.extend(
                    rocky_storage::statement::updated_columns(sql)
                        .into_iter()
                        .map(|column| (written.clone(), crate::Right::Update(column))),
                ),
                Kind::Read => {}
            }
        }
        needed.extend(
            analyzed
                .tables
                .into_iter()
                .filter(|table| {
                    written
                        .as_ref()
                        .is_none_or(|written| !written.eq_ignore_ascii_case(table))
                })
                .map(|table| (table, crate::Right::Read)),
        );
        self.refusal(needed).await
    }

    pub(crate) async fn allocation_refusal(
        &self,
        table: &str,
        failed: &crate::Error,
    ) -> Option<crate::Error> {
        if self.domain().logical().table(table).is_none() {
            return Some(crate::Error::sql(
                crate::connection::code::ERROR,
                format!("no such table: {table}"),
            ));
        }
        if let Some(refused) = self
            .refusal(vec![(table.to_owned(), crate::Right::Allocate)])
            .await
        {
            return Some(refused);
        }
        let unregistered = match failed.kind() {
            crate::ErrorKind::Denied {
                reason: crate::DenyReason::NotGranted,
                ..
            } => true,
            crate::ErrorKind::Sql { code, .. } => *code == crate::connection::code::AUTH,
            _ => false,
        };
        if !unregistered {
            return None;
        }
        let domain = self.domain();
        let definition = domain.logical().table(table)?;
        let settings = self.settings_snapshot().await;
        let reason = match definition.group() {
            Some(group) if Self::per_partition(&domain, &settings, group) => {
                crate::DenyReason::PartitionNotGranted {
                    group: group.to_owned(),
                }
            }
            Some(group) => crate::DenyReason::PartitionNotCreated {
                group: group.to_owned(),
            },
            None => crate::DenyReason::NotGranted,
        };
        Some(crate::Error::denied(
            Some(definition.sql_name.clone()),
            crate::Right::Allocate,
            reason,
        ))
    }

    pub(crate) async fn partition_refusal(&self, error: crate::Error) -> crate::Error {
        let crate::ErrorKind::Denied {
            table: Some(table),
            need,
            reason: crate::DenyReason::NotGranted,
        } = error.kind()
        else {
            return error;
        };
        let domain = self.domain();
        let Some(group) = domain
            .logical()
            .table(table)
            .and_then(|table| table.group())
        else {
            return error;
        };
        let settings = self.settings_snapshot().await;
        if !Self::per_partition(&domain, &settings, group) {
            return error;
        }
        crate::Error::denied(
            Some(table.clone()),
            need.clone(),
            crate::DenyReason::PartitionNotGranted {
                group: group.to_owned(),
            },
        )
    }

    fn per_partition(
        domain: &crate::domain::Domain,
        settings: &crate::enrollment::Settings,
        group: &str,
    ) -> bool {
        let crate::Access::Role(role) = &settings.access else {
            return false;
        };
        domain
            .logical()
            .role(role.as_str())
            .is_some_and(|definition| definition.per_partition.iter().any(|named| named == group))
    }

    fn column_updatable(
        domain: &crate::domain::Domain,
        settings: &crate::enrollment::Settings,
        table: &str,
        column: &str,
    ) -> bool {
        let crate::Access::Role(role) = &settings.access else {
            return true;
        };
        domain
            .logical()
            .role(role.as_str())
            .is_some_and(|definition| {
                definition
                    .tables
                    .iter()
                    .filter(|entry| entry.table.eq_ignore_ascii_case(table))
                    .any(|entry| match &entry.access {
                        rocky_core::TableAccess::Write => true,
                        rocky_core::TableAccess::UpdateColumns(columns) => columns
                            .iter()
                            .any(|allowed| allowed.eq_ignore_ascii_case(column)),
                        rocky_core::TableAccess::Read | rocky_core::TableAccess::Append => false,
                    })
            })
    }

    async fn refusal(&self, needed: Vec<(String, crate::Right)>) -> Option<crate::Error> {
        let settings = self.settings_snapshot().await;
        let domain = self.domain();
        let rights = self.table_rights(&settings).ok()?;
        for (table, need) in needed {
            let Some(definition) = domain.logical().table(&table) else {
                continue;
            };
            let name = definition.sql_name.clone();
            if settings.access == crate::Access::Store {
                return Some(crate::Error::denied(
                    Some(name),
                    need,
                    crate::DenyReason::StoreOnly,
                ));
            }
            let held = rights
                .iter()
                .find(|(named, _)| named.eq_ignore_ascii_case(&name))
                .map(|(_, rights)| *rights)
                .unwrap_or_default();
            let holds = match &need {
                crate::Right::Read => held.read,
                crate::Right::Append | crate::Right::Allocate => held.insert,
                crate::Right::Write => held.delete,
                crate::Right::Update(column) => {
                    held.update && Self::column_updatable(&domain, &settings, &name, column)
                }
                crate::Right::Administer | crate::Right::Sign => true,
            };
            if holds {
                continue;
            }
            let reason = match definition.group() {
                Some(group) if Self::per_partition(&domain, &settings, group) => {
                    crate::DenyReason::PartitionNotGranted {
                        group: group.to_owned(),
                    }
                }
                _ => crate::DenyReason::NotGranted,
            };
            return Some(crate::Error::denied(Some(name), need, reason));
        }
        None
    }

    pub(super) async fn me(&self) -> Result<Me, crate::Failure> {
        let settings = self.settings_snapshot().await;
        let tables = self.table_rights(&settings)?;
        let node = rocky_secure::PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        Ok(Me {
            person: PersonId::new(settings.identity),
            device: DeviceId::new(node),
            access: settings.access.clone(),
            admin: settings.issuer(),
            owner: settings.issuer() && settings.owns(),
            tables,
            expires: seconds(settings.expires()),
        })
    }

    pub(crate) async fn withheld_tables(
        &self,
    ) -> Result<BTreeMap<String, (String, rocky_core::Blocker)>, crate::Failure> {
        let settings = self.settings_snapshot().await;
        let mut withheld = BTreeMap::new();
        let crate::Access::Role(role) = &settings.access else {
            return Ok(withheld);
        };
        let domain = self.domain();
        let logical = domain.logical();
        let Some(definition) = logical
            .role(role.as_str())
            .filter(|definition| !definition.per_partition.is_empty())
        else {
            return Ok(withheld);
        };
        let Some(held) = settings.credentials.held() else {
            return Ok(withheld);
        };
        let partitioned = domain.expand_partitions(&crate::access::granted_of(held)?);
        for entry in &definition.tables {
            let Some(table) = logical.table(&entry.table) else {
                continue;
            };
            let Some(group) = table
                .group()
                .filter(|group| definition.per_partition.iter().any(|named| named == group))
            else {
                continue;
            };
            let id = logical.table_id(table);
            if partitioned.iter().any(|grant| grant.table == id) {
                continue;
            }
            let Some(scope) = domain
                .scopes()
                .find(|(name, _)| name.eq_ignore_ascii_case(&table.sql_name))
                .map(|(_, scope)| scope)
            else {
                continue;
            };
            withheld.insert(
                table.sql_name.to_ascii_lowercase(),
                (
                    table.sql_name.clone(),
                    rocky_core::Blocker {
                        scope: rocky_core::scope_label(&scope),
                        kind: rocky_core::BlockerKind::PartitionNotGranted {
                            group: group.to_owned(),
                        },
                        clears_by: rocky_core::ClearsBy::Application,
                        since: None,
                    },
                ),
            );
        }
        Ok(withheld)
    }

    async fn administered(
        &self,
    ) -> Result<(crate::enrollment::Settings, Registry), crate::Failure> {
        let settings = (*self.settings_snapshot().await).clone();
        if !settings.issuer() {
            return Err(Box::new(crate::Error::refused(
                crate::Right::Administer,
                crate::DenyReason::AdminRequired,
            )));
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
            return Err(Box::new(crate::Error::removed()));
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
                    label: record.device_labels.get(&linked.node).cloned(),
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
        let reach = if widens {
            Reach::Wider
        } else {
            Reach::Narrower
        };
        self.change_rights(identity, changes, reach).await
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
        let fresh = if record.grants.contains(&stored) {
            None
        } else {
            Some(self.grantable(record, &access, partition)?)
        };
        let group = self
            .group_partition(&partition.target(), super::partition::Naming::Known)
            .await?;
        if let Some(granted) = fresh {
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
        }
        let nodes = self.identity_nodes(identity).await?;
        let recipients = self.share_with(&group, &nodes, true).await?;
        self.activity.record(crate::ActivityKind::PartitionShared {
            partition: partition.clone(),
            recipients,
        });
        self.wake_sync();
        Ok(())
    }

    fn grantable(
        &self,
        record: &crate::registry::IdentityRecord,
        access: &crate::Access,
        partition: &Partition,
    ) -> Result<rocky_credentials::Granted, crate::Failure> {
        if record.grants.len() >= MAX_PARTITIONS {
            return Err(Box::new(AccessError::TooManyPartitions {
                limit: MAX_PARTITIONS,
            }));
        }
        let domain = self.domain();
        let fresh = domain.partition_targets(std::slice::from_ref(&partition.target()))?;
        let mut held = record.held(&domain)?;
        held.extend(fresh.iter().cloned());
        let refusal = |error: crate::Failure| -> crate::Failure {
            Box::new(AccessError::of(
                &error,
                access,
                std::slice::from_ref(partition),
            ))
        };
        domain.compile(access, &held).map_err(refusal)?;
        domain.compile(access, &fresh).map_err(refusal)
    }

    pub(super) async fn deliver_grants_to_new_devices(&self) -> Result<(), crate::Failure> {
        let settings = self.settings_snapshot().await;
        if !settings.issuer() {
            return Ok(());
        }
        let registry = self.registry(&settings).await?;
        let membership = self.membership().await?;
        let served = self
            .granted_devices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for (identity, record) in &registry.records {
            if record.grants.is_empty()
                || record.admitted.is_none()
                || registry.trusted.contains_key(identity)
                || settings.revoked_identity(identity, &registry)
            {
                continue;
            }
            let seen = settings
                .authority
                .iter()
                .flat_map(|issuing| &issuing.seen)
                .filter(|(_, seen)| seen.identity == *identity)
                .map(|(node, _)| *node);
            let unserved: std::collections::BTreeSet<PublicKey> = membership
                .nodes(identity)
                .map(|linked| linked.node)
                .chain(seen)
                .filter(|node| !served.contains(node) && !settings.tombstoned(node))
                .collect();
            if unserved.is_empty() {
                continue;
            }
            let mut delivered = true;
            for partition in record.partitions() {
                match self.share_new(&partition, &unserved).await {
                    Ok(0) => {}
                    Ok(recipients) => {
                        self.activity.record(crate::ActivityKind::PartitionShared {
                            partition,
                            recipients,
                        });
                        self.wake_sync();
                    }
                    Err(error) => {
                        delivered = false;
                        self.activity
                            .record(crate::ActivityKind::PartitionShareFailed {
                                partition: Some(partition),
                                error: error.to_string(),
                            });
                    }
                }
            }
            if delivered {
                self.granted_devices
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend(unserved);
            }
        }
        Ok(())
    }

    async fn share_new(
        &self,
        partition: &Partition,
        nodes: &std::collections::BTreeSet<PublicKey>,
    ) -> Result<usize, crate::Failure> {
        let group = self
            .group_partition(&partition.target(), super::partition::Naming::Known)
            .await?;
        self.share_with(&group, nodes, false).await
    }

    pub(super) async fn settle_grant_deliveries(&self) {
        if let Err(error) = self.deliver_grants_to_new_devices().await {
            self.activity
                .record(crate::ActivityKind::PartitionShareFailed {
                    partition: None,
                    error: error.to_string(),
                });
        }
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
        self.change_rights(identity, changes, Reach::Narrower).await
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

    pub(super) async fn change_rights_after(
        &self,
        publication: Option<rocky_core::Commitment>,
        version: u32,
        people: &[PersonId],
        reach: Reach,
    ) -> Result<(), crate::Failure> {
        if people.is_empty() {
            return Ok(());
        }
        self.complete_rights_change().await?;
        let people = people.iter().map(PersonId::key).collect();
        let exposed = match reach {
            Reach::Wider => None,
            Reach::Narrower => {
                let settings = self.settings_snapshot().await;
                Some(settings.exposure(&self.registry(&settings).await?, &people)?)
            }
        };
        let change = RightsChange {
            publication,
            version,
            people,
            exposed,
        };
        self.configure(|settings| settings.change_rights_after(change))
            .await
    }

    pub(super) async fn complete_rights_change(&self) -> Result<(), crate::Failure> {
        let settings = self.settings_snapshot().await;
        let Some(change) = settings
            .authority
            .as_ref()
            .and_then(|authority| authority.rights_change.clone())
        else {
            return Ok(());
        };
        let published = match change.publication {
            Some(publication) => self.replica.schema_stage_recorded(publication).await?,
            None => true,
        };
        if published && settings.schema.version < change.version {
            return Ok(());
        }
        let registry = self.registry(&settings).await?;
        let membership = self.membership().await?;
        let begun = self
            .rotate_revoked(|draft| {
                draft.begin_rights_change(
                    &self.vault,
                    change.publication,
                    published,
                    (&registry, &membership),
                )
            })
            .await?;
        if begun || (published && change.exposed.is_none()) {
            for person in &change.people {
                self.activity.record(crate::ActivityKind::RightsChanged {
                    person: PersonId::new(*person),
                });
            }
            self.wake_sync();
        }
        Ok(())
    }

    pub(super) fn check_fault(
        &self,
        stage: crate::database::fault::Stage,
    ) -> Result<(), crate::Failure> {
        crate::database::fault::check(self.path.parent().unwrap_or(&self.path), stage)
    }

    pub(super) async fn settle_rights_change(&self) {
        if let Err(error) = self.complete_rights_change().await {
            self.activity
                .record(crate::ActivityKind::RightsChangeFailed {
                    error: error.to_string(),
                });
        }
    }

    async fn change_rights(
        &self,
        identity: PublicKey,
        changes: Changes,
        reach: Reach,
    ) -> Result<(), crate::Failure> {
        let version = self.settings_snapshot().await.schema.version;
        self.change_rights_after(None, version, &[PersonId::new(identity)], reach)
            .await?;
        self.check_fault(crate::database::fault::Stage::RightsChangeRecorded)?;
        self.commit(changes).await?;
        self.check_fault(crate::database::fault::Stage::RightsChangeCommitted)?;
        self.complete_rights_change().await
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
            return Err(Box::new(crate::Error::removed()));
        }
        if registry.trusted.contains_key(&identity) && root.signer().is_none() {
            return Err(Box::new(crate::Error::refused(
                crate::Right::Administer,
                crate::DenyReason::OwnerRequired,
            )));
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
            return Err(Box::new(crate::Error::refused(
                crate::Right::Administer,
                crate::DenyReason::OwnerRequired,
            )));
        }
        let version = self.settings_snapshot().await.schema.version;
        self.change_rights_after(None, version, &[person], Reach::Wider)
            .await?;
        self.check_fault(crate::database::fault::Stage::RightsChangeRecorded)?;
        self.trust_identity(person.key(), root).await?;
        self.check_fault(crate::database::fault::Stage::RightsChangeCommitted)?;
        self.complete_rights_change().await
    }

    pub(super) async fn revoke_admin(
        &self,
        person: PersonId,
        removal: Removal,
        root: &Root,
    ) -> Result<(), crate::Failure> {
        let (_, registry) = self.administered().await?;
        if root.signer().is_none() {
            return Err(Box::new(crate::Error::refused(
                crate::Right::Administer,
                crate::DenyReason::OwnerRequired,
            )));
        }
        if !registry.trusted.contains_key(&person.key()) {
            return Err(Box::new(AccessError::NotAdmin));
        }
        self.remove(person, removal, root).await
    }

    pub(super) async fn create_named_partition(
        &self,
        partition: &Partition,
    ) -> Result<Partition, crate::Failure> {
        if !self.settings.lock().await.issuer() {
            return Err(Box::new(crate::Error::refused(
                crate::Right::Administer,
                crate::DenyReason::AdminRequired,
            )));
        }
        self.create_partition(&partition.target())
            .await?
            .partition()
    }
}
