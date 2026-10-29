//! Typed tables as secure rows: one row per business key, one field per
//! replicated column. The row contract that `EncryptedReplica` admits and a
//! secure SQL session commits. See `docs/runtime/secure-sql.md`.
use crate::{
    ApplicationManifest, Commitment, DomainId, KeyColumn, LogicalSchema, RowApplication, RowChange,
    RowFields, RowWrite, SchemaError, SqlValue, commitment, key_components,
};
use std::collections::{BTreeMap, BTreeSet};

mod source;

/// One mergeable table: its field per column, in declared order.
#[derive(Clone, Debug)]
pub struct TypedTable {
    pub sql_name: String,
    pub id: Commitment,
    pub fields: Vec<Commitment>,
    /// Column positions of the primary key, in key order.
    pub key: Vec<usize>,
    key_columns: Vec<KeyColumn>,
    /// The register fields of each foreign key, which publish as one unit.
    units: Vec<BTreeSet<Commitment>>,
}
impl TypedTable {
    /// The register fields of each foreign key, which publish together.
    pub fn units(&self) -> &[BTreeSet<Commitment>] {
        &self.units
    }
}
/// A logical schema's typed tables as a `RowApplication`.
#[derive(Clone, Debug)]
pub struct TypedRows {
    schema: LogicalSchema,
    domains: Vec<DomainId>,
    tables: BTreeMap<Commitment, TypedTable>,
    /// The schema generation of the signed manifest; 1 until a transition.
    generation: u32,
    /// Nullable fields a later generation added: rows written before it
    /// lack them, and they read as NULL.
    added: BTreeSet<Commitment>,
}
impl TypedRows {
    /// The schema must be supported on the secure profile.
    pub fn new(schema: LogicalSchema, domains: Vec<DomainId>) -> Result<Self, SchemaError> {
        schema.supported_on(crate::Profile::Secure)?;
        let mut tables = BTreeMap::new();
        for table in schema.typed() {
            let key: Vec<usize> = table
                .primary_key
                .iter()
                .filter_map(|name| {
                    table
                        .columns
                        .iter()
                        .position(|c| c.sql_name.eq_ignore_ascii_case(name))
                })
                .collect();
            let id = schema.table_id(table);
            tables.insert(
                id,
                TypedTable {
                    sql_name: table.sql_name.clone(),
                    id,
                    fields: table
                        .columns
                        .iter()
                        .map(|column| schema.field_id(table, column))
                        .collect(),
                    key_columns: key
                        .iter()
                        .map(|index| KeyColumn {
                            affinity: table.columns[*index].affinity,
                            collation: table.columns[*index].collation,
                        })
                        .collect(),
                    units: table
                        .foreign_keys
                        .iter()
                        .map(|foreign| {
                            table
                                .registers()
                                .filter(|column| foreign.columns.contains(&column.sql_name))
                                .map(|column| schema.field_id(table, column))
                                .collect::<BTreeSet<_>>()
                        })
                        .filter(|unit| !unit.is_empty())
                        .collect(),
                    key,
                },
            );
        }
        Ok(Self {
            added: fillable(&schema),
            schema,
            domains,
            tables,
            generation: 1,
        })
    }
    /// These tables at schema generation `generation`, where `added` names
    /// the fields transitions added to, or required of, existing tables.
    /// Rows without such a field read NULL, or a required field's default.
    pub fn at_generation(
        mut self,
        generation: u32,
        added: BTreeSet<Commitment>,
    ) -> Result<Self, SchemaError> {
        if generation == 0 || !added.is_subset(&fillable(&self.schema)) {
            return Err(SchemaError::Invalid(
                "added fields must be nullable registers, or required ones with a default".into(),
            ));
        }
        self.generation = generation;
        self.added = fillable(&self.schema);
        Ok(self)
    }
    pub fn generation(&self) -> u32 {
        self.generation
    }
    pub fn added(&self) -> &BTreeSet<Commitment> {
        &self.added
    }
    pub fn schema(&self) -> &LogicalSchema {
        &self.schema
    }
    pub fn table(&self, id: &Commitment) -> Option<&TypedTable> {
        self.tables.get(id)
    }
    pub fn tables(&self) -> impl Iterator<Item = &TypedTable> {
        self.tables.values()
    }
    /// The stable row identity of a key, bound to its domain and table.
    pub fn row_id(
        &self,
        domain: DomainId,
        table: &TypedTable,
        values: &[SqlValue],
    ) -> Result<Commitment, String> {
        let key: Vec<SqlValue> = table
            .key
            .iter()
            .filter_map(|index| values.get(*index).cloned())
            .collect();
        bound_row(domain, table.id, &table.key_columns, &key)
            .map_err(|error| format!("key of {}: {error}", table.sql_name))
    }

