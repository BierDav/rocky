use common::{Device, Failure, introduce};
use rocky_sdk::{
    Database, DropReason, Network, SqlValue, TransactionOutcome, TransactionToken,
    transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

use crate::common;

fn text(name: &str) -> rocky_sdk::Column {
    crate::common::required(name, rocky_sdk::Affinity::Text)
}
fn integer(name: &str) -> rocky_sdk::Column {
    crate::common::required(name, rocky_sdk::Affinity::Integer)
}
fn schema() -> rocky_sdk::Schema {
    crate::common::build(
        rocky_sdk::Schema::builder("counters", 1).table(
            rocky_sdk::Table::replayed("counters")
                .column(text("key"))
                .column(integer("amount"))
                .primary_key(["key"]),
        ),
    )
}

fn open(directory: &Path, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        &schema(),
        None,
        None,
        Some(Network::Owned(hub.connector(name))),
    )
}

async fn create(database: &Database, key: &str, amount: i64) -> Result<TransactionToken, Failure> {
    let mut tx = database.transaction().await?;
    let id = tx.next_id("counters", Vec::new()).await?;
    tx.execute(
        "INSERT INTO counters(key,amount,id) VALUES(?,?,?)",
        vec![
            SqlValue::Text(key.into()),
            SqlValue::Integer(amount),
            id.sql(),
        ],
    )
    .await?;
    match tx.commit().await? {
        rocky_sdk::Commit::Recorded(token) => Ok(token),
        rocky_sdk::Commit::NoChanges => Err("a replayed insert publishes".into()),
    }
}

async fn raise(database: &Database, key: &str) -> Result<TransactionToken, Failure> {
    let mut tx = database.transaction().await?;
    tx.execute(
        "UPDATE counters SET amount=amount+? WHERE key=?",
        vec![SqlValue::Integer(1), SqlValue::Text(key.into())],
    )
    .await?;
    match tx.commit().await? {
        rocky_sdk::Commit::Recorded(token) => Ok(token),
        rocky_sdk::Commit::NoChanges => Err("a replayed update publishes".into()),
    }
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
    let a = Device::new(open(&root.path().join("a"), &hub, "a").create().await?)?;
    let invitation = a.invite_as(rocky_sdk::Access::Full).await;
    let b = Device::new(
        open(&root.path().join("b"), &hub, "b")
            .join(&invitation)
            .await?,
    )?;
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
    raise(&a.database, "c").await?;
    raise(&b.database, "c").await?;
    sync().await?;
    for device in [&a, &b] {
        assert_eq!(
            counters(&device.database).await?,
            [("c".into(), 2)],
            "replay keeps both raises of the amount each device read"
        );
    }
    assert_eq!(
        a.database.transaction_outcome(created).await?,
        Some(TransactionOutcome::Applied { sealed: false })
    );

    let events = b.database.observe(&[]).await?;
    create(&a.database, "k", 1).await?;
    let later = create(&b.database, "k", 2).await?;
    assert_eq!(
        b.database.transaction_outcome(later).await?,
        Some(TransactionOutcome::Applied { sealed: false })
    );
    sync().await?;
    let dropped = Some(TransactionOutcome::Dropped {
        reason: DropReason::Constraint,
        sealed: false,
    });
    for device in [&a, &b] {
        assert_eq!(
            device.database.transaction_outcome(later).await?,
            dropped,
            "the later of two creates of one key drops wherever it replays"
        );
        assert_eq!(
            counters(&device.database).await?,
            [("c".into(), 2), ("k".into(), 1)]
        );
    }
    loop {
        let change = events.next().await?;
        if change.dropped.contains(&rocky_sdk::DroppedTransaction {
            token: later,
            reason: rocky_sdk::DropReason::Constraint,
        }) {
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
    let a = Device::new(open(&root.path().join("a"), &hub, "a").create().await?)?;
    create(&a.database, "c", 0).await?;
    raise(&a.database, "c").await?;
    let invitation = a.invite_as(rocky_sdk::Access::Full).await;
    let b = Device::new(
        open(&root.path().join("b"), &hub, "b")
            .join(&invitation)
            .await?,
    )?;
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

#[tokio::test]
async fn an_applied_transaction_turns_final_with_the_seal_and_stays_final() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::new(open(&root.path().join("a"), &hub, "a").create().await?)?;
    let invitation = a.invite_as(rocky_sdk::Access::Full).await;
    let directory = root.path().join("b");
    let b = Device::new(open(&directory, &hub, "b").join(&invitation).await?)?;
    b.sync().await.map_err(|error| error.to_string())?;
    introduce(&a, &b).await;
    let sync = || async {
        for device in [&a, &b, &a, &b] {
            device.sync().await.map_err(|error| error.to_string())?;
        }
        Ok::<_, Failure>(())
    };
    let events = b.database.observe(&[]).await?;
    let token = create(&b.database, "k", 1).await?;
    let provisional = Some(TransactionOutcome::Applied { sealed: false });
    assert_eq!(b.database.transaction_outcome(token).await?, provisional);
    sync().await?;
    for device in [&a, &b] {
        assert_eq!(
            device.database.transaction_outcome(token).await?,
            provisional
        );
    }
    a.database.seal().await?;
    sync().await?;
    let sealed = Some(TransactionOutcome::Applied { sealed: true });
    for device in [&a, &b] {
        assert_eq!(device.database.transaction_outcome(token).await?, sealed);
    }
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while !events.next().await?.sealed.contains(&token) {}
        Ok::<_, Failure>(())
    })
    .await??;
    drop(events);
    b.close().await;
    drop(b);
    let b = Device::new(open(&directory, &hub, "b").open().await?)?;
    assert_eq!(b.database.transaction_outcome(token).await?, sealed);
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn a_member_holding_the_seal_reports_its_drop_final() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::new(open(&root.path().join("a"), &hub, "a").create().await?)?;
    let invitation = a.invite_as(rocky_sdk::Access::Full).await;
    let b = Device::new(
        open(&root.path().join("b"), &hub, "b")
            .join(&invitation)
            .await?,
    )?;
    b.sync().await.map_err(|error| error.to_string())?;
    introduce(&a, &b).await;
    let sync = || async {
        for device in [&a, &b, &a, &b] {
            device.sync().await.map_err(|error| error.to_string())?;
        }
        Ok::<_, Failure>(())
    };
    create(&a.database, "k", 1).await?;
    let later = create(&b.database, "k", 2).await?;
    sync().await?;
    a.database.seal().await?;
    sync().await?;
    for device in [&a, &b] {
        assert_eq!(
            device.database.transaction_outcome(later).await?,
            Some(TransactionOutcome::Dropped {
                reason: DropReason::Constraint,
                sealed: true,
            })
        );
    }
    a.close().await;
    b.close().await;
    Ok(())
}
