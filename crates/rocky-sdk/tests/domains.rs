//! Bound databases over domains,
//! one-file multi-domain transactions and recorded placement.
#[path = "support/trusted.rs"]
mod trusted;
use rocky_core::{
    BindingError, DatabaseBinding, DomainId, Envelope, LogicalSchema, MeshId, NodeId, PrincipalId,
    Scope, SqlValue, TransactionToken,
};
use rocky_runtime::{
    BoundQuery, Database, DomainInstallation, DomainPlacement, Error, Identity, ReadOptions,
    Replica, Runtime, RuntimeConfig, RuntimeOptions, SqlError, SqlOptions, SqlRows, SqlSession,
    migrate_placement,
};
use rocky_sdk::{Access, TypedApp, generate_room};
use std::path::Path;

const SHOPPING: &str = include_str!("fixtures/shopping-identity.json");
const SHOPPING_POLICY: &str =
    include_str!("../../../examples/shoppinglist-app/shared/schemas/rocky-policy.json");
const CALLER: PrincipalId = PrincipalId([7; 16]);
const NODE: NodeId = NodeId([1; 16]);
const PEER: NodeId = NodeId([2; 16]);

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
fn int(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}
fn shopping() -> LogicalSchema {
    generate_room(SHOPPING, SHOPPING_POLICY).expect("shopping schema")
}
fn tenant(schema: &LogicalSchema, tenant: u16) -> Scope {
    let mut domain = [0; 16];
    domain[..2].copy_from_slice(&tenant.to_be_bytes());
    Scope {
        mesh: MeshId([1; 16]),
        app: schema.app_id(),
        domain: DomainId(domain),
    }
}
/// `admitted` unset installs a policy that refuses this node's own writes.
fn installation(
    node: NodeId,
    schema: &LogicalSchema,
    scope: Scope,
    admitted: bool,
) -> DomainInstallation<TypedApp> {
    DomainInstallation {
        app: trusted::app(schema, &[scope.domain], &[NODE, PEER]),
        policy: Access {
            scope,
            caller: CALLER,
            nodes: if admitted {
                vec![NODE, PEER]
            } else {
                vec![PEER]
            },
        },
        identity: Identity {
            scope,
            node,
            caller: CALLER,
        },
        config: RuntimeConfig {
            authors: vec![(NODE, CALLER), (PEER, CALLER)],
            delivery_targets: vec![NODE, PEER],
            ..RuntimeConfig::default()
        },
    }
}
fn placed(schema: &LogicalSchema, slots: &[(u16, usize)]) -> Vec<DomainPlacement> {
    slots
        .iter()
        .map(|(domain, file)| DomainPlacement {
            scope: tenant(schema, *domain),
            file: *file,
        })
        .collect()
}
fn options(files: usize) -> RuntimeOptions {
    RuntimeOptions {
        files,
        ..RuntimeOptions::default()
    }
}
async fn open(
    path: &Path,
    schema: &LogicalSchema,
    slots: &[(u16, usize)],
    bindings: Vec<DatabaseBinding>,
    files: usize,
) -> Result<Runtime<TypedApp>, Error> {
    let domains = slots
        .iter()
        .map(|(domain, _)| installation(NODE, schema, tenant(schema, *domain), true))
        .collect();
    Runtime::open_bound(
        path,
        domains,
        placed(schema, slots),
        bindings,
        options(files),
    )
    .await
}
async fn session(database: &Database<TypedApp>) -> SqlSession {
    database
        .sql_session(SqlOptions::default())
        .await
        .expect("session")
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
fn incompatible<T>(result: Result<T, Error>, part: &str) {
    match result {
        Err(Error::Incompatible(reason)) => assert!(reason.contains(part), "{reason}"),
        Err(error) => panic!("{error}"),
        Ok(_) => panic!("accepted; expected {part}"),
    }
}
/// A bound read of the domain's own connection: committed rows only.
async fn titles(replica: &Replica<TypedApp>, scope: Scope) -> Vec<Vec<SqlValue>> {
    let query = BoundQuery {
        sql: "SELECT key,title FROM shopping_items ORDER BY key".into(),
        values: Vec::new(),
    };
    replica
        .read(query, ReadOptions::new(scope, 1024))
        .await
        .expect("read")
        .rows
}
async fn operations(replica: &Replica<TypedApp>) -> Vec<Vec<u8>> {
    let (mut operations, mut after) = (Vec::new(), None);
    loop {
        let page = replica.operation_page(after).await.expect("page");
        operations.extend(page.items);
        match page.next {
            Some(next) => after = Some(next),
            None => return operations,
        }
    }
}
async fn audited(replica: &Replica<TypedApp>) {
    replica.checkpoint().await.expect("checkpoint");
    assert!(replica.audit().await.expect("audit").recomputed);
}
fn insert(table: &str, id: &str, title: &str) -> String {
    format!("INSERT INTO {table}(key,title,completed) VALUES('{id}','{title}',0)")
}
fn single(schema: &LogicalSchema, name: &str, domain: u16) -> DatabaseBinding {
    DatabaseBinding::single(name, tenant(schema, domain), schema).expect("binding")
}
fn pair(schema: &LogicalSchema, name: &str, first: u16, second: u16) -> DatabaseBinding {
    DatabaseBinding::new(name)
        .domain_as("a", tenant(schema, first), schema)
        .and_then(|binding| binding.domain_as("b", tenant(schema, second), schema))
        .expect("binding")
}

#[tokio::test]
async fn tenant_handles_with_identical_tables_and_keys_never_leak_or_overwrite() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("shared.sqlite");
    // Tenants 0 and 1 share a file; tenant 2 has its own.
    let slots = [(0, 0), (1, 0), (2, 1)];
    let bindings: Vec<_> = (0..3)
        .map(|domain| single(&schema, &format!("tenant{domain}"), domain))
        .collect();
    let runtime = open(&path, &schema, &slots, bindings, 2)
        .await
        .expect("open");
    let mut sessions = Vec::new();
    for domain in 0..3 {
        let database = runtime.database(&format!("tenant{domain}")).expect("bound");
        sessions.push(session(&database).await);
    }
    // The same statement text, table name and key in every tenant.
    let all = "SELECT key,title,completed FROM shopping_items ORDER BY key";
    let mut prepared = Vec::new();
    for (domain, session) in sessions.iter().enumerate() {
        let title = format!("Milk of {domain}");
        let outcome = run(session, &insert("shopping_items", "a", &title))
            .await
            .committed
            .expect("committed");
        assert_eq!(outcome.domains.len(), 1);
        assert_eq!(outcome.domains[0].scope, tenant(&schema, domain as u16));
        prepared.push(session.prepare(all).await.expect("prepare"));
    }
    run(&sessions[0], &insert("shopping_items", "b", "Bread")).await;
    for (domain, session) in sessions.iter().enumerate() {
        let rows = session
            .execute(&prepared[domain], Vec::new())
            .await
            .expect("execute")
            .rows;
        let own = vec![text("a"), text(&format!("Milk of {domain}")), int(0)];
        assert_eq!(rows[0], own);
        assert_eq!(rows.len(), if domain == 0 { 2 } else { 1 });
        // A statement compiled for another tenant's handle never runs here.
        let foreign = &prepared[(domain + 1) % 3];
        assert!(matches!(
            session.execute(foreign, Vec::new()).await,
            Err(Error::Sql(SqlError::Stale))
        ));
    }
    // Joins, aggregates and updates stay inside the tenant.
    let joined = run(
        &sessions[0],
        "SELECT count(*),min(other.title) FROM shopping_items JOIN shopping_items AS other ON other.completed=shopping_items.completed",
    )
    .await;
    assert_eq!(joined.rows, [[int(4), text("Bread")]]);
    assert_eq!(
        run(&sessions[0], "UPDATE shopping_items SET completed=1")
            .await
            .changes,
        2
    );
    assert_eq!(
        run(&sessions[1], "DELETE FROM shopping_items WHERE key='b'")
            .await
            .changes,
        0
    );
    for (domain, session) in sessions.iter().enumerate().skip(1) {
        let count = "SELECT count(*),sum(completed) FROM shopping_items";
        assert_eq!(run(session, count).await.rows, [[int(1), int(0)]]);
        let scope = tenant(&schema, domain as u16);
        let replica = runtime.domain(scope).expect("domain");
        let own = vec![text("a"), text(&format!("Milk of {domain}"))];
        assert_eq!(titles(&replica, scope).await, [own]);
    }
    // The file holds both tenants' tables; a session reaches only its own,
    // whatever name a statement uses.
    let file = rusqlite::Connection::open(&path).expect("file");
    let physical: Vec<String> = file
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name LIKE 'd%\\_shopping\\_items' ESCAPE '\\' AND name NOT LIKE '%ckpt%'")
        .and_then(|mut names| {
            let names = names.query_map([], |row| row.get(0))?;
            names.collect()
        })
        .expect("names");
    drop(file);
    assert_eq!(physical.len(), 2);
    let mut reached = 0;
    for name in &physical {
        match sessions[0]
            .run(&format!("SELECT count(*) FROM \"{name}\""), Vec::new())
            .await
        {
            Ok(rows) => {
                assert_eq!(rows.rows, [[int(2)]]);
                reached += 1;
            }
            Err(error) => assert!(matches!(error, Error::Sql(SqlError::Denied)), "{error}"),
        }
    }
    assert_eq!(reached, 1);
    for domain in 0..3 {
        audited(&runtime.domain(tenant(&schema, domain)).expect("domain")).await;
    }
    drop(sessions);
    runtime.close().await.expect("close");
}

