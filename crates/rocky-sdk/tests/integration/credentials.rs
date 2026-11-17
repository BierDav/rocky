//! Grant profiles, issuer devices, revocation with key rotation and
//! credentials supplied from outside the SDK.
use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{
    Database, Network, Open, Supplied,
    credentials::{
        Authority, CredentialChain, Credentials, PrivateKey, Revocation, SignedManifest,
    },
    transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

use crate::common;

const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"text","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

fn memory(hub: &Arc<Hub>, name: &str) -> Network {
    Network::Owned(hub.connector(name))
}
fn open<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        identity: common::prepared_identity(directory),
        directory,
        schema: SCHEMA,
        secrets: None,
        network: Some(memory(hub, name)),
        coordination: None,
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
fn reopen(root: &Path, hub: &Arc<Hub>, name: &str) -> Result<Device, Failure> {
    Device::new(Database::open(open(&root.join(name), hub, name))?)
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(root.join(name).join("identity.json"))?)?;
    for epochs in identity["keys"].as_object_mut().ok_or("keys")?.values_mut() {
        for epoch in epochs.as_array_mut().ok_or("epochs")? {
            *epoch = epoch["generation"].clone();
        }
    }
    Ok(identity)
}
/// The secret of `role` in a device's file key store.
fn secret(root: &Path, name: &str, role: &str) -> Result<Option<[u8; 32]>, Failure> {
    let state: Value =
        serde_json::from_slice(&std::fs::read(root.join(name).join("identity.json"))?)?;
    let role = if let Some(reader) = role.strip_prefix("reader.") {
        let (collection, generation) = reader
            .split_once('.')
            .map_or((reader, 1u64), |(name, value)| {
                (name, value.parse().unwrap())
            });
        let epoch = state["keys"][collection].as_array().and_then(|epochs| {
            epochs
                .iter()
                .find(|epoch| epoch["generation"] == generation)
        });
        match epoch {
            Some(epoch) => {
                let epoch: rocky_core::KeyEpoch = serde_json::from_value(epoch.clone())?;
                format!(
                    "reader.{collection}.{}.{}",
                    epoch.generation,
                    data_encoding::HEXLOWER.encode(&epoch.secret_hash)
                )
            }
            None => return Ok(None),
        }
    } else {
        role.to_owned()
    };
    let installation = state["installation"]
        .as_str()
        .ok_or("installation")?
        .to_owned();
    let path = root
        .join(name)
        .join("secrets")
        .join(format!("rocky.{installation}.{role}"));
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes.try_into().map_err(|_| "secret length")?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn secrets(root: &Path, name: &str) -> Result<Vec<String>, Failure> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root.join(name).join("secrets"))? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}
async fn create_row(
    device: &Device,
    handle: u8,
    collection: &str,
    values: Value,
) -> Result<Value, Failure> {
    snap::observe(snap::mutate(
        &device.session,
        vec![handle; 32],
        snap::Action::Create {
            table: snap::text(&(collection)),
            values: snap::fields(values),
        },
        None,
        None,
    ))
    .await
    .map_err(|error| error.to_string().into())
}
async fn note(device: &Device, handle: u8, text: &str) -> Result<Value, Failure> {
    create_row(device, handle, "notes", json!({"text": text})).await
}
async fn item(device: &Device, handle: u8, title: &str) -> Result<Value, Failure> {
    create_row(
        device,
        handle,
        "items",
        json!({"title": title, "completed": false}),
    )
    .await
}
/// The first column of every row `sql` selects, as text.
async fn texts(device: &Device, sql: &str) -> Result<Vec<String>, Failure> {
    let page = snap::observe(device.session.read(rocky_sdk::ReadQuery {
        sql: snap::text(&(sql)),
        values: vec![],
        max_rows: 64,
        max_bytes: None,
        cursor: None,
    }))
    .await
    .map_err(|error| error.to_string())?;
    Ok(page["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row[0]["value"].as_str().map(String::from))
        .collect())
}
async fn notes(device: &Device) -> Result<Vec<String>, Failure> {
    texts(device, "SELECT text FROM notes ORDER BY text").await
}
async fn items(device: &Device) -> Result<Vec<String>, Failure> {
    texts(device, "SELECT title FROM items ORDER BY title").await
}
async fn session(device: &Device) -> Value {
    snap::ok(device.session.info()).await
}
/// Stops each device's automatic sync, so that only the test's explicit
/// syncs reach peers.
async fn manual(devices: &[&Device]) {
    for device in devices {
        snap::ok(device.session.set_automatic_sync(false)).await;
    }
}
async fn synced(device: &Device) -> Result<(), Failure> {
    device
        .sync()
        .await
        .map(drop)
        .map_err(|e| e.to_string().into())
}

#[tokio::test]
async fn profiles_bound_reads_and_writes_across_renewal() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let reader = json!({"kind": "reader", "scopes": ["items"]});
    let writer = json!({"kind": "writer", "scopes": ["notes"]});
    let r = join(root, &hub, "r", &a.invite_as(reader.clone()).await)?;
    let w = join(root, &hub, "w", &a.invite_as(writer.clone()).await)?;
    for profile in [
        json!({"kind": "reader", "scopes": []}),
        json!({"kind": "writer", "scopes": ["unknown"]}),
    ] {
        let refused = snap::observe(
            a.session.invite(
                Some(
                    serde_json::from_value::<rocky_sdk::GrantProfile>(json!(profile))
                        .expect("typed fixture"),
                ),
                None,
            ),
        )
        .await;
        assert!(
            refused.is_err(),
            "a profile naming nothing or an unknown collection invites nobody: {profile}"
        );
    }
    item(&a, 1, "Milk").await?;
    note(&a, 2, "Hello").await?;
    synced(&r).await?;
    synced(&w).await?;

    let check = async |renewed: bool| -> Result<(), Failure> {
        assert_eq!(session(&r).await["profile"], reader, "renewed {renewed}");
        assert_eq!(session(&w).await["profile"], writer, "renewed {renewed}");
        assert_eq!(identity(root, "r")?["profile"], reader);
        assert_eq!(identity(root, "r")?["keys"], json!({"items": [1]}));
        assert_eq!(identity(root, "w")?["keys"], json!({"notes": [1]}));
        assert_eq!(items(&r).await?, ["Milk"], "a reader reads its collection");
        assert!(
            item(&r, 10, "Forged").await.is_err(),
            "a reader writes nothing"
        );
        assert!(
            note(&r, 11, "Forged").await.is_err(),
            "a reader writes nothing"
        );
        assert!(
            snap::observe(
                r.session.invite(
                    Some(
                        serde_json::from_value::<rocky_sdk::GrantProfile>(json!(reader))
                            .expect("typed fixture")
                    ),
                    None
                )
            )
            .await
            .is_err()
        );
        assert!(notes(&r).await.unwrap_or_default().is_empty());
        assert_eq!(notes(&w).await?, ["Hello"]);
        assert!(
            item(&w, 12, "Forged").await.is_err(),
            "a scoped writer does not write another collection"
        );
        assert!(
            items(&w).await.unwrap_or_default().is_empty(),
            "a scoped writer does not read another collection"
        );
        assert!(snap::observe(w.session.identities()).await.is_err());
        Ok(())
    };
    check(false).await?;
    for device in [&r, &w] {
        let before = session(device).await;
        let renewed = snap::ok(async {
            device.session.renew().await?;
            device.session.info().await
        })
        .await;
        assert_eq!(renewed["readable"], before["readable"]);
        assert_eq!(renewed["writable"], before["writable"]);
        assert_eq!(renewed["principal"], before["principal"]);
    }
    check(true).await?;
    note(&w, 3, "From writer").await?;
    synced(&w).await?;
    assert_eq!(notes(&a).await?, ["From writer", "Hello"]);
    assert_eq!(items(&a).await?, ["Milk"]);
    let identities = snap::ok(a.session.identities()).await;
    let mut profiles: Vec<Value> = identities["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|record| record["own"] != true)
        .map(|record| record["profile"].clone())
        .collect();
    profiles.sort_by_key(Value::to_string);
    assert_eq!(
        profiles,
        [reader, writer],
        "the issuer lists each identity with the profile it enrolled as"
    );
    for device in [&a, &r, &w] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn an_issuer_device_enrolls_and_renews_while_the_creator_is_closed() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let i = join(
        root,
        &hub,
        "i",
        &a.invite_as(json!({"kind": "issuer"})).await,
    )?;
    manual(&[&a, &i]).await;
    item(&a, 1, "Milk").await?;
    synced(&i).await?;
    let (creator, issuer) = (session(&a).await, session(&i).await);
    assert_eq!(issuer["issuer"], json!(true));
    assert_eq!(issuer["profile"], json!({"kind": "issuer"}));
    assert_ne!(issuer["node"], creator["node"]);
    assert_ne!(
        issuer["principal"], creator["principal"],
        "each device signs with its own identity through its chain"
    );
    let (owner, paired) = (identity(root, "a")?, identity(root, "i")?);
    assert_eq!(
        paired["publisher"], owner["publisher"],
        "the root is the publisher of every chain"
    );
    assert_ne!(owner["identity"], owner["publisher"]);
    assert_ne!(paired["identity"], paired["publisher"]);
    assert_eq!(
        secret(root, "a", "publisher")?,
        None,
        "no device stores the root"
    );
    assert_eq!(secret(root, "i", "identity")?, None);
    assert_eq!(secret(root, "a", "identity")?, None);
    for name in ["publisher", "user"] {
        assert_eq!(secret(root, "i", name)?, None);
    }
    let scope = a.database.scope();
    a.close().await;

    let m = join(root, &hub, "m", &i.invite().await)?;
    manual(&[&m]).await;
    assert_eq!(m.database.scope(), scope);
    synced(&m).await?;
    assert_eq!(m.titles().await, ["Milk"]);
    m.add(2, "Eggs").await;
    synced(&m).await?;
    snap::ok(async {
        m.session.renew().await?;
        m.session.info().await
    })
    .await;
    assert_eq!(i.titles().await, ["Eggs", "Milk"]);
    let identities = snap::ok(i.session.identities()).await.to_string();
    assert!(identities.contains(m.identity().await.as_str()));
    for device in [&i, &m] {
        device.close().await;
    }
    let a = reopen(root, &hub, "a")?;
    let reopened = session(&a).await;
    assert_eq!(
        (&reopened["node"], &reopened["principal"]),
        (&creator["node"], &creator["principal"])
    );
    assert_eq!(a.database.scope(), scope);
    a.close().await;
    Ok(())
}

#[tokio::test]
async fn revocation_rotates_the_keys_remaining_readers_receive() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let b = join(root, &hub, "b", &a.invite().await)?;
    let c = join(
        root,
        &hub,
        "c",
        &a.invite_as(json!({"kind": "reader", "scopes": ["items"]}))
            .await,
    )?;
    let w = join(
        root,
        &hub,
        "w",
        &a.invite_as(json!({"kind": "writer", "scopes": ["notes"]}))
            .await,
    )?;
    item(&a, 1, "Before").await?;
    for device in [&b, &c] {
        synced(device).await?;
        assert_eq!(items(device).await?, ["Before"]);
    }
    let revoked = snap::ok(
        a.session
            .revoke_identity(&(snap::text(&(c.identity().await)))),
    )
    .await;
    assert_eq!(revoked["revocations"]["statements"], 1);
    assert_eq!(
        identity(root, "a")?["keys"],
        json!({"issuer_registry": [1], "items": [1, 2], "notes": [1]}),
        "only the collections the revoked reader could read get a new epoch"
    );
    assert!(identity(root, "a")?["authority"]["rotation"].is_null());
    item(&a, 2, "After").await?;

    synced(&b).await?;
    assert_eq!(session(&b).await["revocations"]["statements"], 1);
    assert_eq!(identity(root, "b")?["keys"], json!({"items": [1, 2]}));
    assert_eq!(
        secret(root, "b", "reader.items.2")?,
        secret(root, "a", "reader.items.2")?
    );
    assert_eq!(items(&b).await?, ["After", "Before"]);
    item(&b, 3, "Rotated").await?;
    synced(&b).await?;
    assert_eq!(items(&a).await?, ["After", "Before", "Rotated"]);
    synced(&w).await?;
    assert_eq!(
        session(&w).await["revocations"]["statements"],
        1,
        "a writer of another collection learns the revocation"
    );
    assert_eq!(
        identity(root, "w")?["keys"],
        json!({"notes": [1]}),
        "a writer of another collection does not get the new key"
    );

    assert!(common::exchange_failed(&c.sync().await));
    assert!(
        snap::observe(async {
            c.session.renew().await?;
            c.session.info().await
        })
        .await
        .is_err()
    );
    assert!(!snap::ok(c.session.status()).await["authority"]["verified_revocation"].is_null());
    assert_eq!(
        secret(root, "c", "reader.items.2")?,
        None,
        "the revoked reader gets no new key"
    );
    assert!(secrets(root, "c")?.iter().all(|name| !name.ends_with(".2")));
    assert_eq!(identity(root, "c")?["keys"], json!({"items": [1]}));
    assert!(
        !items(&c)
            .await
            .unwrap_or_default()
            .contains(&"After".into()),
        "the revoked reader gets no new rows"
    );
    for device in [&a, &b, &c, &w] {
        device.close().await;
    }

    let reopened = reopen(root, &hub, "c")?;
    assert!(
        !snap::ok(reopened.session.status()).await["authority"]["verified_revocation"].is_null()
    );
    reopened.close().await;
    let notice = root.join("c/revoked.json");
    let original = std::fs::read(&notice)?;
    let mut tampered: Value = serde_json::from_slice(&original)?;
    assert_eq!(tampered["version"], 3);
    tampered["statement"]["statement"]["time"] = json!(999);
    std::fs::write(&notice, serde_json::to_vec(&tampered)?)?;
    let reopened = reopen(root, &hub, "c")?;
    assert!(
        snap::observe(reopened.session.status()).await.is_err(),
        "a local file cannot forge a publisher's revocation notice"
    );
    reopened.close().await;
    std::fs::write(&notice, original)?;

    let before = secrets(root, "b")?;
    let role = before
        .iter()
        .find(|name| name.contains(".reader.items.2."))
        .ok_or("epoch 2 key")?;
    std::fs::remove_file(root.join("b/secrets").join(role))?;
    assert!(
        reopen(root, &hub, "b").is_err(),
        "a lost key epoch is an error at open"
    );
    assert_eq!(
        secrets(root, "b")?.len(),
        before.len() - 1,
        "a lost key epoch is not made again"
    );
    Ok(())
}

