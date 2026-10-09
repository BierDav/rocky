//! Group partitions: one secret per group partition and key epoch, every
//! member table's key derived from it and the holding rule. See
//! `docs/runtime/partitioned-scopes.md`.
use crate::{Failure, domain::Domain, secrets::Vault};
use data_encoding::HEXLOWER;
use rocky_core::{
    Commitment, PartitionGroup, PartitionTuple, PartitionValue, ResourceScope, SqlValue,
};
use rocky_credentials::derive_table_key;
use rocky_secure::KeyMaterial;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Secret epochs kept per group partition: one per revocation that rotated it.
pub(crate) const MAX_GROUP_EPOCHS: usize = 256;

/// A group partition this installation knows, by its group address.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupRecord {
    pub group: String,
    /// Canonical slot values in declaration order.
    pub values: Vec<PartitionValue>,
    pub address: Commitment,
    /// Secret epochs held, ascending; empty for an address registered
    /// without a key.
    pub epochs: Vec<u64>,
}
impl GroupRecord {
    pub fn current(&self) -> Option<u64> {
        self.epochs.last().copied()
    }
    /// Adds an epoch; refused at capacity before any secret is stored.
    pub fn admit(&self, epoch: u64) -> Result<(), Failure> {
        if !self.epochs.contains(&epoch) && self.epochs.len() >= MAX_GROUP_EPOCHS {
            return Err("group secret epoch capacity reached".into());
        }
        Ok(())
    }
    pub fn hold(&mut self, epoch: u64) -> Result<(), Failure> {
        self.admit(epoch)?;
        if !self.epochs.contains(&epoch) {
            self.epochs.push(epoch);
            self.epochs.sort_unstable();
        }
        Ok(())
    }
}
pub(crate) fn record_key(address: &Commitment) -> String {
    HEXLOWER.encode(address)
}
/// The installation vault's role of a group secret epoch.
pub(crate) fn secret_role(address: &Commitment, epoch: u64) -> String {
    format!("group.{}.{epoch}", record_key(address))
}

/// One member table's partition of a group partition.
#[derive(Clone, Debug)]
pub(crate) struct Member {
    pub table: String,
    pub scope: ResourceScope,
    pub tuple: PartitionTuple,
    /// In-group tables this member references.
    pub parents: Vec<String>,
}
/// A group partition resolved against the current schema generation, so a
/// table that joined the group later is a member too.
pub(crate) struct GroupPartition {
    pub record: GroupRecord,
    pub members: Vec<Member>,
}
impl GroupPartition {
    /// Resolves a group, or the group of `table`, at positional values.
    pub fn resolve(
        domain: &Domain,
        group: Option<&str>,
        table: Option<&str>,
        values: Vec<SqlValue>,
    ) -> Result<Self, Failure> {
        let logical = domain.logical();
        let groups = logical.partition_groups()?;
        let found = match (group, table) {
            (Some(name), None) => groups.iter().find(|group| group.name == name),
            (None, Some(table)) => groups
                .iter()
                .find(|group| group.tables.iter().any(|member| member.sql_name == table)),
            _ => return Err("name a partition group or a partitioned table".into()),
        }
        .ok_or("unknown partition group")?;
        if values.len() != found.slots.len() {
            return Err("partition value count".into());
        }
        let first = found.tables.first().ok_or("empty partition group")?;
        let address = logical.group_address(domain.scope, &first.sql_name, &values)?;
        let record = GroupRecord {
            group: address.group,
            values: address.values,
            address: address.commitment,
            epochs: Vec::new(),
        };
        Self::members(domain, record, found)
    }
    /// The group partition of a stored record under the current schema;
    /// generation admission keeps a group's slots and members.
    pub fn of(domain: &Domain, record: &GroupRecord) -> Result<Self, Failure> {
        let groups = domain.logical().partition_groups()?;
        let group = groups
            .iter()
            .find(|group| group.name == record.group)
            .filter(|group| group.slots.len() == record.values.len())
            .ok_or("a stored group partition is not in the schema")?;
        Self::members(domain, record.clone(), group)
    }
    fn members(
        domain: &Domain,
        record: GroupRecord,
        group: &PartitionGroup<'_>,
    ) -> Result<Self, Failure> {
        let logical = domain.logical();
        let names: BTreeSet<&str> = group
            .tables
            .iter()
            .map(|table| table.sql_name.as_str())
            .collect();
        let members = group
            .tables
            .iter()
            .map(|table| {
                let tuple = PartitionTuple::new(
                    table
                        .partition_by
                        .iter()
                        .zip(&record.values)
                        .map(|(name, value)| {
                            let column = table.column(name).ok_or("unknown partition column")?;
                            Ok((logical.field_id(table, column), value.clone()))
                        })
                        .collect::<Result<Vec<_>, Failure>>()?,
                )?;
                Ok(Member {
                    table: table.sql_name.clone(),
                    scope: tuple.scope(domain.scope, logical.table_id(table))?,
                    tuple,
                    parents: table
                        .foreign_keys
                        .iter()
                        .filter(|key| names.contains(key.table.as_str()))
                        .map(|key| key.table.clone())
                        .collect(),
                })
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        Ok(Self { record, members })
    }
    pub fn member(&self, table: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.table == table)
    }
    /// The members an operation covers when the named tables are left out.
    /// A table left out must not be referenced by one the operation keeps:
    /// that child could never resolve its parent.
    pub fn covered(&self, omit: &BTreeSet<String>) -> Result<Vec<&Member>, Failure> {
        if let Some(unknown) = omit.iter().find(|name| self.member(name).is_none()) {
            return Err(format!("{unknown} is no table of group {}", self.record.group).into());
        }
        let covered: Vec<&Member> = self
            .members
            .iter()
            .filter(|member| !omit.contains(&member.table))
            .collect();
        for member in &covered {
            if let Some(parent) = member.parents.iter().find(|parent| omit.contains(*parent)) {
                return Err(format!(
                    "{} references {parent}; leaving {parent} out of group {} is refused",
                    member.table, self.record.group
                )
                .into());
            }
        }
        if covered.is_empty() {
            return Err("every table of the group is left out".into());
        }
        Ok(covered)
    }
    /// The tables that stay held when the covered ones are released: none
    /// may reference a released table.
    pub fn released(&self, keep: &BTreeSet<String>) -> Result<Vec<&Member>, Failure> {
        if let Some(unknown) = keep.iter().find(|name| self.member(name).is_none()) {
            return Err(format!("{unknown} is no table of group {}", self.record.group).into());
        }
        for member in self
            .members
            .iter()
            .filter(|member| keep.contains(&member.table))
        {
            if let Some(parent) = member.parents.iter().find(|parent| !keep.contains(*parent)) {
                return Err(format!(
                    "{} references {parent}; keeping it without {parent} is refused",
                    member.table
                )
                .into());
            }
        }
        Ok(self
            .members
            .iter()
            .filter(|member| !keep.contains(&member.table))
            .collect())
    }
    /// A member's key at one epoch, derived from the held secret.
    pub fn key(&self, vault: &Vault, member: &Member, epoch: u64) -> Result<KeyMaterial, Failure> {
        let secret = vault.get(&secret_role(&self.record.address, epoch))?;
        Ok(KeyMaterial {
            scope: member.scope,
            generation: epoch,
            key: derive_table_key(
                &secret,
                member.scope.domain,
                self.record.address,
                member.scope.table,
                epoch,
            )?,
        })
    }
}

#[cfg(test)]
mod tests;
