use crate::{Failure, domain::Domain, schema::RoleName};
use rocky_core::{
    FieldSelection, LogicalTable, MAX_GRANTS, PartitionSelection, PartitionSelector,
    PartitionValue, ResourceGrant, RowSelection,
};
use rocky_credentials::{Granted, PartitionGrant, WholeAccess};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Full,
    Read,
    Store,
    Role(RoleName),
}
impl Access {
    fn whole(&self) -> Option<WholeAccess> {
        match self {
            Self::Full => Some(WholeAccess::Full),
            Self::Read => Some(WholeAccess::Read),
            Self::Store => Some(WholeAccess::Store),
            Self::Role(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeldPartition {
    pub(crate) group: String,
    pub(crate) values: Vec<PartitionValue>,
}

#[derive(Debug)]
pub(crate) struct UnknownRole(pub(crate) RoleName);
impl std::fmt::Display for UnknownRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the schema declares no role {}", self.0)
    }
}
impl std::error::Error for UnknownRole {}

pub(crate) fn granted_of(
    capability: &rocky_credentials::Capability,
) -> Result<Granted, rocky_credentials::Error> {
    let extended = capability.extended()?;
    Ok(Granted {
        grants: capability.grants.clone(),
        partitions: extended.partitions,
        whole: extended.whole,
        role: extended.role,
    })
}

impl Domain {
    pub(crate) fn compile(
        &self,
        access: &Access,
        partitions: &BTreeSet<HeldPartition>,
    ) -> Result<Granted, Failure> {
        let Access::Role(name) = access else {
            if !partitions.is_empty() {
                return Err("whole access already holds every partition".into());
            }
            let mut grants = Vec::new();
            if *access == Access::Store {
                grants.push(self.registry_grant(WholeAccess::Store));
            }
            return Ok(Granted {
                grants,
                whole: access.whole(),
                ..Granted::default()
            });
        };
        let logical = self.logical();
        let role = logical
            .role(name.as_str())
            .ok_or_else(|| UnknownRole(name.clone()))?;
        if let Some(stray) = partitions
            .iter()
            .find(|held| !role.per_partition.contains(&held.group))
        {
            return Err(format!(
                "role {name} is not granted per partition of {}",
                stray.group
            )
            .into());
        }
        let mut granted = Granted {
            role: Some(logical.role_id(name.as_str())),
            ..Granted::default()
        };
        for entry in &role.tables {
            let table = logical
                .table(&entry.table)
                .ok_or_else(|| format!("role {name} names unknown table {}", entry.table))?;
            let scope = self
                .scope_of(&table.sql_name)
                .ok_or_else(|| format!("{} has no reader scope", table.sql_name))?;
            let group = table.group();
            let per_partition = group.filter(|group| role.per_partition.iter().any(|g| g == group));
            for grant in entry.access.grants() {
                let fields = match grant.columns {
                    None => FieldSelection::All,
                    Some(columns) => FieldSelection::Only(
                        columns
                            .iter()
                            .map(|column| {
                                table
                                    .column(column)
                                    .map(|column| logical.field_id(table, column))
                                    .ok_or_else(|| format!("unknown column {column}"))
                            })
                            .collect::<Result<_, _>>()?,
                    ),
                };
                match per_partition {
                    None => {
                        if group.is_some() {
                            granted.partitions.push(PartitionGrant {
                                domain: scope.domain,
                                table: scope.table,
                                selector: self.selector(table, |_| PartitionSelection::Any),
                                rows: RowSelection::All,
                                rights: grant.rights.clone(),
                                fields: fields.clone(),
                            });
                        }
                        granted.grants.push(ResourceGrant {
                            scope,
                            rows: RowSelection::All,
                            rights: grant.rights,
                            fields,
                        });
                    }
                    Some(group) => {
                        let held: Vec<_> = partitions
                            .iter()
                            .filter(|held| held.group == group)
                            .collect();
                        let selectors = if table.partition_by.len() == 1 {
                            let values: BTreeSet<_> = held
                                .iter()
                                .filter_map(|held| held.values.first().cloned())
                                .collect();
                            if values.is_empty() {
                                Vec::new()
                            } else {
                                vec![
                                    self.selector(table, |_| {
                                        PartitionSelection::Only(values.clone())
                                    }),
                                ]
                            }
                        } else {
                            held.iter()
                                .map(|held| {
                                    self.selector(table, |slot| {
                                        PartitionSelection::Only(
                                            held.values.get(slot).cloned().into_iter().collect(),
                                        )
                                    })
                                })
                                .collect()
                        };
                        for selector in selectors {
                            granted.partitions.push(PartitionGrant {
                                domain: scope.domain,
                                table: scope.table,
                                selector,
                                rows: RowSelection::All,
                                rights: grant.rights.clone(),
                                fields: fields.clone(),
                            });
                        }
                    }
                }
            }
        }
        for grant in &granted.partitions {
            grant.validate()?;
        }
        let count = granted.grants.len() + granted.partitions.len();
        if count > MAX_GRANTS {
            return Err(Box::new(rocky_core::SchemaError::Role(
                rocky_core::RoleError::TooWide {
                    role: name.to_string(),
                    grants: count,
                },
            )));
        }
        Ok(granted)
    }
    pub(crate) fn reads_table(&self, access: &Access, table: &str) -> bool {
        if table == crate::domain::REGISTRY {
            return false;
        }
        match access {
            Access::Full | Access::Read => true,
            Access::Store => false,
            Access::Role(name) => {
                let logical = self.logical();
                let per_partition = |role: &rocky_core::RoleDefinition| {
                    logical
                        .table(table)
                        .and_then(|table| table.group())
                        .is_some_and(|group| role.per_partition.iter().any(|held| held == group))
                };
                logical.role(name.as_str()).is_some_and(|role| {
                    !per_partition(role) && role.tables.iter().any(|entry| entry.table == table)
                })
            }
        }
    }
    pub(crate) fn administer(&self) -> Granted {
        Granted {
            grants: vec![self.registry_grant(WholeAccess::Full)],
            whole: Some(WholeAccess::Full),
            ..Granted::default()
        }
    }
    fn registry_grant(&self, whole: WholeAccess) -> ResourceGrant {
        ResourceGrant {
            scope: self.registry(),
            rows: RowSelection::All,
            rights: whole.rights(),
            fields: whole.fields(),
        }
    }
    fn selector(
        &self,
        table: &LogicalTable,
        selection: impl Fn(usize) -> PartitionSelection,
    ) -> PartitionSelector {
        let logical = self.logical();
        PartitionSelector(
            table
                .partition_by
                .iter()
                .filter_map(|name| table.column(name))
                .enumerate()
                .map(|(slot, column)| (logical.field_id(table, column), selection(slot)))
                .collect(),
        )
    }
    pub(crate) fn expand(&self, granted: &Granted) -> Vec<ResourceGrant> {
        self.whole_tables.resources(granted.whole, &granted.grants)
    }
    pub(crate) fn expand_partitions(&self, granted: &Granted) -> Vec<PartitionGrant> {
        self.whole_tables
            .partitions(granted.whole, &granted.partitions)
    }
    pub(crate) fn held_partitions(&self, grants: &[PartitionGrant]) -> BTreeSet<HeldPartition> {
        let logical = self.logical();
        let mut held = BTreeSet::new();
        for grant in grants {
            let Some(table) = logical
                .typed()
                .find(|table| logical.table_id(table) == grant.table)
            else {
                continue;
            };
            let Some(group) = table.group() else {
                continue;
            };
            let slots: Vec<_> = table
                .partition_by
                .iter()
                .filter_map(|name| table.column(name))
                .map(|column| grant.selector.0.get(&logical.field_id(table, column)))
                .collect();
            match slots.as_slice() {
                [Some(PartitionSelection::Only(values))] => {
                    held.extend(values.iter().map(|value| HeldPartition {
                        group: group.to_owned(),
                        values: vec![value.clone()],
                    }));
                }
                slots => {
                    let values: Option<Vec<_>> = slots
                        .iter()
                        .map(|slot| match slot {
                            Some(PartitionSelection::Only(values)) if values.len() == 1 => {
                                values.first().cloned()
                            }
                            _ => None,
                        })
                        .collect();
                    if let Some(values) = values {
                        held.insert(HeldPartition {
                            group: group.to_owned(),
                            values,
                        });
                    }
                }
            }
        }
        held
    }
    pub(crate) fn partition_targets(
        &self,
        targets: &[crate::PartitionTarget],
    ) -> Result<BTreeSet<HeldPartition>, Failure> {
        let logical = self.logical();
        targets
            .iter()
            .map(|target| {
                let (group, table, values) = target.parts();
                let table = match (group, table) {
                    (_, Some(table)) => table.to_owned(),
                    (Some(group), None) => logical
                        .typed()
                        .find(|table| table.group() == Some(group))
                        .map(|table| table.sql_name.clone())
                        .ok_or_else(|| format!("unknown partition group {group}"))?,
                    (None, None) => return Err("name a partition group or table".into()),
                };
                let address = logical.group_address(self.scope, &table, values)?;
                Ok(HeldPartition {
                    group: address.group,
                    values: address.values,
                })
            })
            .collect()
    }
    pub(crate) fn access_of(&self, capability: &rocky_credentials::Capability) -> Option<Access> {
        let extended = capability.extended().ok()?;
        if let Some(whole) = extended.whole {
            return Some(match whole {
                WholeAccess::Full => Access::Full,
                WholeAccess::Read => Access::Read,
                WholeAccess::Store => Access::Store,
            });
        }
        let role = extended.role?;
        self.logical()
            .role_by_id(role)
            .and_then(|declared| RoleName::new(declared.name.clone()).ok())
            .map(Access::Role)
    }
}
