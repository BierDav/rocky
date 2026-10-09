//! Direct sync between configured authorized peers, and identities that
//! do not depend on routes or transport kinds. See
//! `docs/runtime/sdk-enrollment.md` and `docs/runtime/sdk-transports.md`.
use common::{Device, Failure, introduce};
use data_encoding::BASE64URL_NOPAD;
use rocky_sdk::{
    Network,
    transport::{Connector, TransportOptions, memory::Hub},
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

mod common;

fn memory(hub: &Arc<Hub>, name: &str) -> Network {
    Network::Owned(hub.connector(name))
}
fn tcp() -> Network {
    Network::Owned(Arc::new(TransportOptions::Tcp {
        listen: Some("127.0.0.1:0".into()),
        advertised: None,
    }))
}
/// A creator and members named `b`, `c`, … enrolled over `hub`.
async fn enroll(root: &Path, hub: &Arc<Hub>, members: &[&str]) -> Result<Vec<Device>, Failure> {
    let creator = Device::create(&root.join("a"), Some(memory(hub, "a")))?;
    let mut devices = Vec::new();
    for name in members {
        let invitation = creator.invite().await;
        let member = Device::join(&root.join(name), &invitation, memory(hub, name))?;
        member.sync().await.map_err(|error| error.to_string())?;
        devices.push(member);
    }
    devices.insert(0, creator);
    Ok(devices)
}

#[tokio::test]
async fn enrolled_peers_sync_directly_while_the_creator_is_offline() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let devices = enroll(root.path(), &hub, &["b", "c", "d"]).await?;
    let [a, b, c, d] = &devices[..] else {
        return Err("devices".into());
    };
    introduce(b, c).await;
    // The creator accepts contacts of devices it enrolled, and no others.
    assert!(a.add_peer(&b.contact().await).await.is_ok());
    let foreign = Device::create(&root.path().join("x"), Some(memory(&hub, "x")))?;
    for device in [a, b] {
        let error = device.add_peer(&foreign.contact().await).await;
        assert_eq!(error.err().ok_or("another domain")?["code"], "rejected");
    }
    foreign.close().await;
    assert!(b.add_peer(&b.contact().await).await.is_err());
    a.close().await;
    assert!(!hub.listens("a"));

    b.add(1, "Milk").await;
    c.add(2, "Eggs").await;
    let synced = c.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(synced, json!({"state": "connected", "peers": 1}));
    assert_eq!(c.titles().await, ["Eggs", "Milk"]);
    assert_eq!(b.items().await, c.items().await);
    // The network reports each peer on its own; the database is unaffected.
    assert_eq!(c.peer(&b.node().await).await["state"], "connected");
    let creator = c.ok(json!({"type": "session"})).await;
    let creators: Vec<Value> = c.ok(json!({"type": "peers"})).await["peers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|peer| peer["issuer"] == json!(true))
        .cloned()
        .collect();
    assert_eq!(creators.len(), 1);
    assert_eq!(creators[0]["state"], "unreachable");
    assert_ne!(creators[0]["node"], creator["node"]);
    let network = c.ok(json!({"type": "network"})).await;
    assert_eq!(network["attached"], json!(true));
    assert_eq!(network["kind"], "memory");
    assert_eq!(network["inbound"]["limit"], 32);

    // An enrolled device nobody configured is refused, with valid credentials.
    d.add_peer(&b.contact().await)
        .await
        .map_err(|e| e.to_string())?;
    d.add(3, "Jam").await;
    assert!(d.sync().await.is_err());
    assert_eq!(d.peer(&b.node().await).await["state"], "unreachable");
    assert_eq!(b.titles().await, ["Eggs", "Milk"]);
    assert_eq!(d.titles().await, ["Jam"]);
    // A removed peer is neither dialed nor served.
    b.ok(json!({"type": "peer_remove", "node": c.node().await}))
        .await;
    c.add(4, "Tea").await;
    assert!(c.sync().await.is_err());
    assert_eq!(b.titles().await, ["Eggs", "Milk"]);
    // Any issuer renews any device: the last one can be removed, and the
    // member then reports that it has no issuer contact.
    let creator_node = creators[0]["node"].clone();
    assert_eq!(b.ok(json!({"type": "peers"})).await["issuer_contact"], true);
    b.ok(json!({"type": "peer_remove", "node": creator_node}))
        .await;
    assert_eq!(
        b.ok(json!({"type": "peers"})).await["issuer_contact"],
        false
    );
    assert_eq!(
        b.ok(json!({"type": "status"})).await["authority"]["issuer_contact"],
        false
    );
    for device in [b, c, d] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn edits_relayed_in_any_order_and_repeated_converge() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let devices = enroll(root.path(), &hub, &["b", "c", "d"]).await?;
    let [a, b, c, d] = &devices[..] else {
        return Err("devices".into());
    };
    introduce(b, c).await;
    introduce(c, d).await;
    introduce(b, d).await;
    let milk = a.add(1, "Milk").await;
    for device in [b, c, d] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    a.close().await;
    // Concurrent edits of one row, a delete and new rows, known to one device each.
    b.set(2, &milk, "title", json!("Oat milk")).await;
    c.set(3, &milk, "completed", json!(true)).await;
    d.add(4, "Eggs").await;
    let eggs = c.add(5, "Jam").await;
    c.ok(json!({
        "type": "mutate",
        "handle": vec![6u8; 32],
        "payload": json!({"type": "delete", "collection": "items"}).to_string().into_bytes(),
        "entity": {"id": eggs},
        "baseline": null,
    }))
    .await;
    // The later edit reaches a device before the earlier one, and again.
    for order in [[d, b, c], [c, d, b], [b, b, d], [c, c, c], [d, b, c]] {
        for device in order {
            device.sync().await.map_err(|error| error.to_string())?;
        }
    }
    assert_eq!(b.items().await, ["Eggs/0", "Oat milk/1"]);
    assert_eq!(c.items().await, b.items().await);
    assert_eq!(d.items().await, b.items().await);
    for device in [b, c, d] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_revoked_peer_stays_denied_without_the_creator() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let devices = enroll(root.path(), &hub, &["b", "c"]).await?;
    let [a, b, c] = &devices[..] else {
        return Err("devices".into());
    };
    introduce(b, c).await;
    a.add_peer(&c.contact().await)
        .await
        .map_err(|e| e.to_string())?;
    a.ok(json!({"type": "revoke", "node": c.node().await}))
        .await;
    // Revoking forgets the device's contact and refuses it again.
    assert_eq!(a.peer(&c.node().await).await, Value::Null);
    assert!(a.add_peer(&c.contact().await).await.is_err());
    // The other member learns the new policy epoch from the creator.
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(b.ok(json!({"type": "session"})).await["epoch"], "2");
    a.close().await;

    b.add(1, "Fresh").await;
    c.add(2, "Late").await;
    assert!(c.sync().await.is_err());
    // Its still-configured peer neither takes its rows nor gives it any.
    assert!(b.sync().await.is_err());
    assert_eq!(b.titles().await, ["Fresh"]);
    assert_eq!(c.titles().await, ["Late"]);
    b.close().await;
    c.close().await;
    Ok(())
}

#[tokio::test]
async fn a_domain_survives_a_change_of_routes_and_transport_kind() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let devices = enroll(root.path(), &hub, &["b", "c"]).await?;
    let [a, b, _] = &devices[..] else {
        return Err("devices".into());
    };
    a.add(1, "Milk").await;
    b.sync().await.map_err(|error| error.to_string())?;
    let identity = async |device: &Device| {
        let session = device.ok(json!({"type": "session"})).await;
        (session["node"].clone(), session["principal"].clone())
    };
    let (before_a, before_b) = (identity(a).await, identity(b).await);
    let scope = a.database.scope();
    for device in &devices {
        device.close().await;
    }

    // Both devices come back on another transport kind.
    let a = Device::open(&root.path().join("a"), Some(tcp()))?;
    let b = Device::open(&root.path().join("b"), Some(tcp()))?;
    assert_eq!(a.database.scope(), scope);
    assert_eq!(
        (identity(&a).await, identity(&b).await),
        (before_a, before_b)
    );
    assert_eq!(a.ok(json!({"type": "domain"})).await["transport"], "tcp");
    assert_eq!(b.titles().await, ["Milk"]);
    b.add(2, "Eggs").await;
    // The member only knows the creator's old route.
    let error = b.sync().await.err().ok_or("no route yet")?;
    assert_eq!(error["code"], "rejected", "{error}");
    assert!(error["message"].as_str().is_some_and(|m| m.contains("tcp")));
    // A fresh contact adds the route; the old one is kept for its kind.
    b.add_peer(&a.contact().await)
        .await
        .map_err(|e| e.to_string())?;
    let routes = b.peer(&a.node().await).await["routes"].clone();
    assert_eq!(routes.as_array().map(Vec::len), Some(2));
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);

    // An address is a hint, not a grant: another enrolled device behind
    // the creator's route is not taken for the creator.
    let c = Device::open(&root.path().join("c"), Some(tcp()))?;
    let c_contact: Value = decode(&c.contact().await, "rocky://peer/")?;
    let mut forged: Value = decode(&a.contact().await, "rocky://peer/")?;
    forged["routes"] = c_contact["routes"].clone();
    b.add_peer(&encode(&forged, "rocky://peer/"))
        .await
        .map_err(|e| e.to_string())?;
    c.add_peer(&b.contact().await)
        .await
        .map_err(|e| e.to_string())?;
    b.add(3, "Jam").await;
    assert!(b.sync().await.is_err());
    assert!(c.titles().await.is_empty());
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    for device in [&a, &b, &c] {
        device.close().await;
    }
    Ok(())
}

