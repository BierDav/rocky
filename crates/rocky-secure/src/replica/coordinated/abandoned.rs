//! Entries the owner's recovery seal of a stuck tip abandons. Every device
//! holds their frames for their authors instead of applying them, one that
//! applied them rolls them back with the seal's admission, and trusted
//! nodes catch the sealed log up only through its retained prefix. See
//! `docs/runtime/stuck-recovery.md`.
use super::*;
use crate::replica::incoming::Outcome;
use rocky_core::TipId;
use rocky_credentials::{LogPoint, Recovery, SignedCarry};

pub(in crate::replica) const SCHEMA: &[&str] = &[
    // The log position of each frame of a coordinated write.
    "CREATE TABLE IF NOT EXISTS secure_entry_frames(hash BLOB PRIMARY KEY,log BLOB NOT NULL,position INTEGER NOT NULL) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_entry_frames_position ON secure_entry_frames(log,position)",
];
/// Positions an owner-endorsed gap before the hash window may span.
const MAX_ENDORSED: u64 = 1 << 20;

/// Why the replica refused a call. `code` is the JSON error code.
#[derive(Debug)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Refusal {}
pub(in crate::replica) fn refusal(code: &'static str, message: &str) -> Error {
    Error::Application(Box::new(Refusal {
        code,
        message: message.into(),
    }))
}

