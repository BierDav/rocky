use crate::common::{Device, Failure, introduce};
use rocky_sdk::{Database, Error, ErrorKind, SqlValue, transport::memory::Hub};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Barrier;

const WRITERS: [&str; 3] = ["a", "b", "c"];
const READER: &str = "reader";
const ROUNDS: usize = 40;

async fn replace_row(database: &Database, key: &str, title: &str) -> Result<(), Error> {
    let mut tx = database.transaction().await?;
    tx.execute(
        "DELETE FROM items WHERE key=?",
        vec![SqlValue::Text(key.into())],
    )
    .await?;
    tx.execute(
        "INSERT INTO items(key,title,completed) VALUES(?,?,0)",
        vec![SqlValue::Text(key.into()), SqlValue::Text(title.into())],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn rows(database: &Database) -> Result<Vec<(String, String)>, Error> {
    let mut rows = database
        .query("SELECT key, title FROM items ORDER BY key", Vec::new())
        .await?;
    let mut keyed = Vec::new();
    while let Some(row) = rows.next().await? {
        keyed.push((row.get_at(0)?, row.get_at(1)?));
    }
    Ok(keyed)
}

struct Outcomes {
    busy: AtomicUsize,
    failures: Mutex<Vec<String>>,
}
impl Outcomes {
    async fn settle<T, F: Future<Output = Result<T, Error>>>(
        &self,
        site: &str,
        mut call: impl FnMut() -> F,
    ) -> Option<T> {
        loop {
            match call().await {
                Ok(value) => return Some(value),
                Err(error) if error.kind() == &ErrorKind::Busy => {
                    self.busy.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                }
                Err(error) => {
                    self.failures
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!("{site}: {error}"));
                    return None;
                }
            }
        }
    }
}

async fn sync_until(database: &Database, stop: &AtomicBool) -> usize {
    let mut syncs = 0;
    while !stop.load(Ordering::SeqCst) {
        if database.sync_now().await.is_ok() {
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
    let mut devices = vec![Device::create(&root.path().join("a"), Some(network("a"))).await?];
    for name in ["b", "c", READER] {
        let invitation = devices[0].invite().await;
        let joined = Device::join(&root.path().join(name), &invitation, network(name)).await?;
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
    let outcomes = Outcomes {
        busy: AtomicUsize::new(0),
        failures: Mutex::new(Vec::new()),
    };
    let rounds = Barrier::new(WRITERS.len() + 1);
    let reader = devices.last().ok_or("no reader")?;
    let each_round = async |database: &Database, name: &str| {
        for round in 0..ROUNDS {
            rounds.wait().await;
            let title = round.to_string();
            if name == READER {
                outcomes.settle("reader query", || rows(database)).await;
            }
            outcomes
                .settle(&format!("{name} write"), || {
                    replace_row(database, name, &title)
                })
                .await;
        }
    };
    let rounds_and_stop = async {
        futures_util::future::join_all(
            devices
                .iter()
                .zip(WRITERS)
                .map(|(device, name)| each_round(&device.database, name))
                .chain([each_round(&reader.database, READER)]),
        )
        .await;
        stop.store(true, Ordering::SeqCst);
    };
    let syncers = futures_util::future::join_all(
        devices
            .iter()
            .map(|device| sync_until(&device.database, &stop)),
    );
    let ((), syncs) = tokio::join!(rounds_and_stop, syncers);

    let failures = outcomes
        .failures
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        failures.is_empty(),
        "{} statements failed while replicas synced continuously (syncs {syncs:?}, busy retries {}), first: {:?}",
        failures.len(),
        outcomes.busy.load(Ordering::SeqCst),
        failures.iter().take(8).collect::<Vec<_>>()
    );
    sync_all(&devices).await?;
    let last = (ROUNDS - 1).to_string();
    let expected: Vec<_> = WRITERS
        .into_iter()
        .chain([READER])
        .map(|key| (key.to_owned(), last.clone()))
        .collect();
    for device in &devices {
        assert_eq!(
            rows(&device.database).await?,
            expected,
            "every replica holds each device's last round once the trickle stops"
        );
    }
    for device in &devices {
        device.close().await;
    }
    Ok(())
}
