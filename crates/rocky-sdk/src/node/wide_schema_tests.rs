//! Wide schemas use one signed grant family and one delivered group secret.
use crate::{
    Database, Network, NetworkService, Open, Opened, Schema, Session, enrollment::Settings,
    transport::memory::Hub,
};
use rocky_credentials::{Authority, CredentialChain, PrivateKey, Profile};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
type Failure = crate::Failure;

fn external_identity(directory: &Path) -> Arc<dyn crate::IdentityProvider> {
    Arc::new(crate::IdentityKey::from_secret(rocky_core::commitment(
        "fixture.wide.identity",
        directory.to_string_lossy().as_bytes(),
    )))
}
fn prepared_identity(directory: &Path) -> Option<Arc<dyn crate::IdentityProvider>> {
    directory
        .join("identity.install.json")
        .exists()
        .then(|| external_identity(directory))
}
fn node_keys(directory: &Path) -> Result<crate::NodeKeys, Failure> {
    Database::node_keys_of(directory, None, Some(external_identity(directory)))
}
struct Device {
    database: Arc<Database>,
    _network: Option<NetworkService>,
    session: Session,
    scope: Value,
    identity: Option<Arc<dyn crate::IdentityProvider>>,
}
impl Device {
    fn new(opened: Opened) -> Result<Self, Failure> {
        Ok(Self {
            session: opened.database.session(),
            scope: serde_json::from_str(&opened.database.scope())?,
            database: opened.database,
            _network: opened.network,
            identity: None,
        })
    }
    fn with_identity(mut self, identity: Arc<dyn crate::IdentityProvider>) -> Self {
        self.identity = Some(identity);
        self
    }
    async fn call(&self, request: Value) -> Result<Value, Value> {
        let call = json!({"version":1,"scope":self.scope,"request":request}).to_string();
        let reply = match &self.identity {
            Some(identity) => {
                self.session
                    .call_with_identity(call, identity.clone())
                    .await
            }
            None => self.session.call(call).await,
        }
        .map_err(|error| json!(error.to_string()))?;
        let reply: Value =
            serde_json::from_str(&reply).map_err(|error| json!(error.to_string()))?;
        if reply["status"] == "ok" {
            Ok(reply["value"].clone())
        } else {
            Err(reply["error"].clone())
        }
    }
    async fn ok(&self, request: Value) -> Value {
        self.call(request.clone())
            .await
            .unwrap_or_else(|error| panic!("{request}: {error}"))
    }
    async fn contact(&self) -> String {
        self.ok(json!({"type":"contact"})).await["uri"]
            .as_str()
            .unwrap()
            .into()
    }
    async fn sync(&self) -> Result<Value, Value> {
        self.call(json!({"type":"sync"})).await
    }
    async fn add_peer(&self, contact: &str) -> Result<Value, Value> {
        self.call(json!({"type":"peer_add","contact":contact}))
            .await
    }
}

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
        identity: prepared_identity(directory),
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
fn settings(directory: &Path) -> Settings {
    serde_json::from_slice(&std::fs::read(directory.join("identity.json")).unwrap()).unwrap()
}
fn grant_count(chain: &CredentialChain) -> usize {
    chain
        .0
        .iter()
        .map(|link| {
            link.capability.grants.len() + link.capability.extended().unwrap().partitions.len()
        })
        .max()
        .unwrap()
}
fn metric(label: &str, chain: &CredentialChain) -> usize {
    let bytes = rocky_credentials::encode(chain).unwrap().len();
    let held = grant_count(chain);
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
    assert_eq!(
        grant_count(&settings(&owner_dir).credentials),
        if legacy { 63 } else { 3 }
    );
    let contact = owner.contact().await;
    let mut members = Vec::new();
    for kind in ["writer", "reader"] {
        let dir = root.path().join(kind);
        let keys = node_keys(&dir)?;
        let supplied = owner.ok(json!({"type":"grant","device":keys,"profile":{"kind":kind,"scopes":["table_000"]},"delegations":2})).await["supplied"].clone();
        let device = Device::new(Database::install(
            open(&dir, &schema, &hub, kind),
            serde_json::from_value(supplied)?,
        )?)?
        .with_identity(external_identity(&dir));
        device.add_peer(&contact).await.map_err(|e| e.to_string())?;
        metric(&format!("{label} {kind}"), &settings(&dir).credentials);
        assert_eq!(grant_count(&settings(&dir).credentials), 2);
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
        let record = saved.groups.values().next().unwrap();
        let wrapper = crate::delivery::Contents::seal(
            members[1].1.node,
            crate::delivery::Reason::Share,
            record,
            vec![0; 4096],
            Some(saved.credentials.clone()),
        )?;
        assert!(wrapper.payload_len() > 37 * 1024);
        assert!(wrapper.payload_len() <= rocky_credentials::MAX_DELIVERY_PAYLOAD);
        assert!(
            crate::delivery::Contents::seal(
                members[1].1.node,
                crate::delivery::Reason::Share,
                record,
                vec![0; 4097],
                Some(saved.credentials.clone())
            )
            .is_err()
        );
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
    let writer_sync = writer.sync().await.map_err(|e| e.to_string())?;
    owner
        .add_peer(&writer.contact().await)
        .await
        .map_err(|e| e.to_string())?;
    let owner_sync = owner.sync().await.map_err(|e| e.to_string())?;
    let reader_sync = members[1].0.sync().await.map_err(|e| e.to_string())?;
    let page = members[1].0.ok(json!({"type":"read","query":{"sql":format!("SELECT title FROM {table}"),"values":[],"max_rows":2}})).await;
    if page["rows"][0][0]["value"] != "written" {
        println!(
            "writer sync: {writer_sync}; owner sync: {owner_sync}; reader sync: {reader_sync}"
        );
        println!(
            "writer peers: {}; owner peers: {}; reader peers: {}",
            writer.ok(json!({"type":"peers"})).await,
            owner.ok(json!({"type":"peers"})).await,
            members[1].0.ok(json!({"type":"peers"})).await
        );
        println!(
            "owner activity: {}",
            owner.ok(json!({"type":"activity"})).await
        );
    }
    assert_eq!(page["rows"][0][0]["value"], "written");
    members[1].0.ok(json!({"type":"sql","sql":"BEGIN"})).await;
    assert!(
        members[1]
            .0
            .call(json!({"type":"sql","sql":format!("UPDATE {table} SET title='forged'")}))
            .await
            .is_err()
    );
    members[1]
        .0
        .ok(json!({"type":"sql","sql":"ROLLBACK"}))
        .await;
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

#[tokio::test]
async fn local_sql_sessions_cover_more_than_256_authorized_scopes() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let schema = schema(160);
    let device = Device::new(Database::create(
        Open {
            directory: directory.path(),
            schema: &schema,
            identity: None,
            secrets: None,
            network: None,
            coordination: None,
        },
        None,
    )?)?;
    device.ok(json!({"type":"partition_create","group":"book","values":[{"type":"text","value":"A"}]})).await;
    device.ok(json!({"type":"sql","sql":"BEGIN"})).await;
    let id = device.ok(json!({"type":"next_id","table":"table_159","partition_values":[{"type":"text","value":"A"}]})).await["id"].clone();
    device.ok(json!({"type":"sql","sql":"INSERT INTO table_159(id,book,title) VALUES(?,'A','wide')","values":[{"type":"integer","value":id}]})).await;
    device.ok(json!({"type":"sql","sql":"COMMIT"})).await;
    let rows = device.ok(json!({"type":"read","query":{"sql":"SELECT title FROM table_159","values":[],"max_rows":2}})).await;
    assert_eq!(rows["rows"][0][0]["value"], "wide");
    device.database.close().await?;
    Ok(())
}
