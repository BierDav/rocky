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
        .filter(|peer| peer["creator"] == json!(true))
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
    let creator_node = creators[0]["node"].clone();
    assert!(
        b.call(json!({"type": "peer_remove", "node": creator_node}))
            .await
            .is_err()
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
/// Rewrites a current identity file as version 4 wrote it, or as version
/// 3 did when `transport` names the kind it was bound to.
fn downgrade(directory: &Path, transport: Option<&str>) -> Result<(), Failure> {
    let path = directory.join("identity.json");
    let mut identity: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(identity["version"], 8);
    let object = identity.as_object_mut().ok_or("identity")?;
    object.remove("minting");
    let authority = object.remove("authority").ok_or("authority")?;
    let issuers = object.remove("issuers").ok_or("issuers")?;
    object.remove("profile");
    object.remove("keys");
    let role = if authority.is_null() {
        json!({"role": "member", "creator": issuers[0]})
    } else {
        let mut devices = authority["devices"].clone();
        for device in devices.as_object_mut().ok_or("devices")?.values_mut() {
            device.as_object_mut().ok_or("device")?.remove("profile");
        }
        json!({"role": "creator", "invitation": null, "devices": devices})
    };
    object.insert("role".into(), role);
    object.insert("version".into(), json!(4));
    if let Some(transport) = transport {
        let peers = object.remove("peers").ok_or("peers")?;
        if object["role"]["role"] == "member" {
            let node = object["role"]["creator"].clone();
            let address = peers[node.as_str().ok_or("creator")?]["routes"][0]["address"].clone();
            object["role"]["creator"] = json!({"node": node, "address": address});
        }
        object.insert("transport".into(), json!(transport));
        object.insert("version".into(), json!(3));
    }
    std::fs::write(path, identity.to_string())?;
    Ok(())
}

#[tokio::test]
async fn earlier_records_migrate_and_unknown_versions_are_refused() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let devices = enroll(root.path(), &hub, &["b", "c"]).await?;
    let [a, b, c] = &devices[..] else {
        return Err("devices".into());
    };
    a.add(1, "Milk").await;
    // Invitations from before grant profiles granted every right. They and
    // unknown versions enroll nobody.
    let invite = decode(&a.invite().await, "rocky://join/")?;
    assert_eq!(invite["v"], 3);
    assert_eq!(
        invite["profile"],
        json!({"kind": "writer", "scopes": ["items"]})
    );
    let first = json!({
        "v": 1,
        "transport": invite["routes"][0]["transport"],
        "address": invite["routes"][0]["address"],
        "token": invite["token"],
        "publisher": invite["publisher"],
        "expires": invite["expires"],
    });
    let mut second = invite.clone();
    second["v"] = json!(2);
    second.as_object_mut().ok_or("invite")?.remove("profile");
    let mut newer = invite.clone();
    newer["v"] = json!(4);
    for (name, refused) in [("d", first), ("e", second), ("f", newer)] {
        let joined = Device::join(
            &root.path().join(name),
            &encode(&refused, "rocky://join/"),
            memory(&hub, name),
        );
        assert!(joined.is_err());
    }
    for device in [a, b, c] {
        device.close().await;
    }

    // A member from before version 5 that never served a peer has no
    // route key yet. The migration makes it, once.
    let secrets = |name: &str| -> Result<Vec<String>, Failure> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(root.path().join(name).join("secrets"))? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    };
    let route_key = |name: &str| -> Result<std::path::PathBuf, Failure> {
        let file = secrets(name)?
            .into_iter()
            .find(|file| file.ends_with(".transport"))
            .ok_or("route key")?;
        Ok(root.path().join(name).join("secrets").join(file))
    };
    std::fs::remove_file(route_key("b")?)?;
    downgrade(&root.path().join("a"), Some("memory"))?;
    downgrade(&root.path().join("b"), Some("memory"))?;
    downgrade(&root.path().join("c"), None)?;
    // Versions 3 and 4 open without a network and are rewritten once.
    for name in ["b", "c"] {
        let device = Device::open(&root.path().join(name), None)?;
        let session = device.ok(json!({"type": "session"})).await;
        assert_eq!(
            session["profile"],
            json!({"kind": "writer", "scopes": ["items"]})
        );
        assert_eq!(session["issuer"], json!(false));
        device.close().await;
        let saved: Value = serde_json::from_slice(&std::fs::read(
            root.path().join(name).join("identity.json"),
        )?)?;
        assert_eq!(saved["version"], 8);
        assert_eq!(saved["keys"], json!({"items": [1]}));
        assert!(saved.get("transport").is_none() && saved.get("role").is_none());
    }
    let a = Device::open(&root.path().join("a"), Some(memory(&hub, "a")))?;
    let b = Device::open(&root.path().join("b"), Some(memory(&hub, "b")))?;
    let domain = a.ok(json!({"type": "domain"})).await;
    assert_eq!(domain["profile"], json!({"kind": "issuer"}));
    let devices = a.ok(json!({"type": "devices"})).await;
    assert_eq!(devices["devices"].as_array().map(Vec::len), Some(2));
    let issuer = b.peer(&a.node().await).await;
    assert_eq!(issuer["issuer"], json!(true));
    assert_eq!(
        issuer["routes"],
        json!([{"transport": "memory", "address": "a"}])
    );
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(b.titles().await, ["Milk"]);
    // The migrated member renews with the migrated issuer.
    b.ok(json!({"type": "renew"})).await;
    a.close().await;
    b.close().await;

    // After the migration a lost key is an error: none is made in its place.
    let before = secrets("b")?;
    std::fs::remove_file(route_key("b")?)?;
    assert!(Device::open(&root.path().join("b"), Some(memory(&hub, "b"))).is_err());
    assert_eq!(secrets("b")?.len(), before.len() - 1);
    let reader = before
        .iter()
        .find(|file| file.ends_with(".reader.items"))
        .ok_or("reader key")?;
    std::fs::remove_file(root.path().join("b/secrets").join(reader))?;
    assert!(Device::open(&root.path().join("b"), None).is_err());
    assert_eq!(secrets("b")?.len(), before.len() - 2);

    // Other versions are refused and left as they are.
    for version in [2, 9] {
        let path = root.path().join("c/identity.json");
        let mut identity: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        identity["version"] = json!(version);
        std::fs::write(&path, identity.to_string())?;
        assert!(Device::open(&root.path().join("c"), None).is_err());
        let kept: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(kept["version"], version);
    }
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