#[tokio::test]
async fn a_multi_domain_transaction_commits_or_rolls_back_as_one() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("shared.sqlite");
    let scopes: Vec<_> = (0..3).map(|domain| tenant(&schema, domain)).collect();
    // The third domain's policy refuses this node's writes.
    let domains = (0..3)
        .map(|index| installation(NODE, &schema, scopes[index], index != 2))
        .collect();
    let bindings = vec![pair(&schema, "pair", 0, 1), pair(&schema, "mixed", 0, 2)];
    let runtime = Runtime::open_bound(
        &path,
        domains,
        placed(&schema, &[(0, 0), (1, 0), (2, 0)]),
        bindings,
        options(1),
    )
    .await
    .expect("open");
    let replicas: Vec<_> = scopes
        .iter()
        .map(|scope| runtime.domain(*scope).expect("domain"))
        .collect();
    let database = runtime.database("pair").expect("bound");
    let both = session(&database).await;
    let token = TransactionToken([5; 32]);
    // Replacing statements, so the same body can run again under its token.
    let body = [
        insert("a_shopping_items", "x", "Left").replace("INSERT", "INSERT OR REPLACE"),
        insert("b_shopping_items", "x", "Right").replace("INSERT", "INSERT OR REPLACE"),
        "INSERT OR REPLACE INTO b_shopping_items(key,title,completed) SELECT 'copy',title,1 FROM a_shopping_items".into(),
    ];
    run(&both, "BEGIN").await;
    let mut replay = Vec::new();
    for sql in &body {
        let sql = trusted::rewrite(&both, sql).await.expect("identity");
        both.run(&sql, vec![]).await.expect("statement");
        replay.push(sql);
    }
    run(&both, "ROLLBACK").await;
    run(&both, "BEGIN").await;
    for sql in &replay {
        both.run(sql, vec![]).await.expect("statement");
    }
    // The session reads its own rows across both domains; nobody else does.
    let joined = "SELECT a_shopping_items.title,b_shopping_items.title FROM a_shopping_items JOIN b_shopping_items ON a_shopping_items.key=b_shopping_items.key";
    assert_eq!(
        run(&both, joined).await.rows,
        [[text("Left"), text("Right")]]
    );
    let committed = both.commit(token).await.expect("commit").committed;
    let outcome = committed.expect("committed");
    assert_eq!(outcome.domains.len(), 2);
    assert_eq!(
        (outcome.domains[0].scope, outcome.domains[1].scope),
        (scopes[0], scopes[1])
    );
    assert_eq!(
        (
            outcome.domains[0].operations.len(),
            outcome.domains[1].operations.len(),
            outcome.operations.len()
        ),
        (1, 2, 3)
    );
    assert_eq!(
        titles(&replicas[0], scopes[0]).await,
        [[text("x"), text("Left")]]
    );
    assert_eq!(
        titles(&replicas[1], scopes[1]).await,
        [[text("copy"), text("Left")], [text("x"), text("Right")]]
    );
    // The token recovers one outcome for both domains, and commits once.
    let recovered = database.sql_outcome(token).await.expect("outcome");
    let recovered = recovered.expect("recorded");
    assert!(recovered.replayed);
    assert_eq!(
        (&recovered.intent, &recovered.operations, &recovered.domains),
        (&outcome.intent, &outcome.operations, &outcome.domains)
    );
    run(&both, "BEGIN").await;
    for sql in &replay {
        both.run(sql, vec![]).await.expect("replay statement");
    }
    run(&both, joined).await;
    let again = both.commit(token).await.expect("replay").committed;
    assert!(again.expect("replayed").replayed);
    assert_eq!(
        run(&both, "SELECT count(*) FROM b_shopping_items")
            .await
            .rows,
        [[int(2)]]
    );
    assert_eq!(
        database
            .sql_outcome(TransactionToken([6; 32]))
            .await
            .expect("none"),
        None
    );
    // A name that is not bound is not a table, and a domain's own name is not bound.
    assert!(matches!(
        fails(&both, "SELECT * FROM shopping_items").await,
        Error::Sql(SqlError::Statement { code: 1, .. })
    ));

    // One domain refuses its operation: neither domain keeps anything.
    let revisions = [
        replicas[0].query().await.expect("query").revision,
        replicas[2].query().await.expect("query").revision,
    ];
    let mixed = session(&runtime.database("mixed").expect("bound")).await;
    run(&mixed, "BEGIN").await;
    run(&mixed, &insert("a_shopping_items", "y", "Kept back")).await;
    run(&mixed, &insert("b_shopping_items", "y", "Denied")).await;
    assert_eq!(
        run(&mixed, "SELECT count(*) FROM a_shopping_items")
            .await
            .rows,
        [[int(2)]]
    );
    let denied = mixed.commit(TransactionToken([8; 32])).await.unwrap_err();
    assert!(matches!(denied, Error::Application(_)), "{denied}");
    assert_eq!(
        titles(&replicas[0], scopes[0]).await,
        [[text("x"), text("Left")]]
    );
    assert!(titles(&replicas[2], scopes[2]).await.is_empty());
    assert_eq!(
        replicas[0].query().await.expect("query").revision,
        revisions[0]
    );
    assert_eq!(
        replicas[2].query().await.expect("query").revision,
        revisions[1]
    );
    assert_eq!(operations(&replicas[0]).await.len(), 1);
    assert_eq!(
        runtime
            .database("mixed")
            .expect("bound")
            .sql_outcome(TransactionToken([8; 32]))
            .await
            .expect("none"),
        None
    );
    // The admitted domain continues from the counters it had.
    let next = run(&mixed, &insert("a_shopping_items", "z", "After")).await;
    let next = next.committed.expect("committed");
    assert_eq!(next.domains[0].operations[0].sequence.get(), 2);
    assert_eq!(next.domains[1].operations.len(), 0);

    // Each domain's operations replicate on their own and reproduce its rows.
    let peer = installation(PEER, &schema, scopes[1], true);
    let peer = Replica::open_with_config(
        dir.path().join("peer.sqlite"),
        peer.app,
        peer.policy,
        peer.identity,
        peer.config,
    )
    .await
    .expect("peer");
    let sent = operations(&replicas[1]).await;
    assert_eq!(sent.len(), 2);
    assert!(
        sent.iter()
            .all(|bytes| Envelope::decode(bytes).expect("operation").scope == scopes[1])
    );
    for result in peer.receive_page(sent).await.expect("receive") {
        result.expect("receipt");
    }
    peer.process_pending().await.expect("apply");
    assert_eq!(
        titles(&peer, scopes[1]).await,
        titles(&replicas[1], scopes[1]).await
    );
    for replica in &replicas {
        audited(replica).await;
    }
    peer.close().await.expect("close");
    drop((both, mixed));
    runtime.close().await.expect("close");
}

