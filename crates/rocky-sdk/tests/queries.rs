//! Query surface on the trusted profile: a schema
//! built from portable definition data, managed SQL, snapshot cursors, a
//! session catalog and committed table events. No Room schema is involved.
#[path = "support/trusted.rs"]
mod trusted;
use rocky_core::{
    DomainId, Envelope, LogicalSchema, MeshId, NodeId, PrincipalId, SchemaDefinition, Scope,
    SqlValue, TransactionToken,
};
use rocky_runtime::{
    AbortReason, Cause, CursorLimits, Error, Identity, Replica, RuntimeConfig, SqlError,
    SqlOptions, SqlRows, SqlSession,
};
use rocky_sdk::{Access, TypedApp};
use std::path::Path;

const CALLER: PrincipalId = PrincipalId([7; 16]);
const DOMAIN: DomainId = DomainId([3; 16]);
const DEFINITION: &str = r#"{
    "format":3,
    "publisher": "6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b",
    "application": "notebook",
    "tables": [
        {
            "name": "notes",
            "class": "mergeable",
            "columns": [
                {"name": "key", "type": "text", "not_null": true},
                {"name": "folder", "type": "text", "not_null": true},
                {"name": "body", "type": "text"}
            ],
            "primary_key": ["key"],
            "indexes": [{"name": "notes_by_folder", "columns": [{"name": "folder", "descending": false}]}]
        },
        {
            "name": "tags",
            "class": "mergeable",
            "columns": [
                {"name": "note", "type": "text", "not_null": true},
                {"name": "tag", "type": "text", "not_null": true}
            ],
            "primary_key": ["note", "tag"],
            "indexes": [{"name": "tags_by_tag", "columns": [{"name": "tag", "descending": false}]}]
        },
        {
            "name": "drafts",
            "class": "local_only",
            "columns": [{"name": "key", "type": "integer", "not_null": true}, {"name": "text", "type": "text"}],
            "primary_key": ["key"]
        }
    ]
}"#;

fn schema(definition: &str) -> LogicalSchema {
    SchemaDefinition::parse(definition.as_bytes())
        .and_then(|definition| definition.build())
        .expect("schema")
}
fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
fn nodes() -> Vec<NodeId> {
    (1..=2).map(|node| NodeId([node; 16])).collect()
}
async fn open(dir: &Path, node: u8, schema: &LogicalSchema) -> Result<Replica<TypedApp>, Error> {
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: schema.app_id(),
        domain: DOMAIN,
    };
    let app = trusted::app(schema, &[DOMAIN], &nodes());
    Replica::open_with_config(
        dir.join(format!("node{node}.sqlite")),
        app,
        Access {
            scope,
            caller: CALLER,
            nodes: nodes(),
        },
        Identity {
            scope,
            node: NodeId([node; 16]),
            caller: CALLER,
        },
        RuntimeConfig {
            authors: nodes().into_iter().map(|node| (node, CALLER)).collect(),
            delivery_targets: nodes(),
            ..RuntimeConfig::default()
        },
    )
    .await
}
async fn session(replica: &Replica<TypedApp>) -> SqlSession {
    replica
        .sql_session(SqlOptions::default())
        .await
        .expect("session")
}
async fn run(session: &SqlSession, sql: &str) -> SqlRows {
    trusted::run(session, sql, Vec::new())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}
async fn all(cursor: &mut rocky_runtime::SnapshotCursor) -> Vec<Vec<SqlValue>> {
    let mut rows = Vec::new();
    loop {
        let page = cursor.next().await.expect("page");
        rows.extend(page.rows);
        if page.done {
            return rows;
        }
    }
}
const JOIN: &str = "SELECT n.key, t.tag FROM notes AS n JOIN tags t ON t.note = n.key \
                    WHERE n.folder = ?1 ORDER BY n.key, t.tag";

