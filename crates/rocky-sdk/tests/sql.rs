//! Ordinary SQL statements and
//! transactions on typed tables, captured and committed as operations.
#[path = "support/trusted.rs"]
mod trusted;
use rocky_core::{
    DomainId, Envelope, LogicalSchema, MeshId, NodeId, PrincipalId, Scope, SqlValue,
    TransactionToken, UpdatePolicy,
};
use rocky_runtime::{
    AbortReason, BoundQuery, Certainty, CommitClass, DomainInstallation, Error, Identity,
    ReadOptions, Replica, Runtime, RuntimeConfig, SqlError, SqlOptions, SqlRows, SqlSession,
    SqlState, operation_handle,
};
use rocky_sdk::{Access, TypedApp, TypedRow};
use std::{path::Path, time::Duration};

const SHOPPING: &str = include_str!("fixtures/shopping-descriptor.json");
const EDGE: &str = include_str!("fixtures/edge-descriptor.json");
const CALLER: PrincipalId = PrincipalId([7; 16]);
const DOMAIN: DomainId = DomainId([2; 16]);
const ALL: &str = "SELECT key,title,completed,searchTitle FROM shopping_items ORDER BY key";

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
fn int(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}
fn nodes() -> Vec<NodeId> {
    (1..=4).map(|node| NodeId([node; 16])).collect()
}
fn shopping() -> LogicalSchema {
    LogicalSchema::parse(SHOPPING.as_bytes()).expect("shopping schema")
}
fn scope(schema: &LogicalSchema) -> Scope {
    Scope {
        mesh: MeshId([1; 16]),
        app: schema.app_id(),
        domain: DOMAIN,
    }
}
fn installation(node: u8, schema: &LogicalSchema, scope: Scope) -> DomainInstallation<TypedApp> {
    DomainInstallation {
        app: trusted::app(schema, &[scope.domain], &nodes()),
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
    }
}
fn shown(id: &str, title: &str, completed: i64) -> Vec<SqlValue> {
    vec![
        text(id),
        text(title),
        int(completed),
        text(&title.to_ascii_lowercase()),
    ]
}
fn token(value: u8) -> TransactionToken {
    TransactionToken([value; 32])
}
fn code(error: &Error) -> Option<i32> {
    match error {
        Error::Sql(SqlError::Statement { code, .. }) => Some(*code),
        _ => None,
    }
}

struct Device {
    replica: Replica<TypedApp>,
    scope: Scope,
}
impl Device {
    async fn open(dir: &Path, node: u8, schema: &LogicalSchema) -> Self {
        let scope = scope(schema);
        let domain = installation(node, schema, scope);
        let replica = Replica::open_with_config(
            dir.join(format!("node{node}.sqlite")),
            domain.app,
            domain.policy,
            domain.identity,
            domain.config,
        )
        .await
        .expect("open");
        Self { replica, scope }
    }
    async fn session(&self) -> SqlSession {
        self.replica
            .sql_session(SqlOptions::default())
            .await
            .expect("session")
    }
    async fn rows(&self) -> Vec<TypedRow> {
        self.replica.query().await.expect("query").rows
    }
    /// A bound read on another connection: committed rows only.
    async fn read(&self, sql: &str) -> Vec<Vec<SqlValue>> {
        let query = BoundQuery {
            sql: sql.into(),
            values: Vec::new(),
        };
        self.replica
            .read(query, ReadOptions::new(self.scope, 1024))
            .await
            .expect("read")
            .rows
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
    }
    /// The checkpoint's replay reproduces the private and the typed tables.
    async fn audited(&self) {
        self.replica.checkpoint().await.expect("checkpoint");
        assert!(self.replica.audit().await.expect("audit").recomputed);
    }
}
async fn run(session: &SqlSession, sql: &str) -> SqlRows {
    trusted::run(session, sql, Vec::new())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}
async fn fails(session: &SqlSession, sql: &str) -> Error {
    match trusted::run(session, sql, Vec::new()).await {
        Ok(_) => panic!("{sql} succeeded"),
        Err(error) => error,
    }
}
fn operations(rows: &SqlRows) -> usize {
    rows.committed.as_ref().expect("committed").operations.len()
}

