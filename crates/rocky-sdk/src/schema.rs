use rocky_core::{InvalidName, NameRule, RoleDefinition, RoleTable, SchemaError, TableAccess};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// An application's tables and roles at one version; build it with `Schema::builder`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    pub(crate) format: u32,
    pub(crate) application: String,
    pub(crate) version: u32,
    pub(crate) tables: Vec<rocky_core::TableDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) views: Vec<rocky_core::LogicalView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) assertions: Vec<rocky_core::LogicalAssertion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) restrictions: Vec<rocky_core::LogicalRestriction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) migrations: Vec<Migration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) roles: Vec<RoleDefinition>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Migration {
    pub(crate) version: u32,
    pub(crate) statements: Vec<MigrationStep>,
}
/// One step of a migration to a schema version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum MigrationStep {
    /// A SQL statement run against the new version; it may read the previous version's rows as
    /// `parent.<table>`.
    Sql(String),
    /// Copies the rows a `SELECT` over the previous version returns into a table.
    Insert {
        /// The table to fill.
        insert: String,
        /// The `SELECT` that produces the rows.
        select: String,
    },
}
impl MigrationStep {
    /// An `Insert` step.
    pub fn insert(table: impl Into<String>, select: impl Into<String>) -> Self {
        Self::Insert {
            insert: table.into(),
            select: select.into(),
        }
    }
}
impl From<&str> for MigrationStep {
    fn from(sql: &str) -> Self {
        Self::Sql(sql.to_owned())
    }
}
impl From<String> for MigrationStep {
    fn from(sql: String) -> Self {
        Self::Sql(sql)
    }
}
fn invalid(reason: impl Into<String>) -> SchemaError {
    SchemaError::Invalid(reason.into())
}
fn malformed(error: serde_json::Error) -> SchemaError {
    invalid(error.to_string())
}
impl Schema {
    pub(crate) fn parse(json: &str) -> Result<Self, SchemaError> {
        if json.len() > rocky_core::MAX_DEFINITION_BYTES {
            return Err(invalid("schema quota"));
        }
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        let version: Format = serde_json::from_str(json).map_err(malformed)?;
        if version.format != rocky_core::DEFINITION_FORMAT {
            return Err(invalid("unsupported schema definition format"));
        }
        let schema: Self = serde_json::from_str(json).map_err(malformed)?;
        schema.validated()
    }
    pub(crate) fn validated(&self) -> Result<Self, SchemaError> {
        let mut schema = self.clone();
        schema.validate()?;
        {
            let tables = &mut schema.tables;
            tables.sort_by(|a, b| a.name.cmp(&b.name));
            for table in tables {
                table.indexes.sort_by(|a, b| a.name.cmp(&b.name));
            }
        }
        Ok(schema)
    }
    fn validate(&self) -> Result<(), SchemaError> {
        if serde_json::to_vec(self).map_err(malformed)?.len() > rocky_core::MAX_DEFINITION_BYTES {
            return Err(invalid("schema quota"));
        }
        if !rocky_core::identifier(&self.application) {
            return Err(
                InvalidName::new("application", &self.application, NameRule::Identifier).into(),
            );
        }
        if self.version == 0 {
            return Err(invalid("schema versions start at 1"));
        }
        if self.format != rocky_core::DEFINITION_FORMAT {
            return Err(invalid("unsupported schema definition format"));
        }
        {
            let tables = &self.tables;
            if tables.is_empty()
                || tables
                    .iter()
                    .any(|table| table.columns.is_empty() || table.columns.len() > 128)
            {
                return Err(invalid("definition table or column quota"));
            }
            if tables
                .iter()
                .flat_map(|table| std::iter::once(&table.name).chain(&table.renamed_from))
                .any(|name| name.eq_ignore_ascii_case(crate::domain::REGISTRY))
            {
                return Err(invalid(format!(
                    "{} is reserved for the SDK's issuer registry",
                    crate::domain::REGISTRY
                )));
            }
            if let Some(table) = tables.iter().find(|table| table.explicit_access_only) {
                return Err(invalid(format!(
                    "{} is granted only explicitly, which only the SDK's issuer registry is",
                    table.name
                )));
            }
            self.definition([0; 32])
                .build()?
                .supported_on(rocky_core::Profile::Secure)?;
            if let Some(table) = self
                .migrations
                .iter()
                .flat_map(|migration| &migration.statements)
                .find_map(|step| match step {
                    MigrationStep::Insert { insert, .. }
                        if !self.tables.iter().any(|table| {
                            table.name == *insert && table.class == rocky_core::TableClass::Replayed
                        }) =>
                    {
                        Some(insert)
                    }
                    _ => None,
                })
            {
                return Err(invalid(format!(
                    "a migration inserts into {table}, which is no replayed table"
                )));
            }
            if self.migrations.len() > 64
                || self
                    .migrations
                    .windows(2)
                    .any(|pair| pair[0].version >= pair[1].version)
                || self.migrations.iter().any(|migration| {
                    migration.version == 0
                        || migration.version > self.version
                        || migration.statements.is_empty()
                        || migration.statements.len() > 64
                })
            {
                return Err(invalid(
                    "migrations name ascending versions up to the schema's, each with 1 to 64 statements",
                ));
            }
            Ok(())
        }
    }

