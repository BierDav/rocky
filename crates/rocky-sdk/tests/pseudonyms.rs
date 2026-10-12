//! Stable, durable author allocation through the native call contract.
use common::{Device, Failure};
use rocky_credentials::{CredentialChain, MAX_AUTHOR};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

mod common;

fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    Ok(serde_json::from_slice(&std::fs::read(
        root.join(name).join("identity.json"),
    )?)?)
}
fn author(chain: &Value) -> Result<u32, Failure> {
    let chain: CredentialChain = serde_json::from_value(chain.clone())?;
    chain
        .authors()?
        .and_then(|range| range.single())
        .ok_or("author".into())
}
async fn request(device: &Device) -> Value {
    device
        .ok(json!({"type": "pseudonym_request", "collection": "items"}))
        .await["request"]
        .clone()
}
async fn assign(assigner: &Device, request: &Value) -> Value {
    assigner
        .ok(json!({"type": "pseudonym_assign", "request": request}))
        .await["pseudonym"]
        .clone()
}
async fn install(device: &Device, chain: &Value) -> Value {
    device
        .ok(json!({"type": "pseudonym_install", "pseudonym": chain}))
        .await
}
fn open<'a>(path: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        directory: path,
        schema: common::SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        commands: None,
    }
}

#[tokio::test]
async fn pseudonym_retries_reopen_and_renewal_keep_the_author() -> Result<(), Failure> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let hub = Arc::new(Hub::default());
    let mut a = Device::create(&root.join("a"), Some(Network::Owned(hub.connector("a"))))?;
    let w = Device::join(
        &root.join("w"),
        &a.invite().await,
        Network::Owned(hub.connector("w")),
    )?;
    let asked = request(&w).await;
    // Without a network, x cannot ask for its author before the fault.
    let x_keys = Database::device(&root.join("x"), None)?;
    let supplied = a.ok(json!({"type": "grant", "device": x_keys, "profile": {"kind": "writer", "scopes": ["items"]}})).await["supplied"].clone();
    let x = Device::new(Database::install(
        Open {
            network: None,
            ..open(&root.join("x"), &hub, "x")
        },
        serde_json::from_value(supplied)?,
    )?)?;
    // Stop automatic sync before injecting a failure in the shared writer path.
    a.network.take().ok_or("attached network")?.close().await;
    // A failed durable write returns no assignment and consumes no number.
    let before = identity(root, "a")?;
    std::fs::create_dir(root.join("a/identity.tmp"))?;
    assert!(
        a.call(json!({"type": "pseudonym_assign", "request": request(&x).await}))
            .await
            .is_err()
    );
    // An assignment the registry already holds needs no write.
    assert_eq!(author(&assign(&a, &asked).await)?, 2);
    assert_eq!(identity(root, "a")?, before);
    std::fs::remove_dir(root.join("a/identity.tmp"))?;
    x.close().await;
    a.network = Some(
        a.database
            .attach(Network::Owned(hub.connector("a")))
            .await?,
    );
    // The issuer's first registry write takes its own registry author, 1.
    let chain = assign(&a, &asked).await;
    assert_eq!(author(&chain)?, 2);
    // A lost first answer is retried without a previous chain: the
    // registry holds the assignment.
    assert_eq!(author(&assign(&a, &asked).await)?, 2);
    assert_eq!(identity(root, "a")?["minting"]["next"], 3);
    assert_eq!(install(&w, &chain).await["author"], 2);
    assert_eq!(
        a.ok(json!({"type": "pseudonym_owner", "pseudonym": chain}))
            .await["node"],
        w.node().await
    );
    assert!(
        w.call(json!({"type": "pseudonym_owner", "pseudonym": chain}))
            .await
            .is_err()
    );
    assert!(
        a.call(json!({"type": "pseudonym_install", "pseudonym": chain}))
            .await
            .is_err()
    );
    assert!(
        w.call(json!({"type": "pseudonym_request", "collection": "missing"}))
            .await
            .is_err()
    );
    a.close().await;
    a = Device::open(&root.join("a"), Some(Network::Owned(hub.connector("a"))))?;
    assert_eq!(author(&assign(&a, &asked).await)?, 2);
    w.close().await;
    let w = Device::open(&root.join("w"), Some(Network::Owned(hub.connector("w"))))?;
    assert_eq!(
        w.ok(json!({"type": "session"})).await["pseudonyms"]["items"],
        2
    );
    // Revoking another device leaves the chain current; a new assignment
    // after renewal retains the pseudonym key and number.
    let r = Device::join(
        &root.join("r"),
        &a.invite_as(json!({"kind": "reader", "scopes": ["items"]}))
            .await,
        Network::Owned(hub.connector("r")),
    )?;
    a.ok(json!({"type": "revoke", "node": r.node().await}))
        .await;
    w.ok(json!({"type": "renew"})).await;
    w.ok(json!({"type": "pseudonym_install", "pseudonym": chain}))
        .await;
    let renewed = assign(&a, &request(&w).await).await;
    assert_eq!(author(&renewed)?, 2);
    install(&w, &renewed).await;
    assert!(
        a.call(json!({"type": "pseudonym_assign", "request": request(&r).await}))
            .await
            .is_err()
    );
    for d in [&a, &w, &r] {
        d.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn issuers_split_ranges_and_delegates_carve_durably() -> Result<(), Failure> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let hub = Arc::new(Hub::default());
    let a = Device::create(&root.join("a"), Some(Network::Owned(hub.connector("a"))))?;
    let b = Device::join(
        &root.join("b"),
        &a.invite_as(json!({"kind": "issuer"})).await,
        Network::Owned(hub.connector("b")),
    )?;
    let left = identity(root, "a")?["minting"]["range"].clone();
    let right = identity(root, "b")?["minting"]["range"].clone();
    assert_eq!(
        left["last"].as_u64().ok_or("last")? + 1,
        right["first"].as_u64().ok_or("first")?
    );
    assert_eq!(right["last"], MAX_AUTHOR);
    // Each issuer's first registry write takes the first of its numbers.
    let chain = assign(&b, &request(&a).await).await;
    assert_eq!(
        author(&chain)?,
        right["first"].as_u64().ok_or("first")? as u32 + 1
    );
    install(&a, &chain).await;
    let keys = Database::device(&root.join("d"), None)?;
    let profile = json!({"kind": "writer", "scopes": ["items"]});
    let supplied = a.ok(json!({"type": "grant", "device": keys, "profile": profile, "authors": 2, "delegations": 2})).await["supplied"].clone();
    let d = Device::new(Database::install(
        open(&root.join("d"), &hub, "d"),
        serde_json::from_value(supplied.clone())?,
    )?)?;
    let held = identity(root, "d")?["minting"]["range"].clone();
    assert_eq!(held, json!({"first": 2, "last": 3}));
    let e_keys = Database::device(&root.join("e"), None)?;
    assert!(
        d.call(json!({"type": "delegate", "device": e_keys, "profile": profile, "authors": 3}))
            .await
            .is_err()
    );
    let supplied_e = d
        .ok(json!({"type": "delegate", "device": e_keys, "profile": profile, "authors": 1}))
        .await["supplied"]
        .clone();
    let e = Device::new(Database::install(
        open(&root.join("e"), &hub, "e"),
        serde_json::from_value(supplied_e)?,
    )?)?;
    assert!(
        d.call(json!({"type": "pseudonym_assign", "request": request(&d).await}))
            .await
            .is_err()
    );
    let asked = request(&e).await;
    let assigned = assign(&d, &asked).await;
    assert_eq!(author(&assigned)?, 3);
    install(&e, &assigned).await;
    let e_chain = assign(&e, &request(&a).await).await;
    assert_eq!(author(&e_chain)?, 2);
    assert_eq!(
        a.ok(json!({"type": "pseudonym_owner", "pseudonym": e_chain}))
            .await["node"],
        a.node().await
    );
    assert!(
        d.call(json!({"type": "pseudonym_assign", "request": request(&a).await}))
            .await
            .is_err()
    );
    // Reinstalling the same range does not reset the consumed counter. A
    // non-issuer keeps no assignment record: a retry presenting its
    // previous chain keeps the number.
    d.ok(json!({"type": "install", "supplied": supplied})).await;
    assert_eq!(identity(root, "d")?["minting"]["next"], 4);
    assert_eq!(author(&assign(&d, &request(&e).await).await)?, 3);
    for device in [&a, &b, &d, &e] {
        device.close().await;
    }
    Ok(())
}
