use common::{Device, Failure, introduce};
use rocky_sdk::{
    Affinity, Column, Database, Network, Schema, SqlValue, Table, TransactionOutcome,
    transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

use crate::common;

fn text(name: &str) -> Column {
    common::required(name, Affinity::Text)
}

fn integer(name: &str) -> Column {
    common::required(name, Affinity::Integer)
}

fn prism_schema() -> Schema {
    let mut builder = Schema::builder("prism_noop", 1)
        .table(
            Table::replayed("projects")
                .column(text("key"))
                .column(text("name"))
                .primary_key(["key"]),
        )
        .table(
            Table::replayed("machines")
                .column(text("key"))
                .column(text("metrics_json"))
                .primary_key(["key"]),
        )
        .table(
            Table::replayed("hosts")
                .column(text("key"))
                .column(integer("seen_at"))
                .primary_key(["key"]),
        );
    for index in 0..6 {
        builder = builder.table(
            Table::replayed(format!("pruned_{index}"))
                .column(text("key"))
                .column(integer("done_at"))
                .primary_key(["key"]),
        );
    }
    common::build(builder)
}

fn open(directory: &Path, schema: &Schema, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    common::configuration(
        directory,
        schema,
        None,
        None,
        Some(Network::Owned(hub.connector(name))),
    )
}

fn directory_bytes(path: &Path) -> Result<u64, Failure> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            total += directory_bytes(&entry.path())?;
        }
    }
    Ok(total)
}

