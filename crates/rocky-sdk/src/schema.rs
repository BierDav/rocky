use crate::Failure;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<u32>,
    pub application: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<Collection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tables: Option<Vec<rocky_core::TableDefinition>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<rocky_core::LogicalView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_policy: Option<rocky_core::UpdatePolicy>,
    /// Typed strong commands over the collections. Part of the signed
    /// definition: every device of a domain declares the same ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<rocky_core::CommandDefinitions>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collection {
    pub name: String,
    pub fields: Vec<Field>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub kind: FieldKind,
    #[serde(default)]
    pub nullable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Boolean,
    Integer,
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
        let mut schema: Self = serde_json::from_str(json)?;
        if schema.tables.is_none() && json.len() > 128 * 1024 {
            return Err("schema quota".into());
        }
        schema.validate()?;
        if let Some(tables) = &mut schema.tables {
            tables.sort_by(|a, b| {
                a.logical
                    .as_ref()
                    .unwrap_or(&a.name)
                    .cmp(b.logical.as_ref().unwrap_or(&b.name))
            });
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
        if let Some(tables) = &self.tables {
            if !self.collections.is_empty() || self.version == 0 {
                return Err("definition tables cannot be combined with legacy collections".into());
            }
            if tables.is_empty()
                || tables.len() > 64
                || tables
                    .iter()
                    .any(|table| table.columns.is_empty() || table.columns.len() > 128)
            {
                return Err("definition table or column quota".into());
            }
            self.definition([0; 32])?
                .build()?
                .supported_on(rocky_core::Profile::Secure)?;
            let names: BTreeSet<_> = tables.iter().map(|table| &table.name).collect();
            self.validate_commands(&names)?;
            // Only confirmed commands write such a table: one must name it.
            let commanded: BTreeSet<String> = match &self.commands {
                Some(commands) => commands
                    .build()?
                    .descriptors()
                    .iter()
                    .flat_map(|descriptor| descriptor.resources.clone())
                    .collect(),
                None => BTreeSet::new(),
            };
            if let Some(table) = tables.iter().find(|table| {
                table.class == rocky_core::TableClass::CommandControlled
                    && !commanded.contains(&table.name)
            }) {
                return Err(format!(
                    "command-controlled table {} is a resource of no command",
                    table.name
                )
                .into());
            }
            return Ok(());
        }
        Err("legacy register schemas are unsupported; use format-2 definition tables; export with the original build and reinstall into a new database".into())
    }

    fn validate_commands(&self, names: &BTreeSet<&String>) -> Result<(), Failure> {
        if let Some(commands) = &self.commands {
            for descriptor in commands.build()?.descriptors() {
                if let Some(scope) = descriptor
                    .resources
                    .iter()
                    .find(|scope| !names.contains(scope))
                {
                    return Err(format!(
                        "command {} names {scope}, which is not a collection",
                        descriptor.name
                    )
                    .into());
                }
            }
        }
        Ok(())
    }
    pub fn definition(&self, publisher: [u8; 32]) -> Result<rocky_core::SchemaDefinition, Failure> {
        Ok(rocky_core::SchemaDefinition {
            format: self
                .format
                .unwrap_or(rocky_core::ROW_IDENTITY_SCHEMA_FORMAT),
            publisher: data_encoding::HEXLOWER.encode(&publisher),
            application: self.application.clone(),
            update_policy: self
                .update_policy
                .unwrap_or(rocky_core::UpdatePolicy::Changed),
            views: self.views.clone(),
            tables: self
                .tables
                .clone()
                .ok_or("legacy schema has no definition tables")?,
        })
    }
    pub(crate) fn names(&self) -> Vec<&str> {
        self.tables
            .iter()
            .flatten()
            .map(|table| table.name.as_str())
            .collect()
    }
}
