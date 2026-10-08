//! Existence logs, puppets and virtual rows: foreign keys across reader
//! scopes on devices that hold the child only. See
//! `docs/runtime/existence-logs.md`.
use super::*;
use crate::network::NativeSession;
use crate::sql::{SqlError, SqlOptions};
use crate::storage::collect_rows;
use crate::{ExistenceEvent, ExistenceRecord, MAX_SECURE_BYTES, ReceiveStatus, SecureSql};
use rocky_core::{DomainId, LogicalSchema, Scope, SqlValue, TypedRows};

const DEFINITION: &str = r#"{
    "format": 3, "publisher": "PUBLISHER", "application": "erp",
    "tables": [
        {"name": "lists", "class": "mergeable",
         "columns": [{"name": "title", "type": "text", "not_null": true}],
         "primary_key": ["id"], "existence_log": true},
        {"name": "items", "class": "mergeable",
         "columns": [
            {"name": "list", "type": "integer", "not_null": true},
            {"name": "code", "type": "text", "not_null": true}],
         "primary_key": ["id"],
         "foreign_keys": [{"columns": ["list"], "table": "lists", "on_delete": "cascade"}]},
        {"name": "regions", "class": "mergeable",
         "columns": [{"name": "name", "type": "text", "not_null": true}],
         "primary_key": ["id"]},
        {"name": "customers", "class": "mergeable",
         "columns": [
            {"name": "name", "type": "text", "not_null": true},
            {"name": "region", "type": "integer", "not_null": true},
            {"name": "score", "type": "real", "not_null": true},
            {"name": "photo", "type": "blob", "not_null": true},
            {"name": "note", "type": "text"}],
         "primary_key": ["id"], "existence_log": true,
         "indexes": [{"name": "customers_name", "columns": [{"name": "name", "descending": false}], "unique": true, "on_conflict": "latest_wins"}],
         "foreign_keys": [{"columns": ["region"], "table": "regions", "on_delete": "cascade"}]},
        {"name": "orders", "class": "mergeable",
         "columns": [
            {"name": "customer", "type": "integer", "not_null": true},
            {"name": "backup", "type": "integer"},
            {"name": "label", "type": "text", "not_null": true}],
         "primary_key": ["id"],
         "foreign_keys": [
            {"columns": ["customer"], "table": "customers", "on_delete": "cascade"},
            {"columns": ["backup"], "table": "customers", "on_delete": "set_null"}]}
    ],
    "views": [
        {"name": "partial_customers", "create_sql": "CREATE VIEW partial_customers AS SELECT * FROM customers WHERE row_kind = 0"},
        {"name": "partial_lists", "create_sql": "CREATE VIEW partial_lists AS SELECT * FROM lists WHERE row_kind = 0"}
    ]
}"#;
const TABLES: [&str; 5] = ["lists", "items", "regions", "customers", "orders"];
/// Reader scopes: customers and regions share one, the others have their own.
fn reader(table: &str) -> &'static str {
    match table {
        "lists" => "lists",
        "items" => "items",
        "orders" => "orders",
        _ => "customers",
    }
}

struct Erp {
    publisher: PrivateKey,
    schema: LogicalSchema,
    manifest: SignedManifest,
    policy: SignedPolicy,
    domain: Scope,
}
/// One device and the tables it holds.
struct Device {
    node: PrivateKey,
    tables: Vec<&'static str>,
    credentials: CredentialChain,
    replica: EncryptedReplica<TypedRows>,
}
impl Erp {
    fn new() -> Self {
        let publisher = PrivateKey::new();
        let schema = rocky_core::SchemaDefinition::parse(
            DEFINITION
                .replace("PUBLISHER", &publisher.public_key().to_string())
                .as_bytes(),
        )
        .unwrap()
        .build()
        .unwrap();
        schema.supported_on(rocky_core::Profile::Secure).unwrap();
        let manifest = schema.manifest(&[DomainId([1; 16])]);
        Self {
            manifest: SignedManifest::sign(&manifest, &publisher).unwrap(),
            policy: SignedPolicy::sign(
                PolicyStatement {
                    manifest: manifest.hash(),
                    epoch: 1,
                    revoked: BTreeSet::new(),
                },
                &publisher,
            )
            .unwrap(),
            domain: Scope {
                mesh: MeshId([1; 16]),
                app: schema.app_id(),
                domain: DomainId([1; 16]),
            },
            schema,
            publisher,
        }
    }
    fn manifest_hash(&self) -> Commitment {
        self.manifest
            .verify(self.publisher.public_key())
            .unwrap()
            .hash()
    }
    fn scope(&self, table: &str) -> ResourceScope {
        ResourceScope {
            domain: self.domain,
            encryption: rocky_core::commitment("erp.readers", reader(table).as_bytes()),
            table: self.schema.table_id(self.schema.table(table).unwrap()),
        }
    }
    fn grant(&self, table: &str, rights: &[Right]) -> ResourceGrant {
        let logical = self.schema.table(table).unwrap();
        let fields: Vec<_> = logical
            .columns
            .iter()
            .map(|column| self.schema.field_id(logical, column))
            .collect();
        grant(self.scope(table), rights, &fields)
    }
    fn grants(&self, tables: &[&str]) -> Vec<ResourceGrant> {
        tables
            .iter()
            .map(|table| {
                self.grant(
                    table,
                    &[
                        Right::Read,
                        Right::Insert,
                        Right::Update,
                        Right::Delete,
                        Right::Custody,
                    ],
                )
            })
            .collect()
    }
    fn credentials(&self, node: &PrivateKey, grants: Vec<ResourceGrant>) -> CredentialChain {
        let user = PrivateKey::new();
        let capability = |subject, kind, delegations| Capability {
            manifest: self.manifest_hash(),
            subject,
            kind,
            grants: grants.clone(),
            not_before: 0,
            expires: u64::MAX,
            epoch: 1,
            delegations,
            extension: None,
        };
        CredentialChain(vec![
            SignedCapability::sign(
                capability(user.public_key(), PrincipalKind::User, 1),
                &self.publisher,
            )
            .unwrap(),
            SignedCapability::sign(capability(node.public_key(), PrincipalKind::Node, 0), &user)
                .unwrap(),
        ])
    }
    /// A table pseudonym `number` with `grants`, all on one table.
    fn pseudonym(&self, number: u32, grants: Vec<ResourceGrant>) -> (PrivateKey, CredentialChain) {
        let key = PrivateKey::new();
        let assigner = PrivateKey::new();
        let issued = crate::credentials::Authority {
            publisher: &self.publisher,
            manifest: self.manifest_hash(),
        }
        .issue_terms(
            assigner.public_key(),
            grants.clone(),
            (1, Some(crate::credentials::AuthorRange::ALL)),
            1,
            (0, u64::MAX, 0),
        )
        .unwrap();
        let chain = CredentialChain(vec![issued])
            .assign(
                &assigner,
                key.public_key(),
                number,
                PrivateKey::new().public_key(),
                grants,
                u64::MAX,
                (0, u64::MAX, 0),
            )
            .unwrap();
        (key, chain)
    }
    async fn open(
        &self,
        path: &std::path::Path,
        node: &PrivateKey,
        number: u32,
        tables: &[&'static str],
    ) -> Device {
        let credentials = self.credentials(node, self.grants(tables));
        let replica = EncryptedReplica::open(
            path,
            TypedRows::new(self.schema.clone(), vec![DomainId([1; 16])]).unwrap(),
            SecureConfig {
                publisher: self.publisher.public_key(),
                manifest: self.manifest.clone(),
                policy: self.policy.clone(),
                node: node.clone(),
                credentials: credentials.clone(),
                keys: tables
                    .iter()
                    .map(|table| KeyMaterial {
                        scope: self.scope(table),
                        generation: 1,
                        key: rocky_core::commitment("erp.key", table.as_bytes()),
                    })
                    .collect(),
            },
        )
        .await
        .unwrap();
        replica
            .install_tables(
                self.schema.clone(),
                self.domain,
                TABLES
                    .iter()
                    .map(|table| (table.to_string(), self.scope(table).encryption))
                    .collect(),
            )
            .await
            .unwrap();
        for table in tables {
            let key = PrivateKey::from_bytes(&rocky_core::commitment(table, node.as_bytes()));
            let assigner = PrivateKey::new();
            let grants = self.grants(&[table]);
            let issued = crate::credentials::Authority {
                publisher: &self.publisher,
                manifest: self.manifest_hash(),
            }
            .issue_terms(
                assigner.public_key(),
                grants.clone(),
                (1, Some(crate::credentials::AuthorRange::ALL)),
                1,
                (0, u64::MAX, 0),
            )
            .unwrap();
            let chain = CredentialChain(vec![issued])
                .assign(
                    &assigner,
                    key.public_key(),
                    number,
                    node.public_key(),
                    grants,
                    u64::MAX,
                    (0, u64::MAX, 0),
                )
                .unwrap();
            replica.register_row_author(key, chain).await.unwrap();
        }
        Device {
            node: node.clone(),
            tables: tables.to_vec(),
            credentials,
            replica,
        }
    }
    async fn sql(&self, device: &Device) -> SecureSql<TypedRows> {
        device
            .replica
            .session(
                device
                    .tables
                    .iter()
                    .map(|table| self.scope(table))
                    .collect(),
            )
            .await
            .unwrap()
            .sql(SqlOptions::default())
            .await
            .unwrap()
    }
    /// Copies every existence record of `table` that `to` lacks from `from`.
    async fn existence(&self, from: &Device, to: &Device, table: &str) {
        let scope = self.scope(table);
        let page = from
            .replica
            .existence_page(
                to.node.public_key(),
                &to.credentials,
                scope,
                &to.replica.existence_frontier(scope).await.unwrap(),
                None,
                crate::MAX_EXISTENCE_PAGE,
            )
            .await
            .unwrap();
        for status in to
            .replica
            .receive_existence_all(page.records)
            .await
            .unwrap()
        {
            assert!(matches!(
                status,
                ReceiveStatus::Applied | ReceiveStatus::Duplicate
            ));
        }
    }
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
fn integer(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}
async fn commit(
    sql: &SecureSql<TypedRows>,
    statements: &[(&str, Vec<SqlValue>)],
) -> (Vec<i64>, Vec<EncryptedFrame>) {
    sql.run("BEGIN", vec![]).await.unwrap();
    let mut ids = Vec::new();
    for (statement, values) in statements {
        let mut values = values.clone();
        if statement.contains("?id") {
            let table = statement
                .trim_start_matches("INSERT INTO ")
                .split('(')
                .next()
                .unwrap();
            let id = sql.next_id(table).await.unwrap().integer();
            ids.push(id);
            values.push(integer(id));
        }
        sql.run(&statement.replace("?id", "?"), values)
            .await
            .unwrap();
    }
    let mut handle = [0; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut handle);
    let frames = sql.commit(handle).await.unwrap().committed.unwrap().frames;
    (ids, frames)
}
async fn deliver(device: &Device, frames: &[EncryptedFrame]) {
    for frame in frames {
        let status = device.replica.receive(frame.clone()).await.unwrap();
        assert!(
            matches!(
                status,
                ReceiveStatus::Applied
                    | ReceiveStatus::Duplicate
                    | ReceiveStatus::PendingDependencies
            ),
            "{status:?}"
        );
    }
}
async fn rows(sql: &SecureSql<TypedRows>, statement: &str) -> Vec<Vec<SqlValue>> {
    sql.run(statement, vec![])
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"))
        .rows
}
/// Every typed row of `table`, support rows included, as the file holds it.
async fn stored(device: &Device, sql: &str) -> Vec<Vec<SqlValue>> {
    let inner = device.replica.inner.lock().await;
    collect_rows(inner.conn.query(sql.to_string(), ()).await.unwrap())
        .await
        .unwrap()
}
const ITEMS: &str = "SELECT id,list,code FROM items ORDER BY id";

#[tokio::test]
async fn items_follow_the_list_existence_log_without_the_list_scope() {
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    // A holds everything, B items only, C items and lists.
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 11, &TABLES)
        .await;
    let b = erp
        .open(&dir.path().join("b.db"), &PrivateKey::new(), 12, &["items"])
        .await;
    let c = erp
        .open(
            &dir.path().join("c.db"),
            &PrivateKey::new(),
            13,
            &["lists", "items"],
        )
        .await;
    let (sa, sb, sc) = (erp.sql(&a).await, erp.sql(&b).await, erp.sql(&c).await);
    let lists = erp.scope("lists");
    assert_eq!(b.replica.existence_scopes().await.unwrap(), vec![lists]);

    let (ids, created) = commit(
        &sa,
        &[
            (
                "INSERT INTO lists(title,id) VALUES('groceries',?id)",
                vec![],
            ),
            ("INSERT INTO lists(title,id) VALUES('hardware',?id)", vec![]),
        ],
    )
    .await;
    let (groceries, hardware) = (ids[0], ids[1]);
    deliver(&c, &created).await;
    // A holder hides a list until its existence log says it is alive.
    assert!(rows(&sc, "SELECT id FROM lists").await.is_empty());
    erp.existence(&a, &c, "lists").await;
    assert_eq!(rows(&sc, "SELECT id FROM lists ORDER BY id").await.len(), 2);
    // B reads and verifies the log without the list key.
    erp.existence(&a, &b, "lists").await;
    assert_eq!(
        b.replica.existence(lists, groceries).await.unwrap(),
        Some(true)
    );

    let (item, added) = commit(
        &sa,
        &[(
            "INSERT INTO items(list,code,id) VALUES(?,'milk',?id)",
            vec![integer(groceries)],
        )],
    )
    .await;
    let item = item[0];
    deliver(&b, &added).await;
    deliver(&c, &added).await;
    // B shows the item against a puppet of its list.
    let shown = vec![vec![integer(item), integer(groceries), text("milk")]];
    assert_eq!(rows(&sb, ITEMS).await, shown);
    assert_eq!(
        stored(&b, "SELECT id,title,row_kind FROM lists").await,
        vec![vec![integer(groceries), text(""), integer(1)]]
    );
    assert!(rows(&sb, "SELECT * FROM partial_lists").await.is_empty());
    assert!(sb.run("SELECT * FROM lists", vec![]).await.is_err());

    // C points the item at hardware, offline, while A deletes groceries.
    let (_, repointed) = commit(
        &sc,
        &[(
            "UPDATE items SET list=? WHERE id=?",
            vec![integer(hardware), integer(item)],
        )],
    )
    .await;
    let (_, deleted) = commit(
        &sa,
        &[("DELETE FROM lists WHERE id=?", vec![integer(groceries)])],
    )
    .await;
    assert_eq!(
        deleted.len(),
        1,
        "a parent delete publishes no child change"
    );
    // B learns of the deletion from the log alone: the item hides.
    erp.existence(&a, &b, "lists").await;
    assert_eq!(
        b.replica.existence(lists, groceries).await.unwrap(),
        Some(false)
    );
    assert!(rows(&sb, ITEMS).await.is_empty());
    assert!(stored(&b, "SELECT id FROM lists").await.is_empty());
    // A, which holds lists, agrees on the orphan.
    assert_eq!(rows(&sa, ITEMS).await, rows(&sb, ITEMS).await);

    // The repoint brings it back everywhere, against the hardware puppet on B.
    for device in [&a, &b] {
        deliver(device, &repointed).await;
    }
    deliver(&c, &deleted).await;
    erp.existence(&a, &c, "lists").await;
    let back = vec![vec![integer(item), integer(hardware), text("milk")]];
    for session in [&sa, &sb, &sc] {
        assert_eq!(rows(session, ITEMS).await, back);
    }
    assert_eq!(
        stored(&b, "SELECT id,row_kind FROM lists").await,
        vec![vec![integer(hardware), integer(1)]]
    );

    // B adds an item to an alive list it never held; a dead or unknown
    // list fails like any dangling reference.
    let (_, local) = commit(
        &sb,
        &[(
            "INSERT INTO items(list,code,id) VALUES(?,'nails',?id)",
            vec![integer(hardware)],
        )],
    )
    .await;
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].scope().unwrap(), erp.scope("items"));
    for list in [groceries, 0, hardware + 1000] {
        sb.run("BEGIN", vec![]).await.unwrap();
        let id = sb.next_id("items").await.unwrap().integer();
        sb.run(
            "INSERT INTO items(list,code,id) VALUES(?,'x',?)",
            vec![integer(list), integer(id)],
        )
        .await
        .unwrap();
        assert!(
            matches!(
                sb.commit([list as u8; 32]).await,
                Err(Error::Sql(SqlError::Statement { code: 787, .. }))
            ),
            "{list}"
        );
    }

    // An item that arrives before its list's creation waits hidden.
    let (fresh, list) = commit(
        &sa,
        &[("INSERT INTO lists(title,id) VALUES('garden',?id)", vec![])],
    )
    .await;
    let (_, early) = commit(
        &sa,
        &[(
            "INSERT INTO items(list,code,id) VALUES(?,'seeds',?id)",
            vec![integer(fresh[0])],
        )],
    )
    .await;
    deliver(&b, &early).await;
    assert_eq!(rows(&sb, ITEMS).await.len(), 2);
    erp.existence(&a, &b, "lists").await;
    assert_eq!(rows(&sb, ITEMS).await.len(), 3);
    deliver(&c, &list).await;

    // Two devices with different held scopes agree on every orphan; the
    // list deletion orphans the garden item on both.
    let (_, gone) = commit(
        &sa,
        &[("DELETE FROM lists WHERE id=?", vec![integer(fresh[0])])],
    )
    .await;
    deliver(&a, &local).await;
    deliver(&c, &local).await;
    deliver(&c, &early).await;
    deliver(&c, &gone).await;
    erp.existence(&a, &b, "lists").await;
    erp.existence(&a, &c, "lists").await;
    let visible = rows(&sa, ITEMS).await;
    assert_eq!(visible.len(), 2);
    for session in [&sb, &sc] {
        assert_eq!(rows(session, ITEMS).await, visible);
    }

    // Support rows are rebuilt identically, and never replicated or digested.
    let puppets = stored(&b, "SELECT id,row_kind FROM lists ORDER BY id").await;
    b.replica
        .rebuild_projection(erp.scope("items"))
        .await
        .unwrap();
    assert_eq!(
        stored(&b, "SELECT id,row_kind FROM lists ORDER BY id").await,
        puppets
    );
    assert_eq!(rows(&sb, ITEMS).await, visible);
    let held = stored(&b, "SELECT DISTINCT scope FROM secure_rows").await;
    assert_eq!(
        held,
        vec![vec![SqlValue::Blob(encode(&erp.scope("items")).unwrap())]]
    );
    let digest = |device: &Device| {
        let replica = device.replica.clone();
        let scope = erp.scope("items");
        async move { replica.checkpoint(scope).await.unwrap().statement.rows }
    };
    assert_eq!(digest(&a).await, digest(&b).await);
}