#[tokio::test]
async fn an_interrupted_revocation_completes_once_at_the_next_open() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let b = join(root, &hub, "b", &a.invite().await)?;
    let c = join(root, &hub, "c", &a.invite().await)?;
    item(&a, 1, "Before").await?;
    synced(&b).await?;
    let (c_node, c_identity) = (c.node().await, c.identity().await);
    let identities = snap::ok(a.session.identities()).await;
    let record = identities["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|record| record["identity"] == c_identity)
        .cloned()
        .ok_or("c's record")?;
    for device in [&a, &b, &c] {
        device.close().await;
    }
    let path = root.join("a/identity.json");
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let manifest = SignedManifest::from_bytes(
        &data_encoding::HEXLOWER.decode(saved["genesis"].as_str().ok_or("genesis")?.as_bytes())?,
    )?;
    let statement = Revocation {
        app: manifest
            .verify(serde_json::from_value(saved["publisher"].clone())?)?
            .app,
        issuer: serde_json::from_value(saved["identity"].clone())?,
        time: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        nodes: Default::default(),
        identities: [serde_json::from_value(json!(c_identity))?].into(),
        capabilities: [rocky_core::Commitment::try_from(
            data_encoding::HEXLOWER
                .decode(
                    record["capability"]
                        .as_str()
                        .ok_or("capability")?
                        .as_bytes(),
                )?
                .as_slice(),
        )?]
        .into(),
        cut: Default::default(),
        mode: rocky_credentials::RevocationMode::Lost,
        recovery: None,
    };
    let statement_id = statement.id()?;
    saved["authority"]["rotation"] = json!({
        "identities": [c_identity],
        "nodes": [c_node],
        "statement": statement,
        "keys": {"items": 2},
        "groups": {},
        "statements": [statement_id],
    });
    std::fs::write(&path, saved.to_string())?;
    let installation = saved["installation"].as_str().ok_or("installation")?;
    let made_before_the_device_stopped = [7; 32];
    std::fs::write(
        root.join("a/secrets")
            .join(format!("rocky.{installation}.rotation.reader.items.2")),
        made_before_the_device_stopped,
    )?;

    for round in 0..2 {
        let a = reopen(root, &hub, "a")?;
        assert_eq!(
            session(&a).await["revocations"]["statements"],
            1,
            "round {round}"
        );
        let saved = identity(root, "a")?;
        assert!(saved["authority"]["rotation"].is_null());
        assert_eq!(
            saved["keys"],
            json!({"issuer_registry": [1], "items": [1, 2], "notes": [1]})
        );
        let listed = snap::ok(a.session.identities()).await;
        assert!(
            listed["identities"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|record| record["identity"] == c_identity && record["revoked"] == true),
            "{listed}"
        );
        let authority: rocky_sdk::credentials::RevocationSet =
            serde_json::from_value(saved["revocations"].clone())?;
        assert_eq!(
            authority
                .get(&statement_id)
                .ok_or("completed statement")?
                .statement,
            statement,
            "the statement signed at completion is the one the intent named"
        );
        assert_eq!(
            secret(root, "a", "reader.items.2")?,
            Some(made_before_the_device_stopped),
            "completion keeps the key made before the device stopped"
        );
        if round == 0 {
            item(&a, 2, "After").await?;
        }
        a.close().await;
    }
    let a = reopen(root, &hub, "a")?;
    let b = reopen(root, &hub, "b")?;
    synced(&b).await?;
    assert_eq!(session(&b).await["revocations"]["statements"], 1);
    assert_eq!(
        secret(root, "b", "reader.items.2")?,
        Some(made_before_the_device_stopped)
    );
    assert_eq!(items(&b).await?, ["After", "Before"]);
    let c = reopen(root, &hub, "c")?;
    assert!(common::exchange_failed(&c.sync().await));
    assert_eq!(secret(root, "c", "reader.items.2")?, None);
    for device in [&a, &b, &c] {
        device.close().await;
    }
    Ok(())
}