#[tokio::test]
async fn statements_and_transactions_commit_atomically_and_converge() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let (laptop, phone, tablet) = (
        Device::open(dir.path(), 1, &schema).await,
        Device::open(dir.path(), 2, &schema).await,
        Device::open(dir.path(), 3, &schema).await,
    );
    let session = laptop.session().await;

    // A statement outside a transaction commits before it is reported, and
    // the installed rule owns the derived column whatever the statement set.
    let inserted = run(
        &session,
        "INSERT INTO shopping_items(key,title,completed,searchTitle) VALUES('a','Oat Milk',0,'WRONG')",
    )
    .await;
    assert_eq!((inserted.changes, inserted.state), (1, SqlState::Idle));
    let outcome = inserted.committed.expect("committed");
    assert_eq!(
        (outcome.operations.len(), outcome.class, outcome.replayed),
        (1, CommitClass::Flushed, false)
    );
    assert_eq!(laptop.read(ALL).await, vec![shown("a", "Oat Milk", 0)]);
    let before = laptop.replica.query().await.expect("query").revision;

    // A bulk insert and a multi-field entity update in one transaction.
    let insert = session
        .prepare(
            "INSERT INTO shopping_items(key,title,completed,searchTitle,id) VALUES(?,?,?,'',?)",
        )
        .await
        .expect("prepare");
    assert_eq!((insert.parameters, insert.read_only), (4, false));
    assert_eq!(
        run(&session, "BEGIN IMMEDIATE").await.state,
        SqlState::Active
    );
    for (id, title) in [("b", "Bread"), ("c", "Cheese"), ("d", "Dates")] {
        let rows = session
            .execute(
                &insert,
                vec![
                    text(id),
                    text(title),
                    int(0),
                    int(session
                        .next_id("shopping_items")
                        .await
                        .expect("identity")
                        .integer()),
                ],
            )
            .await
            .expect("insert");
        assert_eq!((rows.changes, rows.committed), (1, None));
    }
    let updated = run(
        &session,
        "UPDATE shopping_items SET title='Soy Milk',completed=1 WHERE key='a'",
    )
    .await;
    assert_eq!(updated.changes, 1);
    run(
        &session,
        "UPDATE shopping_items SET title='Rye Bread' WHERE key='b'",
    )
    .await;
    // The session reads its own writes; every other reader sees the commit.
    let own = run(
        &session,
        "SELECT key,title FROM shopping_items ORDER BY key",
    )
    .await;
    assert_eq!(own.columns, ["key", "title"]);
    assert_eq!(
        own.cursor().map(|row| row[1].clone()).collect::<Vec<_>>(),
        [
            text("Soy Milk"),
            text("Rye Bread"),
            text("Cheese"),
            text("Dates")
        ]
    );
    assert_eq!(laptop.read(ALL).await, vec![shown("a", "Oat Milk", 0)]);
    assert_eq!(laptop.rows().await.len(), 1);
    assert_eq!(laptop.operations().await.len(), 1);
    let committed = run(&session, "END TRANSACTION").await;
    assert_eq!(committed.state, SqlState::Idle);
    // Three inserts and one update of two fields: four operations, one revision.
    let outcome = committed.committed.expect("committed");
    assert_eq!(outcome.operations.len(), 4);
    assert_eq!(Some(outcome.revision), before.next().ok());
    assert_eq!(
        laptop.replica.query().await.expect("query").revision,
        outcome.revision
    );
    let expected = vec![
        shown("a", "Soy Milk", 1),
        shown("b", "Rye Bread", 0),
        shown("c", "Cheese", 0),
        shown("d", "Dates", 0),
    ];
    assert_eq!(laptop.read(ALL).await, expected);

    // Peers receive the row values, in order, reversed and twice.
    let sent = laptop.operations().await;
    assert_eq!(sent.len(), 5);
    phone.receive(sent.clone()).await;
    let mut reversed = sent.clone();
    reversed.reverse();
    tablet.receive(reversed).await;
    tablet.receive(sent).await;
    for device in [&phone, &tablet] {
        assert_eq!(device.read(ALL).await, expected);
        assert_eq!(device.rows().await, laptop.rows().await);
        // Applying received rows wrote the typed table without new operations.
        assert_eq!(device.operations().await.len(), 5);
    }
    // A peer's SQL edit merges with a concurrent edit of another column.
    let remote = phone.session().await;
    run(
        &remote,
        "UPDATE shopping_items SET completed=1 WHERE key='c'",
    )
    .await;
    run(
        &session,
        "UPDATE shopping_items SET title='Blue Cheese' WHERE key='c'",
    )
    .await;
    laptop.receive(phone.operations().await).await;
    phone.receive(laptop.operations().await).await;
    assert_eq!(laptop.read(ALL).await[2], shown("c", "Blue Cheese", 1));
    assert_eq!(phone.read(ALL).await, laptop.read(ALL).await);
    for device in [&laptop, &phone, &tablet] {
        device.audited().await;
    }
}