#[tokio::test]
async fn puppets_are_unreadable_and_read_only() {
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 21, &TABLES)
        .await;
    let b_node = PrivateKey::new();
    let b = erp
        .open(&dir.path().join("b.db"), &b_node, 22, &["orders"])
        .await;
    let (sa, sb) = (erp.sql(&a).await, erp.sql(&b).await);
    let (region, created) = commit(
        &sa,
        &[("INSERT INTO regions(name,id) VALUES('north',?id)", vec![])],
    )
    .await;
    let (customer, customers) = commit(
        &sa,
        &[(
            "INSERT INTO customers(name,region,score,photo,note,id) VALUES('Acme',?,4.5,x'01','vip',?id)",
            vec![integer(region[0])],
        )],
    )
    .await;
    let customer = customer[0];
    let (order, ordered) = commit(
        &sa,
        &[(
            "INSERT INTO orders(customer,backup,label,id) VALUES(?,?,'first',?id)",
            vec![integer(customer), integer(customer)],
        )],
    )
    .await;
    assert!(!created.is_empty() && !customers.is_empty());
    erp.existence(&a, &b, "customers").await;
    deliver(&b, &ordered).await;

    // orders LEFT JOIN partial_customers: NULL for the puppet's columns.
    for sql in [
        "SELECT o.id,o.backup,c.name FROM orders o LEFT JOIN partial_customers c ON c.id=o.customer",
        "SELECT o.id,o.backup,c.name FROM orders o LEFT JOIN customers c ON c.id=o.customer AND c.row_kind=0",
    ] {
        assert_eq!(
            rows(&sb, sql).await,
            vec![vec![integer(order[0]), integer(customer), SqlValue::Null]]
        );
    }
    assert_eq!(
        rows(
            &sa,
            "SELECT o.id,c.name FROM orders o LEFT JOIN partial_customers c ON c.id=o.customer"
        )
        .await,
        vec![vec![integer(order[0]), text("Acme")]]
    );
    // The puppet holds zero values and points at the region's virtual row.
    assert_eq!(
        stored(
            &b,
            "SELECT id,name,region,score,photo,note,row_kind FROM customers"
        )
        .await,
        vec![vec![
            integer(customer),
            text(""),
            integer(0),
            SqlValue::real(0.0).unwrap(),
            SqlValue::Blob(Vec::new()),
            SqlValue::Null,
            integer(1)
        ]]
    );
    assert_eq!(
        stored(&b, "SELECT id,name,row_kind FROM regions").await,
        vec![vec![integer(0), text(""), integer(2)]]
    );
    // Every statement form of the partial-table check refuses them.
    for sql in [
        "SELECT * FROM customers",
        "SELECT name FROM customers WHERE id=1",
        "SELECT count(*) FROM customers",
        "SELECT * FROM customers WHERE row_kind=1",
        "SELECT * FROM customers WHERE row_kind IN (0,1)",
        "SELECT * FROM customers WHERE row_kind=0 OR id>0",
        "SELECT * FROM orders o JOIN customers c ON c.id=o.customer",
        "SELECT * FROM orders o LEFT JOIN customers c ON c.id=o.customer",
        "SELECT id FROM orders UNION SELECT id FROM customers",
        "SELECT id FROM orders WHERE customer IN (SELECT id FROM customers)",
        "SELECT * FROM regions",
        "UPDATE customers SET name='x'",
        "DELETE FROM customers",
        "INSERT INTO customers(name,region,score,photo,id,row_kind) VALUES('x',0,0,x'',5,1)",
    ] {
        assert!(sb.run(sql, vec![]).await.is_err(), "{sql}");
    }
    // A holder reading the table whole never sees a support row either.
    let (_, other) = commit(
        &sb,
        &[(
            "INSERT INTO orders(customer,label,id) VALUES(?,'second',?id)",
            vec![integer(customer)],
        )],
    )
    .await;
    assert_eq!(other.len(), 1, "support rows are never replicated");
    // The generated trigger rejects writes to support rows that bypass the session.
    let everything: BTreeSet<_> = TABLES.iter().map(|table| erp.scope(table)).collect();
    for sql in [
        "UPDATE customers SET name='x' WHERE row_kind=1",
        "DELETE FROM regions WHERE id=0",
    ] {
        let error = b.replica.run_armed(&everything, sql).await.unwrap_err();
        assert!(
            format!("{error:?}").contains("read-only"),
            "{sql}: {error:?}"
        );
    }
    let triggers = stored(
        &b,
        "SELECT name FROM sqlite_schema WHERE type='trigger' AND name LIKE 'secure_support_%' ORDER BY name",
    )
    .await;
    assert_eq!(triggers.len(), 9, "{triggers:?}");

    // A deleted customer hides its orders; support rows go with the last one.
    deliver(&a, &other).await;
    assert_eq!(rows(&sa, "SELECT id FROM orders").await.len(), 2);
    let (_, deleted) = commit(
        &sa,
        &[("DELETE FROM customers WHERE id=?", vec![integer(customer)])],
    )
    .await;
    erp.existence(&a, &b, "customers").await;
    assert!(rows(&sa, "SELECT id FROM orders").await.is_empty());
    assert!(rows(&sb, "SELECT id FROM orders").await.is_empty());
    assert!(stored(&b, "SELECT id FROM customers").await.is_empty());
    assert!(stored(&b, "SELECT id FROM regions").await.is_empty());
    assert_eq!(deleted.len(), 1);

    // Reopening keeps the derived state.
    drop(sb);
    let path = dir.path().join("b.db");
    let Device { replica, .. } = b;
    drop(replica);
    let b = erp.open(&path, &b_node, 22, &["orders"]).await;
    assert!(stored(&b, "SELECT id FROM customers").await.is_empty());
}

