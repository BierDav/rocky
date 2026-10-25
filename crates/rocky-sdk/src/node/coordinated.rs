//! Coordinated writes of a trusted node: entries travel to the in-sync set
//! over the node's live links, and entries it stores for other proposers are
//! resolved in the background. See `docs/runtime/coordinated-writes.md`.
use super::*;
use rocky_core::{DomainLog, TipLog};
use rocky_secure::{
    CommittedEntry, CoordinatedCommit, CoordinatedEntry, CoordinatedError, ENTRY_DEADLINE,
    EntryPeers, PeerFuture, Slot,
};

/// The in-sync set reached over this node's open live links. A member
/// without one misses the entry's deadline and leaves the set.
pub(super) struct LinkPeers<'a>(pub(super) &'a Node);
impl LinkPeers<'_> {
    fn handles(
        &self,
        only: Option<PublicKey>,
    ) -> Vec<rocky_secure::network::link::channels::Handle> {
        let Some(service) = self.0.current() else {
            return Vec::new();
        };
        service
            .live
            .nodes()
            .into_iter()
            .filter(|peer| only.is_none_or(|only| only == *peer))
            .filter_map(|peer| service.live.handle(peer))
            .collect()
    }
}
impl EntryPeers for LinkPeers<'_> {
    fn store<'a>(&'a self, node: PublicKey, entry: &'a CoordinatedEntry) -> PeerFuture<'a, ()> {
        Box::pin(async move {
            let handle = self
                .handles(Some(node))
                .pop()
                .ok_or_else(|| Error::Rejected("no live link to the trusted node".into()))?;
            let mut session = handle.open().await?;
            session.store_entry(entry).await?;
            session.close().await
        })
    }
    fn committed(&self, log: TipLog, after: u64) -> PeerFuture<'_, Vec<CommittedEntry>> {
        Box::pin(async move {
            let mut collected = std::collections::BTreeMap::new();
            for handle in self.handles(None) {
                let Ok(mut session) = handle.open().await else {
                    continue;
                };
                let entries = session.committed_entries(log, after).await;
                let _ = session.close().await;
                if let Ok(entries) = entries {
                    for entry in entries {
                        if let Some(held) = collected.get(&entry.position) {
                            let held: &CommittedEntry = held;
                            if held.hash != entry.hash {
                                return Err(Error::Rejected(
                                    "conflicting committed evidence".into(),
                                ));
                            }
                        }
                        collected.entry(entry.position).or_insert(entry);
                    }
                }
            }
            Ok(collected.into_values().collect())
        })
    }
}

/// A coordinated write's failure as the JSON contract reports it.
#[derive(Debug)]
pub(crate) struct Coordinated(pub(crate) CoordinatedError);
impl std::fmt::Display for Coordinated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "coordinated write: {}", self.0)
    }
}
impl std::error::Error for Coordinated {}
impl Coordinated {
    /// The error code and whether a new attempt may succeed.
    pub(crate) fn code(&self) -> (&'static str, bool) {
        match &self.0 {
            CoordinatedError::Unavailable => ("coordinator_unavailable", true),
            CoordinatedError::BelowMinimum => ("below_minimum", true),
            CoordinatedError::ClockSkew { .. } => ("clock_skew", false),
            CoordinatedError::Conflict => ("conflict", true),
            CoordinatedError::Aborted => ("aborted", true),
            CoordinatedError::Undecided => ("undecided", true),
            CoordinatedError::Behind => ("behind", true),
            CoordinatedError::CustodyRequired => ("custody_required", false),
            CoordinatedError::Unfollowed => ("unfollowed_tip", false),
            CoordinatedError::Sealed => ("sealed", false),
            CoordinatedError::Refused(_) => ("rejected", false),
            CoordinatedError::Storage(_) => ("storage", true),
        }
    }
}
pub(super) fn coordinated(error: CoordinatedError) -> Failure {
    match error {
        CoordinatedError::Storage(error) => Box::new(error),
        other => Box::new(Coordinated(other)),
    }
}

impl Node {
    /// Makes this device a trusted node of its followed tip's coordinated log, or
    /// ends that with `None`. The log is the application's; Rocky never
    /// closes it.
    pub fn coordinate(self: &Arc<Self>, log: Option<Arc<dyn DomainLog>>) {
        let start = log.is_some() && self.replica.domain_log().is_none();
        self.replica.set_domain_log(log);
        if !start {
            return;
        }
        let weak = Arc::downgrade(self);
        let mut stored = self.replica.observe_entries();
        tokio::task::spawn_local(async move {
            loop {
                let Some(node) = weak.upgrade() else {
                    return;
                };
                node.settle_entries().await;
                drop(node);
                if stored.changed().await.is_err() {
                    return;
                }
            }
        });
    }
    fn log(&self) -> Result<Arc<dyn DomainLog>, Failure> {
        self.replica
            .domain_log()
            .ok_or_else(|| refused("no coordinated log is composed with this database"))
    }
    /// Resolves entries this device stores until none is undecided or a
    /// deadline past the last has gone by; a later call continues.
    async fn settle_entries(&self) {
        let Ok(log) = self.log() else {
            return;
        };
        let scope = self.replica.followed_log(self.domain().scope).await;
        let until = tokio::time::Instant::now() + Duration::from_secs(2 * ENTRY_DEADLINE);
        let mut delay = Duration::from_millis(20);
        while tokio::time::Instant::now() < until {
            match self
                .replica
                .resolve_coordinated(scope, log.as_ref(), &LinkPeers(self))
                .await
            {
                Ok(status) if status.undecided == 0 => break,
                _ => {}
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(1));
        }
        self.finalize_seals().await;
        self.observations.touch();
    }
    /// The position the next coordinated write is staged at; decides
    /// stored entries and catches up first.
    pub(super) async fn coordinated_slot(&self) -> Result<Slot, Failure> {
        let log = self.log()?;
        let tip = self.replica.followed_log(self.domain().scope).await;
        self.replica
            .coordinated_slot(tip, log.as_ref(), &LinkPeers(self))
            .await
            .map_err(coordinated)
    }
    /// Drives this device's staged write of `tip`'s log to its decision.
    pub(super) async fn propose(&self, tip: TipLog) -> Result<CoordinatedCommit, Failure> {
        let log = self.log()?;
        let proposed = self
            .replica
            .propose(tip, log.as_ref(), &LinkPeers(self))
            .await
            .map_err(coordinated);
        self.observations.touch();
        proposed
    }
    /// This device's position on its domain's log, after resolving what it
    /// stores; without a composed log only what it holds.
    pub(super) async fn coordinated_status(&self) -> Result<Value, Failure> {
        let scope = self.replica.followed_log(self.domain().scope).await;
        let status = match self.replica.domain_log() {
            Some(log) => self
                .replica
                .resolve_coordinated(scope, log.as_ref(), &LinkPeers(self))
                .await
                .map_err(coordinated)?,
            None => self.replica.coordinated_status(scope).await?,
        };
        Ok(json!({
            "trusted": self.replica.domain_log().is_some(),
            "tip": HEXLOWER.encode(&scope.tip.0),
            "position": status.position.to_string(),
            "undecided": status.undecided,
        }))
    }
}
