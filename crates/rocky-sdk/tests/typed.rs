//! Generated typed schemas, business
//! keys and row incarnations over real replicas.
use rocky_core::{
    Application, DomainId, Envelope, Incompatibility, LogicalSchema, MeshId, NodeId, OperationId,
    PrincipalId, SchemaError, Scope, SqlValue, TableClass, UpdatePolicy,
};
use rocky_runtime::{
    BoundQuery, CheckpointPage, DomainInstallation, Error, Identity, Mutation, ReadOptions,
    Replica, Runtime, RuntimeConfig,
};
use rocky_sdk::{Access, TypedApp, TypedRow};
use std::path::{Path, PathBuf};

const SHOPPING: &str = include_str!("fixtures/shopping-descriptor.json");
const EDGE: &str = include_str!("fixtures/edge-descriptor.json");
const ITEMS: &str = "shopping_items";
const CALLER: PrincipalId = PrincipalId([7; 16]);
const DOMAIN: DomainId = DomainId([2; 16]);

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
fn real(value: f64) -> SqlValue {
    SqlValue::real(value).expect("not NaN")
}
fn nodes() -> Vec<NodeId> {
    (1..=4).map(|node| NodeId([node; 16])).collect()
}
struct Device {
    replica: Replica<TypedApp>,
    app: TypedApp,
    scope: Scope,
    path: PathBuf,
    author: u32,
    seen: std::sync::Mutex<std::collections::BTreeMap<OperationId, TypedRow>>,
}
impl Device {
    async fn open(dir: &Path, node: u8, schema: &LogicalSchema) -> Result<Self, Error> {
        let scope = Scope {
            mesh: MeshId([1; 16]),
            app: schema.app_id(),
            domain: DOMAIN,
        };
        Self::open_in(dir, node, schema, scope).await
    }
    async fn open_in(
        dir: &Path,
        node: u8,
        schema: &LogicalSchema,
        scope: Scope,
    ) -> Result<Self, Error> {
        let path = dir.join(format!("node{node}.sqlite"));
        let domain = installation(node, schema, scope)?;
        let app = domain.app.clone();
        let replica = Replica::open_with_config(
            &path,
            domain.app,
            domain.policy,
            domain.identity,
            domain.config,
        )
        .await?;
        Ok(Self {
            replica,
            app,
            scope,
            path,
            author: u32::from(node),
            seen: Default::default(),
        })
    }
    // These historical reducer scenarios supply explicit modern identities.
    fn identity_row<'a>(
        &self,
        table: &str,
        row: &[(&'a str, SqlValue)],
        current: Option<rocky_core::RowIdentity>,
    ) -> Vec<(&'a str, SqlValue)> {
        let highest = self
            .seen
            .lock()
            .unwrap()
            .values()
            .filter(|row| row.table == table)
            .filter_map(|row| row.identity)
            .filter(|id| id.author() == self.author)
            .map(rocky_core::RowIdentity::sequence)
            .max()
            .unwrap_or(0);
        let id =
            current.unwrap_or_else(|| rocky_core::RowIdentity::next(self.author, highest).unwrap());
        let mut row = row.to_vec();
        row.push(("id", SqlValue::Integer(id.integer())));
        row.push(("row_kind", SqlValue::Integer(0)));
        row
    }
    fn insert(
        &self,
        table: &str,
        row: &[(&str, SqlValue)],
        replaces: &[OperationId],
    ) -> Result<Mutation, rocky_sdk::TypedError> {
        self.app
            .insert(table, &self.identity_row(table, row, None), replaces)
    }
    fn replace(
        &self,
        rows: &[TypedRow],
        table: &str,
        row: &[(&str, SqlValue)],
    ) -> Result<Mutation, rocky_sdk::TypedError> {
        self.app
            .replace(rows, table, &self.identity_row(table, row, None))
    }
    fn upsert(
        &self,
        rows: &[TypedRow],
        table: &str,
        row: &[(&str, SqlValue)],
    ) -> Result<Option<Mutation>, rocky_sdk::TypedError> {
        let schema = self.replica.logical_schema().unwrap().unwrap();
        let key: Vec<_> = schema
            .table(table)
            .unwrap()
            .primary_key
            .iter()
            .map(|name| {
                row.iter()
                    .find(|(field, _)| *field == name)
                    .unwrap()
                    .1
                    .clone()
            })
            .collect();
        let visible = self.app.locate(rows, table, &key)?.visible;
        let current = rows
            .iter()
            .find(|row| Some(row.incarnation) == visible)
            .and_then(|row| row.identity);
        self.app
            .upsert(rows, table, &self.identity_row(table, row, current))
    }
    fn update(
        &self,
        table: &str,
        incarnation: OperationId,
        set: &[(&str, SqlValue)],
    ) -> Result<Mutation, rocky_sdk::TypedError> {
        let seen = self.seen.lock().unwrap();
        let row = seen
            .get(&incarnation)
            .expect("fixture remembers a received or authored incarnation");
        self.app
            .update_id(std::slice::from_ref(row), table, row.identity.unwrap(), set)
    }
    async fn rows(&self) -> Vec<TypedRow> {
        let rows = self.replica.query().await.expect("query").rows;
        self.seen
            .lock()
            .unwrap()
            .extend(rows.iter().map(|row| (row.incarnation, row.clone())));
        rows
    }
    async fn try_commit(&self, mutation: Mutation) -> Result<OperationId, Error> {
        let id = self.replica.mutate(mutation)?.committed().await?.id;
        self.rows().await;
        Ok(id)
    }
    async fn commit(&self, mutation: Mutation) -> OperationId {
        self.try_commit(mutation).await.expect("commit")
    }
    async fn read(&self, sql: &str) -> Vec<Vec<SqlValue>> {
        let query = BoundQuery {
            sql: sql.into(),
            values: Vec::new(),
        };
        self.replica
            .read(query, ReadOptions::new(self.scope, 1024))
            .await
            .unwrap_or_else(|error| panic!("read {sql}: {error}"))
            .rows
    }
    async fn items(&self) -> Vec<Vec<SqlValue>> {
        self.read("SELECT key,title,completed,searchTitle FROM shopping_items ORDER BY key")
            .await
    }
    async fn operations(&self) -> Vec<Vec<u8>> {
        let (mut operations, mut after) = (Vec::new(), None);
        loop {
            let page = self.replica.operation_page(after).await.expect("page");
            operations.extend(page.items);
            match page.next {
                Some(next) => after = Some(next),
                None => return operations,
            }
        }
    }
    /// Receives what this device does not hold yet, in the given order.
    async fn receive(&self, operations: Vec<Vec<u8>>) {
        let state = self.replica.sync_state().await.expect("state");
        let missing: Vec<_> = operations
            .into_iter()
            .filter(|bytes| {
                let id = Envelope::decode(bytes).expect("operation").id;
                !state.received.contains(&id)
                    && !state
                        .held
                        .0
                        .iter()
                        .any(|(node, sequence)| *node == id.node && *sequence >= id.sequence)
            })
            .collect();
        for result in self.replica.receive_page(missing).await.expect("receive") {
            result.expect("receipt");
        }
        self.replica.process_pending().await.expect("apply");
        self.rows().await;
    }
    async fn close(self) -> PathBuf {
        self.replica.close().await.expect("close");
        self.path
    }
}
/// Every device receives every other device's operations.
async fn sync(devices: &[&Device]) {
    for _ in 0..2 {
        for to in devices {
            for from in devices {
                if !std::ptr::eq(*to, *from) {
                    to.receive(from.operations().await).await;
                }
            }
        }
    }
}
fn installation(
    node: u8,
    schema: &LogicalSchema,
    scope: Scope,
) -> Result<DomainInstallation<TypedApp>, Error> {
    let mut app = TypedApp::new(schema.clone(), &[DOMAIN])
        .map_err(|error| Error::Incompatible(error.to_string()))?;
    if [
        rocky_core::LOGICAL_SCHEMA_FORMAT,
        rocky_core::LOGICAL_SCHEMA_FORMAT,
    ]
    .contains(&schema.format)
    {
        for node in nodes() {
            for table in schema.mergeable() {
                app = app
                    .with_row_author(node, &table.sql_name, u32::from(node.0[0]))
                    .map_err(|error| Error::Incompatible(error.to_string()))?;
            }
        }
    }
    Ok(DomainInstallation {
        app,
        policy: Access {
            scope,
            caller: CALLER,
            nodes: nodes(),
        },
        identity: Identity {
            scope,
            node: NodeId([node; 16]),
            caller: CALLER,
        },
        config: RuntimeConfig {
            authors: nodes().into_iter().map(|node| (node, CALLER)).collect(),
            delivery_targets: nodes(),
            ..RuntimeConfig::default()
        },
    })
}
fn item(id: &str, title: &str, completed: bool) -> [(&'static str, SqlValue); 3] {
    [
        ("key", text(id)),
        ("title", text(title)),
        ("completed", SqlValue::Integer(i64::from(completed))),
    ]
}
fn shown(id: &str, title: &str, completed: bool) -> Vec<SqlValue> {
    vec![
        text(id),
        text(title),
        SqlValue::Integer(i64::from(completed)),
        text(&title.to_ascii_lowercase()),
    ]
}
fn shopping() -> LogicalSchema {
    LogicalSchema::parse(SHOPPING.as_bytes()).expect("shopping schema")
}

#[tokio::test]
async fn the_shopping_schema_is_generated_installed_and_bound_across_restart() {
    let schema = shopping();
    // One synced table; Room bookkeeping is never installed.
    assert_eq!(schema.tables.len(), 1);
    let table = &schema.tables[0];
    assert_eq!(
        (table.sql_name.as_str(), table.class),
        (ITEMS, TableClass::Mergeable)
    );
    let columns: Vec<_> = table.columns.iter().map(|c| c.sql_name.as_str()).collect();
    assert_eq!(
        columns,
        ["key", "title", "completed", "searchTitle", "id", "row_kind"]
    );
    assert_eq!(table.columns[3].default.as_deref(), Some("''"));
    assert_eq!(
        table
            .indexes
            .iter()
            .find(|index| index.name == "index_shopping_items_completed_title")
            .unwrap()
            .name,
        "index_shopping_items_completed_title"
    );
    assert_eq!(schema.source.identity, "3f3baae50e56ddfb547ad32260e40a1d");
    assert_eq!(
        (schema.source.version, schema.update_policy),
        (2, UpdatePolicy::Changed)
    );
    schema.installable().expect("installable");
    assert_eq!(
        LogicalSchema::parse(&schema.canonical().expect("canonical")).expect("parse"),
        schema
    );

    let dir = tempfile::tempdir().expect("dir");
    let device = Device::open(dir.path(), 1, &schema).await.expect("open");
    // Introspection is exactly the descriptor.
    assert_eq!(
        *device
            .replica
            .logical_schema()
            .expect("schema")
            .expect("bound"),
        schema
    );
    let milk = device
        .commit(
            device
                .insert(ITEMS, &item("a", "Oat Milk", false), &[])
                .expect("insert"),
        )
        .await;
    assert_eq!(device.items().await, vec![shown("a", "Oat Milk", false)]);
    // One operation sets both columns; the derived column follows the title.
    let edit = [
        ("title", text("Soy Milk")),
        ("completed", SqlValue::Integer(1)),
    ];
    device
        .commit(device.update(ITEMS, milk, &edit).expect("update"))
        .await;
    assert_eq!(device.items().await, vec![shown("a", "Soy Milk", true)]);
    // The typed table is indexed as Room declared it.
    let plan = device
        .replica
        .explain_read(
            BoundQuery {
                sql: "SELECT key FROM shopping_items WHERE completed=1 ORDER BY title".into(),
                values: Vec::new(),
            },
            ReadOptions::new(device.scope, 10),
        )
        .await
        .expect("plan");
    assert!(
        plan.iter()
            .any(|step| step.contains("index_shopping_items_completed_title"))
    );
    // Protocol state is not readable.
    let hidden = BoundQuery {
        sql: "SELECT tbl FROM rocky_incarnations".into(),
        values: Vec::new(),
    };
    assert!(
        device
            .replica
            .read(hidden, ReadOptions::new(device.scope, 10))
            .await
            .is_err()
    );
    let rows = device.rows().await;
    device.close().await;

    let device = Device::open(dir.path(), 1, &schema).await.expect("reopen");
    assert_eq!(device.items().await, vec![shown("a", "Soy Milk", true)]);
    assert_eq!(device.rows().await, rows);
    device.close().await;
    // Another descriptor, even one with the same replicated definition, needs a migration.
    let mut other = schema.clone();
    other.source.identity = "ffffffffffffffffffffffffffffffff".into();
    assert!(matches!(
        Device::open(dir.path(), 1, &other).await,
        Err(Error::Incompatible(_))
    ));
    let mut policy = schema;
    policy.update_policy = UpdatePolicy::Assigned;
    assert!(Device::open(dir.path(), 1, &policy).await.is_err());
}

#[tokio::test]
async fn keys_survive_reordering_concurrent_insertion_deletion_and_replacement() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let a = Device::open(dir.path(), 1, &schema).await.expect("a");
    let b = Device::open(dir.path(), 2, &schema).await.expect("b");
    let c = Device::open(dir.path(), 3, &schema).await.expect("c");
    let all = [&a, &b, &c];
    let key = [text("k")];
    let insert = |device: &Device, title: &str| {
        device
            .insert(ITEMS, &item("k", title, false), &[])
            .expect("insert")
    };

    // Two devices insert the same key offline: one whole row wins everywhere.
    let first = a.commit(insert(&a, "from A")).await;
    let second = b.commit(insert(&b, "from B")).await;
    sync(&all).await;
    for device in all {
        assert_eq!(device.items().await, vec![shown("k", "from B", false)]);
    }
    let rows = a.rows().await;
    let located = a.app.locate(&rows, ITEMS, &key).expect("locate");
    assert_eq!((located.visible, located.live.len()), (Some(second), 2));
    // Locally the key exists, and the hidden incarnation is not a row.
    assert!(a.try_commit(insert(&a, "again")).await.is_err());
    let hidden = a
        .update(ITEMS, first, &[("title", text("x"))])
        .expect("update");
    assert!(a.try_commit(hidden).await.is_err());

    // A deletes the key while C still edits the row it saw.
    let late = c
        .update(ITEMS, second, &[("title", text("late"))])
        .expect("update");
    c.commit(late).await;
    let delete = a
        .app
        .delete_key(&rows, ITEMS, &key)
        .expect("delete")
        .expect("visible");
    a.commit(delete).await;
    sync(&all).await;
    for device in all {
        assert!(device.items().await.is_empty());
        assert!(device.rows().await.is_empty());
    }
    // The deleted row is gone locally too.
    let stale = c
        .update(ITEMS, second, &[("title", text("x"))])
        .expect("update");
    assert!(c.try_commit(stale).await.is_err());

    // Reusing the key is a new incarnation; the old edit does not reach it.
    let third = a.commit(insert(&a, "again")).await;
    sync(&all).await;
    for device in all {
        assert_eq!(device.items().await, vec![shown("k", "again", false)]);
    }

    // B replaces the row while A edits the incarnation being replaced.
    let replacement = b
        .replace(&b.rows().await, ITEMS, &item("k", "replaced", true))
        .expect("replace");
    b.commit(replacement).await;
    let edit = a
        .update(ITEMS, third, &[("title", text("edited"))])
        .expect("update");
    a.commit(edit).await;
    sync(&all).await;
    for device in all {
        assert_eq!(device.items().await, vec![shown("k", "replaced", true)]);
    }

    // A delete removes what it observed; a concurrent replacement survives.
    let delete = a
        .app
        .delete_key(&a.rows().await, ITEMS, &key)
        .expect("delete")
        .expect("visible");
    a.commit(delete).await;
    let survivor = c
        .replace(&c.rows().await, ITEMS, &item("k", "survivor", false))
        .expect("replace");
    c.commit(survivor).await;
    sync(&all).await;
    for device in all {
        assert_eq!(device.items().await, vec![shown("k", "survivor", false)]);
    }

    // Upsert updates the visible incarnation and publishes only what differs.
    let rows = b.rows().await;
    let same = b
        .upsert(&rows, ITEMS, &item("k", "survivor", false))
        .expect("upsert");
    assert!(same.is_none());
    let titled = b
        .upsert(&rows, ITEMS, &item("k", "renamed", false))
        .expect("upsert")
        .expect("changed");
    b.commit(titled).await;
    let visible = a
        .app
        .locate(&a.rows().await, ITEMS, &key)
        .expect("locate")
        .visible;
    let done = [("completed", SqlValue::Integer(1))];
    a.commit(
        a.update(ITEMS, visible.expect("visible"), &done)
            .expect("update"),
    )
    .await;
    sync(&all).await;
    let merged = vec![shown("k", "renamed", true)];
    for device in all {
        assert_eq!(device.items().await, merged);
    }

    // A new device receives the whole history backwards.
    let d = Device::open(dir.path(), 4, &schema).await.expect("d");
    let mut history = a.operations().await;
    history.reverse();
    d.receive(history).await;
    assert_eq!(d.items().await, merged);
    assert_eq!(d.rows().await, a.rows().await);

    // Replay from the checkpoint reproduces the private and the typed table.
    for device in [&a, &b, &c, &d] {
        device.replica.checkpoint().await.expect("checkpoint");
        assert!(device.replica.audit().await.expect("audit").recomputed);
    }
    let rows = a.rows().await;
    a.close().await;
    let a = Device::open(dir.path(), 1, &schema).await.expect("reopen");
    assert_eq!((a.items().await, a.rows().await), (merged, rows));
}

fn line(
    tenant: &str,
    ordinal: SqlValue,
    price: SqlValue,
    payload: SqlValue,
) -> [(&'static str, SqlValue); 6] {
    [
        ("tenant", text(tenant)),
        ("ordinal", ordinal),
        ("price", price),
        ("payload", payload),
        // Converter-backed columns arrive as their stored scalars.
        ("due", SqlValue::Integer(1_700_000_000_000)),
        ("state", text("OPEN")),
    ]
}
#[tokio::test]
async fn compound_blob_collated_and_numeric_keys_agree_with_sqlite() {
    let schema = LogicalSchema::parse(EDGE.as_bytes()).expect("schema");
    let names: Vec<_> = schema.tables.iter().map(|t| t.sql_name.as_str()).collect();
    assert_eq!(names, ["digests", "drafts", "ledger_lines"]);
    let dir = tempfile::tempdir().expect("dir");
    let a = Device::open(dir.path(), 1, &schema).await.expect("a");
    let b = Device::open(dir.path(), 2, &schema).await.expect("b");
    const LINES: &str = "ledger_lines";
    let insert =
        |device: &Device, row: [(&'static str, SqlValue); 6]| device.insert(LINES, &row, &[]);
    let blob = SqlValue::Blob(vec![0, 255, 0]);

    let integer = SqlValue::Integer;
    a.commit(
        insert(
            &a,
            line("ACME", integer(i64::MAX), real(-0.0), blob.clone()),
        )
        .expect("max"),
    )
    .await;
    a.commit(
        insert(
            &a,
            line("acmé", integer(i64::MIN), SqlValue::Null, SqlValue::Null),
        )
        .expect("min"),
    )
    .await;
    // An integral REAL key is its INTEGER; an exact INTEGER price is its REAL.
    a.commit(insert(&a, line("Acme", real(3.0), integer(2), SqlValue::Null)).expect("coerced"))
        .await;
    a.commit(
        insert(
            &a,
            line("x", integer(1), real(f64::INFINITY), SqlValue::Null),
        )
        .expect("inf"),
    )
    .await;
    // NOCASE folds ASCII only: the same key, and a different one.
    let folded = insert(&a, line("ACME", integer(3), SqlValue::Null, SqlValue::Null)).expect("row");
    assert!(a.try_commit(folded).await.is_err());
    a.commit(
        insert(
            &a,
            line("ACMÉ", integer(i64::MIN), SqlValue::Null, SqlValue::Null),
        )
        .expect("É"),
    )
    .await;
    // Values a column cannot store exactly are refused before any write.
    assert!(insert(&a, line("t", real(1.5), SqlValue::Null, SqlValue::Null)).is_err());
    assert!(insert(&a, line("t", text("1"), SqlValue::Null, SqlValue::Null)).is_err());
    assert!(insert(&a, line("t", integer(1), integer(i64::MAX), SqlValue::Null)).is_err());
    assert!(insert(&a, line("t", integer(1), text("2"), SqlValue::Null)).is_err());
    assert!(insert(&a, line("t", integer(1), SqlValue::Null, text("blob"))).is_err());
    assert!(
        insert(
            &a,
            line("t", SqlValue::Null, SqlValue::Null, SqlValue::Null)
        )
        .is_err()
    );
    assert!(SqlValue::real(f64::NAN).is_none());
    let mut stateless = line("t", integer(1), SqlValue::Null, SqlValue::Null);
    stateless[5].1 = SqlValue::Null;
    assert!(insert(&a, stateless).is_err());

    // BLOB keys, the empty one included; an oversized key is refused.
    let digest = |bytes: Vec<u8>, score: f64| {
        a.insert(
            "digests",
            &[("digest", SqlValue::Blob(bytes)), ("score", real(score))],
            &[],
        )
    };
    for (bytes, score) in [(vec![], 0.5), (vec![0; 32], 1e300), (b"a".to_vec(), -2.25)] {
        a.commit(digest(bytes, score).expect("digest")).await;
    }
    assert!(digest(vec![0; 4096], 1.0).is_err());

    // Concurrent inserts of one key in two spellings: one row, one spelling.
    // A's clock is ahead after its earlier writes.
    a.commit(insert(&a, line("Zed", integer(9), SqlValue::Null, SqlValue::Null)).expect("Zed"))
        .await;
    b.commit(insert(&b, line("ZED", integer(9), SqlValue::Null, SqlValue::Null)).expect("ZED"))
        .await;
    sync(&[&a, &b]).await;

    let lines =
        "SELECT tenant,ordinal,price,payload,due,state FROM ledger_lines ORDER BY ordinal,tenant";
    let read = a.read(lines).await;
    assert_eq!(read, b.read(lines).await);
    let stored = |row: [(&'static str, SqlValue); 6]| row.map(|(_, value)| value).to_vec();
    assert_eq!(
        read,
        vec![
            stored(line(
                "ACMÉ",
                integer(i64::MIN),
                SqlValue::Null,
                SqlValue::Null
            )),
            stored(line(
                "acmé",
                integer(i64::MIN),
                SqlValue::Null,
                SqlValue::Null
            )),
            stored(line("x", integer(1), real(f64::INFINITY), SqlValue::Null)),
            stored(line("Acme", integer(3), real(2.0), SqlValue::Null)),
            // The later creation stamp wins with its own spelling.
            stored(line("Zed", integer(9), SqlValue::Null, SqlValue::Null)),
            stored(line("ACME", integer(i64::MAX), real(0.0), blob)),
        ]
    );
    let digests = "SELECT digest,score FROM digests ORDER BY digest";
    assert_eq!(a.read(digests).await, b.read(digests).await);
    assert_eq!(a.read(digests).await.len(), 3);
    let visible = a.rows().await.iter().filter(|row| row.visible).count();
    assert_eq!(visible, 9);
    for device in [&a, &b] {
        assert!(device.replica.audit().await.expect("audit").recomputed);
    }
    b.close().await;

    // The engine's own key comparison and storage classes say the same.
    let file = rusqlite::Connection::open(a.close().await).expect("file");
    let count = |sql: &str| -> i64 { file.query_row(sql, [], |row| row.get(0)).expect(sql) };
    assert_eq!(
        count("SELECT count(*) FROM ledger_lines") + count("SELECT count(*) FROM digests"),
        9
    );
    assert_eq!(
        count("SELECT count(*) FROM ledger_lines WHERE tenant='acme'"),
        2
    );
    assert_eq!(
        count("SELECT count(*) FROM ledger_lines WHERE typeof(price)='real'"),
        3
    );
    assert_eq!(
        count("SELECT count(*) FROM ledger_lines WHERE typeof(ordinal)<>'integer'"),
        0
    );
    assert_eq!(
        count("SELECT count(*) FROM digests WHERE typeof(digest)<>'blob'"),
        0
    );
    let direct = |tenant: &str, ordinal: i64| {
        file.execute(
            "INSERT INTO ledger_lines(tenant,ordinal,state,id) VALUES(?1,?2,'x',4294967396)",
            rusqlite::params![tenant, ordinal],
        )
    };
    file.execute_batch("BEGIN").expect("begin");
    assert!(direct("acme", i64::MAX).is_err());
    assert!(direct("zed", 9).is_err());
    assert!(direct("ACMé", i64::MIN).is_err());
    direct("ÀCME", i64::MAX).expect("another key");
    // The local-only table keeps its ordinary engine-assigned key.
    file.execute("INSERT INTO drafts(body,id) VALUES('a',4294967300)", [])
        .expect("draft");
    assert_eq!(file.last_insert_rowid(), 1);
    file.execute_batch("ROLLBACK").expect("rollback");
}

#[tokio::test]
async fn physical_namespaces_and_checkpoint_imports_keep_row_identity() {
    let schema = LogicalSchema::parse(EDGE.as_bytes()).expect("schema");
    let dir = tempfile::tempdir().expect("dir");
    let plain = Device::open(dir.path(), 1, &schema).await.expect("plain");
    // The same domain in a shared file, where every table has a physical prefix.
    let shared = dir.path().join("shared.sqlite");
    let domain = installation(2, &schema, plain.scope).expect("installation");
    let runtime = Runtime::open(&shared, vec![domain]).await.expect("runtime");
    let scoped = Device {
        replica: runtime.domain(plain.scope).expect("domain"),
        app: plain.app.clone(),
        scope: plain.scope,
        path: shared.clone(),
        author: 2,
        seen: Default::default(),
    };
    assert_eq!(
        *scoped
            .replica
            .logical_schema()
            .expect("schema")
            .expect("bound"),
        schema
    );

    const LINES: &str = "ledger_lines";
    let integer = SqlValue::Integer;
    let row = |tenant: &str, ordinal: i64, price: f64| {
        line(
            tenant,
            integer(ordinal),
            real(price),
            SqlValue::Blob(vec![7]),
        )
    };
    let first = plain
        .commit(
            plain
                .insert(LINES, &row("Acme", 1, 2.0), &[])
                .expect("insert"),
        )
        .await;
    plain
        .commit(
            plain
                .insert(LINES, &row("Acme", 2, 0.25), &[])
                .expect("insert"),
        )
        .await;
    sync(&[&plain, &scoped]).await;
    // The namespaced replica addresses the incarnation the plain one created.
    let edit = [("price", real(1e300)), ("state", text("DONE"))];
    scoped
        .commit(scoped.update(LINES, first, &edit).expect("update"))
        .await;
    let second = scoped
        .app
        .delete_key(&scoped.rows().await, LINES, &[text("ACME"), integer(2)])
        .expect("delete")
        .expect("visible");
    scoped.commit(second).await;
    scoped
        .commit(
            scoped
                .insert(LINES, &row("acme", 2, -4.0), &[])
                .expect("reuse"),
        )
        .await;
    sync(&[&plain, &scoped]).await;
    let lines = "SELECT tenant,ordinal,price,state FROM ledger_lines ORDER BY ordinal";
    let expected = vec![
        vec![text("Acme"), integer(1), real(1e300), text("DONE")],
        vec![text("acme"), integer(2), real(-4.0), text("OPEN")],
    ];
    assert_eq!(plain.read(lines).await, expected);
    assert_eq!(scoped.read(lines).await, expected);
    assert_eq!(plain.rows().await, scoped.rows().await);

    // A new device starts from the checkpoint rows instead of the history.
    plain.replica.checkpoint().await.expect("checkpoint");
    let fresh = Device::open(dir.path(), 3, &schema).await.expect("fresh");
    let mut cursor = None;
    loop {
        let page = plain.replica.checkpoint_page(cursor).await.expect("page");
        let page = CheckpointPage::decode(&page.encode()).expect("wire");
        cursor = page.next;
        if fresh
            .replica
            .import_checkpoint(page)
            .await
            .expect("import")
            .is_some()
        {
            break;
        }
    }
    assert_eq!(fresh.read(lines).await, expected);
    assert_eq!(fresh.rows().await, plain.rows().await);
    // It continues from the imported incarnations.
    let located = fresh
        .app
        .locate(&fresh.rows().await, LINES, &[text("ACME"), integer(1)])
        .expect("locate");
    assert_eq!(located.visible, Some(first));
    fresh
        .commit(
            fresh
                .update(LINES, first, &[("price", SqlValue::Null)])
                .expect("update"),
        )
        .await;
    sync(&[&plain, &scoped, &fresh]).await;
    for device in [&plain, &scoped, &fresh] {
        assert_eq!(device.read(lines).await[0][2], SqlValue::Null);
        assert!(device.replica.audit().await.expect("audit").recomputed);
    }

    drop(scoped);
    runtime.close().await.expect("close");
    let file = rusqlite::Connection::open(&shared).expect("file");
    let tables: Vec<String> = file
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name LIKE '%ledger_lines'")
        .expect("statement")
        .query_map([], |row| row.get(0))
        .expect("tables")
        .collect::<Result<_, _>>()
        .expect("names");
    assert!(!tables.is_empty() && tables.iter().all(|name| name != LINES));
}

#[tokio::test]
async fn renamed_tables_and_columns_keep_their_identity_between_replicas() {
    let schema = shopping();
    let mut renamed = schema.clone();
    let table = &mut renamed.tables[0];
    table.sql_name = "groceries".into();
    table.create_sql = table
        .create_sql
        .replace("shopping_items", "groceries")
        .replace("title", "name");
    table.column("title").expect("title");
    for column in &mut table.columns {
        if column.sql_name == "title" {
            column.sql_name = "name".into();
        }
        if let rocky_core::ColumnClass::Derived { rule } = &mut column.class {
            *rule = rule.replace("title", "name");
        }
    }
    table.primary_key.iter_mut().for_each(|name| {
        if name == "title" {
            *name = "name".into();
        }
    });
    for index in &mut table.indexes {
        index.name = index
            .name
            .replace("shopping_items", "groceries")
            .replace("title", "name");
        index.create_sql = index
            .create_sql
            .replace("shopping_items", "groceries")
            .replace("title", "name");
        for column in &mut index.columns {
            if column.name == "title" {
                column.name = "name".into();
            }
        }
    }
    renamed.validate().expect("renamed descriptor");
    assert_ne!(
        schema.fingerprint().expect("old"),
        renamed.fingerprint().expect("new")
    );
    assert_eq!(
        schema.replicated_fingerprint(),
        renamed.replicated_fingerprint()
    );

    let dir = tempfile::tempdir().expect("dir");
    let old = Device::open(dir.path(), 1, &schema).await.expect("old");
    let new = Device::open(dir.path(), 2, &renamed).await.expect("new");
    let tea = old
        .commit(
            old.insert(ITEMS, &item("t", "Tea", false), &[])
                .expect("insert"),
        )
        .await;
    sync(&[&old, &new]).await;
    let groceries = "SELECT key,name,completed,searchTitle FROM groceries";
    assert_eq!(new.read(groceries).await, vec![shown("t", "Tea", false)]);
    let edit = new
        .update("groceries", tea, &[("name", text("Green Tea"))])
        .expect("update");
    new.commit(edit).await;
    sync(&[&old, &new]).await;
    assert_eq!(old.items().await, vec![shown("t", "Green Tea", false)]);
    assert_eq!(new.read(groceries).await, old.items().await);

    // A rename without the logical name is another definition and another app.
    let mut other = renamed.clone();
    other.tables[0].logical = "groceries".into();
    assert_ne!(
        schema.replicated_fingerprint(),
        other.replicated_fingerprint()
    );
}

#[tokio::test]
async fn incompatible_schemas_bindings_and_codecs_are_refused() {
    let base = LogicalSchema::parse(EDGE.as_bytes()).expect("descriptor");
    let incompatible =
        |schema: LogicalSchema| match (schema.installable(), TypedApp::new(schema, &[DOMAIN])) {
            (Err(SchemaError::Incompatible { table, reason }), Err(_)) => (table, reason),
            _ => panic!("schema was accepted"),
        };
    let mut auto = base.clone();
    auto.tables
        .iter_mut()
        .find(|table| table.sql_name == "drafts")
        .unwrap()
        .class = TableClass::Mergeable;
    assert_eq!(
        incompatible(auto),
        ("drafts".into(), Incompatibility::AutoGeneratedKey)
    );
    let mut local = base.clone();
    local.tables[0].columns[1].class = rocky_core::ColumnClass::LocalOnly;
    assert_eq!(
        incompatible(local),
        (
            "digests".into(),
            Incompatibility::LocalColumn("score".into())
        )
    );
    let mut derived = base;
    derived.tables[0].columns[1].class = rocky_core::ColumnClass::Derived {
        rule: "random()".into(),
    };
    assert_eq!(
        incompatible(derived),
        (
            "digests".into(),
            Incompatibility::DerivedRule("score".into())
        )
    );

    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    // A scope of another publisher's application is not this definition's.
    let foreign = Scope {
        mesh: MeshId([1; 16]),
        app: rocky_core::ApplicationManifest::app_id([9; 32], "shopping"),
        domain: DOMAIN,
    };
    assert!(matches!(
        Device::open_in(dir.path(), 3, &schema, foreign).await,
        Err(Error::Incompatible(_))
    ));

    let device = Device::open(dir.path(), 1, &schema).await.expect("open");
    let committed = device
        .replica
        .mutate(
            device
                .insert(ITEMS, &item("a", "Tea", false), &[])
                .expect("insert"),
        )
        .expect("queued")
        .committed()
        .await
        .expect("committed");
    // Payloads outside the canonical codec never validate.
    let app = device.app.clone();
    let refused = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut operation = committed.clone();
        change(&mut operation.payload);
        app.validate(&operation).is_err()
    };
    assert!(app.validate(&committed).is_ok());
    assert!(refused(&|payload| payload[0] = 9));
    // Historical operation-only update/delete tags are no longer admitted.
    for tag in [2, 3] {
        let mut operation = committed.clone();
        operation.payload = vec![tag];
        operation
            .payload
            .extend(schema.table_id(schema.table(ITEMS).unwrap()));
        if tag == 2 {
            operation.payload.push(1);
            operation.payload.extend(schema.field_id(
                schema.table(ITEMS).unwrap(),
                schema.table(ITEMS).unwrap().column("title").unwrap(),
            ));
            text("legacy").encode(&mut operation.payload);
        } else {
            operation.payload.push(0);
        }
        assert!(app.validate(&operation).is_err());
    }
    assert!(refused(&|payload| payload[1] ^= 1));
    assert!(refused(&|payload| payload.push(0)));
    assert!(refused(&|payload| payload.truncate(payload.len() - 1)));
    // The last byte counts replaced incarnations; none follow it.
    assert!(refused(&|payload| *payload.last_mut().expect("count") = 1));
    let path = device.close().await;

    // The installed tables must still be what the descriptor says.
    let file = rusqlite::Connection::open(&path).expect("file");
    file.execute_batch("DROP INDEX index_shopping_items_completed_title")
        .expect("drop");
    drop(file);
    assert!(matches!(
        Device::open(dir.path(), 1, &schema).await,
        Err(Error::Incompatible(_))
    ));
    let file = rusqlite::Connection::open(&path).expect("file");
    file.execute_batch(
        "CREATE INDEX index_shopping_items_completed_title ON shopping_items(completed, title);
         UPDATE rocky_logical_schema SET descriptor=x'7b7d'",
    )
    .expect("tamper");
    drop(file);
    assert!(matches!(
        Device::open(dir.path(), 1, &schema).await,
        Err(Error::Incompatible(_))
    ));
}

#[tokio::test]
async fn identity_descriptor_installs_the_shared_columns_on_trusted_nodes() {
    let definition = serde_json::json!({
        "format":3, "publisher": "11".repeat(32), "application": "identity-descriptor",
        "tables": [
            {"name": "links", "class": "mergeable", "columns": [
                {"name": "a", "type": "text", "not_null": true},
                {"name": "b", "type": "text", "not_null": true}], "primary_key": ["a", "b"]},
            {"name": "drafts", "class": "local_only", "columns": [
                {"name": "body", "type": "text"}], "primary_key": ["id"]}
        ]
    });
    let schema = rocky_core::SchemaDefinition::parse(&serde_json::to_vec(&definition).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let device = Device::open(directory.path(), 1, &schema).await.unwrap();
        for table in ["links", "drafts"] {
            assert!(
                device
                    .read(&format!("SELECT id,row_kind FROM {table} WHERE row_kind=0"))
                    .await
                    .is_empty()
            );
        }
        device.close().await;
    }
}

#[tokio::test]
async fn trusted_id_reservations_roll_back_and_continue_after_tombstones_and_reopen() {
    let definition = serde_json::json!({"format":3,"publisher":"11".repeat(32),"application":"trusted-identities", "tables":[{"name":"links","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]});
    let schema = rocky_core::SchemaDefinition::parse(&serde_json::to_vec(&definition).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let a = Device::open(directory.path(), 1, &schema).await.unwrap();
    let b = Device::open(directory.path(), 2, &schema).await.unwrap();
    let sql = a.replica.sql_session(Default::default()).await.unwrap();
    assert!(sql.next_id("links").await.is_err());
    sql.run("BEGIN", vec![]).await.unwrap();
    let first = sql.next_id("links").await.unwrap();
    sql.run("SAVEPOINT trial", vec![]).await.unwrap();
    let unused = sql.next_id("links").await.unwrap();
    sql.run("ROLLBACK TO trial", vec![]).await.unwrap();
    assert_eq!(sql.next_id("links").await.unwrap(), unused);
    sql.run("ROLLBACK", vec![]).await.unwrap();
    sql.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(sql.next_id("links").await.unwrap(), first);
    sql.run(
        "INSERT INTO links(key,title,id) VALUES('pair','old',?)",
        vec![SqlValue::Integer(first.integer())],
    )
    .await
    .unwrap();
    sql.run("COMMIT", vec![]).await.unwrap();
    sync(&[&a, &b]).await;
    let held = b.rows().await;
    b.commit(
        b.app
            .update_id(&held, "links", first, &[("title", text("late"))])
            .unwrap(),
    )
    .await;
    let old_delete = b.app.delete_id(&held, "links", first).unwrap();
    sql.run("BEGIN", vec![]).await.unwrap();
    sql.run("DELETE FROM links WHERE key='pair'", vec![])
        .await
        .unwrap();
    let replacement = sql.next_id("links").await.unwrap();
    sql.run(
        "INSERT INTO links(key,title,id) VALUES('pair','new',?)",
        vec![SqlValue::Integer(replacement.integer())],
    )
    .await
    .unwrap();
    sql.run("COMMIT", vec![]).await.unwrap();
    b.commit(old_delete).await;
    sync(&[&a, &b]).await;
    assert_eq!(
        a.read("SELECT id,title,row_kind FROM links").await,
        b.read("SELECT id,title,row_kind FROM links").await
    );
    assert_eq!(
        a.read("SELECT id,title FROM links").await,
        vec![vec![SqlValue::Integer(replacement.integer()), text("new")]]
    );
    for id in [first, rocky_core::RowIdentity::new(2, 1).unwrap()] {
        let forged = a
            .app
            .insert(
                "links",
                &[
                    ("key", text("forged")),
                    ("title", text("bad")),
                    ("id", SqlValue::Integer(id.integer())),
                    ("row_kind", SqlValue::Integer(0)),
                ],
                &[],
            )
            .unwrap();
        assert!(a.try_commit(forged).await.is_err());
    }
    a.replica.checkpoint().await.unwrap();
    let fresh_root = tempfile::tempdir().unwrap();
    let fresh = Device::open(fresh_root.path(), 1, &schema).await.unwrap();
    let mut cursor = None;
    loop {
        let page = a.replica.checkpoint_page(cursor).await.unwrap();
        cursor = page.next;
        if fresh
            .replica
            .import_checkpoint(page)
            .await
            .unwrap()
            .is_some()
        {
            break;
        }
    }
    assert_eq!(
        fresh.read("SELECT id,title FROM links").await,
        a.read("SELECT id,title FROM links").await
    );
    let fresh_sql = fresh.replica.sql_session(Default::default()).await.unwrap();
    fresh_sql.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(fresh_sql.next_id("links").await.unwrap().sequence(), 3);
    fresh_sql.run("ROLLBACK", vec![]).await.unwrap();
    sql.run("DELETE FROM links", vec![]).await.unwrap();
    drop(sql);
    a.close().await;
    let reopened = Device::open(directory.path(), 1, &schema).await.unwrap();
    let sql = reopened
        .replica
        .sql_session(Default::default())
        .await
        .unwrap();
    sql.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(sql.next_id("links").await.unwrap().sequence(), 3);
    sql.run("ROLLBACK", vec![]).await.unwrap();
}

#[tokio::test]
async fn trusted_partition_ids_and_agreed_statements() {
    let definition = serde_json::json!({"format":3,"publisher":"11".repeat(32),"application":"trusted-partitions","tables":[{"name":"bookings","class":"mergeable","columns":[{"name":"month","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"amount","type":"integer","not_null":true}],"primary_key":["month","key"],"partition_by":["month"]}],"views":[{"name":"partial_bookings","create_sql":"CREATE VIEW partial_bookings AS SELECT * FROM bookings WHERE row_kind=0"}]});
    let schema = rocky_core::SchemaDefinition::parse(&serde_json::to_vec(&definition).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let a = Device::open(directory.path(), 1, &schema).await.unwrap();
    let b = Device::open(directory.path(), 2, &schema).await.unwrap();
    let sql = a.replica.sql_session(Default::default()).await.unwrap();
    let mut ids = Vec::new();
    for (month, amount) in [("A", 10), ("B", 20)] {
        sql.run("BEGIN", vec![]).await.unwrap();
        let id = sql
            .next_id_partition("bookings", &[text(month)])
            .await
            .unwrap();
        ids.push(id);
        sql.run(
            "INSERT INTO bookings(month,key,amount,id) VALUES(?,?,?,?)",
            vec![
                text(month),
                text("k"),
                SqlValue::Integer(amount),
                SqlValue::Integer(id.integer()),
            ],
        )
        .await
        .unwrap();
        sql.run("COMMIT", vec![]).await.unwrap();
    }
    assert_eq!(ids[0], ids[1]);
    sync(&[&a, &b]).await;
    for device in [&a, &b] {
        assert_eq!(
            device
                .read("SELECT SUM(amount) FROM bookings WHERE month='A'")
                .await[0][0],
            SqlValue::Integer(10)
        );
        assert_eq!(
            device.read("SELECT SUM(amount) FROM bookings").await[0][0],
            SqlValue::Integer(30)
        );
        assert_eq!(
            device
                .read("SELECT SUM(amount) FROM partial_bookings")
                .await[0][0],
            SqlValue::Integer(30)
        );
        let rows = device.rows().await;
        assert!(
            device
                .app
                .update_id(
                    &rows,
                    "bookings",
                    ids[0],
                    &[("amount", SqlValue::Integer(7))]
                )
                .is_err()
        );
        assert!(
            device
                .app
                .update_partition_id(
                    &rows,
                    "bookings",
                    &[text("A")],
                    ids[0],
                    &[("amount", SqlValue::Integer(7))]
                )
                .is_ok()
        );
    }
    a.close().await;
    b.close().await;
}
