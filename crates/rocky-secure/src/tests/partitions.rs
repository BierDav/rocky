use super::*;
use rocky_core::{
    DomainId, PartitionSelection, PartitionSelector, PartitionTuple, PartitionValue, RowIdentity,
    SchemaDefinition, SqlValue, TypedRows,
};
use rocky_credentials::{AuthorRange, Authority, PartitionGrant};
use std::path::Path;

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
    fn constrained() -> Self {
        Self::build(true)
    }
    fn build(constrained: bool) -> Self {
        Self::build_with(constrained, PrivateKey::new())
    }
    fn build_with(constrained: bool, publisher: PrivateKey) -> Self {
        let mut definition = serde_json::json!({"format":6,"publisher":publisher.public_key().as_bytes().iter().map(|byte|format!("{byte:02x}")).collect::<String>(),"application":"partition-test","tables":[{"name":"items","class":"mergeable","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"amount","type":"integer","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}]});
        if constrained {
            definition["tables"][0]["columns"].as_array_mut().unwrap().push(serde_json::json!({"name":"title","type":"text","not_null":true,"collation":"nocase"}));
            definition["tables"][0]["indexes"] = serde_json::json!([{"name":"items_title","columns":[{"name":"list","descending":false},{"name":"title","descending":false}],"unique":true,"on_conflict":"latest_wins"}]);
        }
        let mut schema = SchemaDefinition::parse(&serde_json::to_vec(&definition).unwrap())
            .unwrap()
            .build()
            .unwrap();
        schema.views.push(rocky_core::LogicalView {
            name: "partial_items".into(),
            create_sql: "CREATE VIEW partial_items AS SELECT * FROM items WHERE row_kind=0".into(),
        });
        schema.views.push(rocky_core::LogicalView {
            name: "nested_items".into(),
            create_sql: "CREATE VIEW nested_items AS SELECT * FROM partial_items".into(),
        });
        let manifest = schema.manifest(&[DomainId([1; 16])]);
        let domain = rocky_core::Scope {
            mesh: MeshId([1; 16]),
            app: schema.app_id(),
            domain: DomainId([1; 16]),
        };
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
    fn tuple(&self, name: &str) -> PartitionTuple {
        let table = self.schema.table("items").unwrap();
        PartitionTuple::new([(
            self.schema.field_id(table, table.column("list").unwrap()),
            PartitionValue::new(
                rocky_core::KeyColumn {
                    affinity: rocky_core::Affinity::Text,
                    collation: rocky_core::Collation::Binary,
                },
                SqlValue::Text(name.into()),
            )
            .unwrap(),
        )])
        .unwrap()
    }
    fn scope(&self, name: &str) -> ResourceScope {
        self.tuple(name)
            .scope(
                self.domain,
                self.schema.table_id(self.schema.table("items").unwrap()),
            )
            .unwrap()
    }
    fn grant(&self, names: Option<&[&str]>, rights: &[Right]) -> PartitionGrant {
        let table = self.schema.table("items").unwrap();
        let column = self.schema.field_id(table, table.column("list").unwrap());
        PartitionGrant {
            domain: self.domain,
            table: self.schema.table_id(table),
            selector: PartitionSelector(
                [(
                    column,
                    names.map_or(PartitionSelection::Any, |names| {
                        PartitionSelection::Only(
                            names
                                .iter()
                                .map(|name| self.tuple(name).values()[&column].clone())
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
    }
    async fn open(
        &self,
        path: &Path,
        node: PrivateKey,
        grant: PartitionGrant,
        signer: Option<(PrivateKey, u32)>,
    ) -> EncryptedReplica<TypedRows> {
        let user = PrivateKey::new();
        let time = now().unwrap();
        let root = Authority::owner(&self.publisher)
            .issue_partition_terms(
                user.public_key(),
                vec![],
                vec![grant.clone()],
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
                [("items".into(), [9; 32])].into(),
            )
            .await
            .unwrap();
        replica
    }
    async fn key(&self, replica: &EncryptedReplica<TypedRows>, name: &str, key: [u8; 32]) {
        replica
            .register_partition(self.scope(name), self.tuple(name))
            .await
            .unwrap();
        replica
            .install_key(KeyMaterial::new(self.scope(name), 1, key))
            .await
            .unwrap();
    }
    fn write(&self, name: &str, sequence: u32, amount: i64) -> RowWrite {
        let app = self.app();
        let table = app.tables().next().unwrap();
        let mut values = vec![
            SqlValue::Text(name.into()),
            SqlValue::Text(format!("key{sequence}")),
            SqlValue::Integer(amount),
            SqlValue::Integer(RowIdentity::new(1, sequence).unwrap().integer()),
            SqlValue::Integer(0),
        ];
        if self
            .schema
            .table("items")
            .unwrap()
            .column("title")
            .is_some()
        {
            values.insert(3, SqlValue::Text("Milk".into()));
        }
        RowWrite::changed(RowChange {
            scope: self.scope(name),
            row: app.row_id(self.domain.domain, table, &values).unwrap(),
            before: None,
            after: Some(app.fields(table, &values).unwrap()),
        })
    }
    fn titled(&self, name: &str, sequence: u32, author: u32, title: &str) -> RowWrite {
        let mut write = self.write(name, sequence, 10);
        let app = self.app();
        let table = app.tables().next().unwrap();
        let mut values = app
            .values(table, write.change.after.as_ref().unwrap())
            .unwrap();
        values[1] = SqlValue::Text(format!("key{sequence}-author{author}"));
        values[3] = SqlValue::Text(title.into());
        values[4] = SqlValue::Integer(RowIdentity::new(author, sequence).unwrap().integer());
        write.change.row = app.row_id(self.domain.domain, table, &values).unwrap();
        write.change.after = Some(app.fields(table, &values).unwrap());
        write
    }
}
mod admission;

fn all() -> Vec<Right> {
    vec![
        Right::Read,
        Right::Insert,
        Right::Update,
        Right::Delete,
        Right::Custody,
    ]
}

#[tokio::test]
async fn a_future_import_does_not_make_verified_empty_partition_history_missing() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let replica = f
        .open(
            &root.path().join("writer.sqlite"),
            PrivateKey::new(),
            f.grant(None, &all()),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    replica
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Full,
            full_nodes: Default::default(),
            custody: 0,
        })
        .await
        .unwrap();
    f.key(&replica, "A", [1; 32]).await;
    let frame = replica
        .author_bundle([1; 32], vec![f.write("A", 1, 10)], vec![])
        .await
        .unwrap()
        .remove(0);
    seal_main(&replica, &f.publisher, f.domain).await;
    replica.finalize_path(f.domain).await.unwrap();
    assert!(
        replica
            .compaction_status(f.domain)
            .await
            .unwrap()
            .finalized
            .is_some()
    );
    f.key(&replica, "B", [2; 32]).await;
    replica.set_importing([f.scope("B")].into()).await;
    assert!(
        replica
            .seal_rebootstraps(f.domain)
            .await
            .unwrap()
            .is_empty()
    );
    let session = replica
        .session([f.scope("A"), f.scope("B")].into())
        .await
        .unwrap();
    let sql = session.sql(Default::default()).await.unwrap();
    assert_eq!(
        sql.run("SELECT count(*) FROM items WHERE row_kind=0", vec![])
            .await
            .unwrap()
            .rows[0][0],
        SqlValue::Integer(1)
    );
    let missing = f
        .open(
            &root.path().join("missing.sqlite"),
            PrivateKey::new(),
            f.grant(None, &all()),
            None,
        )
        .await;
    missing
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Full,
            full_nodes: Default::default(),
            custody: 0,
        })
        .await
        .unwrap();
    f.key(&missing, "A", [1; 32]).await;
    missing.receive(frame).await.unwrap();
    exchange_graphs(&replica, &missing, f.domain).await;
    missing.finalize_path(f.domain).await.unwrap();
    assert!(
        missing
            .compaction_status(f.domain)
            .await
            .unwrap()
            .finalized
            .is_some()
    );
    f.key(&replica, "C", [3; 32]).await;
    replica
        .author_bundle([2; 32], vec![f.write("C", 2, 20)], vec![])
        .await
        .unwrap();
    seal_main(&replica, &f.publisher, f.domain).await;
    exchange_graphs(&replica, &missing, f.domain).await;
    f.key(&missing, "B", [2; 32]).await;
    missing.set_importing([f.scope("B")].into()).await;
    assert!(
        missing
            .seal_rebootstraps(f.domain)
            .await
            .unwrap()
            .iter()
            .any(|need| need.scope == f.scope("B")),
        "missing author history cannot establish an empty partition"
    );
}

#[tokio::test]
async fn unrelated_partition_recovery_does_not_block_constant_queries() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let replica = f
        .open(
            &root.path().join("waiting.sqlite"),
            PrivateKey::new(),
            f.grant(None, &all()),
            None,
        )
        .await;
    f.key(&replica, "A", [1; 32]).await;
    replica
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Full,
            full_nodes: Default::default(),
            custody: 0,
        })
        .await
        .unwrap();
    let session = replica.session([f.scope("A")].into()).await.unwrap();
    let sql = session.sql(Default::default()).await.unwrap();
    {
        let inner = replica.inner.lock().await;
        inner
            .conn
            .execute(
                "INSERT INTO secure_compaction_lost VALUES(?,?)",
                [crate::encode(&f.domain).unwrap(), vec![7; 32]],
            )
            .await
            .unwrap();
    }
    assert_eq!(
        sql.run("SELECT 42", vec![]).await.unwrap().rows[0][0],
        SqlValue::Integer(42)
    );
    for query in [
        "SELECT * FROM items WHERE row_kind=0",
        "SELECT * FROM nested_items",
    ] {
        assert!(sql.run(query, vec![]).await.is_err(), "{query}");
    }
}

