//! Rebootstrap at a seal of the followed path: a held scope whose replay
//! base left the path, because its compacted seal lost a race, or that was
//! pruned under a checkpoint at another cut imports a checkpoint the graph
//! lists at a seal of the path, else replays frames. See
//! `docs/runtime/lazy-compaction.md#rebootstrap`.
use super::*;
use rocky_credentials::{CHECKPOINT_RECORD_FORMAT, CheckpointRecordStatement};

/// How a scope without a listed checkpoint is rebuilt from frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameReplay {
    /// It holds every frame it applied: its rows are rebuilt from them.
    Local,
    /// Frames were pruned: it keeps the ones it holds and the rest come
    /// again from a peer that retains them.
    Peers,
    /// Its existence records were released at a seal off the path; only a
    /// checkpoint with a signed existence base replaces them.
    Unavailable,
}

/// A held scope whose replay base must move to a seal of the followed path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealRebootstrap {
    pub scope: ResourceScope,
    /// Checkpoints of the scope the graph lists at seals of the path that
    /// another node signed, newest seal first.
    pub listed: Vec<Commitment>,
    pub replay: FrameReplay,
    /// The scope keeps an existence log, whose base comes with the import.
    pub logged: bool,
}

impl<A: RowApplication> Inner<A> {
    /// The seal scope `id`'s replay base sits at, if any.
    pub(in crate::replica) async fn base_seal(&self, id: i64) -> Result<Option<Commitment>, Error> {
        Ok(self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed)
            .and_then(|signed| signed.statement.seal))
    }
    fn owns_key(&self, key: PublicKey) -> bool {
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
    /// Whether scope `id`'s replay base sits at a seal off the followed
    /// path: its state is no longer one the path passes through.
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
    /// Read admission of `scope`: the workspace-wide blockers, then only
    /// this scope's own state. A scope another scope's rebootstrap waits
    /// for stays readable; one that waits itself reads only the verified
    /// pages of a direct import. Authoring keeps `require_workspace_base`.
    pub(in crate::replica) async fn require_scope_readable(
        &self,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        let domain = scope.domain;
        let waits = || denied("the workspace waits for a verified seal rebootstrap");
        if self.graphs.contains_key(&domain) {
            for (_, id) in self.domain_scopes(domain).await? {
                if self.unsealed_import(id).await? {
                    return Err(waits());
                }
            }
        }
        if self.compaction_lost(domain).await?.is_some() {
            return Err(waits());
        }
        if self
            .seal_rebootstraps(domain)
            .await?
            .iter()
            .all(|need| need.scope != scope)
            || self.authenticated_direct_import(scope).await?
        {
            return Ok(());
        }
        Err(waits())
    }
    pub(in crate::replica) async fn require_workspace_base(
        &self,
        domain: Scope,
    ) -> Result<(), Error> {
        if self.graphs.contains_key(&domain) {
            // An evicted scope reads as unavailable; its own log returning
            // holds back only authoring.
            for (_, id) in self.domain_scopes(domain).await? {
                if self.unsealed_import(id).await? {
                    return Err(denied(
                        "the workspace waits for a verified seal rebootstrap",
                    ));
                }
            }
        }
        if self.compaction_lost(domain).await?.is_some()
            || !self.seal_rebootstraps(domain).await?.is_empty()
        {
            return Err(denied(
                "the workspace waits for a verified seal rebootstrap",
            ));
        }
        Ok(())
    }
    pub(in crate::replica) async fn seal_rebootstraps(
        &self,
        domain: Scope,
    ) -> Result<Vec<SealRebootstrap>, Error> {
        let Some(graph) = self.graphs.get(&domain) else {
            return Ok(Vec::new());
        };
        let path = self
            .path_seals(domain)?
            .iter()
            .map(|seal| seal.id().map_err(credential))
            .collect::<Result<Vec<_>, _>>()?;
        let unverified = self.unverified_scopes(domain).await?;
        let mut needs = Vec::new();
        for (scope, id) in self.readiness_scopes(domain).await? {
            let unknown = unverified.iter().any(|(candidate, _)| *candidate == scope);
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
                        .seal_waits(domain, seal, None, &[(scope, id)], true)
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
                continue;
            }
            let mut listed = Vec::new();
            for seal in path.iter().rev() {
                for record in graph.checkpoints_at(*seal).map_err(credential)? {
                    if record.statement.table == scope && !self.owns_key(record.statement.signer) {
                        listed.push(record.statement.checkpoint);
                    }
                }
            }
            let logged = self.existence_table(scope);
            let released = logged
                && self
                    .existence_base(scope)
                    .await?
                    .and_then(|base| base.seal())
                    .is_some_and(|seal| !path.contains(&seal));
            if unknown && !missing && !pruned && base.is_none_or(|seal| path.contains(&seal)) {
                continue;
            }
            let replay = match (released, pruned || missing) {
                (true, _) => FrameReplay::Unavailable,
                (false, false) => FrameReplay::Local,
                (false, true) => FrameReplay::Peers,
            };
            needs.push(SealRebootstrap {
                scope,
                listed,
                replay,
                logged,
            });
        }
        Ok(needs)
    }
    /// Inside the caller's transaction, drops scope `id`'s replay base so
    /// its rows are rebuilt from frames. With `reset`, its applied state goes
    /// too: what it holds waits, and the frames it pruned apply again as
    /// they arrive.
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
        // Its own frames come back as after an eviction; it authors again
        // once its log is whole.
        self.save_eviction_guard(scope).await?;
        let blob = encode(&scope)?;
        self.conn
            .execute(
                "UPDATE secure_operations SET status='pending' WHERE hash IN(SELECT hash FROM secure_applied WHERE scope=?)",
                [id],
            )
            .await?;
        // The frames replayed from the start fold the replay ledger again;
        // permanent marks and admission evidence stay.
        for sql in [
            "DELETE FROM secure_rows WHERE scope=?",
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
        self.store_scope_state(id, 0, 0, &checkpoint::Lt::new(), 0)
            .await?;
        self.reproject(scope).await
    }
    /// Inside the caller's transaction, installs the pulled state of every
    /// scope that needs a seal rebootstrap and has one at a seal of the
    /// path. Install checks it again; a refused state is dropped and its
    /// scope stays listed. Returns whether any state installed.
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
    /// Inside the caller's transaction, once every held scope's base sits at
    /// a seal of the path or was dropped for a replay: a domain whose
    /// compacted seal lost forgets its finalized seals, which it finalizes
    /// again from the path's start, and its rows are rebuilt at the
    /// followed tip. Returns whether no scope waits.
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
        for (_, id) in self.domain_scopes(domain).await? {
            for table in ["secure_seal_bounds", "secure_seal_drops"] {
                self.conn
                    .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                    .await?;
            }
            // Held frames the switch indexed before an imported base are
            // indexed again after it.
            self.conn
                .execute(
                    "DELETE FROM secure_applied WHERE scope=? AND hash IN(SELECT hash FROM secure_operations WHERE status='held')",
                    [id],
                )
                .await?;
        }
        self.rebuild_tip_workspace(domain).await?;
        Ok(true)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The held scopes of `domain` whose replay base must move to a seal of
    /// the followed path, with the checkpoints the graph lists for them.
    pub async fn seal_rebootstraps(&self, domain: Scope) -> Result<Vec<SealRebootstrap>, Error> {
        let inner = self.inner.lock().await;
        inner.graph_for(domain)?;
        inner.seal_rebootstraps(domain).await
    }
    /// Rebuilds a scope that needs a seal rebootstrap from frames instead of
    /// a checkpoint: from those it holds, or with `reset` from those a peer
    /// sends again. Refused where existence records were released at a
    /// seal off the path.
    pub async fn replay_at_seals(&self, scope: ResourceScope, reset: bool) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let need = inner
            .seal_rebootstraps(scope.domain)
            .await?
            .into_iter()
            .find(|need| need.scope == scope)
            .ok_or_else(|| denied("the scope's base sits at a seal of the followed path"))?;
        match (need.replay, reset) {
            (FrameReplay::Unavailable, _) => {
                return Err(denied(
                    "existence records were released at a seal off the path; a checkpoint at the winning seal replaces them",
                ));
            }
            (FrameReplay::Peers, false) => {
                return Err(denied("pruned frames must come again from a peer"));
            }
            _ => {}
        }
        let id = inner.scope_id(scope).await?;
        transaction!(inner, |inner| {
            inner.replay_scope(scope, id, reset).await?;
            inner.settle_seal_rebootstrap(scope.domain).await?;
            Ok(())
        })?;
        inner.verified_at.remove(&id);
        inner.flush()?;
        inner.publish_revision().await
    }
    /// Ends a seal rebootstrap of `domain` once no held scope waits: a
    /// compacted seal that lost is forgotten and the rows are rebuilt at
    /// the followed tip. Returns whether none waits.
    pub async fn finish_seal_rebootstrap(&self, domain: Scope) -> Result<bool, Error> {
        let mut inner = self.inner.lock().await;
        inner.graph_for(domain)?;
        let settled = transaction!(inner, |inner| inner.settle_seal_rebootstrap(domain).await)?;
        inner.flush()?;
        inner.publish_revision().await?;
        Ok(settled)
    }
    /// The replay bases at seals of `domain` this node signed that the
    /// graph does not list yet: records the owner, a trusted identity or an
    /// issuer may sign so other devices can rebootstrap from them.
    pub async fn unlisted_checkpoints(
        &self,
        domain: Scope,
    ) -> Result<Vec<CheckpointRecordStatement>, Error> {
        let inner = self.inner.lock().await;
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
