use crate::SqlValue;
use crate::{
    Database, Failure, Network, Open, SecretStore, Session, database::Install,
    enrollment::DEFAULT_TTL, node::Node, transport::memory::Hub,
};
use rocky_credentials::testing::MemoryKeys;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}

pub(super) const SCHEMA: &str = r#"{"application":"simulation","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;

pub(super) struct Running {
    pub(super) database: Arc<Database>,
    session: Session,
    pub(super) node: Weak<Node>,
}
pub(super) struct Member {
    pub(super) name: String,
    pub(super) directory: PathBuf,
    keys: Arc<MemoryKeys>,
    pub(super) running: Option<Running>,
    pub(super) node_key: Option<String>,
    pub(super) revoked: bool,
    schema: &'static str,
    pub(super) ahead: Arc<std::sync::atomic::AtomicU64>,
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
    pub(super) async fn start(&mut self, hub: &Arc<Hub>, install: Install) -> Result<(), Failure> {
        let secrets: Arc<dyn SecretStore> = self.keys.clone();
        let schema = crate::Schema::parse(self.schema)?;
        let hosted = Database::host(
            Open {
                directory: &self.directory,
                schema: &schema,
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
    pub(super) async fn close(&mut self) -> Result<(), Failure> {
        if let Some(running) = self.running.take() {
            running.session.close().await;
            running.database.close().await?;
        }
        Ok(())
    }
    pub(super) async fn crash(&mut self) {
        if let Some(running) = self.running.take() {
            running.database.crash().await;
        }
    }
    pub(super) fn database(&self) -> Result<&Database, Failure> {
        Ok(&self
            .running
            .as_ref()
            .ok_or("the node is not running")?
            .database)
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
    pub(super) async fn write(&self, key: &str, handle: u64) -> Result<(), Failure> {
        let rows = ["a", "b"].map(|half| vec![text(&format!("{key}-{half}")), text(key)]);
        self.insert("items", "key, title", &rows, handle, 0).await
    }
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
    async fn insert(
        &self,
        table: &str,
        columns: &str,
        rows: &[Vec<SqlValue>],
        _handle: u64,
        _kind: u8,
    ) -> Result<(), Failure> {
        let mut tx = self.database()?.transaction().await?;
        for values in rows {
            let id = tx.next_id(table, Vec::new()).await?;
            let mut values = values.clone();
            values.push(id.sql());
            let places = vec!["?"; values.len()].join(", ");
            tx.execute(
                &format!("INSERT INTO {table}({columns}, id) VALUES({places})"),
                values,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub(super) async fn keys(&self) -> Result<Vec<String>, Failure> {
        let rows = self
            .database()?
            .query("SELECT key FROM items ORDER BY key", Vec::new())
            .await?
            .collect()
            .await?;
        let mut keys = Vec::with_capacity(rows.len());
        for row in rows {
            keys.push(row.get_at(0)?);
        }
        Ok(keys)
    }
    pub(super) async fn contact(&self) -> Result<String, Failure> {
        self.session()?.contact().await
    }
}
