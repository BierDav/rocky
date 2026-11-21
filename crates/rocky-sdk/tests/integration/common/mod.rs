use crate::support::snapshot as snap;
use rocky_sdk::{Database, Network, NetworkService, Open, Opened, Session};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

pub type Failure = Box<dyn std::error::Error + Send + Sync>;

pub fn authority_bytes(directory: &Path, key: &str) -> Result<Option<Vec<u8>>, Failure> {
    Ok(
        rocky_secure::read_authority_entry(&directory.join("rocky.sqlite"), key)?
            .map(|(bytes, _)| bytes),
    )
}
pub fn authority_json(directory: &Path, key: &str) -> Result<Value, Failure> {
    let bytes =
        authority_bytes(directory, key)?.ok_or_else(|| format!("missing authority entry {key}"))?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub async fn write_authority(directory: &Path, key: &str, bytes: &[u8]) -> Result<(), Failure> {
    rocky_secure::write_authority_entry(&directory.join("rocky.sqlite"), key, bytes, 1).await?;
    Ok(())
}

pub const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

pub fn external_identity(directory: &Path) -> Arc<dyn rocky_sdk::IdentityProvider> {
    Arc::new(rocky_sdk::IdentityKey::from_secret(rocky_core::commitment(
        "fixture.external-identity",
        directory.to_string_lossy().as_bytes(),
    )))
}
pub fn prepared_identity(directory: &Path) -> Option<Arc<dyn rocky_sdk::IdentityProvider>> {
    directory
        .join("identity.install.json")
        .exists()
        .then(|| external_identity(directory))
}
pub fn node_keys(
    directory: &Path,
    secrets: Option<Arc<dyn rocky_sdk::SecretStore>>,
) -> Result<rocky_sdk::NodeKeys, Failure> {
    Database::node_keys_of(directory, secrets, Some(external_identity(directory)))
}

pub struct Device {
    pub database: Arc<Database>,
    pub network: Option<NetworkService>,
    pub session: Session,
    scope: rocky_sdk::Scope,
    root: Option<String>,
    signer: Option<Arc<dyn rocky_sdk::IdentityProvider>>,
}
pub fn parsed_schema() -> &'static rocky_sdk::Schema {
    static PARSED: std::sync::LazyLock<rocky_sdk::Schema> =
        std::sync::LazyLock::new(|| rocky_sdk::Schema::parse(SCHEMA).unwrap());
    &PARSED
}
fn open(directory: &Path, network: Option<Network>) -> Open<'_> {
    Open {
        identity: prepared_identity(directory),
        directory,
        schema: parsed_schema(),
        secrets: None,
        network,
        coordination: None,
    }
}
impl Device {
    pub fn new(opened: Opened) -> Result<Self, Failure> {
        Ok(Self {
            session: opened.database.session(),
            scope: opened.database.scope(),
            root: opened.root.as_ref().map(rocky_sdk::RootKey::phrase),
            signer: opened
                .root
                .as_ref()
                .map(|root| Arc::new(root.identity()) as Arc<dyn rocky_sdk::IdentityProvider>),
            database: opened.database,
            network: opened.network,
        })
    }
    pub fn create(directory: &Path, network: Option<Network>) -> Result<Self, Failure> {
        let signer = external_identity(directory);
        let mut options = open(directory, network);
        options.identity = Some(signer.clone());
        Self::new(Database::create(options, None)?).map(|device| device.with_identity(signer))
    }
    pub fn join(directory: &Path, invitation: &str, network: Network) -> Result<Self, Failure> {
        let signer = external_identity(directory);
        let mut options = open(directory, Some(network));
        options.identity = Some(signer.clone());
        Self::new(Database::join(options, invitation)?).map(|device| device.with_identity(signer))
    }
    pub fn open(directory: &Path, network: Option<Network>) -> Result<Self, Failure> {
        Self::new(Database::open(open(directory, network))?)
            .map(|device| device.with_identity(external_identity(directory)))
    }
    pub fn with_identity(mut self, signer: Arc<dyn rocky_sdk::IdentityProvider>) -> Self {
        self.signer = Some(signer);
        self
    }
    pub fn root(&self) -> Arc<dyn rocky_sdk::IdentityProvider> {
        self.root_signer().expect("fixture root")
    }
    pub fn scope(&self) -> &rocky_sdk::Scope {
        &self.scope
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn root_signer(&self) -> Result<Arc<dyn rocky_sdk::IdentityProvider>, Failure> {
        Ok(Arc::new(rocky_sdk::RootKey::from_phrase(
            self.root.as_deref().ok_or("no root")?,
        )?))
    }
    async fn handle(&self, handle: u8) -> rocky_core::Commitment {
        let mut bytes = self
            .session
            .info()
            .await
            .expect("session")
            .node
            .to_hex()
            .into_bytes();
        bytes.extend(serde_json::to_vec(&vec![handle; 32]).expect("handle"));
        rocky_core::commitment("fixture.sql.handle", &bytes)
    }
    pub async fn add(&self, handle: u8, title: &str) -> String {
        let handle = self.handle(handle).await;
        let key = data_encoding::HEXLOWER.encode(&handle);
        let exists = self
            .database
            .query(
                "SELECT 1 FROM items WHERE key=?",
                vec![rocky_sdk::SqlValue::Text(key.clone())],
            )
            .await
            .expect("query")
            .next()
            .await
            .expect("next")
            .is_some();
        if !exists {
            let mut tx = self.database.transaction().await.expect("begin");
            let id = tx.next_id("items", Vec::new()).await.expect("id");
            tx.execute(
                "INSERT INTO items(key,id,title,completed) VALUES(?,?,?,0)",
                vec![
                    rocky_sdk::SqlValue::Text(key.clone()),
                    id.sql(),
                    rocky_sdk::SqlValue::Text(title.into()),
                ],
            )
            .await
            .expect("insert");
            tx.commit().await.expect("commit");
        }
        key
    }
    pub async fn set(&self, _handle: u8, id: &str, field: &str, value: Value) {
        use rocky_sdk::SqlValue;
        assert!(
            field
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        );
        let value = match value {
            Value::Null => SqlValue::Null,
            Value::Bool(value) => SqlValue::Integer(i64::from(value)),
            Value::Number(value) => SqlValue::Integer(value.as_i64().expect("integer")),
            Value::String(value) => SqlValue::Text(value),
            _ => panic!("unsupported fixture value"),
        };
        let mut tx = self.database.transaction().await.expect("begin");
        tx.execute(
            &format!("UPDATE items SET \"{field}\"=? WHERE key=?"),
            vec![value, SqlValue::Text(id.into())],
        )
        .await
        .expect("update");
        tx.commit().await.expect("commit");
    }
    pub async fn remove(&self, _handle: u8, id: &str) {
        let mut tx = self.database.transaction().await.expect("begin");
        tx.execute(
            "DELETE FROM items WHERE key=?",
            vec![rocky_sdk::SqlValue::Text(id.into())],
        )
        .await
        .expect("delete");
        tx.commit().await.expect("commit");
    }
    pub async fn items(&self) -> Vec<String> {
        let rows = self
            .database
            .query(
                "SELECT title, completed FROM items ORDER BY title",
                Vec::new(),
            )
            .await
            .expect("read")
            .collect()
            .await
            .expect("collect");
        rows.into_iter()
            .map(|row| {
                format!(
                    "{}/{}",
                    row.get_at::<rocky_sdk::SqlValue>(0)
                        .expect("title")
                        .to_text()
                        .expect("title"),
                    row.get_at::<rocky_sdk::SqlValue>(1)
                        .expect("completed")
                        .to_text()
                        .expect("completed")
                )
            })
            .collect()
    }
    pub async fn titles(&self) -> Vec<String> {
        let items = self.items().await;
        items
            .iter()
            .filter_map(|item| item.split('/').next().map(String::from))
            .collect()
    }
    pub async fn sync_once(&self) -> Result<rocky_sdk::ReconciliationReport, Failure> {
        match self.signer.clone() {
            Some(identity) => self.session.sync_once_with_identity(identity).await,
            None => self.session.sync_once().await,
        }
    }
    pub async fn follow(&self, name: &str) -> Result<rocky_sdk::TipStatus, Failure> {
        match self.signer.clone() {
            Some(identity) => self.session.follow_with_identity(name, identity).await,
            None => self.session.follow(name).await,
        }
    }
    pub async fn open_tip(&self, seal: &str, minimum_in_sync: u32) -> Result<String, Failure> {
        match self.signer.clone() {
            Some(identity) => {
                self.session
                    .open_tip_with_identity(seal, minimum_in_sync, identity)
                    .await
            }
            None => self.session.open_tip(seal, minimum_in_sync).await,
        }
    }
    pub async fn move_bookmark(
        &self,
        name: &str,
        tip: &str,
    ) -> Result<rocky_sdk::TipStatus, Failure> {
        match self.signer.clone() {
            Some(identity) => {
                self.session
                    .move_bookmark_with_identity(name, tip, identity)
                    .await
            }
            None => self.session.move_bookmark(name, tip).await,
        }
    }
    pub async fn follow_when_ready(&self, name: &str) -> Result<rocky_sdk::TipStatus, Failure> {
        match self.signer.clone() {
            Some(identity) => {
                self.session
                    .follow_when_ready_with_identity(name, identity)
                    .await
            }
            None => self.session.follow_when_ready(name).await,
        }
    }
    pub async fn sync(&self) -> Result<Value, Value> {
        snap::observe(self.sync_once()).await
    }
    pub async fn invite(&self) -> String {
        self.invite_as(json!({"kind": "writer", "scopes": ["items"]}))
            .await
    }
    pub async fn invite_as(&self, profile: Value) -> String {
        self.session
            .invitation_with(serde_json::from_value(profile).expect("profile"))
            .await
            .expect("invitation")
            .uri
    }
    pub async fn contact(&self) -> String {
        self.session.contact().await.expect("contact")
    }
    pub async fn node(&self) -> String {
        self.session.info().await.expect("session").node.to_hex()
    }
    pub async fn identity(&self) -> String {
        self.session
            .info()
            .await
            .expect("session")
            .principal
            .to_hex()
    }
    pub async fn add_peer(&self, contact: &str) -> Result<Value, Value> {
        snap::observe(async {
            let value = self.session.add_peer(&(snap::text(&(contact)))).await?;
            Ok(json!({"node":value}))
        })
        .await
    }
    pub async fn peer(&self, node: &str) -> Value {
        let peers = snap::ok(async {
            let network = self.session.network().await?;
            Ok(json!({"peers":network.peers,"discovery":network.discovery}))
        })
        .await;
        peers["peers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|peer| peer["node"] == node)
            .cloned()
            .unwrap_or_default()
    }
    pub async fn close(&self) {
        self.database.close().await.expect("close");
    }
    pub async fn follow_main(&self) {
        self.session
            .create_bookmark("main", None)
            .await
            .expect("bookmark");
        self.follow("main").await.expect("follow");
    }
    pub async fn follow_main_migrating(&self) {
        self.follow_main().await;
        self.session.set_genesis_minimum(1).await.expect("minimum");
    }
    pub async fn seal_and_compact(&self) -> Value {
        self.session.seal(false).await.expect("seal");
        serde_json::to_value(self.session.compact(0).await.expect("compact")).expect("report")
    }
}
pub async fn introduce(left: &Device, right: &Device) {
    left.add_peer(&right.contact().await).await.expect("peer");
    right.add_peer(&left.contact().await).await.expect("peer");
}

pub fn reconciled(report: &Value) -> bool {
    crate::support::reconciled(report)
}
pub fn exchange_failed(result: &Result<Value, Value>) -> bool {
    match result {
        Err(_) => true,
        Ok(report) => report["peers"].as_array().is_some_and(|peers| {
            peers.iter().any(|peer| {
                peer["skip"] == "backoff"
                    || peer["attempts"].as_array().is_some_and(|attempts| {
                        attempts
                            .iter()
                            .any(|attempt| attempt["outcome"] == "failed")
                    })
            })
        }),
    }
}

pub fn confirmed(open: Open<'_>) -> Result<Opened, Failure> {
    let directory = open.directory;
    let schema = open.schema;
    let secrets = open.secrets.clone();
    let report = Database::open(Open {
        identity: None,
        directory,
        schema,
        secrets,
        network: None,
        coordination: None,
    })
    .err()
    .ok_or("expected removal confirmation")?
    .downcast::<rocky_sdk::RolloutConfirmationRequired>()?;
    Database::open_confirmed(open, &report.report.digest)
}

pub async fn link(open: Open<'_>, contact: &str, responder: &Session) -> Result<Opened, Failure> {
    let config = rocky_sdk::Configuration::new(open.directory, open.schema)?;
    let config = if let Some(store) = open.secrets {
        config.secrets(store)
    } else {
        config
    };
    let config = if let Some(network) = open.network {
        config.network(network)
    } else {
        config
    };
    let identity = open.identity.ok_or("linking needs an identity")?;
    let error = match config.link(contact, identity.clone()).await {
        Ok(_) => return Err("unconfirmed link succeeded".into()),
        Err(error) => error,
    };
    let pending = Arc::new(
        *error
            .downcast::<rocky_sdk::AwaitingLink>()
            .map_err(|error| error.to_string())?,
    );
    let shown = responder
        .discovered()
        .await?
        .links
        .into_iter()
        .find(|request| request.node == pending.node)
        .ok_or("missing link request")?;
    assert_eq!(shown.code, pending.code);
    assert!(!shown.confirmed);
    responder.confirm_link(pending.node, &shown.code).await?;
    config
        .confirm_link(contact, identity, pending.clone(), &shown.code)
        .await
}