    /// The engine's form of this schema, as published under `publisher`.
    pub fn definition(&self, publisher: [u8; 32]) -> rocky_core::SchemaDefinition {
        rocky_core::SchemaDefinition {
            format: self.format,
            publisher: data_encoding::HEXLOWER.encode(&publisher),
            application: self.application.clone(),
            views: self.views.clone(),
            assertions: self.assertions.clone(),
            restrictions: self.restrictions.clone(),
            tables: self.tables.clone(),
            roles: self.roles.clone(),
        }
    }
}

/// Collects tables, roles and migrations; `build` validates them.
pub struct SchemaBuilder(Schema);
impl Schema {
    /// Starts the schema of `application` at `version`; raise the version with every change.
    pub fn builder(application: impl Into<String>, version: u32) -> SchemaBuilder {
        SchemaBuilder(Self {
            format: rocky_core::DEFINITION_FORMAT,
            application: application.into(),
            version,
            tables: Vec::new(),
            views: Vec::new(),
            assertions: Vec::new(),
            restrictions: Vec::new(),
            migrations: Vec::new(),
            roles: Vec::new(),
        })
    }
}
impl SchemaBuilder {
    /// Adds a table.
    #[must_use]
    pub fn table(mut self, table: Table) -> Self {
        self.0.tables.push(table.0);
        self
    }
    /// Adds a role that `Access::Role` can grant.
    #[must_use]
    pub fn role(mut self, role: Role) -> Self {
        self.0.roles.push(role.0);
        self
    }
    /// Declares the steps that bring the data to `version`; a step may read the previous version's
    /// rows as `parent.<table>`.
    #[must_use]
    pub fn migration(
        mut self,
        version: u32,
        steps: impl IntoIterator<Item = impl Into<MigrationStep>>,
    ) -> Self {
        self.0.migrations.push(Migration {
            version,
            statements: steps.into_iter().map(Into::into).collect(),
        });
        self
    }
    /// Validates the schema; an invalid name is `ErrorKind::Schema` with
    /// `SchemaError::InvalidName`.
    pub fn build(self) -> Result<Schema, crate::Error> {
        Ok(self.0.validated()?)
    }
}

/// The name of a role the schema declares.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoleName(Cow<'static, str>);
impl RoleName {
    /// A role name known at compile time; `Schema::build` checks it.
    pub const fn from_static(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }
    /// Checks the name is an identifier that is not reserved.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidName> {
        let name = name.into();
        if !rocky_core::role_name_is_valid(&name) {
            return Err(InvalidName::new("roles", name, NameRule::Identifier));
        }
        if rocky_core::role_name_is_reserved(&name) {
            return Err(InvalidName::new("roles", name, NameRule::Reserved));
        }
        Ok(Self(Cow::Owned(name)))
    }
    /// The name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for RoleName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl Serialize for RoleName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_str().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for RoleName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Rights to named tables that a person gets through `Access::Role`.
pub struct Role(RoleDefinition);
impl Role {
    /// An empty role.
    pub fn new(name: RoleName) -> Self {
        Self(RoleDefinition {
            name: name.as_str().to_owned(),
            tables: Vec::new(),
            per_partition: Vec::new(),
        })
    }
    fn grant(mut self, table: impl Into<String>, access: TableAccess) -> Self {
        self.0.tables.push(RoleTable {
            table: table.into(),
            access,
        });
        self
    }
    /// Lets the role read a table.
    #[must_use]
    pub fn read(self, table: impl Into<String>) -> Self {
        self.grant(table, TableAccess::Read)
    }
    /// Lets the role read and write a table.
    #[must_use]
    pub fn write(self, table: impl Into<String>) -> Self {
        self.grant(table, TableAccess::Write)
    }
    /// Lets the role read a table and insert rows, but not change or delete them.
    #[must_use]
    pub fn append(self, table: impl Into<String>) -> Self {
        self.grant(table, TableAccess::Append)
    }
    /// Lets the role read a table and update only the listed columns.
    #[must_use]
    pub fn update_columns(
        self,
        table: impl Into<String>,
        columns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let columns = columns.into_iter().map(Into::into).collect();
        self.grant(table, TableAccess::UpdateColumns(columns))
    }
    /// Gives the role no partition of `group` until an admin calls `Database::grant_partition`.
    #[must_use]
    pub fn per_partition(mut self, group: impl Into<String>) -> Self {
        self.0.per_partition.push(group.into());
        self
    }
}

