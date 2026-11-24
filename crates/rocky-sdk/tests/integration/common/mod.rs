use crate::support::snapshot as snap;
use rocky_sdk::{Configuration, Created, Database, Network};
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

pub const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":7,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

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
pub struct Opened {
    database: Database,
    root: Option<rocky_sdk::RootKey>,
}
impl From<Database> for Opened {
    fn from(database: Database) -> Self {
        Self {
            database,
            root: None,
        }
    }
}
impl From<rocky_sdk::Opened> for Opened {
    fn from(opened: rocky_sdk::Opened) -> Self {
        opened.database.into()
    }
}
impl From<Created> for Opened {
    fn from(created: Created) -> Self {
        Self {
            database: created.database,
            root: Some(created.root),
        }
    }
}
pub struct Device {
    pub database: Database,
    scope: rocky_sdk::Scope,
    root: Option<String>,
    signer: Option<Arc<dyn rocky_sdk::IdentityProvider>>,
}
pub fn parsed_schema() -> &'static rocky_sdk::Schema {
    static PARSED: std::sync::LazyLock<rocky_sdk::Schema> =
        std::sync::LazyLock::new(|| serde_json::from_str(SCHEMA).unwrap());
    &PARSED
}
pub fn configuration(
    directory: &Path,
    schema: &rocky_sdk::Schema,
    secrets: Option<Arc<dyn rocky_sdk::SecretStore>>,
    identity: Option<Arc<dyn rocky_sdk::IdentityProvider>>,
    network: Option<Network>,
) -> Configuration {
    let mut configuration = Configuration::new(directory, schema).expect("fixture schema");
    if let Some(secrets) = secrets {
        configuration = configuration.secrets(secrets);
    }
    if let Some(identity) = identity {
        configuration = configuration.identity(identity);
    }
    if let Some(network) = network {
        configuration = configuration.network(network);
    }
    configuration
}
fn open(
    directory: &Path,
    identity: Option<Arc<dyn rocky_sdk::IdentityProvider>>,
    network: Option<Network>,
) -> Configuration {
    configuration(
        directory,
        parsed_schema(),
        None,
        identity.or_else(|| prepared_identity(directory)),
        network,
    )
}
impl Device {
    pub fn new(opened: impl Into<Opened>) -> Result<Self, Failure> {
        let opened = opened.into();
        Ok(Self {
            scope: opened.database.scope(),
            root: opened.root.as_ref().map(rocky_sdk::RootKey::phrase),
            signer: opened
                .root
                .as_ref()
                .map(|root| Arc::new(root.identity()) as Arc<dyn rocky_sdk::IdentityProvider>),
            database: opened.database,
        })
    }
    pub async fn create(directory: &Path, network: Option<Network>) -> Result<Self, Failure> {
        let signer = external_identity(directory);
        let created = open(directory, Some(signer.clone()), network)
            .create()
            .await?;
        Self::new(created).map(|device| device.with_identity(signer))
    }
    pub async fn join(
        directory: &Path,
        invitation: &rocky_sdk::InvitationCode,
        network: Network,
    ) -> Result<Self, Failure> {
        let signer = external_identity(directory);
        let joined = open(directory, Some(signer.clone()), Some(network))
            .join(invitation)
            .await?;
        Self::new(joined.database).map(|device| device.with_identity(signer))
    }
    pub async fn open(directory: &Path, network: Option<Network>) -> Result<Self, Failure> {
        Self::new(open(directory, None, network).open().await?)
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
    pub fn root_signer(&self) -> Result<Arc<dyn rocky_sdk::IdentityProvider>, Failure> {
        Ok(Arc::new(rocky_sdk::RootKey::from_phrase(
            self.root.as_deref().ok_or("no root")?,
        )?))
    }
    async fn handle(&self, handle: u8) -> rocky_core::Commitment {
        let mut bytes = self
            .database
            .me()
            .await
            .expect("session")
            .device
            .to_string()
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
    pub async fn sync_now(&self) -> Result<rocky_sdk::SyncReport, rocky_sdk::Error> {
        match self.signer.clone() {
            Some(identity) => self.database.sync_now_with_identity(identity).await,
            None => self.database.sync_now().await,
        }
    }
    pub async fn sync(&self) -> Result<Value, Value> {
        snap::observe(self.sync_now()).await
    }
    pub async fn invite(&self) -> rocky_sdk::InvitationCode {
        self.invite_as(rocky_sdk::Access::Full).await
    }
    pub async fn invite_as(&self, access: rocky_sdk::Access) -> rocky_sdk::InvitationCode {
        self.database
            .invite(rocky_sdk::Invite::new(access))
            .await
            .expect("invitation")
            .code
    }
    pub async fn admit(&self, admin: &Device) {
        let identity = admin.database.me().await.expect("session").person.key();
        self.database
            .make_admin(rocky_sdk::PersonId::new(identity), self.root())
            .await
            .expect("make admin");
        for _ in 0..3 {
            admin
                .database
                .renew()
                .await
                .expect("renew into the admin grant");
            if admin
                .database
                .status()
                .await
                .expect("status")
                .authority
                .is_issuer
            {
                return;
            }
            let _ = self.database.sync_now().await;
        }
        panic!("renewal at an issuer that learned the grant installs it");
    }
    pub async fn contact(&self) -> String {
        self.database.contact().await.expect("contact").uri
    }
    pub async fn node(&self) -> String {
        self.database.status().await.expect("status").authority.node
    }
    pub async fn identity(&self) -> String {
        self.database
            .status()
            .await
            .expect("status")
            .authority
            .principal
    }
    pub async fn add_peer(&self, contact: &str) -> Result<Value, Value> {
        snap::observe(async {
            let value = self.database.add_peer(&(snap::text(&(contact)))).await?;
            Ok::<_, rocky_sdk::Error>(json!({"node":value}))
        })
        .await
    }
    pub async fn peer(&self, node: &str) -> Value {
        let peers = snap::ok(async {
            let network = self.database.status().await?.network;
            Ok::<_, rocky_sdk::Error>(json!({"peers":network.peers,"discovery":network.discovery}))
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
    pub async fn seal_and_compact(&self) -> Value {
        self.database.seal().await.expect("seal");
        serde_json::to_value(self.database.compact(0).await.expect("compact")).expect("report")
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

pub async fn confirmed(configuration: Configuration) -> Result<Database, Failure> {
    let error = configuration
        .open()
        .await
        .err()
        .ok_or("expected removal confirmation")?;
    let rocky_sdk::ErrorKind::RolloutConfirmation(required) = error.kind else {
        return Err(format!("expected removal confirmation, found {error}").into());
    };
    Ok(configuration
        .open_confirmed(&required.report.digest)
        .await?)
}

pub async fn link(
    config: Configuration,
    identity: Arc<dyn rocky_sdk::IdentityProvider>,
    contact: &str,
    responder: &Database,
) -> Result<Database, Failure> {
    let pending = config.identity(identity).link(contact).await?;
    let shown = responder
        .link_requests()
        .await?
        .into_iter()
        .find(|request| request.device == pending.device())
        .ok_or("missing link request")?;
    assert_eq!(&shown.code, pending.code());
    assert!(!shown.approved);
    responder.approve_link(shown.device, &shown.code).await?;
    Ok(pending.confirm().await?.database)
}
