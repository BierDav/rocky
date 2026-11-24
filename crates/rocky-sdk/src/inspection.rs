use crate::{Database, request::Request};
use rocky_core::Commitment;
pub use rocky_core::replay::IntentStatement;
pub use rocky_secure::{
    CheckpointStatus, CurrentValue, HeldChange, HeldField, HeldIntent, HeldKind, HeldRow,
    RetentionLimits, RetentionPolicy,
};

/// A collection's retention policy and checkpoint.
#[derive(Clone, Debug)]
pub struct RetentionStatus {
    /// The policy, or `None` to keep all history.
    pub policy: Option<crate::RetentionPolicy>,
    /// The latest checkpoint.
    pub checkpoint: crate::CheckpointStatus,
}
impl Database {
    /// Changes that arrived but cannot apply yet, such as writes for a schema version not
    /// installed.
    pub async fn held_changes(&self) -> Result<Vec<crate::HeldChange>, crate::Error> {
        self.send(Request::HeldChanges).await
    }
    /// Discards one held change.
    pub async fn drop_held(&self, id: Commitment) -> Result<(), crate::Error> {
        self.send(|reply| Request::DropHeld(id, reply)).await
    }
    /// A collection's retention.
    pub async fn retention(&self, collection: &str) -> Result<RetentionStatus, crate::Error> {
        self.send(|reply| Request::Retention(collection.into(), None, false, reply))
            .await
    }
    /// Sets how much history a collection keeps.
    pub async fn set_retention(
        &self,
        collection: &str,
        policy: crate::RetentionPolicy,
    ) -> Result<RetentionStatus, crate::Error> {
        self.send(|reply| Request::Retention(collection.into(), Some(policy), false, reply))
            .await
    }
    /// Applies a collection's retention now.
    pub async fn maintain_retention(
        &self,
        collection: &str,
    ) -> Result<RetentionStatus, crate::Error> {
        self.send(|reply| Request::Retention(collection.into(), None, true, reply))
            .await
    }
    /// The database-wide retention limits.
    pub async fn retention_limits(&self) -> Result<crate::RetentionLimits, crate::Error> {
        self.send(|reply| Request::RetentionLimits(None, reply))
            .await
    }
    /// Changes the database-wide retention limits.
    pub async fn set_retention_limits(
        &self,
        limits: crate::RetentionLimits,
    ) -> Result<crate::RetentionLimits, crate::Error> {
        self.send(|reply| Request::RetentionLimits(Some(limits), reply))
            .await
    }
    /// How long an admin may stay unheard before `TrustedIdentityStale`.
    pub async fn set_trusted_stale_after(&self, seconds: u64) -> Result<(), crate::Error> {
        self.send(|reply| Request::TrustedStaleAfter(seconds, reply))
            .await
    }
    /// The activity log after `after`, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn activity(
        &self,
        after: u64,
    ) -> Result<crate::activity::ActivityPage, crate::Error> {
        self.send(|reply| Request::Activity(after, reply)).await
    }
    /// Waits for activity after `after`, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn next_activity(
        &self,
        after: u64,
    ) -> Result<crate::activity::ActivityPage, crate::Error> {
        self.send(|reply| Request::ActivityNext(after, reply)).await
    }
}