#[tokio::test]
async fn a_keyless_replica_verifies_signatures_and_author_rights() {
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 31, &TABLES)
        .await;
    let b = erp
        .open(&dir.path().join("b.db"), &PrivateKey::new(), 32, &["items"])
        .await;
    let sa = erp.sql(&a).await;
    let lists = erp.scope("lists");
    let (ids, _) = commit(
        &sa,
        &[(
            "INSERT INTO lists(title,id) VALUES('groceries',?id)",
            vec![],
        )],
    )
    .await;
    let page = a
        .replica
        .existence_page(
            b.node.public_key(),
            &b.credentials,
            lists,
            &Default::default(),
            None,
            16,
        )
        .await
        .unwrap();
    let [record] = page.records.as_slice() else {
        panic!("one creation record");
    };
    assert_eq!(record.event(), ExistenceEvent::Created);
    // The record names the row and nothing else of it.
    let bytes = record.to_bytes().unwrap();
    assert!(!bytes.windows(9).any(|window| window == b"groceries"));

    // A changed id breaks the signature.
    assert!(
        b.replica
            .receive_existence(record.clone().tampered(ids[0] + 1))
            .await
            .is_err()
    );
    assert_eq!(
        b.replica.receive_existence(record.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(
        b.replica.receive_existence(record.clone()).await.unwrap(),
        ReceiveStatus::Duplicate
    );
    assert_eq!(
        b.replica.existence(lists, ids[0]).await.unwrap(),
        Some(true)
    );

    // An author may only state what its grant allows, for its own ids.
    let manifest = erp.manifest_hash();
    let (inserter, chain) = erp.pseudonym(40, vec![erp.grant("lists", &[Right::Insert])]);
    let forged = |event, id, sequence, previous| {
        ExistenceRecord::forge(
            &inserter,
            chain.clone(),
            manifest,
            lists,
            id,
            event,
            40,
            sequence,
            previous,
        )
    };
    let own = rocky_core::RowIdentity::next(40, 0).unwrap().integer();
    let first = forged(ExistenceEvent::Created, own, 1, None);
    let error = b
        .replica
        .receive_existence(forged(ExistenceEvent::Deleted, ids[0], 1, None))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("lacks the right"), "{error}");
    let error = b
        .replica
        .receive_existence(forged(ExistenceEvent::Created, ids[0], 1, None))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("another author"), "{error}");
    // A gap waits for its predecessor; a second record at one position is refused.
    let next = rocky_core::RowIdentity::next(40, 1).unwrap().integer();
    let second = forged(
        ExistenceEvent::Created,
        next,
        2,
        Some(first.hash().unwrap()),
    );
    assert_eq!(
        b.replica.receive_existence(second.clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    assert_eq!(b.replica.existence(lists, next).await.unwrap(), None);
    assert_eq!(
        b.replica.receive_existence(first).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert!(
        b.replica
            .receive_existence(forged(ExistenceEvent::Created, next + 1, 1, None))
            .await
            .is_err()
    );
    assert_eq!(
        b.replica.receive_existence(second).await.unwrap(),
        ReceiveStatus::Applied
    );
    // Records of a table without an existence log are refused.
    let (items_key, items_chain) = erp.pseudonym(41, erp.grants(&["items"]));
    let outside = ExistenceRecord::forge(
        &items_key,
        items_chain,
        manifest,
        erp.scope("items"),
        rocky_core::RowIdentity::next(41, 0).unwrap().integer(),
        ExistenceEvent::Created,
        41,
        1,
        None,
    );
    assert!(b.replica.receive_existence(outside).await.is_err());
}

#[tokio::test]
async fn native_sessions_exchange_existence_logs() {
    use tokio::net::{TcpListener, TcpStream};
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 51, &TABLES)
        .await;
    let b = erp
        .open(&dir.path().join("b.db"), &PrivateKey::new(), 52, &["items"])
        .await;
    let (sa, sb) = (erp.sql(&a).await, erp.sql(&b).await);
    let (ids, _) = commit(
        &sa,
        &[(
            "INSERT INTO lists(title,id) VALUES('groceries',?id)",
            vec![],
        )],
    )
    .await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (key, credentials, replica, peer) = (
        a.node.clone(),
        a.credentials.clone(),
        a.replica.clone(),
        b.node.public_key(),
    );
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut session =
            NativeSession::authenticate(stream, key, credentials, peer, MAX_SECURE_BYTES)
                .await
                .unwrap();
        while session.serve_once(&replica).await.is_ok() {}
    });
    let mut session = NativeSession::authenticate(
        TcpStream::connect(address).await.unwrap(),
        b.node.clone(),
        b.credentials.clone(),
        a.node.public_key(),
        MAX_SECURE_BYTES,
    )
    .await
    .unwrap();
    let lists = erp.scope("lists");
    assert_eq!(
        session.sync_existence(&b.replica, lists).await.unwrap(),
        (1, 0)
    );
    assert_eq!(
        b.replica.existence(lists, ids[0]).await.unwrap(),
        Some(true)
    );
    // B's own item frames travel by the items scope; nothing to push here.
    commit(
        &sb,
        &[(
            "INSERT INTO items(list,code,id) VALUES(?,'milk',?id)",
            vec![integer(ids[0])],
        )],
    )
    .await;
    assert_eq!(
        session.sync_existence(&b.replica, lists).await.unwrap(),
        (0, 0)
    );
    session.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn holders_and_non_holders_agree_when_a_held_parent_is_hidden_or_evicted() {
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 61, &TABLES)
        .await;
    let d = erp
        .open(&dir.path().join("d.db"), &PrivateKey::new(), 62, &TABLES)
        .await;
    let b = erp
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            63,
            &["orders"],
        )
        .await;
    let (sa, sd, sb) = (erp.sql(&a).await, erp.sql(&d).await, erp.sql(&b).await);
    let (region, created) = commit(
        &sa,
        &[("INSERT INTO regions(name,id) VALUES('north',?id)", vec![])],
    )
    .await;
    deliver(&d, &created).await;
    let insert = "INSERT INTO customers(name,region,score,photo,id) VALUES(?,?,0,x'',?id)";
    // Offline, both name a customer Acme and order from it.
    let (first, from_a) = commit(&sa, &[(insert, vec![text("Acme"), integer(region[0])])]).await;
    let (second, from_d) = commit(&sd, &[(insert, vec![text("Acme"), integer(region[0])])]).await;
    let order = "INSERT INTO orders(customer,label,id) VALUES(?,'first',?id)";
    let (ours, ordered_a) = commit(&sa, &[(order, vec![integer(first[0])])]).await;
    let (theirs, ordered_d) = commit(&sd, &[(order, vec![integer(second[0])])]).await;
    for (frames, orders) in [(&from_d, &ordered_d), (&from_a, &ordered_a)] {
        for device in [&a, &d] {
            deliver(device, frames).await;
            deliver(device, orders).await;
        }
        deliver(&b, orders).await;
    }
    erp.existence(&a, &d, "customers").await;
    erp.existence(&d, &a, "customers").await;
    erp.existence(&a, &b, "customers").await;
    let names = "SELECT name FROM customers WHERE name='Acme'";
    for session in [&sa, &sd] {
        assert_eq!(rows(session, names).await.len(), 1);
    }
    // The hidden Acme is alive: its order shows everywhere, against a
    // puppet on the holders as on B.
    let mut both = [ours[0], theirs[0]];
    both.sort();
    let orders: Vec<_> = both.iter().map(|id| vec![integer(*id)]).collect();
    for session in [&sa, &sd, &sb] {
        assert_eq!(
            rows(session, "SELECT id FROM orders ORDER BY id").await,
            orders
        );
    }
    let kinds = format!(
        "SELECT row_kind FROM customers WHERE id IN ({},{}) ORDER BY row_kind",
        first[0], second[0]
    );
    for device in [&a, &d] {
        assert_eq!(
            stored(device, &kinds).await,
            vec![vec![integer(0)], vec![integer(1)]]
        );
    }
    assert_eq!(
        stored(&b, &kinds).await,
        vec![vec![integer(1)], vec![integer(1)]]
    );

    // Evicting the customers scope leaves the orders on puppets.
    let customers = erp.scope("customers");
    {
        let mut inner = a.replica.inner.lock().await;
        transaction!(inner, |inner| inner.clear_typed(customers).await).unwrap();
    }
    let listed = "SELECT id FROM orders ORDER BY id";
    assert_eq!(rows(&sa, listed).await, orders);
    assert!(rows(&sa, "SELECT id FROM customers").await.is_empty());
    a.replica.rebuild_projection(customers).await.unwrap();
    assert_eq!(rows(&sa, names).await.len(), 1);
    assert_eq!(rows(&sa, listed).await, orders);
}

#[tokio::test]
async fn horizon_ack_waits_for_the_exact_existence_cut_and_survives_reopen() {
    use crate::credentials::{HorizonAck, SignedHorizon};
    let erp = Erp::new();
    let tmp = TempDir::new().unwrap();
    let all = erp
        .open(&tmp.path().join("all.db"), &PrivateKey::new(), 1, &TABLES)
        .await;
    let node = PrivateKey::new();
    let child_path = tmp.path().join("child.db");
    let child = erp.open(&child_path, &node, 2, &["items"]).await;
    let sql = erp.sql(&all).await;
    commit(
        &sql,
        &[("INSERT INTO lists(title,id) VALUES('horizon',?id)", vec![])],
    )
    .await;
    drop(sql);
    let scope = erp.scope("items");
    let peers = vec![(child.node.public_key(), child.credentials.clone())];
    let statement = all.replica.horizon_statement(scope, &peers).await.unwrap();
    assert_eq!(statement.existence.len(), 1);
    assert_eq!(statement.existence[0].scope, erp.scope("lists"));
    assert_eq!(statement.existence[0].logs.len(), 1);
    let customer_cut = all
        .replica
        .horizon_statement(erp.scope("customers"), &[])
        .await
        .unwrap();
    assert_eq!(
        customer_cut
            .related
            .iter()
            .map(|cut| cut.scope)
            .collect::<Vec<_>>(),
        vec![erp.scope("regions")]
    );
    assert_eq!(
        customer_cut
            .existence
            .iter()
            .map(|cut| cut.scope)
            .collect::<Vec<_>>(),
        vec![erp.scope("customers")]
    );
    let mut omitted = statement.clone();
    omitted.existence.clear();
    omitted.bases.clear();
    assert!(
        all.replica
            .receive_horizon(SignedHorizon::sign(omitted, &erp.publisher).unwrap())
            .await
            .is_err()
    );
    let signed = SignedHorizon::sign(statement.clone(), &erp.publisher).unwrap();
    let impostor = SignedHorizon::sign(statement, &PrivateKey::new()).unwrap();
    assert!(all.replica.receive_horizon(impostor).await.is_err());
    all.replica.receive_horizon(signed.clone()).await.unwrap();
    child.replica.receive_horizon(signed.clone()).await.unwrap();
    assert!(child.replica.acknowledge_horizon(scope).await.is_err());
    assert_eq!(
        all.replica.horizon_progress(scope).await.unwrap().waiting,
        vec![node.public_key()]
    );
    erp.existence(&all, &child, "lists").await;
    let ack = child.replica.acknowledge_horizon(scope).await.unwrap();
    let outsider = HorizonAck::sign(signed.id().unwrap(), &PrivateKey::new()).unwrap();
    assert!(
        all.replica
            .receive_horizon_ack(scope, outsider)
            .await
            .is_err()
    );
    let wrong_cut = HorizonAck::sign([7; 32], &node).unwrap();
    assert!(
        all.replica
            .receive_horizon_ack(scope, wrong_cut)
            .await
            .is_err()
    );
    all.replica
        .receive_horizon_ack(scope, ack.clone())
        .await
        .unwrap();
    all.replica.receive_horizon(signed.clone()).await.unwrap();
    assert_eq!(
        all.replica
            .horizon_progress(scope)
            .await
            .unwrap()
            .acknowledged,
        vec![node.public_key()]
    );
    drop(child);
    let child = erp.open(&child_path, &node, 2, &["items"]).await;
    assert_eq!(
        child
            .replica
            .horizon_progress(scope)
            .await
            .unwrap()
            .acknowledged,
        vec![node.public_key()]
    );
    let deadline = signed.statement.issued + crate::credentials::HORIZON_WAIT_SECONDS;
    let progress = {
        let inner = all.replica.inner.lock().await;
        inner.horizon_progress_at(&signed, deadline).await.unwrap()
    };
    assert!(progress.behind.is_empty());
    assert_eq!(progress.acknowledged, vec![node.public_key()]);
    let mut next = signed.statement.clone();
    next.sequence += 1;
    next.previous = Some(signed.id().unwrap());
    let next = SignedHorizon::sign(next, &erp.publisher).unwrap();
    all.replica.receive_horizon(next.clone()).await.unwrap();
    assert!(all.replica.receive_horizon(signed).await.is_err());
    assert!(all.replica.receive_horizon_ack(scope, ack).await.is_err());
    let inner = all.replica.inner.lock().await;
    let waiting = inner
        .horizon_progress_at(&next, deadline - 1)
        .await
        .unwrap();
    assert_eq!(waiting.waiting, vec![node.public_key()]);
    let expired = inner.horizon_progress_at(&next, deadline).await.unwrap();
    assert!(expired.waiting.is_empty());
    assert_eq!(expired.behind, vec![node.public_key()]);
    drop(inner);
    let late = HorizonAck::sign(next.id().unwrap(), &node).unwrap();
    crate::CLOCK_SKEW.with(|skew| skew.set(crate::credentials::HORIZON_WAIT_SECONDS));
    let result = all.replica.receive_horizon_ack(scope, late).await;
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
    assert!(result.is_err());
}

