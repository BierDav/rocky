use crate::{Failure, domain::Domain, secrets::Vault};
use data_encoding::HEXLOWER;
use rocky_core::{
    Commitment, PartitionGroup, PartitionTuple, PartitionValue, ResourceScope, SqlValue,
};
use rocky_credentials::derive_table_key;
use rocky_secure::KeyMaterial;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_GROUP_EPOCHS: usize = 256;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupRecord {
    pub group: String,
    pub values: Vec<PartitionValue>,
    #[serde(with = "rocky_core::byte_arrays::one")]
    pub address: Commitment,
    #[serde(with = "rocky_core::byte_arrays::one")]
    pub name: rocky_core::PartitionName,
    pub epochs: Vec<rocky_core::KeyEpoch>,
}
impl GroupRecord {
    pub fn current(&self) -> Option<rocky_core::KeyEpoch> {
        self.epochs.last().copied()
    }
    pub fn admit(&self, epoch: rocky_core::KeyEpoch) -> Result<(), Failure> {
        if !epoch.valid() {
            return Err("invalid group secret epoch".into());
        }
        if !self.epochs.contains(&epoch) && self.epochs.len() >= MAX_GROUP_EPOCHS {
            return Err("group secret epoch capacity reached".into());
        }
        Ok(())
    }
    pub fn partition(&self) -> Result<crate::Partition, Failure> {
        Ok(crate::Partition::new(
            self.group.clone(),
            self.values
                .iter()
                .map(PartitionValue::value)
                .collect::<Result<Vec<SqlValue>, _>>()?,
        ))
    }
    pub fn hold(&mut self, epoch: rocky_core::KeyEpoch) -> Result<(), Failure> {
        self.admit(epoch)?;
        if !self.epochs.contains(&epoch) {
            self.epochs.push(epoch);
            self.epochs.sort_unstable();
        }
        Ok(())
    }
}
pub(crate) const NAMING_ROLE: &str = "partition-naming";
pub(crate) fn naming_key(vault: &Vault) -> Result<[u8; 32], Failure> {
    match vault.find(NAMING_ROLE)? {
        Some(root) => Ok(root),
        None => Ok(vault.get("node")?),
    }
}
pub(crate) fn record_key(address: &Commitment) -> String {
    HEXLOWER.encode(address)
}
pub(crate) fn secret_role(address: &Commitment, epoch: rocky_core::KeyEpoch) -> String {
    format!(
        "group.{}.{}.{}",
        record_key(address),
        epoch.generation,
        HEXLOWER.encode(&epoch.secret_hash)
    )
}

#[derive(Clone, Debug)]
pub(crate) struct Member {
    pub table: String,
    pub scope: ResourceScope,
    pub tuple: PartitionTuple,
}
#[derive(Clone)]
pub(crate) struct GroupPartition {
    pub record: GroupRecord,
    pub members: Vec<Member>,
}
impl GroupPartition {
    pub fn resolve(
        domain: &Domain,
        group: Option<&str>,
        table: Option<&str>,
        values: Vec<SqlValue>,
        name: impl FnOnce(&Commitment) -> Result<rocky_core::PartitionName, Failure>,
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
            name: name(&address.commitment)?,
            group: address.group,
            values: address.values,
            address: address.commitment,
            epochs: Vec::new(),
        };
        Self::members(domain, record, found)
    }
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
                    scope: tuple.scope(domain.scope, logical.table_id(table), &record.name)?,
                    tuple,
                })
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        Ok(Self { record, members })
    }
    pub fn member(&self, table: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.table == table)
    }
    pub fn key(
        &self,
        vault: &Vault,
        member: &Member,
        epoch: rocky_core::KeyEpoch,
    ) -> Result<KeyMaterial, Failure> {
        let secret = vault.get(&secret_role(&self.record.address, epoch))?;
        if rocky_core::KeyEpoch::new(epoch.generation, &secret) != epoch {
            return Err("group secret epoch identity mismatch".into());
        }
        Ok(KeyMaterial::at(
            member.scope,
            epoch,
            derive_table_key(
                &secret,
                member.scope.domain,
                self.record.address,
                member.scope.table,
                epoch.generation,
            )?,
        ))
    }
}

#[cfg(test)]
mod tests;
