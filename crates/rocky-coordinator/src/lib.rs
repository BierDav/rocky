//! Generic metadata coordination, its signed protocol and clients.
pub use retention::Limits;
use rocky_core::*;

/// Retained state of one coordinator, by count and accounted bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    pub domains: usize,
    pub bytes: usize,
}

/// Certified bootstrap and service configuration.
pub mod certified;
mod client;
pub mod control;
mod machine;
mod retention;
pub mod service;
mod transport;

/// Generic metadata state applied by a consensus host.
pub mod state {
    pub use crate::machine::Metadata;
}

/// Signed messages and framing shared by clients and voters.
pub mod protocol {
    pub use crate::machine::Request;
    pub use crate::service::Reply;
    #[cfg(not(target_family = "wasm"))]
    pub use crate::transport::exchange;
    pub use crate::transport::{Envelope, WireMessage as Message, converse, nonce, read, write};
}

/// Ordered metadata for coordinated ordinary writes.
pub mod coordinated;
/// Named counters with idempotent claims.
pub mod counters;

#[cfg(all(test, not(target_family = "wasm")))]
mod native_tests;

/// Reaches the voters a database's signed settings name, through the
/// application's transport, once their certified genesis verifies against
/// the settings' identity.
pub struct Connector {
    transport: std::sync::Arc<dyn rocky_core::net::Transport>,
}
impl Connector {
    pub fn new(transport: std::sync::Arc<dyn rocky_core::net::Transport>) -> Self {
        Self { transport }
    }
}
impl rocky_credentials::CoordinatorConnector for Connector {
    fn connect(
        &self,
        bundle: &rocky_credentials::CoordinatorBundle,
    ) -> Result<std::sync::Arc<dyn rocky_credentials::Coordinator>, rocky_core::CoordinationError>
    {
        let genesis = certified::CertifiedGenesis::from_bytes(&bundle.certified)?;
        let root = rocky_credentials::PublicKey::from_bytes(&bundle.identity.root)
            .map_err(|_| rocky_core::CoordinationError::Denied)?;
        genesis.verify(root, bundle.identity.mesh)?;
        if genesis.id()? != bundle.identity.genesis {
            return Err(rocky_core::CoordinationError::Denied);
        }
        Ok(std::sync::Arc::new(service::NetworkCoordinator::connect(
            &certified::TrustBundle::new(&genesis)?,
            rocky_credentials::PrivateKey::new(),
            self.transport.clone(),
        )?))
    }
}
/// Application-owned voter client; Rocky binds an independent node session at open.
pub struct Client;
impl Client {
    pub fn connect(
        bundle: &certified::TrustBundle,
        transport: std::sync::Arc<dyn rocky_core::net::Transport>,
    ) -> Result<std::sync::Arc<dyn rocky_credentials::Coordinator>, rocky_core::CoordinationError>
    {
        Ok(std::sync::Arc::new(service::NetworkCoordinator::connect(
            bundle,
            rocky_credentials::PrivateKey::new(),
            transport,
        )?))
    }
}