/// Credentials an issuer outside the SDK made for `user`, from the
/// creator's node under its certified issuer authority.
fn issued(
    root: &Path,
    user: rocky_sdk::credentials::PublicKey,
    grants_of: &str,
) -> Result<Credentials, Failure> {
    let creator = identity(root, "a")?;
    let node = PrivateKey::from_bytes(&secret(root, "a", "node")?.ok_or("node")?);
    let held: CredentialChain = serde_json::from_value(creator["credentials"].clone())?;
    let issuer = held.capabilities();
    let revocations: rocky_sdk::credentials::RevocationSet =
        serde_json::from_value(creator["revocations"].clone())?;
    let model: CredentialChain =
        serde_json::from_value(identity(root, grants_of)?["credentials"].clone())?;
    let grants_without_keys = model
        .0
        .iter()
        .rev()
        .find(|link| !link.capability.grants.is_empty())
        .ok_or("chain")?
        .capability
        .grants
        .clone();
    let authority = Authority::issuer(&node, &held)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let capability = authority.issue(
        user,
        grants_without_keys,
        rocky_credentials::Validity {
            issued_at: now,
            lifetime: std::time::Duration::from_secs(3600),
            clock_skew: std::time::Duration::from_secs(300),
        },
    )?;
    let mut chain = issuer;
    chain.0.push(capability);
    Ok(Credentials {
        publisher: serde_json::from_value(creator["publisher"].clone())?,
        revocations,
        capabilities: chain,
        certificate: None,
    })
}