impl<A: RowApplication> Inner<A> {
    /// The recovery of `log`'s tip, when the owner's recovery seal won it.
    pub(in crate::replica) fn recovery(&self, log: TipLog) -> Result<Option<Recovery>, Error> {
        let Some(graph) = self.graphs.get(&log.scope) else {
            return Ok(None);
        };
        Ok(graph
            .seal(log.tip)
            .map_err(credential)?
            .and_then(|seal| seal.statement.recovery))
    }
    /// Whether the winning seal of `log`'s tip is a recovery that abandons
    /// any entry.
    pub(in crate::replica) fn abandoning(&self, log: TipLog) -> Result<bool, Error> {
        Ok(self
            .recovery(log)?
            .is_some_and(|recovery| recovery.retained.position < recovery.replaced.position))
    }
    /// Whether `log`'s tip continues a recovery that abandoned entries.
    pub(in crate::replica) fn recovered_from_abandoning(&self, log: TipLog) -> Result<bool, Error> {
        let Some(graph) = self.graphs.get(&log.scope) else {
            return Ok(false);
        };
        for seal in &graph.seals {
            if seal.successor().map_err(credential)? == Some(log.tip) {
                return self.abandoning(TipLog {
                    scope: log.scope,
                    tip: seal.statement.tip,
                });
            }
        }
        Ok(false)
    }
    /// The log position a coordinated frame signs, when this replica reads it.
    pub(in crate::replica) fn entry_of(&self, frame: &EncryptedFrame) -> Option<u64> {
        self.row_payload(frame)
            .ok()?
            .membership()
            .and_then(|membership| membership.entry)
    }
    /// The log and position a coordinated frame committed at: its author's
    /// carry record when the graph holds one, else the tip and entry it
    /// signs. None for a frame of no coordinated write this replica reads.
    pub(in crate::replica) fn coordinate(
        &self,
        frame: &EncryptedFrame,
    ) -> Result<Option<(TipLog, u64)>, Error> {
        let operation = frame.operation_ref()?;
        let header = &operation.header;
        self.coordinate_of(
            frame.id()?,
            header.public_key,
            header.extensions.scope.domain,
            header.extensions.tip,
            self.entry_of(frame),
        )
    }
    /// `coordinate` from a frame's parts: `entry` is the position it signs.
    pub(in crate::replica) fn coordinate_of(
        &self,
        id: Hash,
        author: PublicKey,
        domain: Scope,
        tip: TipId,
        entry: Option<u64>,
    ) -> Result<Option<(TipLog, u64)>, Error> {
        let Some(entry) = entry else {
            return Ok(None);
        };
        if let Some(carry) = self.carry_of(id, author, domain, tip)?
            && carry.statement.from.1 == entry
        {
            let log = TipLog {
                scope: domain,
                tip: carry.statement.tip,
            };
            return Ok(Some((log, carry.statement.position)));
        }
        Ok(Some((TipLog { scope: domain, tip }, entry)))
    }
    /// The effective carry record of a frame `author` signed on `tip`.
    pub(in crate::replica) fn carry_of(
        &self,
        id: Hash,
        author: PublicKey,
        domain: Scope,
        tip: TipId,
    ) -> Result<Option<&SignedCarry>, Error> {
        match self.graphs.get(&domain) {
            Some(graph) if !graph.carries.is_empty() => graph
                .carry_of(id.as_bytes(), author, tip)
                .map_err(credential),
            _ => Ok(None),
        }
    }
    /// The record carrying the entry `ids` committed at `position` of
    /// `log`, for a node catching up on it.
    pub(in crate::replica) fn entry_carry(
        &self,
        log: TipLog,
        position: u64,
        ids: &[Hash],
    ) -> Option<SignedCarry> {
        let ids: Vec<[u8; 32]> = ids.iter().map(|id| *id.as_bytes()).collect();
        self.graphs
            .get(&log.scope)?
            .carries
            .iter()
            .find(|carry| {
                carry.statement.tip == log.tip
                    && carry.statement.position == position
                    && carry.names(&ids)
            })
            .cloned()
    }
    /// Whether a recovery seal abandoned the coordinated write `frame` is
    /// part of. Only a frame placed on a recovered tip is opened to tell.
    pub(in crate::replica) fn abandoned_write(
        &self,
        frame: &EncryptedFrame,
    ) -> Result<bool, Error> {
        // Without any recovery seal nothing is abandoned: no frame is decoded.
        if !self.graphs.values().any(|graph| {
            graph
                .seals
                .iter()
                .any(|seal| seal.statement.recovery.is_some())
        }) {
            return Ok(false);
        }
        let operation = frame.operation_ref()?;
        let header = &operation.header;
        let domain = header.extensions.scope.domain;
        // Where the frame sits without decrypting it: its author's carry
        // record, else the tip it signs.
        let tip = match self.carry_of(
            frame.id()?,
            header.public_key,
            domain,
            header.extensions.tip,
        )? {
            Some(carry) => carry.statement.tip,
            None => header.extensions.tip,
        };
        if self.recovery(TipLog { scope: domain, tip })?.is_none() {
            return Ok(false);
        }
        let Some((log, position)) = self.coordinate(frame)? else {
            return Ok(false);
        };
        Ok(self
            .recovery(log)?
            .is_some_and(|recovery| recovery.abandons(position)))
    }
    /// Notes the log position of frames of a coordinated write; a carry
    /// record admitted later moves them.
    pub(in crate::replica) async fn record_entry_frames(
        &self,
        log: TipLog,
        position: u64,
        ids: &[Hash],
    ) -> Result<(), Error> {
        for id in ids {
            self.conn
                .execute(
                    "INSERT INTO secure_entry_frames VALUES(?1,?2,?3) ON CONFLICT(hash) DO UPDATE SET log=?2,position=?3",
                    bindings(vec![
                        SqlValue::Blob(id.as_bytes().to_vec()),
                        SqlValue::Blob(encode(&log)?),
                        SqlValue::Integer(position as i64),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    /// Moves the held frames a newly admitted carry record names to where
    /// they now resolve, inside the caller's transaction. Own frames of an
    /// uncommitted entry keep the position their entry assigns.
    pub(in crate::replica) async fn index_carry(&self, carry: &SignedCarry) -> Result<(), Error> {
        let staged = self.staged().await?;
        for id in &carry.statement.ids {
            let id = Hash::from_bytes(*id);
            if staged.contains(&id) {
                continue;
            }
            let Some(frame) = self.frame(id).await? else {
                continue;
            };
            if let Some((log, position)) = self.coordinate(&frame)? {
                self.record_entry_frames(log, position, &[id]).await?;
            }
        }
        Ok(())
    }
    /// Holds an abandoned write for its author instead of applying it: it
    /// is offered as a held change, never projected, and its author log
    /// continues.
    pub(in crate::replica) async fn hold_abandoned(
        &self,
        frame: &EncryptedFrame,
        time: u64,
    ) -> Result<Outcome, Error> {
        let outcome = self.hold_closed_frame(frame, time).await?;
        if !matches!(outcome, Outcome::Applied) {
            return Ok(outcome);
        }
        let held = collect_rows(
            self.conn
                .query(
                    "SELECT id FROM secure_held_frames WHERE hash=?",
                    [frame.id()?.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(id)]) = held.first().map(Vec::as_slice) else {
            return Err(Error::Corrupt("abandoned frame was not held".into()));
        };
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT hash FROM secure_held_frames WHERE id=?",
                    [id.clone()],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(hash)] = record.as_slice() else {
                return Err(Error::Corrupt("held frame".into()));
            };
            for sql in [
                "UPDATE secure_operations SET status='abandoned' WHERE hash=?",
                "DELETE FROM secure_staged WHERE hash=?",
                "DELETE FROM secure_unoffered WHERE hash=?",
            ] {
                self.conn.execute(sql, [hash.clone()]).await?;
            }
        }
        self.reseed_domain_ledgers(frame.operation_ref()?.header.extensions.scope.domain)
            .await?;
        Ok(Outcome::Applied)
    }
    /// Rolls back, inside the caller's transaction, every frame this replica
    /// applied or staged that a recovery seal of `domain` abandoned, and
    /// rebuilds the scopes they changed. Their entries of the sealed log go.
    pub(in crate::replica) async fn roll_back_abandoned(
        &mut self,
        domain: Scope,
    ) -> Result<(), Error> {
        let Some(graph) = self.graphs.get(&domain) else {
            return Ok(());
        };
        let mut recovered = Vec::new();
        for seal in &graph.seals {
            if seal.statement.recovery.is_none() {
                continue;
            }
            let log = TipLog {
                scope: domain,
                tip: seal.statement.tip,
            };
            if let Some(recovery) = self.recovery(log)? {
                recovered.push((log, recovery));
            }
        }
        let mut changed = BTreeSet::new();
        for (log, recovery) in recovered {
            let (retained, replaced) = (
                recovery.retained.position as i64,
                recovery.replaced.position as i64,
            );
            let blob = encode(&log)?;
            let frames = collect_rows(
                self.conn
                    .query(
                        "SELECT o.header,o.body FROM secure_entry_frames e JOIN secure_operations o ON o.hash=e.hash WHERE e.log=? AND e.position>? AND e.position<=? AND o.status IN ('applied','pending') ORDER BY o.hash",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Integer(retained),
                            SqlValue::Integer(replaced),
                        ]),
                    )
                    .await?,
            )
            .await?;
            let mut held_frames = Vec::new();
            for record in frames {
                let [SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else {
                    return Err(Error::Corrupt("abandoned frame".into()));
                };
                let frame = self.stored_frame(header.clone(), body.clone()).await?;
                changed.insert(frame.scope()?);
                held_frames.push(frame);
            }
            // Own calculations beyond the retained prefix, including carried
            // entries whose log changed, are settled in this same transaction.
            if retained < replaced {
                for record in rows(&self.conn, "SELECT log,hash,frames,position FROM secure_entries WHERE state IN ('staged','assigned','carried')").await? {
                    let [SqlValue::Blob(entry_log), SqlValue::Blob(hash), SqlValue::Blob(ids), SqlValue::Integer(entry_position)] = record.as_slice() else { return Err(Error::Corrupt("own recovery entry".into())) };
                    let entry_position = *entry_position as u64;
                    let ids: Vec<Hash> = decode_bounded(ids, MAX_SECURE_BYTES)?;
                    let mut affected = Vec::new();
                    // Its own log and position, or where its frames resolve.
                    let entry: TipLog = decode(entry_log)?;
                    let own = entry == log && entry_position > recovery.retained.position;
                    for id in ids {
                        if let Some(frame) = self.frame(id).await?
                            && (own
                                || self.coordinate(&frame)?.is_some_and(|(at, position)| {
                                    at == log && position > recovery.retained.position
                                }))
                        {
                            affected.push(frame);
                        }
                    }
                    if !affected.is_empty() {
                        for frame in affected {
                            changed.insert(frame.scope()?);
                            held_frames.push(frame);
                        }
                        self.conn.execute("DELETE FROM secure_entries WHERE log=? AND hash=?", [entry_log.clone(), hash.clone()]).await?;
                    }
                }
            }
            for frame in held_frames {
                // A bundle member held earlier in this pass is settled.
                if self.stored_status(frame.id()?).await?.as_deref() == Some("abandoned") {
                    continue;
                }
                if !matches!(self.hold_abandoned(&frame, now()?).await?, Outcome::Applied) {
                    return Err(denied("an abandoned bundle is incomplete"));
                }
            }
            self.conn
                .execute(
                    "DELETE FROM secure_entries WHERE log=? AND position>? AND position<=? AND state!='carried'",
                    bindings(vec![
                        SqlValue::Blob(blob),
                        SqlValue::Integer(retained),
                        SqlValue::Integer(replaced),
                    ]),
                )
                .await?;
        }
        for scope in changed {
            if self.find_scope_id(scope).await?.is_none() || self.direct.contains(&scope) {
                continue;
            }
            if let Some(id) = self.find_scope_id(scope).await?
                && self.base_off_path(scope, id).await?
            {
                continue;
            }
            let id = self.scope_id(scope).await?;
            if self.base_seal(id).await?.is_none() {
                self.clear_checkpoints(scope, id).await?;
            }
            self.rebuild_rows(scope).await?;
            self.rebuild_tip_existence(scope).await?;
            self.rebuild_constraints(Some(scope), None).await?;
        }
        Ok(())
    }
    /// Whether a dependency of `header` was abandoned by a recovery.
    pub(in crate::replica) async fn abandoned_dependency(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<bool, Error> {
        for id in header.previous.iter().chain(header.backlink.iter()) {
            if self.stored_status(*id).await?.as_deref() == Some("abandoned") {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// A mutation whose write a recovery abandoned reports that, not success.
    pub(in crate::replica) async fn refuse_abandoned(&self, hash: Hash) -> Result<(), Error> {
        if self.stored_status(hash).await?.as_deref() == Some("abandoned") {
            return Err(refusal(
                "abandoned",
                "the owner's recovery abandoned this coordinated write; it is held for re-authoring",
            ));
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Collects every committed entry any reachable device holds, then
    /// returns the recovery of `log` that retains this node's position and
    /// ends the coordinator's exact commit, for the owner to sign into a
    /// `continue` seal of the tip. Nothing is stored.
    pub async fn recovery_proposal(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<Recovery, CoordinatedError> {
        self.hold_sealed_entries(log, coordinator, peers).await?;
        match self.resolve_coordinated(log, coordinator, peers).await {
            Ok(_) | Err(CoordinatedError::Behind) => {}
            Err(error) => return Err(error),
        }
        let head = coordinator.log_head(log).await.map_err(coordination)?;
        if head.terminal.is_some() {
            return Err(CoordinatedError::Sealed);
        }
        let (position, running, genesis, _) = self
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .log_state(log)
            .await?;
        let running = match running.or(genesis) {
            Some(running) => running,
            None if position == 0 => head.genesis,
            None => return Err(CoordinatedError::Behind),
        };
        if position > head.commit {
            return Err(Error::Corrupt(
                "this node's coordinated log is ahead of the coordinator".into(),
            )
            .into());
        }
        // Where the window reaches, this node's prefix must chain to it.
        let first = head.commit + 1 - head.hashes.len() as u64;
        if position + 1 >= first {
            let mut chained = running;
            for at in position + 1..=head.commit {
                let hash = head.committed(at).ok_or(CoordinatedError::Behind)?;
                chained = coordinated_chain(chained, at, hash);
            }
            if chained != head.running_hash {
                return Err(Error::Corrupt(
                    "this node's coordinated log forks the coordinator's window".into(),
                )
                .into());
            }
        }
        Ok(Recovery {
            retained: LogPoint { position, running },
            replaced: LogPoint {
                position: head.commit,
                running: head.running_hash,
            },
        })
    }
}

/// Verifies committed entries before the hash window that a recovery's
/// retained prefix endorses: the hashes from `running` at `position` must
/// chain to its running hash. Returns the hash of every position through it.
pub(in crate::replica) async fn endorsed_hashes(
    log: TipLog,
    retained: LogPoint,
    (position, running): (u64, Commitment),
    peers: &dyn EntryPeers,
) -> Result<Vec<Commitment>, CoordinatedError> {
    if retained.position <= position || retained.position - position > MAX_ENDORSED {
        return Err(CoordinatedError::Behind);
    }
    let mut hashes = Vec::new();
    let mut chained = running;
    let mut at = position;
    while at < retained.position {
        let entries = peers.committed(log, at).await.map_err(evidence_error)?;
        if entries.is_empty() {
            return Err(CoordinatedError::Behind);
        }
        let before = at;
        if entries
            .windows(2)
            .any(|pair| pair[0].position >= pair[1].position)
            || entries.first().is_none_or(|entry| entry.position != at + 1)
        {
            return Err(CoordinatedError::Refused(CoordinationError::Denied));
        }
        for entry in entries
            .into_iter()
            .take_while(|entry| entry.position <= retained.position)
        {
            if entry.position != at + 1
                || entry_hash(log, entry.position, &entry.ids)? != entry.hash
            {
                return Err(CoordinatedError::Refused(CoordinationError::Denied));
            }
            chained = coordinated_chain(chained, entry.position, entry.hash);
            hashes.push(entry.hash);
            at = entry.position;
        }
        if at == before {
            return Err(CoordinatedError::Refused(CoordinationError::Denied));
        }
    }
    if chained != retained.running {
        return Err(CoordinatedError::Refused(CoordinationError::Denied));
    }
    Ok(hashes)
}

impl<A: RowApplication> Inner<A> {
    #[cfg(test)]
    pub(in crate::replica) fn publication_fault(&mut self, step: u8) -> Result<(), Error> {
        if self.publication_fault == Some(step) {
            self.publication_fault = None;
            return Err(Error::InjectedFailure);
        }
        Ok(())
    }
    #[cfg(not(test))]
    pub(in crate::replica) fn publication_fault(&mut self, _step: u8) -> Result<(), Error> {
        Ok(())
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    #[cfg(test)]
    pub(crate) async fn fail_publication_after(&self, step: u8) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .publication_fault = Some(step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn an_out_of_range_evidence_page_is_rejected_without_requesting_it_again() {
        struct Outside(std::sync::atomic::AtomicUsize);
        impl EntryPeers for Outside {
            fn store<'a>(&'a self, _: PublicKey, _: &'a CoordinatedEntry) -> PeerFuture<'a, ()> {
                Box::pin(async { unreachable!() })
            }
            fn committed(&self, _: TipLog, _: u64) -> PeerFuture<'_, Vec<CommittedEntry>> {
                Box::pin(async move {
                    if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) != 0 {
                        return Err(Error::Rejected("repeated page".into()));
                    }
                    Ok(vec![CommittedEntry {
                        position: 3,
                        hash: [0; 32],
                        ids: vec![],
                        before: None,
                        voids: vec![],
                        frames: vec![],
                        carry: None,
                    }])
                })
            }
        }
        let log = TipLog {
            scope: Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
            tip: rocky_core::TipId([4; 32]),
        };
        let peers = Outside(std::sync::atomic::AtomicUsize::new(0));
        let result = endorsed_hashes(
            log,
            rocky_credentials::LogPoint {
                position: 2,
                running: [1; 32],
            },
            (0, [0; 32]),
            &peers,
        )
        .await;
        assert_eq!(
            result,
            Err(CoordinatedError::Refused(CoordinationError::Denied))
        );
        assert_eq!(peers.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
