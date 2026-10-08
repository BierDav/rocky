//! Generations of an application definition: what changed between two
//! installed schemas, and the transition a publisher signs before a
//! replicated change activates. See `docs/runtime/generations.md`.
use crate::{AppId, CommandSet, Commitment, LogicalSchema, TableClass, commitment, logical};
use std::collections::{BTreeMap, BTreeSet};

/// How a new schema relates to the installed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaChange {
    /// The same installed schema.
    Unchanged,
    /// Local tables, indexes or SQL spelling: replicas agree as before.
    /// Only the local projection is rebuilt.
    Local,
    /// Replicated additions, which need a signed transition: new mergeable
    /// tables, and nullable fields of existing ones.
    Additive {
        tables: BTreeSet<String>,
        /// `table.field`, by logical name.
        fields: BTreeSet<String>,
    },
}
impl SchemaChange {
    pub fn replicated(&self) -> bool {
        matches!(self, Self::Additive { .. })
    }
}

/// A change no transition can make: replicas holding data of the installed
/// schema would no longer agree on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// Another publisher or application name.
    Application,
    UpdatePolicy,
    RemovedTable(String),
    /// A mergeable table became another class, or the reverse.
    TableClass(String),
    ChangedKey(String),
    RemovedField(String),
    /// Type, nullability or collation of `table.field` changed.
    ChangedField(String),
    /// A new field of an existing table must be nullable: rows written
    /// before it hold no value for it.
    RequiredField(String),
    /// A declared command version was removed or redefined.
    ChangedCommand(String),
    /// Commands changed without a higher handler generation.
    HandlerGeneration,
    /// The new generation does not follow the installed one.
    Generation,
    /// A command-controlled table was added, removed or reshaped; its
    /// confirmed rows have no transition.
    CommandTable(String),
}
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Application => f.write_str("another publisher or application"),
            Self::UpdatePolicy => f.write_str("the update policy changed"),
            Self::RemovedTable(table) => write!(f, "table {table} was removed"),
            Self::TableClass(table) => write!(f, "table {table} changed class"),
            Self::ChangedKey(table) => write!(f, "the key of {table} changed"),
            Self::RemovedField(field) => write!(f, "field {field} was removed"),
            Self::ChangedField(field) => write!(f, "field {field} changed type"),
            Self::RequiredField(field) => {
                write!(f, "new field {field} of an existing table must be nullable")
            }
            Self::ChangedCommand(command) => {
                write!(f, "command {command} was removed or redefined")
            }
            Self::HandlerGeneration => {
                f.write_str("changed commands need a higher handler generation")
            }
            Self::Generation => f.write_str("the generation does not follow the installed one"),
            Self::CommandTable(table) => {
                write!(f, "command-controlled table {table} cannot change")
            }
        }
    }
}
impl std::error::Error for Unsupported {}

/// Classifies `new` against the installed `old`. Renames are removals:
/// logical names are the identity of tables and fields.
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
            .find(|table| table.logical == name)
            .map(|table| table.class)
    };
    let (mut tables, mut fields) = (BTreeSet::new(), BTreeSet::new());
    for table in new.mergeable() {
        match class(old, &table.logical) {
            None => {
                tables.insert(table.logical.clone());
            }
            Some(TableClass::Mergeable) => {}
            Some(_) => return Err(Unsupported::TableClass(table.logical.clone())),
        }
    }
    for before in old.mergeable() {
        let name = &before.logical;
        let after = new
            .mergeable()
            .find(|table| table.logical == *name)
            .ok_or_else(|| match class(new, name) {
                Some(_) => Unsupported::TableClass(name.clone()),
                None => Unsupported::RemovedTable(name.clone()),
            })?;
        let (old_key, new_key) = (logical::key_shape(before), logical::key_shape(after));
        if old_key != new_key
            || before.partition_by != after.partition_by
            || before.partition_group != after.partition_group
        {
            return Err(Unsupported::ChangedKey(name.clone()));
        }
        let old_fields = logical::register_shapes(before);
        let new_fields = logical::register_shapes(after);
        for (field, shape) in &old_fields {
            match new_fields.get(field) {
                None => return Err(Unsupported::RemovedField(format!("{name}.{field}"))),
                Some(other) if other != shape => {
                    return Err(Unsupported::ChangedField(format!("{name}.{field}")));
                }
                Some(_) => {}
            }
        }
        for field in new_fields
            .keys()
            .filter(|field| !old_fields.contains_key(*field))
        {
            let column = after.columns.iter().find(|column| column.logical == *field);
            if column.is_none_or(|column| column.not_null) {
                return Err(Unsupported::RequiredField(format!("{name}.{field}")));
            }
            fields.insert(format!("{name}.{field}"));
        }
    }
    // Confirmed rows only change through commands, never a transition.
    let commanded = |schema: &LogicalSchema| -> BTreeMap<String, String> {
        schema
            .tables
            .iter()
            .filter(|table| table.class == TableClass::CommandControlled)
            .map(|table| (table.logical.clone(), logical::table_shape(schema, table)))
            .collect()
    };
    let (before, after) = (commanded(old), commanded(new));
    if let Some(name) = before
        .keys()
        .chain(after.keys())
        .find(|name| before.get(*name) != after.get(*name))
    {
        return Err(Unsupported::CommandTable(name.clone()));
    }
    Ok(SchemaChange::Additive { tables, fields })
}