    /// Row fields of a typed row, in declared column order.
    pub fn fields(&self, table: &TypedTable, values: &[SqlValue]) -> Result<RowFields, String> {
        if values.len() != table.fields.len() {
            return Err(format!(
                "{} has {} columns",
                table.sql_name,
                table.fields.len()
            ));
        }
        Ok(table
            .fields
            .iter()
            .zip(values)
            .map(|(field, value)| {
                let mut bytes = Vec::new();
                value.encode(&mut bytes);
                (*field, bytes)
            })
            .collect())
    }
    /// The typed row of `fields`: every column present, in stored form.
    pub fn values(&self, table: &TypedTable, fields: &RowFields) -> Result<Vec<SqlValue>, String> {
        let logical = self
            .schema
            .tables
            .iter()
            .find(|candidate| candidate.sql_name == table.sql_name)
            .ok_or_else(|| format!("{} is not installed", table.sql_name))?;
        let absent = table
            .fields
            .iter()
            .filter(|field| !fields.contains_key(*field))
            .count();
        if fields.len() + absent != table.fields.len() {
            return Err(format!("{} row holds a foreign field", table.sql_name));
        }
        table
            .fields
            .iter()
            .zip(&logical.columns)
            .map(|(field, column)| {
                column_value(
                    &table.sql_name,
                    &column.sql_name,
                    column.affinity,
                    column.not_null,
                    column.default.as_deref(),
                    self.added.contains(field),
                    fields.get(field).map(Vec::as_slice),
                )
            })
            .collect()
    }
    /// The immutable incarnation carried in both signed row images.
    pub fn identity(&self, change: &RowChange) -> Result<Option<crate::RowIdentity>, String> {
        let table = self
            .tables
            .get(&change.scope.table)
            .ok_or("unknown typed table")?;
        let logical = self
            .schema
            .table(&table.sql_name)
            .ok_or("unknown typed table")?;
        let id_index = logical
            .columns
            .iter()
            .position(|column| column.sql_name.eq_ignore_ascii_case("id"))
            .ok_or("missing identity column")?;
        let kind_index = logical
            .columns
            .iter()
            .position(|column| column.sql_name.eq_ignore_ascii_case("row_kind"))
            .ok_or("missing row kind column")?;
        let mut identity = None;
        for fields in change.before.iter().chain(change.after.iter()) {
            let values = self.values(table, fields)?;
            let SqlValue::Integer(id) = values[id_index] else {
                return Err("id must be an integer".into());
            };
            let id = crate::RowIdentity::from_integer(id).map_err(|error| error.to_string())?;
            if values[kind_index] != SqlValue::Integer(0) {
                return Err(
                    "application rows require row_kind = 0; other kinds are reserved".into(),
                );
            }
            if identity.is_some_and(|previous| previous != id) {
                return Err("a row's id cannot change".into());
            }
            identity = Some(id);
        }
        identity
            .map(Some)
            .ok_or_else(|| "identity row has no image".into())
    }
    pub fn partition(
        &self,
        table: &TypedTable,
        values: &[SqlValue],
    ) -> Result<Option<crate::PartitionTuple>, String> {
        let logical = self
            .schema
            .table(&table.sql_name)
            .ok_or("unknown typed table")?;
        if logical.partition_by.is_empty() {
            return Ok(None);
        }
        crate::PartitionTuple::new(
            logical
                .partition_by
                .iter()
                .map(|name| {
                    let position = logical
                        .columns
                        .iter()
                        .position(|column| column.sql_name.eq_ignore_ascii_case(name))
                        .ok_or("unknown partition column")?;
                    let column = &logical.columns[position];
                    let value = values
                        .get(position)
                        .ok_or("missing partition value")?
                        .clone();
                    let value = crate::PartitionValue::new(
                        KeyColumn {
                            affinity: column.affinity,
                            collation: column.collation,
                        },
                        value,
                    )
                    .map_err(|error| error.to_string())?;
                    Ok((self.schema.field_id(logical, column), value))
                })
                .collect::<Result<Vec<_>, String>>()?,
        )
        .map(Some)
        .map_err(|error| error.to_string())
    }
    pub fn change_partition(
        &self,
        change: &RowChange,
    ) -> Result<Option<crate::PartitionTuple>, String> {
        let table = self
            .tables
            .get(&change.scope.table)
            .ok_or("unknown typed table")?;
        let mut tuple = None;
        for fields in change.before.iter().chain(change.after.iter()) {
            let next = self.partition(table, &self.values(table, fields)?)?;
            if next.as_ref().is_some_and(|tuple| {
                tuple.scope(change.scope.domain, table.id).ok() != Some(change.scope)
            }) {
                return Err("row image differs from its partition scope".into());
            }
            if tuple.is_some() && tuple != next {
                return Err("partition columns cannot change".into());
            }
            tuple = next;
        }
        Ok(tuple)
    }
    fn validate(&self, change: &RowChange) -> Result<(), String> {
        self.identity(change)?;
        self.change_partition(change)?;
        let table = self
            .tables
            .get(&change.scope.table)
            .ok_or("unknown typed table")?;
        if !self.domains.contains(&change.scope.domain.domain) {
            return Err("unknown domain".into());
        }
        let mut keys = BTreeSet::new();
        for fields in change.before.iter().chain(change.after.iter()) {
            let values = self.values(table, fields)?;
            keys.insert(self.row_id(change.scope.domain.domain, table, &values)?);
        }
        // Both images name this row: a key may be respelled, never changed.
        if keys.len() != 1 || !keys.contains(&change.row) {
            return Err(format!(
                "{} row identity differs from its key",
                table.sql_name
            ));
        }
        Ok(())
    }
}
impl RowApplication for TypedRows {
    fn manifest(&self) -> ApplicationManifest {
        self.schema.manifest_at(&self.domains, self.generation)
    }
    fn source_application(
        &self,
        definition: &ApplicationManifest,
    ) -> Result<Box<dyn RowApplication>, String> {
        source::SourceRows::new(definition)
            .map(|source| Box::new(source) as Box<dyn RowApplication>)
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        BTreeSet::new()
    }
    fn row_partition(&self, change: &RowChange) -> Result<Option<crate::PartitionTuple>, String> {
        self.change_partition(change)
    }
    fn row_identity(&self, change: &RowChange) -> Result<Option<crate::RowIdentity>, String> {
        self.identity(change)
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        changes.iter().try_for_each(|change| self.validate(change))
    }
    /// A foreign key's columns merge as one unit: an update publishes all
    /// of them or none, so concurrent edits never mix two references.
    fn validate_write(&self, write: &RowWrite) -> Result<(), String> {
        let table = self
            .tables
            .get(&write.change.scope.table)
            .ok_or("unknown typed table")?;
        let published = write.published_fields();
        if table.units.iter().any(|unit| {
            let count = unit.intersection(&published).count();
            count > 0 && count < unit.len()
        }) {
            return Err(format!(
                "{}: a foreign key's columns publish together",
                table.sql_name
            ));
        }
        self.validate(&write.change)
    }
}

