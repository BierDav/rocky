//! Replayed tables through the SDK: concurrent increments both apply, and
//! a transaction that loses its key in canonical order drops, reported to
//! observers and through its commit token.
use common::{Device, Failure, introduce};
use rocky_sdk::{
    Database, DropReason, Network, Open, SqlValue, TransactionOutcome, TransactionToken,
    transport::memory::Hub,
};
use serde_json::json;
use std::{path::Path, sync::Arc};

mod common;

const SCHEMA: &str = r#"{"application":"counters","version":1,"format":6,"tables":[{"name":"counters","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"amount","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;

fn open<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        identity: None,
        directory,
        schema: SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}

async fn create(database: &Database, key: &str, amount: i64) -> Result<TransactionToken, Failure> {
    let mut tx = database.transaction().await?;
    let id = tx.next_id("counters", Vec::new()).await?;
    tx.execute(
        "INSERT INTO counters(key,amount,id) VALUES(?,?,?)",
        vec![
            SqlValue::Text(key.into()),
            SqlValue::Integer(amount),
            SqlValue::Integer(id),
        ],
    )
    .await?;
    Ok(tx.commit().await?.ok_or("a replayed insert publishes")?)
}

async fn raise(database: &Database, key: &str) -> Result<TransactionToken, Failure> {
    let mut tx = database.transaction().await?;
    tx.execute(
        "UPDATE counters SET amount=amount+? WHERE key=?",
        vec![SqlValue::Integer(1), SqlValue::Text(key.into())],
    )
    .await?;
    Ok(tx.commit().await?.ok_or("a replayed update publishes")?)
}

async fn counters(database: &Database) -> Result<Vec<(String, i64)>, Failure> {
    let rows = database
        .query("SELECT key,amount FROM counters ORDER BY key", Vec::new())
        .await?
        .collect()
        .await?;
    rows.iter()
        .map(|row| Ok((row.get_at(0)?, row.get_at(1)?)))
        .collect()
}

#[tokio::test]
async fn replayed_counters_keep_both_increments_and_report_drops() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::new(Database::create(
        open(&root.path().join("a"), &hub, "a"),
        None,
    )?)?;
    let invitation = a
        .invite_as(json!({"kind": "writer", "scopes": ["counters"]}))
        .await;
    let b = Device::new(Database::join(
        open(&root.path().join("b"), &hub, "b"),
        &invitation,
    )?)?;
    b.sync().await.map_err(|error| error.to_string())?;
    introduce(&a, &b).await;
    let sync = || async {
        for device in [&a, &b, &a] {
            device.sync().await.map_err(|error| error.to_string())?;
        }
        Ok::<_, Failure>(())
    };

    let created = create(&a.database, "c", 0).await?;
    sync().await?;
    // Both raise the amount they read; replay keeps both.
    raise(&a.database, "c").await?;
    raise(&b.database, "c").await?;
    sync().await?;
    for device in [&a, &b] {
        assert_eq!(counters(&device.database).await?, [("c".into(), 2)]);
    }
    assert_eq!(
        a.database.transaction_outcome(created).await?,
        Some(TransactionOutcome::Applied)
    );

    // Both create `k`; the later one drops wherever it replays, and its
    // author's observer hears of it after the commit that records it.
    let events = b.database.observe_tables(None).await?;
    create(&a.database, "k", 1).await?;
    let later = create(&b.database, "k", 2).await?;
    assert_eq!(
        b.database.transaction_outcome(later).await?,
        Some(TransactionOutcome::Applied)
    );
    sync().await?;
    let dropped = Some(TransactionOutcome::Dropped {
        reason: DropReason::Constraint,
        sealed: false,
    });
    for device in [&a, &b] {
        assert_eq!(device.database.transaction_outcome(later).await?, dropped);
        assert_eq!(
            counters(&device.database).await?,
            [("c".into(), 2), ("k".into(), 1)]
        );
    }
    loop {
        let change = events.next().await?;
        if change.dropped.contains(&later) {
            break;
        }
    }
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn a_member_joining_later_receives_replayed_rows() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::new(Database::create(
        open(&root.path().join("a"), &hub, "a"),
        None,
    )?)?;
    create(&a.database, "c", 0).await?;
    raise(&a.database, "c").await?;
    let invitation = a
        .invite_as(json!({"kind": "writer", "scopes": ["counters"]}))
        .await;
    let b = Device::new(Database::join(
        open(&root.path().join("b"), &hub, "b"),
        &invitation,
    )?)?;
    introduce(&a, &b).await;
    for device in [&b, &a, &b] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    assert_eq!(counters(&b.database).await?, [("c".into(), 1)]);
    raise(&b.database, "c").await?;
    for device in [&b, &a] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    assert_eq!(counters(&a.database).await?, [("c".into(), 2)]);
    a.close().await;
    b.close().await;
    Ok(())
}
