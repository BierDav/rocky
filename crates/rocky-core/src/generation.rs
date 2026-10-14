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
        /// `table.field`.
        fields: BTreeSet<String>,
    },
    /// Additions with required fields of existing tables, new or once
    /// nullable, each with a default for rows that never held a value. Its
    /// transition is a cut: earlier generations are no longer admitted.
    Breaking {
        tables: BTreeSet<String>,
        fields: BTreeSet<String>,
        /// `table.field` NOT NULL fields rows of the installed schema lack.
        required: BTreeSet<String>,
    },
}
impl SchemaChange {
    pub fn replicated(&self) -> bool {
        matches!(self, Self::Additive { .. } | Self::Breaking { .. })
    }
    /// Fields rows of the installed schema may lack: `table.field`.
    pub fn added(&self) -> BTreeSet<String> {
        match self {
            Self::Additive { fields, .. } => fields.clone(),
            Self::Breaking {
                fields, required, ..
            } => fields.union(required).cloned().collect(),
            Self::Unchanged | Self::Local => BTreeSet::new(),
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
    RemovedTable(String),
    /// A mergeable table became another class, or the reverse.
    TableClass(String),
    ChangedKey(String),
    RemovedField(String),
    /// Type, nullability or collation of `table.field` changed.
    ChangedField(String),
    /// A required field of an existing table needs a default: rows written
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
            Self::RequiredField(field) => write!(
                f,
                "required field {field} of an existing table needs a default"
            ),
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

/// Classifies `new` against the installed `old`. Names are the identity of
/// tables and fields: a rename is a removal, and a name keeps its type,
/// collation and key role. See decision 0067.
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
        let after = new
            .mergeable()
            .find(|table| table.sql_name == *name)
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
        let column = |table: &crate::LogicalTable, field: &str| {
            table
                .columns
                .iter()
                .find(|column| column.sql_name == field)
                .cloned()
        };
        for (field, shape) in &old_fields {
            match new_fields.get(field) {
                None => return Err(Unsupported::RemovedField(format!("{name}.{field}"))),
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
    // Confirmed rows only change through commands, never a transition.
    let commanded = |schema: &LogicalSchema| -> BTreeMap<String, String> {
        schema
            .tables
            .iter()
            .filter(|table| table.class == TableClass::CommandControlled)
            .map(|table| (table.sql_name.clone(), logical::table_shape(schema, table)))
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
    if required.is_empty() {
        return Ok(SchemaChange::Additive { tables, fields });
    }
    Ok(SchemaChange::Breaking {
        tables,
        fields,
        required,
    })
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
/// installed definition and the next one. Every generation of a lineage
/// stays admitted, except behind a cut. Operations keep their original
/// signed bytes; nothing is re-signed or translated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationTransition {
    pub app: AppId,
    /// The installed manifest's hash.
    pub from: Commitment,
    pub to: Commitment,
    pub before: Generation,
    pub after: Generation,
    /// A [breaking](Self::breaking) cut: operations of every earlier
    /// schema generation are refused.
    pub cut: bool,
    /// When it was signed, in seconds. Transitions that differ only in
    /// this are one generation, whose baseline is the earliest.
    pub time: u64,
}
impl GenerationTransition {
    /// Little-endian numbers in field order.
    pub fn canonical(&self) -> Vec<u8> {
        let mut bytes = b"rocky.transition.v2".to_vec();
        bytes.extend(self.app.0);
        bytes.extend(self.from);
        bytes.extend(self.to);
        for number in [
            self.before.schema,
            self.before.handler,
            self.after.schema,
            self.after.handler,
        ] {
            bytes.extend(number.to_le_bytes());
        }
        bytes.push(u8::from(self.cut));
        bytes.extend(self.time.to_le_bytes());
        bytes
    }
    pub fn from_canonical(bytes: &[u8]) -> Result<Self, crate::ProtocolError> {
        let invalid = || crate::ProtocolError::InvalidManifest;
        let rest = bytes
            .strip_prefix(b"rocky.transition.v2".as_slice())
            .filter(|rest| rest.len() == 16 + 32 + 32 + 4 * 4 + 1 + 8)
            .ok_or_else(invalid)?;
        let (app, rest) = rest.split_at(16);
        let (from, rest) = rest.split_at(32);
        let (to, rest) = rest.split_at(32);
        let (rest, cut) = rest.split_at(4 * 4);
        let (cut, time) = cut.split_at(1);
        let time = u64::from_le_bytes(time.try_into().map_err(|_| invalid())?);
        let cut = match cut {
            [0] => false,
            [1] => true,
            _ => return Err(invalid()),
        };
        let numbers: Vec<u32> = rest
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_le_bytes(*chunk))
            .collect();
        let [before_schema, before_handler, after_schema, after_handler] = numbers[..] else {
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
            cut,
            time,
        };
        transition.validate()?;
        Ok(transition)
    }
    pub fn id(&self) -> Commitment {
        commitment("rocky.transition.id.v1", &self.canonical())
    }
    /// A cut: operations of every earlier schema generation are refused.
    pub fn breaking(&self) -> bool {
        self.cut
    }
    /// Whether `other` makes the same step, signed at another time.
    pub fn same_step(&self, other: &Self) -> bool {
        Self {
            time: other.time,
            ..self.clone()
        } == *other
    }
    /// A step forward: a new manifest, generations that do not go back and
    /// one of them up; a cut moves the schema generation up.
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
            || (self.cut && after.schema == before.schema)
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
            Err(Unsupported::RemovedField("items.name".into()))
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
    fn required_fields_with_defaults_are_breaking() {
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
            Ok(SchemaChange::Breaking {
                tables: BTreeSet::new(),
                fields: ["items.tag".to_string()].into(),
                required: ["items.note".to_string(), "items.position".to_string()].into(),
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
            cut: false,
            time: 1_000,
        };
        assert!(transition.validate().is_ok());
        let later = GenerationTransition {
            time: 2_000,
            ..transition.clone()
        };
        assert!(later.same_step(&transition) && later.id() != transition.id());
        assert_eq!(
            GenerationTransition::from_canonical(&transition.canonical()),
            Ok(transition.clone())
        );
        // A cut admits only the new generation.
        let cut = GenerationTransition {
            cut: true,
            ..transition.clone()
        };
        assert!(cut.validate().is_ok() && cut.breaking() && !transition.breaking());
        assert_eq!(
            GenerationTransition::from_canonical(&cut.canonical()),
            Ok(cut.clone())
        );
        // One current format: another version or flag is refused.
        let mut other = cut.canonical();
        other[b"rocky.transition.v".len()] = b'1';
        assert!(GenerationTransition::from_canonical(&other).is_err());
        let mut flag = cut.canonical();
        let at = flag.len() - 9;
        flag[at] = 2;
        assert!(GenerationTransition::from_canonical(&flag).is_err());
        for invalid in [
            GenerationTransition {
                cut: true,
                after: Generation {
                    schema: 1,
                    handler: 2,
                },
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