fn decode(uri: &str, prefix: &str) -> Result<Value, Failure> {
    let encoded = uri.strip_prefix(prefix).ok_or("prefix")?;
    Ok(serde_json::from_slice(
        &BASE64URL_NOPAD.decode(encoded.as_bytes())?,
    )?)
}
fn encode(value: &Value, prefix: &str) -> String {
    format!(
        "{prefix}{}",
        BASE64URL_NOPAD.encode(value.to_string().as_bytes())
    )
}
#[tokio::test]
async fn other_identity_and_invitation_versions_are_refused_without_writes() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(&root.path().join("a"), Some(memory(&hub, "issuer")))?;
    let invite = decode(&a.invite().await, "rocky://join/")?;
    assert_eq!(invite["v"], 4);
    for version in [0, 1, 2, 3, 5, u32::MAX] {
        let mut other = invite.clone();
        other["v"] = json!(version);
        let directory = root.path().join(format!("join-{version}"));
        assert!(
            Device::join(
                &directory,
                &encode(&other, "rocky://join/"),
                memory(&hub, "join")
            )
            .is_err()
        );
        assert!(!directory.exists());
    }
    a.add(1, "retained").await;
    a.close().await;
    let directory = root.path().join("a");
    let path = directory.join("identity.json");
    let original = std::fs::read(&path)?;
    let database = std::fs::read(directory.join("rocky.sqlite"))?;
    fn files(
        directory: &Path,
    ) -> Result<std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>, Failure> {
        let mut snapshot = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                snapshot.extend(files(&path)?);
            } else {
                snapshot.insert(path.clone(), std::fs::read(path)?);
            }
        }
        Ok(snapshot)
    }
    let mut errors = Vec::new();
    for version in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, u32::MAX] {
        let mut identity: Value = serde_json::from_slice(&original)?;
        identity["version"] = json!(version);
        let bytes = serde_json::to_vec(&identity)?;
        std::fs::write(&path, &bytes)?;
        let before = files(&directory)?;
        let opened = Device::open(&directory, None);
        let error = match opened {
            Err(error) => error.to_string(),
            Ok(device) => {
                device.close().await;
                return Err("unsupported identity opened".into());
            }
        };
        errors.push(error);
        let after = files(&directory)?;
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>()
        );
        for (path, bytes) in &before {
            assert!(
                after.get(path) == Some(bytes),
                "file changed: {}",
                path.display()
            );
        }
        assert_eq!(std::fs::read(&path)?, bytes);
        assert_eq!(std::fs::read(directory.join("rocky.sqlite"))?, database);
    }
    assert!(errors.iter().all(|error| error == &errors[0]));
    std::fs::write(&path, original)?;
    let a = Device::open(&directory, None)?;
    assert_eq!(a.titles().await, ["retained"]);
    a.close().await;
    Ok(())
}

#[tokio::test]
async fn a_started_connector_is_a_borrowed_network() -> Result<(), Failure> {
    // An application keeps one transport across two databases' lifetimes.
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let transport = hub.connector("a").start(true, [0; 32]).await?;
    for round in 0..2 {
        let a = if round == 0 {
            Device::create(&root.path().join("a"), None)?
        } else {
            Device::open(&root.path().join("a"), None)?
        };
        a.database
            .attach(Network::Borrowed(transport.clone()))
            .await?;
        assert_eq!(
            a.ok(json!({"type": "network"})).await["owned"],
            json!(false)
        );
        a.close().await;
        assert!(hub.listens("a"));
    }
    transport.close().await;
    Ok(())
}