#[tokio::test]
async fn rollback_and_nested_savepoints_remove_their_effects() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let device = Device::open(dir.path(), 1, &schema).await;
    let session = device.session().await;
    let insert = |id: &str| {
        format!("INSERT INTO shopping_items(key,title,completed) VALUES('{id}','{id}',0)")
    };
    run(&session, &insert("kept")).await;

    // A rolled back transaction leaves nothing, in the tables or the history.
    run(&session, "BEGIN").await;
    run(&session, &insert("gone")).await;
    run(&session, "DELETE FROM shopping_items WHERE key='kept'").await;
    assert_eq!(run(&session, "ROLLBACK").await.state, SqlState::Idle);
    assert_eq!(device.operations().await.len(), 1);
    assert_eq!(device.read(ALL).await, vec![shown("kept", "kept", 0)]);

    run(&session, "BEGIN DEFERRED TRANSACTION").await;
    run(&session, &insert("one")).await;
    run(&session, "SAVEPOINT outer").await;
    run(&session, &insert("two")).await;
    run(
        &session,
        "UPDATE shopping_items SET completed=1 WHERE key='kept'",
    )
    .await;
    run(&session, "SAVEPOINT \"Inner\"").await;
    run(&session, &insert("three")).await;
    run(&session, "DELETE FROM shopping_items WHERE key='one'").await;
    // Only the inner savepoint's statements are undone; it stays set.
    run(&session, "ROLLBACK TO SAVEPOINT inner").await;
    run(&session, &insert("four")).await;
    run(&session, "RELEASE inner").await;
    assert_eq!(
        code(&fails(&session, "ROLLBACK TO inner").await),
        Some(1),
        "a released savepoint is gone"
    );
    run(&session, "SAVEPOINT third").await;
    run(&session, &insert("five")).await;
    // Rolling back to the outer savepoint removes the nested ones with it.
    run(&session, "ROLLBACK TRANSACTION TO outer").await;
    assert_eq!(code(&fails(&session, "RELEASE third").await), Some(1));
    run(&session, &insert("six")).await;
    assert_eq!(code(&fails(&session, "BEGIN").await), Some(1));
    let committed = run(&session, "COMMIT").await;
    assert_eq!(operations(&committed), 2);
    assert_eq!(
        device.read(ALL).await,
        vec![
            shown("kept", "kept", 0),
            shown("one", "one", 0),
            shown("six", "six", 0)
        ]
    );

    // A savepoint outside a transaction begins one; releasing it commits.
    assert_eq!(
        run(&session, "SAVEPOINT alone").await.state,
        SqlState::Active
    );
    run(&session, &insert("seven")).await;
    run(&session, "SAVEPOINT nested").await;
    run(&session, &insert("eight")).await;
    run(&session, "ROLLBACK TO nested").await;
    let released = run(&session, "RELEASE alone").await;
    assert_eq!((released.state, operations(&released)), (SqlState::Idle, 1));
    assert_eq!(device.read(ALL).await.len(), 4);
    for sql in ["COMMIT", "ROLLBACK", "RELEASE alone"] {
        assert_eq!(code(&fails(&session, sql).await), Some(1), "{sql}");
    }
    device.audited().await;
}

