//! The composable services as separate native handles: a database, its
//! sessions and an optional network service, each closed on its own. The
//! starter `NativeSession` composes the same services. See
//! `docs/runtime/sdk-composition.md`.
use crate::{
    Adapter, ForeignTransport, NativeCommands, NativeError, NativeSecretStore, failure, foreign,
};
use rocky_sdk::{Network, transport::adapter::adapt};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn message(detail: impl std::fmt::Display) -> NativeError {
    NativeError::Failure {
        detail: detail.to_string(),
    }
}
fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A network a database attaches, used once: the starter's own transport,
/// which Rocky starts and closes, or one the application owns.
#[derive(uniffi::Object)]
pub struct NativeNetwork {
    network: Mutex<Option<Network>>,
}
impl NativeNetwork {
    pub(crate) fn new(network: Network) -> Arc<Self> {
        Arc::new(Self {
            network: Mutex::new(Some(network)),
        })
    }
    fn take(&self) -> Result<Network, NativeError> {
        locked(&self.network)
            .take()
            .ok_or_else(|| message("this network was attached already; make another"))
    }
}
#[uniffi::export]
impl NativeNetwork {
    /// The starter's transport `options` name, such as
    /// `{"kind":"iroh"}` or `{"kind":"tcp","listen":"127.0.0.1:0"}`.
    #[uniffi::constructor]
    pub fn starter(options: String) -> Result<Arc<Self>, NativeError> {
        let options: rocky_sdk::transport::TransportOptions =
            serde_json::from_str(&options).map_err(message)?;
        Ok(Self::new(Network::Owned(Arc::new(options))))
    }
    /// A transport the application implements, starts and closes.
    #[uniffi::constructor]
    pub fn foreign(transport: Arc<dyn ForeignTransport>) -> Arc<Self> {
        Self::new(Network::Borrowed(adapt(Arc::new(Adapter(transport)))))
    }
}

/// The network service attached to a database.
#[derive(uniffi::Object)]
pub struct NativeNetworkService {
    service: rocky_sdk::NetworkService,
}
#[uniffi::export]
impl NativeNetworkService {
    /// Stops serving and syncing; a transport the service started is
    /// closed, one the application owns is not. The database stays open.
    pub async fn shutdown(&self) {
        self.service.close().await;
    }
}

/// One caller's view of a database. Sessions are independent: closing one
/// ends its waits and subscriptions only.
#[derive(uniffi::Object)]
pub struct NativeDatabaseSession {
    session: rocky_sdk::Session,
}
#[uniffi::export]
impl NativeDatabaseSession {
    /// Versioned SDK calls, including raw grants, delegation, executor
    /// registration and the `pseudonym_*` credential primitives.
    pub async fn call(&self, request: String) -> Result<String, NativeError> {
        self.session.call(request).await.map_err(failure)
    }
    pub async fn shutdown(&self) {
        self.session.close().await;
    }
}