#[tokio::test]
async fn supplied_credentials_install_without_an_enrollment() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let model = join(root, &hub, "model", &a.invite().await)?;
    model.close().await;
    item(&a, 1, "Milk").await?;
    note(&a, 2, "Private").await?;

    let directory = root.join("s");
    let keys = common::node_keys(&directory, None)?;
    assert_eq!(common::node_keys(&directory, None)?, keys);
    let creator = identity(root, "a")?;
    let ids = || -> Result<_, Failure> {
        Ok((
            serde_json::from_value(creator["mesh"].clone())?,
            serde_json::from_value(creator["domain"].clone())?,
        ))
    };
    let items_key = secret(root, "a", "reader.items")?.ok_or("items key")?;
    let notes_key = secret(root, "a", "reader.notes")?.ok_or("notes key")?;
    let contact = a.contact().await;
    let supplied = |user, keys: Vec<(String, u64, [u8; 32])>| -> Result<Supplied, Failure> {
        let (mesh, domain) = ids()?;
        Ok(Supplied {
            mesh,
            domain,
            manifest: SignedManifest::from_bytes(
                &data_encoding::HEXLOWER
                    .decode(creator["genesis"].as_str().ok_or("genesis")?.as_bytes())?,
            )?,
            credentials: issued(root, user, "model")?,
            keys: keys
                .into_iter()
                .map(|(name, generation, key)| {
                    (name, rocky_core::KeyEpoch::new(generation, &key), key)
                })
                .collect(),
            epoch_statements: std::collections::BTreeMap::new(),
            issuers: vec![contact.clone()],
        })
    };
    let items_only = vec![("items".to_owned(), 1, items_key)];
    let stranger = PrivateKey::new().public_key();
    for refused in [
        supplied(stranger, items_only.clone())?,
        supplied(
            keys.identity,
            vec![items_only[0].clone(), ("notes".into(), 1, notes_key)],
        )?,
        supplied(keys.identity, Vec::new())?,
    ] {
        assert!(
            Database::install(open(&directory, &hub, "s"), refused).is_err(),
            "a chain for another device, an unreadable key or a missing reader key is refused"
        );
        assert!(
            !directory.join("identity.json").exists(),
            "a refused install stores nothing"
        );
    }
    let s = Device::new(Database::install(
        open(&directory, &hub, "s"),
        supplied(keys.identity, items_only)?,
    )?)?;
    assert_eq!(s.database.scope(), a.database.scope());
    let status = session(&s).await;
    assert_eq!(status["node"], json!(keys.node.to_hex()));
    assert_eq!(status["issuer"], json!(false));
    assert_eq!(
        status["profile"],
        json!({"kind": "writer", "scopes": ["items"]})
    );
    assert_eq!(secret(root, "s", "reader.notes")?, None);

    a.add_peer(&s.contact().await)
        .await
        .map_err(|e| e.to_string())?;
    s.add(3, "Eggs").await;
    synced(&s).await?;
    assert_eq!(items(&s).await?, ["Eggs", "Milk"]);
    assert_eq!(
        items(&a).await?,
        ["Eggs", "Milk"],
        "the issuer serves a device it was told about"
    );
    assert!(notes(&s).await.unwrap_or_default().is_empty());
    snap::ok(async {
        s.session.renew().await?;
        s.session.info().await
    })
    .await;
    assert_eq!(
        session(&s).await["profile"],
        json!({"kind": "writer", "scopes": ["items"]}),
        "any issuer renews supplied credentials with their grants"
    );
    assert!(
        common::node_keys(&directory, None).is_err(),
        "credentials install once: the directory now holds a domain"
    );
    for device in [&a, &s] {
        device.close().await;
    }
    Ok(())
}

#[test]
fn a_grant_profile_names_exactly_its_kind_and_scopes() {
    use rocky_sdk::credentials::Profile;
    let parse = |value: Value| serde_json::from_value::<Profile>(value);
    assert_eq!(parse(json!({"kind": "issuer"})).ok(), Some(Profile::Issuer));
    assert!(parse(json!({"kind": "reader", "scopes": ["items"]})).is_ok());
    for refused in [
        json!({"kind": "issuer", "scopes": ["items"]}),
        json!({"kind": "reader", "scopes": ["items"], "rights": ["execute"]}),
        json!({"kind": "writer"}),
        json!({"kind": "owner"}),
        json!({"scopes": ["items"]}),
    ] {
        assert!(parse(refused.clone()).is_err(), "{refused}");
    }
}
