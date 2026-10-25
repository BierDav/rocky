//! Terminal entries: a seal of a tip or a bookmark move off it ends the
//! tip's log. Entries committed before it belong to the sealed state; an
//! own write it aborted is carried into the continuation's log and retried
//! there once, or held after a `close`. See `docs/runtime/coordinated-writes.md`.
use super::*;
use rocky_core::LogTerminal;

/// Keeps a background decision's storage failure; others leave the write
/// to a later resolution or report it to the proposer.
pub(super) fn settled(result: Result<CoordinatedCommit, CoordinatedError>) -> Result<(), Error> {
    match result {
        Err(CoordinatedError::Storage(error)) => Err(error),
        _ => Ok(()),
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Ends `log` with `terminal`, the digest of a seal of its tip or of a
    /// bookmark move off it, then decides this node's entries of it. A
    /// retry of the same terminal answers the same position; another
    /// terminal already ended the log with `Sealed`.
    pub async fn seal_log(
        &self,
        log: TipLog,
        terminal: LogTerminal,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<u64, CoordinatedError> {
        let position = match coordinator.seal(log, terminal).await {
            Ok(LogResult::Committed { position, .. }) => position,
            Ok(LogResult::Acknowledged) => {
                return Err(Error::Corrupt("the coordinator did not commit a seal".into()).into());
            }
            Err(error) => return Err(coordination(error)),
        };
        match self.resolve_coordinated(log, coordinator, peers).await {
            Ok(_) | Err(CoordinatedError::Behind | CoordinatedError::Unavailable) => Ok(position),
            Err(error) => Err(error),
        }
    }
    /// Before this node takes anything of a tip's log, it holds and applied
    /// every entry the log of the tip it starts from committed before the
    /// seal it starts at, or that a recovery retained: the state the tip's
    /// writes compute on. That holds for a continuation and for a branch
    /// opened on a seal that ended its log. An unopened parent log, or one
    /// the coordinator dropped, has nothing to verify.
    pub(super) async fn hold_sealed_entries(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<(), CoordinatedError> {
        // The parent tip, the seal the tip starts at, and whether it opens a
        // branch.
        let start = {
            let inner = self.inner.lock().await;
            let mut start = None;
            if let Some(graph) = inner.graphs.get(&log.scope) {
                for seal in &graph.seals {
                    if seal.successor().map_err(credential)? == Some(log.tip) {
                        start = Some((seal.clone(), false));
                    }
                }
                for opening in &graph.openings {
                    if opening.tip().map_err(credential)? == log.tip {
                        let seal = graph
                            .seal_by_id(opening.statement.seal)
                            .map_err(credential)?;
                        start = Some((seal.clone(), true));
                    }
                }
            }
            start
        };
        let Some((seal, branch)) = start else {
            return Ok(());
        };
        let parent = TipLog {
            scope: log.scope,
            tip: seal.statement.tip,
        };
        let head = coordinator.log_head(parent).await.map_err(coordination)?;
        if let Some(terminal) = head.terminal
            && (!branch || terminal.digest == seal.id().map_err(credential)?)
        {
            let mut through = head.commit.saturating_sub(1);
            if let Some(recovery) = seal.statement.recovery {
                through = through.min(recovery.retained.position);
            }
            if self.inner.lock().await.log_state(parent).await?.0 < through {
                match Box::pin(self.resolve(parent, coordinator, peers)).await {
                    Ok(_) => {}
                    Err(CoordinatedError::Behind) if branch => {}
                    Err(error) => return Err(error),
                }
                if !branch && self.inner.lock().await.log_state(parent).await?.0 < through {
                    return Err(CoordinatedError::Behind);
                }
            }
            let migration = branch
                && matches!(seal.statement.action, rocky_credentials::SealAction::Close)
                && {
                    let inner = self.inner.lock().await;
                    let graph = inner.graph_for(log.scope)?;
                    graph.schema(log.tip).map_err(credential)?
                        != graph.schema_any(parent.tip).map_err(credential)?
                };
            if migration {
                self.require_inherited(parent, log.tip).await?;
            } else {
                self.require_applied(parent).await?;
            }
        }
        let inner = self.inner.lock().await;
        if !inner.seal_rebootstraps(log.scope).await?.is_empty() {
            return Err(CoordinatedError::Behind);
        }
        let scopes = inner.readiness_scopes(log.scope).await?;
        if inner
            .seal_waits(log.scope, &seal, None, &scopes, true)
            .await?
            .is_some()
        {
            return Err(inner.missing_placement(log.scope));
        }
        Ok(())
    }
    /// A migration inherits the signed frontier, while the terminal fixes
    /// the source log's committed extent. Later frames have a held disposition.
    async fn require_inherited(
        &self,
        parent: TipLog,
        target: TipId,
    ) -> Result<(), CoordinatedError> {
        let inner = self.inner.lock().await;
        let graph = inner.graph_for(parent.scope)?;
        for entry in inner.entries(parent, &["committed"]).await? {
            let ids: Vec<Hash> = decode_bounded(&entry.frames, MAX_SECURE_BYTES)?;
            for id in ids {
                if let Some(frame) = inner.frame(id).await? {
                    let header = frame.operation_ref()?.header.clone();
                    let e = &header.extensions;
                    let inherited = inner.visible_frame(
                        graph,
                        e.tip,
                        e.author.ok_or_else(|| denied("inherited author missing"))?,
                        e.author_sequence,
                        target,
                    )?;
                    let status = inner.status(id).await?;
                    if (inherited && status != ReceiveStatus::Applied)
                        || (!inherited && status != ReceiveStatus::Held)
                    {
                        return Err(CoordinatedError::Behind);
                    }
                } else {
                    // Retained held bytes and authenticated void positions are
                    // durable dispositions. Missing inherited history is checked
                    // by seal_waits below, including verified replay bases.
                    let held = collect_rows(inner.conn.query(
                        "SELECT 1 FROM secure_held_frames WHERE hash=?1 UNION ALL SELECT 1 FROM secure_voids WHERE hash=?1",
                        [id.as_bytes().to_vec()],
                    ).await.map_err(Error::from)?).await?;
                    if held.is_empty() {
                        let scopes = inner.readiness_scopes(parent.scope).await?;
                        let seal = graph
                            .seal(parent.tip)
                            .map_err(credential)?
                            .ok_or(CoordinatedError::Behind)?;
                        if inner
                            .seal_waits(parent.scope, seal, None, &scopes, true)
                            .await?
                            .is_some()
                        {
                            return Err(inner.missing_placement(parent.scope));
                        }
                    }
                }
            }
        }
        Ok(())
    }
    /// Whether a terminal entry took `entry`'s position.
    pub(super) async fn ended(
        &self,
        log: TipLog,
        entry: &Entry,
        coordinator: &dyn DomainLog,
    ) -> Result<bool, CoordinatedError> {
        Ok(coordinator
            .log_head(log)
            .await
            .is_ok_and(|head| head.terminal.is_some() && head.commit == entry.position))
    }
    /// Ends an own write whose log ended: carried into the continuation's
    /// log and retried there, or held without a continuation. A write the
    /// terminal entry did not abort is stale and aborts.
    pub(super) async fn ended_own(
        &self,
        log: TipLog,
        entry: &Entry,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<CoordinatedCommit, CoordinatedError> {
        let head = coordinator.log_head(log).await.map_err(coordination)?;
        let Some(terminal) = head.terminal.filter(|_| head.commit == entry.position) else {
            self.abort_own(log, entry).await?;
            return Err(CoordinatedError::Conflict);
        };
        let continuation = self.inner.lock().await.continuation(log, terminal)?;
        // A recovery that abandoned entries left the state the write was
        // computed on: it is held for re-authoring, never carried.
        if self.inner.lock().await.abandoning(log)? {
            self.void_own(log, entry, true).await?;
            return Err(CoordinatedError::Sealed);
        }
        match continuation {
            Some(tip) if tip != log.tip => {
                let next = TipLog {
                    scope: log.scope,
                    tip,
                };
                self.carry(log, next, entry).await?;
                self.retry_carried(next, coordinator, peers).await
            }
            _ => {
                self.void_own(log, entry, true).await?;
                Err(CoordinatedError::Sealed)
            }
        }
    }
    /// Moves an own unsent write to `next`'s log, where it waits unsent
    /// until this node follows that tip. One already waiting there holds it.
    async fn carry(&self, log: TipLog, next: TipLog, entry: &Entry) -> Result<(), Error> {
        let waiting = !self
            .inner
            .lock()
            .await
            .entries(next, &["staged", "assigned", "carried"])
            .await?
            .is_empty();
        if waiting {
            return self.void_own(log, entry, true).await;
        }
        let ids: Vec<Hash> = decode_bounded(&entry.frames, MAX_SECURE_BYTES)?;
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "UPDATE secure_entries SET log=?,state='carried',position=0,pending=NULL,append=NULL,carry=NULL WHERE log=? AND hash=?",
                    [encode(&next)?, encode(&log)?, entry.hash.to_vec()],
                )
                .await?;
            for id in &ids {
                inner
                    .conn
                    .execute(
                        "UPDATE secure_entry_frames SET log=?,position=0 WHERE hash=?",
                        [encode(&next)?, id.as_bytes().to_vec()],
                    )
                    .await?;
            }
            Ok(())
        })?;
        inner.flush()
    }
    /// Retries the write carried into `log` as its first entry, once this
    /// node follows the tip and nothing committed there yet: the write was
    /// computed on the sealed state the tip starts from. Otherwise it waits
    /// (`Undecided`), moves on to a further continuation, or is held
    /// (`Sealed`).
    pub(super) async fn retry_carried(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<CoordinatedCommit, CoordinatedError> {
        let carried = self.inner.lock().await.entries(log, &["carried"]).await?;
        let Some(entry) = carried.into_iter().next() else {
            return Err(denied("no coordinated write is carried into this log").into());
        };
        // A recovery that abandoned entries of the log it was carried from
        // left the state it was computed on.
        if self.inner.lock().await.recovered_from_abandoning(log)? {
            self.void_own(log, &entry, true).await?;
            return Err(CoordinatedError::Sealed);
        }
        // A continuation whose seal lost hands the write to the winner's.
        let canonical = self.inner.lock().await.canonical(log)?;
        if canonical != log.tip {
            let next = TipLog {
                scope: log.scope,
                tip: canonical,
            };
            self.carry(log, next, &entry).await?;
            return Err(CoordinatedError::Undecided);
        }
        if log != self.followed_log(log.scope).await {
            return Err(CoordinatedError::Undecided);
        }
        let mut head = coordinator.log_head(log).await.map_err(coordination)?;
        if let Some(terminal) = head.terminal {
            let continuation = self.inner.lock().await.continuation(log, terminal)?;
            match continuation {
                Some(tip) if tip != log.tip => {
                    let next = TipLog {
                        scope: log.scope,
                        tip,
                    };
                    self.carry(log, next, &entry).await?;
                    return Err(CoordinatedError::Undecided);
                }
                _ => {
                    self.void_own(log, &entry, true).await?;
                    return Err(CoordinatedError::Sealed);
                }
            }
        }
        if head.pending.is_some() {
            return Err(CoordinatedError::Undecided);
        }
        let (position, _, _, _) = self.inner.lock().await.log_state(log).await?;
        if head.commit != 0 || position != 0 {
            // Others wrote here first, on state the write never saw.
            self.void_own(log, &entry, true).await?;
            return Err(CoordinatedError::Sealed);
        }
        self.inner.lock().await.learn_genesis(log, &head).await?;
        let me = coordinator.node();
        if !head.member(&me) {
            self.hold_sealed_entries(log, coordinator, peers).await?;
            self.configure_line(log, &head, coordinator).await?;
            let sequence = {
                let mut inner = self.inner.lock().await;
                let sequence = transaction!(inner, |inner| inner.next_sequence(log).await)?;
                inner.flush()?;
                sequence
            };
            let epoch = coordinator
                .join(log, 0, head.running_hash, sequence)
                .await
                .map_err(coordination)?;
            head = coordinator.log_head(log).await.map_err(coordination)?;
            if head.epoch != epoch || !head.member(&me) || head.commit != 0 {
                return Err(CoordinatedError::Conflict);
            }
        }
        if head.members.len() < head.minimum {
            return Err(CoordinatedError::BelowMinimum);
        }
        let ids: Vec<Hash> = decode_bounded(&entry.frames, MAX_SECURE_BYTES)?;
        let hash = entry_hash(log, 1, &ids)?;
        let bundle = entry
            .bundle
            .ok_or_else(|| Error::Corrupt("carried entry without its bundle".into()))?;
        // Its frames keep the coordinate they sign; in a tip graph the
        // author's record names where they are retried.
        let carry = {
            let inner = self.inner.lock().await;
            match inner.graphs.contains_key(&log.scope) {
                true => Some(inner.sign_carry(log, &ids).await?),
                false => None,
            }
        };
        let carry = match carry {
            Some(Some(carry)) => Some(carry),
            Some(None) => {
                self.void_own(log, &entry, true).await?;
                return Err(CoordinatedError::Sealed);
            }
            None => None,
        };
        {
            let mut inner = self.inner.lock().await;
            transaction!(inner, |inner| {
                let append = Append {
                    expected: 0,
                    epoch: head.epoch,
                    hash,
                    duration: ENTRY_DEADLINE,
                    sequence: inner.next_sequence(log).await?,
                };
                inner
                    .conn
                    .execute(
                        "DELETE FROM secure_entries WHERE log=? AND hash=?",
                        [encode(&log)?, entry.hash.to_vec()],
                    )
                    .await?;
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_entries VALUES(?,?,1,'staged',?,?,NULL,NULL,?,NULL)",
                        [
                            encode(&log)?,
                            hash.to_vec(),
                            bundle.to_vec(),
                            entry.frames.clone(),
                            encode(&append)?,
                        ],
                    )
                    .await?;
                if let Some(carry) = &carry {
                    inner
                        .conn
                        .execute(
                            "UPDATE secure_entries SET carry=? WHERE log=? AND hash=?",
                            [
                                carry.to_bytes().map_err(credential)?,
                                encode(&log)?,
                                hash.to_vec(),
                            ],
                        )
                        .await?;
                }
                for id in &ids {
                    inner
                        .conn
                        .execute(
                            "UPDATE secure_entry_frames SET position=1 WHERE hash=?",
                            [id.as_bytes().to_vec()],
                        )
                        .await?;
                }
                Ok(())
            })?;
            inner.flush()?;
        }
        Box::pin(self.propose_staged(log, coordinator, peers)).await
    }
}
