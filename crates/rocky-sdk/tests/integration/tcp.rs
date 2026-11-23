use crate::common::{self, Device, Failure};
use rocky_sdk::Network;
use std::{net::SocketAddr, sync::Arc};

fn tcp() -> Network {
    Network::Owned(Arc::new(rocky_tcp::Options::new(SocketAddr::from((
        [127, 0, 0, 1],
        0,
    )))))
}

fn websocket() -> Network {
    Network::Owned(Arc::new(
        rocky_tcp::Options::new(SocketAddr::from(([127, 0, 0, 1], 0))).websocket(),
    ))
}

#[tokio::test]
async fn devices_enroll_and_sync_both_ways_over_plain_tcp() -> Result<(), Failure> {
    enroll_and_sync_both_ways(tcp).await
}

#[tokio::test]
async fn devices_enroll_and_sync_both_ways_over_websockets() -> Result<(), Failure> {
    enroll_and_sync_both_ways(websocket).await
}

async fn enroll_and_sync_both_ways(network: fn() -> Network) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(&root.path().join("a"), Some(network())).await?;
    let invitation = a.invite().await;
    let b = Device::join(&root.path().join("b"), &invitation, network()).await?;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        a.database.status().await?.network.kind.as_deref(),
        Some("tcp")
    );

    a.add(1, "Milk").await;
    let synced = b.sync().await.map_err(|error| error.to_string())?;
    assert!(common::reconciled(&synced), "{synced}");
    assert_eq!(b.titles().await, ["Milk"]);

    b.add(2, "Eggs").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    assert_eq!(a.items().await, b.items().await);

    a.close().await;
    b.close().await;
    Ok(())
}
