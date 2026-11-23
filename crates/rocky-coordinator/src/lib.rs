pub use retention::Limits;
use rocky_core::*;
use rocky_credentials::CoordinationError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    pub domains: usize,
    pub bytes: usize,
}

pub mod certified;
mod client;
pub mod control;
mod machine;
mod retention;
pub mod service;
mod transport;

pub mod state {
    pub use crate::machine::Metadata;
}

pub mod protocol {
    pub use crate::machine::Request;
    pub use crate::service::Reply;
    #[cfg(not(target_family = "wasm"))]
    pub use crate::transport::exchange;
    pub use crate::transport::{
        Envelope, MAX_CLIENT_FRAME, WireMessage as Message, converse, nonce, read, read_bounded,
        write,
    };
}

pub mod counters;

#[cfg(all(test, not(target_family = "wasm")))]
mod native_tests;

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
    ) -> Result<
        std::sync::Arc<dyn rocky_credentials::Coordinator>,
        rocky_credentials::CoordinationError,
    > {
        let genesis = certified::CertifiedGenesis::from_bytes(&bundle.certified)?;
        let root = rocky_credentials::PublicKey::from_bytes(&bundle.identity.root)
            .map_err(|_| rocky_credentials::CoordinationError::Denied)?;
        genesis.verify(root, bundle.identity.mesh)?;
        if genesis.id()? != bundle.identity.genesis {
            return Err(rocky_credentials::CoordinationError::Denied);
        }
        Ok(std::sync::Arc::new(service::NetworkCoordinator::connect(
            &certified::TrustBundle::new(&genesis)?,
            rocky_credentials::PrivateKey::new(),
            self.transport.clone(),
        )?))
    }
}
pub struct Client;
impl Client {
    pub fn connect(
        bundle: &certified::TrustBundle,
        transport: std::sync::Arc<dyn rocky_core::net::Transport>,
    ) -> Result<
        std::sync::Arc<dyn rocky_credentials::Coordinator>,
        rocky_credentials::CoordinationError,
    > {
        Ok(std::sync::Arc::new(service::NetworkCoordinator::connect(
            bundle,
            rocky_credentials::PrivateKey::new(),
            transport,
        )?))
    }
}