#[tokio::test]
async fn conflict_clauses_noops_key_changes_and_returning_follow_sqlite() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let (device, peer) = (
        Device::open(dir.path(), 1, &schema).await,
        Device::open(dir.path(), 2, &schema).await,
    );
    let session = device.session().await;
    run(
        &session,
        "INSERT INTO shopping_items(key,title,completed) VALUES('a','Apples',0),('b','Bread',0)",
    )
    .await;
    let incarnation = |rows: &[TypedRow], id: &str| {
        rows.iter()
            .find(|row| row.key == [text(id)])
            .map(|row| row.incarnation)
    };
    let first = incarnation(&device.rows().await, "a").expect("a");

    run(&session, "BEGIN").await;
    // ABORT undoes the statement alone; the transaction and its rows stay.
    run(
        &session,
        "INSERT INTO shopping_items(key,title,completed,id) VALUES('c','Cocoa',0,4294967300)",
    )
    .await;
    let duplicate = fails(
        &session,
        "INSERT INTO shopping_items(key,title,completed,id) VALUES('x','X',0,4294967302),('a','Again',0,9223372036854775807)",
    )
    .await;
    assert_eq!(code(&duplicate), Some(1555), "{duplicate}");
    assert_eq!(duplicate.sql_certainty(), Certainty::Definite);
    // FAIL keeps the rows written before the conflict.
    let failed = fails(
        &session,
        "INSERT OR FAIL INTO shopping_items(key,title,completed,id) VALUES('y','Yams',0,4294967301),('a','Again',0,9223372036854775807)",
    )
    .await;
    assert_eq!(code(&failed), Some(1555));
    // IGNORE changes nothing and reports no row.
    let ignored = run(
        &session,
        "INSERT OR IGNORE INTO shopping_items(key,title,completed) VALUES('a','Ignored',1)",
    )
    .await;
    assert_eq!(ignored.changes, 0);
    // REPLACE deletes the row and inserts another one at its key.
    run(
        &session,
        "INSERT OR REPLACE INTO shopping_items(key,title,completed,id) VALUES('a','Apricots',1,4294967299)",
    )
    .await;
    // An upsert updates the row in place.
    let upserted = run(
        &session,
        "INSERT INTO shopping_items(key,title,completed) VALUES('b','Brioche',0) \
         ON CONFLICT(key) DO UPDATE SET title=excluded.title RETURNING key,title,completed",
    )
    .await;
    assert_eq!(
        upserted.rows,
        vec![vec![text("b"), text("Brioche"), int(0)]]
    );
    let committed = run(&session, "COMMIT").await;
    // Inserts of c and y, a new incarnation of a, an update of b.
    assert_eq!(operations(&committed), 4);
    let rows = device.rows().await;
    assert_ne!(incarnation(&rows, "a"), Some(first));
    assert_eq!(
        device.read(ALL).await,
        vec![
            shown("a", "Apricots", 1),
            shown("b", "Brioche", 0),
            shown("c", "Cocoa", 0),
            shown("y", "Yams", 0)
        ]
    );

    // A write of equal values keeps the row count and publishes nothing.
    let revision = device.replica.query().await.expect("query").revision;
    let same = run(
        &session,
        "UPDATE shopping_items SET title=title,completed=completed",
    )
    .await;
    assert_eq!((same.changes, operations(&same)), (4, 0));
    assert_eq!(
        device.replica.query().await.expect("query").revision,
        revision
    );
    // A row inserted and deleted in one transaction never existed.
    run(&session, "BEGIN").await;
    run(
        &session,
        "INSERT INTO shopping_items(key,title,completed) VALUES('t','Temp',0)",
    )
    .await;
    run(&session, "DELETE FROM shopping_items WHERE key='t'").await;
    assert_eq!(operations(&run(&session, "COMMIT").await), 0);
    // A key change is a delete and an insert.
    run(&session, "BEGIN").await;
    run(&session, "DELETE FROM shopping_items WHERE key='c'").await;
    let moved = run(
        &session,
        "INSERT INTO shopping_items(key,title,completed) VALUES('z','Cocoa',0) RETURNING key,title",
    )
    .await;
    assert_eq!(moved.rows, vec![vec![text("z"), text("Cocoa")]]);
    assert_eq!(operations(&run(&session, "COMMIT").await), 2);
    // A deleted and reinserted key is a new incarnation.
    let second = incarnation(&device.rows().await, "b").expect("b");
    run(&session, "BEGIN").await;
    run(&session, "DELETE FROM shopping_items WHERE key='b'").await;
    run(
        &session,
        "INSERT INTO shopping_items(key,title,completed) VALUES('b','Brioche',0)",
    )
    .await;
    assert_eq!(operations(&run(&session, "COMMIT").await), 1);
    assert_ne!(incarnation(&device.rows().await, "b"), Some(second));
    // Random and clock values are computed once; peers receive the result.
    run(
        &session,
        "INSERT INTO shopping_items(key,title,completed) \
         VALUES('r',hex(randomblob(8))||strftime('%s','now'),0)",
    )
    .await;
    let deleted = run(
        &session,
        "DELETE FROM shopping_items WHERE key IN ('y','nowhere')",
    )
    .await;
    assert_eq!((deleted.changes, operations(&deleted)), (1, 1));

    // ROLLBACK as a conflict clause ends the transaction in the engine.
    run(&session, "BEGIN").await;
    run(
        &session,
        "INSERT INTO shopping_items(key,title,completed) VALUES('lost','Lost',0)",
    )
    .await;
    let rolled = fails(&session, "UPDATE OR ROLLBACK shopping_items SET title=NULL").await;
    assert_eq!(code(&rolled), Some(1299), "{rolled}");
    let aborted = fails(&session, "SELECT 1").await;
    assert!(
        matches!(
            &aborted,
            Error::Sql(SqlError::Aborted(AbortReason::Engine(_)))
        ),
        "{aborted}"
    );
    assert_eq!(run(&session, "ROLLBACK").await.state, SqlState::Idle);
    assert_eq!(
        run(&session, "SELECT count(*) FROM shopping_items")
            .await
            .rows,
        [[int(4)]]
    );

    peer.receive(device.operations().await).await;
    assert_eq!(peer.read(ALL).await, device.read(ALL).await);
    assert_eq!(peer.rows().await, device.rows().await);
    device.audited().await;
    peer.audited().await;
}