#[tokio::test]
async fn bindings_are_validated_recorded_and_changed_only_by_generation() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("shared.sqlite");
    let split = [(0, 0), (1, 1)];
    // Two domains of one schema cannot answer to the same names.
    assert!(matches!(
        DatabaseBinding::single("both", tenant(&schema, 0), &schema)
            .and_then(|binding| binding.domain(tenant(&schema, 1), &schema)),
        Err(BindingError::Ambiguous(_))
    ));
    // A binding across file slots is refused before any file is created.
    incompatible(
        open(&path, &schema, &split, vec![pair(&schema, "pair", 0, 1)], 2).await,
        "blocked capability",
    );
    assert!(!path.exists());
    let together = [(0, 0), (1, 0), (2, 1)];
    // A domain that is assigned but not installed here is unavailable, and
    // a table its domain lacks is not created.
    incompatible(
        open(
            &path,
            &schema,
            &together[..2],
            vec![single(&schema, "other", 2)],
            2,
        )
        .await,
        "not installed here",
    );
    let missing = DatabaseBinding::new("missing").table("items", tenant(&schema, 0), "absent");
    incompatible(
        open(
            &path,
            &schema,
            &together,
            vec![missing.expect("binding")],
            2,
        )
        .await,
        "does not have",
    );
    assert!(!path.exists());

    let first = pair(&schema, "pair", 0, 1);
    let runtime = open(&path, &schema, &together, vec![first.clone()], 2).await;
    let runtime = runtime.expect("open");
    let bound = session(&runtime.database("pair").expect("bound")).await;
    run(&bound, &insert("a_shopping_items", "a", "Recorded")).await;
    let prepared = bound.prepare("SELECT title FROM a_shopping_items").await;
    let prepared = prepared.expect("prepare");
    incompatible(runtime.database("absent").map(|_| ()), "not open here");
    drop(bound);
    runtime.close().await.expect("close");

    // The recorded binding is kept when a process opens without it.
    let runtime = open(&path, &schema, &together, Vec::new(), 2).await;
    let runtime = runtime.expect("open");
    incompatible(runtime.database("pair").map(|_| ()), "not open here");
    runtime.close().await.expect("close");
    // Changed tables need exactly the next generation.
    let changed = DatabaseBinding::new("pair")
        .domain_as("a", tenant(&schema, 0), &schema)
        .expect("binding");
    for generation in [1, 3] {
        let changed = changed.clone().generation(generation);
        incompatible(
            open(&path, &schema, &together, vec![changed], 2).await,
            "next generation",
        );
    }
    let second = changed.generation(2);
    let runtime = open(&path, &schema, &together, vec![second.clone()], 2).await;
    let runtime = runtime.expect("open");
    let bound = session(&runtime.database("pair").expect("bound")).await;
    // A statement of the earlier generation is refused, not rebound.
    assert!(matches!(
        bound.execute(&prepared, Vec::new()).await,
        Err(Error::Sql(SqlError::Stale))
    ));
    assert_eq!(
        run(&bound, "SELECT title FROM a_shopping_items").await.rows,
        [[text("Recorded")]]
    );
    assert!(
        fails(&bound, "SELECT * FROM b_shopping_items")
            .await
            .to_string()
            .contains("no such table")
    );
    drop(bound);
    runtime.close().await.expect("close");
    incompatible(
        open(&path, &schema, &together, vec![first], 2).await,
        "next generation",
    );
    open(&path, &schema, &together, vec![second], 2)
        .await
        .expect("open")
        .close()
        .await
        .expect("close");
}

