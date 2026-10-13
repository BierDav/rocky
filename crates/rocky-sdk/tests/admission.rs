//! Roster-free admission on the in-process hub: acceptance 5 of
//! `docs/features_07/scope-mesh.md` and a device revoked while offline. See
//! `docs/decisions/0051-roster-free-admission.md`.
use common::{Device, Failure};
use rocky_sdk::{Network, transport::memory::Hub};
use serde_json::json;
use std::sync::Arc;

mod common;

async fn statements(device: &Device) -> serde_json::Value {
    device.ok(json!({"type": "session"})).await["revocations"]["statements"].clone()
}

/// C admits B, which it has no contact for, and refuses D in the round in
/// which A told it of D's revocation. D, revoked while it was away, learns
/// it from C with no issuer running, reports `revoked` and stops syncing.
#[tokio::test]
async fn a_member_admits_a_peer_without_a_contact_and_refuses_one_revoked_this_round()
-> Result<(), Failure> {
    let root = common::identity::small_directory()?;
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
    let (b_node, d_node) = (b.node().await, d.node().await);
    let c_contact = c.contact().await;
    for device in [&a, b, d] {
        device
            .add_peer(&c_contact)
            .await
            .map_err(|e| e.to_string())?;
    }

    // C has no contact for B and serves it: B's chain admits it.
    b.add(1, "Milk").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(c.titles().await, ["Milk"]);
    let met = c.peer(&b_node).await;
    assert_eq!(met["is_configured"], false, "{met}");
    assert!(
        met["last_authenticated_exchange_at_epoch_seconds"].is_number(),
        "{met}"
    );

    // A revokes D while D is away and tells C, then is gone.
    a.ok(json!({"type": "revoke", "node": d_node})).await;
    a.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(statements(c).await, 1);
    a.close().await;

    // In the same round C refuses D, which learns why from C.
    d.add(2, "Jam").await;
    let refused = d.sync().await.err().ok_or("a revoked device synced")?;
    assert_eq!(refused["code"], "revoked", "{refused}");
    assert!(!d.ok(json!({"type": "status"})).await["authority"]["verified_revocation"].is_null());
    assert_eq!(c.titles().await, ["Milk"]);
    assert_eq!(c.peer(&d_node).await, serde_json::Value::Null);
    // D stops syncing: it is refused before it dials.
    let again = d.sync().await.err().ok_or("a revoked device synced")?;
    assert_eq!(again["code"], "revoked", "{again}");
    // B and C still sync.
    b.add(3, "Tea").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(c.titles().await, ["Milk", "Tea"]);
    assert_eq!(statements(b).await, 1);
    for device in [b, c, d] {
        device.close().await;
    }
    Ok(())
}
