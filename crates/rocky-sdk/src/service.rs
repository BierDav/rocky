//! What composed services own, and the structured refusals of combinations
//! that cannot work. See `docs/runtime/sdk-composition.md`.
use crate::{
    Failure,
    transport::{Connector, Transport},
};
use std::sync::Arc;

/// The network a service uses and who owns it.
pub enum Network {
    /// Rocky starts the transport and closes it with the service.
    Owned(Arc<dyn Connector>),
    /// A transport the application started. Rocky never closes it.
    Borrowed(Arc<dyn Transport>),
}
impl Network {
    /// The transport and whether the service owns it.
    pub(crate) async fn start(
        self,
        accepts: bool,
        secret: [u8; 32],
    ) -> Result<(Arc<dyn Transport>, bool), Failure> {
        Ok(match self {
            Self::Owned(connector) => (connector.start(accepts, secret).await?, true),
            Self::Borrowed(transport) => (transport, false),
        })
    }
}

/// A combination of services that cannot work, refused before side effects,
/// or a call on something already closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceError {
    /// The operation needs a network service and none is attached.
    NetworkRequired(&'static str),
    /// One network service is attached already; close it first.
    NetworkAttached,
    /// No route of the attached network's kind is known for `peer`.
    NoRoute { peer: &'static str, network: String },
    /// No known route answered; a later attempt may succeed.
    Unreachable(String),
    /// A member's credentials expired: open with a network to renew them.
    CredentialsExpired,
    /// The named handle was closed.
    Closed(&'static str),
}
impl ServiceError {
    /// The stable code of the call contract.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NetworkRequired(_) | Self::CredentialsExpired => "network_required",
            Self::NetworkAttached | Self::NoRoute { .. } => "rejected",
            Self::Unreachable(_) => "unreachable",
            Self::Closed(_) => "closed",
        }
    }
    /// Whether the same call may succeed later without reconfiguration.
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Unreachable(_))
    }
}
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkRequired(operation) => {
                write!(f, "{operation} needs an attached network service")
            }
            Self::NetworkAttached => f.write_str("a network service is already attached"),
            Self::NoRoute { peer, network } => write!(
                f,
                "{peer} has no {network} route; attach its transport or add a current contact"
            ),
            Self::Unreachable(cause) => write!(f, "peer unreachable: {cause}"),
            Self::CredentialsExpired => {
                f.write_str("credentials expired; open with a network that reaches an issuer")
            }
            Self::Closed(handle) => write!(f, "{handle} closed"),
        }
    }
}
impl std::error::Error for ServiceError {}
