use crate::{ColumnClass, LogicalSchema};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaChange {
    Unchanged,
    Local,
    Replicated,
}
impl SchemaChange {
    pub fn replicated(&self) -> bool {
        matches!(self, Self::Replicated)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    Application,
    ChangedField(String),
    RequiredField(String),
}
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Application => f.write_str("another publisher or application"),
            Self::ChangedField(field) => write!(f, "field {field} changed type"),
            Self::RequiredField(field) => write!(
                f,
                "required field {field} of an existing table needs a default"
            ),
        }
    }
}
impl std::error::Error for Unsupported {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub tables: BTreeSet<String>,
    pub fields: BTreeSet<String>,
}
impl Removed {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.fields.is_empty()
    }
}
pub fn removed(old: &LogicalSchema, new: &LogicalSchema) -> Removed {
    let mut removed = Removed::default();
    for before in old.typed() {
        let Some(after) = new.typed().find(|table| table.sql_name == before.sql_name) else {
            removed.tables.insert(before.sql_name.clone());
            continue;
        };
        removed.fields.extend(
            before
                .columns
                .iter()
                .filter(|column| {
                    column.class == ColumnClass::Replicated
                        && after.column(&column.sql_name).is_none()
                })
                .map(|column| format!("{}.{}", before.sql_name, column.sql_name)),
        );
    }
    removed
}

pub fn classify(old: &LogicalSchema, new: &LogicalSchema) -> Result<SchemaChange, Unsupported> {
    if old.publisher != new.publisher || old.application != new.application {
        return Err(Unsupported::Application);
    }
    if old == new {
        return Ok(SchemaChange::Unchanged);
    }
    if old.replicated_fingerprint() == new.replicated_fingerprint() {
        return Ok(SchemaChange::Local);
    }
    let defaulted = |column: &crate::LogicalColumn| {
        column
            .default
            .as_deref()
            .is_some_and(|value| !value.eq_ignore_ascii_case("NULL"))
    };
    for after in new.typed() {
        let Some(before) = old.typed().find(|table| table.sql_name == after.sql_name) else {
            continue;
        };
        for now in after
            .columns
            .iter()
            .filter(|column| column.class == ColumnClass::Replicated)
        {
            let field = || format!("{}.{}", after.sql_name, now.sql_name);
            match before.column(&now.sql_name) {
                Some(was) if was.affinity != now.affinity || was.collation != now.collation => {
                    return Err(Unsupported::ChangedField(field()));
                }
                Some(was) if was.not_null || !now.not_null || defaulted(now) => {}
                None if !now.not_null || defaulted(now) => {}
                _ => return Err(Unsupported::RequiredField(field())),
            }
        }
    }
    Ok(SchemaChange::Replicated)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation {
    pub schema: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Affinity, Collation, ColumnClass, LOGICAL_SCHEMA_FORMAT, LogicalColumn, LogicalTable,
        SchemaSource, TableClass,
    };

    fn column(name: &str, not_null: bool) -> LogicalColumn {
        LogicalColumn {
            sql_name: name.into(),
            affinity: if name == "id" {
                Affinity::Integer
            } else {
                Affinity::Text
            },
            not_null,
            default: None,
            collation: Collation::Binary,
            class: ColumnClass::Replicated,
        }
    }
    fn table(name: &str, columns: Vec<LogicalColumn>) -> LogicalTable {
        let sql: Vec<String> = columns
            .iter()
            .map(|column| {
                let null = if column.not_null { " NOT NULL" } else { "" };
                let key = if column.sql_name == "id" {
                    " PRIMARY KEY"
                } else {
                    ""
                };
                format!(
                    "{} {}{null}{key}",
                    column.sql_name,
                    if column.affinity == Affinity::Integer {
                        "INTEGER"
                    } else {
                        "TEXT"
                    }
                )
            })
            .collect();
        LogicalTable {
            sql_name: name.into(),
            granted_like: None,
            class: TableClass::Replayed,
            create_sql: format!("CREATE TABLE {name}({})", sql.join(",")),
            auto_generate: false,
            columns,
            primary_key: vec!["id".into()],
            partition_by: Vec::new(),
            partition_group: None,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        }
    }
    fn schema(tables: Vec<LogicalTable>) -> LogicalSchema {
        LogicalSchema {
            format: LOGICAL_SCHEMA_FORMAT,
            publisher: [1; 32],
            application: "app".into(),
            source: SchemaSource {
                kind: "test".into(),
                version: 1,
                identity: "app".into(),
            },
            tables,
            views: Vec::new(),
            assertions: Vec::new(),
            restrictions: Vec::new(),
        }
    }
    fn items(columns: &[(&str, bool)]) -> LogicalTable {
        let mut all = vec![column("id", true)];
        all.extend(
            columns
                .iter()
                .map(|(name, required)| column(name, *required)),
        );
        table("items", all)
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn local_and_replicated_changes_are_told_apart() {
        let old = schema(vec![items(&[("name", true)])]);
        assert_eq!(classify(&old, &old), Ok(SchemaChange::Unchanged));
        let mut spelled = old.clone();
        spelled.source.identity = "other".into();
        assert_eq!(classify(&old, &spelled), Ok(SchemaChange::Local));
        let added = schema(vec![items(&[("name", true), ("note", false)])]);
        assert_eq!(classify(&old, &added), Ok(SchemaChange::Replicated));
        let mut renamed = old.clone();
        renamed.application = "other".into();
        assert_eq!(classify(&old, &renamed), Err(Unsupported::Application));
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_name_keeps_its_type_and_a_required_field_needs_a_default() {
        let old = schema(vec![items(&[("name", true), ("note", false)])]);
        let mut retyped = old.clone();
        retyped.tables[0].columns[1].affinity = Affinity::Integer;
        assert_eq!(
            classify(&old, &retyped),
            Err(Unsupported::ChangedField("items.name".into()))
        );
        for added in [("note", true), ("extra", true)] {
            let new = schema(vec![items(&[("name", true), added])]);
            assert_eq!(
                classify(&old, &new),
                Err(Unsupported::RequiredField(format!("items.{}", added.0)))
            );
        }
        let mut required = schema(vec![items(&[("name", true), ("note", true)])]);
        required.tables[0].columns[2].default = Some("''".into());
        assert_eq!(classify(&old, &required), Ok(SchemaChange::Replicated));
    }
}
