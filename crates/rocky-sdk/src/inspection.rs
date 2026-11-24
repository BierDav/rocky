use crate::{Database, request::Request};
use rocky_core::Commitment;
pub use rocky_core::replay::IntentStatement;
pub use rocky_secure::{
    CheckpointStatus, CurrentValue, HeldChange, HeldField, HeldIntent, HeldKind, HeldRow,
    RetentionLimits, RetentionPolicy,
};

#[derive(Clone, Debug)]
pub struct RetentionStatus {
    pub policy: Option<crate::RetentionPolicy>,
    pub checkpoint: crate::CheckpointStatus,
}
impl Database {
    pub async fn held_changes(&self) -> Result<Vec<crate::HeldChange>, crate::Error> {
        self.send(Request::HeldChanges).await
    }
    pub async fn drop_held(&self, id: Commitment) -> Result<(), crate::Error> {
        self.send(|reply| Request::DropHeld(id, reply)).await
    }
    pub async fn retention(&self, collection: &str) -> Result<RetentionStatus, crate::Error> {
        self.send(|reply| Request::Retention(collection.into(), None, false, reply))
            .await
    }
    pub async fn set_retention(
        &self,
        collection: &str,
        policy: crate::RetentionPolicy,
    ) -> Result<RetentionStatus, crate::Error> {
        self.send(|reply| Request::Retention(collection.into(), Some(policy), false, reply))
            .await
    }
    pub async fn maintain_retention(
        &self,
        collection: &str,
    ) -> Result<RetentionStatus, crate::Error> {
        self.send(|reply| Request::Retention(collection.into(), None, true, reply))
            .await
    }
    pub async fn retention_limits(&self) -> Result<crate::RetentionLimits, crate::Error> {
        self.send(|reply| Request::RetentionLimits(None, reply))
            .await
    }
    pub async fn set_retention_limits(
        &self,
        limits: crate::RetentionLimits,
    ) -> Result<crate::RetentionLimits, crate::Error> {
        self.send(|reply| Request::RetentionLimits(Some(limits), reply))
            .await
    }
    pub async fn set_trusted_stale_after(&self, seconds: u64) -> Result<(), crate::Error> {
        self.send(|reply| Request::TrustedStaleAfter(seconds, reply))
            .await
    }
    #[cfg(any(test, feature = "test-support"))]
    pub async fn activity(
        &self,
        after: u64,
    ) -> Result<crate::activity::ActivityPage, crate::Error> {
        self.send(|reply| Request::Activity(after, reply)).await
    }
    #[cfg(any(test, feature = "test-support"))]
    pub async fn next_activity(
        &self,
        after: u64,
    ) -> Result<crate::activity::ActivityPage, crate::Error> {
        self.send(|reply| Request::ActivityNext(after, reply)).await
    }
}