#[tokio::test]
async fn placement_migration_colocates_domains_and_keeps_their_history() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("shared.sqlite");
    let apart = [(0, 0), (1, 1), (2, 1)];
    let token = TransactionToken([4; 32]);
    let runtime = open(&path, &schema, &apart, Vec::new(), 4).await;
    let runtime = runtime.expect("open");
    for domain in 0..3 {
        let replica = runtime.domain(tenant(&schema, domain)).expect("domain");
        let own = replica.sql_session(SqlOptions::default()).await;
        let own = own.expect("session");
        run(&own, "BEGIN").await;
        run(
            &own,
            &insert("shopping_items", "a", &format!("Of {domain}")),
        )
        .await;
        run(&own, &insert("shopping_items", "b", "Second")).await;
        run(&own, "DELETE FROM shopping_items WHERE key='b'").await;
        own.commit(token).await.expect("commit");
        run(&own, "UPDATE shopping_items SET completed=1").await;
    }
    // A running runtime keeps its files: nothing is recorded or moved.
    let moves = placed(&schema, &[(1, 0)]);
    assert!(migrate_placement(&path, moves.clone()).await.is_err());
    let before = operations(&runtime.domain(tenant(&schema, 1)).expect("domain")).await;
    runtime.close().await.expect("close");
    open(&path, &schema, &apart, Vec::new(), 4)
        .await
        .expect("no migration is pending")
        .close()
        .await
        .expect("close");

    incompatible(
        open(&path, &schema, &apart, vec![pair(&schema, "pair", 0, 1)], 4).await,
        "blocked capability",
    );
    let recorded = migrate_placement(&path, moves).await.expect("migrate");
    let together = [(0, 0), (1, 0), (2, 1)];
    assert_eq!(recorded, placed(&schema, &together));
    // The earlier assignment is gone; the recorded one opens the binding.
    incompatible(
        open(&path, &schema, &apart, Vec::new(), 4).await,
        "cannot be removed or moved",
    );
    let bindings = vec![pair(&schema, "pair", 0, 1)];
    let runtime = open(&path, &schema, &together, bindings.clone(), 4).await;
    let runtime = runtime.expect("open");
    let moved = runtime.domain(tenant(&schema, 1)).expect("domain");
    assert_eq!(
        titles(&moved, tenant(&schema, 1)).await,
        [[text("a"), text("Of 1")]]
    );
    assert_eq!(operations(&moved).await, before);
    let outcome = moved.sql_outcome(token).await.expect("outcome");
    assert_eq!(outcome.expect("recorded").operations.len(), 1);
    let bound = session(&runtime.database("pair").expect("bound")).await;
    run(&bound, "BEGIN").await;
    run(&bound, "UPDATE a_shopping_items SET title='Joined'").await;
    run(
        &bound,
        "UPDATE b_shopping_items SET title=(SELECT title FROM a_shopping_items)",
    )
    .await;
    let committed = run(&bound, "COMMIT").await.committed.expect("committed");
    assert_eq!(committed.operations.len(), 2);
    // The moved domain continues its own author sequence.
    assert_eq!(committed.domains[1].operations[0].sequence.get(), 3);
    for domain in 0..3 {
        let scope = tenant(&schema, domain);
        let replica = runtime.domain(scope).expect("domain");
        let title = if domain == 2 { "Of 2" } else { "Joined" };
        assert_eq!(titles(&replica, scope).await, [[text("a"), text(title)]]);
        audited(&replica).await;
    }
    drop(bound);
    runtime.close().await.expect("close");

    // A recorded binding's domains move together or not at all.
    incompatible(
        migrate_placement(&path, placed(&schema, &[(1, 2)])).await,
        "blocked capability",
    );
    let recorded = migrate_placement(&path, placed(&schema, &[(0, 3), (1, 3)])).await;
    let last = [(0, 3), (1, 3), (2, 1)];
    assert_eq!(recorded.expect("migrate"), placed(&schema, &last));
    assert_eq!(
        migrate_placement(&path, Vec::new()).await.expect("report"),
        placed(&schema, &last)
    );
    let runtime = open(&path, &schema, &last, bindings, 4)
        .await
        .expect("open");
    let bound = session(&runtime.database("pair").expect("bound")).await;
    assert_eq!(
        run(&bound, "SELECT a_shopping_items.title,b_shopping_items.completed FROM a_shopping_items,b_shopping_items").await.rows,
        [[text("Joined"), int(1)]]
    );
    drop(bound);
    runtime.close().await.expect("close");
    // The file both domains left holds none of their tables.
    let left = rusqlite::Connection::open(&path).expect("file");
    let tables: i64 = left
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'd%'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(tables, 0);
}