#[tokio::test]
async fn partition_frames_keys_custody_sql_and_checkpoints_are_isolated() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let author = PrivateKey::new();
    let a = f
        .open(
            &root.path().join("writer.sqlite"),
            PrivateKey::new(),
            f.grant(None, &all()),
            Some((author.clone(), 1)),
        )
        .await;
    f.key(&a, "A", [1; 32]).await;
    f.key(&a, "B", [2; 32]).await;
    let reader = f
        .open(
            &root.path().join("reader.sqlite"),
            PrivateKey::new(),
            f.grant(Some(&["A"]), &[Right::Read]),
            None,
        )
        .await;
    f.key(&reader, "A", [1; 32]).await;
    let custody = f
        .open(
            &root.path().join("custody.sqlite"),
            PrivateKey::new(),
            f.grant(Some(&["A"]), &[Right::Custody]),
            None,
        )
        .await;

    let fa = a
        .author_bundle([1; 32], vec![f.write("A", 1, 10)], vec![])
        .await
        .unwrap()
        .remove(0);
    let fb = a
        .author_bundle([2; 32], vec![f.write("B", 2, 20)], vec![])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(fa.operation().unwrap().header.extensions.row_format, 12);
    assert_eq!(
        reader.receive(fa.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert!(reader.receive(fb.clone()).await.is_err());
    assert_eq!(
        custody.receive(fa.clone()).await.unwrap(),
        ReceiveStatus::Custody
    );
    assert!(custody.receive(fb.clone()).await.is_err());
    // A valid author signature cannot downgrade a partitioned table to a legacy frame.
    let legacy = {
        let mut operation = fa.operation().unwrap();
        operation.header.extensions.partition = None;
        operation.header.extensions.row_format = 2;
        operation.header.sign(&author);
        fa.with_header(operation.header.to_bytes())
    };
    assert!(legacy.operation().is_err());
    assert!(custody.receive(legacy).await.is_err());

    assert!(!reader.table_complete(f.scope("A").table).await.unwrap());
    assert!(a.table_complete(f.scope("A").table).await.unwrap());
    let sql = reader
        .session([f.scope("A")].into())
        .await
        .unwrap()
        .sql(crate::sql::SqlOptions::default())
        .await
        .unwrap();
    assert!(
        sql.run("SELECT SUM(amount) FROM items", vec![])
            .await
            .is_err()
    );
    assert_eq!(
        sql.run(
            "SELECT SUM(amount) FROM items WHERE list=?",
            vec![SqlValue::Text("A".into())]
        )
        .await
        .unwrap()
        .rows[0][0],
        SqlValue::Integer(10)
    );
    assert!(
        sql.run(
            "SELECT * FROM items WHERE list=?",
            vec![SqlValue::Text("B".into())]
        )
        .await
        .is_err()
    );
    assert_eq!(
        sql.run("SELECT SUM(amount) FROM items WHERE row_kind=0", vec![])
            .await
            .unwrap()
            .rows[0][0],
        SqlValue::Integer(10)
    );
    assert_eq!(
        sql.run("SELECT SUM(amount) FROM nested_items", vec![])
            .await
            .unwrap()
            .rows[0][0],
        SqlValue::Integer(10)
    );
    assert!(
        sql.run("SELECT * FROM items WHERE row_kind=1", vec![])
            .await
            .is_err()
    );
    assert!(
        sql.run("SELECT * FROM items WHERE list='A' OR amount=10", vec![])
            .await
            .is_err()
    );
    let full = a
        .session([f.scope("A"), f.scope("B")].into())
        .await
        .unwrap()
        .sql(crate::sql::SqlOptions::default())
        .await
        .unwrap();
    assert_eq!(
        full.run("SELECT SUM(amount) FROM items", vec![])
            .await
            .unwrap()
            .rows[0][0],
        SqlValue::Integer(30)
    );
    let (peer, credentials) = {
        let inner = reader.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    assert!(
        a.range_page(
            peer,
            &credentials,
            &crate::ScopeFrontier {
                scope: f.scope("B"),
                logs: vec![],
            },
            None,
            65536
        )
        .await
        .is_err()
    );
    let permitted = a
        .range_page(
            peer,
            &credentials,
            &crate::ScopeFrontier {
                scope: f.scope("A"),
                logs: vec![],
            },
            None,
            65536,
        )
        .await
        .unwrap();
    assert_eq!(permitted.frames.len(), 1);
    let cp = compacted_base(&a, &f.publisher, f.scope("A")).await;
    custody
        .verify_checkpoint_at_for_test(&cp, now().unwrap())
        .await
        .unwrap();
    let legacy_checkpoint = {
        let inner = a.inner.lock().await;
        let mut legacy = cp.clone();
        legacy.statement.format = 3;
        legacy.statement.partition = None;
        let signature = inner.config.node.sign(
            &signing_bytes(
                "rocky.encrypted-checkpoint.v3",
                &(
                    &legacy.statement,
                    inner.config.node.public_key(),
                    &inner.config.credentials,
                ),
            )
            .unwrap(),
        );
        let mut value = serde_json::to_value(&legacy).unwrap();
        value["signature"] = serde_json::to_value(signature).unwrap();
        legacy = serde_json::from_value(value).unwrap();
        legacy
    };
    assert!(
        custody
            .verify_checkpoint_at_for_test(&legacy_checkpoint, now().unwrap())
            .await
            .is_err()
    );

    let restored = f
        .open(
            &root.path().join("restored.sqlite"),
            PrivateKey::new(),
            f.grant(None, &[Right::Read]),
            None,
        )
        .await;
    f.key(&restored, "A", [1; 32]).await;
    f.key(&restored, "B", [2; 32]).await;
    restored.receive(fb).await.unwrap();
    let (peer, credentials) = {
        let inner = restored.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    link_up(&a, &restored, f.scope("A").domain).await;
    restored.begin_rebootstrap(cp.clone()).await.unwrap();
    let mut after = None;
    loop {
        let page = a
            .checkpoint_page(
                peer,
                &credentials,
                f.scope("A"),
                cp.id().unwrap(),
                after,
                65536,
            )
            .await
            .unwrap();
        after = page.next;
        restored.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
    restored.finish_rebootstrap(f.scope("A")).await.unwrap();
    let restored_sql = restored
        .session([f.scope("A"), f.scope("B")].into())
        .await
        .unwrap()
        .sql(Default::default())
        .await
        .unwrap();
    assert_eq!(
        restored_sql
            .run("SELECT SUM(amount) FROM items WHERE row_kind=0", vec![])
            .await
            .unwrap()
            .rows[0][0],
        SqlValue::Integer(30)
    );
    let (full_node, full_credentials) = {
        let inner = a.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    restored
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
            full_nodes: [rocky_credentials::PeerId::of(full_node)].into(),
            custody: 1,
        })
        .await
        .unwrap();
    restored.subscribe(f.scope("A")).await.unwrap();
    restored.subscribe(f.scope("B")).await.unwrap();
    restored
        .acknowledge_frontier(
            full_node,
            &full_credentials,
            &a.frontier(f.scope("A")).await.unwrap(),
        )
        .await
        .unwrap();
    restored.unsubscribe(f.scope("A")).await.unwrap();
    restored.evict(f.scope("A")).await.unwrap();
    assert!(!restored.table_complete(f.scope("A").table).await.unwrap());
    let remaining = restored
        .session([f.scope("B")].into())
        .await
        .unwrap()
        .sql(Default::default())
        .await
        .unwrap();
    assert_eq!(
        remaining
            .run("SELECT SUM(amount) FROM items WHERE row_kind=0", vec![])
            .await
            .unwrap()
            .rows[0][0],
        SqlValue::Integer(20)
    );
    assert!(
        remaining
            .run("SELECT * FROM items WHERE list='A'", vec![])
            .await
            .is_err()
    );
    assert_eq!(cp.statement.format, 17);
    assert_eq!(cp.statement.partition, Some(f.tuple("A")));
    // An authenticated row sealed with B's key still cannot claim B's partition for an A row.
    let mut wrong = f.write("A", 2, 5);
    wrong.change.scope = f.scope("B");
    assert!(a.author_bundle([3; 32], vec![wrong], vec![]).await.is_err());
    reader.rebuild_projection(f.scope("A")).await.unwrap();
    assert_eq!(
        sql.run("SELECT count(*) FROM items WHERE row_kind=0", vec![])
            .await
            .unwrap()
            .rows[0][0],
        SqlValue::Integer(1)
    );
}

#[tokio::test]
#[ignore = "explicit phase-11 measurement; run with --ignored --nocapture"]
async fn measure_200_partitions() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let node = PrivateKey::new();
    let signer = PrivateKey::new();
    let started = std::time::Instant::now();
    let writer = f
        .open(
            &root.path().join("writer.sqlite"),
            node.clone(),
            f.grant(None, &all()),
            Some((signer.clone(), 1)),
        )
        .await;
    let empty_open = started.elapsed();
    let empty_pages = {
        let inner = writer.inner.lock().await;
        crate::storage::rows(&inner.conn, "PRAGMA page_count")
            .await
            .unwrap()[0][0]
            .clone()
    };
    let mut frames = Vec::new();
    let started = std::time::Instant::now();
    for index in 1..=200u8 {
        let name = format!("p{index:03}");
        f.key(&writer, &name, [index; 32]).await;
    }
    let partition_pages = {
        let inner = writer.inner.lock().await;
        crate::storage::rows(&inner.conn, "PRAGMA page_count")
            .await
            .unwrap()[0][0]
            .clone()
    };
    for index in 1..=200u8 {
        let name = format!("p{index:03}");
        frames.push(
            writer
                .author_bundle(
                    [index; 32],
                    vec![f.write(&name, u32::from(index), i64::from(index))],
                    vec![],
                )
                .await
                .unwrap()
                .remove(0),
        );
    }
    let initialize = started.elapsed();
    writer.flush().await.unwrap();
    let reader = f
        .open(
            &root.path().join("reader.sqlite"),
            PrivateKey::new(),
            f.grant(None, &[Right::Read]),
            None,
        )
        .await;
    for index in 1..=200u8 {
        f.key(&reader, &format!("p{index:03}"), [index; 32]).await;
    }
    let bytes = frames
        .iter()
        .map(|frame| frame.to_bytes().unwrap().len())
        .sum::<usize>();
    let started = std::time::Instant::now();
    let (peer, credentials) = {
        let inner = reader.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    for index in 1..=200u8 {
        let scope = f.scope(&format!("p{index:03}"));
        let frontier = reader.frontier(scope).await.unwrap();
        let page = writer
            .range_page(peer, &credentials, &frontier, None, 65536)
            .await
            .unwrap();
        assert_eq!(page.frames.len(), 1);
        let statuses = reader.receive_all(page.frames).await;
        assert!(
            statuses
                .into_iter()
                .all(|status| matches!(status, Ok(ReceiveStatus::Applied)))
        );
    }
    let sync = started.elapsed();
    let started = std::time::Instant::now();
    seal_and_compact(&writer, &f.publisher, f.scope("p001").domain).await;
    for index in 1..=200u8 {
        writer
            .checkpoint(f.scope(&format!("p{index:03}")))
            .await
            .unwrap();
    }
    let checkpoint = started.elapsed();
    let logical_bytes = {
        let inner = writer.inner.lock().await;
        crate::storage::rows(&inner.conn, "PRAGMA page_count")
            .await
            .unwrap()[0][0]
            .clone()
    };
    drop(writer);
    let started = std::time::Instant::now();
    let reopened = f
        .open(
            &root.path().join("writer.sqlite"),
            node,
            f.grant(None, &all()),
            Some((signer, 1)),
        )
        .await;
    for index in 1..=200u8 {
        f.key(&reopened, &format!("p{index:03}"), [index; 32]).await;
    }
    let reopen = started.elapsed();
    let sql = reader
        .session(
            (1..=200u8)
                .map(|index| f.scope(&format!("p{index:03}")))
                .collect(),
        )
        .await
        .unwrap()
        .sql(crate::sql::SqlOptions::default())
        .await
        .unwrap();
    assert_eq!(
        sql.run("SELECT count(*) FROM items", vec![])
            .await
            .unwrap()
            .rows[0][0],
        SqlValue::Integer(200)
    );
    let file_bytes = std::fs::metadata(root.path().join("writer.sqlite"))
        .unwrap()
        .len();
    println!(
        "P11 partitions=200 empty_open_ms={} initialize_ms={} sync_ms={} checkpoints_ms={} reopen_ms={} ciphertext_bytes={} empty_pages={:?} partition_metadata_pages={:?} file_bytes={} page_count={:?}",
        empty_open.as_millis(),
        initialize.as_millis(),
        sync.as_millis(),
        checkpoint.as_millis(),
        reopen.as_millis(),
        bytes,
        empty_pages,
        partition_pages,
        file_bytes,
        logical_bytes
    );
}

async fn constraint_state(
    replica: &EncryptedReplica<TypedRows>,
    partition: &str,
) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock().await;
    crate::storage::collect_rows(inner.conn.query(
        "SELECT c.id,c.rocky_live,c.rocky_stamps,NOT EXISTS(SELECT 1 FROM items i WHERE i.list=c.list AND i.id=c.id) FROM secure_candidates_items c WHERE c.list=? ORDER BY c.id",
        [partition.to_string()],
    ).await.unwrap()).await.unwrap()
}

#[tokio::test]
async fn partitioned_latest_wins_isolated_identity_rebuild_eviction_and_reopen() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::constrained();
    let path = root.path().join("a.sqlite");
    let node = PrivateKey::new();
    let a = f
        .open(
            &path,
            node.clone(),
            f.grant(None, &all()),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    let b = f
        .open(
            &root.path().join("b.sqlite"),
            PrivateKey::new(),
            f.grant(None, &all()),
            Some((PrivateKey::new(), 2)),
        )
        .await;
    for replica in [&a, &b] {
        f.key(replica, "A", [1; 32]).await;
        f.key(replica, "B", [2; 32]).await;
    }
    let early = a
        .author_bundle(
            [1; 32],
            vec![f.titled("A", 1, 1, "Milk"), f.titled("B", 2, 1, "Milk")],
            vec![],
        )
        .await
        .unwrap();
    let clock = b
        .author_bundle(
            [2; 32],
            vec![f.titled("A", 1, 2, "other"), f.titled("B", 2, 2, "other")],
            vec![],
        )
        .await
        .unwrap();
    let late = b
        .author_bundle(
            [3; 32],
            vec![f.titled("A", 3, 2, "MILK"), f.titled("B", 4, 2, "MILK")],
            vec![],
        )
        .await
        .unwrap();
    for frame in clock.iter().chain(late.iter()) {
        a.receive(frame.clone()).await.unwrap();
    }
    for frame in early.iter().rev() {
        b.receive(frame.clone()).await.unwrap();
    }
    assert_eq!(
        constraint_state(&a, "A").await,
        constraint_state(&b, "A").await
    );
    assert_eq!(
        constraint_state(&a, "B").await,
        constraint_state(&b, "B").await
    );
    for name in ["A", "B"] {
        let sql = a
            .session([f.scope(name)].into())
            .await
            .unwrap()
            .sql(Default::default())
            .await
            .unwrap();
        let visible = sql
            .run(
                "SELECT id FROM items WHERE list=? AND title='milk'",
                vec![SqlValue::Text(name.into())],
            )
            .await
            .unwrap()
            .rows;
        assert_eq!(
            visible,
            vec![vec![SqlValue::Integer(
                RowIdentity::new(2, if name == "A" { 3 } else { 4 })
                    .unwrap()
                    .integer()
            )]]
        );
        let state = constraint_state(&a, name).await;
        assert_eq!(
            state
                .iter()
                .filter(|row| row[3] == SqlValue::Integer(1))
                .count(),
            1,
            "{name}: {state:?}"
        );
    }
    let before_a = constraint_state(&a, "A").await;
    let before_b = constraint_state(&a, "B").await;
    {
        let mut inner = a.inner.lock().await;
        transaction!(inner, |inner| inner.clear_typed(f.scope("A")).await).unwrap();
    }
    assert!(constraint_state(&a, "A").await.is_empty());
    assert_eq!(constraint_state(&a, "B").await, before_b);
    a.rebuild_projection(f.scope("A")).await.unwrap();
    assert_eq!(constraint_state(&a, "A").await, before_a);
    assert_eq!(constraint_state(&a, "B").await, before_b);
    let partial = f
        .open(
            &root.path().join("partial.sqlite"),
            PrivateKey::new(),
            f.grant(Some(&["A"]), &[Right::Read]),
            None,
        )
        .await;
    f.key(&partial, "A", [1; 32]).await;
    for frame in early
        .iter()
        .chain(clock.iter())
        .chain(late.iter())
        .filter(|frame| frame.scope().unwrap() == f.scope("A"))
    {
        partial.receive(frame.clone()).await.unwrap();
    }
    assert_eq!(constraint_state(&partial, "A").await, before_a);
    assert!(constraint_state(&partial, "B").await.is_empty());
    for frame in early
        .iter()
        .filter(|frame| frame.scope().unwrap() == f.scope("B"))
    {
        assert!(partial.receive(frame.clone()).await.is_err());
    }
    partial.rebuild_projection(f.scope("A")).await.unwrap();
    assert_eq!(constraint_state(&partial, "A").await, before_a);
    let (full_node, full_credentials) = {
        let inner = b.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    a.set_role(crate::RoleConfig {
        role: crate::ReplicaRole::Partial,
        full_nodes: [rocky_credentials::PeerId::of(full_node)].into(),
        custody: 1,
    })
    .await
    .unwrap();
    a.subscribe(f.scope("A")).await.unwrap();
    a.subscribe(f.scope("B")).await.unwrap();
    a.acknowledge_frontier(
        full_node,
        &full_credentials,
        &b.frontier(f.scope("A")).await.unwrap(),
    )
    .await
    .unwrap();
    a.unsubscribe(f.scope("A")).await.unwrap();
    a.evict(f.scope("A")).await.unwrap();
    assert!(constraint_state(&a, "A").await.is_empty());
    assert_eq!(constraint_state(&a, "B").await, before_b);
    a.rebuild_projection(f.scope("A")).await.unwrap();
    assert_eq!(constraint_state(&a, "B").await, before_b);
    drop(a);
    let reopened = f.open(&path, node, f.grant(None, &all()), None).await;
    assert_eq!(constraint_state(&reopened, "B").await, before_b);
    assert!(constraint_state(&reopened, "A").await.is_empty());
}

#[tokio::test]
async fn partitioned_equal_stamps_break_ties_by_partition_and_id() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::constrained();
    let a = f
        .open(
            &root.path().join("a.sqlite"),
            PrivateKey::new(),
            f.grant(None, &all()),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    for name in ["A", "B"] {
        f.key(&a, name, [if name == "A" { 1 } else { 2 }; 32]).await;
    }
    a.author_bundle(
        [1; 32],
        vec![
            f.titled("A", 1, 1, "milk"),
            f.titled("A", 2, 1, "MILK"),
            f.titled("B", 3, 1, "milk"),
            f.titled("B", 4, 1, "MILK"),
        ],
        vec![],
    )
    .await
    .unwrap();
    for name in ["A", "B"] {
        let state = constraint_state(&a, name).await;
        assert_eq!(state.len(), 2);
        assert_eq!(state[0][3], SqlValue::Integer(1));
        assert_eq!(state[1][3], SqlValue::Integer(0));
        a.rebuild_projection(f.scope(name)).await.unwrap();
        assert_eq!(constraint_state(&a, name).await, state);
    }
}

#[tokio::test]
async fn partitioned_open_refuses_dropped_or_changed_constraint_indexes() {
    for index in [
        "items_title",
        "items_identity",
        "secure_candidates_items_id",
        "secure_candidates_items_u0",
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a.sqlite");
        let f = Fixture::constrained();
        let a = f
            .open(&path, PrivateKey::new(), f.grant(None, &all()), None)
            .await;
        let config = {
            let inner = a.inner.lock().await;
            SecureConfig {
                publisher: inner.config.publisher,
                manifest: inner.config.manifest.clone(),
                revocations: inner.config.revocations.clone(),
                node: inner.config.node.clone(),
                credentials: inner.config.credentials.clone(),
                keys: vec![],
            }
        };
        drop(a);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(&format!("DROP INDEX \"{index}\""), [])
            .unwrap();
        drop(conn);
        let result = EncryptedReplica::open(&path, f.app(), config).await;
        assert!(
            matches!(result,Err(Error::Incompatible(message)) if message.contains("constraint index")),
            "{index}"
        );
    }
}

#[tokio::test]
async fn partitioned_sql_refuses_local_duplicates_and_keeps_other_partition_atomic() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::constrained();
    let a = f
        .open(
            &root.path().join("a.sqlite"),
            PrivateKey::new(),
            f.grant(None, &all()),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    for name in ["A", "B"] {
        f.key(&a, name, [if name == "A" { 1 } else { 2 }; 32]).await;
    }
    let sql = a
        .session([f.scope("A"), f.scope("B")].into())
        .await
        .unwrap()
        .sql(Default::default())
        .await
        .unwrap();
    sql.run("BEGIN", vec![]).await.unwrap();
    for name in ["A", "B"] {
        let id = sql
            .next_id_partition("items", &[SqlValue::Text(name.into())])
            .await
            .unwrap()
            .integer();
        sql.run(
            "INSERT INTO items(list,key,amount,title,id,row_kind) VALUES(?, 'one',10,'milk',?,0)",
            vec![SqlValue::Text(name.into()), SqlValue::Integer(id)],
        )
        .await
        .unwrap();
    }
    sql.commit([1; 32]).await.unwrap();
    let before_b = constraint_state(&a, "B").await;
    sql.run("BEGIN", vec![]).await.unwrap();
    let id = sql
        .next_id_partition("items", &[SqlValue::Text("A".into())])
        .await
        .unwrap()
        .integer();
    assert!(matches!(
        sql.run(
            "INSERT INTO items(list,key,amount,title,id,row_kind) VALUES('A','two',10,'MILK',?,0)",
            vec![SqlValue::Integer(id)]
        )
        .await,
        Err(Error::Sql(crate::sql::SqlError::Statement {
            code: 2067,
            ..
        }))
    ));
    sql.run("ROLLBACK", vec![]).await.unwrap();
    assert_eq!(constraint_state(&a, "B").await, before_b);
    sql.run("BEGIN", vec![]).await.unwrap();
    sql.run("UPDATE items SET title='bread' WHERE list='A'", vec![])
        .await
        .unwrap();
    sql.commit([2; 32]).await.unwrap();
    assert_eq!(constraint_state(&a, "B").await, before_b);
}

#[tokio::test]
async fn an_unproved_partition_requires_custody_without_disclosing_its_header_or_tuple() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let writer = f
        .open(
            &root.path().join("writer.sqlite"),
            PrivateKey::new(),
            f.grant(None, &all()),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    let reader_key = PrivateKey::new();
    let reader = f
        .open(
            &root.path().join("reader.sqlite"),
            reader_key.clone(),
            f.grant(Some(&["A"]), &[Right::Read]),
            None,
        )
        .await;
    f.key(&writer, "A", [1; 32]).await;
    f.key(&writer, "secret-partition", [2; 32]).await;
    f.key(&reader, "A", [1; 32]).await;
    let visible = writer
        .author_bundle([1; 32], vec![f.write("A", 1, 10)], vec![])
        .await
        .unwrap()
        .remove(0);
    let hidden = writer
        .author_bundle([2; 32], vec![f.write("secret-partition", 2, 20)], vec![])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        reader.receive(visible).await.unwrap(),
        ReceiveStatus::Applied
    );
    seal_main(&writer, &f.publisher, f.domain).await;
    exchange_graphs(&writer, &reader, f.domain).await;
    let requests = reader.wanted_placements().await.unwrap();
    let [request] = requests.as_slice() else {
        panic!("{requests:?}")
    };
    let operation = hidden.operation().unwrap();
    assert_eq!(request.author, operation.header.extensions.author.unwrap());
    assert_eq!(request.from, operation.header.extensions.author_sequence);
    assert_eq!(request.to, request.from);
    let credentials = reader.inner.lock().await.config.credentials.clone();
    let page = writer
        .placement_page(
            reader_key.public_key(),
            &credentials,
            &[f.scope("A")].into(),
            *request,
        )
        .await
        .unwrap();
    // This variant carries neither signed headers nor credential chains or tuples.
    assert!(matches!(page, crate::PlacementPage::CustodyRequired));
    assert_eq!(reader.receive_placements(*request, page).await.unwrap(), 0);
    struct NoPeers;
    impl crate::EntryPeers for NoPeers {
        fn store<'a>(
            &'a self,
            _: PublicKey,
            _: &'a crate::CoordinatedEntry,
        ) -> crate::PeerFuture<'a, ()> {
            panic!("readiness must fail before entry storage")
        }
        fn committed(
            &self,
            _: rocky_core::TipLog,
            _: u64,
        ) -> crate::PeerFuture<'_, Vec<crate::CommittedEntry>> {
            panic!("readiness must fail before catch-up")
        }
    }
    let log = rocky_coordinator::coordinated::MemoryLog::new();
    log.configure(f.domain, 1).unwrap();
    let followed = rocky_core::TipLog {
        scope: f.domain,
        tip: reader.followed_tip().await,
    };
    assert!(matches!(
        reader
            .coordinated_slot(followed, &log.client(reader_key.public_key()), &NoPeers)
            .await,
        Err(crate::CoordinatedError::CustodyRequired)
    ));
    let inner = reader.inner.lock().await;
    assert!(
        crate::storage::rows(&inner.conn, "SELECT 1 FROM secure_placements")
            .await
            .unwrap()
            .is_empty()
    );
    let tuples = crate::storage::rows(&inner.conn, "SELECT tuple FROM secure_partitions")
        .await
        .unwrap();
    assert_eq!(tuples.len(), 1);
    assert_eq!(
        tuples[0],
        vec![SqlValue::Blob(encode(&f.tuple("A")).unwrap())]
    );
}