#[tokio::test]
async fn a_definition_consumer_writes_with_savepoints_joins_and_reopens() {
    let dir = tempfile::tempdir().expect("dir");
    let schema = schema(DEFINITION);
    let replica = open(dir.path(), 1, &schema).await.expect("open");
    let sql = session(&replica).await;
    run(&sql, "BEGIN").await;
    run(
        &sql,
        "INSERT INTO notes VALUES('a','work','first'),('b','home','second')",
    )
    .await;
    run(&sql, "SAVEPOINT tagging").await;
    run(
        &sql,
        "INSERT INTO tags VALUES('a','urgent'),('a','later'),('b','urgent')",
    )
    .await;
    run(&sql, "SAVEPOINT scratch").await;
    run(&sql, "INSERT INTO tags VALUES('b','discarded')").await;
    run(&sql, "INSERT INTO drafts VALUES(1,'local')").await;
    run(&sql, "ROLLBACK TO scratch").await;
    run(&sql, "RELEASE tagging").await;
    let committed = sql.commit(TransactionToken([1; 32])).await.expect("commit");
    let outcome = committed.committed.expect("outcome");
    assert_eq!(outcome.operations.len(), 5);

    // The join reads both tables through their indexes, in one snapshot.
    let mut cursor = sql
        .cursor(JOIN, vec![text("work")], CursorLimits::default())
        .await
        .expect("cursor");
    assert_eq!(
        cursor
            .columns()
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["key", "tag"]
    );
    assert_eq!(
        all(&mut cursor).await,
        vec![
            vec![text("a"), text("later")],
            vec![text("a"), text("urgent")]
        ]
    );
    let plan = run(&sql, "SELECT count(*) FROM tags WHERE tag = 'urgent'").await;
    assert_eq!(plan.rows, vec![vec![SqlValue::Integer(2)]]);
    drop(cursor);
    drop(sql);
    replica.close().await.expect("close");

    // The same definition reopens the file; another one is refused.
    let reopened = open(dir.path(), 1, &schema).await.expect("reopen");
    let mut cursor = session(&reopened)
        .await
        .cursor(JOIN, vec![text("home")], CursorLimits::default())
        .await
        .expect("cursor");
    assert_eq!(
        all(&mut cursor).await,
        vec![vec![text("b"), text("urgent")]]
    );
    drop(cursor);
    reopened.close().await.expect("close");
    let changed = DEFINITION.replace(
        "\"body\", \"type\": \"text\"",
        "\"body\", \"type\": \"blob\"",
    );
    assert!(open(dir.path(), 1, &self::schema(&changed)).await.is_err());
}

#[tokio::test]
async fn the_index_serves_the_join() {
    let schema = schema(DEFINITION);
    let notes = schema.table("notes").expect("notes");
    assert!(
        notes
            .indexes
            .iter()
            .find(|index| index.name == "notes_by_folder")
            .unwrap()
            .create_sql
            .contains("ON \"notes\" (\"folder\")")
    );
    // The query planner's choice, checked on an unmanaged copy of the schema.
    let connection = rusqlite::Connection::open_in_memory().expect("memory");
    for table in &schema.tables {
        connection.execute(&table.create_sql, []).expect("table");
        for index in &table.indexes {
            connection.execute(&index.create_sql, []).expect("index");
        }
    }
    let plan: Vec<String> = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {JOIN}"))
        .expect("plan")
        .query_map(["work"], |row| row.get::<_, String>(3))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("plan rows");
    assert!(
        plan.iter().any(|step| step.contains("notes_by_folder")),
        "{plan:?}"
    );
    assert!(
        plan.iter()
            .any(|step| step.contains("sqlite_autoindex_tags") || step.contains("PRIMARY KEY")),
        "{plan:?}"
    );
}

