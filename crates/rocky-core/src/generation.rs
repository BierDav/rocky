//! Generations of an application definition: what changed between two
//! installed schemas, and the transition a publisher signs before a
//! replicated change activates. See `docs/runtime/generations.md`.
use crate::{LogicalSchema, TableClass, logical};
use std::collections::BTreeSet;

/// How a new schema relates to the installed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaChange {
    /// The same installed schema.
    Unchanged,
    /// Local tables, indexes or SQL spelling: replicas agree as before.
    /// Only the local projection is rebuilt.
    Local,
    /// Replicated changes, which need a signed transition: new mergeable
    /// tables, nullable fields of existing ones, required fields of
    /// existing ones, new or once nullable, each with a default, and
    /// removals. Rows of older writers that lack a required field, or hold
    /// NULL in it, read its default.
    Additive {
        tables: BTreeSet<String>,
        /// `table.field`.
        fields: BTreeSet<String>,
        /// `table.field` NOT NULL fields rows of the installed schema lack.
        required: BTreeSet<String>,
        removed: Removed,
    },
}
/// Mergeable tables and fields a new schema no longer has. Every device
/// that adopts it drops them; see `docs/runtime/removal.md`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub tables: BTreeSet<String>,
    /// `table.field` of tables that remain.
    pub fields: BTreeSet<String>,
}
impl Removed {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.fields.is_empty()
    }
}
impl SchemaChange {
    pub fn replicated(&self) -> bool {
        matches!(self, Self::Additive { .. })
    }
    /// Fields rows of the installed schema may lack: `table.field`.
    pub fn added(&self) -> BTreeSet<String> {
        match self {
            Self::Additive {
                fields, required, ..
            } => fields.union(required).cloned().collect(),
            Self::Unchanged | Self::Local => BTreeSet::new(),
        }
    }
    /// What the new schema removed.
    pub fn removed(&self) -> Removed {
        match self {
            Self::Additive { removed, .. } => removed.clone(),
            Self::Unchanged | Self::Local => Removed::default(),
        }
    }
}

/// A change no transition can make: replicas holding data of the installed
/// schema would no longer agree on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// Another publisher or application name.
    Application,
    UpdatePolicy,
    /// A mergeable table became another class, or the reverse.
    TableClass(String),
    ChangedKey(String),
    /// Type, nullability or collation of `table.field` changed.
    ChangedField(String),
    /// A required field of an existing table needs a default: rows written
    /// before it hold no value for it.
    RequiredField(String),
    /// The new generation does not follow the installed one.
    Generation,
}
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Application => f.write_str("another publisher or application"),
            Self::UpdatePolicy => f.write_str("the update policy changed"),
            Self::TableClass(table) => write!(f, "table {table} changed class"),
            Self::ChangedKey(table) => write!(f, "the key of {table} changed"),
            Self::ChangedField(field) => write!(f, "field {field} changed type"),
            Self::RequiredField(field) => write!(
                f,
                "required field {field} of an existing table needs a default"
            ),
            Self::Generation => f.write_str("the generation does not follow the installed one"),
        }
    }
}
impl std::error::Error for Unsupported {}

/// Classifies `new` against the installed `old`. Names are the identity of
/// tables and fields: a rename is a removal and an addition, and a name
/// keeps its type, collation and key role. A removed key or partition column
/// changes its table's key; the table is removed instead. See decisions 0067
/// and 0068.
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
    if old.update_policy != new.update_policy {
        return Err(Unsupported::UpdatePolicy);
    }
    let class = |schema: &LogicalSchema, name: &str| {
        schema
            .tables
            .iter()
            .find(|table| table.sql_name == name)
            .map(|table| table.class)
    };
    let (mut tables, mut fields, mut required) =
        (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    let mut removed = Removed::default();
    for table in new.mergeable() {
        match class(old, &table.sql_name) {
            None => {
                tables.insert(table.sql_name.clone());
            }
            Some(TableClass::Mergeable) => {}
            Some(_) => return Err(Unsupported::TableClass(table.sql_name.clone())),
        }
    }
    for before in old.mergeable() {
        let name = &before.sql_name;
        let Some(after) = new.mergeable().find(|table| table.sql_name == *name) else {
            if class(new, name).is_some() {
                return Err(Unsupported::TableClass(name.clone()));
            }
            removed.tables.insert(name.clone());
            continue;
        };
        let (old_key, new_key) = (logical::key_shape(before), logical::key_shape(after));
        if old_key != new_key
            || before.partition_by != after.partition_by
            || before.partition_group != after.partition_group
        {
            return Err(Unsupported::ChangedKey(name.clone()));
        }
        let old_fields = logical::register_shapes(before);
        let new_fields = logical::register_shapes(after);
        let column = |table: &crate::LogicalTable, field: &str| {
            table
                .columns
                .iter()
                .find(|column| column.sql_name == field)
                .cloned()
        };
        for (field, shape) in &old_fields {
            match new_fields.get(field) {
                None => {
                    removed.fields.insert(format!("{name}.{field}"));
                }
                Some(other) if other != shape => {
                    // Only NOT NULL may tighten, and only with a default.
                    let (Some(was), Some(now)) = (column(before, field), column(after, field))
                    else {
                        return Err(Unsupported::ChangedField(format!("{name}.{field}")));
                    };
                    let relaxed = crate::LogicalColumn {
                        not_null: false,
                        default: was.default.clone(),
                        ..now.clone()
                    };
                    if was.not_null || !now.not_null || relaxed != was {
                        return Err(Unsupported::ChangedField(format!("{name}.{field}")));
                    }
                    if now.default.as_deref().is_none_or(|value| value == "NULL") {
                        return Err(Unsupported::RequiredField(format!("{name}.{field}")));
                    }
                    required.insert(format!("{name}.{field}"));
                }
                Some(_) => {}
            }
        }
        for field in new_fields
            .keys()
            .filter(|field| !old_fields.contains_key(*field))
        {
            let Some(now) = column(after, field) else {
                return Err(Unsupported::RequiredField(format!("{name}.{field}")));
            };
            if !now.not_null {
                fields.insert(format!("{name}.{field}"));
            } else if now.default.as_deref().is_some_and(|value| value != "NULL") {
                required.insert(format!("{name}.{field}"));
            } else {
                return Err(Unsupported::RequiredField(format!("{name}.{field}")));
            }
        }
    }
    Ok(SchemaChange::Additive {
        tables,
        fields,
        required,
        removed,
    })
}

