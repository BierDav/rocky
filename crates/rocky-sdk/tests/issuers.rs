//! Issuer recovery: a second issuer keeps a domain enrolling, renewing and
//! revoking after its creator is lost. See `docs/runtime/sdk-enrollment.md`.
use common::{Device, Failure};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

mod common;

fn open<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        directory,
        schema: common::SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        commands: None,
    }
}
fn create(root: &Path, hub: &Arc<Hub>, name: &str) -> Result<Device, Failure> {
    Device::new(Database::create(open(&root.join(name), hub, name), None)?)
}
fn join(root: &Path, hub: &Arc<Hub>, name: &str, invitation: &str) -> Result<Device, Failure> {
    Device::new(Database::join(
        open(&root.join(name), hub, name),
        invitation,
    )?)
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    Ok(serde_json::from_slice(&std::fs::read(
        root.join(name).join("identity.json"),
    )?)?)
}
/// The issuers a device renews from, by node key.
fn issuers(root: &Path, name: &str) -> Result<Vec<String>, Failure> {
    Ok(serde_json::from_value(
        identity(root, name)?["issuers"].clone(),
    )?)
}
fn range(root: &Path, name: &str) -> Result<(u64, u64), Failure> {
    let range = &identity(root, name)?["minting"]["range"];
    Ok((
        range["first"].as_u64().ok_or("first")?,
        range["last"].as_u64().ok_or("last")?,
    ))
}
async fn synced(device: &Device) -> Result<Value, Failure> {
    device
        .sync()
        .await
        .map_err(|error| error.to_string().into())
}
async fn session(device: &Device) -> Value {
    device.ok(json!({"type": "session"})).await
}

#[tokio::test]
async fn a_second_issuer_carries_the_domain_after_the_creator_is_lost() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    // Enrolled before a second issuer exists: it knows only the creator.
    let early = join(root, &hub, "early", &a.invite().await)?;
    a.add(1, "Milk").await;
    synced(&early).await?;

    let b = join(
        root,
        &hub,
        "b",
        &a.invite_as(json!({"kind": "issuer"})).await,
    )?;
    let (a_node, b_node) = (a.node().await, b.node().await);
    // Each issuer knows the other's routes; no keys or author numbers are
    // shared beyond the publisher key.
    assert_eq!(issuers(root, "a")?, [b_node.as_str()]);
    assert_eq!(issuers(root, "b")?, [a_node.as_str()]);
    assert_eq!(
        identity(root, "a")?["peers"][&b_node]["routes"][0]["address"],
        "b"
    );
    assert_ne!(
        session(&a).await["principal"],
        session(&b).await["principal"]
    );
    let (a_range, b_range) = (range(root, "a")?, range(root, "b")?);
    assert!(a_range.1 < b_range.0, "{a_range:?} {b_range:?}");
    synced(&b).await?;
    assert_eq!(b.titles().await, ["Milk"]);

    // Devices enrolled or renewed after the pairing learn the second issuer.
    let late = join(root, &hub, "late", &a.invite().await)?;
    assert_eq!(issuers(root, "late")?, [a_node.clone(), b_node.clone()]);
    assert_eq!(issuers(root, "early")?, [a_node.as_str()]);
    early.ok(json!({"type": "renew"})).await;
    assert_eq!(issuers(root, "early")?, [a_node.clone(), b_node.clone()]);
    // The second issuer refuses a device it never saw; that device renews
    // from it rather than from the creator, and is served from then on.
    synced(&late).await?;
    let known = b.ok(json!({"type": "devices"})).await.to_string();
    assert!(known.contains(&late.node().await), "{known}");
    assert_eq!(
        identity(root, "late")?["peers"]
            .as_object()
            .map(|peers| peers.len()),
        Some(2)
    );

    // The creator device is destroyed.
    a.close().await;
    drop(a);
    std::fs::remove_dir_all(root.join("a"))?;

    // An issuer cannot revoke itself; it revokes the lost one.
    let error = b
        .call(json!({"type": "revoke", "node": b_node}))
        .await
        .err()
        .ok_or("revoked itself")?;
    assert_eq!(error["code"], "rejected", "{error}");
    let revoked = b.ok(json!({"type": "revoke", "node": a_node})).await;
    assert_eq!(revoked["epoch"], "2");
    assert!(issuers(root, "b")?.is_empty());
    assert!(identity(root, "b")?["peers"].get(&a_node).is_none());

    // The second issuer invites a third device, which syncs with it.
    let c = join(root, &hub, "c", &b.invite().await)?;
    assert_eq!(session(&c).await["epoch"], "2");
    synced(&c).await?;
    assert_eq!(c.titles().await, ["Milk"]);

    // A device the creator enrolled renews through the second issuer and
    // forgets the revoked creator.
    let before = session(&early).await;
    assert_eq!(before["epoch"], "1");
    early.ok(json!({"type": "renew"})).await;
    let after = session(&early).await;
    assert_eq!(after["epoch"], "2");
    assert!(after["expires"].as_str() >= before["expires"].as_str());
    assert_eq!(issuers(root, "early")?, [b_node.as_str()]);
    assert!(identity(root, "early")?["peers"].get(&a_node).is_none());
    early.add(2, "Eggs").await;
    synced(&early).await?;
    synced(&c).await?;
    assert_eq!(c.titles().await, ["Eggs", "Milk"]);

    // A refused sync renews from the second issuer. The device writes after
    // learning the revocation: frames signed under the ended epoch are
    // refused (credentials.md#limits).
    synced(&late).await?;
    assert_eq!(session(&late).await["epoch"], "2");
    assert_eq!(issuers(root, "late")?, [b_node.as_str()]);
    late.add(3, "Bread").await;
    synced(&late).await?;
    synced(&c).await?;
    assert_eq!(c.titles().await, ["Bread", "Eggs", "Milk"]);
    for device in [&b, &c, &early, &late] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_granted_credential_close_to_expiry_renews_through_the_second_issuer()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let b = join(
        root,
        &hub,
        "b",
        &a.invite_as(json!({"kind": "issuer"})).await,
    )?;
    let keys = Database::device(&root.join("g"), None)?;
    let supplied = a
        .ok(json!({"type": "grant", "device": keys, "profile": {"kind": "writer", "scopes": ["items"]}, "ttl": 60}))
        .await["supplied"]
        .clone();
    assert_eq!(supplied["issuers"].as_array().map(Vec::len), Some(2));
    let g = Device::new(Database::install(
        open(&root.join("g"), &hub, "g"),
        serde_json::from_value(supplied)?,
    )?)?;
    let granted: u64 = session(&g).await["expires"]
        .as_str()
        .ok_or("expires")?
        .parse()?;
    a.close().await;
    drop(a);
    std::fs::remove_dir_all(root.join("a"))?;

    g.ok(json!({"type": "renew"})).await;
    let renewed: u64 = session(&g).await["expires"]
        .as_str()
        .ok_or("expires")?
        .parse()?;
    // The second issuer renews it for the domain's lifetime, not 60 seconds.
    assert!(renewed > granted + 24 * 60 * 60, "{granted} {renewed}");
    g.add(1, "Milk").await;
    synced(&g).await?;
    assert_eq!(b.titles().await, ["Milk"]);
    for device in [&b, &g] {
        device.close().await;
    }
    Ok(())
}