#[tokio::test]
async fn unsupported_and_denied_sql_fails_before_any_effect_and_catalogs_hide_protocol_state() {
    let dir = tempfile::tempdir().expect("dir");
    let schema = schema(DEFINITION);
    let replica = open(dir.path(), 1, &schema).await.expect("open");
    let sql = session(&replica).await;
    run(&sql, "BEGIN").await;
    run(&sql, "INSERT INTO notes VALUES('a','work','kept')").await;
    // Outside the surface: refused, the transaction continues.
    for statement in [
        "SELECT * FROM notes INDEXED BY notes_by_folder",
        "WITH notes AS (SELECT 1) SELECT * FROM notes",
        "EXPLAIN QUERY PLAN SELECT * FROM notes",
    ] {
        let error = sql.run(statement, Vec::new()).await.expect_err(statement);
        assert!(
            matches!(error, Error::Sql(SqlError::Unsupported(_))),
            "{statement}: {error}"
        );
    }
    let committed = sql.commit(TransactionToken([2; 32])).await.expect("commit");
    assert_eq!(committed.committed.expect("outcome").operations.len(), 1);
    // Denied before the engine: catalogs, pragmas, functions and protocol.
    for statement in [
        "SELECT name FROM sqlite_schema",
        "SELECT * FROM pragma_table_list",
        "SELECT * FROM rocky_incarnations",
        "SELECT * FROM main.notes",
        "PRAGMA table_info(notes)",
        "CREATE VIEW v AS SELECT * FROM notes",
        "ATTACH DATABASE ':memory:' AS other",
        "SELECT load_extension('x')",
    ] {
        let error = sql.run(statement, Vec::new()).await.expect_err(statement);
        assert!(
            matches!(error, Error::Sql(SqlError::Denied)),
            "{statement}: {error}"
        );
        assert!(
            sql.cursor(statement, Vec::new(), CursorLimits::default())
                .await
                .is_err(),
            "{statement}"
        );
    }
    // A trigger body holds a second statement and never reaches the parser.
    let trigger = "CREATE TRIGGER t AFTER INSERT ON notes BEGIN DELETE FROM notes; END";
    assert!(sql.run(trigger, Vec::new()).await.is_err());
    // A cursor only reads.
    assert!(matches!(
        sql.cursor("DELETE FROM notes", Vec::new(), CursorLimits::default())
            .await,
        Err(Error::Sql(SqlError::Unsupported(_)))
    ));
    let catalog = sql.catalog().await.expect("catalog");
    let names: Vec<_> = catalog.iter().map(|table| table.name.as_str()).collect();
    assert_eq!(names, ["drafts", "notes", "tags"]);
    let tags = &catalog[2];
    assert_eq!(
        tags.columns
            .iter()
            .map(|c| (c.name.as_str(), c.key))
            .collect::<Vec<_>>(),
        [
            ("note", Some(0)),
            ("tag", Some(1)),
            ("id", None),
            ("row_kind", None)
        ]
    );
    assert_eq!(
        tags.indexes
            .iter()
            .find(|index| index.name == "tags_by_tag")
            .unwrap()
            .name,
        "tags_by_tag"
    );
    assert!(catalog.iter().all(|table| table.writable));
    let rows = run(&sql, "SELECT key, body FROM notes").await;
    assert_eq!(rows.rows, vec![vec![text("a"), text("kept")]]);
}

