//! Recovery that needs no peer. The SDK drives it at open, after the tip
//! graph or a commit changes and on a timer while local work remains; it is
//! never called inside a network call or a peer session's transaction.
use super::*;

impl<A: RowApplication> EncryptedReplica<A> {
    /// Applies pending frames, rebuilds scopes whose rebootstrap replays the
    /// frames they hold, ends restorations whose own log is back, forgets
    /// dropped tip frames a listed base covers and ends a seal rebootstrap
    /// no scope waits for. One scope's failure does not stop the others;
    /// the first is returned once all ran.
    pub async fn recover_locally(&self) -> Result<(), Error> {
        let mut first = None;
        let mut keep = |result: Result<(), Error>| {
            if let Err(error) = result {
                first.get_or_insert(error);
            }
        };
        keep(self.settle().await);
        if let Some((domain, _)) = self.followed_bookmark().await {
            let needs = self.seal_rebootstraps(domain).await?;
            for need in &needs {
                if need.replay == FrameReplay::Local {
                    keep(self.replay_at_seals(need.scope, false).await);
                }
            }
            if !needs.is_empty() || self.compaction_status(domain).await?.rebootstrap.is_some() {
                keep(self.finish_seal_rebootstrap(domain).await.map(drop));
            }
        }
        for scope in self.restoring().await? {
            keep(self.restored(scope).await);
        }
        keep(self.forget_covered_drops().await);
        first.map_or(Ok(()), Err)
    }
    /// Whether `scope` applied a frame whose author is revoked now: its
    /// rows are restored from a checkpoint, never kept from frames.
    pub async fn holds_revoked_frames(&self, scope: ResourceScope) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        if inner.config.revocations.is_empty() {
            return Ok(false);
        }
        for record in collect_rows(
            inner
                .conn
                .query(
                    "SELECT header,body FROM secure_operations WHERE scope=? AND status='applied'",
                    [encode(&scope)?],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else {
                return Err(Error::Corrupt("applied operation".into()));
            };
            let frame = inner.stored_frame(header.clone(), body.clone()).await?;
            if inner
                .config
                .revocations
                .revoking(frame.chain()?.credentials())
                .map_err(credential)?
                .is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Scopes whose eviction guard waits for this node's own log.
    async fn restoring(&self) -> Result<Vec<ResourceScope>, Error> {
        let inner = self.inner.lock().await;
        rows(&inner.conn, "SELECT scope FROM secure_evicted_logs")
            .await?
            .iter()
            .map(|record| match record.as_slice() {
                [SqlValue::Blob(scope)] => decode(scope),
                _ => Err(Error::Corrupt("eviction guard".into())),
            })
            .collect()
    }
    /// Forgets dropped tip frames that came back, or whose scope's
    /// verified base, listed in the graph at a seal of the followed path,
    /// holds their tip's state.
    async fn forget_covered_drops(&self) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let mut forget = Vec::new();
        for record in rows(
            &inner.conn,
            "SELECT d.tip,d.hash,d.scope,EXISTS(SELECT 1 FROM secure_operations o WHERE o.hash=d.hash) FROM secure_dropped_tip_frames d",
        )
        .await?
        {
            let [
                SqlValue::Blob(tip),
                SqlValue::Blob(hash),
                SqlValue::Blob(scope),
                SqlValue::Integer(back),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("dropped tip frame".into()));
            };
            if *back == 1 || inner.base_covers(decode(scope)?, decode(tip)?).await? {
                forget.push((tip.clone(), hash.clone()));
            }
        }
        if forget.is_empty() {
            return Ok(());
        }
        transaction!(inner, |inner| {
            for (tip, hash) in &forget {
                inner
                    .conn
                    .execute(
                        "DELETE FROM secure_dropped_tip_frames WHERE tip=? AND hash=?",
                        [tip.clone(), hash.clone()],
                    )
                    .await?;
            }
            Ok(())
        })?;
        inner = flush_guard(inner, &self.inner).await?;
        inner.publish_revision().await
    }
}

impl<A: RowApplication> Inner<A> {
    /// Whether `scope`'s verified base sits at a seal of the followed path
    /// the graph lists a checkpoint of it at, and that seal's state holds
    /// `tip`: the tip is the sealed one or one of its ancestors.
    async fn base_covers(&self, scope: ResourceScope, tip: TipId) -> Result<bool, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(false);
        };
        let Some(record) = self.checkpoint_record(id, true).await? else {
            return Ok(false);
        };
        let Ok(signed) = self.verify_record(scope, &record) else {
            return Ok(false);
        };
        let Some(seal) = signed.statement.seal else {
            return Ok(false);
        };
        let Some(graph) = self.graphs.get(&scope.domain) else {
            return Ok(false);
        };
        let checkpoint = signed.id()?;
        if self.path_index(scope.domain, seal)?.is_none()
            || !graph.checkpoints.iter().any(|listed| {
                listed.statement.table == scope && listed.statement.checkpoint == checkpoint
            })
        {
            return Ok(false);
        }
        let mut cursor = graph.seal_by_id(seal).map_err(credential)?.statement.tip;
        let mut visited = BTreeSet::new();
        while visited.insert(cursor) {
            if cursor == tip {
                return Ok(true);
            }
            let Some((parent, _)) = Self::tip_parent(graph, cursor)? else {
                break;
            };
            cursor = parent;
        }
        Ok(false)
    }
}
