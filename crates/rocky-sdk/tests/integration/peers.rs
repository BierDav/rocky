use crate::support::snapshot as snap;
use common::{Device, Failure, introduce};
use data_encoding::BASE64URL_NOPAD;
use rocky_sdk::{
    Network,
    transport::{Connector, memory::Hub},
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

use crate::common;

fn memory(hub: &Arc<Hub>, name: &str) -> Network {
    Network::Owned(hub.connector(name))
}
fn iroh() -> Network {
    Network::Owned(Arc::new(rocky_iroh::Options::isolated("127.0.0.1:0")))
}
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
    let devices = enroll(root.path(), &hub, &["b", "c"]).await?;
    let [a, b, c] = &devices[..] else {
        return Err("devices".into());
    };
    introduce(b, c).await;
    assert!(
        a.add_peer(&b.contact().await).await.is_ok(),
        "the creator accepts contacts of devices it enrolled"
    );
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
    assert!(common::reconciled(&synced));
    assert_eq!(c.titles().await, ["Eggs", "Milk"]);
    assert_eq!(b.items().await, c.items().await);
    assert!(
        c.peer(&b.node().await).await["attempts"]
            .as_array()
            .expect("attempts")
            .iter()
            .any(|attempt| attempt["outcome"] == "completed"),
        "the network reports each peer on its own"
    );
    let creator = snap::ok(c.session.info()).await;
    let creators: Vec<Value> = snap::ok(async {
        let network = c.session.network().await?;
        Ok(json!({"peers":network.peers,"discovery":network.discovery}))
    })
    .await["peers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|peer| peer["is_issuer"] == json!(true))
        .cloned()
        .collect();
    assert_eq!(creators.len(), 1);
    assert!(
        creators[0]["attempts"]
            .as_array()
            .expect("attempts")
            .iter()
            .any(|attempt| attempt["outcome"] == "failed")
    );
    assert_ne!(creators[0]["node"], creator["node"]);
    let network = snap::ok(c.session.network()).await;
    assert_eq!(network["attachment"], "attached");
    assert_eq!(network["kind"], "memory");
    assert_eq!(network["inbound"]["limit"], 32);

    assert_eq!(c.peer(&b.node().await).await["is_configured"], true);
    snap::ok(b.session.remove_peer(snap::public_key(&(c.node().await)))).await;
    b.add(4, "Tea").await;
    assert!(
        common::exchange_failed(&b.sync().await),
        "a removed contact is no longer dialed"
    );
    assert_eq!(c.titles().await, ["Eggs", "Milk"]);
    c.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        c.titles().await,
        ["Eggs", "Milk", "Tea"],
        "a removed contact is still served"
    );
    let creator_node = creators[0]["node"].clone();
    assert!(
        !b.session
            .status()
            .await?
            .authority
            .renewal_routes
            .is_empty()
    );
    snap::ok(b.session.remove_peer(snap::public_key(&(creator_node)))).await;
    assert!(
        b.session
            .status()
            .await?
            .authority
            .renewal_routes
            .is_empty(),
        "the last issuer contact can be removed, leaving no known renewal candidates"
    );
    assert!(
        snap::ok(b.session.status()).await["authority"]["renewal_routes"]
            .as_array()
            .expect("routes")
            .is_empty()
    );
    for device in [b, c] {
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
    b.set(2, &milk, "title", json!("Oat milk")).await;
    c.set(3, &milk, "completed", json!(true)).await;
    d.add(4, "Eggs").await;
    let eggs = c.add(5, "Jam").await;
    snap::ok(snap::mutate(
        &c.database,
        &c.session,
        vec![6u8; 32],
        snap::Action::Delete {
            table: "items".to_owned(),
        },
        Some(snap::text(&(eggs))),
        None,
    ))
    .await;
    let later_edits_arrive_before_earlier_ones =
        [[d, b, c], [c, d, b], [b, b, d], [c, c, c], [d, b, c]];
    for order in later_edits_arrive_before_earlier_ones {
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
    snap::ok(
        a.session
            .revoke_identity(snap::public_key(&(c.identity().await))),
    )
    .await;
    assert_eq!(
        a.peer(&c.node().await).await,
        Value::Null,
        "revoking forgets the device's contact"
    );
    assert!(
        a.add_peer(&c.contact().await).await.is_err(),
        "a revoked device's contact is refused again"
    );
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        snap::ok(b.session.info()).await["revocations"]["statements"],
        1,
        "the other member learns the revocation from the creator"
    );
    a.close().await;

    b.add(1, "Fresh").await;
    c.add(2, "Late").await;
    assert!(common::exchange_failed(&c.sync().await));
    assert!(
        common::exchange_failed(&b.sync().await),
        "a still-configured peer neither takes a revoked device's rows nor gives it any"
    );
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
        let session = snap::ok(device.session.info()).await;
        (session["node"].clone(), session["principal"].clone())
    };
    let (before_a, before_b) = (identity(a).await, identity(b).await);
    let scope = a.database.scope();
    for device in &devices {
        device.close().await;
    }

    let a = Device::open(&root.path().join("a"), Some(iroh()))?;
    let b = Device::open(&root.path().join("b"), Some(iroh()))?;
    assert_eq!(a.database.scope(), scope);
    assert_eq!(
        (identity(&a).await, identity(&b).await),
        (before_a, before_b)
    );
    assert_eq!(snap::ok(a.session.domain()).await["transport"], "iroh");
    assert_eq!(b.titles().await, ["Milk"]);
    b.add(2, "Eggs").await;
    let error = b
        .sync()
        .await
        .err()
        .ok_or("the member only knows the creator's old route")?;
    assert_eq!(error["code"], "rejected", "{error}");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|m| m.contains("iroh"))
    );
    b.add_peer(&a.contact().await)
        .await
        .map_err(|e| e.to_string())?;
    let routes = b.peer(&a.node().await).await["routes"].clone();
    assert_eq!(
        routes.as_array().map(Vec::len),
        Some(2),
        "a fresh contact adds the route and the old one is kept for its kind"
    );
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);

    let c = Device::open(&root.path().join("c"), Some(iroh()))?;
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
    assert!(common::exchange_failed(&b.sync().await));
    assert!(
        c.titles().await.is_empty(),
        "an address is a hint, not a grant: another device behind the creator's route is not the creator"
    );
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    for device in [&a, &b, &c] {
        device.close().await;
    }
    Ok(())
}