/// The schema generation of a definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation {
    pub schema: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Affinity, Collation, ColumnClass, LOGICAL_SCHEMA_FORMAT, LogicalColumn, LogicalIndex,
        LogicalTable, SchemaSource, UpdatePolicy,
    };

    fn column(name: &str, not_null: bool) -> LogicalColumn {
        LogicalColumn {
            sql_name: name.into(),
            affinity: if ["id", "row_kind"].contains(&name) {
                Affinity::Integer
            } else {
                Affinity::Text
            },
            not_null,
            default: None,
            collation: Collation::Binary,
            class: ColumnClass::Mergeable,
        }
    }
    fn table(name: &str, mut columns: Vec<LogicalColumn>) -> LogicalTable {
        columns.push(column("row_kind", true));
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
            class: TableClass::Mergeable,
            create_sql: format!("CREATE TABLE {name}({})", sql.join(",")),
            auto_generate: false,
            columns,
            primary_key: vec!["id".into()],
            partition_by: Vec::new(),
            partition_group: None,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),

            existence_log: false,
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
            update_policy: UpdatePolicy::Changed,
            tables,
            views: Vec::new(),
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

    #[test]
    fn local_and_additive_changes_are_told_apart_from_unsupported_ones() {
        let old = schema(vec![items(&[("name", true)])]);
        assert_eq!(classify(&old, &old), Ok(SchemaChange::Unchanged));
        // An index changes only the local projection.
        let mut indexed = old.clone();
        indexed.tables[0].indexes.push(LogicalIndex {
            name: "items_name".into(),
            unique: false,
            columns: vec![crate::IndexColumn {
                name: "name".into(),
                descending: false,
            }],
            create_sql: "CREATE INDEX items_name ON items(name)".into(),
            on_conflict: None,
        });
        assert_eq!(classify(&old, &indexed), Ok(SchemaChange::Local));
        let added = schema(vec![
            items(&[("name", true), ("note", false)]),
            table("tags", vec![column("id", true), column("label", true)]),
        ]);
        assert_eq!(
            classify(&old, &added),
            Ok(SchemaChange::Additive {
                tables: ["tags".to_string()].into(),
                fields: ["items.note".to_string()].into(),
                required: BTreeSet::new(),
                removed: Removed::default(),
            })
        );
        for (new, refusal) in [
            (
                schema(vec![items(&[("name", true), ("note", true)])]),
                Unsupported::RequiredField("items.note".into()),
            ),
            (
                schema(vec![items(&[("name", false)])]),
                Unsupported::ChangedField("items.name".into()),
            ),
            (
                schema(vec![items(&[("title", true)])]),
                Unsupported::RequiredField("items.title".into()),
            ),
        ] {
            assert_eq!(classify(&old, &new), Err(refusal));
        }
        let mut other = old.clone();
        other.publisher = [2; 32];
        assert_eq!(classify(&old, &other), Err(Unsupported::Application));
    }
    #[test]
    fn names_are_identities_and_keep_one_type() {
        let old = schema(vec![items(&[("name", true), ("note", false)])]);
        // A renamed field is another field: the old one is removed.
        let mut renamed = old.clone();
        let table = &mut renamed.tables[0];
        table.columns[1].sql_name = "label".into();
        table.create_sql = table.create_sql.replace("name", "label");
        renamed.validate().unwrap();
        assert_ne!(old.manifest(&[]).hash(), renamed.manifest(&[]).hash());
        assert_eq!(
            classify(&old, &renamed),
            Err(Unsupported::RequiredField("items.label".into()))
        );
        renamed.tables[0].columns[1].not_null = false;
        assert_eq!(
            classify(&old, &renamed),
            Ok(SchemaChange::Additive {
                tables: BTreeSet::new(),
                fields: ["items.label".to_string()].into(),
                required: BTreeSet::new(),
                removed: Removed {
                    tables: BTreeSet::new(),
                    fields: ["items.name".to_string()].into(),
                },
            })
        );
        // A name keeps its type, collation and key role.
        let mut retyped = old.clone();
        retyped.tables[0].columns[2].affinity = Affinity::Blob;
        let mut collated = old.clone();
        collated.tables[0].columns[2].collation = Collation::Nocase;
        for new in [retyped, collated] {
            assert_eq!(
                classify(&old, &new),
                Err(Unsupported::ChangedField("items.note".into()))
            );
        }
        let mut keyed = old.clone();
        keyed.tables[0].primary_key = vec!["name".into(), "id".into()];
        assert_eq!(
            classify(&old, &keyed),
            Err(Unsupported::ChangedKey("items".into()))
        );
    }
    #[test]
    fn required_fields_with_defaults_are_additive() {
        let defaulted = |table: &mut LogicalTable, name: &str| {
            for column in table
                .columns
                .iter_mut()
                .filter(|column| column.sql_name == name)
            {
                column.default = Some("''".into());
            }
        };
        let old = schema(vec![items(&[("name", true), ("note", false)])]);
        let mut new = schema(vec![items(&[
            ("name", true),
            ("note", true),
            ("position", true),
            ("tag", false),
        ])]);
        defaulted(&mut new.tables[0], "note");
        defaulted(&mut new.tables[0], "position");
        let change = classify(&old, &new);
        assert_eq!(
            change,
            Ok(SchemaChange::Additive {
                tables: BTreeSet::new(),
                fields: ["items.tag".to_string()].into(),
                required: ["items.note".to_string(), "items.position".to_string()].into(),
                removed: Removed::default(),
            })
        );
        assert_eq!(
            change.map(|change| change.added().len()),
            Ok(3),
            "rows of the installed schema may lack all three"
        );
        // Without a default no transition makes a required field.
        let mut bare = new.clone();
        bare.tables[0]
            .columns
            .iter_mut()
            .for_each(|column| column.default = None);
        assert_eq!(
            classify(&old, &bare),
            Err(Unsupported::RequiredField("items.note".into()))
        );
        // Loosening, or changing the type alongside, is no tightening.
        let mut retyped = new.clone();
        retyped.tables[0].columns[2].affinity = Affinity::Integer;
        assert!(matches!(
            classify(&old, &retyped),
            Err(Unsupported::ChangedField(_) | Unsupported::RequiredField(_))
        ));
        assert_eq!(
            classify(&new, &old),
            Err(Unsupported::ChangedField("items.note".into()))
        );
    }

    #[test]
    fn removed_tables_and_fields_are_replicated_changes() {
        let mut shelves = table("shelves", vec![column("id", true), column("label", true)]);
        shelves.indexes.push(LogicalIndex {
            name: "shelves_label".into(),
            unique: true,
            columns: vec![crate::IndexColumn {
                name: "label".into(),
                descending: false,
            }],
            create_sql: "CREATE UNIQUE INDEX shelves_label ON shelves(label)".into(),
            on_conflict: None,
        });
        let old = schema(vec![items(&[("name", true), ("note", false)]), shelves]);
        let new = schema(vec![items(&[("name", true)])]);
        let change = classify(&old, &new);
        assert_eq!(
            change,
            Ok(SchemaChange::Additive {
                tables: BTreeSet::new(),
                fields: BTreeSet::new(),
                required: BTreeSet::new(),
                removed: Removed {
                    tables: ["shelves".to_string()].into(),
                    fields: ["items.note".to_string()].into(),
                },
            })
        );
        assert!(change.is_ok_and(|change| change.replicated() && change.added().is_empty()));
        // Re-adding a name is an ordinary addition of the same field.
        assert_eq!(
            classify(&new, &old).map(|change| change.removed()),
            Ok(Removed::default())
        );
        // A removed key column changes the key: the table goes instead.
        let mut keyless = schema(vec![items(&[("name", true), ("note", false)])]);
        keyless.tables[0].primary_key = vec!["name".into()];
        keyless.tables[0]
            .columns
            .retain(|column| column.sql_name != "id");
        assert_eq!(
            classify(&old, &keyless),
            Err(Unsupported::ChangedKey("items".into()))
        );
    }
}
