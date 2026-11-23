use crate::common;
use common::{Device, Failure};
use rocky_sdk::{Network, transport::memory::Hub};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn status_tracks_committed_work_and_logout_and_session_close_ends_only_its_wait()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let device = Device::create(root.path(), None)?;
    let session = device.database.session();
    let status = session.status().await?;
    assert!(!status.authority.expired);
    assert_eq!(
        status.network.attachment,
        rocky_sdk::AttachmentState::Detached
    );
    assert!(matches!(status.authority.access, rocky_sdk::Access::Full));
    device.add(1, "apples").await;
    let next = session.status_after(status.token.clone()).await?;
    assert_ne!(
        next.token, status.token,
        "a revision racing registration is returned immediately, never lost"
    );
    let wait = session.status_after(next.token);
    let close = session.close();
    let (ended, ()) = tokio::join!(wait, close);
    assert!(matches!(
        ended.unwrap_err().downcast_ref::<rocky_sdk::ServiceError>(),
        Some(rocky_sdk::ServiceError::Closed("session"))
    ));
    assert_eq!(device.titles().await, ["apples"]);
    device.close().await;
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(
        &root.path().join("issuer"),
        Some(Network::Owned(hub.connector("issuer"))),
    )?;
    let member = Device::join(
        &root.path().join("member"),
        &issuer.invite().await,
        Network::Owned(hub.connector("member")),
    )?;
    let session = member.database.session();
    session.logout(false).await?;
    let logged_out = session.status().await?;
    assert!(logged_out.authority.logged_out);
    assert_eq!(logged_out.network.connected_peer_count, 0);
    assert_eq!(session.network().await?.connected_peer_count, 0);
    session.peers().await?;
    assert!(session.sync_once().await.is_err());
    assert!(session.set_networking_enabled(true).await.is_err());
    assert!(session.set_automatic_sync(true).await.is_err());
    assert!(session.enter_foreground().await.is_err());
    assert!(session.enter_background().await.is_err());
    assert!(session.logout(true).await.is_err());
    session.close().await;
    member.close().await;
    issuer.close().await;
    Ok(())
}
