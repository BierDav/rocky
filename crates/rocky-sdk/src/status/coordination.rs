use super::*;
use rocky_core::{
    Confirmation, CoordinationFuture, Coordinator, NodeId, RequestId, Scope, StrongHead,
    StrongReceipt,
};
use std::sync::Arc;

pub(crate) struct ObservedCoordinator {
    pub inner: Arc<dyn Coordinator>,
    pub observations: Observations,
}
impl ObservedCoordinator {
    fn observe<'a, T: Send + 'a>(
        &'a self,
        operation: AttemptOperation,
        request: impl FnOnce() -> CoordinationFuture<'a, T>,
    ) -> CoordinationFuture<'a, T> {
        let attempt = self.observations.begin(
            "coordinator".into(),
            operation,
            AttemptStage::CoordinatorRequest,
        );
        let request = request();
        Box::pin(async move {
            let result = request.await;
            attempt.finish(result.as_ref().err().map(failure));
            result
        })
    }
}
impl Coordinator for ObservedCoordinator {
    fn head(&self, scope: Scope) -> CoordinationFuture<'_, StrongHead> {
        self.observe(AttemptOperation::CoordinatorHead, || self.inner.head(scope))
    }
    fn confirm(&self, proposal: Confirmation) -> CoordinationFuture<'_, StrongReceipt> {
        self.observe(AttemptOperation::CoordinatorConfirm, || {
            self.inner.confirm(proposal)
        })
    }
    fn lookup(&self, scope: Scope, id: RequestId) -> CoordinationFuture<'_, Option<StrongReceipt>> {
        self.observe(AttemptOperation::CoordinatorLookup, || {
            self.inner.lookup(scope, id)
        })
    }
    fn verify<'a>(&'a self, receipt: &'a StrongReceipt) -> CoordinationFuture<'a, ()> {
        self.observe(AttemptOperation::CoordinatorVerify, || {
            self.inner.verify(receipt)
        })
    }
    fn retire(&self, scope: Scope, node: NodeId, through: u64) -> CoordinationFuture<'_, ()> {
        self.observe(AttemptOperation::CoordinatorRetire, || {
            self.inner.retire(scope, node, through)
        })
    }
    fn export_evidence<'a>(
        &'a self,
        receipt: &'a StrongReceipt,
    ) -> CoordinationFuture<'a, Option<Vec<u8>>> {
        self.observe(AttemptOperation::CoordinatorExportEvidence, || {
            self.inner.export_evidence(receipt)
        })
    }
    fn import_evidence<'a>(&'a self, evidence: &'a [u8]) -> CoordinationFuture<'a, ()> {
        self.observe(AttemptOperation::CoordinatorImportEvidence, || {
            self.inner.import_evidence(evidence)
        })
    }
}

pub(crate) fn failure(error: &rocky_core::CoordinationError) -> AttemptFailure {
    use rocky_core::CoordinationError::*;
    let code = match error {
        Unavailable => "unavailable",
        CoordinationStateLost => "coordination_state_lost",
        UnknownDomain => "unknown_domain",
        Denied => "denied",
        Configuration => "configuration",
        Generations => "generations",
        StaleEpoch => "stale_epoch",
        StaleHead => "stale_head",
        InvalidCommitment => "invalid_commitment",
        RequestConflict => "request_conflict",
        AlreadyConfirmed(_) => "already_confirmed",
        Corrupt => "corrupt",
        Retired => "retired",
        Expired => "expired",
        Backpressure => "backpressure",
        UnsupportedFormat => "unsupported_format",
    };
    AttemptFailure {
        kind: FailureKind::Coordination,
        code: Some(code.into()),
        detail: error.to_string(),
    }
}
