use crate::{
    Failure,
    transport::{Connected, Connector, Transport},
};
use std::sync::Arc;

#[derive(Clone)]
pub enum Network {
    Owned(Arc<dyn Connector>),
    Borrowed(Arc<dyn Transport>),
    Attached(Connected),
}
impl Network {
    pub(crate) async fn start(
        self,
        accepts: bool,
        secret: [u8; 32],
    ) -> Result<(Connected, bool), Failure> {
        Ok(match self {
            Self::Owned(connector) => (connector.start(accepts, secret).await?, true),
            Self::Borrowed(transport) => (Connected::new(transport), false),
            Self::Attached(connected) => (connected, false),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceError {
    NetworkRequired(&'static str),
    NetworkAttached,
    NoRoute { peer: &'static str, network: String },
    Unreachable(String),
    TimedOut(String),
    CredentialsExpired,
    Closed(&'static str),
}
impl ServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NetworkRequired(_) | Self::CredentialsExpired => "network_required",
            Self::NetworkAttached | Self::NoRoute { .. } => "rejected",
            Self::Unreachable(_) | Self::TimedOut(_) => "unreachable",
            Self::Closed(_) => "closed",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Unreachable(_) | Self::TimedOut(_))
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
            Self::TimedOut(cause) => write!(f, "dial timed out: {cause}"),
            Self::CredentialsExpired => {
                f.write_str("credentials expired; open with a network that reaches an issuer")
            }
            Self::Closed(handle) => write!(f, "{handle} closed"),
        }
    }
}
impl std::error::Error for ServiceError {}