#[tokio::test]
async fn cursors_keep_their_snapshot_while_writes_commit() {
    let dir = tempfile::tempdir().expect("dir");
    let schema = schema(DEFINITION);
    let replica = open(dir.path(), 1, &schema).await.expect("open");
    let sql = session(&replica).await;
    for id in 0..10 {
        run(&sql, &format!("INSERT INTO notes VALUES('n{id}','f','')")).await;
    }
    let limits = CursorLimits {
        page_rows: 3,
        ..CursorLimits::default()
    };
    let mut cursor = sql
        .cursor("SELECT key FROM notes ORDER BY key", Vec::new(), limits)
        .await
        .expect("cursor");
    let revision = cursor.revision().expect("revision");
    let first = cursor.next().await.expect("page");
    assert_eq!(first.rows.len(), 3);
    // Writes commit while the cursor holds no writer lease.
    let writer = session(&replica).await;
    run(&writer, "DELETE FROM notes").await;
    run(&writer, "INSERT INTO notes VALUES('z','f','')").await;
    let mut rows = first.rows;
    rows.extend(all(&mut cursor).await);
    assert_eq!(rows.len(), 10);
    cursor.reset().await.expect("reset");
    assert_eq!(all(&mut cursor).await.len(), 10);
    let later = sql
        .cursor("SELECT key FROM notes", Vec::new(), limits)
        .await
        .expect("cursor");
    assert!(later.revision().expect("revision") > revision);
    // Closing the replica ends open cursors deterministically.
    drop(sql);
    drop(writer);
    replica.close().await.expect("close");
    assert!(cursor.next().await.is_err());
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

#[tokio::test]
async fn committed_table_events_wake_only_their_observers_once_committed() {
    let dir = tempfile::tempdir().expect("dir");
    let schema = schema(DEFINITION);
    let laptop = open(dir.path(), 1, &schema).await.expect("laptop");
    let phone = open(dir.path(), 2, &schema).await.expect("phone");
    // Registered before the initial snapshot.
    let mut notes = laptop.observe_tables(Some(&["notes"])).expect("observe");
    let mut tags = laptop.observe_tables(Some(&["TAGS"])).expect("observe");
    let mut everything = laptop.observe_tables(None).expect("observe");
    assert!(
        laptop
            .observe_tables(Some(&["rocky_incarnations"]))
            .is_err()
    );
    let registered = notes.registered()[&laptop.scope()];
    let sql = session(&laptop).await;
    // A rolled back transaction is never announced.
    run(&sql, "BEGIN").await;
    run(&sql, "INSERT INTO notes VALUES('r','f','')").await;
    run(&sql, "ROLLBACK").await;
    assert_eq!(notes.try_next().expect("open"), None);
    // Nor is an open one.
    run(&sql, "BEGIN").await;
    run(&sql, "INSERT INTO notes VALUES('a','f',''),('b','f','')").await;
    assert_eq!(notes.try_next().expect("open"), None);
    let committed = sql.commit(TransactionToken([3; 32])).await.expect("commit");
    let revision = committed.committed.expect("outcome").domains[0]
        .revision
        .get();
    let change = notes.next().await.expect("change");
    assert_eq!(change.tables, ["notes".to_string()].into());
    assert_eq!(change.causes, [Cause::Local].into());
    assert!(change.revisions[&laptop.scope()] >= revision && revision > registered);
    assert_eq!(tags.try_next().expect("open"), None);
    // A query after the event observes at least the announced revision.
    let mut cursor = sql
        .cursor("SELECT key FROM notes", Vec::new(), CursorLimits::default())
        .await
        .expect("cursor");
    assert!(cursor.revision().expect("revision") >= change.revisions[&laptop.scope()]);
    assert_eq!(all(&mut cursor).await.len(), 2);
    drop(cursor);
    // Several commits coalesce into one pending change.
    run(&sql, "INSERT INTO drafts VALUES(1,'x')").await;
    run(&sql, "UPDATE notes SET body='y' WHERE key='a'").await;
    let change = everything.next().await.expect("change");
    assert_eq!(
        change.tables,
        ["drafts".to_string(), "notes".to_string()].into()
    );
    assert_eq!(everything.try_next().expect("open"), None);
    let _ = notes.try_next();
    // Incoming sync wakes observers of the tables it changed.
    let phone_sql = session(&phone).await;
    run(&phone_sql, "INSERT INTO tags VALUES('a','synced')").await;
    let incoming = operations(&phone).await;
    for result in laptop.receive_page(incoming).await.expect("receive") {
        result.expect("receipt");
    }
    laptop.process_pending().await.expect("apply");
    let change = tags.next().await.expect("change");
    assert_eq!(change.causes, [Cause::Sync].into());
    assert_eq!(notes.try_next().expect("open"), None);
    assert!(Envelope::decode(&operations(&laptop).await[0]).is_ok());
    // Closing the domain ends observers.
    drop(sql);
    laptop.close().await.expect("close");
    assert!(matches!(tags.next().await, Err(Error::Closed)));
    let _ = AbortReason::Timeout;
}

/// Measures cursor reads and observer cost against the scale budgets. Run
/// with `cargo test --release -p rocky-sdk --test queries -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "measurement"]
async fn measure_cursor_reads_and_observer_cost() {
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().expect("dir");
    let schema = schema(DEFINITION);
    let replica = open(dir.path(), 1, &schema).await.expect("open");
    let sql = session(&replica).await;
    const ROWS: i64 = 100_000;
    let started = Instant::now();
    for chunk in 0..(ROWS / 50_000) {
        let first = chunk * 50_000;
        run(
            &sql,
            &format!(
                "INSERT INTO notes SELECT printf('n%07d', x), 'f' || (x % 100), '' FROM \
                 (WITH RECURSIVE c(x) AS (SELECT {first} UNION ALL SELECT x + 1 FROM c \
                 WHERE x < {}) SELECT x FROM c)",
                first + 49_999
            ),
        )
        .await;
    }
    println!("loaded {ROWS} rows in {:?}", started.elapsed());
    let median = |mut samples: Vec<Duration>| {
        samples.sort();
        samples[samples.len() / 2]
    };
    // An indexed point read: open a cursor, take its page, close it.
    let mut samples = Vec::new();
    for sample in 0..200 {
        let started = Instant::now();
        let mut cursor = sql
            .cursor(
                "SELECT key, body FROM notes WHERE key = ?1",
                vec![text(&format!("n{:07}", (sample * 997) % ROWS))],
                CursorLimits::default(),
            )
            .await
            .expect("cursor");
        assert_eq!(cursor.next().await.expect("page").rows.len(), 1);
        drop(cursor);
        samples.push(started.elapsed());
    }
    println!("cursor point read median {:?}", median(samples));
    let started = Instant::now();
    let mut cursor = sql
        .cursor(
            "SELECT key, folder FROM notes",
            Vec::new(),
            CursorLimits::default(),
        )
        .await
        .expect("cursor");
    let scanned = all(&mut cursor).await.len();
    drop(cursor);
    println!("cursor scan {scanned} rows in {:?}", started.elapsed());
    // Write latency with no observers, 1,000 of another table and 1,000 of this one.
    let writes = |label: &'static str| {
        let sql = &sql;
        async move {
            let mut samples = Vec::new();
            for write in 0..100 {
                let started = Instant::now();
                run(
                    sql,
                    &format!("UPDATE notes SET body = '{label}{write}' WHERE key = 'n0000001'"),
                )
                .await;
                samples.push(started.elapsed());
            }
            println!("{label}: write median {:?}", median(samples));
        }
    };
    writes("no observers").await;
    let unrelated: Vec<_> = (0..1000)
        .map(|_| replica.observe_tables(Some(&["tags"])).expect("observe"))
        .collect();
    writes("1000 observers of another table").await;
    let mut related: Vec<_> = (0..1000)
        .map(|_| replica.observe_tables(Some(&["notes"])).expect("observe"))
        .collect();
    writes("1000 observers of the written table").await;
    assert!(
        related
            .iter_mut()
            .all(|observer| observer.try_next().expect("open").is_some())
    );
    drop(unrelated);
}