/// Classifies the commands of a new definition. Every declared version
/// stays, byte for byte: tickets submitted under it must remain
/// executable. Any change needs a higher handler generation; a higher one
/// with the same commands is a new handler implementation.
pub fn classify_commands(old: &CommandSet, new: &CommandSet) -> Result<bool, Unsupported> {
    let declared = |set: &CommandSet| -> BTreeMap<(String, u32), Vec<u8>> {
        set.descriptors()
            .iter()
            .map(|descriptor| {
                (
                    (descriptor.name.clone(), descriptor.version),
                    descriptor.canonical(),
                )
            })
            .collect()
    };
    let (before, after) = (declared(old), declared(new));
    for (key, canonical) in &before {
        if after.get(key) != Some(canonical) {
            return Err(Unsupported::ChangedCommand(format!("{} v{}", key.0, key.1)));
        }
    }
    match new.handler().cmp(&old.handler()) {
        std::cmp::Ordering::Less => Err(Unsupported::HandlerGeneration),
        std::cmp::Ordering::Equal if before != after => Err(Unsupported::HandlerGeneration),
        std::cmp::Ordering::Equal => Ok(false),
        std::cmp::Ordering::Greater => Ok(true),
    }
}

/// The schema and handler generations of a definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation {
    pub schema: u32,
    pub handler: u32,
}

