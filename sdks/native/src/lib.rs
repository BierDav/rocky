#[cfg(feature = "iroh")]
use rocky_sdk::{Network, transport::Transport};
use std::sync::Arc;

uniffi::setup_scaffolding!();

mod commands;
mod database;
#[cfg(feature = "iroh")]
mod endpoint;
mod sql;
pub use commands::{
    DevelopmentCoordinator, NativeCommandHandler, NativeCommands, NativeHandlerReads,
};
pub use database::{
    NativeDatabase, NativeDatabaseSession, NativeNetwork, NativeNetworkService, prepare_device,
};
pub use sql::{
    NativeConnection, NativePage, NativeStatement, NativeTableChange, NativeTableEvents,
    NativeValue,
};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum NativeError {
    #[error("{detail}")]
    Failure { detail: String },
    /// A statement failed; `code` is SQLite's result code.
    #[error("Error code: {code}, message: {detail}")]
    Sql { code: i32, detail: String },
}
pub(crate) fn failure(error: Box<dyn std::error::Error + Send + Sync>) -> NativeError {
    NativeError::Failure {
        detail: error.to_string(),
    }
}
impl From<uniffi::UnexpectedUniFFICallbackError> for NativeError {
    fn from(error: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Failure {
            detail: error.reason,
        }
    }
}

/// Platform secret storage implemented by the foreign application.
#[uniffi::export(with_foreign)]
pub trait NativeSecretStore: Send + Sync {
    fn load(&self, name: String) -> Result<Option<Vec<u8>>, NativeError>;
    fn store(&self, name: String, secret: Vec<u8>) -> Result<(), NativeError>;
    fn delete(&self, name: String) -> Result<(), NativeError>;
}
struct Foreign(Arc<dyn NativeSecretStore>);
type Failure = Box<dyn std::error::Error + Send + Sync>;
impl rocky_sdk::SecretStore for Foreign {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, Failure> {
        Ok(self.0.load(name.into())?)
    }
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), Failure> {
        Ok(self.0.store(name.into(), secret.to_vec())?)
    }
    fn delete(&self, name: &str) -> Result<(), Failure> {
        Ok(self.0.delete(name.into())?)
    }
}

/// One reliable ordered byte stream, as chunks: an application protocol's
/// stream on a `NativeEndpoint`.
#[uniffi::export(with_foreign)]
#[async_trait::async_trait]
pub trait ByteStream: Send + Sync {
    /// The next bytes, at most `max`. Empty once the peer finished.
    async fn read(&self, max: u32) -> Result<Vec<u8>, NativeError>;
    /// Delivers `bytes` in order; returns once they are accepted.
    async fn write(&self, bytes: Vec<u8>) -> Result<(), NativeError>;
    /// No more writes follow: the peer's `read` ends after what was sent.
    async fn finish(&self) -> Result<(), NativeError>;
    /// Abandons the stream; a pending `read` ends.
    async fn abandon(&self);
}
pub(crate) fn foreign(
    secrets: Option<Arc<dyn NativeSecretStore>>,
) -> Option<Arc<dyn rocky_sdk::SecretStore>> {
    secrets.map(|store| Arc::new(Foreign(store)) as Arc<dyn rocky_sdk::SecretStore>)
}

#[derive(uniffi::Object)]
pub struct NativeSession {
    engine: Arc<rocky_sdk::Engine>,
}
impl NativeSession {
    /// Opens on a network the application keeps.
    #[cfg(feature = "iroh")]
    fn open_on(
        options: String,
        schema: String,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        transport: Arc<dyn Transport>,
        commands: Option<NativeCommands>,
    ) -> Result<Arc<Self>, NativeError> {
        let network = Some(Network::Borrowed(transport));
        let commands = commands.map(NativeCommands::compose).transpose()?;
        Ok(Arc::new(Self {
            engine: rocky_sdk::Engine::open_composed(
                &options,
                &schema,
                foreign(secrets),
                network,
                commands,
            )
            .map_err(failure)?,
        }))
    }
}
#[uniffi::export]
impl NativeSession {
    /// Without `secrets`, keys are owner-only files in the data directory.
    /// `commands` composes the coordinator and handler of the schema's
    /// strong commands.
    #[uniffi::constructor]
    pub fn open(
        options: String,
        schema: String,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        commands: Option<NativeCommands>,
    ) -> Result<Arc<Self>, NativeError> {
        let commands = commands.map(NativeCommands::compose).transpose()?;
        Ok(Arc::new(Self {
            engine: rocky_sdk::Engine::open_composed(
                &options,
                &schema,
                foreign(secrets),
                None,
                commands,
            )
            .map_err(failure)?,
        }))
    }
    pub fn scope(&self) -> String {
        self.engine.scope()
    }
    pub async fn call(&self, request: String) -> Result<String, NativeError> {
        self.engine.call(request).await.map_err(failure)
    }
    pub async fn shutdown(&self) -> Result<(), NativeError> {
        self.engine.close().await.map_err(failure)
    }
}
