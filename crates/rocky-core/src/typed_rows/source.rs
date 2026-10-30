//! Semantic reading of the current replicated typed shapes, without a SQL projection.
use super::*;
use crate::{
    Affinity, Collation, ConflictPolicy, PartitionTuple, PartitionValue, RowIdentity, TableClass,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Column {
    name: String,
    affinity: Affinity,
    not_null: bool,
    collation: Collation,
    default: Option<String>,
}
impl Column {
    fn key(&self) -> KeyColumn {
        KeyColumn {
            affinity: self.affinity,
            collation: self.collation,
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Unique {
    columns: Vec<String>,
    on_conflict: ConflictPolicy,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    columns: Vec<String>,
    table: String,
    on_delete: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    deferred: bool,
}
/// What a replayed table's replay enforces; see `logical::ReplayedConstraints`.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Replayed {
    create_sql: String,
    indexes: Vec<String>,
    assertions: Vec<crate::LogicalAssertion>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Table {
    class: TableClass,
    name: String,
    granted_like: Option<String>,
    key: Vec<Column>,
    fields: Vec<Column>,
    unique: Vec<Unique>,
    references: Vec<Reference>,
    partition_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition_group: Option<String>,
    existence_log: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replayed: Option<Replayed>,
}
impl Table {
    fn columns(&self) -> impl Iterator<Item = &Column> {
        self.key.iter().chain(&self.fields)
    }
    fn field(&self, definition: &ApplicationManifest, name: &str) -> Commitment {
        definition.item_id("field", &format!("{}.{name}", self.name))
    }
    fn values(
        &self,
        definition: &ApplicationManifest,
        fields: &RowFields,
    ) -> Result<BTreeMap<String, SqlValue>, String> {
        let expected: BTreeSet<_> = self
            .columns()
            .map(|column| self.field(definition, &column.name))
            .collect();
        if fields.keys().any(|field| !expected.contains(field)) {
            return Err(format!("{} row holds a foreign field", self.name));
        }
        self.columns()
            .map(|column| {
                let register = self.fields.iter().any(|field| field.name == column.name);
                let fillable = register
                    && (!column.not_null
                        || column
                            .default
                            .as_deref()
                            .and_then(|literal| default_literal(column.affinity, literal))
                            .is_some());
                let value = column_value(
                    &self.name,
                    &column.name,
                    column.affinity,
                    column.not_null,
                    column.default.as_deref(),
                    fillable,
                    fields
                        .get(&self.field(definition, &column.name))
                        .map(Vec::as_slice),
                )?;
                Ok((column.name.clone(), value))
            })
            .collect()
    }
}
pub(super) struct SourceRows {
    definition: ApplicationManifest,
    tables: BTreeMap<Commitment, Table>,
}
impl SourceRows {
    pub(super) fn new(definition: &ApplicationManifest) -> Result<Self, String> {
        definition.validate().map_err(|error| error.to_string())?;
        let unsupported = || "unsupported or inconsistent typed source definition".to_string();
        if definition.projection != 1
            || definition
                .domains
                .iter()
                .any(|domain| domain.rule != "rocky.sdk.typed.v1")
            || definition.operations.len() != 3
        {
            return Err(unsupported());
        }
        let mut marker = None;
        for name in ["insert", "update", "delete"] {
            let operation = definition
                .operations
                .iter()
                .find(|operation| operation.name == name)
                .ok_or_else(unsupported)?;
            if !matches!(
                operation.shape.as_str(),
                "rocky.sdk.typed.v1:Changed" | "rocky.sdk.typed.v1:Assigned"
            ) || marker.is_some_and(|previous| previous != &operation.shape)
            {
                return Err(unsupported());
            }
            marker = Some(&operation.shape);
        }
        let mut tables = BTreeMap::new();
        let mut fields = BTreeSet::new();
        for entity in &definition.entities {
            let table: Table = serde_json::from_str(&entity.shape).map_err(|_| unsupported())?;
            if serde_json::to_string(&table).map_err(|_| unsupported())? != entity.shape
                || !table.class.typed()
                || table.name != entity.name
                || table.key.is_empty()
            {
                return Err(unsupported());
            }
            let mut names = BTreeSet::new();
            for column in table.columns() {
                let name = format!("{}.{}", table.name, column.name);
                let field = definition
                    .fields
                    .iter()
                    .find(|field| field.name == name)
                    .ok_or_else(unsupported)?;
                if column.name.is_empty()
                    || !names.insert(column.name.to_ascii_lowercase())
                    || column.affinity == Affinity::Numeric
                    || field.shape != serde_json::to_string(column).map_err(|_| unsupported())?
                    || !fields.insert(field.id)
                {
                    return Err(unsupported());
                }
            }
            if table.key.iter().any(|column| !column.not_null)
                || table
                    .fields
                    .windows(2)
                    .any(|pair| pair[0].name >= pair[1].name)
            {
                return Err(unsupported());
            }
            for name in ["id", "row_kind"] {
                if table
                    .columns()
                    .find(|column| column.name == name)
                    .is_none_or(|column| {
                        column.affinity != Affinity::Integer
                            || !column.not_null
                            || column.collation != Collation::Binary
                    })
                {
                    return Err(unsupported());
                }
            }
            let mut partition = BTreeSet::new();
            if table.partition_by.len() > 16
                || table.partition_by.iter().any(|name| {
                    !partition.insert(name) || !table.key.iter().any(|column| &column.name == name)
                })
            {
                return Err(unsupported());
            }
            if table.unique.iter().any(|unique| {
                unique.columns.is_empty()
                    || unique
                        .columns
                        .iter()
                        .any(|name| !table.columns().any(|column| &column.name == name))
            }) || table.references.iter().any(|reference| {
                reference.columns.is_empty()
                    || !matches!(reference.on_delete.as_str(), "CASCADE" | "SET NULL")
                    || reference
                        .columns
                        .iter()
                        .any(|name| !table.columns().any(|column| &column.name == name))
            }) {
                return Err(unsupported());
            }
            tables.insert(entity.id, table);
        }
        if fields.len() != definition.fields.len()
            || tables.values().any(|table| {
                table
                    .references
                    .iter()
                    .any(|reference| !tables.values().any(|target| target.name == reference.table))
                    || table
                        .granted_like
                        .as_ref()
                        .is_some_and(|name| !tables.values().any(|target| &target.name == name))
            })
        {
            return Err(unsupported());
        }
        Ok(Self {
            definition: definition.clone(),
            tables,
        })
    }
    fn table(&self, change: &RowChange) -> Result<&Table, String> {
        if change.scope.domain.app != self.definition.app
            || !self
                .definition
                .domains
                .iter()
                .any(|domain| domain.domain == change.scope.domain.domain)
        {
            return Err("unknown source domain".into());
        }
        self.tables
            .get(&change.scope.table)
            .ok_or_else(|| "unknown source typed table".into())
    }
    fn identity(&self, change: &RowChange) -> Result<RowIdentity, String> {
        let table = self.table(change)?;
        let mut identity = None;
        for fields in change.before.iter().chain(&change.after) {
            let values = table.values(&self.definition, fields)?;
            let Some(SqlValue::Integer(id)) = values.get("id") else {
                return Err("id must be an integer".into());
            };
            let id = RowIdentity::from_integer(*id).map_err(|error| error.to_string())?;
            if values.get("row_kind") != Some(&SqlValue::Integer(0)) {
                return Err(
                    "application rows require row_kind = 0; other kinds are reserved".into(),
                );
            }
            if identity.is_some_and(|previous| previous != id) {
                return Err("a row's id cannot change".into());
            }
            identity = Some(id);
        }
        identity.ok_or_else(|| "identity row has no image".into())
    }
    fn validate(&self, change: &RowChange) -> Result<(), String> {
        self.identity(change)?;
        self.row_partition(change)?;
        let table = self.table(change)?;
        for fields in change.before.iter().chain(&change.after) {
            let values = table.values(&self.definition, fields)?;
            let key = table.key.iter().map(Column::key).collect::<Vec<_>>();
            let values = table
                .key
                .iter()
                .map(|column| {
                    values
                        .get(&column.name)
                        .cloned()
                        .ok_or("missing source key".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            if bound_row(
                change.scope.domain.domain,
                change.scope.table,
                &key,
                &values,
            )? != change.row
            {
                return Err(format!("{} row identity differs from its key", table.name));
            }
        }
        Ok(())
    }
}
impl RowApplication for SourceRows {
    fn manifest(&self) -> ApplicationManifest {
        self.definition.clone()
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        BTreeSet::new()
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        changes.iter().try_for_each(|change| self.validate(change))
    }
    fn row_identity(&self, change: &RowChange) -> Result<Option<RowIdentity>, String> {
        self.identity(change).map(Some)
    }
    fn row_partition(&self, change: &RowChange) -> Result<Option<PartitionTuple>, String> {
        let table = self.table(change)?;
        let mut tuple = None;
        for fields in change.before.iter().chain(&change.after) {
            let values = table.values(&self.definition, fields)?;
            let next = if table.partition_by.is_empty() {
                None
            } else {
                Some(
                    PartitionTuple::new(
                        table
                            .partition_by
                            .iter()
                            .map(|name| {
                                let column =
                                    table
                                        .key
                                        .iter()
                                        .find(|column| &column.name == name)
                                        .ok_or("unknown source partition column".to_string())?;
                                let value = values
                                    .get(name)
                                    .cloned()
                                    .ok_or("missing source partition value".to_string())?;
                                Ok((
                                    table.field(&self.definition, name),
                                    PartitionValue::new(column.key(), value)
                                        .map_err(|error| error.to_string())?,
                                ))
                            })
                            .collect::<Result<Vec<_>, String>>()?,
                    )
                    .map_err(|error| error.to_string())?,
                )
            };
            if next.as_ref().is_some_and(|tuple| {
                tuple.scope(change.scope.domain, change.scope.table).ok() != Some(change.scope)
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
    fn validate_write(&self, write: &RowWrite) -> Result<(), String> {
        self.validate(&write.change)?;
        let table = self.table(&write.change)?;
        let published = write.published_fields();
        for reference in &table.references {
            let unit = reference
                .columns
                .iter()
                .filter(|name| table.fields.iter().any(|column| &column.name == *name))
                .map(|name| table.field(&self.definition, name))
                .collect::<BTreeSet<_>>();
            let count = unit.intersection(&published).count();
            if count > 0 && count < unit.len() {
                return Err(format!(
                    "{}: a foreign key's columns publish together",
                    table.name
                ));
            }
        }
        Ok(())
    }
}
