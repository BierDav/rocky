//! Exact values through the SDK: integer overflow in a replayed statement,
//! a `CHECK` and an assertion fails the author's commit and drops with the
//! same reason wherever it replays; UUIDs, timestamps and JSON round-trip.
use common::{Device, Failure, introduce};
use rocky_sdk::{
    Database, DropReason, Network, Open, SqlValue, Timestamp, TransactionOutcome, TransactionToken,
    params, transport::memory::Hub,
};
use serde_json::json;
use std::{path::Path, sync::Arc};

mod common;

const SCHEMA: &str = r#"{"application":"ledger","version":1,"format":6,
    "tables":[
        {"name":"balances","class":"replayed",
         "columns":[{"name":"key","type":"text","not_null":true},
                    {"name":"amount","type":"integer","not_null":true},
                    {"name":"fee","type":"integer","not_null":true},
                    {"name":"bonus","type":"integer","not_null":true}],
         "checks":["checked_add(amount, fee) >= amount"],
         "primary_key":["key"]},
        {"name":"records","class":"replayed",
         "columns":[{"name":"key","type":"text","not_null":true},
                    {"name":"uuid","type":"blob","not_null":true},
                    {"name":"at","type":"integer","not_null":true},
                    {"name":"doc","type":"text","not_null":true}],
         "primary_key":["key"]}],
    "assertions":[{"name":"bonus_fits",
                   "select_sql":"SELECT key FROM balances WHERE amount + bonus < amount"}]}"#;

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

/// Runs one statement as its own transaction.
async fn write(
    database: &Database,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<TransactionToken, Failure> {
    let mut tx = database.transaction().await?;
    tx.execute(sql, params).await?;
    Ok(tx.commit().await?.ok_or("a replayed write publishes")?)
}

async fn create(database: &Database, key: &str, amount: i64) -> Result<(), Failure> {
    let mut tx = database.transaction().await?;
    let id = tx.next_id("balances", Vec::new()).await?;
    tx.execute(
        "INSERT INTO balances(key,amount,fee,bonus,id) VALUES(?,?,0,0,?)",
        params![key, amount, id],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn balances(database: &Database) -> Result<Vec<(String, i64, i64, i64)>, Failure> {
    let rows = database
        .query(
            "SELECT key,amount,fee,bonus FROM balances ORDER BY key",
            Vec::new(),
        )
        .await?
        .collect()
        .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.get_at(0)?,
                row.get_at(1)?,
                row.get_at(2)?,
                row.get_at(3)?,
            ))
        })
        .collect()
}

/// The error of a write that must fail, as text.
async fn refused(database: &Database, sql: &str, params: Vec<SqlValue>) -> String {
    match write(database, sql, params).await {
        Ok(_) => panic!("{sql} committed"),
        Err(error) => error.to_string(),
    }
}

#[tokio::test]
async fn overflow_fails_the_author_commit_instead_of_becoming_real() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::new(Database::create(
        open(&root.path().join("a"), &hub, "a"),
        None,
    )?)?;
    create(&a.database, "low", i64::MIN + 1).await?;
    create(&a.database, "high", i64::MAX - 1).await?;
    let before = balances(&a.database).await?;

    // SQLite alone would compute -2^63 - 1 as a REAL, round it to -2^63 and
    // store i64::MIN without an error.
    let error = refused(
        &a.database,
        "UPDATE balances SET amount = amount - ? WHERE key = ?",
        params![2_i64, "low"],
    )
    .await;
    assert!(error.contains("integer overflow"), "{error}");
    let error = refused(
        &a.database,
        "UPDATE balances SET amount = amount * 2 WHERE key = 'high'",
        Vec::new(),
    )
    .await;
    assert!(error.contains("integer overflow"), "{error}");
    // The CHECK's sum leaves i64.
    let error = refused(
        &a.database,
        "UPDATE balances SET fee = ? WHERE key = 'high'",
        params![5_i64],
    )
    .await;
    assert!(error.contains("integer overflow"), "{error}");
    // So does the assertion's, at commit.
    let error = refused(
        &a.database,
        "UPDATE balances SET bonus = ? WHERE key = 'high'",
        params![5_i64],
    )
    .await;
    assert!(
        error.contains("bonus_fits") && error.contains("integer overflow"),
        "{error}"
    );
    assert_eq!(balances(&a.database).await?, before);
    a.close().await;
    Ok(())
}

