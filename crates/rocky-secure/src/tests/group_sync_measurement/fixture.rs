use super::*;

pub(crate) struct Fixture {
    pub(crate) publisher: PrivateKey,
    pub(crate) schema: rocky_core::LogicalSchema,
    pub(crate) domain: rocky_core::Scope,
    pub(crate) manifest: SignedManifest,
    pub(crate) revocations: RevocationSet,
}
impl Fixture {
    pub(crate) fn new(tables: usize) -> Self {
        let publisher = PrivateKey::new();
        let mut members = vec![
            serde_json::json!({"name":"customers","class":"mergeable","columns":[{"name":"customer","type":"integer","not_null":true},{"name":"title","type":"text"}],"primary_key":["customer","id"],"partition_by":["customer"],"partition_group":"erp"}),
        ];
        for (name, parent) in [
            ("orders", "customers"),
            ("order_lines", "orders"),
            ("attachments", "order_lines"),
        ]
        .into_iter()
        .take(tables - 1)
        {
            let mut table = serde_json::json!({"name":name,"class":"mergeable","columns":[{"name":"customer","type":"integer","not_null":true},{"name":"parent","type":"integer","not_null":true},{"name":"title","type":"text"}],"primary_key":["customer","id"],"partition_by":["customer"],"partition_group":"erp","foreign_keys":[{"columns":["customer","parent"],"table":parent,"on_delete":"cascade"}]});
            if name == "order_lines" {
                table["columns"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({"name":"product","type":"integer","not_null":true}));
                table["foreign_keys"].as_array_mut().unwrap().push(serde_json::json!({"columns":["product"],"table":"products","on_delete":"cascade"}));
            }
            members.push(table);
        }
        members.push(serde_json::json!({"name":"products","class":"mergeable","columns":[{"name":"title","type":"text"}],"primary_key":["id"],"existence_log":true}));
        let definition = serde_json::json!({"format":4,"publisher":publisher.public_key().to_string(),"application":"erp-measurement","tables":members});
        let schema = SchemaDefinition::parse(&serde_json::to_vec(&definition).unwrap())
            .unwrap()
            .build()
            .unwrap();
        schema.installable().unwrap();
        let domain = rocky_core::Scope {
            mesh: MeshId([1; 16]),
            app: schema.app_id(),
            domain: DomainId([1; 16]),
        };
        let manifest = schema.manifest(&[domain.domain]);
        let revocations = RevocationSet::new();
        Self {
            manifest: SignedManifest::sign(&manifest, &publisher).unwrap(),
            publisher,
            schema,
            domain,
            revocations,
        }
    }
    pub(crate) fn app(&self) -> TypedRows {
        TypedRows::new(self.schema.clone(), vec![self.domain.domain]).unwrap()
    }
    pub(crate) fn tuple(&self, name: &str, partition: i64) -> PartitionTuple {
        let table = self.schema.table(name).unwrap();
        PartitionTuple::new([(
            self.schema
                .field_id(table, table.column(&table.partition_by[0]).unwrap()),
            PartitionValue::new(
                rocky_core::KeyColumn {
                    affinity: rocky_core::Affinity::Integer,
                    collation: rocky_core::Collation::Binary,
                },
                SqlValue::Integer(partition),
            )
            .unwrap(),
        )])
        .unwrap()
    }
    pub(crate) fn scope(&self, name: &str, partition: i64) -> ResourceScope {
        let table = self.schema.table(name).unwrap();
        if table.partition_by.is_empty() {
            return ResourceScope {
                domain: self.domain,
                table: self.schema.table_id(table),
                encryption: [9; 32],
            };
        }
        self.tuple(name, partition)
            .scope(
                self.domain,
                self.schema.table_id(self.schema.table(name).unwrap()),
            )
            .unwrap()
    }
    pub(crate) fn grants(
        &self,
        partitions: Option<&[i64]>,
        rights: &[Right],
    ) -> Vec<PartitionGrant> {
        self.schema
            .tables
            .iter()
            .filter(|table| !table.partition_by.is_empty())
            .map(|table| {
                let column = self
                    .schema
                    .field_id(table, table.column(&table.partition_by[0]).unwrap());
                PartitionGrant {
                    domain: self.domain,
                    table: self.schema.table_id(table),
                    selector: PartitionSelector(
                        [(
                            column,
                            partitions.map_or(PartitionSelection::Any, |values| {
                                PartitionSelection::Only(
                                    values
                                        .iter()
                                        .map(|value| {
                                            self.tuple(&table.sql_name, *value).values()[&column]
                                                .clone()
                                        })
                                        .collect(),
                                )
                            }),
                        )]
                        .into(),
                    ),
                    rows: RowSelection::All,
                    rights: rights.iter().copied().collect(),
                    fields: table
                        .columns
                        .iter()
                        .map(|column| self.schema.field_id(table, column))
                        .collect(),
                }
            })
            .collect()
    }
    pub(crate) async fn open(
        &self,
        path: &Path,
        node: PrivateKey,
        grants: Vec<PartitionGrant>,
        signer: Option<(PrivateKey, u32)>,
        keys: Vec<KeyMaterial>,
    ) -> EncryptedReplica<TypedRows> {
        let user = PrivateKey::new();
        let time = now().unwrap();
        let raw: Vec<_> = self
            .schema
            .tables
            .iter()
            .filter(|table| table.partition_by.is_empty())
            .map(|table| {
                grant(
                    self.scope(&table.sql_name, 0),
                    &[Right::Read, Right::Insert, Right::Update, Right::Delete],
                    &table
                        .columns
                        .iter()
                        .map(|column| self.schema.field_id(table, column))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let root = Authority {
            publisher: &self.publisher,
            manifest: self.app().manifest().hash(),
        }
        .issue_partition_terms(
            user.public_key(),
            raw.clone(),
            grants.clone(),
            (1, Some(AuthorRange::new(1, 1000).unwrap())),
            (time, 3600, 5),
        )
        .unwrap();
        let prefix = CredentialChain(vec![root]);
        let credentials = prefix.clone().complete(&user, node.public_key()).unwrap();
        let replica = EncryptedReplica::open(
            path,
            self.app(),
            SecureConfig {
                publisher: self.publisher.public_key(),
                manifest: self.manifest.clone(),
                revocations: self.revocations.clone(),
                node: node.clone(),
                credentials,
                keys,
            },
        )
        .await
        .unwrap();
        replica
            .install_tables(
                self.schema.clone(),
                self.domain,
                self.schema
                    .tables
                    .iter()
                    .map(|table| (table.sql_name.clone(), [9; 32]))
                    .collect(),
            )
            .await
            .unwrap();
        if let Some((_, number)) = signer {
            for grant in raw {
                let key = PrivateKey::new();
                let credentials = prefix
                    .assign(
                        &user,
                        key.public_key(),
                        number,
                        node.public_key(),
                        vec![grant],
                        time + 3600,
                        (time, 3600, 5),
                    )
                    .unwrap();
                replica.register_row_author(key, credentials).await.unwrap();
            }
            for grant in grants {
                let key = PrivateKey::new();
                let credentials = prefix
                    .assign_partitions(
                        &user,
                        key.public_key(),
                        number,
                        node.public_key(),
                        vec![grant],
                        (time, 3600, 5),
                    )
                    .unwrap();
                replica.register_row_author(key, credentials).await.unwrap();
            }
        }
        replica
    }

    pub(crate) async fn keys(
        &self,
        replica: &EncryptedReplica<TypedRows>,
        partition: i64,
        tables: &[&str],
    ) {
        for name in tables {
            replica
                .register_partition(self.scope(name, partition), self.tuple(name, partition))
                .await
                .unwrap();
            replica
                .install_key(self.key(name, partition))
                .await
                .unwrap();
        }
    }
    pub(crate) fn key(&self, name: &str, partition: i64) -> KeyMaterial {
        if name == "products" {
            return KeyMaterial::new(self.scope(name, 0), 1, [8; 32]);
        }
        KeyMaterial::new(
            self.scope(name, partition),
            1,
            rocky_credentials::derive_table_key(
                &[7; 32],
                self.domain,
                self.schema
                    .group_address(self.domain, name, &[SqlValue::Integer(partition)])
                    .unwrap()
                    .commitment,
                self.scope(name, partition).table,
                1,
            )
            .unwrap(),
        )
    }
    pub(crate) fn write(&self, name: &str, partition: i64) -> RowWrite {
        let app = self.app();
        let table = app.tables().find(|table| table.sql_name == name).unwrap();
        // Partitioned tables continue one creation log per author and table.
        let sequence = u32::try_from(partition.max(1)).unwrap();
        let id = RowIdentity::new(1, sequence).unwrap().integer();
        let mut values = if name == "products" {
            vec![SqlValue::Text("Product".into())]
        } else {
            vec![SqlValue::Integer(partition)]
        };
        if !matches!(name, "customers" | "products") {
            values.push(SqlValue::Integer(id));
        }
        if name != "products" {
            values.push(SqlValue::Text(name.into()));
        }
        if name == "order_lines" {
            values.push(SqlValue::Integer(RowIdentity::new(1, 1).unwrap().integer()));
        }
        values.extend([SqlValue::Integer(id), SqlValue::Integer(0)]);
        RowWrite::changed(RowChange {
            scope: self.scope(name, partition),
            row: app.row_id(self.domain.domain, table, &values).unwrap(),
            before: None,
            after: Some(app.fields(table, &values).unwrap()),
        })
    }
}