/// What a publisher signs to activate a replicated or handler change: the
/// installed definition, the next one, and the oldest schema generation
/// whose operations replicas still admit. Operations keep their original
/// signed bytes; nothing is re-signed or translated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationTransition {
    pub app: AppId,
    /// The installed manifest's hash.
    pub from: Commitment,
    pub to: Commitment,
    pub before: Generation,
    pub after: Generation,
    /// Operations of schema generations below this are refused; their
    /// authors rebootstrap after upgrading. At most `before.schema`.
    pub horizon: u32,
}
impl GenerationTransition {
    /// Little-endian numbers in field order.
    pub fn canonical(&self) -> Vec<u8> {
        let mut bytes = b"rocky.transition.v1".to_vec();
        bytes.extend(self.app.0);
        bytes.extend(self.from);
        bytes.extend(self.to);
        for number in [
            self.before.schema,
            self.before.handler,
            self.after.schema,
            self.after.handler,
            self.horizon,
        ] {
            bytes.extend(number.to_le_bytes());
        }
        bytes
    }
    pub fn from_canonical(bytes: &[u8]) -> Result<Self, crate::ProtocolError> {
        let invalid = || crate::ProtocolError::InvalidManifest;
        let rest = bytes
            .strip_prefix(b"rocky.transition.v1".as_slice())
            .filter(|rest| rest.len() == 16 + 32 + 32 + 5 * 4)
            .ok_or_else(invalid)?;
        let (app, rest) = rest.split_at(16);
        let (from, rest) = rest.split_at(32);
        let (to, rest) = rest.split_at(32);
        let numbers: Vec<u32> = rest
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_le_bytes(*chunk))
            .collect();
        let [
            before_schema,
            before_handler,
            after_schema,
            after_handler,
            horizon,
        ] = numbers[..]
        else {
            return Err(invalid());
        };
        let transition = Self {
            app: AppId(app.try_into().map_err(|_| invalid())?),
            from: from.try_into().map_err(|_| invalid())?,
            to: to.try_into().map_err(|_| invalid())?,
            before: Generation {
                schema: before_schema,
                handler: before_handler,
            },
            after: Generation {
                schema: after_schema,
                handler: after_handler,
            },
            horizon,
        };
        transition.validate()?;
        Ok(transition)
    }
    pub fn id(&self) -> Commitment {
        commitment("rocky.transition.id.v1", &self.canonical())
    }
    /// A step forward: a new manifest, generations that do not go back and
    /// one of them up, and a horizon inside the generations it spans.
    pub fn validate(&self) -> Result<(), crate::ProtocolError> {
        let (before, after) = (self.before, self.after);
        let forward = after.schema >= before.schema
            && after.handler >= before.handler
            && after != before
            && after.schema <= before.schema.saturating_add(1);
        if self.from == self.to
            || !forward
            || before.schema == 0
            || before.handler == 0
            || self.horizon == 0
            || self.horizon > before.schema
        {
            return Err(crate::ProtocolError::InvalidManifest);
        }
        Ok(())
    }
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
            logical: name.into(),
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
                let key = if column.logical == "id" {
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
            logical: name.into(),
            sql_name: name.into(),
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
    fn command_controlled_tables_have_no_transition() {
        let stock = |columns: &[(&str, bool)]| {
            let mut all = vec![column("id", true)];
            all.extend(
                columns
                    .iter()
                    .map(|(name, required)| column(name, *required)),
            );
            let mut stock = table("stock", all);
            stock.class = TableClass::CommandControlled;
            stock
        };
        let old = schema(vec![items(&[("name", true)]), stock(&[("left", true)])]);
        // Mergeable additions still classify; the command table is untouched.
        let added = schema(vec![
            items(&[("name", true), ("note", false)]),
            stock(&[("left", true)]),
        ]);
        assert!(matches!(
            classify(&old, &added),
            Ok(SchemaChange::Additive { .. })
        ));
        for new in [
            schema(vec![
                items(&[("name", true)]),
                stock(&[("left", true), ("note", false)]),
            ]),
            schema(vec![items(&[("name", true)])]),
        ] {
            assert_eq!(
                classify(&old, &new),
                Err(Unsupported::CommandTable("stock".into()))
            );
        }
        let without = schema(vec![items(&[("name", true)])]);
        assert_eq!(
            classify(&without, &old),
            Err(Unsupported::CommandTable("stock".into()))
        );
        let mut merged = old.clone();
        merged.tables[1].class = TableClass::Mergeable;
        assert_eq!(
            classify(&old, &merged),
            Err(Unsupported::TableClass("stock".into()))
        );
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
                Unsupported::RemovedField("items.name".into()),
            ),
            (
                schema(vec![table("tags", vec![column("id", true)])]),
                Unsupported::RemovedTable("items".into()),
            ),
        ] {
            assert_eq!(classify(&old, &new), Err(refusal));
        }
        let mut other = old.clone();
        other.publisher = [2; 32];
        assert_eq!(classify(&old, &other), Err(Unsupported::Application));
    }

    #[test]
    fn transitions_step_forward_and_round_trip() {
        let transition = GenerationTransition {
            app: AppId([3; 16]),
            from: [4; 32],
            to: [5; 32],
            before: Generation {
                schema: 1,
                handler: 1,
            },
            after: Generation {
                schema: 2,
                handler: 1,
            },
            horizon: 1,
        };
        assert!(transition.validate().is_ok());
        assert_eq!(
            GenerationTransition::from_canonical(&transition.canonical()),
            Ok(transition.clone())
        );
        for invalid in [
            GenerationTransition {
                horizon: 2,
                ..transition.clone()
            },
            GenerationTransition {
                to: [4; 32],
                ..transition.clone()
            },
            GenerationTransition {
                after: transition.before,
                ..transition.clone()
            },
            GenerationTransition {
                after: Generation {
                    schema: 3,
                    handler: 1,
                },
                ..transition.clone()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }
}
