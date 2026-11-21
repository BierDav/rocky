use super::*;
use rocky_credentials::{CHECKPOINT_RECORD_FORMAT, CheckpointRecordStatement};

pub use rocky_core::FrameReplay;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealRebootstrap {
    pub scope: ResourceScope,
    pub listed: Vec<Commitment>,
    pub replay: FrameReplay,
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn base_seal(&self, id: i64) -> Result<Option<Commitment>, Error> {
        Ok(self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed)
            .and_then(|signed| signed.statement.seal))
    }
    pub(in crate::replica) fn owns_key(&self, key: PublicKey) -> bool {
        key == self.config.node.public_key()
            || self
                .credential_signer
                .as_ref()
                .is_some_and(|signer| signer.public_key() == key)
            || self
                .node_author
                .as_ref()
                .is_some_and(|author| author.key.public_key() == key)
    }
    pub(in crate::replica) async fn base_off_path(
        &self,
        scope: ResourceScope,
        id: i64,
    ) -> Result<bool, Error> {
        Ok(match self.base_seal(id).await? {
            Some(seal) => self.path_index(scope.domain, seal)?.is_none(),
            None => {
                self.graphs.contains_key(&scope.domain) && self.has_pruned_prefix(scope).await?
            }
        })
    }
    pub(in crate::replica) async fn require_scope_readable(
        &self,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        blockers::refuse(self.readable_blockers(scope).await?)
    }
    pub(in crate::replica) async fn require_domain_ready(
        &self,
        domain: Scope,
        scopes: impl IntoIterator<Item = ResourceScope>,
    ) -> Result<(), Error> {
        let mut blocked = self.domain_blockers(domain).await?;
        if blocked.is_empty() {
            return Ok(());
        }
        for scope in scopes {
            blocked.extend(self.scope_blockers(scope).await?);
        }
        blocked.sort();
        blocked.dedup();
        blockers::refuse(blocked)
    }
    pub(in crate::replica) fn listed_at<'a>(
        &self,
        graph: &rocky_credentials::TipGraph,
        scope: ResourceScope,
        seals: impl IntoIterator<Item = &'a Commitment>,
    ) -> Result<Vec<Commitment>, Error> {
        let mut listed = Vec::new();
        for seal in seals {
            for record in graph.checkpoints_at(*seal).map_err(credential)? {
                if record.statement.table == scope && !self.owns_key(record.statement.signer) {
                    listed.push(record.statement.checkpoint);
                }
            }
        }
        Ok(listed)
    }
    pub(in crate::replica) async fn seal_rebootstraps(
        &self,
        domain: Scope,
    ) -> Result<Vec<SealRebootstrap>, Error> {
        let Some(plan) = self.rebootstrap_plan(domain).await? else {
            return Ok(Vec::new());
        };
        let mut needs = Vec::new();
        for (scope, id) in &plan.scopes {
            needs.extend(self.seal_rebootstrap_of(&plan, *scope, *id).await?);
        }
        Ok(needs)
    }
    pub(in crate::replica) async fn rebootstrap_plan(
        &self,
        domain: Scope,
    ) -> Result<Option<RebootstrapPlan>, Error> {
        if !self.graphs.contains_key(&domain) {
            return Ok(None);
        }
        let path = self
            .path_seals(domain)?
            .iter()
            .map(|seal| seal.id().map_err(credential))
            .collect::<Result<Vec<_>, _>>()?;
        let unverified = self
            .unverified_scopes(domain)
            .await?
            .into_iter()
            .map(|(scope, _)| scope)
            .collect();
        Ok(Some(RebootstrapPlan {
            domain,
            path,
            unverified,
            scopes: self.readiness_scopes(domain).await?,
        }))
    }
    pub(in crate::replica) async fn seal_rebootstrap_of(
        &self,
        plan: &RebootstrapPlan,
        scope: ResourceScope,
        id: Option<i64>,
    ) -> Result<Option<SealRebootstrap>, Error> {
        let (domain, path) = (plan.domain, &plan.path);
        let graph = self.graph_for(domain)?;
        let unknown = plan.unverified.contains(&scope);
        let pruned = self.has_pruned_prefix(scope).await?;
        let base = match id {
            Some(id) => self.base_seal(id).await?,
            None => None,
        };
        let anchored = match id {
            Some(id) => self.has_seal_anchor(id).await?,
            None => false,
        };
        let missing = if unknown || anchored {
            match (id, self.path_seals(domain)?.last()) {
                (Some(id), Some(seal)) => self.replay_has_gaps(scope, id, seal).await?,
                (None, Some(seal)) => self
                    .seal_coverage_waits(domain, seal, None, &[(scope, id)])
                    .await?
                    .is_some(),
                (_, None) => false,
            }
        } else {
            false
        };
        let needed = unknown
            || (anchored && missing)
            || match base {
                Some(seal) => !path.contains(&seal),
                None => pruned,
            };
        if !needed {
            return Ok(None);
        }
        let listed = self.listed_at(graph, scope, path.iter().rev())?;
        let base_stands_in_for_pruned = base.map_or(!pruned, |seal| path.contains(&seal));
        if unknown && !missing && base_stands_in_for_pruned {
            return Ok(None);
        }
        let replay = match pruned || missing {
            false => FrameReplay::Local,
            true => FrameReplay::Peers,
        };
        Ok(Some(SealRebootstrap {
            scope,
            listed,
            replay,
        }))
    }
    async fn replay_scope(
        &mut self,
        scope: ResourceScope,
        id: i64,
        reset: bool,
    ) -> Result<(), Error> {
        if reset {
            self.clear_seal_anchor(id).await?;
        } else {
            self.anchor_seal_coverage(scope, id).await?;
        }
        self.clear_checkpoints(scope, id).await?;
        for table in ["secure_seal_bounds", "secure_seal_drops"] {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        if !reset {
            return Ok(());
        }
        self.save_eviction_guard(scope).await?;
        let blob = encode(&scope)?;
        self.conn
            .execute(
                "UPDATE secure_operations SET status='pending' WHERE hash IN(SELECT hash FROM secure_applied WHERE scope=?)",
                [id],
            )
            .await?;
        self.clear_applied_cache();
        for sql in [
            "DELETE FROM secure_row_records WHERE scope=?",
            "DELETE FROM secure_move_rows WHERE scope=?",
            "DELETE FROM secure_identity_ledger WHERE scope=?",
        ] {
            self.conn.execute(sql, [blob.clone()]).await?;
        }
        for sql in [
            "DELETE FROM secure_applied WHERE scope=?",
            "DELETE FROM secure_logs WHERE scope=?",
            "DELETE FROM secure_stubs WHERE scope=?",
        ] {
            self.conn.execute(sql, [id]).await?;
        }
        self.store_scope_state(id, 0, 0, &checkpoint::LtHash16::new(), 0)
            .await?;
        if self.replayed_scope(scope) {
            self.clear_replayed(scope).await?;
            return self
                .replay_from_frames(now()?)
                .await?
                .map_err(|broken| broken.refuses(Rebuilding::Rebootstrap));
        }
        self.reproject(scope).await
    }
    pub(in crate::replica) async fn install_needed_pulled(
        &mut self,
        domain: Scope,
    ) -> Result<bool, Error> {
        let mut installed = false;
        for need in self.seal_rebootstraps(domain).await? {
            let Some(id) = self.find_scope_id(need.scope).await? else {
                continue;
            };
            if let Some(seal) = self.pulled_seal(id).await?
                && self.path_index(domain, seal)?.is_some()
            {
                installed |= self.install_pulled(need.scope, id).await?
                    == checkpoint::pulled::PulledInstall::Installed;
            }
        }
        Ok(installed)
    }
    pub(in crate::replica) async fn settle_seal_rebootstrap(
        &mut self,
        domain: Scope,
    ) -> Result<bool, Error> {
        self.install_needed_pulled(domain).await?;
        self.fault(checkpoint::Stage::Settle)?;
        if !self.seal_rebootstraps(domain).await?.is_empty() {
            return Ok(false);
        }
        if self.compaction_lost(domain).await?.is_none() {
            self.rederive_replayed(None, now()?).await?;
            return Ok(true);
        }
        for (scope, id) in self.domain_scopes(domain).await? {
            let based = match self.base_seal(id).await? {
                Some(seal) => self.path_index(domain, seal)?.is_some(),
                None => false,
            };
            if !based {
                self.anchor_seal_coverage(scope, id).await?;
            }
        }
        let blob = encode(&domain)?;
        for sql in [
            "DELETE FROM secure_finalized WHERE domain=?",
            "DELETE FROM secure_compaction_lost WHERE domain=?",
        ] {
            self.conn.execute(sql, [blob.clone()]).await?;
        }
        let mut reordered = false;
        for (scope, id) in self.domain_scopes(domain).await? {
            if self.replayed_scope(scope) {
                reordered |= self.join_open_era(id, 0).await?;
            }
            self.conn
                .execute(
                    "UPDATE secure_kept_outcomes SET sealed=0,based=0 WHERE scope=?",
                    [id],
                )
                .await?;
            for table in ["secure_seal_bounds", "secure_seal_drops"] {
                self.conn
                    .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                    .await?;
            }
            self.conn
                .execute(
                    "DELETE FROM secure_applied WHERE scope=? AND hash IN(SELECT hash FROM secure_operations WHERE status='held')",
                    [id],
                )
                .await?;
        }
        self.rebuild_tip_workspace(domain).await?;
        if reordered {
            self.replay_from_frames(now()?)
                .await?
                .map_err(|broken| broken.refuses(Rebuilding::Rebootstrap))?;
        }
        self.rederive_replayed(None, now()?).await?;
        Ok(true)
    }
}

