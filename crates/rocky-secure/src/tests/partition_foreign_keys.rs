use super::*;
use rocky_core::RowApplication;
use rocky_core::{
    DomainId, PartitionSelection, PartitionSelector, PartitionTuple, PartitionValue, RowIdentity,
    SchemaDefinition, SqlValue, TypedRows,
};
use rocky_credentials::{AuthorRange, Authority, PartitionGrant};
use std::path::Path;

mod horizon;

struct Fixture {
    publisher: PrivateKey,
    schema: rocky_core::LogicalSchema,
    domain: rocky_core::Scope,
    manifest: SignedManifest,
    revocations: RevocationSet,
}
impl Fixture {
    fn new() -> Self {
        Self::build(false)
    }
    fn logged() -> Self {
        Self::build(true)
    }
    fn build(logged: bool) -> Self {
        let publisher = PrivateKey::new();
        let mut definition = serde_json::json!({"format":6,"publisher":publisher.public_key().to_string(),"application":"partition-fks","tables":[
            {"name":"lists","class":"mergeable","columns":[{"name":"title","type":"text"}],"primary_key":["id"],"partition_by":["id"],"partition_group":"list"},
            {"name":"categories","class":"mergeable","columns":[{"name":"list","type":"integer","not_null":true},{"name":"title","type":"text"}],"primary_key":["list","id"],"partition_by":["list"],"partition_group":"list"},
            {"name":"items","class":"mergeable","columns":[{"name":"list","type":"integer","not_null":true},{"name":"category","type":"integer"},{"name":"title","type":"text"}],"primary_key":["list","id"],"partition_by":["list"],"partition_group":"list","foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"},{"columns":["list","category"],"table":"categories","on_delete":"set_null"}]}
        ]});
        if !logged {
            definition["tables"][2]["indexes"] = serde_json::json!([{"name":"items_title","columns":[{"name":"list","descending":false},{"name":"title","descending":false}],"unique":true,"on_conflict":"latest_wins"}]);
        }
        if !logged {
            definition["tables"].as_array_mut().unwrap().push(serde_json::json!({
                "name":"attachments","class":"mergeable",
                "columns":[{"name":"list","type":"integer","not_null":true},{"name":"item","type":"integer","not_null":true},{"name":"title","type":"text"}],
                "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
                "foreign_keys":[{"columns":["list","item"],"table":"items","on_delete":"cascade"}]
            }));
        }
        if logged {
            definition["tables"] = serde_json::json!([
                {"name":"products","class":"mergeable","columns":[{"name":"title","type":"text"}],"primary_key":["id"],"existence_log":true},
                {"name":"items","class":"mergeable","columns":[{"name":"list","type":"integer","not_null":true},{"name":"category","type":"integer"},{"name":"title","type":"text"}],"primary_key":["list","id"],"partition_by":["list"],"foreign_keys":[{"columns":["category"],"table":"products","on_delete":"cascade"}]},
                {"name":"categories","class":"mergeable","columns":[{"name":"list","type":"integer","not_null":true},{"name":"title","type":"text"}],"primary_key":["list","id"],"partition_by":["list"],"indexes":[{"name":"categories_title","columns":[{"name":"list","descending":false},{"name":"title","descending":false}],"unique":true,"on_conflict":"latest_wins"}]}
            ]);
        }
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
    fn app(&self) -> TypedRows {
        TypedRows::new(self.schema.clone(), vec![self.domain.domain]).unwrap()
    }
    fn tuple(&self, name: &str, partition: i64) -> PartitionTuple {
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
    fn scope(&self, name: &str, partition: i64) -> ResourceScope {
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
    fn grants(&self, partitions: Option<&[i64]>, rights: &[Right]) -> Vec<PartitionGrant> {
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
    async fn open(
        &self,
        path: &Path,
        node: PrivateKey,
        grants: Vec<PartitionGrant>,
        signer: Option<(PrivateKey, u32)>,
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
        let recorded = if path.exists() {
            let (owner, conn) = crate::database::DatabaseOwner::open(path).await.unwrap();
            let mut rows = conn
                .query(
                    "SELECT number FROM secure_node_authors WHERE node=?",
                    [node.public_key().as_bytes().to_vec()],
                )
                .await
                .unwrap();
            let number = rows
                .next()
                .await
                .unwrap()
                .map(|row| row.get::<i64>(0).unwrap() as u32);
            drop(rows);
            drop(conn);
            drop(owner);
            number
        } else {
            None
        };
        let credentials = prefix
            .clone()
            .complete(
                &user,
                node.public_key(),
                recorded.unwrap_or_else(|| signer.as_ref().map_or(999, |(_, number)| *number)),
            )
            .unwrap();
        let replica = EncryptedReplica::open(
            path,
            self.app(),
            SecureConfig {
                publisher: self.publisher.public_key(),
                manifest: self.manifest.clone(),
                revocations: self.revocations.clone(),
                node: node.clone(),
                credentials,
                keys: vec![],
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
        replica
    }

    async fn keys(&self, replica: &EncryptedReplica<TypedRows>, partition: i64, tables: &[&str]) {
        for name in tables {
            replica
                .register_partition(self.scope(name, partition), self.tuple(name, partition))
                .await
                .unwrap();
            replica
                .install_key(KeyMaterial::new(
                    self.scope(name, partition),
                    1,
                    rocky_core::commitment(
                        "partition-fk-key",
                        &self.scope(name, partition).encryption,
                    ),
                ))
                .await
                .unwrap();
        }
    }
    fn write(&self, name: &str, partition: i64, sequence: u32, category: Option<i64>) -> RowWrite {
        let app = self.app();
        let table = app.tables().find(|table| table.sql_name == name).unwrap();
        let id = RowIdentity::new(1, sequence).unwrap().integer();
        let values = match name {
            "lists" | "products" => vec![
                SqlValue::Text("List".into()),
                SqlValue::Integer(partition),
                SqlValue::Integer(0),
            ],
            "categories" => vec![
                SqlValue::Integer(partition),
                SqlValue::Text("Category".into()),
                SqlValue::Integer(id),
                SqlValue::Integer(0),
            ],
            _ => vec![
                SqlValue::Integer(partition),
                category.map_or(SqlValue::Null, SqlValue::Integer),
                SqlValue::Text("Item".into()),
                SqlValue::Integer(id),
                SqlValue::Integer(0),
            ],
        };
        RowWrite::changed(RowChange {
            scope: self.scope(name, partition),
            row: app.row_id(self.domain.domain, table, &values).unwrap(),
            before: None,
            after: Some(app.fields(table, &values).unwrap()),
        })
    }
}
async fn visible(replica: &EncryptedReplica<TypedRows>, table: &str) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock().await;
    crate::storage::collect_rows(
        inner
            .conn
            .query(
                format!("SELECT * FROM {table} WHERE row_kind=0 ORDER BY id"),
                (),
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn group_references_hold_cascade_and_null_only_category_per_partition() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let a_id = RowIdentity::new(1, 1).unwrap().integer();
    let b_id = RowIdentity::new(1, 2).unwrap().integer();
    let category = RowIdentity::new(1, 1).unwrap().integer();
    let writer = f
        .open(
            &root.path().join("writer"),
            PrivateKey::new(),
            f.grants(
                None,
                &[Right::Read, Right::Insert, Right::Update, Right::Delete],
            ),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    let reader = f
        .open(
            &root.path().join("reader"),
            PrivateKey::new(),
            f.grants(Some(&[a_id]), &[Right::Read]),
            None,
        )
        .await;
    let b = f
        .open(
            &root.path().join("b"),
            PrivateKey::new(),
            f.grants(Some(&[b_id]), &[Right::Read]),
            None,
        )
        .await;
    for partition in [a_id, b_id] {
        f.keys(&writer, partition, &["lists", "categories", "items"])
            .await;
    }
    f.keys(&reader, a_id, &["items"]).await;
    f.keys(&b, b_id, &["lists", "categories", "items"]).await;
    let list = f.write("lists", a_id, 1, None);
    let cat = f.write("categories", a_id, 1, None);
    let item = f.write("items", a_id, 1, Some(category));
    let mut lists = writer
        .author_bundle([1; 32], vec![list.clone()], vec![])
        .await
        .unwrap();
    lists.extend(
        writer
            .author_bundle([6; 32], vec![f.write("lists", b_id, 2, None)], vec![])
            .await
            .unwrap(),
    );
    let cats = writer
        .author_bundle([2; 32], vec![cat.clone()], vec![])
        .await
        .unwrap();
    let items = writer
        .author_bundle([3; 32], vec![item, f.write("items", b_id, 2, None)], vec![])
        .await
        .unwrap();
    for frame in &items {
        if frame.operation().unwrap().header.extensions.scope == f.scope("items", a_id) {
            reader.receive(frame.clone()).await.unwrap();
        } else {
            b.receive(frame.clone()).await.unwrap();
            assert!(reader.receive(frame.clone()).await.is_err());
        }
    }
    assert!(visible(&reader, "items").await.is_empty());
    assert!(
        reader
            .horizon_statement(f.scope("items", a_id), &[])
            .await
            .is_err()
    );
    f.keys(&reader, a_id, &["lists", "categories"]).await;
    for frame in &lists {
        if frame.operation().unwrap().header.extensions.scope == f.scope("lists", a_id) {
            reader.receive(frame.clone()).await.unwrap();
        } else {
            b.receive(frame.clone()).await.unwrap();
        }
    }
    for frame in cats {
        reader.receive(frame).await.unwrap();
    }
    assert_eq!(visible(&reader, "items").await.len(), 1);
    assert_eq!(visible(&b, "items").await.len(), 1);
    let cut = reader
        .horizon_statement(f.scope("items", a_id), &[])
        .await
        .unwrap();
    let scopes: BTreeSet<_> = cut.related.iter().map(|cut| cut.scope).collect();
    assert_eq!(
        scopes,
        BTreeSet::from([f.scope("lists", a_id), f.scope("categories", a_id)])
    );
    reader
        .rebuild_projection(f.scope("items", a_id))
        .await
        .unwrap();
    assert_eq!(visible(&reader, "items").await.len(), 1);
    let deleted = RowWrite::changed(RowChange {
        scope: cat.change.scope,
        row: cat.change.row,
        before: cat.change.after.clone(),
        after: None,
    });
    for frame in writer
        .author_bundle([4; 32], vec![deleted], vec![])
        .await
        .unwrap()
    {
        reader.receive(frame).await.unwrap();
    }
    let rows = visible(&reader, "items").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], SqlValue::Integer(a_id));
    assert_eq!(rows[0][1], SqlValue::Null);
    let deleted = RowWrite::changed(RowChange {
        scope: list.change.scope,
        row: list.change.row,
        before: list.change.after.clone(),
        after: None,
    });
    for frame in writer
        .author_bundle([5; 32], vec![deleted], vec![])
        .await
        .unwrap()
    {
        assert!(b.receive(frame.clone()).await.is_err());
        reader.receive(frame).await.unwrap();
    }
    assert!(visible(&reader, "items").await.is_empty());
    assert_eq!(visible(&b, "items").await.len(), 1);
}

async fn sync_existence(
    from: &EncryptedReplica<TypedRows>,
    to: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
) {
    let (node, credentials) = {
        let inner = to.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    let page = from
        .existence_page(
            node,
            &credentials,
            scope,
            &to.existence_frontier(scope).await.unwrap(),
            None,
            crate::MAX_EXISTENCE_PAGE,
        )
        .await
        .unwrap();
    to.receive_existence_all(page.records).await.unwrap();
}
#[tokio::test]
async fn partitioned_children_follow_keyless_parent_liveness_and_restore() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::logged();
    let writer = f
        .open(
            &root.path().join("writer"),
            PrivateKey::new(),
            f.grants(
                None,
                &[Right::Read, Right::Insert, Right::Update, Right::Delete],
            ),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    let reader = f
        .open(
            &root.path().join("reader"),
            PrivateKey::new(),
            f.grants(Some(&[10]), &[Right::Read]),
            None,
        )
        .await;
    let other = f
        .open(
            &root.path().join("other"),
            PrivateKey::new(),
            f.grants(Some(&[10, 20]), &[Right::Read]),
            None,
        )
        .await;
    writer
        .install_key(KeyMaterial::new(f.scope("products", 0), 1, [7; 32]))
        .await
        .unwrap();
    f.keys(&writer, 10, &["items"]).await;
    f.keys(&writer, 20, &["items"]).await;
    f.keys(&reader, 10, &["items"]).await;
    f.keys(&other, 10, &["items"]).await;
    f.keys(&other, 20, &["items"]).await;
    let product_id = RowIdentity::new(1, 1).unwrap().integer();
    let product = f.write("products", product_id, 1, None);
    writer
        .author_bundle([1; 32], vec![product.clone()], vec![])
        .await
        .unwrap();
    let mut item = f.write("items", 10, 1, Some(product_id));
    let frames = writer
        .author_bundle(
            [2; 32],
            vec![item.clone(), f.write("items", 20, 2, Some(product_id))],
            vec![],
        )
        .await
        .unwrap();
    for frame in frames {
        other.receive(frame.clone()).await.unwrap();
        if frame.operation().unwrap().header.extensions.scope == f.scope("items", 10) {
            reader.receive(frame).await.unwrap();
        }
    }
    assert!(visible(&reader, "items").await.is_empty());
    for device in [&reader, &other] {
        sync_existence(&writer, device, f.scope("products", 0)).await;
    }
    assert_eq!(visible(&reader, "items").await.len(), 1);
    assert_eq!(visible(&other, "items").await.len(), 2);
    assert_eq!(visible(&reader, "products").await.len(), 0);
    {
        let mut inner = writer.inner.lock().await;
        transaction!(inner, |inner| inner
            .clear_typed(f.scope("products", 0))
            .await)
        .unwrap();
    }
    assert!(visible(&writer, "products").await.is_empty());
    assert_eq!(visible(&writer, "items").await.len(), 2);
    writer
        .rebuild_projection(f.scope("products", 0))
        .await
        .unwrap();
    assert_eq!(visible(&writer, "products").await.len(), 1);
    assert_eq!(visible(&writer, "items").await.len(), 2);

    let delete = RowWrite::changed(RowChange {
        scope: product.change.scope,
        row: product.change.row,
        before: product.change.after.clone(),
        after: None,
    });
    writer
        .author_bundle([3; 32], vec![delete], vec![])
        .await
        .unwrap();
    for device in [&reader, &other] {
        sync_existence(&writer, device, f.scope("products", 0)).await;
        assert!(visible(device, "items").await.is_empty());
    }
    let second_id = RowIdentity::new(1, 2).unwrap().integer();
    writer
        .author_bundle(
            [4; 32],
            vec![f.write("products", second_id, 2, None)],
            vec![],
        )
        .await
        .unwrap();
    let app = f.app();
    let table = app
        .tables()
        .find(|table| table.sql_name == "items")
        .unwrap();
    let mut values = app
        .values(table, item.change.after.as_ref().unwrap())
        .unwrap();
    values[1] = SqlValue::Integer(second_id);
    let after = app.fields(table, &values).unwrap();
    item.change.before = item.change.after.clone();
    item.change.after = Some(after);
    item = RowWrite::changed(item.change);
    let frames = writer
        .author_bundle([5; 32], vec![item], vec![])
        .await
        .unwrap();
    for frame in frames {
        reader.receive(frame.clone()).await.unwrap();
        other.receive(frame).await.unwrap();
    }
    for device in [&reader, &other] {
        sync_existence(&writer, device, f.scope("products", 0)).await;
    }
    assert_eq!(
        visible(&reader, "items").await,
        visible(&other, "items").await
    );
    assert_eq!(visible(&reader, "items").await.len(), 1);
    assert!(reader.key_epochs(f.scope("products", 0)).await.is_empty());
}

fn refusal(schema: &rocky_core::LogicalSchema) -> rocky_core::Incompatibility {
    match schema.installable() {
        Err(rocky_core::SchemaError::Incompatible { reason, .. }) => reason,
        other => panic!("expected refusal: {other:?}"),
    }
}
fn rewrite_reference(
    schema: &mut rocky_core::LogicalSchema,
    change: impl FnOnce(&mut rocky_core::LogicalForeignKey),
) {
    let table = schema
        .tables
        .iter_mut()
        .find(|table| table.sql_name == "items")
        .unwrap();
    let before = rocky_core::foreign_key_clause(&table.foreign_keys[0]);
    change(&mut table.foreign_keys[0]);
    let after = rocky_core::foreign_key_clause(&table.foreign_keys[0]);
    table.create_sql = table.create_sql.replace(&before, &after);
}
#[test]
fn foreign_keys_refuse_different_partition_groups() {
    let mut schema = Fixture::new().schema;
    schema
        .tables
        .iter_mut()
        .find(|table| table.sql_name == "lists")
        .unwrap()
        .partition_group = Some("other".into());
    assert_eq!(
        refusal(&schema),
        rocky_core::Incompatibility::CrossScopeReference(
            "partitioned references require the same explicit partition group".into()
        )
    );
}
#[test]
fn foreign_keys_refuse_unlogged_parent_outside_group() {
    let mut schema = Fixture::logged().schema;
    schema
        .tables
        .iter_mut()
        .find(|table| table.sql_name == "products")
        .unwrap()
        .existence_log = false;
    assert_eq!(
        refusal(&schema),
        rocky_core::Incompatibility::CrossScopeReference(
            "a partitioned child requires an unpartitioned logged parent outside its group".into()
        )
    );
}
#[test]
fn partitioned_foreign_keys_refuse_restrict() {
    let mut schema = Fixture::new().schema;
    rewrite_reference(&mut schema, |key| key.on_delete = "RESTRICT".into());
    assert!(matches!(
        refusal(&schema),
        rocky_core::Incompatibility::Reference(_)
    ));
}
#[test]
fn partitioned_foreign_keys_refuse_natural_keys() {
    let mut schema = Fixture::new().schema;
    rewrite_reference(&mut schema, |key| key.references = vec!["title".into()]);
    assert!(matches!(
        refusal(&schema),
        rocky_core::Incompatibility::Reference(_)
    ));
}
#[test]
fn partitioned_foreign_keys_refuse_nulling_the_whole_partition() {
    let mut schema = Fixture::new().schema;
    rewrite_reference(&mut schema, |key| {
        key.on_delete = rocky_core::DELETE_SET_NULL.into()
    });
    assert_eq!(
        refusal(&schema),
        rocky_core::Incompatibility::Reference(
            "SET NULL requires a non-partition reference column".into()
        )
    );
}

#[test]
fn in_group_identity_accepts_text_partition_slots_and_refuses_wrong_slot_mapping() {
    let definition = serde_json::json!({"format":6,"publisher":"1111111111111111111111111111111111111111111111111111111111111111","application":"text-partition-fks","tables":[
        {"name":"parents","class":"mergeable","columns":[{"name":"customer","type":"text","not_null":true}],"primary_key":["customer","id"],"partition_by":["customer"],"partition_group":"customer"},
        {"name":"items","class":"mergeable","columns":[{"name":"account","type":"text","not_null":true},{"name":"parent","type":"integer"}],"primary_key":["account","id"],"partition_by":["account"],"partition_group":"customer","foreign_keys":[{"columns":["account","parent"],"table":"parents","on_delete":"set_null"}]}
    ]});
    let mut schema = SchemaDefinition::parse(&serde_json::to_vec(&definition).unwrap())
        .unwrap()
        .build()
        .unwrap();
    schema.supported_on(rocky_core::Profile::Secure).unwrap();
    rewrite_reference(&mut schema, |key| key.columns.reverse());
    assert!(matches!(
        refusal(&schema),
        rocky_core::Incompatibility::Reference(_)
    ));
}

#[path = "row_moves.rs"]
mod row_moves;
