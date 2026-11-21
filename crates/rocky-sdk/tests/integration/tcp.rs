use crate::common::{self, Device, Failure};
use crate::support::snapshot as snap;
use rocky_sdk::Network;
use std::{net::SocketAddr, sync::Arc};

fn tcp() -> Network {
    Network::Owned(Arc::new(rocky_tcp::Options::new(SocketAddr::from((
        [127, 0, 0, 1],
        0,
    )))))
}

#[tokio::test]
async fn devices_enroll_and_sync_both_ways_over_plain_tcp() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(&root.path().join("a"), Some(tcp()))?;
    let invitation = a.invite().await;
    let b = Device::join(&root.path().join("b"), &invitation, tcp())?;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(snap::ok(a.session.domain()).await["transport"], "tcp");

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