#[tokio::test]
async fn the_supported_limits_open_only_selected_files_with_one_writer_each() {
    let schema = shopping();
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("wide.sqlite");
    // 4,096 assigned domains over 256 slots, 16 to a slot.
    let assigned: Vec<(u16, usize)> = (0..4096)
        .map(|domain| (domain, domain as usize % 256))
        .collect();
    let placement = placed(&schema, &assigned);
    let serve = async |domains: &[u16], bindings: Vec<DatabaseBinding>| {
        let installed = domains
            .iter()
            .map(|domain| installation(NODE, &schema, tenant(&schema, *domain), true))
            .collect();
        Runtime::open_bound(&path, installed, placement.clone(), bindings, options(256)).await
    };
    // Domains 5 and 261 share slot 5; domain 4095 is alone in slot 255.
    let first = serve(&[5, 261], vec![pair(&schema, "pair", 5, 261)]).await;
    let first = first.expect("open");
    let second = serve(&[4095], vec![single(&schema, "last", 4095)]).await;
    let second = second.expect("disjoint files coexist");
    // A second writer of an owned file is refused.
    assert!(serve(&[5], Vec::new()).await.is_err());
    assert!(serve(&[4095], Vec::new()).await.is_err());
    incompatible(
        serve(&[5, 6], vec![pair(&schema, "apart", 5, 6)]).await,
        "blocked capability",
    );
    let both = session(&first.database("pair").expect("bound")).await;
    run(&both, "BEGIN").await;
    run(&both, &insert("a_shopping_items", "a", "Slot five")).await;
    run(&both, &insert("b_shopping_items", "a", "Slot five too")).await;
    assert_eq!(
        run(&both, "COMMIT")
            .await
            .committed
            .expect("committed")
            .operations
            .len(),
        2
    );
    let last = session(&second.database("last").expect("bound")).await;
    run(&last, &insert("shopping_items", "a", "Slot 255")).await;
    drop((both, last));
    first.close().await.expect("close");
    second.close().await.expect("close");
    // Two data files exist; no other slot was created, opened or attached.
    let mut files: Vec<String> = std::fs::read_dir(dir.path())
        .expect("dir")
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.ends_with(".sqlite"))
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "wide-255.sqlite",
            "wide-5.sqlite",
            "wide.sqlite-placement.sqlite"
        ]
    );
    let reopened = serve(&[261, 4095], Vec::new()).await.expect("reopen");
    for (domain, title) in [(261, "Slot five too"), (4095, "Slot 255")] {
        let scope = tenant(&schema, domain);
        let replica = reopened.domain(scope).expect("domain");
        assert_eq!(titles(&replica, scope).await, [[text("a"), text(title)]]);
    }
    assert!(reopened.domain(tenant(&schema, 5)).is_err());
    reopened.close().await.expect("close");
}

