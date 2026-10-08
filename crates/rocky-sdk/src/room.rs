//! Generates the logical schema from Room's exported schema JSON and a setup
//! policy. See `docs/runtime/logical-schema.md`.
use crate::Failure;
use rocky_core::{
    Affinity, Collation, ColumnClass, IndexColumn, LogicalColumn, LogicalForeignKey, LogicalIndex,
    LogicalSchema, LogicalTable, LogicalView, ROW_IDENTITY_SCHEMA_FORMAT, SchemaSource, TableClass,
    UpdatePolicy, declared_collations,
};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Export {
    format_version: u32,
    database: Database,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Database {
    version: u32,
    identity_hash: String,
    entities: Vec<Entity>,
    #[serde(default)]
    views: Vec<View>,
    #[serde(default)]
    setup_queries: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entity {
    table_name: String,
    create_sql: String,
    fields: Vec<Field>,
    primary_key: PrimaryKey,
    #[serde(default)]
    indices: Vec<Index>,
    #[serde(default)]
    foreign_keys: Vec<ForeignKey>,
    /// Present on FTS entities, which are virtual tables.
    #[serde(default)]
    fts_version: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Field {
    column_name: String,
    affinity: String,
    #[serde(default)]
    not_null: bool,
    #[serde(default)]
    default_value: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrimaryKey {
    #[serde(default)]
    auto_generate: bool,
    column_names: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    name: String,
    unique: bool,
    column_names: Vec<String>,
    #[serde(default)]
    orders: Vec<String>,
    create_sql: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForeignKey {
    table: String,
    on_delete: String,
    on_update: String,
    columns: Vec<String>,
    referenced_columns: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct View {
    view_name: String,
    create_sql: String,
}

/// What an application states once at setup. Everything else is generated.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    /// 64 hexadecimal digits.
    publisher: String,
    application: String,
    #[serde(default)]
    update_policy: Option<UpdatePolicy>,
    /// Class of tables the policy does not name. Without it every table
    /// must be named, so nothing is replicated by omission.
    #[serde(default)]
    default: Option<TableClass>,
    #[serde(default)]
    tables: BTreeMap<String, TablePolicy>,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TablePolicy {
    #[serde(default)]
    class: Option<TableClass>,
    /// The identity to keep after renaming the table.
    #[serde(default)]
    logical: Option<String>,
    #[serde(default)]
    columns: BTreeMap<String, ColumnPolicy>,
    /// The application inserts a key again after deleting it.
    #[serde(default)]
    reuses_keys: bool,
    /// Every device of the domain reads the table's creations and
    /// deletions, so other reader scopes may reference it.
    #[serde(default)]
    existence_log: bool,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ColumnPolicy {
    /// The identity to keep after renaming the column.
    #[serde(default)]
    logical: Option<String>,
    /// An expression over the row's replicated columns.
    #[serde(default)]
    derived: Option<String>,
    #[serde(default)]
    local_only: bool,
}

/// ORM and Rocky bookkeeping, never part of an installed schema.
fn bookkeeping(table: &str) -> bool {
    let table = table.to_ascii_lowercase();
    ["rocky_", "room_", "sqlite_"]
        .iter()
        .any(|prefix| table.starts_with(prefix))
        || table == "android_metadata"
}
fn publisher(hex: &str) -> Result<[u8; 32], Failure> {
    let digits = hex.as_bytes();
    if digits.len() != 64 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return Err("publisher must be 64 hexadecimal digits".into());
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(digits.chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair)?, 16)?;
    }
    Ok(bytes)
}

/// The descriptor of a Room database. It is structurally valid; whether it
/// can be installed is `LogicalSchema::installable`.
pub fn generate_room(export: &str, policy: &str) -> Result<LogicalSchema, Failure> {
    if export.len() > rocky_core::MAX_LOGICAL_SCHEMA_BYTES || policy.len() > 1024 * 1024 {
        return Err("schema source quota".into());
    }
    let export: Export = serde_json::from_str(export)?;
    let policy: Policy = serde_json::from_str(policy)?;
    if export.format_version != 1 {
        return Err("unsupported Room schema format".into());
    }
    let database = export.database;
    // Room writes its identity twice; a hand-edited file tends to differ.
    if !database.setup_queries.is_empty()
        && !database
            .setup_queries
            .iter()
            .any(|query| query.contains(&database.identity_hash))
    {
        return Err("Room identity hash differs from its setup queries".into());
    }
    if let Some(unknown) = policy.tables.keys().find(|name| {
        bookkeeping(name)
            || !database
                .entities
                .iter()
                .any(|entity| entity.table_name == **name)
    }) {
        return Err(format!("policy names unknown or bookkeeping table {unknown}").into());
    }
    let mut tables = Vec::new();
    for entity in database.entities {
        let name = entity.table_name;
        if bookkeeping(&name) {
            continue;
        }
        let empty = TablePolicy::default();
        let table = policy.tables.get(&name).unwrap_or(&empty);
        let class = table.class.or(policy.default).ok_or_else(|| {
            format!("table {name} has no class; name it in the policy or set a default")
        })?;
        if entity.fts_version.is_some() {
            return Err(format!("table {name} is a virtual FTS table").into());
        }
        if let Some(unknown) = table.columns.keys().find(|column| {
            !entity
                .fields
                .iter()
                .any(|field| field.column_name == **column)
        }) {
            return Err(format!("policy names unknown column {name}.{unknown}").into());
        }
        let create_sql = entity.create_sql.replace("${TABLE_NAME}", &name);
        let collations = declared_collations(&create_sql);
        let mut columns = Vec::new();
        for field in entity.fields {
            let column = table.columns.get(&field.column_name);
            let collation = match collations
                .iter()
                .find(|(declared, _)| *declared == field.column_name)
            {
                Some((_, collation)) => Collation::named(collation).ok_or_else(|| {
                    format!(
                        "{name}.{} uses unsupported collation {collation}",
                        field.column_name
                    )
                })?,
                None => Collation::Binary,
            };
            let affinity = match field.affinity.as_str() {
                "TEXT" => Affinity::Text,
                "INTEGER" => Affinity::Integer,
                "REAL" => Affinity::Real,
                "BLOB" => Affinity::Blob,
                "NUMERIC" => Affinity::Numeric,
                other => {
                    return Err(format!("{name}.{} has affinity {other}", field.column_name).into());
                }
            };
            let class = match column {
                Some(ColumnPolicy {
                    derived: Some(rule),
                    local_only: false,
                    ..
                }) => ColumnClass::Derived { rule: rule.clone() },
                Some(ColumnPolicy {
                    derived: None,
                    local_only: true,
                    ..
                }) => ColumnClass::LocalOnly,
                Some(ColumnPolicy {
                    derived: Some(_), ..
                }) => {
                    return Err(format!(
                        "{name}.{} cannot be both derived and local-only",
                        field.column_name
                    )
                    .into());
                }
                _ => ColumnClass::Mergeable,
            };
            columns.push(LogicalColumn {
                logical: column
                    .and_then(|column| column.logical.clone())
                    .unwrap_or_else(|| field.column_name.clone()),
                sql_name: field.column_name,
                affinity,
                not_null: field.not_null,
                default: field.default_value,
                collation,
                class,
            });
        }
        let mut indexes: Vec<_> = entity
            .indices
            .into_iter()
            .map(|index| LogicalIndex {
                columns: index
                    .column_names
                    .into_iter()
                    .enumerate()
                    .map(|(position, column)| IndexColumn {
                        name: column,
                        descending: index
                            .orders
                            .get(position)
                            .is_some_and(|order| order.eq_ignore_ascii_case("DESC")),
                    })
                    .collect(),
                name: index.name,
                unique: index.unique,
                create_sql: index.create_sql.replace("${TABLE_NAME}", &name),
                on_conflict: None,
            })
            .collect();
        indexes.sort_by(|a, b| a.name.cmp(&b.name));
        tables.push(LogicalTable {
            logical: table.logical.clone().unwrap_or_else(|| name.clone()),
            sql_name: name,
            class,
            create_sql,
            auto_generate: entity.primary_key.auto_generate,
            columns,
            primary_key: entity.primary_key.column_names,
            partition_by: Vec::new(),
            indexes,
            foreign_keys: entity
                .foreign_keys
                .into_iter()
                .map(|key| LogicalForeignKey {
                    table: key.table,
                    columns: key.columns,
                    references: key.referenced_columns,
                    on_delete: key.on_delete,
                    on_update: key.on_update,
                })
                .collect(),
            reuses_keys: table.reuses_keys,
            existence_log: table.existence_log,
        });
    }
    for table in &mut tables {
        if table
            .columns
            .iter()
            .any(|column| ["id", "row_kind"].contains(&column.sql_name.as_str()))
        {
            return Err("historical Room identity columns are unsupported; rename application keys, export with the original build and reinstall into a new format-2 database".into());
        }
        for (name, default) in [("id", None), ("row_kind", Some("0".to_string()))] {
            table.columns.push(LogicalColumn {
                logical: name.into(),
                sql_name: name.into(),
                affinity: Affinity::Integer,
                not_null: true,
                default,
                collation: Collation::Binary,
                class: ColumnClass::Mergeable,
            });
        }
        let end = table
            .create_sql
            .find(", PRIMARY KEY")
            .or_else(|| table.create_sql.find(",PRIMARY KEY"))
            .or_else(|| table.create_sql.rfind(')'))
            .ok_or("Room CREATE TABLE shape")?;
        table.create_sql.insert_str(
            end,
            ", id INTEGER NOT NULL, row_kind INTEGER NOT NULL DEFAULT 0",
        );
        let name = format!("{}_identity", table.sql_name);
        table.indexes.push(LogicalIndex {
            name: name.clone(),
            unique: true,
            columns: vec![IndexColumn {
                name: "id".into(),
                descending: false,
            }],
            create_sql: format!(
                "CREATE UNIQUE INDEX IF NOT EXISTS \"{name}\" ON \"{}\" (id)",
                table.sql_name
            ),
            on_conflict: None,
        });
        table.indexes.sort_by(|a, b| a.name.cmp(&b.name));
    }
    tables.sort_by(|a, b| a.logical.cmp(&b.logical));
    let schema = LogicalSchema {
        format: ROW_IDENTITY_SCHEMA_FORMAT,
        publisher: publisher(&policy.publisher)?,
        application: policy.application,
        source: SchemaSource {
            kind: "room".into(),
            version: database.version,
            identity: database.identity_hash,
        },
        update_policy: policy.update_policy.unwrap_or(UpdatePolicy::Changed),
        tables,
        views: database
            .views
            .into_iter()
            .map(|view| LogicalView {
                create_sql: view.create_sql.replace("${VIEW_NAME}", &view.view_name),
                name: view.view_name,
            })
            .collect(),
    };
    schema.validate()?;
    Ok(schema)
}
