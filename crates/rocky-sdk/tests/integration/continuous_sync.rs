use crate::common::{Device, Failure, introduce};
use rocky_sdk::{Database, Session, SqlValue, transport::memory::Hub};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const WRITERS: [&str; 3] = ["a", "b", "c"];
const READER_ROUNDS: usize = 300;
const WRITE_PERIOD: Duration = Duration::from_millis(20);

async fn replace_row(database: &Database, key: &str, title: &str) -> Result<(), Failure> {
    let mut tx = database.transaction().await?;
    tx.execute(
        "DELETE FROM items WHERE key=?",
        vec![SqlValue::Text(key.into())],
    )
    .await?;
    let id = tx.next_id("items", Vec::new()).await?;
    tx.execute(
        "INSERT INTO items(key,id,title,completed) VALUES(?,?,?,0)",
        vec![
            SqlValue::Text(key.into()),
            id.sql(),
            SqlValue::Text(title.into()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn titles(database: &Database) -> Result<Vec<String>, Failure> {
    let mut rows = database
        .query("SELECT title FROM items ORDER BY key", Vec::new())
        .await?;
    let mut titles = Vec::new();
    while let Some(row) = rows.next().await? {
        titles.push(row.get_at(0)?);
    }
    Ok(titles)
}

fn record(failures: &Mutex<Vec<String>>, site: &str, error: &dyn std::fmt::Display) {
    failures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(format!("{site}: {error}"));
}

async fn write_until(
    database: &Database,
    name: &str,
    stop: &AtomicBool,
    failures: &Mutex<Vec<String>>,
) -> u64 {
    let mut written = 0u64;
    let mut ticks = tokio::time::interval(WRITE_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while !stop.load(Ordering::SeqCst) {
        ticks.tick().await;
        match replace_row(database, name, &written.to_string()).await {
            Ok(()) => written += 1,
            Err(error) => record(failures, &format!("writer {name}"), &error),
        }
    }
    written
}

async fn sync_until(session: &Session, stop: &AtomicBool) -> u64 {
    let mut syncs = 0u64;
    while !stop.load(Ordering::SeqCst) {
        if session.sync_once().await.is_ok() {
            syncs += 1;
        }
        tokio::task::yield_now().await;
    }
    syncs
}

async fn sync_all(devices: &[Device]) -> Result<(), Failure> {
    for _ in 0..2 {
        for device in devices {
            device.sync().await.map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn soak_replicas_stay_usable_while_applying_a_steady_trickle_of_remote_writes()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| rocky_sdk::Network::Owned(hub.connector(name));
    let mut devices = vec![Device::create(&root.path().join("a"), Some(network("a")))?];
    for name in ["b", "c", "reader"] {
        let invitation = devices[0].invite().await;
        let joined = Device::join(&root.path().join(name), &invitation, network(name))?;
        for known in &devices {
            introduce(known, &joined).await;
        }
        devices.push(joined);
    }
    sync_all(&devices).await?;
    for (device, name) in devices.iter().zip(WRITERS) {
        replace_row(&device.database, name, "seed").await?;
    }
    sync_all(&devices).await?;

    let stop = AtomicBool::new(false);
    let failures = Mutex::new(Vec::new());
    let reader = devices.last().ok_or("no reader")?;
    let observed = async {
        let mut remote_changes = 0usize;
        let mut last = Vec::new();
        for round in 0..READER_ROUNDS {
            match titles(&reader.database).await {
                Ok(seen) => {
                    if !last.is_empty() && seen[..WRITERS.len()] != last[..WRITERS.len()] {
                        remote_changes += 1;
                    }
                    last = seen;
                }
                Err(error) => record(&failures, "reader query", &error),
            }
            if let Err(error) = replace_row(&reader.database, "reader", &round.to_string()).await {
                record(&failures, "reader allocation", &error);
            }
            tokio::task::yield_now().await;
        }
        stop.store(true, Ordering::SeqCst);
        remote_changes
    };
    let writers = futures_util::future::join_all(
        devices
            .iter()
            .zip(WRITERS)
            .map(|(device, name)| write_until(&device.database, name, &stop, &failures)),
    );
    let syncers = futures_util::future::join_all(
        devices
            .iter()
            .map(|device| sync_until(device.session(), &stop)),
    );
    let (remote_changes, written, syncs) = tokio::join!(observed, writers, syncers);
    for device in &devices {
        device.close().await;
    }

    let failures = failures
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        failures.is_empty(),
        "{} statements failed while replicas synced continuously (written {written:?}, syncs {syncs:?}, remote changes read {remote_changes}), first: {:?}",
        failures.len(),
        failures.iter().take(8).collect::<Vec<_>>()
    );
    Ok(())
}
