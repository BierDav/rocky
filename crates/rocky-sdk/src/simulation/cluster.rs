//! The simulated nodes: databases hosted on the simulation's thread, each
//! with its directory, its secrets and its name on the hub.
use crate::SqlValue;
use crate::{
    Database, Failure, Network, Open, SecretStore, Session, database::Install,
    enrollment::DEFAULT_TTL, node::Node, transport::memory::Hub,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError, Weak},
};

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}

pub(super) const SCHEMA: &str = r#"{"application":"simulation","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;

/// Keys in memory, kept across restarts and crashes of one node.
#[derive(Default)]
pub(super) struct Keys(Mutex<BTreeMap<String, Vec<u8>>>);
impl SecretStore for Keys {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, Failure> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned())
    }
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), Failure> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.into(), secret.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), Failure> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        Ok(())
    }
}

/// An open node.
pub(super) struct Running {
    pub(super) database: Arc<Database>,
    session: Session,
    pub(super) node: Weak<Node>,
}
/// One simulated device, open or not.
pub(super) struct Member {
    pub(super) name: String,
    pub(super) directory: PathBuf,
    keys: Arc<Keys>,
    pub(super) running: Option<Running>,
    /// Its node key, once known.
    pub(super) node_key: Option<String>,
    /// Revoked members never open again.
    pub(super) revoked: bool,
    schema: &'static str,
    /// Seconds this device's wall clock runs ahead, across restarts.
    pub(super) ahead: Arc<std::sync::atomic::AtomicU64>,
    /// The offline root of the domain this member created: its user holds it.
    pub(super) root: Option<Arc<crate::RootKey>>,
    pending_join: Option<crate::AwaitingAdmission>,
}