#[tokio::test]
async fn generated_ids_and_counts_belong_to_the_session() {
    let schema = LogicalSchema::parse(EDGE.as_bytes()).expect("edge schema");
    let dir = tempfile::tempdir().expect("dir");
    let device = Device::open(dir.path(), 1, &schema).await;
    let (session, other) = (device.session().await, device.session().await);
    let observed = "SELECT changes(),last_insert_rowid(),total_changes()";

    // A local table takes engine-assigned keys; nothing is replicated.
    let first = run(&session, "INSERT INTO drafts(body) VALUES('one'),('two')").await;
    assert_eq!((first.changes, first.last_insert_rowid), (2, 2));
    assert_eq!(operations(&first), 0);
    // Protocol rows written for a replicated statement do not show.
    let line = run(
        &session,
        "INSERT INTO ledger_lines(tenant,ordinal,price,payload,due,state) \
         VALUES('Acme',1,2.0,x'00ff',NULL,'open'),('acme',2,0.5,NULL,7,'open')",
    )
    .await;
    assert_eq!((line.changes, operations(&line)), (2, 2));
    assert_eq!(
        run(&session, observed).await.rows,
        [[int(2), int(line.last_insert_rowid), int(4)]]
    );
    // Another session on the same connection has its own counts.
    assert_eq!(run(&other, observed).await.rows, [[int(0), int(0), int(0)]]);
    let third = run(
        &other,
        "INSERT INTO drafts(body) VALUES('three') RETURNING key",
    )
    .await;
    assert_eq!(
        (third.rows, third.last_insert_rowid),
        (vec![vec![int(3)]], 3)
    );
    assert_eq!(
        run(&session, observed).await.rows,
        [[int(2), int(line.last_insert_rowid), int(4)]]
    );

    // A compound key under NOCASE: another spelling is the same row, and
    // rewriting the spelling makes a new incarnation.
    let incarnations = device.rows().await;
    run(&session, "BEGIN").await;
    let held = run(
        &session,
        "SELECT ordinal,price,payload,due FROM ledger_lines WHERE tenant='acme' AND ordinal=1",
    )
    .await
    .rows
    .remove(0);
    run(
        &session,
        "DELETE FROM ledger_lines WHERE tenant='acme' AND ordinal=1",
    )
    .await;
    trusted::run(&session, "INSERT INTO ledger_lines(tenant,ordinal,price,payload,due,state) VALUES('ACME',?,?,?,?,'paid')", held).await.expect("fresh incarnation");
    let spelled = run(&session, "COMMIT").await;
    assert_eq!(operations(&spelled), 1);
    let rows = device.rows().await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.key == [text("ACME"), int(1)]));
    assert!(
        rows.iter()
            .any(|row| !incarnations.contains(row) && row.visible)
    );
    // An integral REAL reads back as the REAL it is.
    assert_eq!(
        device
            .read("SELECT price FROM ledger_lines ORDER BY ordinal")
            .await,
        [
            [SqlValue::real(2.0).expect("real")],
            [SqlValue::real(0.5).expect("real")]
        ]
    );
    // A value outside the column's declared type cannot be published.
    run(&session, "BEGIN").await;
    run(
        &session,
        "UPDATE ledger_lines SET due='soon' WHERE ordinal=2",
    )
    .await;
    let refused = fails(&session, "COMMIT").await;
    assert!(
        matches!(refused, Error::Sql(SqlError::Capture(_))),
        "{refused}"
    );
    assert_eq!(
        run(&session, "SELECT due FROM ledger_lines WHERE ordinal=2")
            .await
            .rows,
        [[int(7)]]
    );
    device.audited().await;
}

