//! Deterministic network faults on the in-process transport: disconnects,
//! backpressure and inbound work over budget. Durable outgoing work
//! survives, nothing is applied in part and resources stay bounded.
use common::{Device, Failure};
use rocky_sdk::{
    Network,
    transport::{Connector, Protocol, memory::Hub},
};
use serde_json::json;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

mod common;

const ROWS: u8 = 30;

fn memory(hub: &Arc<Hub>, name: &str) -> Network {
    Network::Owned(hub.connector(name))
}
/// A creator and a member that each hold `ROWS` rows the other lacks.
async fn diverged(root: &std::path::Path, hub: &Arc<Hub>) -> Result<(Device, Device), Failure> {
    let a = Device::create(&root.join("a"), Some(memory(hub, "a")))?;
    let b = Device::join(&root.join("b"), &a.invite().await, memory(hub, "b"))?;
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

#[tokio::test]
async fn a_cut_stream_loses_nothing_and_applies_nothing_in_part() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (a, b) = diverged(root.path(), &hub).await?;
    let mut failed = 0;
    // Cuts during the handshake, the push and the pull.
    for bytes in [0, 300, 3_000, 9_000, 20_000, 45_000] {
        b.ok(json!({"type": "connection", "online": true})).await;
        hub.cut_after(Some(bytes));
        failed += usize::from(b.sync().await.is_err());
        hub.cut_after(None);
        // `items` reads both fields of every row; a partial row would fail it.
        let (ours, theirs) = (b.items().await, a.items().await);
        assert!(holds(&ours, &own('b')) && holds(&theirs, &own('a')));
        assert!(ours.iter().chain(&theirs).all(|row| union().contains(row)));
    }
    assert!(failed >= 3, "only {failed} cuts interrupted a sync");
    assert_eq!(
        b.peer(&a.node().await).await["state"],
        if b.items().await == union() {
            "connected"
        } else {
            "unreachable"
        }
    );

    // The interrupted sync repeats safely, and a duplicate changes nothing.
    b.ok(json!({"type": "connection", "online": true})).await;
    for _ in 0..2 {
        b.sync().await.map_err(|error| error.to_string())?;
        assert_eq!(b.items().await, union());
        assert_eq!(a.items().await, union());
    }
    a.close().await;
    b.close().await;
    let a = Device::open(&root.path().join("a"), None)?;
    assert_eq!(a.items().await, union());
    a.close().await;
    Ok(())
}

#[tokio::test]
async fn a_slow_reader_holds_the_writer_back_without_loss() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    // Writers wait for the reader after 4 KiB, far below a page of frames.
    // Both ends send their handshake before reading, so a stream must
    // buffer that much; see the transport contract.
    hub.capacity.store(4096, Ordering::SeqCst);
    let (a, b) = diverged(root.path(), &hub).await?;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(b.items().await, union());
    assert_eq!(a.items().await, union());
    a.close().await;
    b.close().await;
    Ok(())
}

async fn inbound(device: &Device, field: &str) -> u64 {
    let value = device.ok(json!({"type": "network"})).await["inbound"][field].clone();
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .unwrap_or(u64::MAX)
}
async fn until(device: &Device, field: &str, expected: u64) -> Result<(), Failure> {
    for _ in 0..2_000 {
        if inbound(device, field).await == expected {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    Err(format!("inbound {field} never reached {expected}").into())
}

#[tokio::test]
async fn inbound_streams_over_budget_are_dropped_and_served_again_later() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (a, b) = diverged(root.path(), &hub).await?;
    let limit = inbound(&a, "limit").await;
    assert_eq!(limit, 32);
    // An unauthenticated dialer holds every slot without ever speaking.
    let stranger = hub.connector("x").start(false, [0; 32]).await?;
    let mut held = Vec::new();
    for open in 1..=limit {
        held.push(stranger.connect("a", Protocol::Sync).await?);
        until(&a, "active", open).await?;
    }
    for dropped in 1..=3 {
        held.push(stranger.connect("a", Protocol::Sync).await?);
        until(&a, "refused", dropped).await?;
    }
    assert_eq!(inbound(&a, "active").await, limit);
    // The database is as usable as before, and nothing was stored.
    assert_eq!(a.items().await, own('a'));
    // An enrolled device is dropped too while the slots are taken.
    assert!(b.sync().await.is_err());

    drop(held);
    until(&a, "active", 0).await?;
    b.ok(json!({"type": "connection", "online": true})).await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.items().await, union());
    stranger.close().await;
    a.close().await;
    b.close().await;
    Ok(())
}
