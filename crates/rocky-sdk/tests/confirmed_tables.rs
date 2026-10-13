//! Command-controlled tables through the driver: confirmed rows read like
//! any table, writes fail with SQLite's authorization code and confirmation
//! invalidates readers. See `docs/runtime/confirmed-tables.md`.
mod support;
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{HandlerInput, HandlerOutput, Outcome, SqlValue, TableWrite};
use rocky_sdk::{CommandHandler, Commands, Connection, ConnectionOptions, Database, Open};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

type Failure = Box<dyn std::error::Error + Send + Sync>;

const SCHEMA: &str = r#"{"application":"market","version":1,"format":5,
 "commands":{"format":1,"handler":1,"commands":[
  {"name":"Restock","version":1,"handler":"stock.restock.v1",
   "arguments":[{"name":"item","type":"text"},{"name":"quantity","type":"integer"}],
   "accepted":[{"name":"remaining","type":"integer"}],"rejected":[],
   "invocation":"holds","result":"holds","resources":["holds","stock"]}]},
 "tables":[
  {"name":"holds","class":"mergeable","columns":[
    {"name":"key","type":"text","not_null":true},{"name":"item","type":"text","not_null":true}],
   "primary_key":["key"]},
  {"name":"stock","class":"command_controlled","columns":[
    {"name":"key","type":"text","not_null":true},{"name":"remaining","type":"integer","not_null":true}],
   "primary_key":["key"]}]}"#;

/// Adds to an item's confirmed stock.
struct Restock;
impl CommandHandler for Restock {
    fn handle(&self, input: &HandlerInput) -> Result<HandlerOutput, String> {
        let number = |value: &SqlValue| match value {
            SqlValue::Integer(number) => Ok(*number),
            _ => Err("not a number".to_string()),
        };
        let item = input.arguments[0].clone();
        let remaining = match input.reads.get("stock", std::slice::from_ref(&item))? {
            Some(row) => number(&row[1])?,
            None => 0,
        } + number(&input.arguments[1])?;
        Ok(HandlerOutput {
            outcome: Outcome::Accepted(vec![SqlValue::Integer(remaining)]),
            writes: vec![TableWrite::Put {
                table: "stock".into(),
                row: support::identity_row(vec![item, SqlValue::Integer(remaining)]),
            }],
            jobs: Vec::new(),
        })
    }
}