#[tokio::test]
async fn update_policies_publish_changed_or_assigned_values() {
    for policy in [UpdatePolicy::Changed, UpdatePolicy::Assigned] {
        let mut schema = shopping();
        schema.update_policy = policy;
        let dir = tempfile::tempdir().expect("dir");
        let (laptop, phone) = (
            Device::open(dir.path(), 1, &schema).await,
            Device::open(dir.path(), 2, &schema).await,
        );
        let (local, remote) = (laptop.session().await, phone.session().await);
        run(
            &local,
            "INSERT INTO shopping_items(key,title,completed) VALUES('a','Tea',0)",
        )
        .await;
        phone.receive(laptop.operations().await).await;
        // The phone edits the title. The laptop, unaware and later on the
        // clock, assigns the title it already holds and changes `completed`.
        run(
            &remote,
            "UPDATE shopping_items SET title='Green Tea' WHERE key='a'",
        )
        .await;
        run(
            &local,
            "INSERT INTO shopping_items(key,title,completed) VALUES('b','B',0)",
        )
        .await;
        run(
            &local,
            "INSERT INTO shopping_items(key,title,completed) VALUES('c','C',0)",
        )
        .await;
        let assigned = run(
            &local,
            "UPDATE shopping_items SET title='Tea',completed=1 WHERE key='a'",
        )
        .await;
        assert_eq!((assigned.changes, operations(&assigned)), (1, 1));
        laptop.receive(phone.operations().await).await;
        phone.receive(laptop.operations().await).await;
        // Changed values leave the title's stamp alone, so the phone's edit
        // stands. Assigned values republish the equal title, which wins.
        let title = match policy {
            UpdatePolicy::Changed => "Green Tea",
            UpdatePolicy::Assigned => "Tea",
        };
        assert_eq!(
            laptop.read(ALL).await[0],
            shown("a", title, 1),
            "{policy:?}"
        );
        assert_eq!(phone.read(ALL).await, laptop.read(ALL).await);
        // An assignment of only equal values is published under `assigned`.
        let equal = run(
            &local,
            "UPDATE shopping_items SET title=title WHERE key='b'",
        )
        .await;
        let expected = usize::from(policy == UpdatePolicy::Assigned);
        assert_eq!((equal.changes, operations(&equal)), (1, expected));
        laptop.audited().await;
        phone.audited().await;
    }
}

#[tokio::test]
async fn a_token_recovers_the_outcome_once_and_conflicts_on_other_input() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let device = Device::open(dir.path(), 1, &schema).await;
    let session = device.session().await;
    run(
        &session,
        "INSERT INTO shopping_items(key,title,completed) VALUES('a','Tea',0)",
    )
    .await;
    assert_eq!(
        device.replica.sql_outcome(token(1)).await.expect("lookup"),
        None
    );

    let input: [&str; 2] = [
        "UPDATE shopping_items SET completed=completed+1 WHERE key='a'",
        "INSERT OR REPLACE INTO shopping_items(key,title,completed) SELECT 'n1','New',0 FROM shopping_items WHERE key='a'",
    ];
    run(&session, "BEGIN").await;
    let mut body = Vec::new();
    for sql in input {
        let sql = trusted::rewrite(&session, sql).await.expect("identity");
        session.run(&sql, vec![]).await.expect("statement");
        body.push(sql);
    }
    run(&session, "ROLLBACK").await;
    run(&session, "BEGIN").await;
    for sql in &body {
        session.run(sql, vec![]).await.expect("statement");
    }

    let original = session.commit(token(1)).await.expect("commit");
    let original = original.committed.expect("committed");
    assert_eq!(
        (original.operations.len(), original.token, original.replayed),
        (2, Some(token(1)), false)
    );
    // Later edits do not change what the token reports.
    run(
        &session,
        "UPDATE shopping_items SET title='Later' WHERE key='a'",
    )
    .await;
    let recovered = device
        .replica
        .sql_outcome(token(1))
        .await
        .expect("lookup")
        .expect("recorded");
    assert!(recovered.replayed);
    assert_eq!(
        (
            recovered.operations.clone(),
            recovered.intent,
            recovered.revision
        ),
        (
            original.operations.clone(),
            original.intent,
            original.revision
        )
    );
    // The original bytes are the retained operations.
    for key in &original.operations {
        let stored = device
            .replica
            .lookup_mutation(operation_handle(*key))
            .await
            .expect("lookup")
            .expect("operation");
        assert_eq!(stored.id, *key);
    }
    // The same body under the same token is not committed a second time.
    let count = device.operations().await.len();
    run(&session, "BEGIN").await;
    for sql in &body {
        session.run(sql, vec![]).await.expect("replay statement");
    }
    let again = session.commit(token(1)).await.expect("commit");
    assert_eq!(again.state, SqlState::Idle);
    let again = again.committed.expect("outcome");
    assert!(again.replayed);
    assert_eq!(again.operations, original.operations);
    assert_eq!(device.operations().await.len(), count);
    assert_eq!(
        device.read(ALL).await,
        vec![shown("a", "Later", 1), shown("n1", "New", 0)]
    );
    // Other statements under the token conflict and change nothing.
    run(&session, "BEGIN").await;
    run(&session, "DELETE FROM shopping_items").await;
    let conflict = session.commit(token(1)).await.expect_err("conflict");
    assert!(matches!(conflict, Error::Conflict), "{conflict}");
    assert_eq!(device.read(ALL).await.len(), 2);
    // A single statement carries a token as well.
    let delete = session
        .prepare("DELETE FROM shopping_items WHERE key=?")
        .await
        .expect("prepare");
    let single = session
        .execute_with_token(&delete, vec![text("n1")], token(2))
        .await
        .expect("delete");
    assert_eq!(operations(&single), 1);
    let repeated = session
        .execute_with_token(&delete, vec![text("n1")], token(2))
        .await
        .expect("repeat");
    assert!(repeated.committed.expect("outcome").replayed);

    // The record survives a restart.
    drop(session);
    device.replica.close().await.expect("close");
    let device = Device::open(dir.path(), 1, &schema).await;
    let recovered = device
        .replica
        .sql_outcome(token(1))
        .await
        .expect("lookup")
        .expect("recorded");
    assert_eq!(
        (recovered.operations, recovered.class),
        (original.operations, CommitClass::Flushed)
    );
    assert_eq!(device.read(ALL).await, vec![shown("a", "Later", 1)]);
    device.audited().await;
}

