//! Coordinated writes: ordinary bundles a tip's coordinated log orders.
//! Every in-sync trusted node stores an entry before it commits; nothing of
//! it leaves a node before it commits, and an aborted entry is voided. See
//! `docs/runtime/coordinated-writes.md` and decision 0072.
use super::checkpoint::hash_of;
use super::*;
use rocky_core::{
    Append, DomainLog, LogHead, LogPending, LogResult, Scope, TipLog, coordinated_chain,
};
use std::future::Future;
use std::pin::Pin;

pub(super) mod cut;
mod finality;
pub(super) mod proposal;
mod recovery;
mod statements;
mod terminal;
pub use cut::{CutEvidence, CutRefusal, MAX_PAGE_CUTS};
pub use proposal::{BaseReport, CutProposal};
use terminal::settled;

pub(super) const SCHEMA: &[&str] = &[
    // Per log: the committed position and running hash this replica holds,
    // the log's genesis hash once known, and the last request sequence.
    "CREATE TABLE IF NOT EXISTS secure_coordinated(log BLOB PRIMARY KEY,position INTEGER NOT NULL,running BLOB,genesis BLOB,sequence INTEGER NOT NULL) WITHOUT ROWID",
    // Entries by state: this device's `staged` and `assigned` ones, other
    // proposers' `stored` ones with their frames, and `committed` ones
    // with the running hash after them.
    "CREATE TABLE IF NOT EXISTS secure_entries(log BLOB NOT NULL,hash BLOB NOT NULL,position INTEGER NOT NULL,state TEXT NOT NULL,bundle BLOB,frames BLOB NOT NULL,running BLOB,pending BLOB,append BLOB,PRIMARY KEY(log,hash)) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_entries_position ON secure_entries(log,state,position)",
    // This device's frames of an uncommitted entry: no peer is sent them.
    "CREATE TABLE IF NOT EXISTS secure_staged(hash BLOB PRIMARY KEY,scope BLOB NOT NULL) WITHOUT ROWID",
];
/// Seconds an entry may wait for storage acknowledgments; within the
/// coordinator's 30-second cap.
pub const ENTRY_DEADLINE: u64 = 20;
/// Committed entries a node keeps to serve catch-up: the coordinator's
/// hash window.
const RETAINED_ENTRIES: u64 = 1024;
/// Committed entries one catch-up page carries.
pub const MAX_CATCH_UP_ENTRIES: usize = 16;

/// A trusted node's composed log and its stored-entry signal.
pub(super) struct Trusted {
    pub(super) log: std::sync::RwLock<Option<Arc<dyn DomainLog>>>,
    pub(super) stored: watch::Sender<u64>,
    /// Decides entries one caller at a time, so each applies once and in
    /// order.
    deciding: Mutex<()>,
    /// Logs whose staged write this process proposes next; resolution
    /// drives only staged writes a restart left behind.
    proposing: std::sync::Mutex<BTreeSet<TipLog>>,
}
impl Trusted {
    fn proposing(&self) -> std::sync::MutexGuard<'_, BTreeSet<TipLog>> {
        self.proposing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Marks `log`'s next staged write as this process's to propose.
    pub(super) fn begin_proposal(&self, log: TipLog) {
        self.proposing().insert(log);
    }
    pub(super) fn end_proposal(&self, log: TipLog) {
        self.proposing().remove(&log);
    }
}
impl Default for Trusted {
    fn default() -> Self {
        Self {
            log: Default::default(),
            stored: watch::channel(0).0,
            deciding: Mutex::new(()),
            proposing: Default::default(),
        }
    }
}

/// A position this replica may stage a coordinated write at: it holds
/// every committed entry and is in the in-sync set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub log: TipLog,
    /// The commit index the write is computed on; it is appended after it.
    pub expected: u64,
    pub epoch: u64,
}
/// An assigned entry as a proposer sends it to the in-sync set.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoordinatedEntry {
    pub log: TipLog,
    pub pending: LogPending,
    pub frames: Vec<EncryptedFrame>,
    /// Void records consuming the positions before the frames.
    pub voids: Vec<SignedVoid>,
}
/// A committed entry served to a node catching up. `frames` holds those
/// still retained; `before` is the running hash before the entry when the
/// server knows it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommittedEntry {
    pub position: u64,
    pub hash: Commitment,
    pub ids: Vec<Hash>,
    pub before: Option<Commitment>,
    pub frames: Vec<EncryptedFrame>,
    pub voids: Vec<SignedVoid>,
}
/// A committed coordinated write.
#[derive(Clone, Debug)]
pub struct CoordinatedCommit {
    pub position: u64,
    pub hash: Commitment,
    pub frames: Vec<EncryptedFrame>,
}
/// Why a coordinated write did not commit. Only `Undecided` leaves its
/// local effects in place; every other failure rolled them back, and none
/// is queued as success.
#[derive(Debug)]
pub enum CoordinatedError {
    /// The coordinator is unreachable; nothing was committed.
    Unavailable,
    /// The in-sync set is below the domain's minimum size.
    BelowMinimum,
    /// Another entry is pending or took the position; retry on a new slot.
    Conflict,
    /// The entry missed its deadline without enough storage acknowledgments.
    Aborted,
    /// The coordinator was lost after assigning the entry. The local
    /// effects stay unsent until `resolve_coordinated` decides it.
    Undecided,
    /// Committed entries are missing locally and no peer supplied them.
    Behind,
    /// This node does not follow the log's tip: only trusted nodes that
    /// follow a tip write through its log.
    Unfollowed,
    /// A seal or bookmark move ended the log. A write it aborted after a
    /// `close` is rolled back and held for the application.
    Sealed,
    /// The coordinator refused the call.
    Refused(rocky_core::CoordinationError),
    Storage(Error),
}
impl std::fmt::Display for CoordinatedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for CoordinatedError {}
impl From<Error> for CoordinatedError {
    fn from(error: Error) -> Self {
        Self::Storage(error)
    }
}
fn coordination(error: rocky_core::CoordinationError) -> CoordinatedError {
    use rocky_core::CoordinationError as C;
    match error {
        C::Unavailable | C::CoordinationStateLost => CoordinatedError::Unavailable,
        C::StaleHead | C::StaleEpoch => CoordinatedError::Conflict,
        C::Sealed => CoordinatedError::Sealed,
        other => CoordinatedError::Refused(other),
    }
}
/// A replica's progress on one coordinated log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoordinatedStatus {
    /// The last committed position this replica holds.
    pub position: u64,
    /// Entries this replica stores or proposed that are not decided yet.
    pub undecided: usize,
}

