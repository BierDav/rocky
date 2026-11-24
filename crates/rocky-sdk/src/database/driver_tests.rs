use super::Connection;
use crate::test_snapshot as snap;
use crate::{
    Configuration, Database, Network, SqlValue, connection::ConnectionOptions, connection::Page,
    transport::memory::Hub,
};
use rocky_credentials::testing::MemoryKeys;
use serde_json::{Value, json};
use sqlparser::{
    ast::{Expr, Ident, SelectItem, SetExpr, Statement},
    dialect::SQLiteDialect,
    parser::Parser,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

type Failure = crate::Failure;

const SQLITE_ERROR: i32 = 1;
const SQLITE_READONLY: i32 = 8;
const SQLITE_TOOBIG: i32 = 18;
const SQLITE_AUTH: i32 = 23;
const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;

const SHOPPING: &str = r#"{"format":7,"application":"shopping","version":3,"tables":[
 {"name":"lists","class":"replayed","columns":[
   {"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],
  "primary_key":["key"]},
 {"name":"items","class":"replayed","columns":[
   {"name":"key","type":"text","not_null":true},{"name":"list","type":"text","not_null":true},
   {"name":"title","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},
   {"name":"note","type":"blob"}],
  "primary_key":["key"],
  "indexes":[{"name":"items_by_list","columns":[{"name":"list","descending":false},{"name":"title","descending":true}]}]}
]}"#;

static KEYS: LazyLock<Mutex<HashMap<PathBuf, Arc<MemoryKeys>>>> = LazyLock::new(Default::default);
fn open(directory: &Path, network: Option<Network>) -> Result<Configuration, Failure> {
    let keys: Arc<MemoryKeys> = KEYS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(directory.to_path_buf())
        .or_default()
        .clone();
    let configuration = Configuration::new(directory, snap::schema(SHOPPING))?
        .secrets(keys)
        .sync(crate::SyncMode::Manual);
    Ok(match network {
        Some(network) => configuration.network(network),
        None => configuration,
    })
}
async fn create(configuration: Configuration) -> Result<Database, Failure> {
    let database = configuration.create().await?.database;
    let mut tx = database.transaction().await?;
    for table in ["lists", "items"] {
        let id = tx.next_id(table, Vec::new()).await?;
        assert_eq!(id.sequence(), 1, "{table}");
    }
    tx.rollback().await?;
    Ok(database)
}
fn expression(sql: &str) -> Expr {
    let Statement::Query(query) = Parser::parse_sql(&SQLiteDialect {}, &format!("SELECT {sql}"))
        .unwrap()
        .remove(0)
    else {
        panic!("fixture expression")
    };
    let SetExpr::Select(select) = *query.body else {
        panic!("fixture select")
    };
    let SelectItem::UnnamedExpr(expr) = select.projection.into_iter().next().unwrap() else {
        panic!("fixture expression")
    };
    expr
}
async fn identity_insert(connection: &Connection, sql: &str) -> Result<String, Failure> {
    let Ok(mut statements) = Parser::parse_sql(&SQLiteDialect {}, sql) else {
        return Ok(sql.into());
    };
    if statements.len() != 1 {
        return Ok(sql.into());
    }
    let statement = match &mut statements[0] {
        Statement::Query(query) => match query.body.as_mut() {
            SetExpr::Insert(statement) => statement,
            _ => return Ok(sql.into()),
        },
        statement => statement,
    };
    let Statement::Insert(insert) = statement else {
        return Ok(sql.into());
    };
    let table = insert.table.to_string();
    if !["lists", "items"].contains(&table.as_str()) {
        return Ok(sql.into());
    }
    insert.columns.push(Ident::new("id").into());
    let source = insert.source.as_mut().unwrap();
    let query = source.to_string();
    match source.body.as_mut() {
        SetExpr::Values(values) => {
            for row in &mut values.rows {
                let id = allocated_identity(connection, &table).await?;
                row.push(expression(&format!("X'{id}'")));
            }
        }
        SetExpr::Select(select) => {
            let statement = connection.statement();
            let sql = format!("SELECT count(*) FROM ({query})");
            let mut page = connection.execute(statement, sql, vec![]).await?;
            let rows = match page.rows.first().map(Vec::as_slice) {
                Some([SqlValue::Integer(rows)]) => *rows,
                _ => return Err("fixture row count".into()),
            };
            while !page.done {
                page = connection.next(statement).await?;
            }
            let mut arms = String::new();
            for row in 1..=rows {
                let id = allocated_identity(connection, &table).await?;
                arms.push_str(&format!(" WHEN {row} THEN X'{id}'"));
            }
            let id = if arms.is_empty() {
                "NULL".to_owned()
            } else {
                format!("CASE row_number() OVER (){arms} END")
            };
            select
                .projection
                .push(SelectItem::UnnamedExpr(expression(&id)));
        }
        _ => panic!("fixture insert"),
    }
    Ok(statements[0].to_string())
}
fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
const WRITER: ConnectionOptions = ConnectionOptions {
    read_only: false,
    transaction_timeout: None,
};
const READER: ConnectionOptions = ConnectionOptions {
    read_only: true,
    transaction_timeout: None,
};

async fn all(
    connection: &Connection,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<Vec<Vec<SqlValue>>, Failure> {
    let statement = connection.statement();
    let mut page: Page = connection
        .execute(statement, identity_insert(connection, sql).await?, params)
        .await?;
    let mut rows = std::mem::take(&mut page.rows);
    while !page.done {
        page = connection.next(statement).await?;
        rows.append(&mut page.rows);
    }
    Ok(rows)
}
async fn allocated_identity(
    connection: &Connection,
    table: &str,
) -> Result<rocky_sdk::RowIdentity, Failure> {
    let statement = connection.statement();
    let mut page = connection
        .execute(
            statement,
            format!("SELECT rocky_next_id('{table}')"),
            vec![],
        )
        .await?;
    let id = match page.rows.first().and_then(|row| row.first()) {
        Some(SqlValue::Blob(id)) => rocky_sdk::RowIdentity::from_bytes(id)?,
        _ => return Err("allocator returned no identity".into()),
    };
    while !page.done {
        page = connection.next(statement).await?;
    }
    Ok(id)
}
async fn run(connection: &Connection, sql: &str) -> Result<(), Failure> {
    all(connection, sql, Vec::new()).await.map(drop)
}
async fn count(connection: &Connection, table: &str) -> Result<i64, Failure> {
    match all(
        connection,
        &format!("SELECT count(*) FROM {table}"),
        Vec::new(),
    )
    .await?[..]
    {
        [ref row] => match row[..] {
            [SqlValue::Integer(count)] => Ok(count),
            _ => Err("count".into()),
        },
        _ => Err("count".into()),
    }
}
fn published(directory: &Path) -> Result<(i64, i64), Failure> {
    let file = rusqlite::Connection::open_with_flags(
        directory.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let bundles = file.query_row(
        "SELECT count(DISTINCT handle) FROM secure_bundle_handles",
        [],
        |row| row.get(0),
    )?;
    let frames = file.query_row("SELECT count(*) FROM secure_operations", [], |row| {
        row.get(0)
    })?;
    Ok((bundles, frames))
}

#[tokio::test]
async fn a_writer_transaction_is_one_bundle_and_savepoints_nest() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = create(open(root.path(), None)?).await?;
    let writer = database.connect(WRITER).await?;
    let reader = database.connect(READER).await?;
    let before = published(root.path())?;

    run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
    assert!(writer.in_transaction().await?);
    all(
        &writer,
        "INSERT INTO lists(key,title) VALUES(?,?)",
        vec![text("l1"), text("Groceries")],
    )
    .await?;
    for (key, title) in [("i1", "Milk"), ("i2", "Eggs"), ("i3", "Jam")] {
        all(
            &writer,
            "INSERT INTO items(key,list,title,completed,note) VALUES(?,?,?,0,?)",
            vec![
                text(key),
                text("l1"),
                text(title),
                SqlValue::Blob(vec![1, 2]),
            ],
        )
        .await?;
    }
    run(&writer, "SAVEPOINT 'outer'").await?;
    run(&writer, "SAVEPOINT 'inner'").await?;
    run(&writer, "DELETE FROM items WHERE key = 'i3'").await?;
    run(&writer, "ROLLBACK TRANSACTION TO SAVEPOINT 'inner'").await?;
    run(&writer, "RELEASE SAVEPOINT 'inner'").await?;
    run(&writer, "UPDATE items SET completed = 1 WHERE key = 'i1'").await?;
    run(&writer, "RELEASE SAVEPOINT 'outer'").await?;
    assert_eq!(
        count(&writer, "items").await?,
        3,
        "the writer reads its own writes; a nested savepoint's rollback undoes only its delete"
    );
    run(&writer, "END TRANSACTION").await?;
    assert!(!writer.in_transaction().await?);
    assert_eq!(count(&reader, "items").await?, 3);
    let rows = all(
        &reader,
        "SELECT title, completed, note FROM items ORDER BY key",
        Vec::new(),
    )
    .await?;
    assert_eq!(
        rows[0],
        vec![
            text("Milk"),
            SqlValue::Integer(1),
            SqlValue::Blob(vec![1, 2])
        ]
    );
    let after = published(root.path())?;
    assert_eq!(
        (after.0 - before.0, after.1 - before.1),
        (1, 4),
        "parent and children publish one bundle: a frame per table's scope and the outcome record"
    );

    run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
    all(
        &writer,
        "INSERT INTO lists(key,title) VALUES('gone','Temporary')",
        Vec::new(),
    )
    .await?;
    run(&writer, "DELETE FROM lists WHERE key = 'gone'").await?;
    run(&writer, "END TRANSACTION").await?;
    let after = published(root.path())?;
    assert_eq!(
        after,
        (before.0 + 2, before.1 + 5),
        "an insert deleted in the same transaction still replicates: another replay may differ"
    );
    run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
    run(&writer, "END TRANSACTION").await?;
    assert_eq!(
        published(root.path())?,
        after,
        "a transaction that only began and ended publishes nothing"
    );

    run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
    let duplicate = all(
        &writer,
        "INSERT INTO lists(key,title) VALUES('l1','again')",
        Vec::new(),
    )
    .await
    .err()
    .ok_or("duplicate key")?;
    let duplicate = duplicate
        .downcast::<rocky_sdk::SqlFailure>()
        .map_err(|_| "code")?;
    assert_eq!(
        duplicate.code, SQLITE_CONSTRAINT_PRIMARYKEY,
        "engine errors keep SQLite's extended codes: {duplicate}"
    );
    assert!(
        writer.in_transaction().await?,
        "an engine error leaves the transaction open"
    );
    run(&writer, "ROLLBACK TRANSACTION").await?;
    for sql in [
        "PRAGMA user_version",
        "SELECT * FROM sqlite_master",
        "SELECT * FROM secure_operations",
    ] {
        let error = all(&writer, sql, Vec::new()).await.err().ok_or(sql)?;
        let error = error
            .downcast::<rocky_sdk::SqlFailure>()
            .map_err(|_| "code")?;
        assert_eq!(
            error.code, SQLITE_AUTH,
            "the engine catalog and pragmas stay denied: {sql}: {error}"
        );
    }
    writer.close().await;
    reader.close().await;
    database.close().await?;
    Ok(())
}

async fn exact(
    connection: &Connection,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<Vec<Vec<SqlValue>>, rocky_sdk::SqlFailure> {
    let page = connection
        .execute(connection.statement(), sql.into(), params)
        .await?;
    assert!(page.done);
    Ok(page.rows)
}

#[tokio::test]
async fn a_row_id_is_reserved_before_its_insert_in_the_same_transaction() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = create(open(root.path(), None)?).await?;
    let writer = database.connect(WRITER).await?;
    let reader = database.connect(READER).await?;
    let before = published(root.path())?;
    let id = |rows: Vec<Vec<SqlValue>>| match rows[..] {
        [ref row] => match row[..] {
            [SqlValue::Blob(ref id)] => {
                rocky_sdk::RowIdentity::from_bytes(id).map_err(|_| "a row identity")
            }
            _ => Err("one row identity"),
        },
        _ => Err("one row"),
    };

    exact(&writer, "BEGIN IMMEDIATE TRANSACTION", vec![]).await?;
    let first = id(exact(&writer, "SELECT rocky_next_id('items')", vec![]).await?)?;
    let second = id(exact(&writer, "select ROCKY_NEXT_ID(?);", vec![text("items")]).await?)?;
    assert_eq!((first.sequence(), second.sequence()), (1, 2));
    assert_eq!(first.writer(), second.writer());
    for (key, id) in [("i1", first), ("i2", second)] {
        exact(
            &writer,
            "INSERT INTO items(key,list,title,completed,id) VALUES(?,'l1',?,0,?)",
            vec![text(key), text(key), id.sql()],
        )
        .await?;
    }
    exact(&writer, "COMMIT", vec![]).await?;
    assert_eq!(
        all(&reader, "SELECT key, id FROM items ORDER BY key", vec![]).await?,
        vec![
            vec![text("i1"), first.sql()],
            vec![text("i2"), second.sql()]
        ]
    );
    let after = published(root.path())?;
    assert_eq!(
        after.0 - before.0,
        1,
        "reserving and inserting in one transaction publishes one bundle"
    );

    for _ in 0..2 {
        exact(&writer, "BEGIN IMMEDIATE TRANSACTION", vec![]).await?;
        let third = id(exact(&writer, "SELECT rocky_next_id('items')", vec![]).await?)?;
        assert_eq!(
            (third.writer(), third.sequence()),
            (first.writer(), 3),
            "a rollback releases the reservation to the next transaction"
        );
        exact(&writer, "ROLLBACK", vec![]).await?;
    }
    assert_eq!(published(root.path())?, after);

    let outside = exact(&writer, "SELECT rocky_next_id('items')", vec![])
        .await
        .err()
        .ok_or("outside a transaction")?;
    assert_eq!(
        outside.code, SQLITE_ERROR,
        "only a write transaction reserves ids: {outside}"
    );
    assert!(outside.message.contains("open transaction"), "{outside}");
    let read = exact(&reader, "SELECT rocky_next_id('items')", vec![])
        .await
        .err()
        .ok_or("on a reader")?;
    assert_eq!(read.code, SQLITE_READONLY, "{read}");
    exact(&writer, "BEGIN IMMEDIATE TRANSACTION", vec![]).await?;
    for (sql, params) in [
        ("SELECT rocky_next_id(7)", vec![]),
        ("SELECT rocky_next_id(?)", vec![SqlValue::Integer(7)]),
        ("SELECT rocky_next_id('items')", vec![text("items")]),
        ("SELECT rocky_next_id('absent')", vec![]),
    ] {
        let refused = exact(&writer, sql, params).await.err().ok_or(sql)?;
        assert_ne!(refused.code, 0, "{sql}");
        assert!(writer.in_transaction().await?, "{sql}");
    }
    exact(&writer, "ROLLBACK", vec![]).await?;
    assert_eq!(published(root.path())?, after);
    writer.close().await;
    reader.close().await;
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn bundle_limits_and_the_lease_fail_the_transaction_without_splitting_it()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = create(open(root.path(), None)?).await?;
    let writer = database.connect(WRITER).await?;
    let before = published(root.path())?;
    let code = |error: Failure| {
        error
            .downcast::<rocky_sdk::SqlFailure>()
            .map(|failure| *failure)
    };

    let (title_a_seeds, all_seeds) = (32, 65);
    run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
    for key in 0..all_seeds {
        all(
            &writer,
            "INSERT INTO lists(key,title) VALUES(?,?)",
            vec![
                text(&format!("s{key:02}")),
                text(if key < title_a_seeds { "a" } else { "b" }),
            ],
        )
        .await?;
    }
    run(&writer, "END TRANSACTION").await?;
    let seeded = published(root.path())?;
    run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
    all(
        &writer,
        "INSERT INTO lists(key,title) SELECT x.key || y.key, 'list' FROM lists x, lists y \
         WHERE x.title = 'a' AND y.title IN ('a','b')",
        Vec::new(),
    )
    .await?;
    let refused = code(
        all(&writer, "END TRANSACTION", Vec::new())
            .await
            .err()
            .ok_or("rows")?,
    )
    .map_err(|_| "code")?;
    assert_eq!(
        refused.code, SQLITE_TOOBIG,
        "a statement replaying more rows than a bundle holds fails the commit: {refused}"
    );
    assert!(!writer.in_transaction().await?);
    assert_eq!(
        count(&writer, "lists").await?,
        all_seeds,
        "a commit over the limit fails as a whole"
    );
    run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
    all(
        &writer,
        "INSERT INTO lists(key,title) VALUES(?,?)",
        vec![text("big"), text(&"x".repeat(256 * 1024))],
    )
    .await?;
    let refused = code(
        all(&writer, "END TRANSACTION", Vec::new())
            .await
            .err()
            .ok_or("bytes")?,
    )
    .map_err(|_| "code")?;
    assert!(
        refused.message.contains("quota"),
        "an intent frame over a range page's bytes fails the commit: {refused}"
    );
    assert_eq!(count(&writer, "lists").await?, all_seeds);
    run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
    all(
        &writer,
        "INSERT INTO lists(key,title) SELECT x.key || y.key, 'list' FROM lists x, lists y \
         WHERE x.title = 'a' AND y.title = 'b'",
        Vec::new(),
    )
    .await?;
    run(&writer, "END TRANSACTION").await?;
    let after = published(root.path())?;
    assert_eq!(
        (after.0 - seeded.0, after.1 - seeded.1),
        (1, 1),
        "exactly the replayed-row limit commits as one bundle of one intent frame"
    );
    assert!(seeded.0 > before.0);
    writer.close().await;
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_transaction_timeout_of_zero_or_past_sixty_seconds_is_refused() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = create(open(root.path(), None)?).await?;
    for refused in [Duration::ZERO, Duration::from_secs(61)] {
        let outcome = async {
            let writer = database
                .connect(ConnectionOptions {
                    transaction_timeout: Some(refused),
                    ..WRITER
                })
                .await?;
            let ran = all(&writer, "SELECT count(*) FROM lists", Vec::new()).await;
            writer.close().await;
            ran
        }
        .await;
        assert!(
            outcome.is_err(),
            "a transaction timeout of {refused:?} is refused"
        );
    }
    let writer = database
        .connect(ConnectionOptions {
            transaction_timeout: Some(Duration::from_secs(60)),
            ..WRITER
        })
        .await?;
    assert_eq!(count(&writer, "lists").await?, 0);
    writer.close().await;
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn readers_page_snapshots_and_never_hold_the_writer() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = create(open(root.path(), None)?).await?;
    let writer = database.connect(WRITER).await?;
    let reader = database.connect(READER).await?;
    let batch_under_intent_statement_quota = 200;
    for batch in 0..3 {
        run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
        for i in 1..=batch_under_intent_statement_quota {
            all(
                &writer,
                "INSERT INTO lists(key,title) VALUES(?,'list')",
                vec![text(&format!(
                    "l{:04}",
                    batch * batch_under_intent_statement_quota + i
                ))],
            )
            .await?;
        }
        run(&writer, "END TRANSACTION").await?;
    }

    let streaming = reader.statement();
    let first = reader
        .execute(
            streaming,
            "SELECT key FROM lists ORDER BY key".into(),
            Vec::new(),
        )
        .await?;
    assert_eq!(
        (first.columns.clone(), first.rows.len(), first.done),
        (vec!["key".to_string()], 256, false)
    );
    let transaction = database.connect(READER).await?;
    run(&transaction, "BEGIN DEFERRED TRANSACTION").await?;
    assert_eq!(count(&transaction, "lists").await?, 600);
    let outlasts_a_loaded_ci_host = Duration::from_secs(60);
    tokio::time::timeout(outlasts_a_loaded_ci_host, async {
        run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
        run(&writer, "DELETE FROM lists WHERE key > 'l0100'").await?;
        run(&writer, "END TRANSACTION").await
    })
    .await
    .map_err(|_| "an open cursor or read transaction blocked the writer")??;
    let mut seen = first.rows.len();
    loop {
        let page = reader.next(streaming).await?;
        seen += page.rows.len();
        if page.done {
            break;
        }
    }
    assert_eq!(seen, 600, "an open cursor still reads its snapshot");
    assert_eq!(
        count(&transaction, "lists").await?,
        600,
        "an open read transaction still reads its snapshot"
    );
    run(&transaction, "END TRANSACTION").await?;
    assert_eq!(
        count(&transaction, "lists").await?,
        100,
        "a new statement reads the commit"
    );
    assert_eq!(count(&reader, "lists").await?, 100);
    for sql in ["DELETE FROM lists", "BEGIN IMMEDIATE TRANSACTION"] {
        let error = all(&reader, sql, Vec::new()).await.err().ok_or(sql)?;
        let error = error
            .downcast::<rocky_sdk::SqlFailure>()
            .map_err(|_| "code")?;
        assert_eq!(
            error.code, SQLITE_READONLY,
            "readers refuse writes with SQLite's read-only code: {sql}: {error}"
        );
    }
    assert_eq!(
        all(&reader, "PRAGMA query_only", Vec::new()).await?,
        vec![vec![SqlValue::Integer(1)]],
        "a reader reports `query_only`"
    );
    run(&reader, "PRAGMA query_only = 1").await?;
    run(&writer, "PRAGMA query_only = 1").await?;
    let flag = all(&writer, "PRAGMA query_only", Vec::new()).await?;
    assert_eq!(flag, vec![vec![SqlValue::Integer(1)]]);
    assert!(
        all(&writer, "DELETE FROM lists", Vec::new()).await.is_err(),
        "`query_only` makes the writer read-only, as in SQLite"
    );
    run(&writer, "PRAGMA query_only = 0").await?;
    run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
    assert!(
        all(&writer, "PRAGMA query_only = 1", Vec::new())
            .await
            .is_err(),
        "`query_only` switches only between transactions"
    );
    run(&writer, "DELETE FROM lists WHERE key = 'l0001'").await?;
    run(&writer, "END TRANSACTION").await?;
    assert_eq!(count(&reader, "lists").await?, 99);
    for connection in [writer, reader, transaction] {
        connection.close().await;
    }
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_deferred_transaction_opens_on_the_session_renewed_after_its_begin() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let database = create(open(root.path(), None)?).await?;
    let writer = database.connect(WRITER).await?;
    run(&writer, "BEGIN DEFERRED TRANSACTION").await?;
    snap::value(async {
        database.renew().await?;
        snap::device(&database).await
    })
    .await?;
    let first_real_statement = count(&writer, "items")
        .await
        .map_err(|error| format!("the first real statement uses the renewed session: {error}"))?;
    assert_eq!(first_real_statement, 0);
    run(&writer, "ROLLBACK").await?;
    writer.close().await;
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn peers_apply_a_transaction_as_one_bundle_and_see_only_changed_columns()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = create(open(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?)
    .await?;
    let invitation = a
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?;
    let b = open(
        &root.path().join("b"),
        Some(Network::Owned(hub.connector("b"))),
    )?
    .join(&invitation.code)
    .await?
    .database;
    snap::value(b.sync_now()).await?;

    let (writer_a, writer_b) = (a.connect(WRITER).await?, b.connect(WRITER).await?);
    let reader_b = b.connect(READER).await?;
    let events = b.observe(&[]).await?;

    run(&writer_a, "BEGIN IMMEDIATE TRANSACTION").await?;
    run(
        &writer_a,
        "INSERT INTO lists(key,title) VALUES('l1','Groceries')",
    )
    .await?;
    run(
        &writer_a,
        "INSERT INTO items(key,list,title,completed) VALUES('i1','l1','Milk',0)",
    )
    .await?;
    run(
        &writer_a,
        "INSERT INTO items(key,list,title,completed) VALUES('i2','l1','Eggs',0)",
    )
    .await?;
    run(&writer_a, "END TRANSACTION").await?;
    snap::value(b.sync_now()).await?;
    let change = tokio::time::timeout(Duration::from_secs(5), events.next()).await??;
    assert_eq!(
        change.tables,
        vec!["items".to_string(), "lists".to_string()],
        "one change covers both tables: the bundle applies as a whole"
    );
    assert!(
        change.causes.contains(&rocky_sdk::ChangeCause::Sync),
        "{change:?}"
    );
    assert_eq!(count(&reader_b, "items").await?, 2);

    run(&writer_a, "UPDATE items SET key = 'i1', list = 'l1', title = 'Oat milk', completed = 0, note = NULL WHERE key = 'i1'").await?;
    run(&writer_b, "UPDATE items SET completed = 1 WHERE key = 'i1'").await?;
    snap::value(b.sync_now()).await?;
    for writer in [&writer_a, &writer_b] {
        let rows = all(
            writer,
            "SELECT title, completed FROM items WHERE key = 'i1'",
            Vec::new(),
        )
        .await?;
        assert_eq!(
            rows,
            vec![vec![text("Oat milk"), SqlValue::Integer(1)]],
            "a whole-row update changing only the title and a concurrent change of `completed` both survive"
        );
    }
    for connection in [writer_a, writer_b, reader_b] {
        connection.close().await;
    }
    b.close().await?;
    a.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_member_installs_only_its_readable_tables_even_after_reopen() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let mut schema: Value = serde_json::from_str(SHOPPING)?;
    schema["tables"][0]["indexes"] = json!([{"name":"list_by_title", "unique":false,
        "columns":[{"name":"title","descending":false}]}]);
    schema["tables"]
        .as_array_mut()
        .ok_or("tables")?
        .push(json!({
            "name":"devices", "class":"replayed",
            "columns":[{"name":"key","type":"text","not_null":true},
                {"name":"name","type":"text","not_null":true}], "primary_key":["key"], "indexes":[{"name":"devices_by_name", "columns":[{"name":"name","descending":false}]}]
        }));
    schema["roles"] = json!([{"name":"list_reader","tables":[{"table":"lists","access":"read"}]}]);
    let schema = schema.to_string();
    let a_path = root.path().join("a");
    let b_path = root.path().join("b");
    let setup = |directory: &Path, name| {
        Configuration::new(directory, snap::schema(&schema))
            .map(|configuration| configuration.network(Network::Owned(hub.connector(name))))
    };
    let a = setup(&a_path, "a")?.create().await?.database;
    let invitation = a
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Role(
            rocky_sdk::RoleName::from_static("list_reader"),
        )))
        .await?;
    let b = setup(&b_path, "b")?.join(&invitation.code).await?.database;
    {
        let reader = b.connect(READER).await?;
        all(&reader, "SELECT * FROM lists", vec![]).await?;
        for table in ["devices", "items", "issuer_registry", "keys", "identities"] {
            let error = all(&reader, &format!("SELECT * FROM {table}"), vec![])
                .await
                .unwrap_err();
            assert!(error.to_string().contains("no such table"), "{error}");
            let file = rusqlite::Connection::open_with_flags(
                b_path.join("rocky.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let count: i64 = file.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?",
                [table],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0, "{table}");
        }
        reader.close().await;
    }
    b.close().await?;
    let b = setup(&b_path, "b")?.open().await?;
    let reader = b.connect(READER).await?;
    assert!(
        all(&reader, "SELECT * FROM devices", vec![])
            .await
            .unwrap_err()
            .to_string()
            .contains("no such table")
    );
    all(&reader, "SELECT * FROM lists", vec![]).await?;
    reader.close().await;
    b.close().await?;
    a.close().await?;
    Ok(())
}

#[cfg(not(target_family = "wasm"))]
#[tokio::test(start_paused = true)]
async fn a_live_commit_reaches_a_connected_peer_without_sync_now_or_a_tick() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let live = |name: &str| -> Result<Configuration, Failure> {
        Ok(open(
            &root.path().join(name),
            Some(Network::Owned(hub.connector(name))),
        )?
        .sync(crate::SyncMode::Live))
    };
    tokio::task::LocalSet::new()
        .run_until(time_frozen(async {
            let root_key = Arc::new(crate::RootKey::generate());
            let a = super::Database::launch_local(&live("a")?, super::Install::create(root_key))
                .await?;
            let invitation = a.invite(crate::Invite::new(crate::Access::Full)).await?;
            let (joining, _) = super::Install::invited(&invitation.code);
            let b = super::Database::launch_local(&live("b")?, joining).await?;
            for database in [&a, &b] {
                loop {
                    let status = database.status().await?;
                    let settled = status.network.connected_peer_count == 1
                        && !status.network.reconciling
                        && !status.network.worker_running
                        && !status.data.bootstrap_pending;
                    if settled {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
            let reader = b.connect(READER).await?;
            assert_eq!(count(&reader, "items").await?, 0);
            let events = b.observe(&["items"]).await?;
            let frozen_at = tokio::time::Instant::now();
            let writer = a.connect(WRITER).await?;
            run(&writer, "BEGIN IMMEDIATE TRANSACTION").await?;
            run(
                &writer,
                "INSERT INTO lists(key,title) VALUES('l1','Groceries')",
            )
            .await?;
            run(
                &writer,
                "INSERT INTO items(key,list,title,completed) VALUES('i1','l1','Milk',0)",
            )
            .await?;
            run(&writer, "END TRANSACTION").await?;
            let change = loop {
                let change = events.next().await?;
                if change.causes.contains(&crate::ChangeCause::Sync) {
                    break change;
                }
            };
            assert_eq!(change.tables, ["items"]);
            assert_eq!(
                tokio::time::Instant::now(),
                frozen_at,
                "no tick or retry deadline could fire: the commit was pushed"
            );
            assert_eq!(count(&reader, "items").await?, 1);
            for connection in [writer, reader] {
                connection.close().await;
            }
            b.close().await?;
            a.close().await?;
            Ok(())
        }))
        .await
}

async fn time_frozen<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let watchdog = std::time::Instant::now();
    loop {
        tokio::select! {
            biased;
            output = &mut future => return output,
            () = tokio::task::yield_now() => {
                std::thread::park_timeout(Duration::from_micros(50));
                assert!(watchdog.elapsed() < Duration::from_secs(120), "test made no progress");
            }
        }
    }
}