fn is_database_file(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("rocky.sqlite"))
}

fn identity_without_signed_authority(bytes: &[u8]) -> Result<Vec<u8>, Failure> {
    let mut settings: Value = serde_json::from_slice(bytes)?;
    let chain: rocky_sdk::credentials::CredentialChain =
        serde_json::from_value(settings["credentials"].clone())?;
    let database =
        chain.database_context(serde_json::from_value(settings["publisher"].clone())?)?;
    let authority: rocky_sdk::credentials::RevocationSet =
        serde_json::from_value(settings["revocations"].clone())?;
    authority.verify_in(database)?;
    settings["revocations"]
        .as_object_mut()
        .ok_or("authority set")?
        .remove("certificates");
    settings
        .as_object_mut()
        .ok_or("settings object")?
        .remove("credentials");
    Ok(serde_json::to_vec(&settings)?)
}

fn discovery_without_endorsements(bytes: &[u8]) -> Result<Vec<u8>, Failure> {
    let mut discovery: Value = serde_json::from_slice(bytes)?;
    if let Some(root) = discovery["root"].as_object_mut() {
        root.remove("endorsement");
    }
    for value in discovery["values"]
        .as_object_mut()
        .ok_or("discovery values")?
        .values_mut()
    {
        value
            .as_object_mut()
            .ok_or("discovery value")?
            .remove("endorsement");
    }
    Ok(serde_json::to_vec(&discovery)?)
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
    let invite = decode(&a.invite().await, "rocky://provision/")?;
    assert_eq!(invite["v"], 2);
    for version in [0, 1, 3, u32::MAX] {
        let mut other = invite.clone();
        other["v"] = json!(version);
        let directory = root.path().join(format!("join-{version}"));
        assert!(
            Device::join(
                &directory,
                &encode(&other, "rocky://provision/"),
                memory(&hub, "join")
            )
            .is_err()
        );
        assert!(!directory.exists());
    }
    a.add(1, "retained").await;
    a.close().await;
    let directory = root.path().join("a");
    let original = common::authority_bytes(&directory, "identity")?.ok_or("identity")?;
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
    assert_eq!(serde_json::from_slice::<Value>(&original)?["version"], 40);
    for version in (0..40).chain([41, u32::MAX]) {
        let mut identity: Value = serde_json::from_slice(&original)?;
        identity["version"] = json!(version);
        let bytes = serde_json::to_vec(&identity)?;
        common::write_authority(&directory, "identity", &bytes).await?;
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
        assert_eq!(
            common::authority_bytes(&directory, "identity")?,
            Some(bytes)
        );
    }
    assert!(errors.iter().all(|error| error == &errors[0]));
    std::fs::write(directory.join("rocky.sqlite"), database)?;
    let a = Device::open(&directory, None)?;
    assert_eq!(a.titles().await, ["retained"]);
    a.close().await;
    Ok(())
}

#[tokio::test]
async fn a_started_connector_is_a_borrowed_network() -> Result<(), Failure> {
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
            .attach(Network::Attached(transport.clone()))
            .await?;
        assert_eq!(
            snap::ok(a.session.network()).await["ownership"],
            json!("application")
        );
        a.close().await;
        assert!(hub.listens("a"));
    }
    transport.close().await;
    Ok(())
}

#[tokio::test]
async fn reopened_member_resolves_saved_routes_without_foreign_node_keys_outside_signed_authority()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(&root.path().join("issuer"), Some(memory(&hub, "issuer")))?;
    let member_path = root.path().join("member");
    let member = Device::join(&member_path, &issuer.invite().await, memory(&hub, "member"))?;
    member.sync().await.map_err(|error| error.to_string())?;
    let foreign: rocky_secure::PublicKey = serde_json::from_value(json!(issuer.node().await))?;
    member.close().await;
    issuer.add(1, "After restart").await;
    let reopened = Device::open(&member_path, Some(memory(&hub, "reopened")))?;
    reopened.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(reopened.titles().await, ["After restart"]);
    reopened.close().await;
    issuer.close().await;
    let mut directories = vec![member_path];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                directories.push(path);
                continue;
            }
            if is_database_file(&path) {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            let bytes = if path.file_name().is_some_and(|name| name == "identity.json") {
                identity_without_signed_authority(&bytes)?
            } else if path
                .extension()
                .is_some_and(|extension| extension == "discovery")
            {
                discovery_without_endorsements(&bytes)?
            } else {
                bytes
            };
            for needle in [foreign.as_bytes().as_slice(), foreign.to_hex().as_bytes()] {
                assert!(
                    !bytes.windows(needle.len()).any(|part| part == needle),
                    "foreign node key in {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}