/// One secure database. It owns its file and reactor thread, and no
/// network unless one is attached. Coordinators and borrowed transports
/// stay the application's.
#[derive(uniffi::Object)]
pub struct NativeDatabase {
    database: Arc<rocky_sdk::Database>,
    /// The service of the network the database opened with.
    opened: Mutex<Option<rocky_sdk::NetworkService>>,
}
struct Parts<'a> {
    directory: &'a str,
    schema: &'a str,
    secrets: Option<Arc<dyn NativeSecretStore>>,
    network: Option<Arc<NativeNetwork>>,
    commands: Option<NativeCommands>,
}
impl<'a> Parts<'a> {
    fn open(self) -> Result<rocky_sdk::Open<'a>, NativeError> {
        Ok(rocky_sdk::Open {
            directory: Path::new(self.directory),
            schema: self.schema,
            secrets: foreign(self.secrets),
            network: self.network.map(|network| network.take()).transpose()?,
            commands: self.commands.map(NativeCommands::compose).transpose()?,
        })
    }
}
impl NativeDatabase {
    pub(crate) fn database(&self) -> &rocky_sdk::Database {
        &self.database
    }
    fn wrap(
        opened: Result<rocky_sdk::Opened, rocky_sdk::Failure>,
    ) -> Result<Arc<Self>, NativeError> {
        let opened = opened.map_err(failure)?;
        Ok(Arc::new(Self {
            database: opened.database,
            opened: Mutex::new(opened.network),
        }))
    }
}
#[uniffi::export]
impl NativeDatabase {
    /// Creates a domain in an empty directory; this device is its first
    /// issuer. Credentials last `credential_ttl` seconds, 30 days by default.
    #[uniffi::constructor]
    pub fn create(
        directory: String,
        schema: String,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        network: Option<Arc<NativeNetwork>>,
        commands: Option<NativeCommands>,
        credential_ttl: Option<u64>,
    ) -> Result<Arc<Self>, NativeError> {
        let open = Parts {
            directory: &directory,
            schema: &schema,
            secrets,
            network,
            commands,
        }
        .open()?;
        Self::wrap(rocky_sdk::Database::create(open, credential_ttl))
    }
    /// Installs credentials and reader keys from an independent issuer/provider.
    #[uniffi::constructor]
    pub fn install(
        directory: String,
        schema: String,
        supplied: String,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        network: Option<Arc<NativeNetwork>>,
        commands: Option<NativeCommands>,
    ) -> Result<Arc<Self>, NativeError> {
        if supplied.len() > 1024 * 1024 {
            return Err(message("credential frame quota"));
        }
        let supplied = serde_json::from_str(&supplied).map_err(message)?;
        let open = Parts {
            directory: &directory,
            schema: &schema,
            secrets,
            network,
            commands,
        }
        .open()?;
        Self::wrap(rocky_sdk::Database::install(open, supplied))
    }
    /// Opens the domain installed in the directory, moving it to the
    /// schema's generation first when that changed.
    #[uniffi::constructor]
    pub fn open(
        directory: String,
        schema: String,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        network: Option<Arc<NativeNetwork>>,
        commands: Option<NativeCommands>,
    ) -> Result<Arc<Self>, NativeError> {
        let open = Parts {
            directory: &directory,
            schema: &schema,
            secrets,
            network,
            commands,
        }
        .open()?;
        Self::wrap(rocky_sdk::Database::open(open))
    }
    /// Enrolls with the issuer `invitation` names, over `network`, which
    /// stays attached.
    #[uniffi::constructor]
    pub fn join(
        directory: String,
        schema: String,
        invitation: String,
        network: Arc<NativeNetwork>,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        commands: Option<NativeCommands>,
    ) -> Result<Arc<Self>, NativeError> {
        let open = Parts {
            directory: &directory,
            schema: &schema,
            secrets,
            network: Some(network),
            commands,
        }
        .open()?;
        Self::wrap(rocky_sdk::Database::join(open, &invitation))
    }
    /// Restores a backup archive under a fresh identity `invitation`
    /// enrolls over `network`.
    #[uniffi::constructor]
    pub fn restore(
        directory: String,
        schema: String,
        archive: String,
        invitation: String,
        network: Arc<NativeNetwork>,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        commands: Option<NativeCommands>,
    ) -> Result<Arc<Self>, NativeError> {
        let open = Parts {
            directory: &directory,
            schema: &schema,
            secrets,
            network: Some(network),
            commands,
        }
        .open()?;
        Self::wrap(rocky_sdk::Database::restore(
            open,
            Path::new(&archive),
            &invitation,
        ))
    }
    /// The domain's scope as the call contract spells it.
    pub fn scope(&self) -> String {
        self.database.scope()
    }
    pub fn session(&self) -> Arc<NativeDatabaseSession> {
        Arc::new(NativeDatabaseSession {
            session: self.database.session(),
        })
    }
    /// The service of the network the database opened with, once.
    pub fn opened_network(&self) -> Option<Arc<NativeNetworkService>> {
        locked(&self.opened)
            .take()
            .map(|service| Arc::new(NativeNetworkService { service }))
    }
    /// Attaches `network`; one can be attached at a time.
    pub async fn attach(
        &self,
        network: Arc<NativeNetwork>,
    ) -> Result<Arc<NativeNetworkService>, NativeError> {
        let service = self
            .database
            .attach(network.take()?)
            .await
            .map_err(failure)?;
        Ok(Arc::new(NativeNetworkService { service }))
    }
    /// Writes a signed backup archive to `path` and returns its inventory.
    pub async fn backup(&self, path: String) -> Result<String, NativeError> {
        self.database
            .backup(Path::new(&path))
            .await
            .map_err(failure)
    }
    /// Ends every session, detaches the network service, flushes and
    /// closes the file. Closing again does nothing.
    pub async fn shutdown(&self) -> Result<(), NativeError> {
        self.database.close().await.map_err(failure)
    }
}

/// Prepares fresh signing keys without opening a database or contacting a network.
#[uniffi::export]
pub fn prepare_device(
    directory: String,
    secrets: Option<Arc<dyn NativeSecretStore>>,
) -> Result<String, NativeError> {
    let keys =
        rocky_sdk::Database::device(Path::new(&directory), foreign(secrets)).map_err(failure)?;
    serde_json::to_string(&keys).map_err(message)
}
