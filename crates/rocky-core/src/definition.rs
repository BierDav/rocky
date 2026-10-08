//! Portable schema definitions: the ORM-neutral way to construct a
//! `LogicalSchema` from data a host language can write, without Room or Rust.
//! See `docs/runtime/schema-definitions.md`.
use crate::{
    Affinity, Collation, ColumnClass, ConflictPolicy, IndexColumn, LogicalColumn,
    LogicalForeignKey, LogicalIndex, LogicalSchema, LogicalTable, SchemaError, SchemaSource,
    TableClass, UpdatePolicy, commitment,
};
use serde::{Deserialize, Serialize};

/// The `SchemaSource::kind` of descriptors built from a definition.
pub const DEFINITION_SOURCE: &str = "rocky.definition";
pub const DEFINITION_FORMAT: u32 = 2;
/// Largest definition document `SchemaDefinition::parse` accepts.
pub const MAX_DEFINITION_BYTES: usize = 1024 * 1024;

/// What an application declares about its tables. `build` generates the
/// statements and the canonical descriptor; nothing is written twice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaDefinition {
    pub format: u32,
    /// 64 hexadecimal digits.
    pub publisher: String,
    pub application: String,
    #[serde(default = "changed")]
    pub update_policy: UpdatePolicy,
    pub tables: Vec<TableDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<crate::LogicalView>,
}
fn changed() -> UpdatePolicy {
    UpdatePolicy::Changed
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableDefinition {
    pub name: String,
    /// The identity to keep after a rename; the name by default.
    #[serde(default)]
    pub logical: Option<String>,
    pub class: TableClass,
    pub columns: Vec<ColumnDefinition>,
    pub primary_key: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partition_by: Vec<String>,
    #[serde(default)]
    pub indexes: Vec<IndexDefinition>,
    /// The application inserts a key again after deleting it.
    #[serde(default)]
    pub reuses_keys: bool,
    /// References to the identity of another mergeable table. Omitted when
    /// empty, so earlier definitions keep their identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign_keys: Vec<ForeignKeyDefinition>,
    /// Every device of the domain reads the table's row creations and
    /// deletions, so another reader scope may reference it. Omitted when
    /// false, so earlier definitions keep their identity.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub existence_log: bool,
}
/// What a child shows when its parent is not alive; see `docs/runtime/mergeable-constraints.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteAction {
    Cascade,
    SetNull,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignKeyDefinition {
    /// The child's columns, one per column of the parent's identity.
    pub columns: Vec<String>,
    /// The parent table.
    pub table: String,
    pub on_delete: DeleteAction,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnDefinition {
    pub name: String,
    #[serde(default)]
    pub logical: Option<String>,
    #[serde(rename = "type")]
    pub affinity: Affinity,
    #[serde(default)]
    pub not_null: bool,
    /// A literal: string, number, boolean or null.
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default = "binary")]
    pub collation: Collation,
    /// An expression over the row's replicated columns.
    #[serde(default)]
    pub derived: Option<String>,
    #[serde(default)]
    pub local_only: bool,
}
fn binary() -> Collation {
    Collation::Binary
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexDefinition {
    pub name: String,
    pub columns: Vec<IndexColumn>,
    #[serde(default)]
    pub unique: bool,
    /// Required for a unique index of a mergeable table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_conflict: Option<ConflictPolicy>,
}

fn invalid<T>(reason: impl Into<String>) -> Result<T, SchemaError> {
    Err(SchemaError::Invalid(reason.into()))
}
fn quoted(name: &str) -> String {
    format!("\"{name}\"")
}
fn publisher(hex: &str) -> Result<[u8; 32], SchemaError> {
    let digits = hex.as_bytes();
    if digits.len() != 64 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return invalid("publisher must be 64 hexadecimal digits");
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(digits.chunks(2)) {
        let pair = std::str::from_utf8(pair).unwrap_or("00");
        *byte = u8::from_str_radix(pair, 16).unwrap_or(0);
    }
    Ok(bytes)
}
/// A JSON scalar as the SQL literal of a column default.
fn literal(value: &serde_json::Value) -> Result<String, SchemaError> {
    use serde_json::Value;
    Ok(match value {
        Value::Null => "NULL".into(),
        Value::Bool(value) => i64::from(*value).to_string(),
        Value::Number(number) if number.is_i64() || number.is_u64() => number.to_string(),
        Value::Number(number) => match number.as_f64() {
            Some(real) if real.is_finite() => format!("{real:?}"),
            _ => return invalid("default number"),
        },
        Value::String(text) if !text.contains('\0') => format!("'{}'", text.replace('\'', "''")),
        _ => return invalid("a default must be a string, number, boolean or null"),
    })
}
fn declared(affinity: Affinity) -> &'static str {
    match affinity {
        Affinity::Integer => "INTEGER",
        Affinity::Real => "REAL",
        Affinity::Text => "TEXT",
        Affinity::Blob => "BLOB",
        Affinity::Numeric => "NUMERIC",
    }
}
fn collation(collation: Collation) -> &'static str {
    match collation {
        Collation::Binary => "",
        Collation::Nocase => " COLLATE NOCASE",
        Collation::Rtrim => " COLLATE RTRIM",
    }
}

