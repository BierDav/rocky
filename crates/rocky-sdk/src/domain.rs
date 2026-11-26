use crate::{Failure, schema::Schema};
use rocky_core::{
    ApplicationManifest, Commitment, DomainId, LogicalSchema, MeshId, ResourceScope,
    RowApplication, Scope, TypedRows,
};
use std::collections::BTreeMap;

pub(crate) const REGISTRY: &str = "issuer_registry";

pub(crate) struct Domain {
    pub app: TypedRows,
    pub scope: Scope,
    tables: BTreeMap<String, ResourceScope>,
    names: BTreeMap<String, String>,
    pub whole_tables: rocky_credentials::WholeTables,
}
impl Domain {
    pub fn new(
        schema: &Schema,
        publisher: [u8; 32],
        mesh: MeshId,
        domain: DomainId,
    ) -> Result<Self, Failure> {
        Self::at(
            schema,
            publisher,
            mesh,
            domain,
            schema.version,
            rocky_core::EngineIdentity::current(),
        )
    }
    pub fn at(
        schema: &Schema,
        publisher: [u8; 32],
        mesh: MeshId,
        domain: DomainId,
        generation: u32,
        engine: rocky_core::EngineIdentity,
    ) -> Result<Self, Failure> {
        let logical = logical(schema, publisher)?;
        let scope = Scope {
            mesh,
            app: logical.app_id(),
            domain,
        };
        let tables: BTreeMap<String, ResourceScope> = logical
            .typed()
            .map(|table| {
                (
                    table.identity().to_owned(),
                    ResourceScope {
                        domain: scope,
                        encryption: rocky_core::commitment(
                            "rocky.sdk.readers.v1",
                            table.identity().as_bytes(),
                        ),
                        table: logical.table_id(table),
                    },
                )
            })
            .collect();
        let names = logical
            .typed()
            .map(|table| (table.sql_name.clone(), table.identity().to_owned()))
            .collect();
        let app = TypedRows::new(logical, vec![domain])?
            .at_generation(generation)?
            .on_engine(engine);
        let whole_tables = rocky_credentials::WholeTables::new(
            &app.manifest(),
            &tables.values().copied().collect::<Vec<_>>(),
        )?;
        Ok(Self {
            app,
            whole_tables,
            scope,
            tables,
            names,
        })
    }
    pub fn effective_partitions(
        &self,
        grants: &[rocky_credentials::PartitionGrant],
    ) -> Vec<rocky_credentials::PartitionGrant> {
        let logical = self.logical();
        grants
            .iter()
            .filter(|grant| {
                logical
                    .tables
                    .iter()
                    .any(|table| logical.table_id(table) == grant.table)
            })
            .cloned()
            .collect()
    }
    pub fn verified(
        &self,
        chain: &rocky_credentials::CredentialChain,
        publisher: rocky_credentials::PublicKey,
        revocations: &rocky_credentials::RevocationSet,
        time: u64,
    ) -> Result<rocky_credentials::VerifiedChain, rocky_credentials::Error> {
        chain.verified(publisher, revocations, time, &self.whole_tables)
    }

    pub fn logical(&self) -> &LogicalSchema {
        self.app.schema()
    }
    pub fn manifest(&self) -> ApplicationManifest {
        self.app.manifest()
    }
    pub fn scopes(&self) -> impl Iterator<Item = (&str, ResourceScope)> {
        self.tables
            .iter()
            .map(|(name, scope)| (name.as_str(), *scope))
    }
    pub fn partitioned_table(&self, scope: &ResourceScope) -> bool {
        let logical = self.logical();
        logical.tables.iter().any(|table| {
            !table.partition_by.is_empty() && self.scope_of(&table.sql_name) == Some(*scope)
        })
    }
    pub fn readers(&self) -> BTreeMap<String, Commitment> {
        self.names
            .keys()
            .filter_map(|name| Some((name.clone(), self.scope_of(name)?.encryption)))
            .collect()
    }
    pub fn registry(&self) -> ResourceScope {
        self.tables[REGISTRY]
    }