async fn sync_all(devices: &[&Device]) -> Result<(), Failure> {
    for device in devices {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    for device in devices {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn assert_healthy(device: &Device) -> Result<(), Failure> {
    let status = device.database.status().await?;
    assert!(
        status.blockers().is_empty(),
        "unexpected blockers: {:?}",
        status.blockers()
    );
    assert!(
        status.alerts().is_empty(),
        "unexpected alerts: {:?}",
        status.alerts()
    );
    Ok(())
}

async fn count_rows(database: &Database, table: &str) -> Result<i64, Failure> {
    let rows = database
        .query(&format!("SELECT count(*) FROM {table}"), Vec::new())
        .await?
        .collect()
        .await?;
    Ok(rows[0].get_at(0)?)
}

#[tokio::test]
async fn noop_dml_and_unchanged_reinsert_emit_no_frames_and_keep_replicas_bounded()
-> Result<(), Failure> {
    let schema = prism_schema();
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());

    let dir_a = root.path().join("a");
    let dir_b = root.path().join("b");
    let dir_c = root.path().join("c");

    let a = Device::new(open(&dir_a, &schema, &hub, "a").create().await?)?;
    let inv_b = a.invite_as(rocky_sdk::Access::Full).await;
    let b = Device::new(open(&dir_b, &schema, &hub, "b").join(&inv_b).await?)?;
    let inv_c = a.invite_as(rocky_sdk::Access::Full).await;
    let c = Device::new(open(&dir_c, &schema, &hub, "c").join(&inv_c).await?)?;

    introduce(&a, &b).await;
    introduce(&a, &c).await;
    introduce(&b, &c).await;
    sync_all(&[&a, &b, &c]).await?;

    let seed = a
        .database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO projects(key, name) VALUES(?, ?), (?, ?)",
                vec![
                    SqlValue::Text("p1".into()),
                    SqlValue::Text("alpha".into()),
                    SqlValue::Text("p2".into()),
                    SqlValue::Text("beta".into()),
                ],
            )
            .await?;
            tx.execute(
                "INSERT INTO machines(key, metrics_json) VALUES(?, ?)",
                vec![
                    SqlValue::Text("m1".into()),
                    SqlValue::Text("{\"cpu\":12}".into()),
                ],
            )
            .await?;
            tx.execute(
                "INSERT INTO hosts(key, seen_at) VALUES(?, ?)",
                vec![SqlValue::Text("h1".into()), SqlValue::Integer(1)],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(tx.token())
        })
        .await?;
    assert!(matches!(
        a.database.transaction_outcome(seed).await?,
        Some(TransactionOutcome::Applied { .. })
    ));
    sync_all(&[&a, &b, &c]).await?;

    let baseline_a = directory_bytes(&dir_a)?;
    let baseline_b = directory_bytes(&dir_b)?;
    let baseline_c = directory_bytes(&dir_c)?;

    for tick in 0..50i64 {
        let prune_token = a
            .database
            .write(async |tx| {
                for index in 0..6 {
                    tx.execute(
                        &format!("DELETE FROM pruned_{index} WHERE done_at < ?"),
                        vec![SqlValue::Integer(1_000 + tick)],
                    )
                    .await?;
                }
                Ok::<_, rocky_sdk::Error>(tx.token())
            })
            .await?;
        assert_eq!(
            a.database.transaction_outcome(prune_token).await?,
            None,
            "0-row DELETE across 6 tables must not emit any frame or outcome"
        );

        let reinsert_token = b
            .database
            .write(async |tx| {
                tx.execute("DELETE FROM projects", Vec::new()).await?;
                tx.execute(
                    "INSERT INTO projects(key, name) VALUES(?, ?), (?, ?)",
                    vec![
                        SqlValue::Text("p1".into()),
                        SqlValue::Text("alpha".into()),
                        SqlValue::Text("p2".into()),
                        SqlValue::Text("beta".into()),
                    ],
                )
                .await?;
                Ok::<_, rocky_sdk::Error>(tx.token())
            })
            .await?;
        assert_eq!(
            b.database.transaction_outcome(reinsert_token).await?,
            None,
            "unchanged DELETE + INSERT must not emit any frame or outcome"
        );

        let noop_update_token = c
            .database
            .write(async |tx| {
                tx.execute(
                    "UPDATE machines SET metrics_json = ? WHERE key = ?",
                    vec![
                        SqlValue::Text("{\"cpu\":12}".into()),
                        SqlValue::Text("m1".into()),
                    ],
                )
                .await?;
                tx.execute(
                    "UPDATE machines SET metrics_json = ? WHERE key = ?",
                    vec![
                        SqlValue::Text("{\"cpu\":99}".into()),
                        SqlValue::Text("missing".into()),
                    ],
                )
                .await?;
                Ok::<_, rocky_sdk::Error>(tx.token())
            })
            .await?;
        assert_eq!(
            c.database.transaction_outcome(noop_update_token).await?,
            None,
            "unchanged and 0-row UPDATEs must not emit any frame or outcome"
        );
    }

    sync_all(&[&a, &b, &c]).await?;

    let after_a = directory_bytes(&dir_a)?;
    let after_b = directory_bytes(&dir_b)?;
    let after_c = directory_bytes(&dir_c)?;

    assert!(
        after_a <= baseline_a + 1_048_576,
        "replica a grew unexpectedly after 50 no-op ticks: {baseline_a} -> {after_a}"
    );
    assert!(
        after_b <= baseline_b + 1_048_576,
        "replica b grew unexpectedly after 50 no-op ticks: {baseline_b} -> {after_b}"
    );
    assert!(
        after_c <= baseline_c + 1_048_576,
        "replica c grew unexpectedly after 50 no-op ticks: {baseline_c} -> {after_c}"
    );

    let mixed = a
        .database
        .write(async |tx| {
            tx.execute(
                "DELETE FROM pruned_0 WHERE done_at < ?",
                vec![SqlValue::Integer(9_999)],
            )
            .await?;
            tx.execute(
                "UPDATE hosts SET seen_at = ? WHERE key = ?",
                vec![SqlValue::Integer(2), SqlValue::Text("h1".into())],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(tx.token())
        })
        .await?;
    assert!(matches!(
        a.database.transaction_outcome(mixed).await?,
        Some(TransactionOutcome::Applied { .. })
    ));

    sync_all(&[&a, &b, &c]).await?;

    for device in [&a, &b, &c] {
        assert_healthy(device).await?;
        assert_eq!(count_rows(&device.database, "projects").await?, 2);
        assert_eq!(count_rows(&device.database, "machines").await?, 1);
        assert_eq!(count_rows(&device.database, "hosts").await?, 1);
        for index in 0..6 {
            assert_eq!(
                count_rows(&device.database, &format!("pruned_{index}")).await?,
                0
            );
        }
    }

    for device in [a, b, c] {
        device.database.close().await?;
    }
    Ok(())
}
