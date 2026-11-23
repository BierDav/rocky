use crate::{CredentialChain, PrivateKey, RevocationSet, SignedCapability, counters};
use rocky_core::{Commitment, MeshId, Scope};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub type NodeKey = [u8; 32];
pub type Configuration = [u8; 16];
pub const MAX_COORDINATOR_BUNDLE: usize = 64 * 1024;
pub const MAX_BUNDLE_URL: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CoordinatorIdentity {
    pub root: NodeKey,
    pub mesh: MeshId,
    pub genesis: Commitment,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinatorBundle {
    pub identity: CoordinatorIdentity,
    #[serde(with = "serde_bytes")]
    pub certified: Vec<u8>,
    pub url: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoordinationError {
    Unavailable,
    ClockSkew { coordinator_time: u64 },
    CoordinationStateLost,
    UnknownDomain,
    Denied,
    Configuration,
    StaleEpoch,
    StaleHead,
    InvalidCommitment,
    RequestConflict,
    Corrupt,
    Retired,
    Expired,
    Backpressure,
    UnsupportedFormat,
    Sealed,
}
impl CoordinationError {
    pub fn category(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::ClockSkew { .. } => "clock_skew",
            Self::CoordinationStateLost => "state_lost",
            Self::UnknownDomain => "unknown_domain",
            Self::Denied => "denied",
            Self::Configuration => "configuration",
            Self::StaleEpoch => "stale_epoch",
            Self::StaleHead => "stale_head",
            Self::InvalidCommitment => "invalid_commitment",
            Self::RequestConflict => "request_conflict",
            Self::Corrupt => "corrupt",
            Self::Retired => "retired",
            Self::Expired => "expired",
            Self::Backpressure => "backpressure",
            Self::UnsupportedFormat => "unsupported_format",
            Self::Sealed => "sealed",
        }
    }
}
impl std::fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CoordinationError {}
pub type CoordinationFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, CoordinationError>> + Send + 'a>>;
pub fn genesis(scope: Scope, configuration: Configuration) -> Commitment {
    let mut b = Vec::new();
    b.extend(scope.mesh.0);
    b.extend(scope.app.0);
    b.extend(scope.domain.0);
    b.extend(configuration);
    rocky_core::commitment("rocky.mvp.genesis.v1", &b)
}

pub type CoordinationObserver = Arc<dyn Fn(Result<(), &CoordinationError>) + Send + Sync>;

pub trait Coordinator: Send + Sync {
    fn node(&self) -> NodeKey;
    fn identity(&self) -> CoordinatorIdentity;
    fn bundle(&self) -> Result<crate::CoordinatorBundle, CoordinationError>;
    fn bind(
        &self,
        key: PrivateKey,
        certificate: SignedCapability,
    ) -> Result<Arc<dyn Coordinator>, CoordinationError>;
    fn certify(&self, certificate: SignedCapability);
    fn observe(&self, observer: CoordinationObserver);
    fn present_claims(&self, claims: Option<CredentialChain>);
    fn claim(
        &self,
        scope: Scope,
        counter: counters::Name,
        claim: counters::Claim,
    ) -> CoordinationFuture<'_, u64>;
    fn counter(
        &self,
        scope: Scope,
        counter: counters::Name,
        claim: Option<counters::Claim>,
    ) -> CoordinationFuture<'_, counters::Reading>;
    fn revoke(
        &self,
        scope: Scope,
        revocations: RevocationSet,
    ) -> CoordinationFuture<'_, Commitment>;
}

pub trait CoordinatorConnector: Send + Sync {
    fn connect(
        &self,
        bundle: &crate::CoordinatorBundle,
    ) -> Result<Arc<dyn Coordinator>, CoordinationError>;
}