    pub fn scope_of(&self, table: &str) -> Option<ResourceScope> {
        self.tables.get(self.names.get(table)?).copied()
    }
}
pub(crate) fn logical(schema: &Schema, publisher: [u8; 32]) -> Result<LogicalSchema, Failure> {
    let mut definition = schema.definition(publisher);
    definition.tables.push(registry_table()?);
    Ok(definition.build()?)
}
pub(crate) fn registry_table() -> Result<rocky_core::TableDefinition, Failure> {
    Ok(serde_json::from_value(serde_json::json!({
        "name": REGISTRY,
        "explicit_access_only": true,
        "class": "replayed",
        "columns": [
            {"name": "identity", "type": "text", "not_null": true},
            {"name": "entry", "type": "text", "not_null": true},
            {"name": "value", "type": "text", "not_null": true},
        ],
        "primary_key": ["identity", "entry"],
    }))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_core::Affinity;
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn typed_app_and_sdk_definitions_refuse_historical_formats() {
        let json = r#"{"format":10,"application":"refusal","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"title","type":"text"}],"primary_key":["id"]}]}"#;
        let mut definition = Schema::parse(json).unwrap().definition([1; 32]);
        let descriptor = definition.build().unwrap();
        for format in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11] {
            definition.format = format;
            assert!(
                definition
                    .build()
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported schema definition format")
            );
            let input = json.replace("\"format\":10", &format!("\"format\":{format}"));
            let error = Schema::parse(&input).unwrap_err().to_string();
            assert!(error.contains("unsupported schema"), "{error}");
        }
        let current = rocky_core::LOGICAL_SCHEMA_FORMAT;
        assert_eq!(descriptor.format, current);
        for format in (0..current).chain([current + 1]) {
            let mut old = descriptor.clone();
            old.format = format;
            let error = rocky_core::TypedRows::new(old, vec![])
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("unsupported schema"), "{error}");
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn definition_tables_keep_keys_indexes_and_scalar_types() -> Result<(), Failure> {
        let schema = Schema::parse(
            r#"{"format":10,"application":"measurements","version":1,"tables":[{
            "name":"samples","class":"replayed",
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
        assert_eq!(logical, &super::logical(&schema, [9; 32])?);
        logical.supported_on(rocky_core::Profile::Secure)?;
        logical.supported_on(rocky_core::Profile::Secure)?;
        assert_eq!(
            schema
                .tables
                .iter()
                .map(|table| table.name.as_str())
                .collect::<Vec<_>>(),
            ["samples"]
        );
        Ok(())
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn identity_references_keep_independent_readers_and_table_resources() -> Result<(), Failure> {
        let schema = Schema::parse(
            r#"{"format":10,"application":"references","version":1,"tables":[
            {"name":"parents","class":"replayed","columns":[{"name":"next","type":"blob"}],"primary_key":["id"],
             "foreign_keys":[{"columns":["next"],"table":"children","on_delete":"set_null"}]},
            {"name":"children","class":"replayed","columns":[{"name":"parent","type":"blob"}],"primary_key":["id"],
             "foreign_keys":[{"columns":["parent"],"table":"parents","on_delete":"cascade"}]},
            {"name":"other","class":"replayed","columns":[{"name":"title","type":"text"}],"primary_key":["id"]}
        ]}"#,
        )?;
        let domain = Domain::new(&schema, [9; 32], MeshId([1; 16]), DomainId([2; 16]))?;
        let resources: BTreeMap<_, _> = domain.scopes().collect();
        let child = resources.get("children").ok_or("child")?;
        let parent = resources.get("parents").ok_or("parent")?;
        assert_ne!(child.encryption, parent.encryption);
        assert_ne!(child.table, parent.table);
        assert_eq!(
            child.encryption,
            rocky_core::commitment("rocky.sdk.readers.v1", b"children")
        );
        assert_eq!(
            parent.encryption,
            rocky_core::commitment("rocky.sdk.readers.v1", b"parents")
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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn removed_command_definitions_are_refused() {
        let json = r#"{"format":10,"application":"market","version":1,"tables":[{"name":"stock","class":"command_controlled","columns":[{"name":"item","type":"text","not_null":true}],"primary_key":["item"]}]}"#;
        assert!(Schema::parse(json).is_err());
        let confirmed = json.replace("command_controlled", "confirmed_read_only");
        assert!(Schema::parse(&confirmed).is_err());
        let ordinary = json.replace("command_controlled", "replayed");
        assert!(Schema::parse(&ordinary).is_ok());
        let commands = ordinary.replace(
            "\"version\":1",
            "\"version\":1,\"commands\":{\"format\":1,\"handler\":1,\"commands\":[]}",
        );
        assert!(Schema::parse(&commands).is_err());
    }
}