pub(in crate::replica) struct RebootstrapPlan {
    domain: Scope,
    path: Vec<Commitment>,
    unverified: BTreeSet<ResourceScope>,
    scopes: Vec<(ResourceScope, Option<i64>)>,
}

impl<A: RowApplication> Inner<A> {
    fn rebootstrap_context(&self) -> (u64, u64, TipId, u64) {
        (
            self.readiness.writes(),
            self.authority,
            self.workspace.tip,
            self.context_epoch.load(Ordering::Relaxed),
        )
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn seal_rebootstraps(&self, domain: Scope) -> Result<Vec<SealRebootstrap>, Error> {
        'pass: for _ in 0..super::super::headers::SCAN_PASSES {
            let (plan, context) = {
                let inner = self.inner.lock(crate::replica::Class::Background).await;
                inner.graph_for(domain)?;
                let Some(plan) = inner.rebootstrap_plan(domain).await? else {
                    return Ok(Vec::new());
                };
                (plan, inner.rebootstrap_context())
            };
            let mut needs = Vec::new();
            for (scope, id) in &plan.scopes {
                rocky_platform::task::yield_now().await;
                let inner = self.inner.lock(crate::replica::Class::Background).await;
                if inner.rebootstrap_context() != context {
                    continue 'pass;
                }
                needs.extend(inner.seal_rebootstrap_of(&plan, *scope, *id).await?);
            }
            return Ok(needs);
        }
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.seal_rebootstraps(domain).await
    }
    pub async fn replay_at_seals(&self, scope: ResourceScope, reset: bool) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut need = None;
        if let Some(plan) = inner.rebootstrap_plan(scope.domain).await?
            && let Some((_, id)) = plan.scopes.iter().find(|(held, _)| *held == scope)
        {
            need = inner.seal_rebootstrap_of(&plan, scope, *id).await?;
        }
        let need =
            need.ok_or_else(|| denied("the scope's base sits at a seal of the followed path"))?;
        if need.replay == FrameReplay::Peers && !reset {
            return Err(denied("pruned frames must come again from a peer"));
        }
        let id = inner.scope_id(scope).await?;
        transaction!(inner, |inner| {
            inner.replay_scope(scope, id, reset).await?;
            inner.settle_seal_rebootstrap(scope.domain).await?;
            Ok(())
        })?;
        inner.verified_at.remove(&id);
        inner = flush_guard(inner, &self.inner).await?;
        inner.publish_revision().await?;
        drop(inner);
        self.settle().await?;
        self.restored(scope).await?;
        self.finish_seal_rebootstrap(scope.domain).await?;
        Ok(())
    }
    pub async fn finish_seal_rebootstrap(&self, domain: Scope) -> Result<bool, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.graph_for(domain)?;
        let settled = transaction!(inner, |inner| inner.settle_seal_rebootstrap(domain).await)?;
        inner = flush_guard(inner, &self.inner).await?;
        inner.publish_revision().await?;
        Ok(settled)
    }
    pub async fn unlisted_checkpoints(
        &self,
        domain: Scope,
    ) -> Result<Vec<CheckpointRecordStatement>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let graph = inner.graph_for(domain)?;
        let mut records = Vec::new();
        for (scope, id) in inner.domain_scopes(domain).await? {
            let Some(signed) = inner
                .checkpoint_record(id, true)
                .await?
                .and_then(|record| record.signed)
            else {
                continue;
            };
            let Some(seal) = signed.statement.seal else {
                continue;
            };
            let checkpoint = signed.id()?;
            if !inner.owns_key(signed.signer())
                || graph.seal_by_id(seal).is_err()
                || graph
                    .checkpoints
                    .iter()
                    .any(|record| record.statement.checkpoint == checkpoint)
            {
                continue;
            }
            records.push(CheckpointRecordStatement {
                format: CHECKPOINT_RECORD_FORMAT,
                scope: domain,
                seal,
                table: scope,
                checkpoint,
                signer: signed.signer(),
                rows: signed.statement.rows.count,
            });
        }
        Ok(records)
    }
}
