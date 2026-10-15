//! Identities through the SDK: further nodes of an enrolled identity join
//! without an issuer, and revoking one node or the identity stops exactly
//! those nodes. See `docs/runtime/sdk-enrollment.md#identities`.
use common::{Device, Failure};
use rocky_sdk::{
    Database, FileIdentityProvider, IdentityProvider, Network, Open, transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

mod common;

fn open<'a>(
    directory: &'a Path,
    hub: &Arc<Hub>,
    name: &str,
    identity: Option<Arc<dyn IdentityProvider>>,
) -> Open<'a> {
    Open {
        identity,
        directory,
        schema: common::SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
/// The identity in `directory`, as each of its node processes opens it.
fn person(directory: &Path) -> Result<Arc<dyn IdentityProvider>, Failure> {
    Ok(Arc::new(FileIdentityProvider::open(directory)?))
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    Ok(serde_json::from_slice(&std::fs::read(
        root.join(name).join("identity.json"),
    )?)?)
}
/// Whether `device` holds any secret of `role` in its file key store.
fn holds(root: &Path, name: &str, role: &str) -> Result<bool, Failure> {
    let installation = identity(root, name)?["installation"]
        .as_str()
        .ok_or("installation")?
        .to_owned();
    Ok(
        std::fs::read_dir(root.join(name).join("secrets"))?.any(|entry| {
            entry.is_ok_and(|entry| {
                entry.file_name().to_string_lossy() == format!("rocky.{installation}.{role}")
            })
        }),
    )
}

/// The owner, a member identity invited once, and two further nodes of that
/// identity, each certified by its provider from a node of the same
/// identity while no issuer is reachable.
struct Household {
    _root: tempfile::TempDir,
    hub: Arc<Hub>,
    owner: Device,
    nodes: [Device; 3],
}
async fn household() -> Result<Household, Failure> {
    let root = tempfile::tempdir()?;
    let path = root.path();
    let hub = Arc::new(Hub::default());
    let owner = Device::new(Database::create(
        open(&path.join("a"), &hub, "a", None),
        None,
    )?)?;
    let first = Device::new(Database::join(
        open(
            &path.join("m1"),
            &hub,
            "m1",
            Some(person(&path.join("person"))?),
        ),
        &owner.invite().await,
    )?)?;
    common::introduce(&owner, &first).await;
    owner.add(1, "Milk").await;
    first.sync().await.map_err(|error| error.to_string())?;
    // No issuer answers from here on.
    owner.close().await;
    let second = Device::new(Database::enroll_identity(
        open(
            &path.join("m2"),
            &hub,
            "m2",
            Some(person(&path.join("person"))?),
        ),
        &first.contact().await,
    )?)?;
    let third = Device::new(Database::enroll_identity(
        open(
            &path.join("m3"),
            &hub,
            "m3",
            Some(person(&path.join("person"))?),
        ),
        &second.contact().await,
    )?)?;
    let owner = Device::open(&path.join("a"), Some(Network::Owned(hub.connector("a"))))?;
    Ok(Household {
        _root: root,
        hub,
        owner,
        nodes: [first, second, third],
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn further_nodes_of_an_identity_join_without_an_issuer() -> Result<(), Failure> {
    let household = household().await?;
    let root = household._root.path();
    let key = person(&root.join("person"))?.public_key()?.to_hex();
    let mut nodes = Vec::new();
    for (device, name) in household.nodes.iter().zip(["m1", "m2", "m3"]) {
        let session = device.ok(json!({"type": "session"})).await;
        // One identity holds the grants; each node signs with its own key
        // under its own certificate, and none of them holds an identity key.
        assert_eq!(session["principal"], json!(key), "{name}");
        assert_eq!(
            session["profile"],
            json!({"kind": "writer", "scopes": ["items"]})
        );
        nodes.push(session["node"].clone());
        assert!(!holds(root, name, "identity")?, "{name}");
        assert!(!holds(root, name, "publisher")?, "{name}");
        assert_eq!(identity(root, name)?["identity"], json!(key));
    }
    nodes.sort_by_key(Value::to_string);
    nodes.dedup();
    assert_eq!(nodes.len(), 3);
    // The third node reads what the first wrote, through the second.
    household.nodes[1]
        .sync()
        .await
        .map_err(|error| error.to_string())?;
    household.nodes[2]
        .sync()
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(household.nodes[2].titles().await, ["Milk"]);
    // Each node writes its identity's table under its own number: the rows
    // never collide and every node reads them all.
    let mut ids = Vec::new();
    for (index, device) in household.nodes.iter().enumerate() {
        ids.push(device.add(10 + index as u8, &format!("Node {index}")).await);
    }
    for index in [2, 1, 0, 1, 2] {
        household.nodes[index]
            .sync()
            .await
            .map_err(|error| error.to_string())?;
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3);
    for device in &household.nodes {
        assert_eq!(
            device.titles().await,
            ["Milk", "Node 0", "Node 1", "Node 2"]
        );
    }
    // A provider of another identity is refused by the identity's nodes.
    let stranger = Database::enroll_identity(
        open(
            &root.join("x"),
            &household.hub,
            "x",
            Some(person(&root.join("stranger"))?),
        ),
        &household.nodes[0].contact().await,
    );
    assert!(stranger.is_err());
    for device in household.nodes.iter().chain([&household.owner]) {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_node_stops_it_alone_and_revoking_the_identity_stops_all_its_nodes()
-> Result<(), Failure> {
    let household = household().await?;
    let Household { owner, nodes, .. } = &household;
    for node in nodes {
        common::introduce(owner, node).await;
    }
    let key = person(&household._root.path().join("person"))?
        .public_key()?
        .to_hex();
    // One node certificate.
    owner
        .ok(json!({"type": "revoke", "node": nodes[1].node().await}))
        .await;
    let synced = async |device: &Device| {
        device
            .sync()
            .await
            .is_ok_and(|report| common::reconciled(&report))
    };
    assert!(synced(&nodes[0]).await);
    assert!(synced(&nodes[2]).await);
    let refused = nodes[1].sync().await.err().ok_or("a revoked node synced")?;
    assert_eq!(refused["code"], "revoked", "{refused}");
    // The identity: every node of it, those the owner never recorded too.
    owner.ok(json!({"type": "revoke", "identity": key})).await;
    for node in [&nodes[0], &nodes[2]] {
        let refused = node.sync().await.err().ok_or("a revoked identity synced")?;
        assert_eq!(refused["code"], "revoked", "{refused}");
    }
    // The owner's own identity is never revoked.
    let own = owner.ok(json!({"type": "session"})).await["principal"].clone();
    assert!(
        owner
            .call(json!({"type": "revoke", "identity": own}))
            .await
            .is_err()
    );
    for device in nodes.iter().chain([owner]) {
        device.close().await;
    }
    Ok(())
}