#[tokio::test]
async fn compound_key_definition_captures_real_and_blob_on_the_trusted_profile() {
    let mut definition: serde_json::Value = serde_json::from_str(DEFINITION).unwrap();
    let columns = definition["tables"][1]["columns"].as_array_mut().unwrap();
    columns.push(serde_json::json!({"name":"reading", "type":"real"}));
    columns.push(serde_json::json!({"name":"raw", "type":"blob"}));
    let schema = schema(&definition.to_string());
    let dir = tempfile::tempdir().unwrap();
    let replica = open(dir.path(), 1, &schema).await.unwrap();
    let connection = rusqlite::Connection::open(dir.path().join("node1.sqlite")).unwrap();
    let installed: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND sql LIKE '%reading%' AND sql LIKE '%PRIMARY KEY%'",
        [], |row| row.get(0),
    ).unwrap();
    assert!(
        installed.contains("PRIMARY KEY(\"note\",\"tag\")"),
        "{installed}"
    );
    let indexes: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND tbl_name='tags' AND sql IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 2);
    drop(connection);
    let session = session(&replica).await;
    let row = vec![
        text("note"),
        text("tag"),
        SqlValue::real(1.25).unwrap(),
        SqlValue::Blob(vec![0, 255, 42]),
    ];
    trusted::run(
        &session,
        "INSERT INTO tags(note,tag,reading,raw) VALUES(?,?,?,?)",
        row.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        run(&session, "SELECT note,tag,reading,raw FROM tags")
            .await
            .rows,
        vec![row.clone()]
    );
    assert!(
        trusted::run(
            &session,
            "INSERT INTO tags(note,tag,reading,raw) VALUES('note','tag',2.0,X'00')",
            vec![]
        )
        .await
        .is_err()
    );
    drop(session);
    replica.close().await.expect("close before reopen");
    drop(replica);
    let reopened = open(dir.path(), 1, &schema).await.unwrap();
    assert_eq!(
        run(
            &self::session(&reopened).await,
            "SELECT note,tag,reading,raw FROM tags"
        )
        .await
        .rows,
        vec![row]
    );
    drop(reopened);
}