#[tokio::test]
async fn bound_cursors_catalogs_and_events_use_the_bound_names() {
    let dir = tempfile::tempdir().expect("dir");
    let schema = shopping();
    let both = DatabaseBinding::new("both")
        .domain_as("a", tenant(&schema, 1), &schema)
        .expect("a")
        .domain_as("b", tenant(&schema, 2), &schema)
        .expect("b");
    let runtime = open(
        &dir.path().join("bound.sqlite"),
        &schema,
        &[(1, 0), (2, 0)],
        vec![both],
        1,
    )
    .await
    .expect("open");
    let database = runtime.database("both").expect("database");
    let mut first = database
        .observe_tables(Some(&["a_shopping_items"]))
        .expect("observe");
    let mut every = database.observe_tables(None).expect("observe");
    assert!(database.observe_tables(Some(&["shopping_items"])).is_err());
    let sql = session(&database).await;
    run(
        &sql,
        "INSERT INTO b_shopping_items(key,title,completed) VALUES('x','Milk',0)",
    )
    .await;
    let change = every.next().await.expect("change");
    assert_eq!(change.tables, ["b_shopping_items".to_string()].into());
    assert_eq!(
        change.revisions.keys().copied().collect::<Vec<_>>(),
        [tenant(&schema, 2)]
    );
    assert_eq!(first.try_next().expect("open"), None);
    run(
        &sql,
        "INSERT INTO a_shopping_items(key,title,completed) VALUES('x','Tea',0)",
    )
    .await;
    assert_eq!(
        first.next().await.expect("change").tables,
        ["a_shopping_items".to_string()].into()
    );
    // One snapshot over both co-located tenants, aliased and joined.
    let mut cursor = sql
        .cursor(
            "SELECT a.title, b.title FROM a_shopping_items a JOIN b_shopping_items b USING(key)",
            Vec::new(),
            rocky_runtime::CursorLimits::default(),
        )
        .await
        .expect("cursor");
    assert_eq!(
        cursor.next().await.expect("page").rows,
        vec![vec![text("Tea"), text("Milk")]]
    );
    let catalog = sql.catalog().await.expect("catalog");
    assert!(
        catalog
            .iter()
            .all(|table| table.name.starts_with("a_") || table.name.starts_with("b_")),
        "{catalog:?}"
    );
    assert!(catalog.iter().any(|table| table.name == "b_shopping_items"));
    drop(cursor);
    drop(sql);
    runtime.close().await.expect("close");
}
