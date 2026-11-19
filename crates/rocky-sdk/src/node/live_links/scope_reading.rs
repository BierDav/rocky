use super::Registry;
use crate::{Failure, node::Node};
use rocky_core::{Commitment, ResourceScope};
use rocky_secure::{ScopeFrontier, network::link};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::node) struct ScopeToken(Commitment);

impl ScopeToken {
    pub(super) fn from_handshake(token: Commitment) -> Self {
        Self(token)
    }
    pub(in crate::node) async fn read(node: &Node, scope: ResourceScope) -> Result<Self, Failure> {
        Ok(Self(link::scope_token(&node.replica, scope).await?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::node) enum ScopeReadSite {
    LinkOpen,
    FailedPushRepair,
    Receipt,
}

pub(in crate::node) struct ScopeReading {
    token: ScopeToken,
    frontier: ScopeFrontier,
}

impl ScopeReading {
    pub(in crate::node) async fn racing_commit_stays_dirty(
        node: &Node,
        live: &Registry,
        site: ScopeReadSite,
        scope: ResourceScope,
        token_read_first: ScopeToken,
    ) -> Result<Self, Failure> {
        live.between_scope_reads(site, scope).await?;
        Ok(Self {
            token: token_read_first,
            frontier: node.replica.frontier(scope).await?,
        })
    }
    pub(in crate::node) async fn racing_commit_stays_in_repair(
        node: &Node,
        live: &Registry,
        site: ScopeReadSite,
        scope: ResourceScope,
        frontier_read_first: ScopeFrontier,
    ) -> Result<Self, Failure> {
        live.between_scope_reads(site, scope).await?;
        Ok(Self {
            token: ScopeToken::read(node, scope).await?,
            frontier: frontier_read_first,
        })
    }
    pub(super) fn token(&self) -> ScopeToken {
        self.token
    }
    pub(super) fn frontier(&self) -> &ScopeFrontier {
        &self.frontier
    }
    pub(super) fn into_frontier(self) -> ScopeFrontier {
        self.frontier
    }
}

#[cfg(test)]
pub(in crate::node) struct ScopeReadGate {
    site: ScopeReadSite,
    scope: ResourceScope,
    pub(in crate::node) entered: tokio::sync::Semaphore,
    pub(in crate::node) resume: tokio::sync::Semaphore,
}

#[cfg(test)]
impl ScopeReadGate {
    pub(in crate::node) fn arm(
        live: &Registry,
        site: ScopeReadSite,
        scope: ResourceScope,
    ) -> std::sync::Arc<Self> {
        let gate = std::sync::Arc::new(Self {
            site,
            scope,
            entered: tokio::sync::Semaphore::new(0),
            resume: tokio::sync::Semaphore::new(0),
        });
        *live
            .scope_read_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(gate.clone());
        gate
    }
}

impl Registry {
    #[cfg(test)]
    async fn between_scope_reads(
        &self,
        site: ScopeReadSite,
        scope: ResourceScope,
    ) -> Result<(), Failure> {
        let gate = {
            let mut armed = self
                .scope_read_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match armed.as_ref() {
                Some(gate) if gate.site == site && gate.scope == scope => armed.take(),
                Some(_) | None => None,
            }
        };
        if let Some(gate) = gate {
            gate.entered.add_permits(1);
            gate.resume.acquire().await?.forget();
        }
        Ok(())
    }
    #[cfg(not(test))]
    async fn between_scope_reads(&self, _: ScopeReadSite, _: ResourceScope) -> Result<(), Failure> {
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::node) fn linked_scope(
        &self,
        peer: rocky_secure::PublicKey,
        scope: ResourceScope,
    ) -> Option<(u64, ScopeToken, ScopeFrontier)> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = entries.get(&peer)?;
        Some((
            entry.cursor_generation,
            *entry.tokens.get(&scope)?,
            entry.cursors.get(&scope)?.clone(),
        ))
    }
}