#[tokio::test]
async fn limits_denials_and_dropped_sessions_end_the_lease() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let device = Device::open(dir.path(), 1, &schema).await;
    let app = trusted::app(&schema, &[DOMAIN], &nodes());
    let insert = |id: &str| {
        format!("INSERT INTO shopping_items(key,title,completed) VALUES('{id}','{id}',0)")
    };
    let mutation = |key: &str| {
        let row = [
            ("key", text(key)),
            ("title", text(key)),
            ("completed", int(0)),
            ("id", int(if key == "m2" { 4294967299 } else { 4294967297 })),
            ("row_kind", int(0)),
        ];
        app.insert("shopping_items", &row, &[]).expect("mutation")
    };
    let aborted = |error: &Error, reason: fn(&AbortReason) -> bool| match error {
        Error::Sql(SqlError::Aborted(found)) => reason(found),
        _ => false,
    };

    // A transaction that outlives its timeout is rolled back, and a write
    // that waited behind it commits.
    let brief = device
        .replica
        .sql_session(SqlOptions {
            transaction_timeout: Duration::from_millis(60),
            ..SqlOptions::default()
        })
        .await
        .expect("session");
    run(&brief, "BEGIN").await;
    run(&brief, &insert("late")).await;
    // Reads are answered while the lease is held, from committed rows.
    assert!(device.rows().await.is_empty());
    assert!(device.read(ALL).await.is_empty());
    let waiting = device.replica.mutate(mutation("m1")).expect("queued");
    waiting.committed().await.expect("commit after the lease");
    let timed_out = fails(&brief, &insert("later")).await;
    assert!(
        aborted(&timed_out, |reason| *reason == AbortReason::Timeout),
        "{timed_out}"
    );
    assert_eq!(timed_out.sql_certainty(), Certainty::Definite);
    let unacknowledged = fails(&brief, "COMMIT").await;
    assert!(aborted(&unacknowledged, |reason| *reason == AbortReason::Timeout));
    // The failed commit acknowledged the rollback.
    assert_eq!(operations(&run(&brief, &insert("after")).await), 1);
    // The commit reply precedes lease accounting; a queued call observes cleanup.
    brief
        .prepare("SELECT 1")
        .await
        .expect("lease cleanup barrier");
    let stats = device.replica.writer_stats();
    assert!(stats.leases >= 2);
    assert!(
        (60_000..5_000_000).contains(&stats.lease_max_micros),
        "{stats:?}"
    );

    // A statement beyond the session's tables poisons its transaction.
    let session = device.session().await;
    for sql in [
        "SELECT * FROM rocky_operations",
        "DELETE FROM rocky_incarnations",
        "CREATE TABLE other(a)",
        "PRAGMA journal_mode=DELETE",
        "SELECT 1; SELECT 2",
    ] {
        let denied = fails(&session, sql).await;
        assert!(
            matches!(denied, Error::Sql(SqlError::Denied)) || code(&denied) == Some(1),
            "{sql}: {denied}"
        );
    }
    assert!(session.prepare("SELECT * FROM rocky_outbox").await.is_err());
    run(&session, "BEGIN").await;
    run(&session, &insert("poisoned")).await;
    let denied = fails(&session, "UPDATE rocky_replica SET revision=99").await;
    assert!(matches!(denied, Error::Sql(SqlError::Denied)), "{denied}");
    let poisoned = fails(&session, "COMMIT").await;
    assert!(aborted(&poisoned, |reason| matches!(
        reason,
        AbortReason::Poisoned(_)
    )));
    // More row changes than the session may capture.
    let small = device
        .replica
        .sql_session(SqlOptions {
            captured_rows: 2,
            ..SqlOptions::default()
        })
        .await
        .expect("session");
    run(&small, "BEGIN").await;
    run(&small, &insert("s1")).await;
    run(&small, &insert("s2")).await;
    let over = fails(&small, &insert("s3")).await;
    assert!(matches!(over, Error::Sql(SqlError::Capture(_))), "{over}");
    run(&small, "ROLLBACK").await;
    let over = fails(
        &small,
        "INSERT INTO shopping_items(key,title,completed) VALUES('s1','1',0),('s2','2',0),('s3','3',0)",
    )
    .await;
    assert!(matches!(over, Error::Sql(SqlError::Capture(_))), "{over}");
    // A statement that runs past its budget is interrupted.
    let hurried = device
        .replica
        .sql_session(SqlOptions {
            statement_budget: Duration::from_millis(20),
            ..SqlOptions::default()
        })
        .await
        .expect("session");
    let endless =
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n) SELECT count(*) FROM n";
    let interrupted = fails(&hurried, endless).await;
    assert_eq!(code(&interrupted), Some(9), "{interrupted}");
    // A result larger than its bound is refused, not truncated.
    let narrow = device
        .replica
        .sql_session(SqlOptions {
            result_bytes: 8,
            ..SqlOptions::default()
        })
        .await
        .expect("session");
    let large = fails(&narrow, "SELECT id,title FROM shopping_items").await;
    assert!(matches!(large, Error::Quota { .. }), "{large}");

    // A dropped session rolls its transaction back; a cancelled one too.
    let dropped = device.session().await;
    run(&dropped, "BEGIN IMMEDIATE").await;
    run(&dropped, &insert("dropped")).await;
    drop(dropped);
    let cancelled = device.session().await;
    run(&cancelled, "BEGIN").await;
    run(&cancelled, &insert("cancelled")).await;
    cancelled.cancel();
    let after = fails(&cancelled, "SELECT 1").await;
    assert!(
        aborted(&after, |reason| *reason == AbortReason::Cancelled),
        "{after}"
    );
    run(&cancelled, "ROLLBACK").await;
    device
        .replica
        .mutate(mutation("m2"))
        .expect("queued")
        .committed()
        .await
        .expect("commit");
    let ids: Vec<_> = device
        .read(ALL)
        .await
        .into_iter()
        .map(|row| row[0].clone())
        .collect();
    assert_eq!(ids, [text("after"), text("m1"), text("m2")]);
    device.audited().await;
}