#[tokio::test]
async fn a_horizon_proposal_alone_never_releases_tombstones_or_existence_history() {
    for cycles in [16, 64] {
        let erp = Erp::new();
        let tmp = TempDir::new().unwrap();
        let device = erp
            .open(&tmp.path().join("scope.db"), &PrivateKey::new(), 1, &TABLES)
            .await;
        let sql = erp.sql(&device).await;
        for _ in 0..cycles {
            let (ids, _) = commit(
                &sql,
                &[("INSERT INTO lists(title,id) VALUES('churn',?id)", vec![])],
            )
            .await;
            commit(
                &sql,
                &[("DELETE FROM lists WHERE id=?", vec![integer(ids[0])])],
            )
            .await;
        }
        device.replica.checkpoint(erp.scope("lists")).await.unwrap();
        let storage = "SELECT (SELECT COUNT(*) FROM secure_rows),(SELECT COALESCE(SUM(LENGTH(fields)),0) FROM secure_rows),(SELECT COUNT(*) FROM secure_existence_log),(SELECT COALESCE(SUM(LENGTH(record)),0) FROM secure_existence_log)";
        let before = stored(&device, storage).await;
        let statement = device
            .replica
            .horizon_statement(erp.scope("lists"), &[])
            .await
            .unwrap();
        let signed = crate::credentials::SignedHorizon::sign(statement, &erp.publisher).unwrap();
        device.replica.receive_horizon(signed).await.unwrap();
        assert_eq!(stored(&device, storage).await, before);
        assert_eq!(before[0][0], integer(cycles));
        assert_eq!(before[0][2], integer(cycles * 2));
        eprintln!(
            "horizon proposal churn: cycles={cycles}; tombstones,row_bytes,existence_records,existence_bytes={before:?}"
        );
    }
}

