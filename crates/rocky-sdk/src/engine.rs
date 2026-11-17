use crate::{
    Failure,
    database::{Database, NetworkService, Open, Opened, Session},
    secrets::SecretStore,
    service::Network,
};
use std::{path::PathBuf, sync::Arc};

pub enum EngineMode {
    Create {
        credential_ttl: Option<u64>,
        root: Option<Arc<dyn crate::IdentityProvider>>,
    },
    Join {
        invitation: String,
    },
    Resume,
}
pub struct EngineOptions {
    pub directory: PathBuf,
    pub transport: Arc<dyn rocky_core::net::Connector>,
    pub mode: EngineMode,
}

pub struct Engine {
    database: Arc<Database>,
    session: Session,
    network: Option<NetworkService>,
    root: std::sync::Mutex<Option<crate::RootKey>>,
}
impl Engine {
    pub fn open(
        options: EngineOptions,
        schema: &crate::Schema,
        secrets: Option<Arc<dyn SecretStore>>,
    ) -> Result<Arc<Self>, Failure> {
        Self::open_with(options, schema, secrets, None)
    }
    pub fn open_with(
        options: EngineOptions,
        schema: &crate::Schema,
        secrets: Option<Arc<dyn SecretStore>>,
        network: Option<Network>,
    ) -> Result<Arc<Self>, Failure> {
        Self::open_composed(options, schema, secrets, network, None)
    }
    pub fn open_composed(
        options: EngineOptions,
        schema: &crate::Schema,
        secrets: Option<Arc<dyn SecretStore>>,
        network: Option<Network>,
        coordination: Option<crate::CoordinatorSlot>,
    ) -> Result<Arc<Self>, Failure> {
        let network = network.unwrap_or_else(|| Network::Owned(options.transport.clone()));
        let open = Open {
            identity: None,
            directory: &options.directory,
            schema: &serde_json::to_string(schema)?,
            secrets,
            network: Some(network),
            coordination,
        };
        let Opened {
            database,
            network,
            root,
            ..
        } = match options.mode {
            EngineMode::Create {
                credential_ttl,
                root: Some(root),
            } => Database::create_with_root(open, credential_ttl, root)?,
            EngineMode::Create {
                credential_ttl,
                root: None,
            } => Database::create(open, credential_ttl)?,
            EngineMode::Join { invitation } => Database::join(open, &invitation)?,
            EngineMode::Resume => Database::open(open)?,
        };
        Ok(Arc::new(Self {
            session: database.session(),
            database,
            network,
            root: std::sync::Mutex::new(root),
        }))
    }
    pub fn scope(&self) -> crate::Scope {
        self.database.scope()
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn database(&self) -> &Arc<Database> {
        &self.database
    }
    pub fn take_root(&self) -> Option<crate::RootKey> {
        self.root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
    pub async fn close(&self) -> Result<(), Failure> {
        if let Some(network) = &self.network {
            network.close().await;
        }
        self.database.close().await
    }
}