#[tokio::test]
async fn a_shared_file_domain_runs_sessions_under_its_physical_names() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let scopes: Vec<Scope> = [DOMAIN, DomainId([3; 16])]
        .into_iter()
        .map(|domain| Scope {
            domain,
            ..scope(&schema)
        })
        .collect();
    let domains = scopes
        .iter()
        .map(|scope| installation(1, &schema, *scope))
        .collect();
    let runtime = Runtime::open(dir.path().join("shared.sqlite"), domains)
        .await
        .expect("runtime");
    let (first, second) = (
        runtime.domain(scopes[0]).expect("domain"),
        runtime.domain(scopes[1]).expect("domain"),
    );
    let (one, two) = (
        first
            .sql_session(SqlOptions::default())
            .await
            .expect("session"),
        second
            .sql_session(SqlOptions::default())
            .await
            .expect("session"),
    );
    run(&one, "BEGIN").await;
    run(
        &one,
        "INSERT INTO shopping_items(key,title,completed) VALUES('a','First',0)",
    )
    .await;
    run(
        &one,
        "UPDATE shopping_items SET completed=1 WHERE shopping_items.key='a' AND title='First'",
    )
    .await;
    assert_eq!(operations(&run(&one, "COMMIT").await), 1);
    // The other domain of the file has its own table of the same name.
    assert_eq!(
        run(&two, "SELECT count(*) FROM shopping_items").await.rows,
        [[int(0)]]
    );
    run(
        &two,
        "INSERT INTO \"shopping_items\"(key,title,completed) VALUES('a','Second',0)",
    )
    .await;
    assert_eq!(
        run(
            &one,
            "SELECT title,completed FROM shopping_items AS s WHERE s.key='a'"
        )
        .await
        .rows,
        [[text("First"), int(1)]]
    );
    for replica in [&first, &second] {
        replica.checkpoint().await.expect("checkpoint");
        assert!(replica.audit().await.expect("audit").recomputed);
    }
    runtime.close().await.expect("close");
}