/// A table whose writes are replayed on every device; its primary key is `id` unless set.
pub struct Table(rocky_core::TableDefinition);
impl Table {
    /// A table named `name` with an `id` primary key, which `Transaction::next_id` fills.
    pub fn replayed(name: impl Into<String>) -> Self {
        Self::of(name, rocky_core::TableClass::Replayed)
    }
    fn of(name: impl Into<String>, class: rocky_core::TableClass) -> Self {
        Self(rocky_core::TableDefinition {
            name: name.into(),
            renamed_from: Vec::new(),
            explicit_access_only: false,
            class,
            columns: Vec::new(),
            checks: Vec::new(),
            primary_key: vec!["id".into()],
            partition_by: Vec::new(),
            partition_group: None,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        })
    }
    /// Adds a column.
    #[must_use]
    pub fn column(mut self, column: Column) -> Self {
        self.0.columns.push(column.0);
        self
    }
    /// Replaces the `id` primary key.
    #[must_use]
    pub fn primary_key(mut self, columns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.primary_key = columns.into_iter().map(Into::into).collect();
        self
    }
    /// Splits the table into partitions by these columns' values, each encrypted under its own key.
    #[must_use]
    pub fn partition_by(mut self, columns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.partition_by = columns.into_iter().map(Into::into).collect();
        self
    }
    /// Shares one partition key with the other tables of `group`, so a grant covers them together.
    #[must_use]
    pub fn partition_group(mut self, group: impl Into<String>) -> Self {
        self.0.partition_group = Some(group.into());
        self
    }
    /// Adds an index.
    #[must_use]
    pub fn index(mut self, index: rocky_core::IndexDefinition) -> Self {
        self.0.indexes.push(index);
        self
    }
    /// Adds a foreign key, checked when writes replay.
    #[must_use]
    pub fn foreign_key(mut self, foreign_key: rocky_core::ForeignKeyDefinition) -> Self {
        self.0.foreign_keys.push(foreign_key);
        self
    }
    /// Adds a `CHECK` constraint, checked when writes replay.
    #[must_use]
    pub fn check(mut self, expression: impl Into<String>) -> Self {
        self.0.checks.push(expression.into());
        self
    }
    /// Declares an earlier name of this table, so writes made under it land here; call it once
    /// per earlier name, oldest first, and keep every call in later versions.
    #[must_use]
    pub fn renamed_from(mut self, old: impl Into<String>) -> Self {
        self.0.renamed_from.push(old.into());
        self
    }
}

/// A table column.
pub struct Column(rocky_core::ColumnDefinition);
impl Column {
    /// A nullable column with no default.
    pub fn new(name: impl Into<String>, affinity: rocky_core::Affinity) -> Self {
        Self(rocky_core::ColumnDefinition {
            name: name.into(),
            affinity,
            not_null: false,
            default: None,
            collation: rocky_core::Collation::Binary,
            derived: None,
            local_only: false,
            renamed_from: Vec::new(),
        })
    }
    /// Makes the column `NOT NULL`.
    #[must_use]
    pub fn required(mut self) -> Self {
        self.0.not_null = true;
        self
    }
    /// Sets the value an insert that omits the column gets.
    #[must_use]
    pub fn default(mut self, value: impl crate::IntoSqlValue) -> Self {
        self.0.default = Some(match value.into_sql_value() {
            rocky_core::SqlValue::Null => serde_json::Value::Null,
            rocky_core::SqlValue::Integer(value) => value.into(),
            rocky_core::SqlValue::Real(value) => value.get().into(),
            rocky_core::SqlValue::Text(value) => value.into(),
            rocky_core::SqlValue::Blob(value) => value.into(),
        });
        self
    }
    /// Declares an earlier name of this column, so writes made under it land here; call it once
    /// per earlier name, oldest first, and keep every call in later versions.
    #[must_use]
    pub fn renamed_from(mut self, old: impl Into<String>) -> Self {
        self.0.renamed_from.push(old.into());
        self
    }
}
