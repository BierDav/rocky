//! Committed frontier caching. Validity belongs to the replica, never SQLite counters.
use super::*;

#[derive(Default)]
pub(super) struct Frontiers {
    generation: u64,
    entries: HashMap<ResourceScope, (u64, TipId, ScopeFrontier)>,
    /// The last row replay ledger, extended as a replay advances.
    pub(super) replay_ledger: Option<ReplayLedger>,
}
/// A row replay ledger of scope number `id` towards `target` from the base
/// at `base` through `through`.
pub(super) struct ReplayLedger {
    pub(super) id: i64,
    pub(super) target: TipId,
    pub(super) base: i64,
    pub(super) through: i64,
    pub(super) ledger: identity::IdentityLedger,
}

impl<A> Inner<A> {
    pub(crate) fn invalidate_frontiers(&self) {
        let mut cache = self
            .tip_frontiers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.generation = cache.generation.wrapping_add(1);
        cache.entries.clear();
        cache.replay_ledger = None;
        drop(cache);
        self.readiness.invalidate();
    }
    /// Also used after rollback and after restoring an earlier in-memory context.
    pub(crate) fn invalidate_context(&self) {
        self.invalidate_frontiers();
        self.invalidate_records();
        *self
            .graph_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Default::default();
        self.path_fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
    pub(crate) fn cached_frontier(&self, scope: ResourceScope) -> Option<ScopeFrontier> {
        let cache = self
            .tip_frontiers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .entries
            .get(&scope)
            .filter(|(generation, tip, _)| {
                *generation == cache.generation && *tip == self.workspace.tip
            })
            .map(|(_, _, frontier)| frontier.clone())
    }
    pub(super) fn cache_frontier(&self, frontier: &ScopeFrontier) {
        let mut cache = self
            .tip_frontiers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = cache.generation;
        cache.entries.insert(
            frontier.scope,
            (generation, self.workspace.tip, frontier.clone()),
        );
    }
}

#[cfg(test)]
impl<A: RowApplication> Inner<A> {
    pub(crate) fn checked_seal_path(
        &self,
        domain: rocky_core::Scope,
    ) -> Result<Vec<rocky_credentials::SignedSeal>, Error> {
        let cached = self.path_seals(domain)?;
        let expected = if self.workspace.followed.as_ref().map(|(scope, _)| *scope) == Some(domain)
        {
            compaction::tip_path_seals(self.graph_for(domain)?, self.workspace.tip)?
        } else {
            Vec::new()
        };
        assert_eq!(cached, expected);
        Ok(cached)
    }
    pub(crate) async fn reload_frontier_graphs(&mut self) -> Result<(), Error> {
        self.load_tip_graphs().await
    }
    pub(crate) async fn refresh_frontier_revocations(
        &mut self,
        revocations: &rocky_credentials::RevocationSet,
    ) -> Result<bool, Error> {
        self.resolve_revoked(revocations).await
    }
}

#[cfg(test)]
impl<A> Inner<A> {
    pub(crate) fn frontier_cache_stats(&self) -> (u64, usize, u64, u64) {
        let cache = self
            .tip_frontiers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            cache.generation,
            cache.entries.len(),
            self.frontier_queries.load(Ordering::Relaxed),
            self.frontier_decodes.load(Ordering::Relaxed),
        )
    }
}