/// The mergeable fields a stored row may lack: nullable ones, and required
/// ones with a default.
pub fn fillable(schema: &LogicalSchema) -> BTreeSet<Commitment> {
    schema
        .mergeable()
        .flat_map(|table| {
            table
                .registers()
                .filter(|column| !column.not_null || default_value(column).is_some())
                .map(|column| schema.field_id(table, column))
        })
        .collect()
}
/// The value of a column's default literal, as the definition writes it:
/// an integer, a real or a quoted string. NULL and anything else is none.
pub fn default_value(column: &crate::LogicalColumn) -> Option<SqlValue> {
    default_literal(column.affinity, column.default.as_deref()?)
}
fn default_literal(affinity: crate::Affinity, literal: &str) -> Option<SqlValue> {
    let value = if let Some(text) = literal
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        SqlValue::Text(text.replace("''", "'"))
    } else if let Ok(integer) = literal.parse::<i64>() {
        SqlValue::Integer(integer)
    } else {
        SqlValue::real(
            literal
                .parse::<f64>()
                .ok()
                .filter(|real| real.is_finite())?,
        )?
    };
    affinity.store(value).ok()
}

fn bound_row(
    domain: DomainId,
    table: Commitment,
    columns: &[KeyColumn],
    values: &[SqlValue],
) -> Result<Commitment, String> {
    let components = key_components(columns, values).map_err(|error| error.to_string())?;
    Ok(commitment(
        "rocky.secure-row.v1",
        &crate::bound_key(domain, &table, &components),
    ))
}
fn column_value(
    table: &str,
    column: &str,
    affinity: crate::Affinity,
    not_null: bool,
    default: Option<&str>,
    fillable: bool,
    bytes: Option<&[u8]>,
) -> Result<SqlValue, String> {
    let filled = || match default.and_then(|literal| default_literal(affinity, literal)) {
        Some(value) => Ok(value),
        None if not_null => Err(format!("{table}.{column} has no default")),
        None => Ok(SqlValue::Null),
    };
    let Some(mut bytes) = bytes else {
        return if fillable {
            filled()
        } else {
            Err(format!("{table}.{column} is missing"))
        };
    };
    let value = SqlValue::decode(&mut bytes)
        .ok()
        .filter(|_| bytes.is_empty())
        .ok_or_else(|| format!("{table}.{column} is not one value"))?;
    if value == SqlValue::Null && not_null && fillable {
        return filled();
    }
    if affinity.store(value.clone()).ok().as_ref() != Some(&value)
        || (not_null && value == SqlValue::Null)
    {
        return Err(format!(
            "{table}.{column} holds a value outside its declared type"
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Affinity, Collation, ColumnClass, LOGICAL_SCHEMA_FORMAT, LogicalColumn, LogicalTable,
        MeshId, ResourceScope, SchemaSource, Scope, TableClass, UpdatePolicy,
    };

    fn column(name: &str, affinity: Affinity, collation: Collation) -> LogicalColumn {
        LogicalColumn {
            sql_name: name.into(),
            affinity,
            not_null: true,
            default: None,
            collation,
            class: ColumnClass::Mergeable,
        }
    }
    fn rows() -> TypedRows {
        let schema = LogicalSchema {
            format: LOGICAL_SCHEMA_FORMAT,
            publisher: [1; 32],
            application: "typed".into(),
            source: SchemaSource {
                kind: "test".into(),
                version: 1,
                identity: "t".into(),
            },
            update_policy: UpdatePolicy::Changed,
            tables: vec![LogicalTable {
                sql_name: "items".into(),
            granted_like: None,
                class: TableClass::Mergeable,
                coordinated: false,
                create_sql: "CREATE TABLE items(key TEXT NOT NULL COLLATE NOCASE PRIMARY KEY,n INTEGER NOT NULL,id INTEGER NOT NULL,row_kind INTEGER NOT NULL DEFAULT 0)".into(),
                auto_generate: false,
                columns: vec![
                    column("key", Affinity::Text, Collation::Nocase),
                    column("n", Affinity::Integer, Collation::Binary),
                    column("id", Affinity::Integer, Collation::Binary),
                    column("row_kind", Affinity::Integer, Collation::Binary),
                ],
                primary_key: vec!["key".into()],
                partition_by: Vec::new(),
            partition_group: None,
                indexes: vec![crate::LogicalIndex { name: "items_identity".into(), unique: true, columns: vec![crate::IndexColumn { name: "id".into(), descending: false }], create_sql: "CREATE UNIQUE INDEX items_identity ON items(id)".into(), on_conflict: None }],
                foreign_keys: Vec::new(),

                existence_log: false,
            }],
            views: Vec::new(),
        };
        TypedRows::new(schema, vec![DomainId([2; 16])]).unwrap()
    }

    #[test]
    fn a_required_field_rows_lack_reads_its_default() {
        let mut schema = rows().schema().clone();
        let n = &mut schema.tables[0].columns[1];
        n.default = Some("7".into());
        let plain = TypedRows::new(schema.clone(), vec![DomainId([2; 16])]).unwrap();
        let table = plain.tables().next().unwrap().clone();
        let field = schema.field_id(&schema.tables[0], &schema.tables[0].columns[1]);
        let stored = |n: Option<SqlValue>| {
            let mut values = vec![SqlValue::Text("a".into())];
            values.extend(n);
            values.extend([SqlValue::Integer(4294967297), SqlValue::Integer(0)]);
            values
        };
        let mut fields = plain
            .fields(&table, &stored(Some(SqlValue::Integer(1))))
            .unwrap();
        fields.remove(&field);
        assert_eq!(
            plain.values(&table, &fields).unwrap(),
            stored(Some(SqlValue::Integer(7)))
        );
        let defaulted = plain.at_generation(2, [field].into()).unwrap();
        let expected = stored(Some(SqlValue::Integer(7)));
        assert_eq!(defaulted.values(&table, &fields).unwrap(), expected);
        fields.insert(field, {
            let mut bytes = Vec::new();
            SqlValue::Null.encode(&mut bytes);
            bytes
        });
        assert_eq!(defaulted.values(&table, &fields).unwrap(), expected);
        // Without a default, a required field cannot be one rows may lack.
        let bare = TypedRows::new(rows().schema().clone(), vec![DomainId([2; 16])]).unwrap();
        assert!(bare.at_generation(2, [field].into()).is_err());
    }

    #[test]
    fn a_row_is_named_by_its_key_and_holds_every_column_in_its_type() {
        let rows = rows();
        let table = rows.tables().next().unwrap().clone();
        let scope = ResourceScope {
            domain: Scope {
                mesh: MeshId([3; 16]),
                app: rows.schema().app_id(),
                domain: DomainId([2; 16]),
            },
            encryption: [4; 32],
            table: table.id,
        };
        let image = |id: &str, n: i64| {
            vec![
                SqlValue::Text(id.into()),
                SqlValue::Integer(n),
                SqlValue::Integer(4294967297),
                SqlValue::Integer(0),
            ]
        };
        let row = rows
            .row_id(DomainId([2; 16]), &table, &image("a", 1))
            .unwrap();
        let fields = |id: &str, n: i64| rows.fields(&table, &image(id, n)).unwrap();
        let change = |row, before: Option<RowFields>, after: Option<RowFields>| RowChange {
            scope,
            row,
            before,
            after,
        };
        assert_eq!(rows.values(&table, &fields("a", 1)).unwrap(), image("a", 1));
        assert!(
            rows.validate_changes(&[change(row, None, Some(fields("a", 1)))])
                .is_ok()
        );
        // A respelled NOCASE key names the same row; a changed key does not.
        assert!(
            rows.validate_changes(&[change(row, Some(fields("a", 1)), Some(fields("A", 2)))])
                .is_ok()
        );
        for invalid in [
            change(row, Some(fields("a", 1)), Some(fields("b", 1))),
            change(row, Some(fields("b", 1)), Some(fields("a", 2))),
            change([9; 32], None, Some(fields("a", 1))),
        ] {
            let error = "items row identity differs from its key";
            assert_eq!(
                rows.validate_changes(std::slice::from_ref(&invalid)),
                Err(error.into())
            );
            assert_eq!(
                rows.validate_write(&RowWrite::changed(invalid)),
                Err(error.into())
            );
        }
        // Every column, each in its declared type.
        let mut missing = fields("a", 1);
        missing.pop_last();
        let mut text = fields("a", 1);
        let mut bytes = Vec::new();
        SqlValue::Text("one".into()).encode(&mut bytes);
        text.insert(table.fields[1], bytes);
        for invalid in [missing, text] {
            assert!(
                rows.validate_changes(&[change(row, None, Some(invalid))])
                    .is_err()
            );
        }
    }
    #[test]
    fn source_shapes_preserve_current_values_keys_and_incarnations() {
        let mut schema = rows().schema().clone();
        schema.tables[0].columns[1].default = Some("7".into());
        let rows = TypedRows::new(schema, vec![DomainId([2; 16])]).unwrap();
        let definition = rows.manifest();
        let source = rows.source_application(&definition).unwrap();
        assert_eq!(source.manifest(), definition);
        let table = rows.tables().next().unwrap();
        let values = vec![
            SqlValue::Text("A".into()),
            SqlValue::Integer(1),
            SqlValue::Integer(4294967297),
            SqlValue::Integer(0),
        ];
        let mut change = RowChange {
            scope: ResourceScope {
                domain: Scope {
                    mesh: MeshId([3; 16]),
                    app: definition.app,
                    domain: DomainId([2; 16]),
                },
                encryption: [4; 32],
                table: table.id,
            },
            row: rows.row_id(DomainId([2; 16]), table, &values).unwrap(),
            before: None,
            after: Some(rows.fields(table, &values).unwrap()),
        };
        source
            .validate_write(&RowWrite::changed(change.clone()))
            .unwrap();
        assert_eq!(source.row_identity(&change), rows.row_identity(&change));
        change.after.as_mut().unwrap().remove(&table.fields[1]);
        source
            .validate_write(&RowWrite::changed(change.clone()))
            .unwrap();
        rows.validate_changes(&[change.clone()]).unwrap();
        for (field, value) in [
            (table.fields[2], SqlValue::Integer(0)),
            (table.fields[3], SqlValue::Integer(1)),
            (table.fields[0], SqlValue::Text("another key".into())),
            ([99; 32], SqlValue::Null),
        ] {
            let mut invalid = change.clone();
            let mut bytes = Vec::new();
            value.encode(&mut bytes);
            invalid.after.as_mut().unwrap().insert(field, bytes);
            let write = RowWrite::changed(invalid);
            assert!(source.validate_write(&write).is_err());
            assert!(rows.validate_write(&write).is_err());
        }
        let mut changed_id = change.clone();
        changed_id.before = changed_id.after.clone();
        let mut bytes = Vec::new();
        SqlValue::Integer(4294967298).encode(&mut bytes);
        changed_id
            .after
            .as_mut()
            .unwrap()
            .insert(table.fields[2], bytes);
        assert!(source.row_identity(&changed_id).is_err());
        assert!(rows.validate_write(&RowWrite::changed(changed_id)).is_err());
        change.row = [88; 32];
        let write = RowWrite::changed(change);
        assert!(source.validate_write(&write).is_err());
        assert!(rows.validate_write(&write).is_err());
    }

    #[test]
    fn source_shapes_refuse_inconsistent_fields_markers_and_unsupported_shapes() {
        let rows = rows();
        let definition = rows.manifest();
        for mutate in 0..5 {
            let mut invalid = definition.clone();
            match mutate {
                0 => invalid.domains[0].rule = "other".into(),
                1 => invalid.operations[0].shape = "other".into(),
                2 => invalid.fields[0].shape = "{}".into(),
                3 => invalid.entities[0].shape = "{}".into(),
                _ => {
                    invalid.fields.pop();
                }
            }
            assert!(rows.source_application(&invalid).is_err());
        }
    }

    #[test]
    fn a_partial_reference_unit_is_refused_before_an_invalid_row_key() {
        let mut rows = rows();
        let table = rows.tables.values_mut().next().unwrap();
        // Isolate the existing unit gate's error order with explicit private metadata.
        let unit = [table.fields[1], table.fields[3]].into();
        table.units = vec![unit];
        let table = rows.tables().next().unwrap();
        let values = vec![
            SqlValue::Text("a".into()),
            SqlValue::Integer(1),
            SqlValue::Integer(4294967297),
            SqlValue::Integer(0),
        ];
        let mut after = values.clone();
        after[1] = SqlValue::Integer(2);
        let mut write = RowWrite::changed(RowChange {
            scope: ResourceScope {
                domain: Scope {
                    mesh: MeshId([3; 16]),
                    app: rows.manifest().app,
                    domain: DomainId([2; 16]),
                },
                encryption: [4; 32],
                table: table.id,
            },
            row: [99; 32],
            before: Some(rows.fields(table, &values).unwrap()),
            after: Some(rows.fields(table, &after).unwrap()),
        });
        assert_eq!(
            rows.validate_write(&write),
            Err("items: a foreign key's columns publish together".into())
        );
        write.assigned = table.units()[0].clone();
        assert_eq!(
            rows.validate_write(&write),
            Err("items row identity differs from its key".into())
        );
        write.change.row = rows
            .row_id(write.change.scope.domain.domain, table, &values)
            .unwrap();
        rows.validate_write(&write).unwrap();
    }

    #[test]
    fn source_shapes_keep_partition_values_and_scope_bound_to_complete_images() {
        let definition = serde_json::json!({"format":6,"publisher":"01".repeat(32),"application":"source-partitions","tables":[{"name":"items","class":"mergeable","columns":[{"name":"list","type":"text","not_null":true,"collation":"nocase"},{"name":"key","type":"text","not_null":true},{"name":"amount","type":"integer","not_null":true,"default":7}],"primary_key":["list","key"],"partition_by":["list"]}]});
        let schema = crate::SchemaDefinition::parse(&serde_json::to_vec(&definition).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let rows = TypedRows::new(schema, vec![DomainId([2; 16])]).unwrap();
        let source = rows.source_application(&rows.manifest()).unwrap();
        let table = rows.tables().next().unwrap();
        let values = vec![
            SqlValue::Text("A".into()),
            SqlValue::Text("key".into()),
            SqlValue::Integer(7),
            SqlValue::Integer(4294967297),
            SqlValue::Integer(0),
        ];
        let domain = Scope {
            mesh: MeshId([3; 16]),
            app: rows.manifest().app,
            domain: DomainId([2; 16]),
        };
        let tuple = rows.partition(table, &values).unwrap().unwrap();
        let mut change = RowChange {
            scope: tuple.scope(domain, table.id).unwrap(),
            row: rows.row_id(domain.domain, table, &values).unwrap(),
            before: None,
            after: Some(rows.fields(table, &values).unwrap()),
        };
        assert_eq!(source.row_partition(&change), Ok(Some(tuple)));
        change.after.as_mut().unwrap().remove(&table.fields[2]);
        source
            .validate_write(&RowWrite::changed(change.clone()))
            .unwrap();
        let mut wrong = values.clone();
        wrong[0] = SqlValue::Text("B".into());
        change.after = Some(rows.fields(table, &wrong).unwrap());
        assert!(source.row_partition(&change).is_err());
    }

    #[test]
    fn source_shapes_read_current_partitioned_reference_contracts() {
        let definition = serde_json::json!({"format":6,"publisher":"01".repeat(32),"application":"source-references","tables":[
            {"name":"lists","class":"mergeable","columns":[{"name":"title","type":"text"}],"primary_key":["id"],"partition_by":["id"],"partition_group":"list"},
            {"name":"items","class":"mergeable","columns":[{"name":"list","type":"integer","not_null":true},{"name":"title","type":"text"}],"primary_key":["list","id"],"partition_by":["list"],"partition_group":"list","foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"}]}
        ]});
        let schema = crate::SchemaDefinition::parse(&serde_json::to_vec(&definition).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let rows = TypedRows::new(schema, vec![DomainId([2; 16])]).unwrap();
        let definition = rows.manifest();
        let source = rows.source_application(&definition).unwrap();
        let table = rows
            .tables()
            .find(|table| table.sql_name == "items")
            .unwrap();
        let values = vec![
            SqlValue::Integer(4294967297),
            SqlValue::Text("child".into()),
            SqlValue::Integer(4294967298),
            SqlValue::Integer(0),
        ];
        let domain = Scope {
            mesh: MeshId([3; 16]),
            app: definition.app,
            domain: DomainId([2; 16]),
        };
        let scope = rows
            .partition(table, &values)
            .unwrap()
            .unwrap()
            .scope(domain, table.id)
            .unwrap();
        let change = RowChange {
            scope,
            row: rows.row_id(domain.domain, table, &values).unwrap(),
            before: None,
            after: Some(rows.fields(table, &values).unwrap()),
        };
        source.validate_write(&RowWrite::changed(change)).unwrap();
        let mut invalid = definition;
        let item = invalid
            .entities
            .iter_mut()
            .find(|item| item.name == "items")
            .unwrap();
        item.shape = item
            .shape
            .replace("\"table\":\"lists\"", "\"table\":\"missing\"");
        assert!(rows.source_application(&invalid).is_err());
    }
}
