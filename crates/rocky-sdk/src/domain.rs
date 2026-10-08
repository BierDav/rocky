//! Format-2 typed secure tables of a consumer schema in one domain.
use crate::{Failure, schema::Schema};
use rocky_core::{
    ApplicationManifest, Commitment, DomainId, LogicalSchema, MeshId, ResourceScope,
    RowApplication, Scope, TypedCommands, TypedRows,
};
use rocky_credentials::Resource;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Domain {
    /// The typed tables and commands. It holds no handler until `hosted`.
    pub app: TypedCommands,
    pub scope: Scope,
    tables: BTreeMap<String, ResourceScope>,
}
impl Domain {
    pub fn new(
        schema: &Schema,
        publisher: [u8; 32],
        mesh: MeshId,
        domain: DomainId,
    ) -> Result<Self, Failure> {
        Self::at(schema, publisher, mesh, domain, 1, &BTreeSet::new())
    }
    /// The domain at schema generation `generation`, where `added` names
    /// the nullable `collection.field`s transitions added to collections
    /// that existed before them. See `docs/runtime/generations.md`.
    pub fn at(
        schema: &Schema,
        publisher: [u8; 32],
        mesh: MeshId,
        domain: DomainId,
        generation: u32,
        added: &BTreeSet<String>,
    ) -> Result<Self, Failure> {
        let logical = logical(schema, publisher)?;
        let added = logical
            .tables
            .iter()
            .flat_map(|table| {
                table
                    .columns
                    .iter()
                    .filter(|column| {
                        added.contains(&format!("{}.{}", table.logical, column.logical))
                    })
                    .map(|column| logical.field_id(table, column))
            })
            .collect();
        let scope = Scope {
            mesh,
            app: logical.app_id(),
            domain,
        };
        // Reference-connected tables share the phase-12 reader scope. Table grants
        // and encryption keys remain separate ResourceScopes. A reference to a
        // table with an existence log may cross scopes (`docs/runtime/existence-logs.md`).
        let mut readers: BTreeMap<String, String> = logical
            .tables
            .iter()
            .map(|table| (table.sql_name.clone(), table.sql_name.clone()))
            .collect();
        loop {
            let mut changed = false;
            for table in &logical.tables {
                for key in table.foreign_keys.iter().filter(|key| {
                    logical
                        .table(&key.table)
                        .is_none_or(|parent| !parent.existence_log)
                }) {
                    let left = readers
                        .get(&table.sql_name)
                        .ok_or("missing child reader scope")?
                        .clone();
                    let right = readers
                        .get(&key.table)
                        .ok_or("missing parent reader scope")?
                        .clone();
                    let first = left.min(right);
                    for name in [&table.sql_name, &key.table] {
                        let reader = readers.get_mut(name).ok_or("missing reader scope")?;
                        if *reader != first {
                            *reader = first.clone();
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let tables: BTreeMap<String, ResourceScope> = logical
            .tables
            .iter()
            .map(|table| {
                (
                    table.sql_name.clone(),
                    ResourceScope {
                        domain: scope,
                        encryption: rocky_core::commitment(
                            "rocky.sdk.readers.v1",
                            readers
                                .get(&table.sql_name)
                                .map_or(table.sql_name.as_bytes(), |name| name.as_bytes()),
                        ),
                        table: logical.table_id(table),
                    },
                )
            })
            .collect();
        let commands = match &schema.commands {
            Some(definitions) => definitions.build()?,
            None => Default::default(),
        };
        Ok(Self {
            app: TypedCommands::new(
                TypedRows::new(logical, vec![domain])?.at_generation(generation, added)?,
                commands,
                tables.clone(),
                None,
            )?,
            scope,
            tables,
        })
    }
    /// This domain with the handlers its device executes commands with.
    pub fn hosted(mut self, host: Option<std::sync::Arc<dyn rocky_core::CommandHost>>) -> Self {
        self.app = self.app.with_host(host);
        self
    }
    pub fn logical(&self) -> &LogicalSchema {
        self.app.rows().schema()
    }
    /// The signed definition: the tables and, with commands, their methods
    /// and handler generation.
    pub fn manifest(&self) -> ApplicationManifest {
        self.app.manifest()
    }
    /// Collections and their resource scopes, in name order.
    pub fn scopes(&self) -> impl Iterator<Item = (&str, ResourceScope)> {
        self.tables
            .iter()
            .map(|(name, scope)| (name.as_str(), *scope))
    }
    pub fn readers(&self) -> BTreeMap<String, Commitment> {
        self.tables
            .iter()
            .map(|(name, scope)| (name.clone(), scope.encryption))
            .collect()
    }
    /// What an issuer can grant: every collection, its scope and columns.
    pub fn resources(&self) -> Vec<Resource> {
        let logical = self.logical();
        let manifest = self.manifest();
        let commands = self.app.commands().descriptors();
        let methods = |named: &dyn Fn(&rocky_core::CommandDescriptor) -> bool| {
            commands
                .iter()
                .filter(|descriptor| named(descriptor))
                .map(|descriptor| manifest.item_id("method", &descriptor.name))
                .collect()
        };
        logical
            .tables
            .iter()
            .map(|table| Resource {
                name: table.sql_name.clone(),
                scope: self.tables[&table.sql_name],
                columns: table
                    .columns
                    .iter()
                    .map(|column| (column.logical.clone(), logical.field_id(table, column)))
                    .collect(),
                invokes: methods(&|descriptor| descriptor.invocation == table.sql_name),
                executes: methods(&|descriptor| descriptor.resources.contains(&table.sql_name)),
            })
            .collect()
    }
}
/// The format-3 typed tables of `schema` under `publisher`.
pub(crate) fn logical(schema: &Schema, publisher: [u8; 32]) -> Result<LogicalSchema, Failure> {
    Ok(schema.definition(publisher)?.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_core::Affinity;
    #[test]
    fn typed_app_and_sdk_definitions_refuse_historical_formats() {
        let json = r#"{"format":4,"application":"refusal","version":1,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"title","type":"text"}],"primary_key":["id"]}]}"#;
        // The portable builder supplies both required columns.
        let mut definition = Schema::parse(json).unwrap().definition([1; 32]).unwrap();
        let descriptor = definition.build().unwrap();
        for format in [0, 1, 2, 3, 5] {
            definition.format = format;
            assert!(
                definition
                    .build()
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported schema definition format")
            );
            let mut old = descriptor.clone();
            old.format = format;
            let error = crate::TypedApp::new(old, &[]).err().unwrap().to_string();
            assert!(error.contains("unsupported schema"), "{error}");
            let input = json.replace("\"format\":4", &format!("\"format\":{format}"));
            let error = Schema::parse(&input).unwrap_err().to_string();
            assert!(error.contains("unsupported schema"), "{error}");
        }
    }

    #[test]
    fn definition_tables_keep_keys_indexes_and_scalar_types() -> Result<(), Failure> {
        let schema = Schema::parse(
            r#"{"format":4,"application":"measurements","version":1,"tables":[{
            "name":"samples","class":"mergeable",
            "columns":[{"name":"sensor","type":"text","not_null":true},
                       {"name":"sequence","type":"integer","not_null":true},
                       {"name":"reading","type":"real"},{"name":"raw","type":"blob"}],
            "primary_key":["sensor","sequence"],
            "indexes":[{"name":"samples_by_reading","columns":[{"name":"reading","descending":true}]}]
        }]}"#,
        )?;
        let domain = Domain::new(&schema, [9; 32], MeshId([1; 16]), DomainId([2; 16]))?;
        let logical = domain.logical();
        let table = logical.table("samples").ok_or("table")?;
        assert_eq!(table.primary_key, ["sensor", "sequence"]);
        assert!(
            table
                .create_sql
                .contains("PRIMARY KEY(\"sensor\",\"sequence\")")
        );
        assert_eq!(
            table.column("reading").ok_or("reading")?.affinity,
            Affinity::Real
        );
        assert_eq!(table.column("raw").ok_or("raw")?.affinity, Affinity::Blob);
        assert!(table.indexes[0].create_sql.contains("\"reading\" DESC"));
        assert_eq!(logical, &schema.definition([9; 32])?.build()?);
        logical.supported_on(rocky_core::Profile::Trusted)?;
        logical.supported_on(rocky_core::Profile::Secure)?;
        assert_eq!(schema.names(), ["samples"]);
        Ok(())
    }

    #[test]
    fn identity_references_share_readers_but_keep_table_resources() -> Result<(), Failure> {
        let schema = Schema::parse(
            r#"{"format":4,"application":"references","version":1,"tables":[
            {"name":"parents","class":"mergeable","columns":[{"name":"next","type":"integer"}],"primary_key":["id"],
             "foreign_keys":[{"columns":["next"],"table":"children","on_delete":"set_null"}]},
            {"name":"children","class":"mergeable","columns":[{"name":"parent","type":"integer"}],"primary_key":["id"],
             "foreign_keys":[{"columns":["parent"],"table":"parents","on_delete":"cascade"}]},
            {"name":"other","class":"mergeable","columns":[{"name":"title","type":"text"}],"primary_key":["id"]}
        ]}"#,
        )?;
        let domain = Domain::new(&schema, [9; 32], MeshId([1; 16]), DomainId([2; 16]))?;
        let resources: BTreeMap<_, _> = domain.scopes().collect();
        let child = resources.get("children").ok_or("child")?;
        let parent = resources.get("parents").ok_or("parent")?;
        assert_eq!(child.encryption, parent.encryption);
        assert_ne!(child.table, parent.table);
        assert_eq!(
            child.encryption,
            rocky_core::commitment("rocky.sdk.readers.v1", b"children")
        );
        assert_eq!(
            resources.get("other").ok_or("other")?.encryption,
            rocky_core::commitment("rocky.sdk.readers.v1", b"other")
        );
        let mut reordered = schema.clone();
        reordered.tables.reverse();
        assert_eq!(
            domain.readers(),
            Domain::new(&reordered, [9; 32], MeshId([1; 16]), DomainId([2; 16]))?.readers()
        );
        Ok(())
    }

    #[test]
    fn references_to_an_existence_log_keep_their_reader_scopes() -> Result<(), Failure> {
        let schema = Schema::parse(
            r#"{"format":4,"application":"logs","version":1,"tables":[
            {"name":"lists","class":"mergeable","columns":[{"name":"title","type":"text"}],"primary_key":["id"],"existence_log":true},
            {"name":"items","class":"mergeable","columns":[{"name":"list","type":"integer","not_null":true}],"primary_key":["id"],
             "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"}]}
        ]}"#,
        )?;
        let domain = Domain::new(&schema, [9; 32], MeshId([1; 16]), DomainId([2; 16]))?;
        let resources: BTreeMap<_, _> = domain.scopes().collect();
        assert_eq!(
            resources.get("items").ok_or("items")?.encryption,
            rocky_core::commitment("rocky.sdk.readers.v1", b"items")
        );
        assert_eq!(
            resources.get("lists").ok_or("lists")?.encryption,
            rocky_core::commitment("rocky.sdk.readers.v1", b"lists")
        );
        let logical = domain.logical();
        assert_eq!(
            logical
                .existence_tables()
                .map(|table| table.sql_name.as_str())
                .collect::<Vec<_>>(),
            ["lists"]
        );
        logical.supported_on(rocky_core::Profile::Secure)?;
        Ok(())
    }

    #[test]
    fn a_command_controlled_table_needs_a_command() {
        let json = r#"{"format":4,"application":"market","version":1,"tables":[{"name":"stock","class":"command_controlled","columns":[{"name":"item","type":"text","not_null":true}],"primary_key":["item"]}]}"#;
        let error = Schema::parse(json).unwrap_err().to_string();
        assert!(error.contains("resource of no command"), "{error}");
        let confirmed = json.replace("command_controlled", "confirmed_read_only");
        assert!(Schema::parse(&confirmed).is_err());
    }
}
