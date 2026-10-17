use crate::Failure;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    pub format: u32,
    pub application: String,
    pub version: u32,
    pub tables: Vec<rocky_core::TableDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<rocky_core::LogicalView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_policy: Option<rocky_core::UpdatePolicy>,
    /// SQL the first issuer to sign the schema's generation runs once, after
    /// signing it; see `docs/runtime/generations.md#migrations`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub migrations: Vec<Migration>,
}
/// Statements leading to application schema `version`, run in order in
/// one transaction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    pub version: u32,
    pub statements: Vec<MigrationStep>,
}
/// One statement of a migration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum MigrationStep {
    /// SQL run as it is.
    Sql(String),
    /// A row of table `insert` for each row `select` returns, its columns
    /// named like the table's, with an `id` Rocky issues.
    Insert { insert: String, select: String },
}
fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && name.as_bytes()[0].is_ascii_alphabetic()
}
impl Schema {
    pub fn parse(json: &str) -> Result<Self, Failure> {
        if json.len() > rocky_core::MAX_DEFINITION_BYTES {
            return Err("schema quota".into());
        }
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        let version: Format = serde_json::from_str(json)?;
        if version.format != rocky_core::DEFINITION_FORMAT {
            return Err("unsupported schema definition format".into());
        }
        let mut schema: Self = serde_json::from_str(json)?;
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
    fn validate(&self) -> Result<(), Failure> {
        if !identifier(&self.application) || self.version == 0 {
            return Err("invalid application schema".into());
        }
        if self.format != rocky_core::DEFINITION_FORMAT {
            return Err("unsupported schema definition format".into());
        }
        {
            let tables = &self.tables;
            if tables.is_empty()
                || tables.len() > 64
                || tables
                    .iter()
                    .any(|table| table.columns.is_empty() || table.columns.len() > 128)
            {
                return Err("definition table or column quota".into());
            }
            if tables
                .iter()
                .any(|table| table.name.eq_ignore_ascii_case(crate::domain::REGISTRY))
            {
                return Err(format!(
                    "{} is reserved for the SDK's issuer registry",
                    crate::domain::REGISTRY
                )
                .into());
            }
            self.definition([0; 32])?
                .build()?
                .supported_on(rocky_core::Profile::Secure)?;
            self.validate_granted_like()?;
            if let Some(table) = self
                .migrations
                .iter()
                .flat_map(|migration| &migration.statements)
                .find_map(|step| match step {
                    MigrationStep::Insert { insert, .. }
                        if !self.tables.iter().any(|table| {
                            table.name == *insert
                                && table.class == rocky_core::TableClass::Mergeable
                        }) =>
                    {
                        Some(insert)
                    }
                    _ => None,
                })
            {
                return Err(format!(
                    "a migration inserts into {table}, which is no mergeable table"
                )
                .into());
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
                return Err(
                    "migrations name ascending versions up to the schema's, each with 1 to 64 statements"
                        .into(),
                );
            }
            Ok(())
        }
    }

    pub(crate) fn granted_like(&self) -> BTreeMap<String, String> {
        self.tables
            .iter()
            .filter_map(|table| {
                table
                    .granted_like
                    .as_ref()
                    .map(|like| (table.name.clone(), like.clone()))
            })
            .collect()
    }
    /// Each table follows one mergeable table that follows none.
    fn validate_granted_like(&self) -> Result<(), Failure> {
        let mergeable = |name: &str| {
            self.tables
                .iter()
                .any(|table| table.name == name && table.class == rocky_core::TableClass::Mergeable)
        };
        let followers = self.granted_like();
        for (table, like) in &followers {
            if table == like
                || !mergeable(table)
                || !mergeable(like)
                || followers.contains_key(like)
            {
                return Err(format!(
                    "{table} cannot be granted like {like}: both must be distinct mergeable tables, and {like} granted like none"
                )
                .into());
            }
        }
        Ok(())
    }
    pub fn definition(&self, publisher: [u8; 32]) -> Result<rocky_core::SchemaDefinition, Failure> {
        Ok(rocky_core::SchemaDefinition {
            format: self.format,
            publisher: data_encoding::HEXLOWER.encode(&publisher),
            application: self.application.clone(),
            update_policy: self
                .update_policy
                .unwrap_or(rocky_core::UpdatePolicy::Changed),
            views: self.views.clone(),
            tables: self.tables.clone(),
        })
    }
    pub(crate) fn names(&self) -> Vec<&str> {
        self.tables
            .iter()
            .map(|table| table.name.as_str())
            .collect()
    }
}
