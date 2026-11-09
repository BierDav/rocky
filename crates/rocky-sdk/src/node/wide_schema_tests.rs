//! Wide schemas use one signed grant family and one delivered group secret.
#[path = "../../tests/common/mod.rs"]
mod common;
use crate::{Database, Network, Open, Schema, enrollment::Settings, transport::memory::Hub};
use common::{Device, Failure};
use rocky_credentials::{Authority, CredentialChain, PrivateKey, Profile};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn schema(count: usize) -> String {
    let tables: Vec<_> = (0..count)
        .map(|i| {
            let mut table = json!({"name":format!("table_{i:03}"),"class":"replayed",
            "columns":[{"name":"book","type":"text","not_null":true},
                {"name":"title","type":"text","not_null":true}],
            "primary_key":["book","id"],"partition_by":["book"],"partition_group":"book"});
            if i != 0 {
                table["granted_like"] = json!("table_000");
            }
            table
        })
        .collect();
    json!({"format":6,"application":"wide_grants","version":1,"tables":tables}).to_string()
}
fn open<'a>(directory: &'a Path, schema: &'a str, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        directory,
        schema,
        identity: common::prepared_identity(directory),
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
fn settings(directory: &Path) -> Settings {
    serde_json::from_slice(&std::fs::read(directory.join("identity.json")).unwrap()).unwrap()
}
fn metric(label: &str, chain: &CredentialChain) -> usize {
    let bytes = rocky_credentials::encode(chain).unwrap().len();
    let held = chain
        .0
        .iter()
        .map(|link| {
            link.capability.grants.len() + link.capability.extended().unwrap().partitions.len()
        })
        .max()
        .unwrap();
    assert!(held <= rocky_credentials::MAX_GRANTS);
    assert!(bytes <= rocky_credentials::MAX_CREDENTIAL_BYTES);
    println!("{label}: grants={held}, chain_bytes={bytes}");
    bytes
}
fn epochs_request() -> Value {
    json!({"type":"partition_status","group":"book","values":[{"type":"text","value":"A"}]})
}
async fn assert_epochs(device: &Device, count: usize, expected: &[u64]) {
    let status = device.ok(epochs_request()).await;
    let tables = status["tables"].as_array().unwrap();
    assert_eq!(tables.len(), count);
    for table in tables {
        let epochs: Vec<_> = table["epochs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|epoch| epoch["generation"].as_u64().unwrap())
            .collect();
        assert_eq!(epochs, expected, "{table}");
    }
}
async fn exercise(count: usize, legacy: bool) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let schema = schema(count);
    assert_eq!(Schema::parse(&schema)?.tables.len(), count);
    let hub = Arc::new(Hub::default());
    let owner_dir = root.path().join("owner");
    let mut owner = Device::new(Database::create(
        open(&owner_dir, &schema, &hub, "owner"),
        None,
    )?)?;
    if legacy {
        owner.database.close().await?;
        let mut held = settings(&owner_dir);
        let store: Arc<dyn crate::SecretStore> =
            Arc::new(crate::FileSecretStore::new(owner_dir.join("secrets")));
        let node = PrivateKey::from_bytes(&held.vault(&store).get("node")?);
        let domain = held.domain()?;
        let grants = crate::enrollment::grants_of(&Profile::Issuer, &domain)?;
        let partitions = crate::enrollment::whole_partitions(&domain, &grants);
        let certificate = held.credentials.certificate().unwrap().clone();
        let mut links = held.issuer_links();
        let certified = links.with_certificate(certificate.clone());
        let authority = Authority::issuer(&node, &certified)?;
        let capability = authority.hold(
            grants,
            partitions,
            (crate::enrollment::now()?, held.ttl, 300),
        )?;
        links.0.extend([capability, certificate]);
        links.verify_selectors(
            held.publisher,
            node.public_key(),
            &held.revocations,
            crate::enrollment::now()?,
        )?;
        held.credentials = links;
        held.save(&owner_dir.join("identity.json"))?;
        owner = Device::new(Database::open(open(&owner_dir, &schema, &hub, "owner"))?)?;
    }
    let label = format!(
        "{count} tables{}",
        if legacy { " per-table" } else { " family" }
    );
    let before = metric(&format!("{label} owner"), &settings(&owner_dir).credentials);
    let contact = owner.contact().await;
    let mut members = Vec::new();
    for kind in ["writer", "reader"] {
        let dir = root.path().join(kind);
        let keys = common::node_keys(&dir, None)?;
        let supplied = owner.ok(json!({"type":"grant","device":keys,"profile":{"kind":kind,"scopes":["table_000"]},"delegations":2})).await["supplied"].clone();
        let device = Device::new(Database::install(
            open(&dir, &schema, &hub, kind),
            serde_json::from_value(supplied)?,
        )?)?
        .with_identity(common::external_identity(&dir));
        device.add_peer(&contact).await.map_err(|e| e.to_string())?;
        metric(&format!("{label} {kind}"), &settings(&dir).credentials);
        members.push((device, keys, dir));
    }
    owner.ok(json!({"type":"partition_create","group":"book","values":[{"type":"text","value":"A"}]})).await;
    owner.ok(json!({"type":"partition_share","group":"book","values":[{"type":"text","value":"A"}],"recipients":[members[0].1.node,members[1].1.node]})).await;
    let saved = settings(&owner_dir);
    let delivery = saved
        .deliveries
        .values()
        .find_map(|pending| pending.sealed().transpose())
        .transpose()?
        .unwrap();
    println!(
        "{label}: share_payload_bytes={}, share_frame_bytes={}",
        delivery.payload_len(),
        delivery.to_bytes()?.len()
    );
    assert!(delivery.payload_len() <= rocky_credentials::MAX_DELIVERY_PAYLOAD);
    assert!(delivery.to_bytes()?.len() < rocky_secure::MAX_SECURE_FRAME);
    if legacy {
        assert!(delivery.payload_len() > 37 * 1024);
    }
    for (device, _, _) in &members {
        device.sync().await.map_err(|e| e.to_string())?;
        assert_epochs(device, count, &[1]).await;
    }
    let table = format!("table_{:03}", count - 1);
    let writer = &members[0].0;
    writer.ok(json!({"type":"sql","sql":"BEGIN"})).await;
    let id = writer.ok(json!({"type":"next_id","table":table,"partition_values":[{"type":"text","value":"A"}]})).await["id"].clone();
    writer.ok(json!({"type":"sql","sql":format!("INSERT INTO {table}(id,book,title) VALUES(?,'A','written')"),"values":[{"type":"integer","value":id}]})).await;
    writer.ok(json!({"type":"sql","sql":"COMMIT"})).await;
    writer.sync().await.map_err(|e| e.to_string())?;
    members[1].0.sync().await.map_err(|e| e.to_string())?;
    let page = members[1].0.ok(json!({"type":"read","query":{"sql":format!("SELECT title FROM {table}"),"values":[],"max_rows":2}})).await;
    assert_eq!(page["rows"][0][0]["value"], "written");
    assert!(
        members[1]
            .0
            .call(json!({"type":"sql","sql":format!("UPDATE {table} SET title='forged'")}))
            .await
            .is_err()
    );
    owner
        .ok(json!({"type":"revoke","identity":members[0].1.identity}))
        .await;
    members[1].0.sync().await.map_err(|e| e.to_string())?;
    assert_epochs(&members[1].0, count, &[1, 2]).await;
    assert_epochs(&members[0].0, count, &[1]).await;
    let after = metric(
        &format!("{label} owner after rotation"),
        &settings(&owner_dir).credentials,
    );
    if legacy {
        assert!(after < before);
    } else {
        assert_eq!(after, before);
    }
    for (device, _, _) in members {
        device.database.close().await?;
    }
    owner.database.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_hundred_tables_share_and_rotate_one_grant_family() -> Result<(), Failure> {
    exercise(100, false).await
}

#[tokio::test]
async fn thirty_one_tables_use_the_same_credential_budget() -> Result<(), Failure> {
    exercise(31, false).await
}

#[tokio::test]
async fn per_table_credentials_reopen_share_and_renew_into_families() -> Result<(), Failure> {
    exercise(31, true).await
}
