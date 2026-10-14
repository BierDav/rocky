//! Format-2 typed secure tables of a consumer schema in one domain.
use crate::{Failure, schema::Schema};
use rocky_core::{
    ApplicationManifest, Commitment, DomainId, LogicalSchema, MeshId, ResourceScope,
    RowApplication, Scope, TypedCommands, TypedRows,
};
use rocky_credentials::{Grant, Profile, Resource};
use std::collections::{BTreeMap, BTreeSet};

/// The reserved collection of issuer records; see
/// `docs/decisions/0048-replicated-issuer-registry.md`.
pub(crate) const REGISTRY: &str = "issuer_registry";

pub(crate) struct Domain {
    /// The typed tables and commands. It holds no handler until `hosted`.
    pub app: TypedCommands,
    pub scope: Scope,
    tables: BTreeMap<String, ResourceScope>,
    granted_like: BTreeMap<String, String>,
    /// Ids of tables and fields earlier generations removed: grants that
    /// still name them read as if they did not.
    removed: BTreeSet<Commitment>,
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
    /// The domain at schema generation `generation`, where `added` holds
    /// the field ids transitions added to, or required of, tables that
    /// existed before them; ids this schema no longer holds are ignored.
    /// See `docs/runtime/generations.md`.
    pub fn at(
        schema: &Schema,
        publisher: [u8; 32],
        mesh: MeshId,
        domain: DomainId,
        generation: u32,
        added: &BTreeSet<Commitment>,
    ) -> Result<Self, Failure> {
        let logical = logical(schema, publisher)?;
        let added = rocky_core::fillable(&logical)
            .intersection(added)
            .copied()
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
            granted_like: schema.granted_like.clone(),
            removed: BTreeSet::new(),
        })
    }
    /// This domain, whose lineage removed the table and field ids
    /// `removed`; a name added again is current.
    pub fn removing(mut self, removed: BTreeSet<Commitment>) -> Self {
        let logical = self.logical();
        let current: BTreeSet<Commitment> = logical
            .tables
            .iter()
            .flat_map(|table| {
                table
                    .columns
                    .iter()
                    .map(|column| logical.field_id(table, column))
                    .chain([logical.table_id(table)])
            })
            .collect();
        self.removed = removed.difference(&current).copied().collect();
        self
    }
    /// Whether a generation removed the table or `table.field` `name`.
    fn removed(&self, name: &str) -> bool {
        let logical = self.logical();
        let kind = if name.contains('.') {
            "field"
        } else {
            "entity"
        };
        self.removed.contains(&rocky_core::definition_item_id(
            logical.publisher,
            logical.app_id(),
            kind,
            name,
        ))
    }
    /// Partition grants as this generation reads them. A grant that named
    /// every field of its table, apart from fields later generations added,
    /// covers the whole row; and with the schema's `granted_like`, a
    /// whole-row grant on a followed table also selects the same partitions
    /// of the table following it, slot by slot.
    pub fn effective_partitions(
        &self,
        grants: &[rocky_credentials::PartitionGrant],
    ) -> Vec<rocky_credentials::PartitionGrant> {
        let logical = self.logical();
        let resources = self.resources();
        let fields = |name: &str| -> BTreeSet<Commitment> {
            resources
                .iter()
                .find(|resource| resource.name == name)
                .map(|resource| resource.columns.values().copied().collect())
                .unwrap_or_default()
        };
        let slots = |table: &rocky_core::LogicalTable| -> Vec<Commitment> {
            table
                .partition_by
                .iter()
                .filter_map(|name| table.column(name))
                .map(|column| logical.field_id(table, column))
                .collect()
        };
        let added = self.app.rows().added();
        let current: BTreeSet<Commitment> = resources
            .iter()
            .flat_map(|resource| resource.columns.values().copied())
            .collect();
        let table_name = |id: &Commitment| {
            logical
                .tables
                .iter()
                .find(|table| logical.table_id(table) == *id)
                .map(|table| table.sql_name.clone())
        };
        // Tables and fields a later generation removed are dropped.
        let grants: Vec<_> = grants
            .iter()
            .filter(|grant| table_name(&grant.table).is_some())
            .cloned()
            .map(|mut grant| {
                grant.fields.retain(|field| current.contains(field));
                if let Some(name) = table_name(&grant.table) {
                    let all = fields(&name);
                    if !grant.fields.is_empty()
                        && grant.fields.is_subset(&all)
                        && all
                            .iter()
                            .all(|field| grant.fields.contains(field) || added.contains(field))
                    {
                        grant.fields = all;
                    }
                }
                grant
            })
            .collect();
        let mut all = grants.clone();
        for (name, like) in &self.granted_like {
            let (Some(table), Some(parent)) = (logical.table(name), logical.table(like)) else {
                continue;
            };
            let (id, parent_id) = (logical.table_id(table), logical.table_id(parent));
            let (own, parents) = (slots(table), slots(parent));
            if own.len() != parents.len() || grants.iter().any(|grant| grant.table == id) {
                continue;
            }
            for grant in grants.iter().filter(|grant| {
                grant.table == parent_id
                    && grant.rows == rocky_core::RowSelection::All
                    && grant.fields == fields(like)
            }) {
                let selector: BTreeMap<_, _> = grant
                    .selector
                    .0
                    .iter()
                    .filter_map(|(slot, selection)| {
                        let index = parents.iter().position(|parent| parent == slot)?;
                        Some((own[index], selection.clone()))
                    })
                    .collect();
                if selector.len() != grant.selector.0.len() {
                    continue;
                }
                all.push(rocky_credentials::PartitionGrant {
                    domain: grant.domain,
                    table: id,
                    selector: rocky_core::PartitionSelector(selector),
                    rows: rocky_core::RowSelection::All,
                    rights: grant
                        .rights
                        .iter()
                        .copied()
                        .filter(|right| {
                            !matches!(
                                right,
                                rocky_core::Right::Invoke | rocky_core::Right::Execute
                            )
                        })
                        .collect(),
                    fields: fields(name),
                });
            }
        }
        all
    }
    /// `profile` as this generation reads it: without the tables and
    /// columns a later generation removed, and with the schema's
    /// `granted_like` applied: a table granted like another receives the
    /// same rights wherever the profile names that one in whole rows and is
    /// silent on it. Commands stay with their own tables. Every
    /// interpretation of a profile goes through here.
    pub fn effective(&self, profile: &Profile) -> Profile {
        let profile = &self.current(profile);
        let follow = |scopes: &std::collections::BTreeSet<String>| {
            let mut scopes = scopes.clone();
            for (table, like) in &self.granted_like {
                if scopes.contains(like) {
                    scopes.insert(table.clone());
                }
            }
            scopes
        };
        match profile {
            Profile::Issuer => Profile::Issuer,
            Profile::Custody { scopes } => Profile::Custody {
                scopes: follow(scopes),
            },
            Profile::Reader { scopes } => Profile::Reader {
                scopes: follow(scopes),
            },
            Profile::Writer { scopes } => Profile::Writer {
                scopes: follow(scopes),
            },
            Profile::Grants { grants } => {
                let mut all = grants.clone();
                for (table, like) in &self.granted_like {
                    if grants.iter().any(|grant| grant.scope == *table) {
                        continue;
                    }
                    all.extend(
                        grants
                            .iter()
                            .filter(|grant| {
                                grant.scope == *like
                                    && grant
                                        .columns
                                        .as_ref()
                                        .is_none_or(|columns| columns.is_empty())
                            })
                            .map(|grant| Grant {
                                scope: table.clone(),
                                rights: grant
                                    .rights
                                    .iter()
                                    .copied()
                                    .filter(|right| {
                                        !matches!(
                                            right,
                                            rocky_core::Right::Invoke | rocky_core::Right::Execute
                                        )
                                    })
                                    .collect(),
                                columns: grant.columns.clone(),
                            })
                            .filter(|grant| !grant.rights.is_empty()),
                    );
                }
                Profile::Grants { grants: all }
            }
        }
    }
    /// `profile` without the tables and columns earlier generations
    /// removed; any other unknown name is still refused where it resolves.
    fn current(&self, profile: &Profile) -> Profile {
        let named = |scopes: &BTreeSet<String>| -> BTreeSet<String> {
            scopes
                .iter()
                .filter(|name| !self.removed(name))
                .cloned()
                .collect()
        };
        match profile {
            Profile::Issuer => Profile::Issuer,
            Profile::Custody { scopes } => Profile::Custody {
                scopes: named(scopes),
            },
            Profile::Reader { scopes } => Profile::Reader {
                scopes: named(scopes),
            },
            Profile::Writer { scopes } => Profile::Writer {
                scopes: named(scopes),
            },
            Profile::Grants { grants } => Profile::Grants {
                grants: grants
                    .iter()
                    .filter(|grant| !self.removed(&grant.scope))
                    .cloned()
                    .map(|mut grant| {
                        if let Some(columns) = &mut grant.columns {
                            columns
                                .retain(|name| !self.removed(&format!("{}.{name}", grant.scope)));
                        }
                        grant
                    })
                    .collect(),
            },
        }
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
    /// Whether `scope` is a partitioned table's own scope, which holds no
    /// rows: each partition has its own scope and checkpoint.
    pub fn partitioned_table(&self, scope: &ResourceScope) -> bool {
        let logical = self.logical();
        self.scopes().any(|(name, own)| {
            own == *scope
                && logical
                    .table(name)
                    .is_some_and(|table| !table.partition_by.is_empty())
        })
    }
    pub fn readers(&self) -> BTreeMap<String, Commitment> {
        self.tables
            .iter()
            .map(|(name, scope)| (name.clone(), scope.encryption))
            .collect()
    }
    /// The registry's resource scope.
    pub fn registry(&self) -> ResourceScope {
        self.tables[REGISTRY]
    }
    /// The application's collections, without the registry: what sessions
    /// of the application read and write.
    pub fn collections(&self) -> impl Iterator<Item = (&str, ResourceScope)> {
        self.scopes().filter(|(name, _)| *name != REGISTRY)
    }
    /// What an issuer can grant: every application collection, its scope
    /// and columns. The registry is no grantable resource.
    pub fn resources(&self) -> Vec<Resource> {
        self.all_resources()
            .into_iter()
            .filter(|resource| resource.name != REGISTRY)
            .collect()
    }
    /// The registry as a resource, for the grants the SDK adds itself.
    pub fn registry_resource(&self) -> Vec<Resource> {
        self.all_resources()
            .into_iter()
            .filter(|resource| resource.name == REGISTRY)
            .collect()
    }
    fn all_resources(&self) -> Vec<Resource> {
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
                    .map(|column| (column.sql_name.clone(), logical.field_id(table, column)))
                    .collect(),
                invokes: methods(&|descriptor| descriptor.invocation == table.sql_name),
                executes: methods(&|descriptor| descriptor.resources.contains(&table.sql_name)),
            })
            .collect()
    }
}
/// The format-3 typed tables of `schema` under `publisher`, with the
/// reserved registry table every SDK domain carries.
pub(crate) fn logical(schema: &Schema, publisher: [u8; 32]) -> Result<LogicalSchema, Failure> {
    let mut definition = schema.definition(publisher)?;
    definition.tables.push(registry_table()?);
    Ok(definition.build()?)
}
/// One row per entry of a device's record, keyed by node and entry.
pub(crate) fn registry_table() -> Result<rocky_core::TableDefinition, Failure> {
    Ok(serde_json::from_value(serde_json::json!({
        "name": REGISTRY,
        "class": "mergeable",
        "columns": [
            {"name": "node", "type": "text", "not_null": true},
            {"name": "entry", "type": "text", "not_null": true},
            {"name": "value", "type": "text", "not_null": true},
        ],
        "primary_key": ["node", "entry"],
    }))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_core::Affinity;
    #[test]
    fn typed_app_and_sdk_definitions_refuse_historical_formats() {
        let json = r#"{"format":6,"application":"refusal","version":1,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"title","type":"text"}],"primary_key":["id"]}]}"#;
        // The portable builder supplies both required columns.
        let mut definition = Schema::parse(json).unwrap().definition([1; 32]).unwrap();
        let descriptor = definition.build().unwrap();
        // Definitions are format 6; the descriptors they build are format 5.
        for format in [0, 1, 2, 3, 4, 5, 7] {
            definition.format = format;
            assert!(
                definition
                    .build()
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported schema definition format")
            );
            let input = json.replace("\"format\":6", &format!("\"format\":{format}"));
            let error = Schema::parse(&input).unwrap_err().to_string();
            assert!(error.contains("unsupported schema"), "{error}");
        }
        assert_eq!(descriptor.format, rocky_core::LOGICAL_SCHEMA_FORMAT);
        for format in [0, 1, 2, 3, 4, 6] {
            let mut old = descriptor.clone();
            old.format = format;
            let error = crate::TypedApp::new(old, &[]).err().unwrap().to_string();
            assert!(error.contains("unsupported schema"), "{error}");
        }
    }

    #[test]
    fn definition_tables_keep_keys_indexes_and_scalar_types() -> Result<(), Failure> {
        let schema = Schema::parse(
            r#"{"format":6,"application":"measurements","version":1,"tables":[{
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
        assert_eq!(logical, &super::logical(&schema, [9; 32])?);
        logical.supported_on(rocky_core::Profile::Trusted)?;
        logical.supported_on(rocky_core::Profile::Secure)?;
        assert_eq!(schema.names(), ["samples"]);
        Ok(())
    }

    #[test]
    fn identity_references_share_readers_but_keep_table_resources() -> Result<(), Failure> {
        let schema = Schema::parse(
            r#"{"format":6,"application":"references","version":1,"tables":[
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
            r#"{"format":6,"application":"logs","version":1,"tables":[
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
        let json = r#"{"format":6,"application":"market","version":1,"tables":[{"name":"stock","class":"command_controlled","columns":[{"name":"item","type":"text","not_null":true}],"primary_key":["item"]}]}"#;
        let error = Schema::parse(json).unwrap_err().to_string();
        assert!(error.contains("resource of no command"), "{error}");
        let confirmed = json.replace("command_controlled", "confirmed_read_only");
        assert!(Schema::parse(&confirmed).is_err());
    }
}
