use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{Network, transport::memory::Hub};
use std::sync::Arc;

use crate::common;

async fn statements(device: &Device) -> serde_json::Value {
    snap::ok(device.session.info()).await["revocations"]["statements"].clone()
}

async fn finish_issuer_bootstrap(members: &[Device]) -> Result<(), Failure> {
    for device in members {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tokio::test]
async fn a_member_admits_a_peer_without_a_contact_and_refuses_one_revoked_this_round()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Network::Owned(hub.connector(name));
    let a = Device::create(&root.path().join("a"), Some(network("a")))?;
    let mut members = Vec::new();
    for name in ["b", "c", "d"] {
        members.push(Device::join(
            &root.path().join(name),
            &a.invite().await,
            network(name),
        )?);
    }
    let [b, c, d] = &members[..] else {
        return Err("members".into());
    };
    finish_issuer_bootstrap(&members).await?;
    let (b_node, d_node) = (b.node().await, d.node().await);
    let c_contact = c.contact().await;
    for device in [&a, b, d] {
        device
            .add_peer(&c_contact)
            .await
            .map_err(|e| e.to_string())?;
    }

    b.add(1, "Milk").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        c.titles().await,
        ["Milk"],
        "C serves B by its chain without a contact for it"
    );
    let met = c.peer(&b_node).await;
    assert_eq!(met["is_configured"], false, "{met}");
    assert!(
        met["last_authenticated_exchange_at_epoch_seconds"].is_number(),
        "{met}"
    );

    snap::ok(
        a.session
            .revoke_identity(&(snap::text(&(d.identity().await)))),
    )
    .await;
    a.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        statements(c).await,
        1,
        "A tells C of D's revocation while D is away"
    );
    a.close().await;

    d.add(2, "Jam").await;
    let refused = d.sync().await.err().ok_or("a revoked device synced")?;
    assert_eq!(refused["code"], "revoked", "{refused}");
    assert!(!snap::ok(d.session.status()).await["authority"]["verified_revocation"].is_null());
    assert_eq!(
        c.titles().await,
        ["Milk"],
        "C refuses D in the round it learned of the revocation"
    );
    assert_eq!(c.peer(&d_node).await, serde_json::Value::Null);
    let again = d
        .sync()
        .await
        .err()
        .ok_or("a revoked device dialed again")?;
    assert_eq!(
        again["code"], "revoked",
        "a revoked device is refused before it dials: {again}"
    );
    b.add(3, "Tea").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        c.titles().await,
        ["Milk", "Tea"],
        "unrevoked members keep syncing"
    );
    assert_eq!(statements(b).await, 1);
    for device in [b, c, d] {
        device.close().await;
    }
    Ok(())
}