pub type PeerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;
/// The peer-to-peer side of coordinated writes, supplied by the transport.
pub trait EntryPeers: Send + Sync {
    /// Delivers `entry` to the trusted node `node`, which stores it
    /// durably and acknowledges it to the coordinator itself.
    fn store<'a>(&'a self, node: PublicKey, entry: &'a CoordinatedEntry) -> PeerFuture<'a, ()>;
    /// Entry evidence after `position` from any reachable device.
    /// The receiver checks every hash against the coordinator before use.
    fn committed(&self, log: TipLog, after: u64) -> PeerFuture<'_, Vec<CommittedEntry>>;
}

/// The hash an entry is appended under: its log, position and frames.
pub fn entry_hash(log: TipLog, position: u64, ids: &[Hash]) -> Result<Commitment, Error> {
    let mut ids: Vec<[u8; 32]> = ids.iter().map(|id| *id.as_bytes()).collect();
    ids.sort_unstable();
    Ok(rocky_core::commitment(
        "rocky.coordinated-entry.v2",
        &encode(&(log, position, ids))?,
    ))
}

/// One row of `secure_entries`.
struct Entry {
    hash: Commitment,
    position: u64,
    state: String,
    bundle: Option<Commitment>,
    frames: Vec<u8>,
    pending: Option<LogPending>,
    append: Option<Append>,
}
/// What the coordinator's head says of an entry.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Committed,
    Pending,
    Aborted,
    /// Committed positions past the coordinator's window.
    Unknown,
}
fn decision(head: &LogHead, position: u64, hash: Commitment) -> Decision {
    if let Some(pending) = &head.pending
        && pending.position == position
        && pending.hash == hash
    {
        return Decision::Pending;
    }
    match head.committed(position) {
        Some(committed) if committed == hash => Decision::Committed,
        Some(_) => Decision::Aborted,
        None if position > head.commit => Decision::Aborted,
        None => Decision::Unknown,
    }
}
fn commitment_of(bytes: &[u8]) -> Result<Commitment, Error> {
    bytes
        .try_into()
        .map_err(|_| Error::Corrupt("coordinated entry".into()))
}
fn ids_of(frames: &[EncryptedFrame]) -> Result<Vec<Hash>, Error> {
    frames.iter().map(EncryptedFrame::id).collect()
}

