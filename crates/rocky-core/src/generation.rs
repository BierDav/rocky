use crate::{ColumnClass, LogicalColumn, LogicalForeignKey, LogicalSchema, LogicalTable};
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
    AddedReference(String),
    FilledReference(String),
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
            Self::AddedReference(key) => write!(
                f,
                "foreign key {key} would constrain existing rows; declare it on a new optional field"
            ),
            Self::FilledReference(key) => write!(
                f,
                "foreign key {key} would hold a required field's default on existing rows; keep the field optional or drop its default"
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
    classify_carrying(old, new, |_, _| true)
}
pub fn classify_carrying(
    old: &LogicalSchema,
    new: &LogicalSchema,
    carried: impl Fn(&LogicalTable, Option<&LogicalColumn>) -> bool,
) -> Result<SchemaChange, Unsupported> {
    if old.publisher != new.publisher || old.application != new.application {
        return Err(Unsupported::Application);
    }
    let change = if old == new {
        SchemaChange::Unchanged
    } else if old.replicated_fingerprint() == new.replicated_fingerprint() {
        SchemaChange::Local
    } else {
        SchemaChange::Replicated
    };
    let carries_everything = old.typed().all(|table| {
        carried(table, None)
            && table
                .columns
                .iter()
                .filter(|column| column.class == ColumnClass::Replicated)
                .all(|column| carried(table, Some(column)))
    });
    if change != SchemaChange::Replicated && carries_everything {
        return Ok(change);
    }
    let carried_parent = |key: &LogicalForeignKey| {
        old.typed()
            .find(|table| table.sql_name.eq_ignore_ascii_case(&key.table))
            .is_some_and(|parent| {
                carried(parent, None)
                    && key.references.iter().all(|name| {
                        parent
                            .column(name)
                            .is_some_and(|column| carried(parent, Some(column)))
                    })
            })
    };
    let defaulted = |column: &LogicalColumn| {
        column
            .default
            .as_deref()
            .is_some_and(|value| !value.eq_ignore_ascii_case("NULL"))
    };
    for after in new.typed() {
        let Some(before) = old
            .typed()
            .find(|table| table.sql_name == after.sql_name && carried(table, None))
        else {
            continue;
        };
        let held = |name: &str| before.column(name).filter(|was| carried(before, Some(was)));
        for now in after
            .columns
            .iter()
            .filter(|column| column.class == ColumnClass::Replicated)
        {
            let field = || format!("{}.{}", after.sql_name, now.sql_name);
            match held(&now.sql_name) {
                Some(was) if was.affinity != now.affinity || was.collation != now.collation => {
                    return Err(Unsupported::ChangedField(field()));
                }
                Some(was) if was.not_null || !now.not_null || defaulted(now) => {}
                None if !now.not_null || defaulted(now) => {}
                _ => return Err(Unsupported::RequiredField(field())),
            }
        }
        let starts_empty = |name: &String| {
            held(name).is_none()
                && after
                    .column(name)
                    .is_some_and(|column| !column.not_null && !defaulted(column))
        };
        let filled = |name: &String| {
            held(name).is_some_and(|was| !was.not_null)
                && after
                    .column(name)
                    .is_some_and(|column| column.not_null && defaulted(column))
        };
        for key in &after.foreign_keys {
            if key.columns.iter().any(starts_empty) {
                continue;
            }
            let named = || {
                format!(
                    "{}({}) -> {}",
                    after.sql_name,
                    key.columns.join(","),
                    key.table
                )
            };
            let kept = carried_parent(key)
                && before.foreign_keys.iter().any(|was| {
                    same_reference(was, key) && was.columns.iter().all(|name| held(name).is_some())
                });
            if !kept {
                return Err(Unsupported::AddedReference(named()));
            }
            if key.columns.iter().any(filled) {
                return Err(Unsupported::FilledReference(named()));
            }
        }
    }
    Ok(change)
}
fn same_reference(was: &LogicalForeignKey, now: &LogicalForeignKey) -> bool {
    let same = |a: &[String], b: &[String]| {
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.eq_ignore_ascii_case(b))
    };
    was.table.eq_ignore_ascii_case(&now.table)
        && same(&was.columns, &now.columns)
        && same(&was.references, &now.references)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation {
    pub schema: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Affinity, Collation, ColumnClass, LOGICAL_SCHEMA_FORMAT, LogicalColumn, LogicalForeignKey,
        LogicalTable, SchemaSource, TableClass,
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

    fn reference(column: &str, parent: &str, on_delete: &str, deferred: bool) -> LogicalForeignKey {
        LogicalForeignKey {
            table: parent.into(),
            columns: vec![column.into()],
            references: vec!["id".into()],
            on_delete: on_delete.into(),
            on_update: "NO ACTION".into(),
            deferred,
        }
    }
    fn linked(columns: &[&str], keys: Vec<LogicalForeignKey>) -> LogicalSchema {
        let mut tags = table(
            "tags",
            std::iter::once(column("id", true))
                .chain(columns.iter().map(|name| column(name, false)))
                .collect(),
        );
        tags.foreign_keys = keys;
        schema(vec![
            items(&[("name", true)]),
            table("lists", vec![column("id", true)]),
            tags,
        ])
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_foreign_key_never_constrains_rows_written_before_it() {
        let unlinked = linked(&["item"], Vec::new());
        let cascade = reference("item", "items", crate::DELETE_CASCADE, false);
        let refused = |key: &str| Err(Unsupported::AddedReference(key.into()));
        assert_eq!(
            classify(&unlinked, &linked(&["item"], vec![cascade.clone()])),
            refused("tags(item) -> items"),
            "a reference over a field existing rows already hold is refused"
        );
        let linked_items = linked(&["item"], vec![cascade.clone()]);
        assert_eq!(
            classify(
                &linked_items,
                &linked(
                    &["item"],
                    vec![reference("item", "lists", crate::DELETE_CASCADE, false)]
                )
            ),
            refused("tags(item) -> lists"),
            "a reference moved to another table is a new reference"
        );
        assert_eq!(
            classify(
                &linked_items,
                &linked(
                    &["item"],
                    vec![reference("item", "items", crate::DELETE_SET_NULL, true)]
                )
            ),
            Ok(SchemaChange::Replicated),
            "a kept reference may change its delete action and deferral"
        );
        assert_eq!(
            classify(&linked_items, &unlinked),
            Ok(SchemaChange::Replicated),
            "dropping a reference constrains nothing"
        );
        let added = linked(
            &["item", "owner"],
            vec![reference("owner", "items", crate::DELETE_CASCADE, false)],
        );
        assert_eq!(
            classify(&unlinked, &added),
            Ok(SchemaChange::Replicated),
            "a reference over a new optional field starts empty on every existing row"
        );
        let mut defaulted = added.clone();
        defaulted.tables[2].columns[2].default = Some("'x'".into());
        assert_eq!(
            classify(&unlinked, &defaulted),
            refused("tags(owner) -> items"),
            "a new field whose default fills existing rows is constrained like an old one"
        );
        let mut filled = linked_items.clone();
        filled.tables[2].columns[1].not_null = true;
        filled.tables[2].columns[1].default = Some("x'00'".into());
        assert_eq!(
            classify(&linked_items, &filled),
            Err(Unsupported::FilledReference("tags(item) -> items".into())),
            "a kept reference never receives a default written into rows that held no value"
        );
        let mut optional_default = linked_items.clone();
        optional_default.tables[2].columns[1].default = Some("x'00'".into());
        assert_eq!(
            classify(&linked_items, &optional_default),
            Ok(SchemaChange::Replicated),
            "a default on an optional field leaves existing rows as they are"
        );
        let mut fresh = unlinked.clone();
        fresh.tables.pop();
        assert_eq!(
            classify(&fresh, &linked_items),
            Ok(SchemaChange::Replicated),
            "a new table may reference any table"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_field_removed_on_the_switch_path_starts_empty_like_a_new_one() {
        let held = linked(&["item"], Vec::new());
        let mut readded = linked(
            &["item"],
            vec![reference("item", "items", crate::DELETE_CASCADE, false)],
        );
        readded.tables[2].columns[1].affinity = Affinity::Blob;
        assert_eq!(
            classify(&held, &readded),
            Err(Unsupported::ChangedField("tags.item".into())),
            "a field held along the whole path keeps its type"
        );
        let item_removed_between = |table: &LogicalTable, column: Option<&LogicalColumn>| {
            table.sql_name != "tags" || column.is_none_or(|column| column.sql_name != "item")
        };
        assert_eq!(
            classify_carrying(&held, &readded, item_removed_between),
            Ok(SchemaChange::Replicated),
            "a field removed and added again on the path carries no value, so it may change type and gain a reference"
        );
        let linked_items = linked(
            &["item"],
            vec![reference("item", "items", crate::DELETE_CASCADE, false)],
        );
        let mut defaulted = linked_items.clone();
        defaulted.tables[2].columns[1].default = Some("x'00'".into());
        assert_eq!(
            classify_carrying(&linked_items, &defaulted, item_removed_between),
            Err(Unsupported::AddedReference("tags(item) -> items".into())),
            "a readded field whose default fills every row is constrained like an old one"
        );
        let mut required = held.clone();
        required.tables[2].columns.push(column("label", true));
        assert_eq!(
            classify(&held, &required),
            Err(Unsupported::RequiredField("tags.label".into()))
        );
        assert_eq!(
            classify_carrying(&held, &required, |table, _| table.sql_name != "tags"),
            Ok(SchemaChange::Replicated),
            "a table removed and added again on the path starts with no rows"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_reference_to_a_table_removed_on_the_switch_path_is_a_new_reference() {
        let linked_items = linked(
            &["item"],
            vec![reference("item", "items", crate::DELETE_CASCADE, false)],
        );
        let items_removed_between =
            |table: &LogicalTable, _: Option<&LogicalColumn>| table.sql_name != "items";
        assert_eq!(
            classify(&linked_items, &linked_items),
            Ok(SchemaChange::Unchanged)
        );
        let refused = Err(Unsupported::AddedReference("tags(item) -> items".into()));
        assert_eq!(
            classify_carrying(&linked_items, &linked_items, items_removed_between),
            refused,
            "a parent removed and added again on the path starts empty, so the rows referencing it would break the reference"
        );
        let mut deferred = linked_items.clone();
        deferred.tables[2].foreign_keys[0].deferred = true;
        assert_eq!(
            classify_carrying(&linked_items, &deferred, items_removed_between),
            refused
        );
        assert_eq!(
            classify_carrying(&linked_items, &linked_items, |table, _| {
                table.sql_name != "lists"
            }),
            Ok(SchemaChange::Unchanged),
            "a path that removes only a table no reference touches leaves the schema unchanged"
        );
    }
}
