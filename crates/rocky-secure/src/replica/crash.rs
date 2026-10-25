//! Crash points of qualification builds: an armed point fails the step that
//! reaches it inside its transaction, or right after its commit, as a
//! process death there would interrupt it. The caller then drops the
//! replica without closing it.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashPoint {
    /// A checkpoint page import, before its transaction commits.
    Import,
    /// Switching a scope to an imported checkpoint.
    Switch,
    /// One pruning step.
    Prune,
    /// Compacting at a seal.
    Compact,
    /// Settling a seal rebootstrap.
    Settle,
    /// Evicting a scope.
    Evict,
    /// A group commit, before it commits.
    GroupBeforeCommit,
    /// A group commit, after it committed and before anyone hears of it.
    GroupAfterCommit,
}
impl CrashPoint {
    pub const ALL: [Self; 8] = [
        Self::Import,
        Self::Switch,
        Self::Prune,
        Self::Compact,
        Self::Settle,
        Self::Evict,
        Self::GroupBeforeCommit,
        Self::GroupAfterCommit,
    ];
    pub(super) fn stage(self) -> Option<checkpoint::Stage> {
        use checkpoint::Stage;
        Some(match self {
            Self::Import => Stage::Import,
            Self::Switch => Stage::Switch,
            Self::Prune => Stage::Prune,
            Self::Compact => Stage::Compact,
            Self::Settle => Stage::Settle,
            Self::Evict => Stage::Evict,
            Self::GroupBeforeCommit | Self::GroupAfterCommit => return None,
        })
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Arms `point` for the next step that reaches it; `None` disarms.
    pub async fn arm_crash(&self, point: Option<CrashPoint>) {
        let mut inner = self.inner.lock().await;
        inner.crash = point;
        inner.crashed = None;
    }
    /// The armed point that fired since it was armed.
    pub async fn crashed(&self) -> Option<CrashPoint> {
        self.inner.lock().await.crashed
    }
}

impl<A: RowApplication> Inner<A> {
    /// Fails once at the armed point.
    pub(super) fn crash_at(&mut self, point: impl Fn(CrashPoint) -> bool) -> Result<(), Error> {
        match self.crash {
            Some(armed) if point(armed) => {
                self.crash = None;
                self.crashed = Some(armed);
                Err(Error::InjectedFailure)
            }
            _ => Ok(()),
        }
    }
}