impl Member {
    pub(super) fn new(root: &Path, name: &str, schema: &'static str) -> Self {
        Self {
            schema,
            name: name.into(),
            directory: root.join(name),
            keys: Arc::default(),
            running: None,
            node_key: None,
            revoked: false,
            ahead: Arc::default(),
            root: None,
            pending_join: None,
        }
    }
    /// Starts the node: creating, joining or opening its directory.
    pub(super) async fn start(&mut self, hub: &Arc<Hub>, install: Install) -> Result<(), Failure> {
        let secrets: Arc<dyn SecretStore> = self.keys.clone();
        let hosted = Database::host(
            Open {
                directory: &self.directory,
                schema: self.schema,
                secrets: Some(secrets),
                identity: self
                    .pending_join
                    .as_ref()
                    .map(|pending| pending.identity.clone()),
                network: Some(Network::Owned(hub.connector(&self.name))),
                coordination: None,
            },
            install,
            self.ahead.clone(),
        )
        .await;
        let hosted = match hosted {
            Ok(hosted) => hosted,
            Err(error) => {
                if let Some(pending) = error.downcast_ref::<crate::AwaitingAdmission>() {
                    self.pending_join = Some(pending.clone());
                }
                return Err(error);
            }
        };
        self.pending_join = None;
        let database = hosted.opened.database;
        self.running = Some(Running {
            session: database.session(),
            database,
            node: hosted.node,
        });
        if self.node_key.is_none() {
            self.node_key = Some(self.session()?.info().await?.node.to_hex());
        }
        Ok(())
    }
    pub(super) async fn create(&mut self, hub: &Arc<Hub>) -> Result<(), Failure> {
        let root = Arc::new(crate::RootKey::generate());
        self.root = Some(root.clone());
        self.start(
            hub,
            Install::Create {
                ttl: DEFAULT_TTL,
                root: std::sync::Mutex::new(Some(root)),
                bookmark: None,
                personal: None,
            },
        )
        .await
    }
    pub(super) async fn join(&mut self, hub: &Arc<Hub>, invitation: &str) -> Result<(), Failure> {
        self.start(
            hub,
            Install::Join {
                invitation: invitation.into(),
                name: self.name.clone(),
            },
        )
        .await
    }
    pub(super) async fn open(&mut self, hub: &Arc<Hub>) -> Result<(), Failure> {
        self.start(hub, Install::Installed).await
    }
    /// Closes the node as an application does.
    pub(super) async fn close(&mut self) -> Result<(), Failure> {
        if let Some(running) = self.running.take() {
            running.session.close().await;
            running.database.close().await?;
        }
        Ok(())
    }
    /// Stops the node as a process death does.
    pub(super) async fn crash(&mut self) {
        if let Some(running) = self.running.take() {
            running.database.crash().await;
        }
    }
    pub(super) fn session(&self) -> Result<&Session, Failure> {
        Ok(&self
            .running
            .as_ref()
            .ok_or("the node is not running")?
            .session)
    }
    pub(super) async fn sync(&self) -> Result<crate::ReconciliationReport, Failure> {
        match &self.root {
            Some(root) => {
                self.session()?
                    .sync_once_with_identity(Arc::new(root.identity()))
                    .await
            }
            None => self.session()?.sync_once().await,
        }
    }
    pub(super) async fn follow(&self, name: &str) -> Result<crate::TipStatus, Failure> {
        match &self.root {
            Some(root) => {
                self.session()?
                    .follow_with_identity(name, Arc::new(root.identity()))
                    .await
            }
            None => self.session()?.follow(name).await,
        }
    }
    /// One transaction of two rows, `key-a` and `key-b`, under a handle
    /// that makes a retry safe.
    pub(super) async fn write(&self, key: &str, handle: u64) -> Result<(), Failure> {
        let rows = ["a", "b"].map(|half| vec![text(&format!("{key}-{half}")), text(key)]);
        self.insert("items", "key, title", &rows, handle, 0).await
    }
    /// One row of the partition `list`.
    pub(super) async fn write_part(
        &self,
        list: &str,
        key: &str,
        handle: u64,
    ) -> Result<(), Failure> {
        self.insert(
            "parts",
            "list, key",
            &[vec![text(list), text(key)]],
            handle,
            1,
        )
        .await
    }
    /// Inserts `rows` of `table` in one transaction; `kind` keeps the
    /// handles of different writes of one step apart.
    async fn insert(
        &self,
        table: &str,
        columns: &str,
        rows: &[Vec<SqlValue>],
        handle: u64,
        kind: u8,
    ) -> Result<(), Failure> {
        self.session()?.execute("BEGIN", Vec::new()).await?;
        let result = async {
            for values in rows {
                let id = self.session()?.next_id(table).await?;
                let mut values = values.clone();
                values.push(id.sql());
                let places = vec!["?"; values.len()].join(", ");
                self.session()?
                    .execute(
                        &format!("INSERT INTO {table}({columns}, id) VALUES({places})"),
                        values,
                    )
                    .await?;
            }
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&handle.to_be_bytes());
            bytes[8] = kind;
            self.session()?.commit(bytes).await
        }
        .await;
        if result.is_err() {
            let _ = self.session()?.execute("ROLLBACK", Vec::new()).await;
        }
        result.map(drop)
    }
    /// Every item key, sorted.
    pub(super) async fn keys(&self) -> Result<Vec<String>, Failure> {
        let mut keys = Vec::new();
        let mut after = String::new();
        loop {
            let mut query =
                crate::ReadQuery::new("SELECT key FROM items WHERE key > ? ORDER BY key", 512);
            query.values = vec![SqlValue::Text(after)];
            let page = self.session()?.read(query).await?;
            let rows = &page.rows;
            for row in rows {
                keys.push(row[0].to_text().ok_or("key")?);
            }
            match keys.last() {
                Some(last) if rows.len() == 512 => after = last.clone(),
                _ => return Ok(keys),
            }
        }
    }
    pub(super) async fn contact(&self) -> Result<String, Failure> {
        self.session()?.contact().await
    }
}
