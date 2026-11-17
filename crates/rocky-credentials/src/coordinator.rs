use crate::{CredentialChain, PrivateKey, RevocationSet, SignedCapability, counters};
use rocky_core::{Commitment, CoordinationError, CoordinationFuture, DomainLog, Scope};
use std::sync::Arc;

pub type CoordinationObserver = Arc<dyn Fn(Result<(), &CoordinationError>) + Send + Sync>;

pub trait Coordinator: DomainLog {
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