impl SchemaDefinition {
    /// Parses a definition document and builds its descriptor.
    pub fn parse(json: &[u8]) -> Result<Self, SchemaError> {
        if json.len() > MAX_DEFINITION_BYTES {
            return invalid("definition quota");
        }
        serde_json::from_slice(json).map_err(|error| SchemaError::Invalid(error.to_string()))
    }
    /// The canonical descriptor. It is structurally valid; whether a
    /// profile can install it is `LogicalSchema::supported_on`. The same
    /// definition always builds the same descriptor.
    pub fn build(&self) -> Result<LogicalSchema, SchemaError> {
        if ![DEFINITION_FORMAT, crate::PARTITION_SCHEMA_FORMAT].contains(&self.format) {
            return invalid(
                "row identity requires definition format 2; export with the original build and reinstall into a new database",
            );
        }
        if self.format != crate::PARTITION_SCHEMA_FORMAT
            && self
                .tables
                .iter()
                .any(|table| !table.partition_by.is_empty())
        {
            return invalid("partition columns require definition format 3");
        }
        let mut tables = self
            .tables
            .iter()
            .map(|table| table.build())
            .collect::<Result<Vec<_>, _>>()?;
        // References name the parent's identity, known once every table is built.
        let identities: Vec<(String, Vec<String>)> = tables
            .iter()
            .map(|table| (table.sql_name.clone(), table.identity_columns()))
            .collect();
        for (table, definition) in tables.iter_mut().zip(&self.tables) {
            // Declaration order does not matter, as for tables and indexes.
            let mut keys: Vec<_> = definition.foreign_keys.iter().collect();
            keys.sort_by(|a, b| a.columns.cmp(&b.columns));
            for key in keys {
                let references = identities
                    .iter()
                    .find(|(name, _)| *name == key.table)
                    .map(|(_, columns)| columns.clone())
                    .ok_or_else(|| {
                        SchemaError::Invalid(format!(
                            "{} references unknown table {}",
                            table.sql_name, key.table
                        ))
                    })?;
                let key = LogicalForeignKey {
                    table: key.table.clone(),
                    columns: key.columns.clone(),
                    references,
                    on_delete: match key.on_delete {
                        DeleteAction::Cascade => crate::DELETE_CASCADE,
                        DeleteAction::SetNull => crate::DELETE_SET_NULL,
                    }
                    .into(),
                    on_update: "NO ACTION".into(),
                };
                let clause = crate::foreign_key_clause(&key);
                table.create_sql.pop();
                table.create_sql.push_str(&format!(", {clause})"));
                table.foreign_keys.push(key);
            }
        }
        tables.sort_by(|a, b| a.logical.cmp(&b.logical));
        let identity =
            serde_json::to_vec(self).map_err(|error| SchemaError::Invalid(error.to_string()))?;
        let schema = LogicalSchema {
            format: self.format,
            publisher: publisher(&self.publisher)?,
            application: self.application.clone(),
            source: SchemaSource {
                kind: DEFINITION_SOURCE.into(),
                version: self.format,
                identity: commitment("rocky.schema-definition.v1", &identity)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            },
            update_policy: self.update_policy,
            tables,
            views: self.views.clone(),
        };
        schema.validate()?;
        Ok(schema)
    }
}
impl TableDefinition {
    fn build(&self) -> Result<LogicalTable, SchemaError> {
        let name = &self.name;
        if !crate::identifier(name) {
            return invalid(format!("table name {name}"));
        }
        let mut columns = Vec::new();
        let mut parts = Vec::new();
        for column in &self.columns {
            if ["id", "row_kind"].contains(&column.name.as_str()) {
                return invalid(format!(
                    "{name}.{} is supplied by Rocky; do not declare it",
                    column.name
                ));
            }
            if !crate::identifier(&column.name) {
                return invalid(format!("{name} column {}", column.name));
            }
            let class = match (&column.derived, column.local_only) {
                (Some(rule), false) => ColumnClass::Derived { rule: rule.clone() },
                (None, true) => ColumnClass::LocalOnly,
                (None, false) => ColumnClass::Mergeable,
                (Some(_), true) => {
                    return invalid(format!(
                        "{name}.{} cannot be both derived and local-only",
                        column.name
                    ));
                }
            };
            let default = column.default.as_ref().map(literal).transpose()?;
            let mut part = format!("{} {}", quoted(&column.name), declared(column.affinity));
            if column.not_null {
                part.push_str(" NOT NULL");
            }
            if let Some(default) = &default {
                part.push_str(" DEFAULT ");
                part.push_str(default);
            }
            part.push_str(collation(column.collation));
            parts.push(part);
            columns.push(LogicalColumn {
                logical: column
                    .logical
                    .clone()
                    .unwrap_or_else(|| column.name.clone()),
                sql_name: column.name.clone(),
                affinity: column.affinity,
                not_null: column.not_null,
                default,
                collation: column.collation,
                class,
            });
        }
        {
            for (name, default) in [("id", None), ("row_kind", Some("0".to_string()))] {
                let sql_default = default
                    .as_ref()
                    .map(|value| format!(" DEFAULT {value}"))
                    .unwrap_or_default();
                parts.push(format!("{} INTEGER NOT NULL{sql_default}", quoted(name)));
                columns.push(LogicalColumn {
                    logical: name.into(),
                    sql_name: name.into(),
                    affinity: Affinity::Integer,
                    not_null: true,
                    default,
                    collation: Collation::Binary,
                    class: ColumnClass::Mergeable,
                });
            }
        }
        let key: Vec<String> = self
            .primary_key
            .iter()
            .map(|column| quoted(column))
            .collect();
        if !key.is_empty() {
            parts.push(format!("PRIMARY KEY({})", key.join(",")));
        }
        let mut indexes = Vec::new();
        for index in &self.indexes {
            if !crate::identifier(&index.name) {
                return invalid(format!("{name} index {}", index.name));
            }
            let terms: Vec<String> = index
                .columns
                .iter()
                .map(|column| {
                    let order = if column.descending { " DESC" } else { "" };
                    format!("{}{order}", quoted(&column.name))
                })
                .collect();
            if index.on_conflict.is_some() && !index.unique {
                return invalid(format!(
                    "{name}: on_conflict needs a unique index {}",
                    index.name
                ));
            }
            let unique = if index.unique { "UNIQUE " } else { "" };
            let create_sql = if index.on_conflict.is_some() && self.class == TableClass::Mergeable {
                crate::unique_index_sql(name, &index.name, &index.columns)
            } else {
                format!(
                    "CREATE {unique}INDEX IF NOT EXISTS {} ON {} ({})",
                    quoted(&index.name),
                    quoted(name),
                    terms.join(",")
                )
            };
            indexes.push(LogicalIndex {
                name: index.name.clone(),
                unique: index.unique,
                columns: index.columns.clone(),
                create_sql,
                on_conflict: index.on_conflict,
            });
        }
        let mut identity = self.partition_by.clone();
        if !identity.iter().any(|name| name == "id") {
            identity.push("id".into());
        }
        if self.primary_key != identity {
            if indexes.iter().any(|index| {
                index.unique
                    && index
                        .columns
                        .iter()
                        .map(|column| &column.name)
                        .eq(identity.iter())
            }) {
                return invalid(format!("{name}: Rocky supplies the identity index"));
            }
            let index_name = format!("{}_identity", self.name);
            indexes.push(LogicalIndex {
                name: index_name.clone(),
                unique: true,
                columns: identity
                    .iter()
                    .map(|name| IndexColumn {
                        name: name.clone(),
                        descending: false,
                    })
                    .collect(),
                create_sql: format!(
                    "CREATE UNIQUE INDEX IF NOT EXISTS {} ON {} ({})",
                    quoted(&index_name),
                    quoted(name),
                    identity
                        .iter()
                        .map(|name| quoted(name))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                on_conflict: None,
            });
        }
        indexes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(LogicalTable {
            logical: self.logical.clone().unwrap_or_else(|| name.clone()),
            sql_name: name.clone(),
            class: self.class,
            create_sql: format!("CREATE TABLE {} ({})", quoted(name), parts.join(", ")),
            auto_generate: false,
            columns,
            primary_key: self.primary_key.clone(),
            partition_by: self.partition_by.clone(),
            indexes,
            foreign_keys: Vec::new(),
            reuses_keys: self.reuses_keys,
            existence_log: self.existence_log,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Incompatibility, Profile};

    pub(super) const DEFINITION: &str = r#"{
        "format": 2,
        "publisher": "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
        "application": "notes",
        "tables": [
            {
                "name": "notes",
                "class": "mergeable",
                "columns": [
                    {"name": "key", "type": "text", "not_null": true},
                    {"name": "folder", "type": "text", "not_null": true, "default": "inbox", "collation": "nocase"},
                    {"name": "body", "type": "text"},
                    {"name": "pinned", "type": "integer", "not_null": true, "default": false}
                ],
                "primary_key": ["key"],
                "indexes": [{"name": "notes_by_folder", "columns": [{"name": "folder", "descending": false}]}]
            },
            {
                "name": "drafts",
                "class": "local_only",
                "columns": [{"name": "key", "type": "integer", "not_null": true}],
                "primary_key": ["key"]
            }
        ]
    }"#;

    #[test]
    fn a_definition_builds_one_installable_descriptor_on_both_profiles() {
        let definition = SchemaDefinition::parse(DEFINITION.as_bytes()).unwrap();
        let schema = definition.build().unwrap();
        assert_eq!(schema, definition.build().unwrap());
        assert_eq!(schema.source.kind, DEFINITION_SOURCE);
        assert_eq!(
            schema
                .tables
                .iter()
                .map(|t| t.sql_name.as_str())
                .collect::<Vec<_>>(),
            ["drafts", "notes"]
        );
        let notes = schema.table("notes").unwrap();
        assert_eq!(
            notes.create_sql,
            "CREATE TABLE \"notes\" (\"key\" TEXT NOT NULL, \"folder\" TEXT NOT NULL DEFAULT 'inbox' COLLATE NOCASE, \"body\" TEXT, \"pinned\" INTEGER NOT NULL DEFAULT 0, \"id\" INTEGER NOT NULL, \"row_kind\" INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(\"key\"))"
        );
        assert_eq!(notes.column("folder").unwrap().collation, Collation::Nocase);
        assert_eq!(
            crate::declared_collations(&notes.create_sql),
            vec![("folder".to_string(), "NOCASE".to_string())]
        );
        schema.supported_on(Profile::Trusted).unwrap();
        schema.supported_on(Profile::Secure).unwrap();
        let bytes = schema.canonical().unwrap();
        assert_eq!(LogicalSchema::parse(&bytes).unwrap(), schema);
    }

    #[test]
    fn profile_gaps_are_diagnosed_before_installation() {
        let mut definition = SchemaDefinition::parse(DEFINITION.as_bytes()).unwrap();
        definition.tables[0].reuses_keys = true;
        definition.tables[0].columns.push(ColumnDefinition {
            name: "search".into(),
            logical: None,
            affinity: Affinity::Text,
            not_null: false,
            default: None,
            collation: Collation::Binary,
            derived: Some("lower(body)".into()),
            local_only: false,
        });
        let schema = definition.build().unwrap();
        schema.supported_on(Profile::Trusted).unwrap();
        let reasons: Vec<_> = schema
            .diagnose(Profile::Secure)
            .into_iter()
            .map(|error| match error {
                SchemaError::Incompatible { reason, .. } => reason,
                other => panic!("{other}"),
            })
            .collect();
        assert_eq!(reasons, [Incompatibility::DerivedColumn("search".into())]);
        assert!(schema.supported_on(Profile::Secure).is_err());
        // A refused definition never reaches a file: TypedRows refuses it too.
        assert!(crate::TypedRows::new(schema, Vec::new()).is_err());
    }

    #[test]
    fn historical_and_unknown_definitions_require_export_reinstall() {
        for format in [1, 0, 4] {
            let mut definition = SchemaDefinition::parse(DEFINITION.as_bytes()).unwrap();
            definition.format = format;
            let error = definition.build().unwrap_err().to_string();
            assert!(error.contains("row identity"));
            assert!(error.contains("export"));
            assert!(error.contains("reinstall"));
            let mut descriptor = SchemaDefinition::parse(DEFINITION.as_bytes())
                .unwrap()
                .build()
                .unwrap();
            descriptor.format = format;
            assert!(crate::TypedRows::new(descriptor.clone(), vec![]).is_err());
            assert!(descriptor.supported_on(crate::Profile::Trusted).is_err());
        }
    }

    #[test]
    fn malformed_definitions_are_refused() {
        let mut definition = SchemaDefinition::parse(DEFINITION.as_bytes()).unwrap();
        definition.publisher = "zz".into();
        assert!(definition.build().is_err());
        let mut definition = SchemaDefinition::parse(DEFINITION.as_bytes()).unwrap();
        definition.tables[0].columns[0].name = "id; DROP".into();
        assert!(definition.build().is_err());
        let mut definition = SchemaDefinition::parse(DEFINITION.as_bytes()).unwrap();
        definition.tables[0].columns[2].default = Some(serde_json::json!([1]));
        assert!(definition.build().is_err());
        let mut definition = SchemaDefinition::parse(DEFINITION.as_bytes()).unwrap();
        definition.tables[0].primary_key = vec!["absent".into()];
        assert!(definition.build().is_err());
        assert!(SchemaDefinition::parse(br#"{"format":1,"unknown":true}"#).is_err());
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn identity_columns_are_implicit_on_every_table_and_fingerprinted() {
        let mut definition = SchemaDefinition::parse(super::tests::DEFINITION.as_bytes()).unwrap();
        definition.format = 2;
        for table in &mut definition.tables {
            table.columns[0].name = "key".into();
            table.primary_key = vec!["key".into()];
        }
        let schema = definition.build().unwrap();
        assert_eq!(schema.format, crate::ROW_IDENTITY_SCHEMA_FORMAT);
        for table in &schema.tables {
            assert_eq!(table.column("id").unwrap().affinity, Affinity::Integer);
            assert_eq!(
                table.column("row_kind").unwrap().default.as_deref(),
                Some("0")
            );
            assert!(table.indexes.iter().any(LogicalIndex::is_identity));
        }
        schema.supported_on(crate::Profile::Secure).unwrap();
        schema.supported_on(crate::Profile::Trusted).unwrap();
        let mut invalid = schema.clone();
        invalid.tables[0].indexes.clear();
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("identity index")
        );
        let mut reserved = definition.clone();
        reserved.tables[0].columns[0].name = "id".into();
        assert!(
            reserved
                .build()
                .unwrap_err()
                .to_string()
                .contains("do not declare")
        );
        definition.tables[0].primary_key = vec!["id".into()];
        let id_key = definition.build().unwrap();
        assert!(
            !id_key
                .table("notes")
                .unwrap()
                .indexes
                .iter()
                .any(LogicalIndex::is_identity)
        );
        assert_ne!(
            schema.replicated_fingerprint(),
            id_key.replicated_fingerprint()
        );
    }
}

#[cfg(test)]
mod constraint_tests {
    use super::*;
    use crate::{Incompatibility, Profile};

    const SHOP: &str = r#"{
        "format": 2,
        "publisher": "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
        "application": "shop",
        "tables": [
            {
                "name": "lists",
                "class": "mergeable",
                "columns": [{"name": "title", "type": "text", "not_null": true}],
                "primary_key": ["id"]
            },
            {
                "name": "items",
                "class": "mergeable",
                "columns": [
                    {"name": "list", "type": "integer", "not_null": true},
                    {"name": "title", "type": "text", "collation": "nocase"},
                    {"name": "code", "type": "text", "not_null": true},
                    {"name": "moved", "type": "integer"}
                ],
                "primary_key": ["id"],
                "indexes": [
                    {"name": "items_title", "columns": [{"name": "title", "descending": false}], "unique": true, "on_conflict": "set_null"},
                    {"name": "items_code", "columns": [{"name": "list", "descending": false}, {"name": "code", "descending": false}], "unique": true, "on_conflict": "latest_wins"}
                ],
                "foreign_keys": [
                    {"columns": ["list"], "table": "lists", "on_delete": "cascade"},
                    {"columns": ["moved"], "table": "lists", "on_delete": "set_null"}
                ]
            }
        ]
    }"#;
    fn shop() -> SchemaDefinition {
        SchemaDefinition::parse(SHOP.as_bytes()).unwrap()
    }
    fn reason(schema: &LogicalSchema) -> Option<Incompatibility> {
        match schema.supported_on(Profile::Secure) {
            Ok(()) => None,
            Err(SchemaError::Incompatible { reason, .. }) => Some(reason),
            Err(error) => panic!("{error}"),
        }
    }

    #[test]
    fn constraints_build_real_statements_secure_only() {
        let schema = shop().build().unwrap();
        schema.supported_on(Profile::Secure).unwrap();
        let trusted: Vec<_> = schema
            .diagnose(Profile::Trusted)
            .into_iter()
            .map(|error| match error {
                SchemaError::Incompatible { reason, .. } => reason,
                other => panic!("{other}"),
            })
            .collect();
        assert_eq!(
            trusted,
            [
                Incompatibility::UniqueConstraint,
                Incompatibility::ForeignKey
            ]
        );
        let items = schema.table("items").unwrap();
        assert!(items.create_sql.ends_with(
            "FOREIGN KEY(\"list\") REFERENCES \"lists\"(\"id\") DEFERRABLE INITIALLY DEFERRED, FOREIGN KEY(\"moved\") REFERENCES \"lists\"(\"id\") DEFERRABLE INITIALLY DEFERRED)"
        ));
        let title = items
            .indexes
            .iter()
            .find(|index| index.name == "items_title")
            .unwrap();
        assert_eq!(
            title.create_sql,
            "CREATE UNIQUE INDEX IF NOT EXISTS \"items_title\" ON \"items\" (\"title\") WHERE \"row_kind\" = 0"
        );
        assert_eq!(items.foreign_keys[1].on_delete, crate::DELETE_SET_NULL);
        assert!(schema.constrained(schema.table("lists").unwrap()));
        assert_eq!(
            LogicalSchema::parse(&schema.canonical().unwrap()).unwrap(),
            schema
        );
        // Policies are replicated: the same tables without them are another definition.
        let mut plain = shop();
        plain.tables[1].foreign_keys.clear();
        for index in &mut plain.tables[1].indexes {
            index.unique = false;
            index.on_conflict = None;
        }
        let plain = plain.build().unwrap();
        assert_ne!(
            schema.replicated_fingerprint(),
            plain.replicated_fingerprint()
        );
        let mut cascade = shop();
        cascade.tables[1].foreign_keys[1].on_delete = DeleteAction::Cascade;
        cascade.tables[1].columns[3].not_null = true;
        let mut nullable = shop();
        nullable.tables[1].columns[3].not_null = true;
        nullable.tables[1].foreign_keys.pop();
        assert_ne!(
            cascade.build().unwrap().replicated_fingerprint(),
            nullable.build().unwrap().replicated_fingerprint()
        );
    }

    #[test]
    fn definitions_without_constraints_keep_their_bytes() {
        let definition = SchemaDefinition::parse(super::tests::DEFINITION.as_bytes()).unwrap();
        let bytes = String::from_utf8(serde_json::to_vec(&definition).unwrap()).unwrap();
        assert!(!bytes.contains("foreign_keys") && !bytes.contains("on_conflict"));
        let schema = definition.build().unwrap();
        let canonical = String::from_utf8(schema.canonical().unwrap()).unwrap();
        assert!(!canonical.contains("on_conflict"));
        let manifest = schema.manifest(&[crate::DomainId([1; 16])]);
        assert!(
            manifest
                .entities
                .iter()
                .all(|entity| !entity.shape.contains("unique")
                    && !entity.shape.contains("references"))
        );
    }

    #[test]
    fn unresolvable_constraints_are_refused_at_installation() {
        let refused = |change: &dyn Fn(&mut SchemaDefinition)| {
            let mut definition = shop();
            change(&mut definition);
            reason(&definition.build().unwrap())
        };
        assert_eq!(
            refused(&|d| d.tables[1].indexes[1].on_conflict = None),
            Some(Incompatibility::UniqueConstraint)
        );
        // SET NULL needs nullable columns outside the key.
        assert_eq!(
            refused(&|d| d.tables[1].columns[1].not_null = true),
            Some(Incompatibility::ConflictPolicy("items_title".into()))
        );
        assert_eq!(
            refused(&|d| d.tables[1].columns[3].not_null = true),
            Some(Incompatibility::Reference("moved".into()))
        );
        assert_eq!(
            refused(&|d| d.tables[1].foreign_keys[0].columns = vec!["code".into()]),
            Some(Incompatibility::Reference("code".into()))
        );
        assert_eq!(
            refused(&|d| d.tables[0].class = TableClass::LocalOnly),
            Some(Incompatibility::Reference("list".into()))
        );
        assert_eq!(
            refused(&|d| {
                for table in &mut d.tables {
                    table.class = TableClass::LocalOnly;
                    table.indexes.clear();
                }
            }),
            Some(Incompatibility::ForeignKey)
        );
        let mut unknown = shop();
        unknown.tables[1].foreign_keys[0].table = "absent".into();
        assert!(
            unknown
                .build()
                .unwrap_err()
                .to_string()
                .contains("unknown table")
        );
        let mut loose = shop();
        loose.tables[1].indexes[0].unique = false;
        assert!(loose.build().is_err());
        // Descriptors written elsewhere: restricting actions, natural keys
        // and statements other than the generated ones.
        let edited = |change: &dyn Fn(&mut LogicalTable)| {
            let mut schema = shop().build().unwrap();
            let items = schema
                .tables
                .iter_mut()
                .find(|t| t.sql_name == "items")
                .unwrap();
            change(items);
            reason(&schema)
        };
        assert_eq!(
            edited(&|t| t.foreign_keys[0].on_delete = "RESTRICT".into()),
            Some(Incompatibility::Reference("list".into()))
        );
        assert_eq!(
            edited(&|t| t.foreign_keys[0].references = vec!["title".into()]),
            Some(Incompatibility::Reference("list".into()))
        );
        assert_eq!(
            edited(&|t| t.create_sql = t
                .create_sql
                .replace(" DEFERRABLE INITIALLY DEFERRED", " ON DELETE CASCADE")),
            Some(Incompatibility::Reference("list".into()))
        );
        assert_eq!(
            edited(&|t| t.indexes[0].create_sql = t.indexes[0]
                .create_sql
                .replace(" WHERE \"row_kind\" = 0", "")),
            Some(Incompatibility::ConflictPolicy("items_code".into()))
        );
    }
}

#[cfg(test)]
mod partition_tests {
    use super::*;
    fn definition() -> SchemaDefinition {
        SchemaDefinition::parse(br#"{"format":3,"publisher":"1111111111111111111111111111111111111111111111111111111111111111","application":"partitions","tables":[{"name":"items","class":"mergeable","columns":[{"name":"list","type":"text","not_null":true,"collation":"nocase"},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}]}"#).unwrap()
    }
    #[test]
    fn partition_shape_identity_and_manifest_are_bound() {
        let schema = definition().build().unwrap();
        let table = schema.table("items").unwrap();
        assert_eq!(table.partition_by, ["list"]);
        assert_eq!(table.identity_columns(), ["list", "id"]);
        assert!(
            table
                .indexes
                .iter()
                .any(|index| table.is_identity_index(index))
        );
        assert_eq!(
            table.indexes[0].create_sql,
            "CREATE UNIQUE INDEX IF NOT EXISTS \"items_identity\" ON \"items\" (\"list\",\"id\")"
        );
        schema.supported_on(crate::Profile::Secure).unwrap();
        schema.supported_on(crate::Profile::Trusted).unwrap();
        assert_eq!(
            LogicalSchema::parse(&schema.canonical().unwrap()).unwrap(),
            schema
        );
        let mut unpartitioned = definition();
        unpartitioned.tables[0].partition_by.clear();
        assert_ne!(
            schema.manifest(&[]).hash(),
            unpartitioned.build().unwrap().manifest(&[]).hash()
        );
        let mut legacy = schema.clone();
        legacy.format = 2;
        assert!(legacy.validate().is_err());
        let mut wrong_index = schema.clone();
        wrong_index.tables[0].indexes[0].columns.remove(0);
        assert!(wrong_index.validate().is_err());
    }
    #[test]
    fn partitioned_tables_refuse_constraints_and_command_control() {
        let reason = |json: &str| match SchemaDefinition::parse(json.as_bytes())
            .unwrap()
            .build()
            .unwrap()
            .supported_on(crate::Profile::Secure)
        {
            Err(SchemaError::Incompatible { reason, .. }) => reason,
            other => panic!("{other:?}"),
        };
        let items = |extra: &str| {
            format!(
                r#"{{"format":3,"publisher":"1111111111111111111111111111111111111111111111111111111111111111","application":"partitions","tables":[{{"name":"lists","class":"mergeable","columns":[{{"name":"title","type":"text"}}],"primary_key":["id"]}},{{"name":"items","class":"mergeable","columns":[{{"name":"list","type":"text","not_null":true}},{{"name":"key","type":"text","not_null":true}},{{"name":"code","type":"text"}},{{"name":"parent","type":"integer"}}],"primary_key":["list","key"],"partition_by":["list"]{extra}}}]}}"#
            )
        };
        assert_eq!(
            reason(&items(
                r#","indexes":[{"name":"items_code","columns":[{"name":"list","descending":false},{"name":"code","descending":false}],"unique":true,"on_conflict":"latest_wins"}]"#
            )),
            crate::Incompatibility::ConflictPolicy("items_code".into())
        );
        assert_eq!(
            reason(&items(
                r#","foreign_keys":[{"columns":["parent"],"table":"lists","on_delete":"set_null"}]"#
            )),
            crate::Incompatibility::Reference("parent".into())
        );
        let mut commanded = definition();
        commanded.tables[0].class = TableClass::CommandControlled;
        assert_eq!(
            match commanded
                .build()
                .unwrap()
                .supported_on(crate::Profile::Secure)
            {
                Err(SchemaError::Incompatible { reason, .. }) => reason,
                other => panic!("{other:?}"),
            },
            crate::Incompatibility::TableClass(TableClass::CommandControlled)
        );
    }
    #[test]
    fn existence_logs_bind_the_manifest_and_refuse_nondeterministic_support_rows() {
        let shop = |lists: &str, extra: &str| {
            format!(
                r#"{{"format":2,"publisher":"1111111111111111111111111111111111111111111111111111111111111111","application":"logs","tables":[{{"name":"lists","class":"mergeable","columns":[{{"name":"title","type":"text","not_null":true}},{{"name":"owner","type":"integer","not_null":true}}],"primary_key":{lists},"foreign_keys":[{{"columns":["owner"],"table":"owners","on_delete":"cascade"}}]{extra}}},{{"name":"owners","class":"mergeable","columns":[{{"name":"name","type":"text","not_null":true}}],"primary_key":["name"]}}]}}"#
            )
        };
        let build = |json: String| {
            SchemaDefinition::parse(json.as_bytes())
                .unwrap()
                .build()
                .unwrap()
        };
        let plain = build(shop(r#"["id"]"#, ""));
        let logged = build(shop(r#"["id"]"#, r#","existence_log":true"#));
        // Omitted when false: earlier descriptors keep their bytes.
        assert!(
            !String::from_utf8(plain.canonical().unwrap())
                .unwrap()
                .contains("existence_log")
        );
        assert!(
            String::from_utf8(logged.canonical().unwrap())
                .unwrap()
                .contains("\"existence_log\":true")
        );
        assert_ne!(
            plain.manifest(&[crate::DomainId([1; 16])]).hash(),
            logged.manifest(&[crate::DomainId([1; 16])]).hash()
        );
        assert_eq!(
            logged
                .support_tables()
                .iter()
                .map(|table| table.sql_name.as_str())
                .collect::<Vec<_>>(),
            ["lists", "owners"]
        );
        // The virtual owner row needs an id key; so do puppets.
        let refused = |schema: crate::LogicalSchema, profile| match schema.supported_on(profile) {
            Err(SchemaError::Incompatible { table, reason }) => (table, reason),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            refused(logged.clone(), crate::Profile::Secure),
            (
                "owners".into(),
                crate::Incompatibility::SupportRow("primary key (name)".into())
            )
        );
        assert_eq!(
            refused(
                build(shop(r#"["title"]"#, r#","existence_log":true"#)),
                crate::Profile::Secure
            ),
            (
                "lists".into(),
                crate::Incompatibility::SupportRow("primary key (title)".into())
            )
        );
        let mut keyed = logged.clone();
        keyed.tables.iter_mut().for_each(|table| {
            if table.sql_name == "owners" {
                table.primary_key = vec!["id".into()];
                table.create_sql = table
                    .create_sql
                    .replace("PRIMARY KEY(\"name\")", "PRIMARY KEY(\"id\")");
                table.indexes.clear();
            }
        });
        keyed.supported_on(crate::Profile::Secure).unwrap();
        assert!(matches!(
            keyed.diagnose(crate::Profile::Trusted).first(),
            Some(SchemaError::Incompatible {
                reason: crate::Incompatibility::SupportRow(_),
                ..
            })
        ));
        // A build that does not know the entry refuses it as malformed.
        let bytes = String::from_utf8(logged.canonical().unwrap()).unwrap();
        assert!(
            crate::LogicalSchema::parse(
                bytes.replace("existence_log", "existence_logs").as_bytes()
            )
            .is_err()
        );
    }
    #[test]
    fn invalid_and_legacy_partition_declarations_are_refused() {
        for columns in [vec!["absent"], vec!["list", "list"], vec!["row_kind"]] {
            let mut definition = definition();
            definition.tables[0].partition_by = columns.into_iter().map(String::from).collect();
            assert!(definition.build().is_err());
        }
        let mut nullable = definition();
        nullable.tables[0].columns[0].not_null = false;
        assert!(nullable.build().is_err());
        let mut legacy = definition();
        legacy.format = 2;
        assert!(legacy.build().is_err());
        let mut own_id = definition();
        own_id.tables[0].primary_key = vec!["id".into()];
        own_id.tables[0].partition_by = vec!["id".into()];
        let schema = own_id.build().unwrap();
        assert!(schema.table("items").unwrap().indexes.is_empty());
        schema.validate().unwrap();
    }
}
