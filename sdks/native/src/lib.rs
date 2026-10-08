use rocky_sdk::{
    Network,
    transport::{
        BoxFuture, Transport,
        adapter::{ChunkStream, ChunkTransport, Inbound, adapt},
    },
};
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

/// One reliable ordered byte stream, as chunks. A foreign transport
/// implements it for its streams; `NativeEndpoint` returns its own.
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
#[derive(uniffi::Record)]
pub struct InboundStream {
    /// The versioned protocol name the dialer gave.
    pub protocol: String,
    pub stream: Arc<dyn ByteStream>,
}
/// What `connect` returns. A record, because an async callback cannot
/// return the stream object itself.
#[derive(uniffi::Record)]
pub struct OutboundStream {
    pub stream: Arc<dyn ByteStream>,
}
/// A transport the foreign application implements, starts and closes.
/// Streams must buffer 64 KiB per direction or forward without waiting
/// for the peer to read.
#[uniffi::export(with_foreign)]
#[async_trait::async_trait]
pub trait ForeignTransport: Send + Sync {
    /// Names the address format; routes of another kind are not dialed.
    fn kind(&self) -> String;
    /// Where peers reach this node.
    async fn address(&self) -> Result<String, NativeError>;
    async fn connect(
        &self,
        address: String,
        protocol: String,
    ) -> Result<OutboundStream, NativeError>;
    /// The next inbound stream; `None` once the transport closed.
    async fn accept(&self) -> Result<Option<InboundStream>, NativeError>;
}
struct Chunks(Arc<dyn ByteStream>);
impl ChunkStream for Chunks {
    fn read(&self, max: u32) -> BoxFuture<'_, Result<Vec<u8>, Failure>> {
        Box::pin(async move { Ok(self.0.read(max).await?) })
    }
    fn write(&self, bytes: Vec<u8>) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move { Ok(self.0.write(bytes).await?) })
    }
    fn finish(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move { Ok(self.0.finish().await?) })
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        self.0.abandon()
    }
}
pub(crate) struct Adapter(pub(crate) Arc<dyn ForeignTransport>);
impl ChunkTransport for Adapter {
    fn kind(&self) -> String {
        self.0.kind()
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async move { Ok(self.0.address().await?) })
    }
    fn connect(
        &self,
        address: String,
        protocol: String,
    ) -> BoxFuture<'_, Result<Arc<dyn ChunkStream>, Failure>> {
        Box::pin(async move {
            let connected = self.0.connect(address, protocol).await?;
            Ok(Arc::new(Chunks(connected.stream)) as Arc<dyn ChunkStream>)
        })
    }
    fn accept(&self) -> BoxFuture<'_, Result<Option<Inbound>, Failure>> {
        Box::pin(async move {
            Ok(self.0.accept().await?.map(|inbound| {
                let stream = Arc::new(Chunks(inbound.stream)) as Arc<dyn ChunkStream>;
                (inbound.protocol, stream)
            }))
        })
    }
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
    /// Opens over a transport the application owns instead of the one
    /// `options` name. Closing the session leaves that transport open.
    #[uniffi::constructor]
    pub fn open_with_transport(
        options: String,
        schema: String,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        transport: Arc<dyn ForeignTransport>,
        commands: Option<NativeCommands>,
    ) -> Result<Arc<Self>, NativeError> {
        Self::open_on(
            options,
            schema,
            secrets,
            adapt(Arc::new(Adapter(transport))),
            commands,
        )
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
