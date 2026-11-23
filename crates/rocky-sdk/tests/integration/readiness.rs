use crate::common;
use common::{Device, Failure};
use rocky_sdk::{Network, transport::memory::Hub};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn status_tracks_committed_work_and_logout_and_close_ends_a_status_wait()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let device = Device::create(root.path(), None).await?;
    let mut changes = device.database.status_changes();
    let status = changes.next().await?;
    assert!(!status.authority.expired);
    assert_eq!(
        status.network.attachment,
        rocky_sdk::AttachmentState::Detached
    );
    assert!(matches!(status.authority.access, rocky_sdk::Access::Full));
    device.add(1, "apples").await;
    let next = changes.next().await?;
    assert_ne!(
        next.token, status.token,
        "a revision racing registration is returned immediately, never lost"
    );
    assert_eq!(device.titles().await, ["apples"]);
    let (ended, closed) = tokio::join!(changes.next(), device.database.close());
    closed?;
    assert_eq!(
        ended.unwrap_err().kind,
        rocky_sdk::ErrorKind::Service(rocky_sdk::ServiceError::Closed("database"))
    );
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(
        &root.path().join("issuer"),
        Some(Network::Owned(hub.connector("issuer"))),
    )
    .await?;
    let member = Device::join(
        &root.path().join("member"),
        &issuer.invite().await,
        Network::Owned(hub.connector("member")),
    )
    .await?;
    let database = &member.database;
    database.logout(false).await?;
    let logged_out = database.status().await?;
    assert!(logged_out.authority.logged_out);
    assert_eq!(logged_out.network.connected_peer_count, 0);
    assert!(database.sync_now().await.is_err());
    for mode in [
        rocky_sdk::SyncMode::Off,
        rocky_sdk::SyncMode::Manual,
        rocky_sdk::SyncMode::Foreground,
        rocky_sdk::SyncMode::Background,
    ] {
        assert!(database.set_sync(mode).await.is_err(), "{mode:?}");
    }
    assert!(database.logout(true).await.is_err());
    member.close().await;
    issuer.close().await;
    Ok(())
}
