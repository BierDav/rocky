//! A coordinator client one device alone cannot reach while its switch is
//! off: the same service, unreachable from that device only.
use rocky_core::{
    Append, Commitment, CoordinationError, CoordinationFuture, CoordinatorIdentity, DomainLog,
    LogHead, LogPending, LogResult, LogStatus, LogTerminal, NodeKey, Scope, TipLog,
};
use rocky_sdk::credentials::{CoordinationObserver, CredentialChain, PrivateKey, RevocationSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct Reach {
    inner: Arc<dyn rocky_sdk::Coordinator>,
    down: Arc<AtomicBool>,
}
impl Reach {
    /// `inner`, and the switch that makes it unreachable.
    pub fn new(inner: Arc<dyn rocky_sdk::Coordinator>) -> (Arc<Self>, Arc<AtomicBool>) {
        let down = Arc::new(AtomicBool::new(false));
        (
            Arc::new(Self {
                inner,
                down: down.clone(),
            }),
            down,
        )
    }
    fn reached(&self) -> Result<(), CoordinationError> {
        match self.down.load(Ordering::SeqCst) {
            true => Err(CoordinationError::Unavailable),
            false => Ok(()),
        }
    }
}
impl DomainLog for Reach {
    fn node(&self) -> NodeKey {
        self.inner.node()
    }
    fn identity(&self) -> CoordinatorIdentity {
        self.inner.identity()
    }
    fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead> {
        Box::pin(async move {
            self.reached()?;
            self.inner.log_head(log).await
        })
    }
    fn pending_status(
        &self,
        log: TipLog,
        position: u64,
        hash: Commitment,
    ) -> CoordinationFuture<'_, LogStatus> {
        Box::pin(async move {
            self.reached()?;
            self.inner.pending_status(log, position, hash).await
        })
    }
    fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()> {
        Box::pin(async move {
            self.reached()?;
            self.inner.configure(log, minimum).await
        })
    }
    fn join(
        &self,
        log: TipLog,
        position: u64,
        running_hash: Commitment,
        sequence: u64,
    ) -> CoordinationFuture<'_, u64> {
        Box::pin(async move {
            self.reached()?;
            self.inner.join(log, position, running_hash, sequence).await
        })
    }
    fn append(&self, log: TipLog, append: Append) -> CoordinationFuture<'_, LogPending> {
        Box::pin(async move {
            self.reached()?;
            self.inner.append(log, append).await
        })
    }
    fn acknowledge(&self, log: TipLog, pending: LogPending) -> CoordinationFuture<'_, LogResult> {
        Box::pin(async move {
            self.reached()?;
            self.inner.acknowledge(log, pending).await
        })
    }
    fn seal(&self, log: TipLog, terminal: LogTerminal) -> CoordinationFuture<'_, LogResult> {
        Box::pin(async move {
            self.reached()?;
            self.inner.seal(log, terminal).await
        })
    }
    fn recover(&self, log: TipLog, seal: Vec<u8>) -> CoordinationFuture<'_, LogResult> {
        Box::pin(async move {
            self.reached()?;
            self.inner.recover(log, seal).await
        })
    }
    fn acknowledge_terminal(
        &self,
        log: TipLog,
        acknowledgment: Vec<u8>,
    ) -> CoordinationFuture<'_, ()> {
        Box::pin(async move {
            self.reached()?;
            self.inner.acknowledge_terminal(log, acknowledgment).await
        })
    }
}
impl rocky_sdk::Coordinator for Reach {
    fn bundle(&self) -> Result<rocky_sdk::CoordinatorBundle, CoordinationError> {
        self.inner.bundle()
    }
    fn bind(
        &self,
        key: PrivateKey,
        certificate: rocky_sdk::credentials::SignedCapability,
    ) -> Result<Arc<dyn rocky_sdk::Coordinator>, CoordinationError> {
        Ok(Arc::new(Self {
            inner: self.inner.bind(key, certificate)?,
            down: self.down.clone(),
        }))
    }
    fn certify(&self, certificate: rocky_sdk::credentials::SignedCapability) {
        self.inner.certify(certificate);
    }
    fn observe(&self, observer: CoordinationObserver) {
        self.inner.observe(observer);
    }
    fn present_claims(&self, claims: Option<CredentialChain>) {
        self.inner.present_claims(claims);
    }
    fn claim(
        &self,
        scope: Scope,
        counter: rocky_sdk::counters::Name,
        claim: rocky_sdk::counters::Claim,
    ) -> CoordinationFuture<'_, u64> {
        Box::pin(async move {
            self.reached()?;
            self.inner.claim(scope, counter, claim).await
        })
    }
    fn counter(
        &self,
        scope: Scope,
        counter: rocky_sdk::counters::Name,
        claim: Option<rocky_sdk::counters::Claim>,
    ) -> CoordinationFuture<'_, rocky_sdk::counters::Reading> {
        Box::pin(async move {
            self.reached()?;
            self.inner.counter(scope, counter, claim).await
        })
    }
    fn revoke(
        &self,
        scope: Scope,
        revocations: RevocationSet,
    ) -> CoordinationFuture<'_, Commitment> {
        Box::pin(async move {
            self.reached()?;
            self.inner.revoke(scope, revocations).await
        })
    }
}
