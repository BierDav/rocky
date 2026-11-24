use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_credentials::Revocation;
use rocky_sdk::{Access, Network, RoleName, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

use crate::common;

fn text(name: &str) -> rocky_sdk::Column {
    crate::common::required(name, rocky_sdk::Affinity::Text)
}
fn schema() -> rocky_sdk::Schema {
    crate::common::build(
        crate::common::shopping(1)
            .table(crate::common::items())
            .table(
                rocky_sdk::Table::replayed("notes")
                    .column(text("key"))
                    .column(text("text"))
                    .primary_key(["key"]),
            )
            .role(
                rocky_sdk::Role::new(rocky_sdk::RoleName::from_static("item_reader")).read("items"),
            )
            .role(
                rocky_sdk::Role::new(rocky_sdk::RoleName::from_static("note_writer"))
                    .write("notes"),
            )
            .role(
                rocky_sdk::Role::new(rocky_sdk::RoleName::from_static("item_writer"))
                    .write("items"),
            ),
    )
}
const ITEM_READER: RoleName = RoleName::from_static("item_reader");
const NOTE_WRITER: RoleName = RoleName::from_static("note_writer");
const ITEM_WRITER: RoleName = RoleName::from_static("item_writer");

fn memory(hub: &Arc<Hub>, name: &str) -> Network {
    Network::Owned(hub.connector(name))
}
fn open(directory: &Path, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        &schema(),
        None,
        common::prepared_identity(directory),
        Some(memory(hub, name)),
    )
}
async fn create(root: &Path, hub: &Arc<Hub>, name: &str) -> Result<Device, Failure> {
    Device::new(open(&root.join(name), hub, name).create().await?)
}
async fn join(
    root: &Path,
    hub: &Arc<Hub>,
    name: &str,
    invitation: &rocky_sdk::InvitationCode,
) -> Result<Device, Failure> {
    Device::new(open(&root.join(name), hub, name).join(invitation).await?)
}
async fn reopen(root: &Path, hub: &Arc<Hub>, name: &str) -> Result<Device, Failure> {
    Device::new(open(&root.join(name), hub, name).open().await?)
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    let mut identity = common::authority_json(&root.join(name), "identity")?;
    for epochs in identity["keys"].as_object_mut().ok_or("keys")?.values_mut() {
        for epoch in epochs.as_array_mut().ok_or("epochs")? {
            *epoch = epoch["generation"].clone();
        }
    }
    Ok(identity)
}
fn secret(root: &Path, name: &str, role: &str) -> Result<Option<[u8; 32]>, Failure> {
    let state = common::authority_json(&root.join(name), "identity")?;
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
        &device.database,
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
async fn texts(device: &Device, sql: &str) -> Result<Vec<String>, Failure> {
    let page = snap::observe(snap::read(
        &device.database,
        &snap::text(&(sql)),
        Vec::new(),
    ))
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
async fn device_info(device: &Device) -> Value {
    snap::ok(snap::device(&device.database)).await
}
async fn manual(devices: &[&Device]) {
    for device in devices {
        snap::ok(device.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
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
async fn roles_bound_reads_and_writes_across_renewal() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a").await?;
    let reader = Access::Role(ITEM_READER);
    let writer = Access::Role(NOTE_WRITER);
    let r = join(root, &hub, "r", &a.invite_as(reader.clone()).await).await?;
    let w = join(root, &hub, "w", &a.invite_as(writer.clone()).await).await?;
    let refused = snap::observe(a.database.invite(rocky_sdk::Invite::new(Access::Role(
        RoleName::new("unknown")?,
    ))))
    .await;
    assert!(refused.is_err(), "an undeclared role invites nobody");
    item(&a, 1, "Milk").await?;
    note(&a, 2, "Hello").await?;
    synced(&r).await?;
    synced(&w).await?;

    let check = async |renewed: bool| -> Result<(), Failure> {
        assert_eq!(
            device_info(&r).await["access"],
            json!(reader),
            "renewed {renewed}"
        );
        assert_eq!(
            device_info(&w).await["access"],
            json!(writer),
            "renewed {renewed}"
        );
        assert_eq!(identity(root, "r")?["access"], json!(reader));
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
            snap::observe(r.database.invite(rocky_sdk::Invite::new(reader.clone())))
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
        assert!(snap::observe(w.database.people()).await.is_err());
        Ok(())
    };
    check(false).await?;
    for device in [&r, &w] {
        let before = device.database.me().await?;
        device.database.renew().await?;
        let renewed = device.database.me().await?;
        assert_eq!(renewed.tables, before.tables);
        assert_eq!(renewed.person, before.person);
    }
    check(true).await?;
    note(&w, 3, "From writer").await?;
    synced(&w).await?;
    assert_eq!(notes(&a).await?, ["From writer", "Hello"]);
    assert_eq!(items(&a).await?, ["Milk"]);
    let identities = snap::ok(a.database.people()).await;
    let mut listed: Vec<Value> = identities["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|record| record["owner"] != true)
        .map(|record| record["access"].clone())
        .collect();
    listed.sort_by_key(Value::to_string);
    assert_eq!(
        listed,
        [json!(reader), json!(writer)],
        "the issuer lists each identity with the access it enrolled with"
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
    let a = create(root, &hub, "a").await?;
    let i = join(root, &hub, "i", &a.invite_as(rocky_sdk::Access::Full).await).await?;
    a.admit(&i).await;
    manual(&[&a, &i]).await;
    item(&a, 1, "Milk").await?;
    synced(&i).await?;
    let (creator, issuer) = (device_info(&a).await, device_info(&i).await);
    assert_eq!(issuer["issuer"], json!(true));
    assert_eq!(issuer["access"], json!("full"));
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

    let m = join(root, &hub, "m", &i.invite().await).await?;
    manual(&[&m]).await;
    assert_eq!(m.database.scope(), scope);
    synced(&m).await?;
    assert_eq!(m.titles().await, ["Milk"]);
    m.add(2, "Eggs").await;
    synced(&m).await?;
    snap::ok(async {
        m.database.renew().await?;
        snap::device(&m.database).await
    })
    .await;
    assert_eq!(i.titles().await, ["Eggs", "Milk"]);
    let identities = snap::ok(i.database.people()).await.to_string();
    assert!(identities.contains(m.identity().await.as_str()));
    for device in [&i, &m] {
        device.close().await;
    }
    let a = reopen(root, &hub, "a").await?;
    let reopened = device_info(&a).await;
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
    let a = create(root, &hub, "a").await?;
    let b = join(
        root,
        &hub,
        "b",
        &a.invite_as(Access::Role(ITEM_WRITER)).await,
    )
    .await?;
    let c = join(
        root,
        &hub,
        "c",
        &a.invite_as(Access::Role(ITEM_READER)).await,
    )
    .await?;
    let w = join(
        root,
        &hub,
        "w",
        &a.invite_as(Access::Role(NOTE_WRITER)).await,
    )
    .await?;
    item(&a, 1, "Before").await?;
    for device in [&b, &c] {
        synced(device).await?;
        assert_eq!(items(device).await?, ["Before"]);
    }
    snap::ok(a.database.remove(
        rocky_sdk::PersonId::new(snap::public_key(&(c.identity().await))),
        rocky_sdk::Removal::Departed,
    ))
    .await;
    let revoked = snap::ok(snap::device(&a.database)).await;
    assert_eq!(revoked["revocations"]["statements"], 1);
    assert_eq!(
        identity(root, "a")?["keys"],
        json!({"issuer_registry": [1], "items": [1, 2], "notes": [1]}),
        "only the collections the revoked reader could read get a new epoch"
    );
    assert!(identity(root, "a")?["authority"]["rotation"].is_null());
    item(&a, 2, "After").await?;

    synced(&b).await?;
    assert_eq!(device_info(&b).await["revocations"]["statements"], 1);
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
        device_info(&w).await["revocations"]["statements"],
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
            c.database.renew().await?;
            snap::device(&c.database).await
        })
        .await
        .is_err()
    );
    assert!(!snap::ok(c.database.status()).await["authority"]["verified_revocation"].is_null());
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

    let reopened = reopen(root, &hub, "c").await?;
    assert!(
        !snap::ok(reopened.database.status()).await["authority"]["verified_revocation"].is_null()
    );
    reopened.close().await;
    let c_dir = root.join("c");
    let original = common::authority_bytes(&c_dir, "revoked")?.ok_or("revoked notice")?;
    let mut tampered: Value = serde_json::from_slice(&original)?;
    assert_eq!(tampered["version"], 3);
    tampered["statement"]["statement"]["time"] = json!(999);
    common::write_authority(&c_dir, "revoked", &serde_json::to_vec(&tampered)?).await?;
    let reopened = reopen(root, &hub, "c").await?;
    assert!(
        snap::observe(reopened.database.status()).await.is_err(),
        "a local file cannot forge a publisher's revocation notice"
    );
    reopened.close().await;
    common::write_authority(&c_dir, "revoked", &original).await?;

    let before = secrets(root, "b")?;
    let role = before
        .iter()
        .find(|name| name.contains(".reader.items.2."))
        .ok_or("epoch 2 key")?;
    std::fs::remove_file(root.join("b/secrets").join(role))?;
    assert!(
        reopen(root, &hub, "b").await.is_err(),
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
    let a = create(root, &hub, "a").await?;
    let b = join(root, &hub, "b", &a.invite().await).await?;
    let c = join(root, &hub, "c", &a.invite().await).await?;
    item(&a, 1, "Before").await?;
    synced(&b).await?;
    let (c_node, c_identity) = (c.node().await, c.identity().await);
    let c_chain: rocky_credentials::CredentialChain =
        serde_json::from_value(identity(root, "c")?["credentials"].clone())?;
    let c_capability = c_chain
        .0
        .iter()
        .rev()
        .find(|link| link.capability.kind == rocky_core::PrincipalKind::User)
        .ok_or("c's capability")?
        .id()?;
    for device in [&a, &b, &c] {
        device.close().await;
    }
    let a_dir = root.join("a");
    let mut saved = common::authority_json(&a_dir, "identity")?;
    let manifest = rocky_credentials::SignedManifest::from_bytes(
        &data_encoding::HEXLOWER.decode(saved["genesis"].as_str().ok_or("genesis")?.as_bytes())?,
    )?;
    let statement = Revocation {
        app: manifest
            .verify_historic(serde_json::from_value(saved["publisher"].clone())?)?
            .app,
        issuer: serde_json::from_value(saved["identity"].clone())?,
        time: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        nodes: Default::default(),
        identities: [serde_json::from_value(json!(c_identity))?].into(),
        capabilities: [c_capability].into(),
        cut: Default::default(),
        mode: rocky_credentials::RevocationMode::Lost,
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
    common::write_authority(&a_dir, "identity", saved.to_string().as_bytes()).await?;
    let installation = saved["installation"].as_str().ok_or("installation")?;
    let made_before_the_device_stopped = [7; 32];
    std::fs::write(
        root.join("a/secrets")
            .join(format!("rocky.{installation}.rotation.reader.items.2")),
        made_before_the_device_stopped,
    )?;

    for round in 0..2 {
        let a = reopen(root, &hub, "a").await?;
        assert_eq!(
            device_info(&a).await["revocations"]["statements"],
            1,
            "round {round}"
        );
        let saved = identity(root, "a")?;
        assert!(saved["authority"]["rotation"].is_null());
        assert_eq!(
            saved["keys"],
            json!({"issuer_registry": [1], "items": [1, 2], "notes": [1]})
        );
        let listed = snap::ok(a.database.people()).await;
        assert!(
            listed["identities"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|record| record["identity"] != c_identity),
            "{listed}"
        );
        let authority: rocky_credentials::RevocationSet =
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
    let a = reopen(root, &hub, "a").await?;
    let b = reopen(root, &hub, "b").await?;
    synced(&b).await?;
    assert_eq!(device_info(&b).await["revocations"]["statements"], 1);
    assert_eq!(
        secret(root, "b", "reader.items.2")?,
        Some(made_before_the_device_stopped)
    );
    assert_eq!(items(&b).await?, ["After", "Before"]);
    let c = reopen(root, &hub, "c").await?;
    assert!(common::exchange_failed(&c.sync().await));
    assert_eq!(secret(root, "c", "reader.items.2")?, None);
    for device in [&a, &b, &c] {
        device.close().await;
    }
    Ok(())
}

#[test]
fn access_serializes_as_a_name_and_refuses_reserved_role_names() {
    assert_eq!(json!(Access::Full), json!("full"));
    assert_eq!(json!(Access::Store), json!("store"));
    assert_eq!(
        json!(Access::Role(ITEM_READER)),
        json!({"role": "item_reader"})
    );
    for refused in [
        json!({"role": "admin"}),
        json!({"role": "Read"}),
        json!({"role": "two words"}),
        json!("admin"),
        json!({"kind": "writer"}),
    ] {
        assert!(
            serde_json::from_value::<Access>(refused.clone()).is_err(),
            "{refused}"
        );
    }
}
