use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashPoint {
    Custody,
    Import,
    Switch,
    Prune,
    Compact,
    BasePrepare,
    BasePrepared,
    BasePublished,
    BaseCleanup,
    BaseCleaned,
    Settle,
    Evict,
    GroupBeforeCommit,
    GroupAfterCommit,
    Apply,
    Rewind,
    Redo,
    UndoDeletion,
}
impl CrashPoint {
    pub const ALL: [Self; 18] = [
        Self::Custody,
        Self::Import,
        Self::Switch,
        Self::Prune,
        Self::Compact,
        Self::BasePrepare,
        Self::BasePrepared,
        Self::BasePublished,
        Self::BaseCleanup,
        Self::BaseCleaned,
        Self::Settle,
        Self::Evict,
        Self::GroupBeforeCommit,
        Self::GroupAfterCommit,
        Self::Apply,
        Self::Rewind,
        Self::Redo,
        Self::UndoDeletion,
    ];
    pub const REPLAY: [Self; 4] = [Self::Apply, Self::Rewind, Self::Redo, Self::UndoDeletion];
    pub(super) fn stage(self) -> Option<checkpoint::Stage> {
        use checkpoint::Stage;
        Some(match self {
            Self::Custody => Stage::Custody,
            Self::Import => Stage::Import,
            Self::Switch => Stage::Switch,
            Self::Prune => Stage::Prune,
            Self::Compact => Stage::Compact,
            Self::BasePrepare => Stage::BasePrepare,
            Self::BaseCleanup => Stage::BaseCleanup,
            Self::Settle => Stage::Settle,
            Self::Evict => Stage::Evict,
            Self::BasePrepared
            | Self::BasePublished
            | Self::BaseCleaned
            | Self::GroupBeforeCommit
            | Self::GroupAfterCommit
            | Self::Apply
            | Self::Rewind
            | Self::Redo
            | Self::UndoDeletion => return None,
        })
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn arm_crash(&self, point: Option<CrashPoint>) {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        *inner
            .crash
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (point, None);
    }
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

#[cfg(feature = "qualification")]
pub(super) fn qualification_kill(point: &str) {
    if std::env::var("ROCKY_SQL_KILL_AT").is_ok_and(|selected| selected == point) {
        std::process::exit(87);
    }
}