async fn call(database: &Database, request: Value) -> Result<Value, Failure> {
    let scope: Value = serde_json::from_str(&database.scope())?;
    let reply = database
        .session()
        .call(json!({"version": 1, "scope": scope, "request": request}).to_string())
        .await?;
    let reply: Value = serde_json::from_str(&reply)?;
    if reply["status"] == "ok" {
        Ok(reply["value"].clone())
    } else {
        Err(reply["error"].to_string().into())
    }
}
async fn all(connection: &Connection, sql: &str) -> Result<Vec<Vec<SqlValue>>, Failure> {
    let statement = connection.statement();
    let mut page = connection
        .execute(statement, sql.into(), Vec::new())
        .await?;
    let mut rows = std::mem::take(&mut page.rows);
    while !page.done {
        page = connection.next(statement).await?;
        rows.append(&mut page.rows);
    }
    Ok(rows)
}
fn stock(remaining: i64) -> Vec<Vec<SqlValue>> {
    vec![vec![
        SqlValue::Text("bolt".into()),
        SqlValue::Integer(remaining),
    ]]
}
/// Bundles this device authored, read with ordinary SQLite.
fn bundles(directory: &Path) -> Result<i64, Failure> {
    let file = rusqlite::Connection::open_with_flags(
        directory.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    Ok(file.query_row(
        "SELECT count(DISTINCT handle) FROM secure_bundle_handles",
        [],
        |row| row.get(0),
    )?)
}

#[tokio::test(flavor = "multi_thread")]
async fn confirmed_rows_read_through_the_driver_and_refuse_its_writes() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("market");
    let coordinator = Arc::new(MemoryCoordinator::default());
    let opened = Database::create(
        Open {
            directory: &directory,
            schema: SCHEMA,
            secrets: None,
            network: None,
            commands: Some(Commands {
                coordinator: rocky_sdk::Coordination::Composed(coordinator.clone()),
                handler: Some(Arc::new(Restock)),
                deadline: None,
            }),
        },
        None,
    )?;
    let database = opened.database;
    support::database(&database);
    let scope: Value = serde_json::from_str(&database.scope())?;
    support::prepare(&scope, "holds", &|request| {
        let database = database.clone();
        async move {
            call(&database, request)
                .await
                .map_err(|error| json!(error.to_string()))
        }
    })
    .await
    .map_err(|error| error.to_string())?;
    let identity = call(&database, json!({"type": "command_identity"})).await?;
    let hex = |name: &str| -> Result<[u8; 16], Failure> {
        let bytes =
            data_encoding::HEXLOWER.decode(identity[name].as_str().ok_or(name)?.as_bytes())?;
        Ok(bytes.try_into().map_err(|_| "16 bytes")?)
    };
    let (node, principal) = (
        rocky_core::NodeId(hex("node")?),
        rocky_core::PrincipalId(hex("principal")?),
    );
    let domain: rocky_core::Scope = serde_json::from_value(identity["scope"].clone())?;
    coordinator.register(domain, principal, [1; 16], vec![node], 1, 1)?;
    coordinator.set_invokers(domain, principal, vec![(node, principal)])?;
    coordinator.acquire(domain, principal, node)?;

    let events = database.observe_tables(Some(vec!["stock".into()])).await?;
    let writer = database
        .connect(ConnectionOptions {
            read_only: false,
            profile: None,
            transaction_timeout: None,
        })
        .await?;
    let reader = database
        .connect(ConnectionOptions {
            read_only: true,
            profile: None,
            transaction_timeout: None,
        })
        .await?;
    assert!(
        all(&reader, "SELECT key,remaining FROM stock")
            .await?
            .is_empty()
    );

    // A queued command is not a row; its confirmed result is.
    let restock = |handle: u8, quantity: i64| {
        json!({"type": "command_submit", "handle": vec![handle; 32], "method": "Restock", "version": 1,
               "arguments": [{"type": "text", "value": "bolt"}, {"type": "integer", "value": quantity.to_string()}]})
    };
    call(&database, restock(1, 5)).await?;
    assert!(
        all(&reader, "SELECT key,remaining FROM stock")
            .await?
            .is_empty()
    );
    let run = call(
        &database,
        json!({"type": "command_run", "handle": vec![1; 32]}),
    )
    .await?;
    assert_eq!(run["tickets"][0]["state"], "applied");
    let change = tokio::time::timeout(Duration::from_secs(5), events.next()).await??;
    assert!(change.tables.contains(&"stock".to_string()));
    assert!(change.causes.contains(&"confirmed".to_string()));
    assert_eq!(
        all(&reader, "SELECT key,remaining FROM stock").await?,
        stock(5)
    );
    assert_eq!(
        all(&writer, "SELECT key,remaining FROM stock").await?,
        stock(5)
    );
    let rows = call(&database, json!({"type": "command_rows", "table": "stock"})).await?;
    assert_eq!(rows["complete"], true);
    assert_eq!(rows["rows"].as_array().map(Vec::len), Some(1));

    // Each write fails with SQLITE_AUTH, also inside a transaction that
    // otherwise succeeds, and that transaction publishes nothing.
    let published = bundles(&directory)?;
    for write in [
        "INSERT INTO stock(key,remaining,id) VALUES('nut',1,4294967999)",
        "UPDATE stock SET remaining=0",
        "DELETE FROM stock",
    ] {
        all(&writer, "BEGIN").await?;
        let id = all(&writer, "SELECT rocky_next_id('holds')").await?;
        let SqlValue::Integer(id) = id[0][0] else {
            return Err("id".into());
        };
        all(
            &writer,
            &format!("INSERT INTO holds(key,item,id) VALUES('h','bolt',{id})"),
        )
        .await?;
        let refused = all(&writer, write).await.unwrap_err();
        let refused = refused
            .downcast_ref::<rocky_sdk::SqlFailure>()
            .ok_or("code")?;
        assert_eq!(refused.code, 23, "{write}: {refused}");
        assert!(all(&writer, "COMMIT").await.is_err());
        if writer.in_transaction().await? {
            all(&writer, "ROLLBACK").await?;
        }
        let alone = all(&writer, write).await.unwrap_err();
        assert_eq!(
            alone
                .downcast_ref::<rocky_sdk::SqlFailure>()
                .ok_or("code")?
                .code,
            23
        );
    }
    assert_eq!(bundles(&directory)?, published);
    assert!(all(&reader, "SELECT key FROM holds").await?.is_empty());
    assert_eq!(
        all(&reader, "SELECT key,remaining FROM stock").await?,
        stock(5)
    );

    call(&database, restock(2, 2)).await?;
    call(
        &database,
        json!({"type": "command_run", "handle": vec![2; 32]}),
    )
    .await?;
    let change = tokio::time::timeout(Duration::from_secs(5), events.next()).await??;
    assert!(change.tables.contains(&"stock".to_string()));
    assert_eq!(
        all(&reader, "SELECT key,remaining FROM stock").await?,
        stock(7)
    );
    writer.close().await;
    reader.close().await;
    database.close().await?;
    Ok(())
}
