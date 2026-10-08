//! Typed tables as secure rows: one row per business key, one field per
//! replicated column. The row contract that `EncryptedReplica` admits and a
//! secure SQL session commits. See `docs/runtime/secure-sql.md`.
use crate::{
    ApplicationManifest, Commitment, DomainId, KeyColumn, LogicalSchema, RowApplication, RowChange,
    RowFields, RowWrite, SchemaError, SqlValue, TableClass, commitment, key_components,
};
use std::collections::{BTreeMap, BTreeSet};

/// One mergeable or command-controlled table: its field per column, in
/// declared order.
#[derive(Clone, Debug)]
pub struct TypedTable {
    pub sql_name: String,
    pub id: Commitment,
    /// Command-controlled rows change only through confirmed results.
    pub class: TableClass,
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
/// A logical schema's typed tables as a `RowApplication`. Its row writes
/// are mergeable; confirmed results also reach command-controlled tables.
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
                    class: table.class,
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
            schema,
            domains,
            tables,
            generation: 1,
            added: BTreeSet::new(),
        })
    }
    /// These tables at schema generation `generation`, where `added` names
    /// the nullable fields transitions added to existing tables.
    pub fn at_generation(
        mut self,
        generation: u32,
        added: BTreeSet<Commitment>,
    ) -> Result<Self, SchemaError> {
        let nullable: BTreeSet<Commitment> = self
            .schema
            .mergeable()
            .flat_map(|table| {
                table
                    .registers()
                    .filter(|column| !column.not_null)
                    .map(|column| self.schema.field_id(table, column))
            })
            .collect();
        if generation == 0 || !added.is_subset(&nullable) {
            return Err(SchemaError::Invalid(
                "added fields must be nullable registers of a generation".into(),
            ));
        }
        self.generation = generation;
        self.added = added;
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
        let components = key_components(&table.key_columns, &key)
            .map_err(|error| format!("key of {}: {error}", table.sql_name))?;
        Ok(commitment(
            "rocky.secure-row.v1",
            &crate::bound_key(domain, &table.id, &components),
        ))
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
                let Some(bytes) = fields.get(field) else {
                    if self.added.contains(field) {
                        return Ok(SqlValue::Null);
                    }
                    return Err(format!("{}.{} is missing", table.sql_name, column.sql_name));
                };
                let mut bytes = bytes.as_slice();
                let value = SqlValue::decode(&mut bytes)
                    .ok()
                    .filter(|_| bytes.is_empty())
                    .ok_or_else(|| {
                        format!("{}.{} is not one value", table.sql_name, column.sql_name)
                    })?;
                let stored = column.affinity.store(value.clone()).ok();
                if stored.as_ref() != Some(&value) || (column.not_null && value == SqlValue::Null) {
                    return Err(format!(
                        "{}.{} holds a value outside its declared type",
                        table.sql_name, column.sql_name
                    ));
                }
                Ok(value)
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
    /// Checks the changes of a confirmed command result, which may also
    /// change command-controlled tables.
    pub fn validate_confirmed(&self, changes: &[RowChange]) -> Result<(), String> {
        changes.iter().try_for_each(|change| self.validate(change))
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
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        BTreeSet::new()
    }
    fn row_partition(&self, change: &RowChange) -> Result<Option<crate::PartitionTuple>, String> {
        self.change_partition(change)
    }
    fn row_identity(&self, change: &RowChange) -> Result<Option<crate::RowIdentity>, String> {
        self.identity(change)
    }
    /// Row writes: a command-controlled table refuses every one.
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        for change in changes {
            if let Some(table) = self.tables.get(&change.scope.table)
                && table.class == TableClass::CommandControlled
            {
                return Err(format!(
                    "{} is command-controlled; only confirmed commands change it",
                    table.sql_name
                ));
            }
        }
        self.validate_confirmed(changes)
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
        Ok(())
    }
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
            logical: name.into(),
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
                logical: "items".into(),
                sql_name: "items".into(),
                class: TableClass::Mergeable,
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
            change([9; 32], None, Some(fields("a", 1))),
        ] {
            assert!(rows.validate_changes(&[invalid]).is_err());
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
}
