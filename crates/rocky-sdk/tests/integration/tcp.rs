use crate::common::{Device, Failure};
use rocky_sdk::{ErrorKind, Network, ServiceError};
use std::net::SocketAddr;

fn tcp() -> Network {
    rocky_tcp::Options::new(SocketAddr::from(([127, 0, 0, 1], 0))).into()
}

fn websocket() -> Network {
    rocky_tcp::Options::new(SocketAddr::from(([127, 0, 0, 1], 0)))
        .websocket()
        .into()
}

fn dial_only() -> Network {
    rocky_tcp::Options::dialer().into()
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
    let network = a.database.status().await?.network;
    assert_eq!(network.schemes, ["tcp", "ws", "wss"]);
    assert!(!network.dial_only);

    a.add(1, "Milk").await;
    let synced = b.sync_now().await?;
    assert!(synced.complete(), "{synced}");
    assert_eq!(b.titles().await, ["Milk"]);

    b.add(2, "Eggs").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    assert_eq!(a.items().await, b.items().await);

    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn a_dial_only_device_syncs_with_a_listening_peer_over_tcp() -> Result<(), Failure> {
    dial_only_syncs_with(tcp).await
}

#[tokio::test]
async fn a_dial_only_device_syncs_with_a_listening_peer_over_websockets() -> Result<(), Failure> {
    dial_only_syncs_with(websocket).await
}

async fn dial_only_syncs_with(listening: fn() -> Network) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(&root.path().join("a"), Some(listening())).await?;
    let b = Device::join(&root.path().join("b"), &a.invite().await, listening()).await?;
    let c = Device::join(&root.path().join("c"), &a.invite().await, dial_only()).await?;
    let status = c.database.status().await?.network;
    assert!(status.dial_only, "{status:?}");
    assert!(
        c.database.contact().await?.routes.is_empty(),
        "a dial-only device names no route"
    );

    a.add(1, "Milk").await;
    let synced = c.sync_now().await?;
    assert!(synced.complete(), "{synced}");
    assert_eq!(c.titles().await, ["Milk"]);

    c.add(2, "Eggs").await;
    c.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);

    b.sync().await.map_err(|error| error.to_string())?;
    let (known, complete) = known_peers_and_complete(&b).await?;
    assert!(
        known == 1 && complete,
        "the dial-only member leaves no dead route on its peers"
    );
    assert!(
        known_peers_and_complete(&a).await?.0 <= 1,
        "the dial-only member is no sync target of the admin"
    );
    assert_eq!(b.titles().await, ["Eggs", "Milk"]);

    a.close().await;
    b.close().await;
    c.close().await;
    Ok(())
}

async fn known_peers_and_complete(device: &Device) -> Result<(usize, bool), Failure> {
    match device.sync_now().await {
        Ok(report) => Ok((report.peers.len(), report.complete())),
        Err(error) => match error.kind() {
            ErrorKind::Service(ServiceError::NoPeerReached(report)) => {
                Ok((report.peers.len(), false))
            }
            _ => Err(error.into()),
        },
    }
}

#[tokio::test]
async fn the_issuer_reaches_its_joiner_without_add_peer() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let issuer = Device::create(&root.path().join("issuer"), Some(tcp())).await?;
    let invitation = issuer.invite().await;
    let joiner = Device::join(&root.path().join("joiner"), &invitation, tcp()).await?;

    joiner.add(1, "Milk").await;
    let synced = issuer.sync_now().await?;
    assert!(synced.complete(), "{synced}");
    assert_eq!(issuer.titles().await, ["Milk"]);

    joiner.close().await;
    let reopened = Device::open(&root.path().join("joiner"), Some(tcp())).await?;
    reopened.add(2, "Eggs").await;
    reopened.sync_now().await?;
    reopened.add(3, "Bread").await;
    let synced = issuer.sync_now().await?;
    assert!(
        synced.complete(),
        "the issuer dials the port the joiner advertised after reopening: {synced}"
    );
    assert_eq!(issuer.titles().await, ["Bread", "Eggs", "Milk"]);

    reopened.close().await;
    issuer.close().await;
    Ok(())
}
