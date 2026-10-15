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
        match terminal.continuation {
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
                    "UPDATE secure_entries SET log=?,state='carried',position=0,pending=NULL,append=NULL WHERE log=? AND hash=?",
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
        if log != self.followed_log(log.scope).await {
            return Err(CoordinatedError::Undecided);
        }
        let mut head = coordinator.log_head(log).await.map_err(coordination)?;
        if let Some(terminal) = head.terminal {
            match terminal.continuation {
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
        {
            let mut inner = self.inner.lock().await;
            transaction!(inner, |inner| {
                let append = Append {
                    expected: 0,
                    epoch: head.epoch,
                    hash,
                    deadline: now()? + ENTRY_DEADLINE,
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
                        "INSERT INTO secure_entries VALUES(?,?,1,'staged',?,?,NULL,NULL,?)",
                        [
                            encode(&log)?,
                            hash.to_vec(),
                            bundle.to_vec(),
                            entry.frames.clone(),
                            encode(&append)?,
                        ],
                    )
                    .await?;
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
