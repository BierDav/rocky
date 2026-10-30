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
    /// Replaying a received transaction's intents, before its commit.
    Apply,
    /// Rewinding applied transactions through their undo records.
    Rewind,
    /// Between transactions redone after a rewind.
    Redo,
    /// Deleting undo records a seal covers.
    UndoDeletion,
}
impl CrashPoint {
    pub const ALL: [Self; 12] = [
        Self::Import,
        Self::Switch,
        Self::Prune,
        Self::Compact,
        Self::Settle,
        Self::Evict,
        Self::GroupBeforeCommit,
        Self::GroupAfterCommit,
        Self::Apply,
        Self::Rewind,
        Self::Redo,
        Self::UndoDeletion,
    ];
    /// The points of replayed tables.
    pub const REPLAY: [Self; 4] = [Self::Apply, Self::Rewind, Self::Redo, Self::UndoDeletion];
    pub(super) fn stage(self) -> Option<checkpoint::Stage> {
        use checkpoint::Stage;
        Some(match self {
            Self::Import => Stage::Import,
            Self::Switch => Stage::Switch,
            Self::Prune => Stage::Prune,
            Self::Compact => Stage::Compact,
            Self::Settle => Stage::Settle,
            Self::Evict => Stage::Evict,
            Self::GroupBeforeCommit
            | Self::GroupAfterCommit
            | Self::Apply
            | Self::Rewind
            | Self::Redo
            | Self::UndoDeletion => return None,
        })
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Arms `point` for the next step that reaches it; `None` disarms.
    pub async fn arm_crash(&self, point: Option<CrashPoint>) {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        *inner
            .crash
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (point, None);
    }
    /// The armed point that fired since it was armed.
    pub async fn crashed(&self) -> Option<CrashPoint> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .crash
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
    }
}

impl<A: RowApplication> Inner<A> {
    /// Fails once at the armed point.
    pub(super) fn crash_at(&self, point: impl Fn(CrashPoint) -> bool) -> Result<(), Error> {
        let mut crash = self
            .crash
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match crash.0 {
            Some(armed) if point(armed) => {
                *crash = (None, Some(armed));
                Err(Error::InjectedFailure)
            }
            _ => Ok(()),
        }
    }
}

// Only qualification builds read this variable. Exit without destructors so
// SQLite must recover the pending WAL/transaction on the next process open.
#[cfg(feature = "qualification")]
pub(super) fn qualification_kill(point: &str) {
    if std::env::var("ROCKY_SQL_KILL_AT").is_ok_and(|selected| selected == point) {
        std::process::exit(87);
    }
}
