use crate::{LogicalSchema, LogicalTable, Right, SchemaError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_GRANTS: usize = 64;
pub const RESERVED_ROLE_NAMES: [&str; 4] = ["admin", "full", "read", "store"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleDefinition {
    pub name: String,
    pub tables: Vec<RoleTable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_partition: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleTable {
    pub table: String,
    pub access: TableAccess,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableAccess {
    Read,
    Write,
    Append,
    UpdateColumns(Vec<String>),
}
pub struct AccessGrant<'a> {
    pub rights: BTreeSet<Right>,
    pub columns: Option<&'a [String]>,
}
impl TableAccess {
    pub fn grants(&self) -> Vec<AccessGrant<'_>> {
        let rights = |rights: &[Right]| rights.iter().copied().collect::<BTreeSet<_>>();
        let whole = |granted: &[Right]| AccessGrant {
            rights: rights(granted),
            columns: None,
        };
        match self {
            Self::Read => vec![whole(&[Right::Read, Right::Custody])],
            Self::Write => vec![whole(&[
                Right::Read,
                Right::Insert,
                Right::Update,
                Right::Delete,
                Right::Custody,
            ])],
            Self::Append => vec![whole(&[Right::Read, Right::Insert, Right::Custody])],
            Self::UpdateColumns(columns) => vec![
                whole(&[Right::Read, Right::Custody]),
                AccessGrant {
                    rights: rights(&[Right::Update]),
                    columns: Some(columns),
                },
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleError {
    InvalidName(String),
    ReservedName(String),
    DuplicateName(String),
    Empty(String),
    UnknownTable {
        role: String,
        table: String,
    },
    UnknownColumn {
        role: String,
        table: String,
        column: String,
    },
    NoColumns {
        role: String,
        table: String,
    },
    UnknownPartitionGroup {
        role: String,
        group: String,
    },
    DuplicatePartitionGroup {
        role: String,
        group: String,
    },
    PartitionGroupWithoutTable {
        role: String,
        group: String,
    },
    TooWide {
        role: String,
        grants: usize,
    },
}
impl std::fmt::Display for RoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(role) => write!(f, "role name {role:?} is not an identifier"),
            Self::ReservedName(role) => write!(f, "role name {role} is reserved"),
            Self::DuplicateName(role) => write!(f, "role {role} is declared twice"),
            Self::Empty(role) => write!(f, "role {role} grants no table"),
            Self::UnknownTable { role, table } => {
                write!(f, "role {role} names unknown table {table}")
            }
            Self::UnknownColumn {
                role,
                table,
                column,
            } => write!(f, "role {role} names unknown column {table}.{column}"),
            Self::NoColumns { role, table } => {
                write!(f, "role {role} updates no column of {table}")
            }
            Self::UnknownPartitionGroup { role, group } => {
                write!(f, "role {role} names unknown partition group {group}")
            }
            Self::DuplicatePartitionGroup { role, group } => {
                write!(f, "role {role} names partition group {group} twice")
            }
            Self::PartitionGroupWithoutTable { role, group } => {
                write!(f, "role {role} grants no table of partition group {group}")
            }
            Self::TooWide { role, grants } => write!(
                f,
                "role {role} compiles to {grants} grants; a credential holds at most {MAX_GRANTS}"
            ),
        }
    }
}
impl std::error::Error for RoleError {}
impl From<RoleError> for SchemaError {
    fn from(error: RoleError) -> Self {
        Self::Role(error)
    }
}

pub fn role_name_is_valid(name: &str) -> bool {
    crate::identifier(name)
}
pub fn role_name_is_reserved(name: &str) -> bool {
    RESERVED_ROLE_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}

impl LogicalTable {
    pub fn group(&self) -> Option<&str> {
        (!self.partition_by.is_empty())
            .then(|| self.partition_group.as_deref().unwrap_or(&self.sql_name))
    }
}
impl LogicalSchema {
    pub fn role(&self, name: &str) -> Option<&RoleDefinition> {
        self.roles.iter().find(|role| role.name == name)
    }
    pub fn role_id(&self, name: &str) -> crate::Commitment {
        crate::definition_item_id(self.publisher, self.app_id(), "role", name)
    }
    pub fn role_by_id(&self, id: crate::Commitment) -> Option<&RoleDefinition> {
        self.roles
            .iter()
            .find(|role| self.role_id(&role.name) == id)
    }
    pub(crate) fn validate_roles(&self) -> Result<(), SchemaError> {
        let groups: BTreeSet<String> = self
            .partition_groups()?
            .iter()
            .map(|group| group.name.to_owned())
            .collect();
        let mut names = BTreeSet::new();
        for role in &self.roles {
            let name = || role.name.clone();
            if !role_name_is_valid(&role.name) {
                return Err(RoleError::InvalidName(name()).into());
            }
            if role_name_is_reserved(&role.name) {
                return Err(RoleError::ReservedName(name()).into());
            }
            if !names.insert(role.name.to_ascii_lowercase()) {
                return Err(RoleError::DuplicateName(name()).into());
            }
            if role.tables.is_empty() {
                return Err(RoleError::Empty(name()).into());
            }
            for granted in &role.tables {
                let table = self
                    .typed()
                    .find(|table| table.sql_name == granted.table)
                    .ok_or_else(|| RoleError::UnknownTable {
                        role: name(),
                        table: granted.table.clone(),
                    })?;
                if let TableAccess::UpdateColumns(columns) = &granted.access {
                    if columns.is_empty() {
                        return Err(RoleError::NoColumns {
                            role: name(),
                            table: granted.table.clone(),
                        }
                        .into());
                    }
                    if let Some(column) = columns
                        .iter()
                        .find(|column| column.as_str() == "id" || table.column(column).is_none())
                    {
                        return Err(RoleError::UnknownColumn {
                            role: name(),
                            table: granted.table.clone(),
                            column: column.clone(),
                        }
                        .into());
                    }
                }
            }
            let mut per_partition = BTreeSet::new();
            for group in &role.per_partition {
                if !groups.contains(group) {
                    return Err(RoleError::UnknownPartitionGroup {
                        role: name(),
                        group: group.clone(),
                    }
                    .into());
                }
                if !per_partition.insert(group.as_str()) {
                    return Err(RoleError::DuplicatePartitionGroup {
                        role: name(),
                        group: group.clone(),
                    }
                    .into());
                }
                if !role.tables.iter().any(|granted| {
                    self.table(&granted.table).and_then(LogicalTable::group) == Some(group.as_str())
                }) {
                    return Err(RoleError::PartitionGroupWithoutTable {
                        role: name(),
                        group: group.clone(),
                    }
                    .into());
                }
            }
            let grants = self.least_role_grants(role);
            if grants > MAX_GRANTS {
                return Err(RoleError::TooWide {
                    role: name(),
                    grants,
                }
                .into());
            }
        }
        Ok(())
    }
    fn least_role_grants(&self, role: &RoleDefinition) -> usize {
        role.tables
            .iter()
            .map(|granted| {
                let rights = granted.access.grants().len();
                let group = self.table(&granted.table).and_then(LogicalTable::group);
                match group {
                    Some(group) if role.per_partition.iter().any(|named| named == group) => rights,
                    Some(_) => rights * 2,
                    None => rights,
                }
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SchemaDefinition;

    fn definition(roles: serde_json::Value) -> SchemaDefinition {
        serde_json::from_value(serde_json::json!({
            "format": crate::DEFINITION_FORMAT,
            "publisher": "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
            "application": "clinic",
            "tables": [
                {"name": "wards", "class": "replayed",
                 "columns": [{"name": "name", "type": "text"}],
                 "primary_key": ["id"], "partition_by": ["id"], "partition_group": "ward"},
                {"name": "charts", "class": "replayed",
                 "columns": [{"name": "ward", "type": "blob", "not_null": true},
                             {"name": "note", "type": "text"}],
                 "primary_key": ["ward", "id"], "partition_by": ["ward"], "partition_group": "ward"},
                {"name": "tickets", "class": "replayed",
                 "columns": [{"name": "holder", "type": "text"}], "primary_key": ["id"]},
                {"name": "check_ins", "class": "replayed",
                 "columns": [{"name": "ticket", "type": "blob"}, {"name": "gate", "type": "text"}],
                 "primary_key": ["id"]}
            ],
            "roles": roles,
        }))
        .unwrap()
    }
    fn refusal(roles: serde_json::Value) -> RoleError {
        match definition(roles).build() {
            Err(SchemaError::Role(error)) => error,
            other => panic!("expected a role refusal, got {other:?}"),
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn declared_roles_reach_the_manifest_as_role_items() {
        let schema = definition(serde_json::json!([
            {"name": "scanner", "tables": [
                {"table": "tickets", "access": "read"},
                {"table": "check_ins", "access": "append"}]},
            {"name": "clinician", "tables": [
                {"table": "charts", "access": {"update_columns": ["note"]}}],
             "per_partition": ["ward"]}
        ]))
        .build()
        .unwrap();
        let manifest = schema.manifest(&[]);
        let names: Vec<_> = manifest
            .roles
            .iter()
            .map(|role| role.name.as_str())
            .collect();
        assert_eq!(names, ["scanner", "clinician"]);
        assert_eq!(
            schema.role("scanner").unwrap().tables[1].access,
            TableAccess::Append
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn every_role_rule_is_a_typed_refusal() {
        let read = |table: &str| serde_json::json!({"table": table, "access": "read"});
        for reserved in ["admin", "Full", "read", "store"] {
            assert_eq!(
                refusal(serde_json::json!([{"name": reserved, "tables": [read("tickets")]}])),
                RoleError::ReservedName(reserved.into())
            );
        }
        assert_eq!(
            refusal(serde_json::json!([{"name": "1st", "tables": [read("tickets")]}])),
            RoleError::InvalidName("1st".into())
        );
        assert_eq!(
            refusal(serde_json::json!([
                {"name": "scanner", "tables": [read("tickets")]},
                {"name": "Scanner", "tables": [read("tickets")]}
            ])),
            RoleError::DuplicateName("Scanner".into())
        );
        assert_eq!(
            refusal(serde_json::json!([{"name": "idle", "tables": []}])),
            RoleError::Empty("idle".into())
        );
        assert_eq!(
            refusal(serde_json::json!([{"name": "scanner", "tables": [read("missing")]}])),
            RoleError::UnknownTable {
                role: "scanner".into(),
                table: "missing".into()
            }
        );
        assert_eq!(
            refusal(serde_json::json!([{"name": "gatekeeper",
                "tables": [{"table": "check_ins", "access": {"update_columns": ["absent"]}}]}])),
            RoleError::UnknownColumn {
                role: "gatekeeper".into(),
                table: "check_ins".into(),
                column: "absent".into()
            }
        );
        assert_eq!(
            refusal(serde_json::json!([{"name": "gatekeeper",
                "tables": [{"table": "check_ins", "access": {"update_columns": []}}]}])),
            RoleError::NoColumns {
                role: "gatekeeper".into(),
                table: "check_ins".into()
            }
        );
        assert_eq!(
            refusal(serde_json::json!([{"name": "clinician",
                "tables": [read("charts")], "per_partition": ["floor"]}])),
            RoleError::UnknownPartitionGroup {
                role: "clinician".into(),
                group: "floor".into()
            }
        );
        assert_eq!(
            refusal(serde_json::json!([{"name": "clinician",
                "tables": [read("charts")], "per_partition": ["ward", "ward"]}])),
            RoleError::DuplicatePartitionGroup {
                role: "clinician".into(),
                group: "ward".into()
            }
        );
        assert_eq!(
            refusal(serde_json::json!([{"name": "clinician",
                "tables": [read("tickets")], "per_partition": ["ward"]}])),
            RoleError::PartitionGroupWithoutTable {
                role: "clinician".into(),
                group: "ward".into()
            }
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn grants_on_one_table_compose_instead_of_being_refused() {
        let schema = definition(serde_json::json!([{"name": "dispatcher", "tables": [
            {"table": "check_ins", "access": "append"},
            {"table": "check_ins", "access": {"update_columns": ["gate"]}}]}]))
        .build()
        .unwrap();
        let rights: BTreeSet<Right> = schema
            .role("dispatcher")
            .unwrap()
            .tables
            .iter()
            .flat_map(|granted| granted.access.grants())
            .flat_map(|grant| grant.rights)
            .collect();
        assert_eq!(
            rights,
            BTreeSet::from([Right::Read, Right::Insert, Right::Update, Right::Custody])
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_role_wider_than_a_credential_is_refused() {
        let mut wide = definition(serde_json::json!([]));
        for index in 0..33 {
            wide.tables.push(
                serde_json::from_value(serde_json::json!({
                    "name": format!("t{index:02}"), "class": "replayed",
                    "columns": [{"name": "body", "type": "text"}], "primary_key": ["id"]
                }))
                .unwrap(),
            );
        }
        let updates = |count: usize| -> Vec<RoleTable> {
            (0..count)
                .map(|index| RoleTable {
                    table: format!("t{index:02}"),
                    access: TableAccess::UpdateColumns(vec!["body".into()]),
                })
                .collect()
        };
        wide.roles = vec![RoleDefinition {
            name: "editor".into(),
            tables: updates(32),
            per_partition: Vec::new(),
        }];
        assert!(wide.build().is_ok(), "64 grants fit one credential");
        wide.roles[0].tables = updates(33);
        assert_eq!(
            wide.build().unwrap_err(),
            SchemaError::Role(RoleError::TooWide {
                role: "editor".into(),
                grants: 66
            })
        );
    }
}