impl<A: RowApplication> Inner<A> {
    /// The log of `domain`'s tip this replica follows.
    pub(in crate::replica) fn tip_log(&self, domain: Scope) -> TipLog {
        TipLog {
            scope: domain,
            tip: self.tip,
        }
    }
    /// Position, running hash, genesis and last sequence of `log`.
    async fn log_state(
        &self,
        log: TipLog,
    ) -> Result<(u64, Option<Commitment>, Option<Commitment>, u64), Error> {
        let record = collect_rows(
            self.conn
                .query(
                    "SELECT position,running,genesis,sequence FROM secure_coordinated WHERE log=?",
                    [encode(&log)?],
                )
                .await?,
        )
        .await?;
        let Some(record) = record.first() else {
            return Ok((0, None, None, 0));
        };
        let optional = |value: &SqlValue| match value {
            SqlValue::Blob(bytes) => commitment_of(bytes).map(Some),
            SqlValue::Null => Ok(None),
            _ => Err(Error::Corrupt("coordinated log".into())),
        };
        let [
            SqlValue::Integer(position),
            running,
            genesis,
            SqlValue::Integer(sequence),
        ] = record.as_slice()
        else {
            return Err(Error::Corrupt("coordinated log".into()));
        };
        Ok((
            *position as u64,
            optional(running)?,
            optional(genesis)?,
            *sequence as u64,
        ))
    }
    /// Records the coordinator's genesis anchor, including after commits.
    async fn learn_genesis(&self, log: TipLog, head: &LogHead) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO secure_coordinated VALUES(?1,0,?2,?2,0) ON CONFLICT(log) DO UPDATE SET running=COALESCE(running,?2),genesis=COALESCE(genesis,?2) WHERE position=0",
                [encode(&log)?, head.genesis.to_vec()],
            )
            .await?;
        Ok(())
    }
    /// Allocates the next join or append sequence inside the caller's
    /// transaction; it is durable before the coordinator sees it.
    async fn next_sequence(&self, log: TipLog) -> Result<u64, Error> {
        let (_, _, _, previous) = self.log_state(log).await?;
        let sequence = rocky_core::next_request_sequence(previous, now()?)
            .map_err(Error::Protocol)?
            .get();
        self.conn
            .execute(
                "INSERT INTO secure_coordinated VALUES(?1,0,NULL,NULL,?2) ON CONFLICT(log) DO UPDATE SET sequence=?2",
                bindings(vec![
                    SqlValue::Blob(encode(&log)?),
                    SqlValue::Integer(sequence as i64),
                ]),
            )
            .await?;
        Ok(sequence)
    }
    async fn entries(&self, log: TipLog, states: &[&str]) -> Result<Vec<Entry>, Error> {
        let placeholders = vec!["?"; states.len()].join(",");
        let mut values = vec![SqlValue::Blob(encode(&log)?)];
        values.extend(states.iter().map(|state| SqlValue::Text((*state).into())));
        let mut entries = Vec::new();
        for record in collect_rows(
            self.conn
                .query(
                    format!("SELECT hash,position,state,bundle,frames,pending,append FROM secure_entries WHERE log=? AND state IN ({placeholders}) ORDER BY position,hash"),
                    bindings(values),
                )
                .await?,
        )
        .await?
        {
            let [
                SqlValue::Blob(hash),
                SqlValue::Integer(position),
                SqlValue::Text(state),
                bundle,
                SqlValue::Blob(frames),
                pending,
                append,
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("coordinated entry".into()));
            };
            entries.push(Entry {
                hash: commitment_of(hash)?,
                position: *position as u64,
                state: state.clone(),
                bundle: match bundle {
                    SqlValue::Blob(bytes) => Some(commitment_of(bytes)?),
                    _ => None,
                },
                frames: frames.clone(),
                pending: match pending {
                    SqlValue::Blob(bytes) => Some(decode(bytes)?),
                    _ => None,
                },
                append: match append {
                    SqlValue::Blob(bytes) => Some(decode(bytes)?),
                    _ => None,
                },
            });
        }
        Ok(entries)
    }
    /// Whether `scope` has frames of an uncommitted entry of this device.
    pub(crate) async fn staged_in(&self, scope: ResourceScope) -> Result<bool, Error> {
        let mut found = self
            .conn
            .query(
                "SELECT 1 FROM secure_staged WHERE scope=? LIMIT 1",
                [encode(&scope)?],
            )
            .await?;
        Ok(found.next().await?.is_some())
    }
    /// The frames of uncommitted entries of this device.
    pub(super) async fn staged(&self) -> Result<HashSet<Hash>, Error> {
        rows(&self.conn, "SELECT hash FROM secure_staged")
            .await?
            .iter()
            .map(|record| match record.as_slice() {
                [SqlValue::Blob(hash)] => hash_of(hash),
                _ => Err(Error::Corrupt("staged frame".into())),
            })
            .collect()
    }
    /// Stages the bundle just authored at `slot`, inside the SQL commit:
    /// its frames stay unsent until the entry commits.
    pub(super) async fn stage_entry(
        &self,
        slot: &Slot,
        transaction: Commitment,
        frames: &[EncryptedFrame],
    ) -> Result<(), Error> {
        if slot.log != self.tip_log(slot.log.scope) {
            return Err(denied("the followed tip moved since the coordinated slot"));
        }
        let (position, _, _, _) = self.log_state(slot.log).await?;
        if position != slot.expected {
            return Err(denied("the coordinated slot is stale"));
        }
        if !self
            .entries(slot.log, &["staged", "assigned", "carried"])
            .await?
            .is_empty()
        {
            return Err(denied("a coordinated write of this device is undecided"));
        }
        let ids = ids_of(frames)?;
        let entry = slot
            .expected
            .checked_add(1)
            .ok_or_else(|| denied("coordinated log exhausted"))?;
        let hash = entry_hash(slot.log, entry, &ids)?;
        let append = Append {
            expected: slot.expected,
            epoch: slot.epoch,
            hash,
            deadline: now()? + ENTRY_DEADLINE,
            sequence: self.next_sequence(slot.log).await?,
        };
        self.conn
            .execute(
                "INSERT INTO secure_entries VALUES(?,?,?,'staged',?,?,NULL,NULL,?)",
                bindings(vec![
                    SqlValue::Blob(encode(&slot.log)?),
                    SqlValue::Blob(hash.to_vec()),
                    SqlValue::Integer(entry as i64),
                    SqlValue::Blob(transaction.to_vec()),
                    SqlValue::Blob(encode(&ids)?),
                    SqlValue::Blob(encode(&append)?),
                ]),
            )
            .await?;
        self.record_entry_frames(slot.log, entry, &ids).await?;
        for frame in frames {
            if frame.scope()?.domain != slot.log.scope {
                return Err(denied("a coordinated write stays in its log's domain"));
            }
            self.conn
                .execute(
                    "INSERT INTO secure_staged VALUES(?,?)",
                    [frame.id()?.as_bytes().to_vec(), encode(&frame.scope()?)?],
                )
                .await?;
        }
        Ok(())
    }
    /// Refuses to let staged frames leave through an ordinary path.
    pub(super) async fn refuse_staged(&self, hashes: &[Hash]) -> Result<(), Error> {
        for hash in hashes {
            let mut found = self
                .conn
                .query(
                    "SELECT 1 FROM secure_staged WHERE hash=?",
                    [hash.as_bytes().to_vec()],
                )
                .await?;
            if found.next().await?.is_some() {
                return Err(denied("an uncommitted coordinated entry is never sent"));
            }
        }
        Ok(())
    }
    /// Records `entry` committed at its position inside the caller's
    /// transaction and advances the log; the position must follow the
    /// replica's. Returns the running hash after it.
    async fn commit_entry(
        &self,
        log: TipLog,
        position: u64,
        hash: Commitment,
        ids: &[Hash],
        before: Option<Commitment>,
    ) -> Result<Commitment, Error> {
        let (local, running, genesis, _) = self.log_state(log).await?;
        if local + 1 != position {
            return Err(denied("coordinated entries commit in order"));
        }
        let running = running
            .or(before)
            .ok_or_else(|| denied("the coordinated log's genesis is unknown"))?;
        let after = coordinated_chain(running, position, hash);
        let blob = encode(&log)?;
        self.conn
            .execute(
                "INSERT INTO secure_entries VALUES(?1,?2,?3,'committed',NULL,?4,?5,NULL,NULL) ON CONFLICT(log,hash) DO UPDATE SET state='committed',frames=?4,running=?5,pending=NULL,append=NULL",
                bindings(vec![
                    SqlValue::Blob(blob.clone()),
                    SqlValue::Blob(hash.to_vec()),
                    SqlValue::Integer(position as i64),
                    SqlValue::Blob(encode(&ids)?),
                    SqlValue::Blob(after.to_vec()),
                ]),
            )
            .await?;
        let genesis = genesis.or_else(|| (position == 1).then_some(running));
        self.conn
            .execute(
                "INSERT INTO secure_coordinated VALUES(?1,?2,?3,?4,0) ON CONFLICT(log) DO UPDATE SET position=?2,running=?3,genesis=?4",
                bindings(vec![
                    SqlValue::Blob(blob.clone()),
                    SqlValue::Integer(position as i64),
                    SqlValue::Blob(after.to_vec()),
                    genesis.map_or(SqlValue::Null, |genesis| SqlValue::Blob(genesis.to_vec())),
                ]),
            )
            .await?;
        for id in ids {
            let id = id.as_bytes().to_vec();
            for sql in [
                "DELETE FROM secure_staged WHERE hash=?",
                // Every in-sync node holds them: they count as offered.
                "DELETE FROM secure_unoffered WHERE hash=?",
            ] {
                self.conn.execute(sql, [id.clone()]).await?;
            }
        }
        self.conn
            .execute(
                "DELETE FROM secure_entries WHERE log=? AND state='committed' AND position<=?",
                bindings(vec![
                    SqlValue::Blob(blob),
                    SqlValue::Integer(position.saturating_sub(RETAINED_ENTRIES) as i64),
                ]),
            )
            .await?;
        Ok(after)
    }
    async fn record(
        &self,
        sql: &str,
        log: TipLog,
        hash: Commitment,
        value: Vec<u8>,
    ) -> Result<(), Error> {
        self.conn
            .execute(sql, [value, encode(&log)?, hash.to_vec()])
            .await?;
        Ok(())
    }
    /// This device's frames of an entry, with their chains.
    async fn own_frames(&self, ids: &[Hash]) -> Result<Vec<EncryptedFrame>, Error> {
        let mut frames = Vec::with_capacity(ids.len());
        for id in ids {
            frames.push(
                self.frame(*id)
                    .await?
                    .ok_or_else(|| Error::Corrupt("staged frame is not retained".into()))?,
            );
        }
        Ok(frames)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The log of `domain`'s tip this replica follows.
    pub async fn followed_log(&self, domain: Scope) -> TipLog {
        self.inner.lock().await.tip_log(domain)
    }
    /// Decides this replica's undecided entries of `log`, applies those
    /// committed, catches up from `peers` and joins the in-sync set. The
    /// slot is the position the next coordinated write is staged at.
    pub async fn coordinated_slot(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<Slot, CoordinatedError> {
        if log != self.followed_log(log.scope).await {
            return Err(CoordinatedError::Unfollowed);
        }
        let _deciding = self.domain_log.deciding.lock().await;
        if self.inner.lock().await.log_state(log).await?.0 == 0 {
            self.hold_sealed_entries(log, coordinator, peers).await?;
        }
        let status = self.resolve(log, coordinator, peers).await?;
        if status.undecided > 0 {
            return Err(CoordinatedError::Undecided);
        }
        let mut head = coordinator.log_head(log).await.map_err(coordination)?;
        if head.terminal.is_some() {
            return Err(CoordinatedError::Sealed);
        }
        if head.pending.is_some() {
            return Err(CoordinatedError::Conflict);
        }
        let (position, running, _, _) = self.inner.lock().await.log_state(log).await?;
        // The coordinator drops only ended logs: one that forgot entries
        // this node holds ended, though its seal has not arrived yet.
        if head.epoch == 0 && position > head.commit {
            return Err(CoordinatedError::Sealed);
        }
        if position != head.commit || running != Some(head.running_hash) {
            return Err(CoordinatedError::Behind);
        }
        self.require_applied(log).await?;
        let me = coordinator.node();
        if !head.member(&me) {
            self.configure_line(log, &head, coordinator).await?;
            let sequence = {
                let mut inner = self.inner.lock().await;
                let sequence = transaction!(inner, |inner| inner.next_sequence(log).await)?;
                inner.flush()?;
                sequence
            };
            let epoch = coordinator
                .join(log, head.commit, head.running_hash, sequence)
                .await
                .map_err(coordination)?;
            head = coordinator.log_head(log).await.map_err(coordination)?;
            if head.epoch != epoch || !head.member(&me) || head.commit != position {
                return Err(CoordinatedError::Conflict);
            }
        }
        if head.members.len() < head.minimum {
            return Err(CoordinatedError::BelowMinimum);
        }
        Ok(Slot {
            log,
            expected: head.commit,
            epoch: head.epoch,
        })
    }
    /// Refuses a slot while a committed entry's frames wait for dependencies.
    pub(super) async fn require_applied(&self, log: TipLog) -> Result<(), CoordinatedError> {
        let inner = self.inner.lock().await;
        for entry in inner.entries(log, &["committed"]).await? {
            let ids: Vec<Hash> = decode_bounded(&entry.frames, MAX_SECURE_BYTES)?;
            for id in ids {
                if inner.frame(id).await?.is_some()
                    && inner.status(id).await? != ReceiveStatus::Applied
                {
                    return Err(CoordinatedError::Behind);
                }
            }
        }
        Ok(())
    }
    /// Drives this device's staged write of `log` to its decision: appends
    /// it, sends it to the in-sync set, acknowledges it and waits for the
    /// commit. An abort voids it and rolls its effects back.
    pub async fn propose(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<CoordinatedCommit, CoordinatedError> {
        let _deciding = self.domain_log.deciding.lock().await;
        let proposed = self.propose_staged(log, coordinator, peers).await;
        self.domain_log.end_proposal(log);
        proposed
    }
    async fn propose_staged(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<CoordinatedCommit, CoordinatedError> {
        let own = {
            let inner = self.inner.lock().await;
            inner.entries(log, &["staged", "assigned"]).await?
        };
        let Some(mut entry) = own.into_iter().next() else {
            return Err(denied("no coordinated write is staged").into());
        };
        if entry.state == "staged" {
            let append = entry
                .append
                .ok_or_else(|| Error::Corrupt("staged entry without its append".into()))?;
            match coordinator.append(log, append).await {
                Ok(pending) => {
                    self.assign(log, &entry, &pending).await?;
                    entry.pending = Some(pending);
                }
                Err(rocky_core::CoordinationError::Sealed) => {
                    return self.ended_own(log, &entry, coordinator, peers).await;
                }
                Err(error) => {
                    // Never sent and never acknowledged: it cannot commit.
                    self.abort_own(log, &entry).await?;
                    return Err(match coordination(error) {
                        CoordinatedError::Refused(rocky_core::CoordinationError::Backpressure)
                            if coordinator
                                .log_head(log)
                                .await
                                .is_ok_and(|head| head.members.len() < head.minimum) =>
                        {
                            CoordinatedError::BelowMinimum
                        }
                        CoordinatedError::Refused(
                            rocky_core::CoordinationError::Configuration
                            | rocky_core::CoordinationError::Denied,
                        ) => CoordinatedError::Conflict,
                        other => other,
                    });
                }
            }
        }
        let pending = entry
            .pending
            .clone()
            .ok_or_else(|| Error::Corrupt("assigned entry without its assignment".into()))?;
        let ids: Vec<Hash> = decode_bounded(&entry.frames, MAX_SECURE_BYTES)?;
        let (frames, decided) = self
            .send_own(log, &pending, &ids, coordinator, peers)
            .await?;
        let decided = match decided {
            Some(decided) => decided,
            None => {
                self.await_decision(log, coordinator, peers, &pending, &ids)
                    .await?
            }
        };
        match decided {
            Decision::Committed => {
                self.commit_own(log, &entry, &ids).await?;
                Ok(CoordinatedCommit {
                    position: pending.position,
                    hash: pending.hash,
                    frames,
                })
            }
            Decision::Aborted => {
                if self.ended(log, &entry, coordinator).await? {
                    return self.ended_own(log, &entry, coordinator, peers).await;
                }
                self.abort_own(log, &entry).await?;
                Err(CoordinatedError::Aborted)
            }
            Decision::Pending | Decision::Unknown => Err(CoordinatedError::Undecided),
        }
    }
    /// Sends this device's assigned entry to the rest of the in-sync set,
    /// then acknowledges it; returns its frames and whether it committed.
    async fn send_own(
        &self,
        log: TipLog,
        pending: &LogPending,
        ids: &[Hash],
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<(Vec<EncryptedFrame>, Option<Decision>), CoordinatedError> {
        let (frames, voids) = {
            let inner = self.inner.lock().await;
            let frames = inner.own_frames(ids).await?;
            let voids = inner.voids_before(&frames).await?;
            (frames, voids)
        };
        let sent = CoordinatedEntry {
            log,
            pending: pending.clone(),
            frames,
            voids,
        };
        let me = coordinator.node();
        let Ok(head) = coordinator.log_head(log).await else {
            return Ok((sent.frames, None));
        };
        let decided = decision(&head, pending.position, pending.hash);
        if decided != Decision::Pending {
            return Ok((sent.frames, Some(decided)));
        }
        for member in head.members.iter().filter(|member| member.key != me) {
            // A member that does not store it misses the deadline and
            // leaves the set.
            if let Ok(key) = PublicKey::from_bytes(&member.key) {
                let _ = peers.store(key, &sent).await;
            }
        }
        let decided = match coordinator.acknowledge(log, pending.clone()).await {
            Ok(LogResult::Committed { .. }) => Some(Decision::Committed),
            _ => None,
        };
        Ok((sent.frames, decided))
    }
    /// Retries storage and polls until `pending` is decided, a little past
    /// its deadline. A lost coordinator leaves it undecided.
    async fn await_decision(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
        pending: &LogPending,
        ids: &[Hash],
    ) -> Result<Decision, CoordinatedError> {
        let mut delay = std::time::Duration::from_millis(10);
        loop {
            let Ok(head) = coordinator.log_head(log).await else {
                return Ok(Decision::Pending);
            };
            let decided = decision(&head, pending.position, pending.hash);
            if decided != Decision::Pending || now()? > pending.deadline + ENTRY_DEADLINE {
                return Ok(decided);
            }
            tokio::time::sleep(delay).await;
            if let (_, Some(decided)) = self.send_own(log, pending, ids, coordinator, peers).await?
            {
                return Ok(decided);
            }
            delay = (delay * 2).min(std::time::Duration::from_millis(500));
        }
    }
    async fn assign(&self, log: TipLog, entry: &Entry, pending: &LogPending) -> Result<(), Error> {
        if pending.hash != entry.hash || pending.position != entry.position {
            return Err(Error::Corrupt(
                "the coordinator assigned another entry".into(),
            ));
        }
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| {
            inner
                .record(
                    "UPDATE secure_entries SET state='assigned',pending=?,append=NULL WHERE log=? AND hash=?",
                    log,
                    entry.hash,
                    encode(pending)?,
                )
                .await
        })?;
        inner.flush()
    }
    async fn commit_own(&self, log: TipLog, entry: &Entry, ids: &[Hash]) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| {
            inner
                .commit_entry(log, entry.position, entry.hash, ids, None)
                .await
                .map(|_| ())
        })?;
        inner.flush()
    }
    /// Voids an own entry that never committed and rolls its effects back.
    async fn abort_own(&self, log: TipLog, entry: &Entry) -> Result<(), Error> {
        self.void_own(log, entry, false).await
    }
    /// Voids an own entry and rolls its effects back; with `hold` its
    /// bundle is held for the application to re-author.
    async fn void_own(&self, log: TipLog, entry: &Entry, hold: bool) -> Result<(), Error> {
        let ids: Vec<Hash> = decode_bounded(&entry.frames, MAX_SECURE_BYTES)?;
        let bundle = entry
            .bundle
            .ok_or_else(|| Error::Corrupt("own entry without its bundle".into()))?;
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| {
            for id in &ids {
                for sql in [
                    "DELETE FROM secure_staged WHERE hash=?",
                    "DELETE FROM secure_entry_frames WHERE hash=?",
                ] {
                    inner.conn.execute(sql, [id.as_bytes().to_vec()]).await?;
                }
            }
            inner.void_bundle(bundle, hold).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_entries WHERE log=? AND hash=?",
                    [encode(&log)?, entry.hash.to_vec()],
                )
                .await?;
            Ok(())
        })?;
        inner.pull_again().await?;
        inner.flush()?;
        inner.publish_revision().await
    }
    /// Stores another proposer's assigned entry durably and unapplied, then
    /// acknowledges it. Nothing of it is applied or sent before it commits.
    pub async fn store_entry(
        &self,
        entry: CoordinatedEntry,
        coordinator: &dyn DomainLog,
    ) -> Result<LogResult, CoordinatedError> {
        // An entry reaches only the nodes of its tip's log.
        if entry.log != self.followed_log(entry.log.scope).await {
            return Err(CoordinatedError::Unfollowed);
        }
        let pending = &entry.pending;
        let ids = ids_of(&entry.frames)?;
        if entry.frames.is_empty()
            || entry.frames.len() > MAX_BUNDLE_FRAMES
            || entry_hash(entry.log, pending.position, &ids)? != pending.hash
        {
            return Err(denied("the entry's frames do not match its hash").into());
        }
        for frame in &entry.frames {
            frame.operation()?;
            frame.chain()?;
            if frame.scope()?.domain != entry.log.scope {
                return Err(denied("a coordinated entry stays in its log's domain").into());
            }
        }
        {
            let mut inner = self.inner.lock().await;
            transaction!(inner, |inner| {
                inner
                    .conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_entries VALUES(?,?,?,'stored',NULL,?,NULL,?,NULL)",
                        bindings(vec![
                            SqlValue::Blob(encode(&entry.log)?),
                            SqlValue::Blob(pending.hash.to_vec()),
                            SqlValue::Integer(pending.position as i64),
                            SqlValue::Blob(encode(&(&entry.frames, &entry.voids))?),
                            SqlValue::Blob(encode(pending)?),
                        ]),
                    )
                    .await?;
                Ok(())
            })?;
            // The acknowledgment promises durable storage.
            inner.flush()?;
        }
        self.domain_log.stored.send_modify(|count| *count += 1);
        let (log, hash) = (entry.log, entry.pending.hash);
        match coordinator.acknowledge(log, entry.pending).await {
            Ok(result) => Ok(result),
            // Not the pending entry: nothing will commit it.
            Err(error) if coordination(error.clone()) != CoordinatedError::Unavailable => {
                let mut inner = self.inner.lock().await;
                transaction!(inner, |inner| {
                    inner
                        .conn
                        .execute(
                            "DELETE FROM secure_entries WHERE log=? AND hash=? AND state='stored'",
                            [encode(&log)?, hash.to_vec()],
                        )
                        .await?;
                    Ok(())
                })?;
                Err(coordination(error))
            }
            Err(error) => Err(coordination(error)),
        }
    }
    /// Decides every undecided entry of `log` against the coordinator's
    /// head: committed ones apply in log order, aborted ones are voided or
    /// discarded, pending ones are acknowledged again. Missing committed
    /// entries are fetched from `peers` first.
    pub async fn resolve_coordinated(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<CoordinatedStatus, CoordinatedError> {
        let _deciding = self.domain_log.deciding.lock().await;
        if self.inner.lock().await.log_state(log).await?.0 == 0 {
            self.hold_sealed_entries(log, coordinator, peers).await?;
        }
        self.resolve(log, coordinator, peers).await
    }
    pub(super) async fn resolve(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<CoordinatedStatus, CoordinatedError> {
        let head = coordinator.log_head(log).await.map_err(coordination)?;
        self.inner.lock().await.learn_genesis(log, &head).await?;
        // A submission a lost reply left undecided is sent again first.
        self.retry_cut(log, coordinator).await?;
        let head = coordinator.log_head(log).await.map_err(coordination)?;
        // Trusted nodes take cuts from the coordinator head only.
        if let Some(cut) = &head.cut {
            self.receive_cuts(&[CutEvidence::from_log(cut)?]).await?;
        }
        let me = coordinator.node();
        // A staged entry whose append a restart lost: its first outcome
        // decides. One this process proposes next is left to `propose`.
        let proposing = self.domain_log.proposing().contains(&log);
        let staged = match proposing {
            true => Vec::new(),
            false => self.inner.lock().await.entries(log, &["staged"]).await?,
        };
        for entry in staged {
            let append = entry
                .append
                .ok_or_else(|| Error::Corrupt("staged entry without its append".into()))?;
            match coordinator.append(log, append).await {
                Ok(pending) => self.assign(log, &entry, &pending).await?,
                Err(error) if coordination(error.clone()) == CoordinatedError::Unavailable => {}
                Err(rocky_core::CoordinationError::Sealed) => {
                    settled(self.ended_own(log, &entry, coordinator, peers).await)?;
                }
                Err(_) => self.abort_own(log, &entry).await?,
            }
        }
        // A write a seal carried here retries once this node follows the tip.
        let carried = self.inner.lock().await.entries(log, &["carried"]).await?;
        if !proposing && !carried.is_empty() {
            settled(self.retry_carried(log, coordinator, peers).await)?;
        }
        let head = coordinator.log_head(log).await.map_err(coordination)?;
        let mut caught_up = false;
        let undecided = self
            .inner
            .lock()
            .await
            .entries(log, &["assigned", "stored"])
            .await?;
        for entry in undecided {
            match decision(&head, entry.position, entry.hash) {
                Decision::Pending => {
                    let pending = entry
                        .pending
                        .clone()
                        .ok_or_else(|| Error::Corrupt("undecided entry".into()))?;
                    if entry.state == "assigned" {
                        let ids: Vec<Hash> = decode_bounded(&entry.frames, MAX_SECURE_BYTES)?;
                        let (_, decided) = self
                            .send_own(log, &pending, &ids, coordinator, peers)
                            .await?;
                        if decided == Some(Decision::Committed) {
                            self.apply_committed(log, &entry).await?;
                        }
                    } else if head.member(&me) {
                        // Acknowledgments are leader memory: repeat them.
                        let _ = coordinator.acknowledge(log, pending).await;
                    }
                }
                Decision::Aborted if entry.state == "assigned" => {
                    if head.terminal.is_some() && head.commit == entry.position {
                        settled(self.ended_own(log, &entry, coordinator, peers).await)?;
                    } else {
                        self.abort_own(log, &entry).await?;
                    }
                }
                Decision::Aborted => {
                    let mut inner = self.inner.lock().await;
                    transaction!(inner, |inner| {
                        inner
                            .conn
                            .execute(
                                "DELETE FROM secure_entries WHERE log=? AND hash=?",
                                [encode(&log)?, entry.hash.to_vec()],
                            )
                            .await?;
                        Ok(())
                    })?;
                }
                Decision::Committed => {
                    let (position, _, _, _) = self.inner.lock().await.log_state(log).await?;
                    if position + 1 < entry.position && !caught_up {
                        self.catch_up(log, &head, peers, entry.position - 1).await?;
                        caught_up = true;
                    }
                    self.apply_committed(log, &entry).await?;
                }
                Decision::Unknown => {}
            }
        }
        // A terminal entry is metadata no device holds; the sealed state
        // ends before it.
        let through = head.commit - u64::from(head.terminal.is_some());
        let (position, _, _, _) = self.inner.lock().await.log_state(log).await?;
        if position < through {
            self.catch_up(log, &head, peers, through).await?;
        }
        self.coordinated_status(log).await.map_err(Into::into)
    }
    /// Applies one committed entry this replica stores or proposed.
    async fn apply_committed(&self, log: TipLog, entry: &Entry) -> Result<(), CoordinatedError> {
        {
            let inner = self.inner.lock().await;
            let (position, _, _, _) = inner.log_state(log).await?;
            if entry.position <= position {
                // Catching up may have committed it already.
                return match inner
                    .entries(log, &["committed"])
                    .await?
                    .iter()
                    .any(|committed| committed.hash == entry.hash)
                {
                    true => Ok(()),
                    false => {
                        Err(Error::Corrupt("a committed entry is decided twice".into()).into())
                    }
                };
            }
        }
        let ids = if entry.state == "stored" {
            let (frames, voids): (Vec<EncryptedFrame>, Vec<SignedVoid>) =
                decode_bounded(&entry.frames, MAX_SECURE_BYTES)?;
            let ids = ids_of(&frames)?;
            self.receive_entry(frames, &voids).await?;
            ids
        } else {
            decode_bounded(&entry.frames, MAX_SECURE_BYTES)?
        };
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| {
            inner
                .commit_entry(log, entry.position, entry.hash, &ids, None)
                .await
                .map(|_| ())
        })?;
        inner.flush()?;
        Ok(())
    }
    /// Admits a committed entry's voids and frames as any received ones.
    async fn receive_entry(
        &self,
        frames: Vec<EncryptedFrame>,
        voids: &[SignedVoid],
    ) -> Result<(), CoordinatedError> {
        for status in self.receive_voids(voids).await {
            status?;
        }
        for status in self.receive_all(frames).await {
            if status? == ReceiveStatus::Rejected {
                return Err(denied("a committed entry's frame was rejected").into());
            }
        }
        Ok(())
    }
    /// Fetches and applies the committed entries after this replica's
    /// position through `through`, verified against the head.
    async fn catch_up(
        &self,
        log: TipLog,
        head: &LogHead,
        peers: &dyn EntryPeers,
        through: u64,
    ) -> Result<(), CoordinatedError> {
        // Hashes before the window that a cut's retained chain endorsed.
        let mut endorsed: BTreeMap<u64, Commitment> = BTreeMap::new();
        loop {
            let (position, running, _, _) = self.inner.lock().await.log_state(log).await?;
            if position >= through {
                return Ok(());
            }
            let next = {
                let mut inner = self.inner.lock().await;
                // A held cut whose retained prefix ends here moves past it.
                if let Some(cut) = inner
                    .cut_after(log, position)
                    .filter(|cut| cut.statement.entry() <= head.commit)
                {
                    let jumped = transaction!(inner, |inner| inner.jump_cut(&cut).await)?;
                    inner.flush()?;
                    if !jumped {
                        return Err(CoordinatedError::Behind);
                    }
                    continue;
                }
                inner.next_cut(log, position)
            };
            let limit = next
                .as_ref()
                .map_or(through, |cut| cut.statement.retained.position.min(through));
            if head.committed(position + 1).is_none() && !endorsed.contains_key(&(position + 1)) {
                let (Some(cut), Some(running)) = (&next, running) else {
                    return Err(CoordinatedError::Behind);
                };
                let hashes = cut::endorsed_hashes(log, cut, (position, running), peers).await?;
                endorsed.extend(
                    hashes
                        .into_iter()
                        .enumerate()
                        .map(|(offset, hash)| (position + 1 + offset as u64, hash)),
                );
            }
            let entries: Vec<CommittedEntry> = peers
                .committed(log, position)
                .await
                .map_err(|_| CoordinatedError::Behind)?
                .into_iter()
                .take_while(|entry| entry.position <= limit)
                .collect();
            let Some(first) = entries.first() else {
                return Err(CoordinatedError::Behind);
            };
            // Verify the whole page before applying any of it.
            let mut chained = running.or(first.before).ok_or(CoordinatedError::Behind)?;
            for (offset, entry) in entries.iter().enumerate() {
                let expected = head
                    .committed(entry.position)
                    .or_else(|| endorsed.get(&entry.position).copied());
                if entry.position != position + 1 + offset as u64
                    || entry.position > head.commit
                    || entry_hash(log, entry.position, &entry.ids)? != entry.hash
                    || expected != Some(entry.hash)
                    || entry
                        .frames
                        .iter()
                        .any(|frame| frame.id().map_or(true, |id| !entry.ids.contains(&id)))
                {
                    return Err(CoordinatedError::Behind);
                }
                chained = coordinated_chain(chained, entry.position, entry.hash);
                if entry.position == head.commit && chained != head.running_hash {
                    return Err(CoordinatedError::Behind);
                }
            }
            for entry in entries {
                self.receive_entry(entry.frames, &entry.voids).await?;
                let mut inner = self.inner.lock().await;
                for id in &entry.ids {
                    if inner.frame(*id).await?.is_none() && inner.stub(*id).await?.is_none() {
                        return Err(CoordinatedError::Behind);
                    }
                }
                transaction!(inner, |inner| {
                    inner
                        .commit_entry(log, entry.position, entry.hash, &entry.ids, entry.before)
                        .await
                        .map(|_| ())
                })?;
                inner.flush()?;
            }
        }
    }
    /// Committed entries of `log` after `after` for a peer catching up,
    /// with the frames it may hold; they are marked offered first.
    pub async fn committed_entries(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        log: TipLog,
        after: u64,
    ) -> Result<Vec<CommittedEntry>, Error> {
        let mut inner = self.inner.lock().await;
        let (_, _, genesis, _) = inner.log_state(log).await?;
        let mut entries = Vec::new();
        let mut before = None;
        for record in collect_rows(
            inner
                .conn
                .query(
                    "SELECT position,hash,frames,running FROM secure_entries WHERE log=? AND state='committed' AND position>=? ORDER BY position LIMIT ?",
                    bindings(vec![
                        SqlValue::Blob(encode(&log)?),
                        SqlValue::Integer(after as i64),
                        SqlValue::Integer(MAX_CATCH_UP_ENTRIES as i64 + 1),
                    ]),
                )
                .await?,
        )
        .await?
        {
            let [
                SqlValue::Integer(position),
                SqlValue::Blob(hash),
                SqlValue::Blob(ids),
                SqlValue::Blob(running),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("committed entry".into()));
            };
            let position = *position as u64;
            if position == after {
                before = Some(commitment_of(running)?);
                continue;
            }
            if entries.len() == MAX_CATCH_UP_ENTRIES {
                break;
            }
            let ids: Vec<Hash> = decode_bounded(ids, MAX_SECURE_BYTES)?;
            let mut frames = Vec::new();
            for id in &ids {
                if let Some(frame) = inner.frame(*id).await? {
                    inner.authorize_peer(peer, credentials, frame.scope()?)?;
                    frames.push(frame);
                }
            }
            entries.push(CommittedEntry {
                position,
                hash: commitment_of(hash)?,
                ids,
                before: if position == 1 { genesis } else { before },
                voids: inner.voids_before(&frames).await?,
                frames,
            });
            before = Some(commitment_of(running)?);
        }
        // Members rebuild evidence from retained bundles of the log's tip.
        if entries.is_empty() {
            entries = inner
                .recovered_entries(peer, credentials, log, after)
                .await?;
        }
        let hashes = entries
            .iter()
            .flat_map(|entry| entry.frames.iter().map(EncryptedFrame::id))
            .collect::<Result<Vec<_>, _>>()?;
        inner.offer(&hashes).await?;
        Ok(entries)
    }
    /// This replica's progress on `log`.
    pub async fn coordinated_status(&self, log: TipLog) -> Result<CoordinatedStatus, Error> {
        let inner = self.inner.lock().await;
        let (position, _, _, _) = inner.log_state(log).await?;
        Ok(CoordinatedStatus {
            position,
            undecided: inner
                .entries(log, &["staged", "assigned", "stored", "carried"])
                .await?
                .len(),
        })
    }
}
impl PartialEq for CoordinatedError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Storage(_), _) | (_, Self::Storage(_)) => false,
            (Self::Refused(a), Self::Refused(b)) => a == b,
            (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
        }
    }
}
