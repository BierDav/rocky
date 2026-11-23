use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{
    Network,
    transport::{Connector, Protocol, memory::Hub},
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use crate::common;

const ROWS: u8 = 30;

fn memory(hub: &Arc<Hub>, name: &str) -> Network {
    Network::Owned(hub.connector(name))
}
async fn diverged(root: &std::path::Path, hub: &Arc<Hub>) -> Result<(Device, Device), Failure> {
    let a = Device::create(&root.join("a"), Some(memory(hub, "a"))).await?;
    let b = Device::join(&root.join("b"), &a.invite().await, memory(hub, "b")).await?;
    b.sync().await.map_err(|error| error.to_string())?;
    for row in 0..ROWS {
        a.add(row + 1, &format!("a{row:02}")).await;
        b.add(row + 101, &format!("b{row:02}")).await;
    }
    Ok((a, b))
}
fn own(prefix: char) -> Vec<String> {
    (0..ROWS).map(|row| format!("{prefix}{row:02}/0")).collect()
}
fn union() -> Vec<String> {
    [own('a'), own('b')].concat()
}
fn holds(items: &[String], rows: &[String]) -> bool {
    rows.iter().all(|row| items.contains(row))
}
const CUTS_THROUGH_HANDSHAKE_PUSH_AND_PULL: [usize; 6] = [0, 300, 3_000, 9_000, 20_000, 45_000];
const SLOW_READER_CAPACITY: usize = 4096;

#[tokio::test]
async fn a_cut_stream_loses_nothing_and_applies_nothing_in_part() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (a, b) = diverged(root.path(), &hub).await?;
    let mut failed = 0;
    for bytes in CUTS_THROUGH_HANDSHAKE_PUSH_AND_PULL {
        snap::ok(b.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
        hub.cut_after(Some(bytes));
        failed += usize::from(common::exchange_failed(&b.sync().await));
        hub.cut_after(None);
        let (ours, theirs) = (b.items().await, a.items().await);
        assert!(
            holds(&ours, &own('b')) && holds(&theirs, &own('a')),
            "a cut at {bytes} bytes loses no row"
        );
        assert!(
            ours.iter().chain(&theirs).all(|row| union().contains(row)),
            "a cut at {bytes} bytes applies no row in part"
        );
    }
    assert!(failed >= 3, "only {failed} cuts interrupted a sync");
    let peer = b.peer(&a.node().await).await;
    let last = peer["attempts"]
        .as_array()
        .expect("attempts")
        .iter()
        .rev()
        .find(|attempt| attempt["operation"] == "reconcile")
        .expect("reconciliation");
    assert_eq!(last["outcome"] == "completed", b.items().await == union());

    snap::ok(b.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    for _ in 0..2 {
        b.sync().await.map_err(|error| error.to_string())?;
        assert_eq!(
            b.items().await,
            union(),
            "an interrupted sync repeats safely and a duplicate changes nothing"
        );
        assert_eq!(a.items().await, union());
    }
    a.close().await;
    b.close().await;
    let a = Device::open(&root.path().join("a"), None).await?;
    assert_eq!(a.items().await, union());
    a.close().await;
    Ok(())
}

#[tokio::test]
async fn a_slow_reader_holds_the_writer_back_without_loss() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    hub.capacity.store(SLOW_READER_CAPACITY, Ordering::SeqCst);
    let (a, b) = diverged(root.path(), &hub).await?;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(b.items().await, union());
    assert_eq!(a.items().await, union());
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn inbound_streams_over_budget_are_dropped_and_served_again_later() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (a, b) = diverged(root.path(), &hub).await?;
    let mut changes = hub.admission_changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            changes.borrow_and_update();
            if hub.rpc_admission("a").ok_or("RPC listener")?.active == 0 {
                return Ok::<_, Failure>(());
            }
            changes.changed().await?;
        }
    })
    .await??;
    let limit = hub.rpc_admission("a").ok_or("RPC listener")?.limit;
    assert_eq!(limit, 32);
    let stranger = hub.connector("x").start(false, [0; 32]).await?;
    let mut held_without_speaking = Vec::new();
    for open in 1..=limit {
        held_without_speaking.push(stranger.connect("a", Protocol::Rpc, None).await?);
        assert_eq!(hub.rpc_admission("a").ok_or("RPC listener")?.active, open);
    }
    for dropped in 1..=3 {
        assert!(stranger.connect("a", Protocol::Rpc, None).await.is_err());
        assert_eq!(
            hub.rpc_admission("a").ok_or("RPC listener")?.refused,
            dropped
        );
    }
    assert_eq!(hub.rpc_admission("a").ok_or("RPC listener")?.active, limit);
    assert_eq!(
        a.database
            .status()
            .await?
            .network
            .inbound
            .ok_or("database admission")?
            .active,
        0
    );
    assert_eq!(
        a.items().await,
        own('a'),
        "a stranger holding every RPC slot stores nothing"
    );
    b.sync()
        .await
        .map_err(|error| format!("links have their own budget beside RPC: {error}"))?;
    assert_eq!(a.items().await, union());

    let mut changed = hub.admission_changes();
    drop(held_without_speaking);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            changed.borrow_and_update();
            if hub.rpc_admission("a").ok_or("RPC listener")?.active == 0 {
                return Ok::<_, Failure>(());
            }
            changed.changed().await?;
        }
    })
    .await??;
    drop(stranger.connect("a", Protocol::Rpc, None).await?);
    stranger.close().await;
    a.close().await;
    b.close().await;
    Ok(())
}