#[tokio::test]
async fn horizon_checkpoint_fences_legacy_import_and_preserves_declarations_on_rebootstrap() {
    use crate::credentials::SignedHorizon;
    let erp = Erp::new();
    let tmp = TempDir::new().unwrap();
    let source = erp
        .open(
            &tmp.path().join("source.db"),
            &PrivateKey::new(),
            1,
            &TABLES,
        )
        .await;
    let target_node = PrivateKey::new();
    let target_path = tmp.path().join("target.db");
    let target = erp.open(&target_path, &target_node, 2, &TABLES).await;
    let sql = erp.sql(&source).await;
    commit(
        &sql,
        &[("INSERT INTO lists(title,id) VALUES('retained',?id)", vec![])],
    )
    .await;
    drop(sql);
    let scope = erp.scope("lists");
    let legacy = source.replica.checkpoint(scope).await.unwrap();
    assert_eq!(legacy.statement.format, 3);
    let horizon = SignedHorizon::sign(
        source.replica.horizon_statement(scope, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    source
        .replica
        .receive_horizon(horizon.clone())
        .await
        .unwrap();
    target
        .replica
        .receive_horizon(horizon.clone())
        .await
        .unwrap();
    assert!(
        target
            .replica
            .begin_rebootstrap(legacy.clone())
            .await
            .is_err()
    );
    let checkpoint = source.replica.checkpoint(scope).await.unwrap();
    assert_eq!(checkpoint.statement.format, 5);
    assert_eq!(checkpoint.statement.horizon.as_deref(), Some(&horizon));
    let offered = source
        .replica
        .checkpoint_offer(target_node.public_key(), &target.credentials, scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offered.id().unwrap(), checkpoint.id().unwrap());
    // A signed outer checkpoint cannot substitute an impostor issuer declaration.
    let mut tampered = checkpoint.statement.clone();
    tampered.horizon = Some(Box::new(
        SignedHorizon::sign(horizon.statement.clone(), &PrivateKey::new()).unwrap(),
    ));
    let forged =
        crate::replica::SignedCheckpoint::sign(tampered, &source.node, source.credentials.clone())
            .unwrap();
    assert!(target.replica.begin_rebootstrap(forged).await.is_err());
    // Even an issuer-signed successor cannot move a replacement base behind
    // the cut this holder already admitted.
    let mut backward = horizon.statement.clone();
    backward.sequence += 1;
    backward.previous = Some(horizon.id().unwrap());
    backward.logs.clear();
    let mut older = checkpoint.statement.clone();
    older.horizon = Some(Box::new(
        SignedHorizon::sign(backward, &erp.publisher).unwrap(),
    ));
    older.frontier.clear();
    let older =
        crate::replica::SignedCheckpoint::sign(older, &source.node, source.credentials.clone())
            .unwrap();
    assert!(target.replica.begin_rebootstrap(older).await.is_err());
    target
        .replica
        .begin_rebootstrap(checkpoint.clone())
        .await
        .unwrap();
    let next = SignedHorizon::sign(
        source.replica.horizon_statement(scope, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    // A declaration admitted while pages are staged fences the final switch too.
    target.replica.receive_horizon(next).await.unwrap();
    let page = source
        .replica
        .checkpoint_page(
            target_node.public_key(),
            &target.credentials,
            scope,
            checkpoint.id().unwrap(),
            None,
            crate::MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    target.replica.stage_checkpoint_page(page).await.unwrap();
    assert!(target.replica.finish_rebootstrap(scope).await.is_err());
    let fresh_node = PrivateKey::new();
    let fresh_path = tmp.path().join("fresh.db");
    let fresh = erp.open(&fresh_path, &fresh_node, 3, &TABLES).await;
    fresh
        .replica
        .begin_rebootstrap(checkpoint.clone())
        .await
        .unwrap();
    let page = source
        .replica
        .checkpoint_page(
            fresh_node.public_key(),
            &fresh.credentials,
            scope,
            checkpoint.id().unwrap(),
            None,
            crate::MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    fresh.replica.stage_checkpoint_page(page).await.unwrap();
    erp.existence(&source, &fresh, "lists").await;
    fresh.replica.finish_rebootstrap(scope).await.unwrap();
    assert!(fresh.replica.begin_rebootstrap(legacy).await.is_err());
    let identity = {
        let inner = fresh.replica.inner.lock().await;
        let rows = collect_rows(
            inner
                .conn
                .query("SELECT identity FROM secure_metadata WHERE id=1", ())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        let [SqlValue::Blob(bytes)] = rows[0].as_slice() else {
            panic!("identity");
        };
        crate::decode::<(String, Commitment, PublicKey)>(bytes)
            .unwrap()
            .0
    };
    assert_eq!(identity, "rocky.encrypted-replica.v6");
    drop(fresh);
    let reopened = erp.open(&fresh_path, &fresh_node, 3, &TABLES).await;
    assert_eq!(
        reopened
            .replica
            .inner
            .lock()
            .await
            .mergeable_horizon(scope)
            .await
            .unwrap(),
        Some(Box::new(horizon))
    );
}

#[tokio::test]
async fn horizon_recovery_refuses_packages_that_cannot_preserve_the_declaration() {
    use rocky_core::{CommandSet, TypedCommands};
    let erp = Erp::new();
    let tmp = TempDir::new().unwrap();
    let source_node = PrivateKey::new();
    let target_node = PrivateKey::new();
    let configs = [&source_node, &target_node].map(|node| SecureConfig {
        publisher: erp.publisher.public_key(),
        manifest: erp.manifest.clone(),
        policy: erp.policy.clone(),
        node: node.clone(),
        credentials: erp.credentials(node, erp.grants(&TABLES)),
        keys: TABLES
            .iter()
            .map(|table| KeyMaterial {
                scope: erp.scope(table),
                generation: 1,
                key: rocky_core::commitment("erp.key", table.as_bytes()),
            })
            .collect(),
    });
    let target_credentials = configs[1].credentials.clone();
    let coordinator = rocky_coordinator::MemoryCoordinator::default();
    let owner = crate::secure_principal_id(configs[0].credentials.0[0].capability.subject);
    coordinator
        .register(
            erp.domain,
            owner,
            [1; 16],
            vec![
                crate::secure_node_id(source_node.public_key()),
                crate::secure_node_id(target_node.public_key()),
            ],
            1,
            1,
        )
        .unwrap();
    coordinator
        .acquire(
            erp.domain,
            owner,
            crate::secure_node_id(source_node.public_key()),
        )
        .unwrap();
    let app = || {
        TypedCommands::new(
            TypedRows::new(erp.schema.clone(), vec![erp.domain.domain]).unwrap(),
            CommandSet::default(),
            TABLES
                .iter()
                .map(|table| (table.to_string(), erp.scope(table)))
                .collect(),
            None,
        )
        .unwrap()
    };
    let [source_config, target_config] = configs;
    let source = EncryptedReplica::open(tmp.path().join("source.db"), app(), source_config)
        .await
        .unwrap();
    let target = EncryptedReplica::open(tmp.path().join("target.db"), app(), target_config)
        .await
        .unwrap();
    for replica in [&source, &target] {
        replica
            .install_tables(
                erp.schema.clone(),
                erp.domain,
                TABLES
                    .iter()
                    .map(|table| (table.to_string(), erp.scope(table).encryption))
                    .collect(),
            )
            .await
            .unwrap();
    }
    let scope = erp.scope("lists");
    let package = source
        .export_recovery(
            target_node.public_key(),
            &target_credentials,
            vec![scope],
            &[7; 32],
            &coordinator,
        )
        .await
        .unwrap();
    target
        .import_recovery(&package, &[7; 32], &coordinator)
        .await
        .unwrap();
    let signed = crate::credentials::SignedHorizon::sign(
        source.horizon_statement(scope, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    source.receive_horizon(signed.clone()).await.unwrap();
    target.receive_horizon(signed.clone()).await.unwrap();
    let before = target.snapshot(scope).await.unwrap().revision;
    assert!(
        source
            .export_recovery(
                target_node.public_key(),
                &target_credentials,
                vec![scope],
                &[7; 32],
                &coordinator
            )
            .await
            .is_err()
    );
    assert!(
        target
            .import_recovery(&package, &[7; 32], &coordinator)
            .await
            .is_err()
    );
    assert_eq!(target.snapshot(scope).await.unwrap().revision, before);
    assert_eq!(
        target
            .inner
            .lock()
            .await
            .mergeable_horizon(scope)
            .await
            .unwrap(),
        Some(Box::new(signed))
    );
}

#[tokio::test]
async fn horizon_file_identity_upgrades_v4_and_refuses_unknown_future_files() {
    let erp = Erp::new();
    let tmp = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let path = tmp.path().join("file.db");
    let device = erp.open(&path, &node, 1, &TABLES).await;
    let legacy = crate::encode(&(
        "rocky.encrypted-replica.v4",
        erp.manifest_hash(),
        node.public_key(),
    ))
    .unwrap();
    device
        .replica
        .inner
        .lock()
        .await
        .conn
        .execute("UPDATE secure_metadata SET identity=? WHERE id=1", [legacy])
        .await
        .unwrap();
    drop(device);
    let upgraded = erp.open(&path, &node, 1, &TABLES).await;
    let inner = upgraded.replica.inner.lock().await;
    let rows = collect_rows(
        inner
            .conn
            .query("SELECT identity FROM secure_metadata WHERE id=1", ())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let [SqlValue::Blob(bytes)] = rows[0].as_slice() else {
        panic!("identity");
    };
    assert_eq!(
        crate::decode::<(String, Commitment, PublicKey)>(bytes)
            .unwrap()
            .0,
        "rocky.encrypted-replica.v6"
    );
    // The current binary must likewise refuse a future fence, without downgrading it.
    let future = crate::encode(&(
        "rocky.encrypted-replica.v7",
        erp.manifest_hash(),
        node.public_key(),
    ))
    .unwrap();
    inner
        .conn
        .execute(
            "UPDATE secure_metadata SET identity=? WHERE id=1",
            [future.clone()],
        )
        .await
        .unwrap();
    drop(inner);
    drop(upgraded);
    let result = EncryptedReplica::open(
        &path,
        TypedRows::new(erp.schema.clone(), vec![erp.domain.domain]).unwrap(),
        SecureConfig {
            publisher: erp.publisher.public_key(),
            manifest: erp.manifest.clone(),
            policy: erp.policy.clone(),
            node: node.clone(),
            credentials: erp.credentials(&node, erp.grants(&TABLES)),
            keys: TABLES
                .iter()
                .map(|table| KeyMaterial {
                    scope: erp.scope(table),
                    generation: 1,
                    key: rocky_core::commitment("erp.key", table.as_bytes()),
                })
                .collect(),
        },
    )
    .await;
    assert!(result.is_err());
    let (owner, raw) = crate::database::DatabaseOwner::open(&path).await.unwrap();
    let conn = crate::scoped_connection::ScopedConnection::new(raw, None, crate::store::SCHEMA);
    let rows = collect_rows(
        conn.query("SELECT identity FROM secure_metadata WHERE id=1", ())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(rows, vec![vec![SqlValue::Blob(future)]]);
    drop(owner);
}

#[tokio::test]
async fn horizon_finalizes_child_only_orphans_and_stored_null_atomically() {
    use crate::credentials::SignedHorizon;
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let full = erp
        .open(&dir.path().join("full.db"), &PrivateKey::new(), 71, &TABLES)
        .await;
    let node = PrivateKey::new();
    let path = dir.path().join("child.db");
    let child = erp.open(&path, &node, 72, &["orders"]).await;
    let sql = erp.sql(&full).await;
    let (region, _) = commit(
        &sql,
        &[("INSERT INTO regions(name,id) VALUES('north',?id)", vec![])],
    )
    .await;
    let (customers, _) = commit(
        &sql,
        &[
            (
                "INSERT INTO customers(name,region,score,photo,id) VALUES('main',?,0,x'',?id)",
                vec![integer(region[0])],
            ),
            (
                "INSERT INTO customers(name,region,score,photo,id) VALUES('backup',?,0,x'',?id)",
                vec![integer(region[0])],
            ),
        ],
    )
    .await;
    let (orders, created) = commit(
        &sql,
        &[
            (
                "INSERT INTO orders(customer,backup,label,id) VALUES(?,?,'survivor',?id)",
                vec![integer(customers[0]), integer(customers[1])],
            ),
            (
                "INSERT INTO orders(customer,label,id) VALUES(?,'orphan',?id)",
                vec![integer(customers[1])],
            ),
            (
                "INSERT INTO orders(customer,label,id) VALUES(?,'deleted',?id)",
                vec![integer(customers[0])],
            ),
        ],
    )
    .await;
    deliver(&child, &created).await;
    let (_, deleted) = commit(
        &sql,
        &[
            (
                "DELETE FROM customers WHERE id=?",
                vec![integer(customers[1])],
            ),
            ("DELETE FROM orders WHERE id=?", vec![integer(orders[2])]),
        ],
    )
    .await;
    // Only the orders frame is readable by the partial holder.
    for frame in &deleted {
        if frame.operation().unwrap().header.extensions.scope == erp.scope("orders") {
            deliver(&child, std::slice::from_ref(frame)).await;
        }
    }
    let scope = erp.scope("orders");
    let horizon = SignedHorizon::sign(
        full.replica.horizon_statement(scope, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    for device in [&full, &child] {
        device
            .replica
            .receive_horizon(horizon.clone())
            .await
            .unwrap();
    }
    assert!(child.replica.finalize_horizon(scope).await.is_err());
    erp.existence(&full, &child, "customers").await;
    // Checkpoint verification succeeds before injecting the final switch fault.
    child.replica.checkpoint(scope).await.unwrap();
    let before = child.replica.snapshot(scope).await.unwrap();
    child.replica.inner.lock().await.fail_before_commit = 1;
    assert!(child.replica.finalize_horizon(scope).await.is_err());
    assert_eq!(
        child.replica.snapshot(scope).await.unwrap().revision,
        before.revision
    );
    let a = full.replica.finalize_horizon(scope).await.unwrap();
    let b = child.replica.finalize_horizon(scope).await.unwrap();
    assert_eq!(a.statement.rows, b.statement.rows);
    assert_eq!(a.statement.rows.count, 1);
    assert_eq!(a.statement.tombstones, 0);
    assert_eq!(a.statement.identities, b.statement.identities);
    assert_eq!(a.statement.format, 6);
    assert_eq!(a.statement.finalized.as_deref(), Some(&horizon));
    let child_sql = erp.sql(&child).await;
    assert_eq!(
        rows(&child_sql, "SELECT label,backup FROM orders").await,
        vec![vec![text("survivor"), SqlValue::Null]]
    );
    let snapshot = child.replica.snapshot(scope).await.unwrap();
    let table = erp.schema.table("orders").unwrap();
    let field = erp.schema.field_id(
        table,
        table
            .columns
            .iter()
            .find(|c| c.sql_name == "backup")
            .unwrap(),
    );
    let mut null = Vec::new();
    SqlValue::Null.encode(&mut null);
    assert_eq!(
        snapshot.rows.values().next().unwrap().get(&field),
        Some(&null)
    );
    child.replica.audit(scope).await.unwrap();
    child.replica.rebuild_projection(scope).await.unwrap();
    assert_eq!(
        child
            .replica
            .finalize_horizon(scope)
            .await
            .unwrap()
            .id()
            .unwrap(),
        b.id().unwrap()
    );
    drop(child_sql);
    drop(child);
    let reopened = erp.open(&path, &node, 72, &["orders"]).await;
    assert_eq!(
        reopened
            .replica
            .checkpoint(scope)
            .await
            .unwrap()
            .statement
            .rows,
        a.statement.rows
    );
    reopened.replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn horizon_drops_absent_local_edits_and_deletes_without_ending_plain_or_logged_logs() {
    use crate::credentials::SignedHorizon;
    for (number, table, column) in [(81, "regions", "name"), (82, "lists", "title")] {
        let erp = Erp::new();
        let dir = TempDir::new().unwrap();
        let device = erp
            .open(
                &dir.path().join("device.db"),
                &PrivateKey::new(),
                number,
                &TABLES,
            )
            .await;
        let witness = erp
            .open(
                &dir.path().join("witness.db"),
                &PrivateKey::new(),
                number + 10,
                &TABLES,
            )
            .await;
        let sql = erp.sql(&device).await;
        let (ids, created) = commit(
            &sql,
            &[(
                &format!("INSERT INTO {table}({column},id) VALUES('original',?id)"),
                vec![],
            )],
        )
        .await;
        deliver(&witness, &created).await;
        let scope = erp.scope(table);
        let before = device.replica.snapshot(scope).await.unwrap();
        let fields = before.rows.values().next().unwrap().clone();
        let typed = TypedRows::new(erp.schema.clone(), vec![erp.domain.domain]).unwrap();
        let logical = erp.schema.table(table).unwrap();
        let typed_table = typed.tables().find(|t| t.sql_name == table).unwrap();
        let values = typed.values(typed_table, &fields).unwrap();
        let row = typed
            .row_id(erp.domain.domain, typed_table, &values)
            .unwrap();
        let mut edited = fields.clone();
        let field = erp.schema.field_id(
            logical,
            logical
                .columns
                .iter()
                .find(|c| c.sql_name == column)
                .unwrap(),
        );
        let mut value = Vec::new();
        text("pending").encode(&mut value);
        edited.insert(field, value);
        let pending = witness
            .replica
            .author(
                [85; 32],
                vec![RowChange {
                    scope,
                    row,
                    before: Some(fields.clone()),
                    after: Some(edited.clone()),
                }],
                vec![],
            )
            .await
            .unwrap();
        let continued = witness
            .replica
            .author(
                [86; 32],
                vec![RowChange {
                    scope,
                    row,
                    before: Some(edited),
                    after: None,
                }],
                vec![],
            )
            .await
            .unwrap();
        let (_, deleted) = commit(
            &sql,
            &[(
                &format!("DELETE FROM {table} WHERE id=?"),
                vec![integer(ids[0])],
            )],
        )
        .await;
        let signed = SignedHorizon::sign(
            device.replica.horizon_statement(scope, &[]).await.unwrap(),
            &erp.publisher,
        )
        .unwrap();
        device.replica.receive_horizon(signed).await.unwrap();
        device.replica.finalize_horizon(scope).await.unwrap();
        for frame in created.iter().chain(&deleted) {
            device
                .replica
                .acknowledge(
                    witness.node.public_key(),
                    &witness.credentials,
                    frame.id().unwrap(),
                    true,
                )
                .await
                .unwrap();
        }
        device
            .replica
            .set_retention_policy(
                scope,
                crate::RetentionPolicy {
                    required: BTreeSet::from([witness.node.public_key()]),
                    behind: BTreeSet::new(),
                    copies: 1,
                    window: 0,
                },
            )
            .await
            .unwrap();
        if table == "lists" {
            let base = device
                .replica
                .installed_existence_base(scope)
                .await
                .unwrap()
                .unwrap();
            let id = base.id().unwrap();
            witness.replica.begin_existence_base(base).await.unwrap();
            let page = device
                .replica
                .existence_base_page(
                    witness.node.public_key(),
                    &witness.credentials,
                    scope,
                    id,
                    None,
                    256,
                )
                .await
                .unwrap();
            witness
                .replica
                .stage_existence_base(scope, page)
                .await
                .unwrap();
            witness.replica.finish_existence_base(scope).await.unwrap();
            let checkpoint = device.replica.checkpoint(scope).await.unwrap();
            let id = checkpoint.id().unwrap();
            witness
                .replica
                .begin_rebootstrap(checkpoint.clone())
                .await
                .unwrap();
            let page = device
                .replica
                .checkpoint_page(
                    witness.node.public_key(),
                    &witness.credentials,
                    scope,
                    id,
                    None,
                    crate::MAX_SECURE_FRAME,
                )
                .await
                .unwrap();
            witness.replica.stage_checkpoint_page(page).await.unwrap();
            witness.replica.finish_rebootstrap(scope).await.unwrap();
            device
                .replica
                .acknowledge_horizon_rebootstrap(
                    witness.node.public_key(),
                    &witness.credentials,
                    scope,
                    checkpoint.statement.finalized.unwrap().id().unwrap(),
                )
                .await
                .unwrap();
        }
        assert_eq!(device.replica.prune(scope).await.unwrap().pruned, 2);
        assert_eq!(
            stored(&device, "SELECT COUNT(*) FROM secure_operations").await,
            vec![vec![integer(0)]]
        );
        assert!(
            device
                .replica
                .snapshot(scope)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            device.replica.receive(pending.clone()).await.unwrap(),
            ReceiveStatus::Applied
        );
        assert_eq!(
            device.replica.receive(continued.clone()).await.unwrap(),
            ReceiveStatus::Applied
        );
        assert_eq!(
            continued.operation().unwrap().header.backlink,
            Some(pending.id().unwrap())
        );
        assert!(
            device
                .replica
                .snapshot(scope)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        let existence_before = device.replica.existence_frontier(scope).await.unwrap();
        let mut after = fields.clone();
        let field = erp.schema.field_id(
            logical,
            logical
                .columns
                .iter()
                .find(|c| c.sql_name == column)
                .unwrap(),
        );
        let mut value = Vec::new();
        text("late").encode(&mut value);
        after.insert(field, value);
        let edit = device
            .replica
            .author(
                [83; 32],
                vec![RowChange {
                    scope,
                    row,
                    before: Some(fields.clone()),
                    after: Some(after),
                }],
                vec![],
            )
            .await
            .unwrap();
        let delete = device
            .replica
            .author(
                [84; 32],
                vec![RowChange {
                    scope,
                    row,
                    before: Some(fields),
                    after: None,
                }],
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(
            delete.operation().unwrap().header.backlink,
            Some(edit.id().unwrap())
        );
        assert_eq!(
            device.replica.existence_frontier(scope).await.unwrap(),
            existence_before
        );
        assert!(
            device
                .replica
                .snapshot(scope)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        let (_, next) = commit(
            &sql,
            &[(
                &format!("INSERT INTO {table}({column},id) VALUES('next',?id)"),
                vec![],
            )],
        )
        .await;
        assert_eq!(
            next[0].operation().unwrap().header.backlink,
            Some(delete.id().unwrap())
        );
        device.replica.checkpoint(scope).await.unwrap();
        device.replica.rebuild_projection(scope).await.unwrap();
        device.replica.audit(scope).await.unwrap();
        assert_eq!(
            rows(&sql, &format!("SELECT {column} FROM {table}")).await,
            vec![vec![text("next")]]
        );
    }
}

#[tokio::test]
async fn horizon_finalizes_unique_losers_without_later_reappearance() {
    use crate::credentials::SignedHorizon;
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 91, &TABLES)
        .await;
    let b = erp
        .open(&dir.path().join("b.db"), &PrivateKey::new(), 92, &TABLES)
        .await;
    let sa = erp.sql(&a).await;
    let sb = erp.sql(&b).await;
    let (region, created) = commit(
        &sa,
        &[("INSERT INTO regions(name,id) VALUES('north',?id)", vec![])],
    )
    .await;
    deliver(&b, &created).await;
    let insert = "INSERT INTO customers(name,region,score,photo,id) VALUES('same',?,0,x'',?id)";
    let (_, ours) = commit(&sa, &[(insert, vec![integer(region[0])])]).await;
    let (_, theirs) = commit(&sb, &[(insert, vec![integer(region[0])])]).await;
    deliver(&a, &theirs).await;
    deliver(&b, &ours).await;
    erp.existence(&a, &b, "customers").await;
    erp.existence(&b, &a, "customers").await;
    let scope = erp.scope("customers");
    let signed = SignedHorizon::sign(
        a.replica.horizon_statement(scope, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    for device in [&a, &b] {
        device
            .replica
            .receive_horizon(signed.clone())
            .await
            .unwrap();
    }
    let ca = a.replica.finalize_horizon(scope).await.unwrap();
    let cb = b.replica.finalize_horizon(scope).await.unwrap();
    assert_eq!(ca.statement.rows, cb.statement.rows);
    assert_eq!(ca.statement.rows.count, 1);
    let winner = rows(&sa, "SELECT id FROM customers WHERE row_kind=0").await[0][0].clone();
    let (_, deleted) = commit(&sa, &[("DELETE FROM customers WHERE id=?", vec![winner])]).await;
    deliver(&b, &deleted).await;
    erp.existence(&a, &b, "customers").await;
    for (device, sql) in [(&a, &sa), (&b, &sb)] {
        device.replica.rebuild_projection(scope).await.unwrap();
        assert!(
            rows(sql, "SELECT id FROM customers WHERE row_kind=0")
                .await
                .is_empty()
        );
        device.replica.audit(scope).await.unwrap();
    }
}

#[tokio::test]
async fn horizon_finalization_and_retention_plateau() {
    use crate::credentials::SignedHorizon;
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 101, &TABLES)
        .await;
    let b = erp
        .open(&dir.path().join("b.db"), &PrivateKey::new(), 102, &TABLES)
        .await;
    for holder in [&a, &b] {
        let mut limits = holder.replica.retention_limits().await;
        limits.handles = 4;
        holder.replica.set_retention_limits(limits).await.unwrap();
    }
    let sql = erp.sql(&a).await;
    let scope = erp.scope("lists");
    for (holder, peer) in [(&a, &b), (&b, &a)] {
        holder
            .replica
            .set_retention_policy(
                scope,
                crate::RetentionPolicy {
                    required: BTreeSet::from([peer.node.public_key()]),
                    behind: BTreeSet::new(),
                    copies: 1,
                    window: 0,
                },
            )
            .await
            .unwrap();
    }
    let mut plateau = None;
    let mut physical = None;
    for cycle in 1..=64 {
        let (ids, created) = commit(
            &sql,
            &[("INSERT INTO lists(title,id) VALUES('churn',?id)", vec![])],
        )
        .await;
        let (_, deleted) = commit(
            &sql,
            &[("DELETE FROM lists WHERE id=?", vec![integer(ids[0])])],
        )
        .await;
        deliver(&b, &created).await;
        deliver(&b, &deleted).await;
        erp.existence(&a, &b, "lists").await;
        let signed = SignedHorizon::sign(
            a.replica
                .horizon_statement(
                    scope,
                    &[
                        (a.node.public_key(), a.credentials.clone()),
                        (b.node.public_key(), b.credentials.clone()),
                    ],
                )
                .await
                .unwrap(),
            &erp.publisher,
        )
        .unwrap();
        for holder in [&a, &b] {
            holder
                .replica
                .receive_horizon(signed.clone())
                .await
                .unwrap();
        }
        let aa = a.replica.acknowledge_horizon(scope).await.unwrap();
        let ab = b.replica.acknowledge_horizon(scope).await.unwrap();
        a.replica
            .receive_horizon_ack(scope, ab.clone())
            .await
            .unwrap();
        b.replica.receive_horizon_ack(scope, aa).await.unwrap();
        for holder in [&a, &b] {
            holder.replica.finalize_horizon(scope).await.unwrap();
        }
        for (holder, peer) in [(&a, &b), (&b, &a)] {
            for frame in created.iter().chain(&deleted) {
                holder
                    .replica
                    .acknowledge(
                        peer.node.public_key(),
                        &peer.credentials,
                        frame.id().unwrap(),
                        true,
                    )
                    .await
                    .unwrap();
            }
        }
        for holder in [&a, &b] {
            let report = holder.replica.prune(scope).await.unwrap();
            assert_eq!(report.blocked, None);
            assert_eq!(report.pruned, 2);
        }
        if [16, 64].contains(&cycle) {
            let sql = "SELECT (SELECT COUNT(*) FROM secure_rows),(SELECT COUNT(*) FROM secure_stubs),(SELECT COUNT(*) FROM secure_existence),(SELECT COUNT(*) FROM secure_existence_log),(SELECT COALESCE(SUM(LENGTH(record)),0) FROM secure_existence_log),(SELECT COUNT(*) FROM secure_operations),(SELECT COUNT(*) FROM secure_existence_base),(SELECT COUNT(*) FROM secure_logs),(SELECT COUNT(*) FROM secure_checkpoints),(SELECT COUNT(*) FROM secure_handles),(SELECT COUNT(*) FROM secure_outbox),(SELECT COUNT(*) FROM secure_applied)";
            let measurement = stored(&a, sql).await;
            {
                let inner = a.replica.inner.lock().await;
                collect_rows(
                    inner
                        .conn
                        .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
                        .await
                        .unwrap(),
                )
                .await
                .unwrap();
            }
            let pages = stored(&a, "PRAGMA page_count").await;
            let bytes = std::fs::metadata(dir.path().join("a.db")).unwrap().len();
            println!(
                "finalization physical plateau: cycles={cycle}; pages={pages:?}; database_bytes={bytes}"
            );
            if let Some(previous) = physical {
                assert_eq!(previous, bytes);
            }
            physical = Some(bytes);
            println!(
                "finalization churn: cycles={cycle}; rows,stubs,existence_state,existence_records,existence_bytes,operations,bases,logs,checkpoints={measurement:?}"
            );
            let metadata = stored(&a, "SELECT (SELECT COUNT(*) FROM secure_existence_archive),(SELECT COUNT(*) FROM secure_existence_archive_rows),(SELECT COUNT(*) FROM secure_checkpoint_stubs),(SELECT COUNT(*) FROM secure_checkpoint_build_stubs),(SELECT COUNT(*) FROM secure_legacy_creations),(SELECT COUNT(*) FROM secure_horizon_live_creations),(SELECT COUNT(*) FROM secure_horizon_ack),(SELECT COUNT(*) FROM secure_horizon_rejoined),(SELECT COUNT(*) FROM secure_horizon_bootstrap)").await;
            assert_eq!(
                metadata,
                vec![vec![
                    integer(1),
                    integer(0),
                    integer(0),
                    integer(0),
                    integer(0),
                    integer(0),
                    integer(2),
                    integer(0),
                    integer(0)
                ]]
            );
            println!(
                "finalization metadata plateau: cycles={cycle}; archives,archive_rows,checkpoint_proofs,building_proofs,legacy_proofs,live_scratch,acks,rejoined,bootstrap_guards={metadata:?}"
            );
            let other = stored(&b, sql).await;
            assert_eq!(other[0][..9], measurement[0][..9]);
            assert_eq!(
                measurement[0],
                vec![
                    integer(0),
                    integer(0),
                    integer(0),
                    integer(0),
                    integer(0),
                    integer(0),
                    integer(1),
                    integer(1),
                    integer(1),
                    integer(4),
                    integer(0),
                    integer(0)
                ]
            );
            if let Some(previous) = &plateau {
                assert_eq!(previous, &measurement);
            }
            plateau = Some(measurement);
        }
    }
    for holder in [&a, &b] {
        holder.replica.audit(scope).await.unwrap();
    }
}

#[tokio::test]
async fn horizon_base_conflicts_drop_but_fresh_concurrent_inserts_and_causal_reuse_converge() {
    use crate::credentials::SignedHorizon;
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 111, &TABLES)
        .await;
    let b = erp
        .open(&dir.path().join("b.db"), &PrivateKey::new(), 112, &TABLES)
        .await;
    let offline = erp
        .open(
            &dir.path().join("offline.db"),
            &PrivateKey::new(),
            113,
            &TABLES,
        )
        .await;
    let (sa, sb, so) = (
        erp.sql(&a).await,
        erp.sql(&b).await,
        erp.sql(&offline).await,
    );
    let (region, records) = commit(
        &sa,
        &[("INSERT INTO regions(name,id) VALUES('north',?id)", vec![])],
    )
    .await;
    for holder in [&b, &offline] {
        deliver(holder, &records).await;
    }
    let insert = "INSERT INTO customers(name,region,score,photo,id) VALUES(?,?,0,x'',?id)";
    let (_, stale) = commit(&so, &[(insert, vec![text("base"), integer(region[0])])]).await;
    let (base, records) = commit(&sa, &[(insert, vec![text("base"), integer(region[0])])]).await;
    deliver(&b, &records).await;
    erp.existence(&a, &b, "customers").await;
    let scope = erp.scope("customers");
    let signed = SignedHorizon::sign(
        a.replica.horizon_statement(scope, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    for holder in [&a, &b] {
        holder
            .replica
            .receive_horizon(signed.clone())
            .await
            .unwrap();
        holder.replica.finalize_horizon(scope).await.unwrap();
    }
    deliver(&a, &stale).await;
    assert_eq!(
        rows(&sa, "SELECT id FROM customers WHERE name='base'").await,
        vec![vec![integer(base[0])]]
    );
    let (_, ours) = commit(&sa, &[(insert, vec![text("fresh"), integer(region[0])])]).await;
    let (_, theirs) = commit(&sb, &[(insert, vec![text("fresh"), integer(region[0])])]).await;
    deliver(&a, &theirs).await;
    deliver(&b, &ours).await;
    erp.existence(&a, &b, "customers").await;
    erp.existence(&b, &a, "customers").await;
    assert_eq!(
        rows(&sa, "SELECT id FROM customers WHERE name='fresh'").await,
        rows(&sb, "SELECT id FROM customers WHERE name='fresh'").await
    );
    let (_, deleted) = commit(
        &sa,
        &[("DELETE FROM customers WHERE id=?", vec![integer(base[0])])],
    )
    .await;
    deliver(&b, &deleted).await;
    let (reused, records) = commit(&sa, &[(insert, vec![text("base"), integer(region[0])])]).await;
    deliver(&b, &records).await;
    erp.existence(&a, &b, "customers").await;
    // First delivery after deletion/reuse must make the same definite-drop decision.
    deliver(&b, &stale).await;
    assert_eq!(
        rows(&sa, "SELECT id FROM customers WHERE name='base'").await,
        vec![vec![integer(reused[0])]]
    );
    assert_eq!(
        a.replica.checkpoint(scope).await.unwrap().statement.rows,
        b.replica.checkpoint(scope).await.unwrap().statement.rows
    );
    for holder in [&a, &b] {
        holder.replica.audit(scope).await.unwrap();
    }
}

#[tokio::test]
async fn horizon_expired_child_rebootstraps_authenticated_base_without_parent_keys_or_resurrection()
{
    use crate::credentials::{HORIZON_WAIT_SECONDS, SignedHorizon};
    use tokio::net::{TcpListener, TcpStream};
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let full = erp
        .open(
            &dir.path().join("full.db"),
            &PrivateKey::new(),
            121,
            &TABLES,
        )
        .await;
    let offline = erp
        .open(
            &dir.path().join("offline.db"),
            &PrivateKey::new(),
            122,
            &["items"],
        )
        .await;
    let fresh = erp
        .open(
            &dir.path().join("fresh.db"),
            &PrivateKey::new(),
            123,
            &["items"],
        )
        .await;
    let sql = erp.sql(&full).await;
    let (lists, _) = commit(
        &sql,
        &[
            ("INSERT INTO lists(title,id) VALUES('live',?id)", vec![]),
            ("INSERT INTO lists(title,id) VALUES('dead',?id)", vec![]),
        ],
    )
    .await;
    let (_, items) = commit(
        &sql,
        &[
            (
                "INSERT INTO items(list,code,id) VALUES(?,'milk',?id)",
                vec![integer(lists[0])],
            ),
            (
                "INSERT INTO items(list,code,id) VALUES(?,'orphan',?id)",
                vec![integer(lists[1])],
            ),
        ],
    )
    .await;
    deliver(&offline, &items).await;
    erp.existence(&full, &offline, "lists").await;
    commit(
        &sql,
        &[("DELETE FROM lists WHERE id=?", vec![integer(lists[1])])],
    )
    .await;
    let scope = erp.scope("items");
    let mut statement = full
        .replica
        .horizon_statement(
            scope,
            &[(offline.node.public_key(), offline.credentials.clone())],
        )
        .await
        .unwrap();
    statement.issued = crate::now().unwrap() - HORIZON_WAIT_SECONDS - 1;
    let signed = SignedHorizon::sign(statement, &erp.publisher).unwrap();
    full.replica.receive_horizon(signed.clone()).await.unwrap();
    // The offline holder has not learned the declaration yet.
    assert!(
        !offline
            .replica
            .horizon_rebootstrap_required(scope)
            .await
            .unwrap()
    );
    let base = full.replica.finalize_horizon(scope).await.unwrap();
    assert_eq!(base.statement.rows.count, 1);
    full.replica
        .set_retention_policy(
            scope,
            crate::RetentionPolicy {
                required: BTreeSet::from([offline.node.public_key()]),
                behind: BTreeSet::new(),
                copies: 1,
                window: 0,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        full.replica.prune(scope).await.unwrap().blocked,
        Some(crate::PruneBlock::HorizonCustody { .. })
    ));
    for (is_offline, mut child) in [(false, fresh), (true, offline)] {
        if is_offline {
            assert_eq!(
                stored(&full, "SELECT COUNT(*) FROM secure_existence_log").await,
                vec![vec![integer(0)]]
            );
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let source = full.replica.clone();
        let key = full.node.clone();
        let credentials = full.credentials.clone();
        let peer = child.node.public_key();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut session =
                NativeSession::authenticate(stream, key, credentials, peer, MAX_SECURE_BYTES)
                    .await
                    .unwrap();
            while session.serve_once(&source).await.is_ok() {}
        });
        let mut session = NativeSession::authenticate(
            TcpStream::connect(addr).await.unwrap(),
            child.node.clone(),
            child.credentials.clone(),
            full.node.public_key(),
            MAX_SECURE_BYTES,
        )
        .await
        .unwrap();
        if is_offline {
            assert!(
                session
                    .catch_up(&child.replica, scope, MAX_SECURE_FRAME / 2)
                    .await
                    .unwrap()
                    .behind
            );
            assert!(
                child
                    .replica
                    .horizon_rebootstrap_required(scope)
                    .await
                    .unwrap()
            );
            child.replica.flush().await.unwrap();
            let node = child.node.clone();
            drop(child);
            child = erp
                .open(&dir.path().join("offline.db"), &node, 122, &["items"])
                .await;
            assert!(
                child
                    .replica
                    .horizon_rebootstrap_required(scope)
                    .await
                    .unwrap()
            );
            assert!(child.replica.acknowledge_horizon(scope).await.is_err());
            let sql = erp.sql(&child).await;
            assert!(
                sql.run(
                    "INSERT INTO items(list,code,id) VALUES(?,'blocked',?id)",
                    vec![integer(lists[0])]
                )
                .await
                .is_err()
            );
        }
        let result = session
            .sync_scope(&child.replica, scope, MAX_SECURE_FRAME / 2)
            .await
            .unwrap();
        assert!(result.rebootstrap.is_some());
        assert!(
            !child
                .replica
                .horizon_rebootstrap_required(scope)
                .await
                .unwrap()
        );
        assert_eq!(
            child
                .replica
                .checkpoint(scope)
                .await
                .unwrap()
                .statement
                .rows,
            base.statement.rows
        );
        assert_eq!(
            child
                .replica
                .existence(erp.scope("lists"), lists[1])
                .await
                .unwrap(),
            Some(false)
        );
        let csql = erp.sql(&child).await;
        assert_eq!(
            rows(&csql, "SELECT code FROM items").await,
            vec![vec![text("milk")]]
        );
        child.replica.audit(scope).await.unwrap();
        drop(session);
        server.await.unwrap();
        if !is_offline {
            // Expiry exempts the offline required peer, but a durable operation copy
            // is still required independently of the finalized-base receipt.
            for frame in &items {
                full.replica
                    .acknowledge(
                        child.node.public_key(),
                        &child.credentials,
                        frame.id().unwrap(),
                        true,
                    )
                    .await
                    .unwrap();
            }
        }
        let report = full.replica.prune(scope).await.unwrap();
        assert_eq!(report.blocked, None);
    }
}

#[tokio::test]
async fn horizon_existence_base_pages_resume_refuse_tampering_and_preserve_unknown_deletes() {
    use crate::credentials::SignedHorizon;
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let source = erp
        .open(
            &dir.path().join("source.db"),
            &PrivateKey::new(),
            141,
            &TABLES,
        )
        .await;
    let node = PrivateKey::new();
    let path = dir.path().join("child.db");
    let target = erp.open(&path, &node, 142, &["items"]).await;
    let sql = erp.sql(&source).await;
    let mut ids = Vec::new();
    for batch in 0..17 {
        let count = if batch == 16 { 4 } else { 16 };
        let changes = vec![("INSERT INTO lists(title,id) VALUES('live',?id)", vec![]); count];
        ids.extend(commit(&sql, &changes).await.0);
    }
    let scope = erp.scope("lists");
    let (key, credentials) = erp.pseudonym(
        145,
        vec![erp.grant("lists", &[Right::Insert, Right::Delete])],
    );
    let unknown = rocky_core::RowIdentity::new(145, 9).unwrap().integer();
    let deleted = ExistenceRecord::forge(
        &key,
        credentials.clone(),
        erp.manifest_hash(),
        scope,
        unknown,
        ExistenceEvent::Deleted,
        145,
        1,
        None,
    );
    source
        .replica
        .receive_existence(deleted.clone())
        .await
        .unwrap();
    let signed = SignedHorizon::sign(
        source.replica.horizon_statement(scope, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    source.replica.receive_horizon(signed).await.unwrap();
    source.replica.finalize_horizon(scope).await.unwrap();
    let base = source
        .replica
        .existence_base_offer(target.node.public_key(), &target.credentials, scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(base.descriptor().unwrap().rows, 261);
    let id = base.id().unwrap();
    target
        .replica
        .begin_existence_base(base.clone())
        .await
        .unwrap();
    let first = source
        .replica
        .existence_base_page(
            target.node.public_key(),
            &target.credentials,
            scope,
            id,
            None,
            64,
        )
        .await
        .unwrap();
    assert_eq!(first.rows.len(), 64);
    let cursor = first.next;
    let mut bad = first.clone();
    bad.base = [9; 32];
    assert!(
        target
            .replica
            .stage_existence_base(scope, bad)
            .await
            .is_err()
    );
    target
        .replica
        .stage_existence_base(scope, first.clone())
        .await
        .unwrap();
    assert!(
        target
            .replica
            .stage_existence_base(scope, first)
            .await
            .is_err()
    );
    assert!(target.replica.finish_existence_base(scope).await.is_err());
    // Flush and reopen a partial transfer; the installed state is still absent.
    target.replica.inner.lock().await.flush().unwrap();
    drop(target);
    let target = erp.open(&path, &node, 142, &["items"]).await;
    assert_eq!(
        target
            .replica
            .begin_existence_base(base.clone())
            .await
            .unwrap(),
        (cursor, false)
    );
    let mut after = cursor;
    loop {
        let page = source
            .replica
            .existence_base_page(
                target.node.public_key(),
                &target.credentials,
                scope,
                id,
                after,
                64,
            )
            .await
            .unwrap();
        assert!(page.rows.len() <= 64);
        after = page.next;
        target
            .replica
            .stage_existence_base(scope, page)
            .await
            .unwrap();
        if after.is_none() {
            break;
        }
    }
    target.replica.finish_existence_base(scope).await.unwrap();
    for row in ids {
        assert_eq!(
            target.replica.existence(scope, row).await.unwrap(),
            Some(true)
        );
    }
    assert_eq!(
        target.replica.existence(scope, unknown).await.unwrap(),
        Some(false)
    );
    // A creation first seen after the base must not undo a prior unknown deletion.
    let created = ExistenceRecord::forge(
        &key,
        credentials,
        erp.manifest_hash(),
        scope,
        unknown,
        ExistenceEvent::Created,
        145,
        2,
        Some(deleted.hash().unwrap()),
    );
    assert_eq!(
        target.replica.receive_existence(created).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(
        target.replica.existence(scope, unknown).await.unwrap(),
        Some(false)
    );
    assert_eq!(
        target.replica.receive_existence(deleted).await.unwrap(),
        ReceiveStatus::Pruned
    );
    // A complete page set with altered live hashes never switches the installed base.
    let other = erp
        .open(
            &dir.path().join("bad.db"),
            &PrivateKey::new(),
            143,
            &["items"],
        )
        .await;
    other.replica.begin_existence_base(base).await.unwrap();
    let mut after = None;
    loop {
        let mut page = source
            .replica
            .existence_base_page(
                other.node.public_key(),
                &other.credentials,
                scope,
                id,
                after,
                256,
            )
            .await
            .unwrap();
        if after.is_none() {
            page.rows[0].1 = Some([0; 32]);
        }
        after = page.next;
        other
            .replica
            .stage_existence_base(scope, page)
            .await
            .unwrap();
        if after.is_none() {
            break;
        }
    }
    assert!(other.replica.finish_existence_base(scope).await.is_err());
    assert!(
        other
            .replica
            .installed_existence_base(scope)
            .await
            .unwrap()
            .is_none()
    );
    other.replica.abort_existence_base(scope).await.unwrap();
}

#[tokio::test]
async fn horizon_legacy_creation_proofs_bootstrap_in_pages_and_release_pending_old_edits() {
    use crate::credentials::SignedHorizon;
    use tokio::net::{TcpListener, TcpStream};
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let source = erp
        .open(
            &dir.path().join("source.db"),
            &PrivateKey::new(),
            151,
            &TABLES,
        )
        .await;
    let target = erp
        .open(
            &dir.path().join("target.db"),
            &PrivateKey::new(),
            152,
            &["lists"],
        )
        .await;
    let scope = erp.scope("lists");
    let (key, credentials) = erp.pseudonym(
        151,
        vec![erp.grant("lists", &[Right::Insert, Right::Update, Right::Delete])],
    );
    let app = TypedRows::new(erp.schema.clone(), vec![erp.domain.domain]).unwrap();
    let table = app
        .tables()
        .find(|table| table.sql_name == "lists")
        .unwrap();
    let values = vec![
        text("old"),
        integer(rocky_core::RowIdentity::new(151, 1).unwrap().integer()),
        integer(0),
    ];
    let fields = app.fields(table, &values).unwrap();
    let row = app.row_id(erp.domain.domain, table, &values).unwrap();
    let change = RowChange {
        scope,
        row,
        before: None,
        after: Some(fields.clone()),
    };
    let row_key = source
        .replica
        .inner
        .lock()
        .await
        .config
        .keys
        .iter()
        .find(|key| key.scope == scope)
        .unwrap()
        .key;
    let extension = crate::wire::Extensions {
        manifest: erp.manifest_hash(),
        scope,
        generation: 1,
        credentials: credentials.clone(),
        author: Some(151),
        row_format: 2,
        partition: None,
    };
    let created = crate::wire::author(
        &key,
        extension.clone(),
        &row_key,
        (0, None, 1),
        vec![],
        &crate::encode(&vec![change]).unwrap(),
    )
    .unwrap();
    source.replica.receive(created.clone()).await.unwrap();
    let deleted = crate::wire::author(
        &key,
        extension,
        &row_key,
        (1, Some(created.id().unwrap()), 2),
        vec![],
        &crate::encode(&vec![RowChange {
            scope,
            row,
            before: Some(fields.clone()),
            after: None,
        }])
        .unwrap(),
    )
    .unwrap();
    source.replica.receive(deleted).await.unwrap();
    let signed = SignedHorizon::sign(
        source.replica.horizon_statement(scope, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    source.replica.receive_horizon(signed).await.unwrap();
    let checkpoint = source.replica.finalize_horizon(scope).await.unwrap();
    assert_eq!(checkpoint.statement.rows.count, 0);
    assert_eq!(checkpoint.statement.creations.count, 1);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let replica = source.replica.clone();
    let node = source.node.clone();
    let chain = source.credentials.clone();
    let peer = target.node.public_key();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut session = NativeSession::authenticate(stream, node, chain, peer, MAX_SECURE_BYTES)
            .await
            .unwrap();
        while session.serve_once(&replica).await.is_ok() {}
    });
    let mut session = NativeSession::authenticate(
        TcpStream::connect(addr).await.unwrap(),
        target.node.clone(),
        target.credentials.clone(),
        source.node.public_key(),
        MAX_SECURE_BYTES,
    )
    .await
    .unwrap();
    assert!(
        session
            .sync_scope(&target.replica, scope, MAX_SECURE_FRAME / 2)
            .await
            .unwrap()
            .rebootstrap
            .is_some()
    );
    drop(session);
    server.await.unwrap();
    assert_eq!(
        target.replica.receive(created.clone()).await.unwrap(),
        ReceiveStatus::Pruned
    );
    let (other, chain) = erp.pseudonym(
        153,
        vec![erp.grant("lists", &[Right::Insert, Right::Update, Right::Delete])],
    );
    let mut changed = values.clone();
    changed[0] = text("late");
    let extension = crate::wire::Extensions {
        manifest: erp.manifest_hash(),
        scope,
        generation: 1,
        credentials: chain,
        author: Some(153),
        row_format: 2,
        partition: None,
    };
    let edit = crate::wire::author(
        &other,
        extension.clone(),
        &row_key,
        (0, None, 3),
        vec![created.id().unwrap()],
        &crate::encode(&vec![RowChange {
            scope,
            row,
            before: Some(fields),
            after: Some(app.fields(table, &changed).unwrap()),
        }])
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        target.replica.receive(edit.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    changed[1] = integer(rocky_core::RowIdentity::new(153, 1).unwrap().integer());
    let insert = RowChange {
        scope,
        row: app.row_id(erp.domain.domain, table, &changed).unwrap(),
        before: None,
        after: Some(app.fields(table, &changed).unwrap()),
    };
    let continued = crate::wire::author(
        &other,
        crate::wire::Extensions {
            row_format: 4,
            ..extension
        },
        &row_key,
        (1, Some(edit.id().unwrap()), 4),
        vec![],
        &crate::wire::RowPayload::witness(
            crate::encode(&vec![insert]).unwrap(),
            vec![],
            checkpoint
                .statement
                .finalized
                .as_ref()
                .map(|signed| (signed.statement.sequence, signed.id().unwrap())),
            vec![],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        target.replica.receive(continued).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(target.replica.snapshot(scope).await.unwrap().rows.len(), 1);
    target.replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn horizon_parent_loser_base_finalizes_child_only_orphans_identically() {
    use crate::credentials::SignedHorizon;
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 151, &TABLES)
        .await;
    let b = erp
        .open(&dir.path().join("b.db"), &PrivateKey::new(), 152, &TABLES)
        .await;
    let child = erp
        .open(
            &dir.path().join("child.db"),
            &PrivateKey::new(),
            153,
            &["orders"],
        )
        .await;
    let sa = erp.sql(&a).await;
    let sb = erp.sql(&b).await;
    let (region, created) = commit(
        &sa,
        &[("INSERT INTO regions(name,id) VALUES('north',?id)", vec![])],
    )
    .await;
    deliver(&b, &created).await;
    let insert = "INSERT INTO customers(name,region,score,photo,id) VALUES('same',?,0,x'',?id)";
    let (ours, af) = commit(&sa, &[(insert, vec![integer(region[0])])]).await;
    let (theirs, bf) = commit(&sb, &[(insert, vec![integer(region[0])])]).await;
    let (_, ao) = commit(
        &sa,
        &[(
            "INSERT INTO orders(customer,label,id) VALUES(?,'ours',?id)",
            vec![integer(ours[0])],
        )],
    )
    .await;
    let (_, bo) = commit(
        &sb,
        &[(
            "INSERT INTO orders(customer,label,id) VALUES(?,'theirs',?id)",
            vec![integer(theirs[0])],
        )],
    )
    .await;
    deliver(&a, &bf).await;
    deliver(&b, &af).await;
    deliver(&a, &bo).await;
    deliver(&b, &ao).await;
    deliver(&child, &ao).await;
    deliver(&child, &bo).await;
    for (source, target) in [(&a, &b), (&b, &a), (&a, &child), (&b, &child)] {
        erp.existence(source, target, "customers").await;
    }
    let parent = erp.scope("customers");
    let scope = erp.scope("orders");
    let signed = SignedHorizon::sign(
        a.replica.horizon_statement(parent, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    for holder in [&a, &b] {
        holder
            .replica
            .receive_horizon(signed.clone())
            .await
            .unwrap();
        holder.replica.finalize_horizon(parent).await.unwrap();
    }
    let winner = rows(&sa, "SELECT id FROM customers WHERE row_kind=0").await[0][0].clone();
    let loser = if winner == integer(ours[0]) {
        theirs[0]
    } else {
        ours[0]
    };
    assert_eq!(
        a.replica.existence(parent, loser).await.unwrap(),
        Some(false)
    );
    assert_eq!(
        child.replica.existence(parent, loser).await.unwrap(),
        Some(true)
    );
    let signed = SignedHorizon::sign(
        a.replica
            .horizon_statement(
                scope,
                &[(child.node.public_key(), child.credentials.clone())],
            )
            .await
            .unwrap(),
        &erp.publisher,
    )
    .unwrap();
    child.replica.receive_horizon(signed.clone()).await.unwrap();
    assert!(child.replica.acknowledge_horizon(scope).await.is_err());
    assert!(child.replica.finalize_horizon(scope).await.is_err());
    let base = a
        .replica
        .existence_base_offer(child.node.public_key(), &child.credentials, parent)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(base.descriptor().unwrap().rows, 1);
    let id = base.id().unwrap();
    child.replica.begin_existence_base(base).await.unwrap();
    let page = a
        .replica
        .existence_base_page(
            child.node.public_key(),
            &child.credentials,
            parent,
            id,
            None,
            256,
        )
        .await
        .unwrap();
    child
        .replica
        .stage_existence_base(parent, page)
        .await
        .unwrap();
    child.replica.finish_existence_base(parent).await.unwrap();
    let ack = child.replica.acknowledge_horizon(scope).await.unwrap();
    let mut root = None;
    for holder in [&a, &b, &child] {
        holder
            .replica
            .receive_horizon(signed.clone())
            .await
            .unwrap();
        holder
            .replica
            .receive_horizon_ack(scope, ack.clone())
            .await
            .unwrap();
        let checkpoint = holder.replica.finalize_horizon(scope).await.unwrap();
        assert_eq!(checkpoint.statement.rows.count, 1);
        if let Some(previous) = &root {
            assert_eq!(previous, &checkpoint.statement.rows);
        }
        root = Some(checkpoint.statement.rows);
        let sql = erp.sql(holder).await;
        assert_eq!(
            rows(&sql, "SELECT customer FROM orders").await,
            vec![vec![winner.clone()]]
        );
        assert_eq!(
            holder.replica.existence(parent, loser).await.unwrap(),
            Some(false)
        );
        holder.replica.audit(scope).await.unwrap();
    }
}

#[tokio::test]
async fn horizon_old_inserts_stay_dropped_across_successors_and_future_frames_wait() {
    use crate::credentials::SignedHorizon;
    async fn import(
        source: &Device,
        target: &Device,
        scope: ResourceScope,
        signed: crate::SignedCheckpoint,
    ) {
        for descriptor in &signed.statement.finalized.as_ref().unwrap().statement.bases {
            let base = source
                .replica
                .installed_existence_base(descriptor.scope)
                .await
                .unwrap()
                .unwrap();
            let id = base.id().unwrap();
            target.replica.begin_existence_base(base).await.unwrap();
            let mut after = None;
            loop {
                let page = source
                    .replica
                    .existence_base_page(
                        target.node.public_key(),
                        &target.credentials,
                        descriptor.scope,
                        id,
                        after,
                        256,
                    )
                    .await
                    .unwrap();
                after = page.next;
                target
                    .replica
                    .stage_existence_base(descriptor.scope, page)
                    .await
                    .unwrap();
                if after.is_none() {
                    break;
                }
            }
            target
                .replica
                .finish_existence_base(descriptor.scope)
                .await
                .unwrap();
        }
        let id = signed.id().unwrap();
        target.replica.begin_rebootstrap(signed).await.unwrap();
        let mut after = None;
        loop {
            let page = source
                .replica
                .checkpoint_page(
                    target.node.public_key(),
                    &target.credentials,
                    scope,
                    id,
                    after,
                    crate::MAX_SECURE_FRAME / 2,
                )
                .await
                .unwrap();
            after = page.next;
            target.replica.stage_checkpoint_page(page).await.unwrap();
            if after.is_none() {
                break;
            }
        }
        let report = target.replica.finish_rebootstrap(scope).await.unwrap();
        assert!(report.rejected.is_empty());
        assert!(report.pending.is_empty());
    }
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 161, &TABLES)
        .await;
    let b = erp
        .open(&dir.path().join("b.db"), &PrivateKey::new(), 162, &TABLES)
        .await;
    let c = erp
        .open(&dir.path().join("c.db"), &PrivateKey::new(), 163, &TABLES)
        .await;
    let sa = erp.sql(&a).await;
    let sb = erp.sql(&b).await;
    let (region, created) = commit(
        &sa,
        &[("INSERT INTO regions(name,id) VALUES('north',?id)", vec![])],
    )
    .await;
    for holder in [&b, &c] {
        deliver(holder, &created).await;
    }
    let insert = "INSERT INTO customers(name,region,score,photo,id) VALUES(?,?,0,x'',?id)";
    let (baseline, _) = commit(&sa, &[(insert, vec![text("same"), integer(region[0])])]).await;
    let (_, stale) = commit(&sb, &[(insert, vec![text("same"), integer(region[0])])]).await;
    let scope = erp.scope("customers");
    let signed = SignedHorizon::sign(
        a.replica.horizon_statement(scope, &[]).await.unwrap(),
        &erp.publisher,
    )
    .unwrap();
    a.replica.receive_horizon(signed).await.unwrap();
    let first = a.replica.finalize_horizon(scope).await.unwrap();
    import(&a, &b, scope, first.clone()).await;
    import(&a, &c, scope, first).await;
    assert_eq!(
        c.replica.receive(stale[0].clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    commit(
        &sa,
        &[(
            "DELETE FROM customers WHERE id=?",
            vec![integer(baseline[0])],
        )],
    )
    .await;
    let mut latest = None;
    for expected in [2, 3] {
        let signed = SignedHorizon::sign(
            a.replica.horizon_statement(scope, &[]).await.unwrap(),
            &erp.publisher,
        )
        .unwrap();
        assert_eq!(signed.statement.sequence, expected);
        a.replica.receive_horizon(signed).await.unwrap();
        latest = Some(a.replica.finalize_horizon(scope).await.unwrap());
    }
    let latest = latest.unwrap();
    import(&a, &b, scope, latest.clone()).await;
    assert!(
        rows(&sb, "SELECT id FROM customers WHERE row_kind=0")
            .await
            .is_empty()
    );
    assert_eq!(
        a.replica.receive(stale[0].clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    let (_, fresh) = commit(&sb, &[(insert, vec![text("fresh"), integer(region[0])])]).await;
    assert_eq!(
        fresh[0].operation().unwrap().header.backlink,
        Some(stale[0].id().unwrap())
    );
    // C holds cut 1 and knows the predecessor: only the missing issuer cut blocks this birth.
    assert_eq!(
        c.replica.receive(fresh[0].clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    import(&a, &c, scope, latest).await;
    deliver(&a, &fresh).await;
    for holder in [&a, &c] {
        erp.existence(&b, holder, "customers").await;
    }
    let root = b.replica.checkpoint(scope).await.unwrap().statement.rows;
    for holder in [&a, &b, &c] {
        let sql = erp.sql(holder).await;
        assert_eq!(
            rows(&sql, "SELECT name FROM customers WHERE row_kind=0").await,
            vec![vec![text("fresh")]]
        );
        assert_eq!(
            holder
                .replica
                .checkpoint(scope)
                .await
                .unwrap()
                .statement
                .rows,
            root
        );
        holder.replica.audit(scope).await.unwrap();
    }
}