#[tokio::test]
async fn concurrent_overflows_drop_with_the_same_reason_on_every_replica() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::new(Database::create(
        open(&root.path().join("a"), &hub, "a"),
        None,
    )?)?;
    let invitation = a
        .invite_as(json!({"kind": "writer", "scopes": ["balances"]}))
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
    let start = i64::MAX - 10;
    for key in ["check", "assert", "update"] {
        create(&a.database, key, start).await?;
    }
    sync().await?;

    // Each pair succeeds alone on its author; together they leave i64 in
    // the statement, the CHECK or the assertion, whichever replays second.
    let raise = "UPDATE balances SET amount = amount + ? WHERE key = ?";
    let pairs = [
        (
            write(&a.database, raise, params![6_i64, "update"]).await?,
            write(&b.database, raise, params![6_i64, "update"]).await?,
        ),
        (
            write(
                &a.database,
                "UPDATE balances SET fee = ? WHERE key = 'check'",
                params![6_i64],
            )
            .await?,
            write(&b.database, raise, params![6_i64, "check"]).await?,
        ),
        (
            write(
                &a.database,
                "UPDATE balances SET bonus = ? WHERE key = 'assert'",
                params![6_i64],
            )
            .await?,
            write(&b.database, raise, params![6_i64, "assert"]).await?,
        ),
    ];
    sync().await?;
    let dropped = TransactionOutcome::Dropped {
        reason: DropReason::Constraint,
        sealed: false,
    };
    for (first, second) in pairs {
        let mut outcomes = Vec::new();
        for device in [&a, &b] {
            outcomes.push((
                device.database.transaction_outcome(first).await?,
                device.database.transaction_outcome(second).await?,
            ));
        }
        assert_eq!(outcomes[0], outcomes[1], "replicas agree");
        let (first, second) = outcomes[0];
        assert!(
            matches!(
                (first, second),
                (Some(TransactionOutcome::Applied { sealed: false }), Some(outcome))
                    | (Some(outcome), Some(TransactionOutcome::Applied { sealed: false }))
                    if outcome == dropped
            ),
            "{first:?} {second:?}"
        );
    }
    let rows = balances(&a.database).await?;
    assert_eq!(rows, balances(&b.database).await?);
    assert_eq!(
        rows.iter()
            .find(|(key, ..)| key == "update")
            .map(|(_, amount, ..)| *amount),
        Some(start + 6)
    );
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn uuids_timestamps_json_and_extreme_integers_round_trip() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::new(Database::create(
        open(&root.path().join("a"), &hub, "a"),
        None,
    )?)?;
    let uuid: [u8; 16] = *b"\x67\xe5\x50\x44\x10\xb1\x42\x6f\x92\x47\xbb\x68\x0e\x5f\xe0\xc8";
    let at = Timestamp::from_unix_micros(-1_234_567);
    let doc = json!({"b": [1, 2.5, null], "a": {"z": "x", "y": true}});
    let mut tx = a.database.transaction().await?;
    let id = tx.next_id("records", Vec::new()).await?;
    tx.execute(
        "INSERT INTO records(key,uuid,at,doc,id) VALUES(?,?,?,?,?)",
        params!["r", uuid, at, doc.clone(), id],
    )
    .await?;
    tx.commit().await?;
    create(&a.database, "min", i64::MIN).await?;
    create(&a.database, "max", i64::MAX).await?;

    let rows = a
        .database
        .query(
            "SELECT uuid,at,doc,typeof(uuid),typeof(at),typeof(doc) FROM records",
            Vec::new(),
        )
        .await?
        .collect()
        .await?;
    let [row] = rows.as_slice() else {
        return Err("one record".into());
    };
    assert_eq!(row.get::<[u8; 16]>("uuid")?, uuid);
    assert_eq!(row.get::<Timestamp>("at")?, at);
    assert_eq!(row.get::<serde_json::Value>("doc")?, doc);
    // JSON is stored as the compact text bound, never rewritten.
    assert_eq!(
        row.get::<String>("doc")?,
        r#"{"a":{"y":true,"z":"x"},"b":[1,2.5,null]}"#
    );
    assert_eq!(
        (
            row.get::<String>("typeof(uuid)")?,
            row.get::<String>("typeof(at)")?,
            row.get::<String>("typeof(doc)")?
        ),
        ("blob".into(), "integer".into(), "text".into())
    );
    assert!(row.get::<[u8; 15]>("uuid").is_err());
    assert!(row.get::<Timestamp>("doc").is_err());

    let extremes: Vec<(String, i64, i64, i64)> = balances(&a.database).await?;
    assert_eq!(
        extremes,
        [
            ("max".into(), i64::MAX, 0, 0),
            ("min".into(), i64::MIN, 0, 0)
        ]
    );

    // A SystemTime rounds down to its microsecond, also before the epoch.
    let epoch = std::time::UNIX_EPOCH;
    let later = epoch + std::time::Duration::from_nanos(1_500);
    let earlier = epoch - std::time::Duration::from_nanos(1_500);
    assert_eq!(Timestamp::try_from(later)?.unix_micros(), 1);
    assert_eq!(Timestamp::try_from(earlier)?.unix_micros(), -2);
    assert_eq!(
        Timestamp::from_unix_micros(-2).system_time(),
        Some(epoch - std::time::Duration::from_micros(2))
    );
    a.close().await;
    Ok(())
}
