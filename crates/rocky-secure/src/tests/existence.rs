//! Existence logs, puppets and virtual rows: foreign keys across reader
//! scopes on devices that hold the child only. See
//! `docs/runtime/existence-logs.md`.
use super::*;
use crate::sql::{SqlError, SqlOptions};
use crate::storage::collect_rows;
use crate::{ExistenceEvent, ExistenceRecord, ReceiveStatus, SecureSql};
use rocky_core::{DomainId, LogicalSchema, Scope, SqlValue, TypedRows};

const DEFINITION: &str = r#"{
    "format": 6, "publisher": "PUBLISHER", "application": "erp",
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
    policy: RevocationSet,
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
            policy: RevocationSet::new(),
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
    fn credentials(
        &self,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
        number: u32,
    ) -> CredentialChain {
        let user = PrivateKey::new();
        let (not_before, expires) = crate::tests::lifetime();
        let capability = |subject, kind, delegations| Capability {
            subject,
            kind,
            grants: grants.clone(),
            not_before,
            expires,
            delegations,
            extension: rocky_credentials::Extension::new(rocky_credentials::Extended {
                authors: Some(if kind == PrincipalKind::Node {
                    rocky_credentials::AuthorRange::new(number, number).unwrap()
                } else {
                    rocky_credentials::AuthorRange::ALL
                }),
                node: (kind == PrincipalKind::Node).then(|| rocky_credentials::PeerId::of(subject)),
                ..Default::default()
            })
            .unwrap(),
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
    /// A node `number` with `grants`, all on one table.
    fn certified_node(
        &self,
        number: u32,
        grants: Vec<ResourceGrant>,
    ) -> (PrivateKey, CredentialChain) {
        let key = PrivateKey::new();
        let assigner = PrivateKey::new();
        let issued = crate::credentials::Authority::owner(&self.publisher)
            .issue_terms(
                assigner.public_key(),
                grants.clone(),
                (1, Some(crate::credentials::AuthorRange::ALL)),
                crate::tests::validity(),
            )
            .unwrap();
        let chain = CredentialChain(vec![issued])
            .complete(&assigner, key.public_key(), number)
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
        let credentials = self.credentials(node, self.grants(tables), number);
        let replica = EncryptedReplica::open(
            path,
            TypedRows::new(self.schema.clone(), vec![DomainId([1; 16])]).unwrap(),
            SecureConfig {
                publisher: self.publisher.public_key(),
                manifest: self.manifest.clone(),
                revocations: self.policy.clone(),
                node: node.clone(),
                credentials: credentials.clone(),
                keys: tables
                    .iter()
                    .map(|table| {
                        KeyMaterial::new(
                            self.scope(table),
                            1,
                            rocky_core::commitment("erp.key", table.as_bytes()),
                        )
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
        let mut after = None;
        loop {
            let page = from
                .replica
                .existence_page(
                    to.node.public_key(),
                    &to.credentials,
                    scope,
                    &to.replica.existence_frontier(scope).await.unwrap(),
                    after,
                    crate::MAX_EXISTENCE_PAGE,
                )
                .await
                .unwrap();
            after = page.next;
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
            if after.is_none() {
                break;
            }
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
    let inner = device
        .replica
        .inner
        .lock(crate::replica::Class::Background)
        .await;
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
    let held = stored(&b, "SELECT DISTINCT scope FROM secure_row_records").await;
    assert_eq!(
        held,
        vec![vec![SqlValue::Blob(encode(&erp.scope("items")).unwrap())]]
    );
    // Both hold the same items rows.
    let scope = erp.scope("items");
    assert_eq!(
        a.replica.snapshot(scope).await.unwrap().rows,
        b.replica.snapshot(scope).await.unwrap().rows
    );
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
    let (inserter, chain) = erp.certified_node(40, vec![erp.grant("lists", &[Right::Insert])]);
    let forged = |event, id, sequence, previous| {
        ExistenceRecord::forge(
            &inserter,
            chain.clone(),
            rocky_credentials::genesis_tip(manifest),
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
    let (items_key, items_chain) = erp.certified_node(41, erp.grants(&["items"]));
    let outside = ExistenceRecord::forge(
        &items_key,
        items_chain,
        rocky_credentials::genesis_tip(manifest),
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
async fn link_sessions_exchange_existence_logs() {
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
    let (near, far) = network::link::testing::tcp().await;
    let (mut session, mut served) = network::link::testing::sessions_over(
        near,
        far,
        (&b.replica, b.node.clone(), b.credentials.clone()),
        (&a.replica, a.node.clone(), a.credentials.clone()),
    )
    .await
    .unwrap();
    let replica = a.replica.clone();
    let server = tokio::spawn(async move { while served.serve_once(&replica).await.is_ok() {} });
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
        let mut inner = a
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        transaction!(inner, |inner| inner.clear_typed(customers).await).unwrap();
    }
    let listed = "SELECT id FROM orders ORDER BY id";
    assert_eq!(rows(&sa, listed).await, orders);
    assert!(rows(&sa, "SELECT id FROM customers").await.is_empty());
    a.replica.rebuild_projection(customers).await.unwrap();
    assert_eq!(rows(&sa, names).await.len(), 1);
    assert_eq!(rows(&sa, listed).await, orders);
}

mod scan_order;
mod seals;

#[tokio::test]
async fn warmed_header_scans_keep_existence_and_placement_counts_distinct() {
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let device = erp
        .open(
            &dir.path().join("header-counts.db"),
            &PrivateKey::new(),
            11,
            &TABLES,
        )
        .await;
    let (_, frames) = commit(
        &erp.sql(&device).await,
        &[("INSERT INTO lists(title,id) VALUES('counted',?id)", vec![])],
    )
    .await;
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    let header: p2panda_core::Header<crate::wire::Extensions> =
        crate::decode(frame.header_bytes()).unwrap();
    let author = header.extensions.author.unwrap();
    let position = header.extensions.author_sequence;
    assert_eq!(header.extensions.existence, 1);
    let scope = erp.scope("lists");
    let scopes = [(scope, None)];
    let held = BTreeSet::from([scope]);
    let inner = device
        .replica
        .inner
        .lock(crate::replica::Class::Background)
        .await;
    inner.stored_header_memo_limits(8, 1 << 20);
    let warm = inner
        .scan_positions_for_test(erp.domain, &scopes, &held, &BTreeSet::new())
        .await
        .unwrap();
    assert!(warm.2.is_empty());
    let stats = inner.stored_header_memo_stats();
    let counted = inner
        .scan_positions_for_test(erp.domain, &scopes, &held, &held)
        .await
        .unwrap();
    assert_eq!(counted.0[&author], BTreeSet::from([position]));
    assert_eq!(counted.1[&(scope, author)], position);
    assert_eq!(counted.2[&(scope, author)][&position], 1);
    assert_eq!(inner.stored_header_memo_stats(), stats);
    inner
        .conn
        .execute("SAVEPOINT counted_header", ())
        .await
        .unwrap();
    inner.conn.execute("INSERT INTO secure_voids(scope,author,sequence,hash,header) SELECT scope,author,sequence,hash,header FROM secure_operations", ()).await.unwrap();
    inner
        .conn
        .execute("DELETE FROM secure_operations", ())
        .await
        .unwrap();
    assert_eq!(
        inner
            .scan_positions_for_test(erp.domain, &scopes, &held, &held)
            .await
            .unwrap(),
        counted
    );
    inner
        .conn
        .execute("DELETE FROM secure_voids", ())
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "INSERT INTO secure_placements VALUES(?,?,?,?,?,?)",
            crate::storage::bindings(vec![
                SqlValue::Blob(crate::encode(&erp.domain).unwrap()),
                SqlValue::Integer(i64::from(author)),
                SqlValue::Integer(position as i64),
                SqlValue::Blob(crate::encode(&scope).unwrap()),
                SqlValue::Blob(frame.id().unwrap().as_bytes().to_vec()),
                SqlValue::Blob(frame.header_bytes().to_vec()),
            ]),
        )
        .await
        .unwrap();
    let placed = inner
        .scan_positions_for_test(erp.domain, &scopes, &BTreeSet::new(), &held)
        .await
        .unwrap();
    assert_eq!(placed.0, counted.0);
    assert!(placed.1.is_empty());
    assert!(
        placed.2.is_empty(),
        "a positive signed count is not placement custody"
    );
    assert!(
        inner
            .scan_positions_for_test(erp.domain, &scopes, &held, &held)
            .await
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(inner.stored_header_memo_stats(), stats);
    inner
        .conn
        .execute("ROLLBACK TO counted_header", ())
        .await
        .unwrap();
    inner
        .conn
        .execute("RELEASE counted_header", ())
        .await
        .unwrap();
    drop(inner);
    device.replica.audit(scope).await.unwrap();
}
