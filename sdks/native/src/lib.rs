#[cfg(feature = "iroh")]
use rocky_sdk::{Network, transport::Transport};
use std::sync::Arc;

uniffi::setup_scaffolding!();

mod database;
#[cfg(feature = "iroh")]
mod endpoint;
mod events;
mod sql;
mod status;
pub use database::{
    NativeDatabase, NativeDatabaseSession, NativeDevelopmentLog, NativeNetwork,
    NativeNetworkService, prepare_device,
};
pub use events::{
    NativeEvent, NativeEventListener, NativeLevel, dropped_events, set_event_listener,
};
pub use sql::{
    NativeConnection, NativePage, NativeStatement, NativeTableChange, NativeTableEvents,
    NativeValue,
};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum NativeError {
    #[error("{detail}")]
    Failure { detail: String },
    #[error("{detail}")]
    Request {
        code: String,
        retryable: bool,
        detail: String,
    },
    /// A statement failed; `code` is SQLite's result code.
    #[error("Error code: {code}, message: {detail}")]
    Sql { code: i32, detail: String },
}
pub(crate) fn failure(error: Box<dyn std::error::Error + Send + Sync>) -> NativeError {
    if let Some(required) = error.downcast_ref::<rocky_sdk::RolloutConfirmationRequired>() {
        return match serde_json::to_string(&required.report) {
            Ok(detail) => NativeError::Request {
                code: if required.mismatch {
                    "confirmation_mismatch"
                } else {
                    "confirmation_required"
                }
                .into(),
                retryable: false,
                detail,
            },
            Err(error) => NativeError::Failure {
                detail: error.to_string(),
            },
        };
    }
    if let Some(service) = error.downcast_ref::<rocky_sdk::ServiceError>() {
        return NativeError::Request {
            code: service.code().into(),
            retryable: service.retryable(),
            detail: service.to_string(),
        };
    }
    match error.downcast::<rocky_sdk::CallFailure>() {
        Ok(error) => NativeError::Request {
            code: error.code,
            retryable: error.retryable,
            detail: error.detail,
        },
        Err(error) => NativeError::Failure {
            detail: error.to_string(),
        },
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

/// An identity the application supplies, such as a passkey's: its public
/// key, signatures and author numbers. It never hands out the private key.
/// `purpose` is `node_certificate`, `capability`, `revocation` or
/// `statement`.
#[uniffi::export(with_foreign)]
pub trait NativeIdentityProvider: Send + Sync {
    fn public_key(&self) -> Result<Vec<u8>, NativeError>;
    fn sign(&self, purpose: String, message: Vec<u8>) -> Result<Vec<u8>, NativeError>;
    /// An author number of `first..=last` never handed out before.
    fn reserve_author(&self, first: u32, last: u32) -> Result<u32, NativeError>;
}
struct ForeignIdentity(Arc<dyn NativeIdentityProvider>);
fn provider(error: NativeError) -> rocky_sdk::credentials::Error {
    rocky_sdk::credentials::Error::Provider(error.to_string().into())
}
impl rocky_sdk::IdentityProvider for ForeignIdentity {
    fn public_key(
        &self,
    ) -> Result<rocky_sdk::credentials::PublicKey, rocky_sdk::credentials::Error> {
        let bytes: [u8; 32] = self
            .0
            .public_key()
            .map_err(provider)?
            .try_into()
            .map_err(|_| rocky_sdk::credentials::Error::Denied("invalid identity key".into()))?;
        rocky_sdk::credentials::PublicKey::from_bytes(&bytes)
            .map_err(|_| rocky_sdk::credentials::Error::Denied("invalid identity key".into()))
    }
    fn sign(
        &self,
        purpose: rocky_sdk::Purpose,
        message: &[u8],
    ) -> Result<rocky_sdk::credentials::Signature, rocky_sdk::credentials::Error> {
        let purpose = match purpose {
            rocky_sdk::Purpose::NodeCertificate => "node_certificate",
            rocky_sdk::Purpose::Capability => "capability",
            rocky_sdk::Purpose::Revocation => "revocation",
            rocky_sdk::Purpose::Statement => "statement",
        };
        let bytes: [u8; 64] = self
            .0
            .sign(purpose.into(), message.to_vec())
            .map_err(provider)?
            .try_into()
            .map_err(|_| {
                rocky_sdk::credentials::Error::Denied("invalid identity signature".into())
            })?;
        Ok(rocky_sdk::credentials::Signature::from_bytes(&bytes))
    }
    fn reserve_author(
        &self,
        range: rocky_sdk::credentials::AuthorRange,
    ) -> Result<u32, rocky_sdk::credentials::Error> {
        self.0
            .reserve_author(range.first, range.last)
            .map_err(provider)
    }
}
pub(crate) fn identity(
    identity: Option<Arc<dyn NativeIdentityProvider>>,
) -> Option<Arc<dyn rocky_sdk::IdentityProvider>> {
    identity
        .map(|identity| Arc::new(ForeignIdentity(identity)) as Arc<dyn rocky_sdk::IdentityProvider>)
}
/// The file-backed identity in `directory`, made there when absent: for
/// tests and servers. Its key is plaintext at rest.
struct FileIdentity(rocky_sdk::FileIdentityProvider);
fn described(detail: impl std::fmt::Display) -> NativeError {
    NativeError::Failure {
        detail: detail.to_string(),
    }
}
impl NativeIdentityProvider for FileIdentity {
    fn public_key(&self) -> Result<Vec<u8>, NativeError> {
        use rocky_sdk::IdentityProvider;
        Ok(self.0.public_key().map_err(described)?.as_bytes().to_vec())
    }
    fn sign(&self, _: String, message: Vec<u8>) -> Result<Vec<u8>, NativeError> {
        use rocky_sdk::IdentityProvider;
        Ok(self
            .0
            .sign(rocky_sdk::Purpose::Statement, &message)
            .map_err(described)?
            .to_bytes()
            .to_vec())
    }
    fn reserve_author(&self, first: u32, last: u32) -> Result<u32, NativeError> {
        use rocky_sdk::IdentityProvider;
        let range = rocky_sdk::credentials::AuthorRange::new(first, last).map_err(described)?;
        self.0.reserve_author(range).map_err(described)
    }
}
#[uniffi::export]
pub fn file_identity(directory: String) -> Result<Arc<dyn NativeIdentityProvider>, NativeError> {
    Ok(Arc::new(FileIdentity(
        rocky_sdk::FileIdentityProvider::open(directory).map_err(described)?,
    )))
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

/// Composes only a supported current service bundle; no command adapters.
fn coordination(bundle: Option<String>) -> Result<Option<rocky_sdk::Coordination>, NativeError> {
    match bundle {
        None => Ok(None),
        #[cfg(feature = "iroh")]
        Some(bundle) => Ok(Some(rocky_sdk::Coordination::Service(Box::new(
            serde_json::from_str(&bundle).map_err(|error| NativeError::Failure {
                detail: error.to_string(),
            })?,
        )))),
        #[cfg(not(feature = "iroh"))]
        Some(_) => Err(NativeError::Failure {
            detail: "a coordination service needs iroh".into(),
        }),
    }
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
        coordinator: Option<String>,
    ) -> Result<Arc<Self>, NativeError> {
        let network = Some(Network::Borrowed(transport));
        let coordination = coordination(coordinator)?;
        Ok(Arc::new(Self {
            engine: rocky_sdk::Engine::open_composed(
                &options,
                &schema,
                foreign(secrets),
                network,
                coordination,
            )
            .map_err(failure)?,
        }))
    }
}
#[uniffi::export]
impl NativeSession {
    /// Without `secrets`, keys are owner-only files in the data directory.
    /// `coordinator` pins a metadata service by its trust bundle.
    #[uniffi::constructor]
    pub fn open(
        options: String,
        schema: String,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        coordinator: Option<String>,
    ) -> Result<Arc<Self>, NativeError> {
        let coordination = coordination(coordinator)?;
        Ok(Arc::new(Self {
            engine: rocky_sdk::Engine::open_composed(
                &options,
                &schema,
                foreign(secrets),
                None,
                coordination,
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
