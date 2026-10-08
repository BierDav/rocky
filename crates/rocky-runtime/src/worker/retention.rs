//! Checkpoints, the horizon and pruning of one domain. The checkpoint is a
//! second copy of the application's tables that lags the projection; it
//! advances by replaying retained operations. See
//! `docs/runtime/checkpoints.md`.
use super::*;
use crate::checkpoint::{
    CheckpointHold, CheckpointInfo, CheckpointRecord, Deduplication, PeerRetention, Request,
    Response, RetentionPolicy, RetentionStatus, RowAudit, Step, StrongAnchor,
};
use crate::digest::{SetDigest, Tracker, operation_hash};
use rocky_core::{Commitment, RequestId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Mutex;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS rocky_checkpoint(id INTEGER PRIMARY KEY CHECK(id=1),live BLOB NOT NULL,shadow BLOB NOT NULL,state BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS rocky_rejected_local(node BLOB NOT NULL,sequence INTEGER NOT NULL,reason TEXT NOT NULL,PRIMARY KEY(node,sequence))",
    "CREATE INDEX IF NOT EXISTS rocky_mutation_handles_sequence ON rocky_mutation_handles(node,sequence)",
    "CREATE TABLE IF NOT EXISTS rocky_request_holds(node BLOB NOT NULL,sequence INTEGER NOT NULL,stored INTEGER NOT NULL,released INTEGER NOT NULL,PRIMARY KEY(node,sequence))",
];
pub(crate) fn initial_checkpoint() -> Result<Vec<crate::sqlite::Value>, Error> {
    Ok(vec![
        crate::sqlite::Value::Blob(SetDigest::default().encode()),
        crate::sqlite::Value::Blob(SetDigest::default().encode()),
        crate::sqlite::Value::Blob(encode_stored(&Stored::default())?),
    ])
}
/// Milliseconds since the Unix epoch on this replica's clock.
pub(super) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
/// Operations one requested step replays into the checkpoint copy. A step
/// beside a group replays twice the group's writes, within these bounds, so
/// the copy catches up under any load without one long step.
pub(super) const ADVANCE_OPERATIONS: usize = 128;
pub(super) const ADVANCE_BOUNDS: (usize, usize) = (32, 256);
/// Records one step deletes, and sequences it moves the horizon per author.
pub(super) const PRUNE_BATCH: usize = 512;
/// Strong results one step deletes.
pub(super) const STRONG_BATCH: usize = 16;
/// Rows and bytes of one exported checkpoint page.
pub(super) const EXPORT_ROWS: usize = 512;
pub(super) const EXPORT_BYTES: usize = 256 * 1024;
/// How long an export keeps automatic checkpoints from moving its rows.
pub(super) const EXPORT_LEASE: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) type Marks = Vec<(NodeId, u64)>;
pub(super) fn at(marks: &[(NodeId, u64)], node: NodeId) -> u64 {
    marks
        .iter()
        .find(|(author, _)| *author == node)
        .map_or(0, |(_, sequence)| *sequence)
}
pub(super) fn set(marks: &mut Marks, node: NodeId, sequence: u64) {
    match marks.iter_mut().find(|(author, _)| *author == node) {
        Some(mark) => mark.1 = sequence,
        None => {
            marks.push((node, sequence));
            marks.sort();
        }
    }
}
pub(super) fn frontier(marks: &[(NodeId, u64)]) -> Result<rocky_core::AppliedFrontier, Error> {
    Ok(rocky_core::AppliedFrontier(
        marks
            .iter()
            .filter(|(_, sequence)| *sequence > 0)
            .map(|(node, sequence)| Ok((*node, AuthorSequence::new(*sequence)?)))
            .collect::<Result<_, rocky_core::ProtocolError>>()?,
    ))
}
pub(super) fn marks(frontier: &rocky_core::AppliedFrontier) -> Marks {
    let mut marks: Marks = frontier
        .0
        .iter()
        .map(|(node, sequence)| (*node, sequence.get()))
        .collect();
    marks.sort();
    marks
}
pub(super) fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Candidate {
    pub(super) frontier: Marks,
    /// The projection's digest when it stood at `frontier`.
    pub(super) live: (Commitment, u64),
    /// Recomputation reached `frontier` with other rows.
    pub(super) failed: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct PeerMark {
    pub(super) behind: bool,
    /// The peer reported everything at or before this as applied.
    pub(super) applied: Marks,
    /// The peer reported holding everything at or before this.
    #[serde(default)]
    pub(super) held: Marks,
    /// Since when, in Unix milliseconds, the peer has held back the horizon.
    #[serde(default)]
    pub(super) holding_since: Option<i64>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Import {
    pub(super) record: Vec<u8>,
    pub(super) strong: Option<(Vec<u8>, Vec<u8>)>,
    pub(super) table: u32,
    pub(super) position: i64,
}
/// Everything persistent besides the two row digests.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Stored {
    /// Frontier of the checkpoint copy; None after an aborted import.
    pub(super) shadow: Option<Marks>,
    pub(super) clock: u64,
    pub(super) history: (Commitment, u64),
    pub(super) candidate: Option<Candidate>,
    pub(super) record: Option<Vec<u8>>,
    pub(super) horizon: Marks,
    pub(super) peers: Vec<(NodeId, PeerMark)>,
    /// Sequence and head the retained strong results start after.
    pub(super) strong_base: Option<(u64, Commitment)>,
    pub(super) import: Option<Import>,
}
impl Default for Stored {
    fn default() -> Self {
        Self {
            shadow: Some(Vec::new()),
            clock: 0,
            history: Default::default(),
            candidate: None,
            record: None,
            horizon: Vec::new(),
            peers: Vec::new(),
            strong_base: None,
            import: None,
        }
    }
}
pub(super) struct Table {
    pub(super) logical: String,
    pub(super) live: String,
    pub(super) shadow: String,
    pub(super) without_rowid: bool,
    /// Declared by the application: digested, exported and replaced.
    pub(super) checkpointed: bool,
}
pub(super) struct Retention {
    pub(super) policy: RetentionPolicy,
    pub(super) shadow_writer: ScopedConnection,
    pub(super) shadow_reader: Option<ScopedConnection>,
    pub(super) tables: Vec<Table>,
    pub(super) tracker: Arc<Mutex<Tracker>>,
    /// Tracker slots of the projection and of the checkpoint copy.
    pub(super) slots: (usize, usize),
    pub(super) live: SetDigest,
    pub(super) shadow: SetDigest,
    /// Digests a step set outright; tracked changes after that still count.
    pub(super) bases: (Option<SetDigest>, Option<SetDigest>),
    pub(super) stored: Stored,
    pub(super) record: Option<(CheckpointRecord, Commitment)>,
    pub(super) definition: Commitment,
    /// A flush covers the latest record.
    pub(super) record_flushed: bool,
    /// The horizon may be able to move, or rows remain behind it.
    pub(super) prune_wanted: bool,
    pub(super) lease: Option<std::time::Instant>,
}
/// Counters a finished import puts in place of the worker's.
pub(super) struct Switched {
    pub(super) frontier: BTreeMap<NodeId, AuthorSequence>,
    pub(super) lamport: u64,
    pub(super) revision: LocalRevision,
    pub(super) retained: (usize, usize),
}
pub(super) struct Advanced {
    pub(super) stored: Stored,
    pub(super) step: Step,
    pub(super) failed: bool,
}
impl Retention {
    pub(super) fn new<A: StoredApplication>(
        conn: &ScopedConnection,
        app: &A,
        policy: RetentionPolicy,
    ) -> Result<Self, Error> {
        let namespace = match conn.names().namespace() {
            Some(namespace) => format!("{namespace}_ckpt"),
            None => "rocky_ckpt".to_string(),
        };
        let schema = app.installed_schema();
        let shadow = conn.rescoped(Some(&namespace), &schema.statements);
        let declared = &schema.checkpoint_tables;
        let mut tables = Vec::new();
        for (logical, without_rowid) in crate::scoped_connection::tables(&schema.statements) {
            let physical = |names: &crate::scoped_connection::Names| {
                names
                    .physical(&logical)
                    .map(str::to_string)
                    .ok_or_else(|| Error::Incompatible("unmapped application table".into()))
            };
            tables.push(Table {
                live: physical(conn.names())?,
                shadow: physical(shadow.names())?,
                checkpointed: declared
                    .iter()
                    .any(|table| table.eq_ignore_ascii_case(&logical)),
                logical,
                without_rowid,
            });
        }
        if declared.iter().any(|table| {
            !tables
                .iter()
                .any(|known| known.logical.eq_ignore_ascii_case(table))
        }) {
            return Err(Error::Incompatible(
                "a checkpoint table is not in the application schema".into(),
            ));
        }
        let tracker = conn.connection().tracker()?;
        let slots = {
            let mut tracker = tracker
                .lock()
                .map_err(|_| Error::Corrupt("row tracker".into()))?;
            let checkpointed = || tables.iter().filter(|table| table.checkpointed);
            (
                tracker.register(
                    checkpointed().map(|table| (table.live.clone(), table.logical.clone())),
                ),
                tracker.register(
                    checkpointed().map(|table| (table.shadow.clone(), table.logical.clone())),
                ),
            )
        };
        Ok(Self {
            policy,
            shadow_writer: shadow,
            shadow_reader: None,
            tables,
            tracker,
            slots,
            live: SetDigest::default(),
            shadow: SetDigest::default(),
            bases: (None, None),
            stored: Stored::default(),
            record: None,
            definition: definition_commitment(app),
            record_flushed: false,
            prune_wanted: true,
            lease: None,
        })
    }
    pub(super) fn attach_reader(&mut self, reader: Arc<crate::sqlite::Connection>) {
        self.shadow_reader = Some(self.shadow_writer.with_connection(reader));
    }
    pub(super) fn checkpointed(&self) -> impl Iterator<Item = (usize, &Table)> {
        self.tables
            .iter()
            .filter(|table| table.checkpointed)
            .enumerate()
    }
    pub(super) fn horizon(&self, node: NodeId) -> u64 {
        at(&self.stored.horizon, node)
    }
    pub(super) fn below_horizon(&self, id: OperationId) -> bool {
        id.sequence.get() <= self.horizon(id.node)
    }
    /// An acknowledgment may let the horizon follow the checkpoint further.
    pub(super) fn acknowledged(&mut self) {
        if let Some((record, _)) = &self.record {
            let window = self.policy.window as u64;
            self.prune_wanted |= record.frontier.0.iter().any(|(node, covered)| {
                covered.get().saturating_sub(window) > at(&self.stored.horizon, *node)
            });
        }
    }
    /// Whether `peer` reported holding `id` in a catch-up.
    pub(super) fn held_by(&self, peer: NodeId, id: OperationId) -> bool {
        self.stored
            .peers
            .iter()
            .any(|(known, mark)| *known == peer && at(&mark.held, id.node) >= id.sequence.get())
    }
    /// What `peer` last reported holding and applying.
    pub(super) fn reported(&self, peer: NodeId) -> (Marks, Marks) {
        self.stored
            .peers
            .iter()
            .find(|(known, _)| *known == peer)
            .map(|(_, mark)| (mark.held.clone(), mark.applied.clone()))
            .unwrap_or_default()
    }
    /// The checkpoint copy's frontier, when it can serve as a replay base.
    pub(super) fn base(&self) -> Option<&[(NodeId, u64)]> {
        self.stored.shadow.as_deref()
    }
    /// The sequence up to which `peer` holds `node`'s pruned operations:
    /// the horizon, or what it reported applied while declared behind.
    pub(super) fn acknowledged_behind(&self, peer: NodeId, node: NodeId) -> u64 {
        let horizon = self.horizon(node);
        match self.stored.peers.iter().find(|(known, _)| *known == peer) {
            Some((_, mark)) if mark.behind => at(&mark.applied, node).min(horizon),
            _ => horizon,
        }
    }
    pub(super) fn status_record(&self) -> Option<&CheckpointRecord> {
        self.record.as_ref().map(|(record, _)| record)
    }
    pub(super) fn strong_base(&self) -> Option<(u64, Commitment)> {
        self.stored.strong_base
    }
    pub(super) fn handle_window(&self) -> usize {
        self.policy.handles
    }
    pub(super) fn row_audit(&self, marker: String) -> RowAudit {
        RowAudit {
            marker,
            tables: self
                .checkpointed()
                .map(|(_, table)| {
                    (
                        table.logical.clone(),
                        table.live.clone(),
                        table.shadow.clone(),
                    )
                })
                .collect(),
        }
    }
    pub(super) fn tracker(&self) -> Result<std::sync::MutexGuard<'_, Tracker>, Error> {
        self.tracker
            .lock()
            .map_err(|_| Error::Corrupt("row tracker".into()))
    }
    /// Starts a step: forgets row changes of an attempt that did not commit.
    pub(super) fn begin_step(&mut self) -> Result<(), Error> {
        let slots = self.slots;
        let mut tracker = self.tracker()?;
        tracker.take(slots.0);
        tracker.take(slots.1);
        drop(tracker);
        self.bases = (None, None);
        Ok(())
    }
    /// The digests as the running step leaves them.
    pub(super) fn current(&self) -> Result<(SetDigest, SetDigest), Error> {
        let tracker = self.tracker()?;
        let mut live = self.bases.0.unwrap_or(self.live);
        live.merge(&tracker.peek(self.slots.0));
        let mut shadow = self.bases.1.unwrap_or(self.shadow);
        shadow.merge(&tracker.peek(self.slots.1));
        Ok((live, shadow))
    }
    /// Sets the checkpoint copy's digest outright, after statements the
    /// tracker need not have followed.
    pub(super) fn rebase_shadow(&mut self, digest: SetDigest) -> Result<(), Error> {
        let slot = self.slots.1;
        self.tracker()?.take(slot);
        self.bases.1 = Some(digest);
        Ok(())
    }
    pub(super) fn rebase_live(&mut self, digest: SetDigest) -> Result<(), Error> {
        let slot = self.slots.0;
        self.tracker()?.take(slot);
        self.bases.0 = Some(digest);
        Ok(())
    }
    pub(super) fn adopt(&mut self, stored: Stored) -> Result<(), Error> {
        self.record = match &stored.record {
            Some(bytes) => {
                let record = CheckpointRecord::decode(bytes)
                    .map_err(|_| Error::Corrupt("stored checkpoint record".into()))?;
                let commitment = record.commitment();
                Some((record, commitment))
            }
            None => None,
        };
        if stored.record != self.stored.record {
            self.record_flushed = false;
        }
        self.stored = stored;
        Ok(())
    }
    pub(super) fn flushed(&mut self) {
        self.record_flushed = true;
    }
    pub(super) fn hold(&self) -> Option<CheckpointHold> {
        if self.stored.import.is_some() {
            Some(CheckpointHold::Rebootstrap)
        } else if self.stored.shadow.is_none() {
            Some(CheckpointHold::NoBase)
        } else if self
            .stored
            .candidate
            .as_ref()
            .is_some_and(|candidate| candidate.failed)
        {
            Some(CheckpointHold::Unverifiable)
        } else {
            None
        }
    }
}
/// The installed reducer, schema, projection and handler generations.
pub(crate) fn definition_commitment<A: StoredApplication>(app: &A) -> Commitment {
    rocky_core::installed_definition(app)
}

impl<A> Worker<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    pub(super) fn shadow(&self) -> &ScopedConnection {
        match (&self.retention.shadow_reader, self.reading) {
            (Some(reader), true) => reader,
            _ => &self.retention.shadow_writer,
        }
    }
    pub(super) fn retention_horizon(&self) -> Result<rocky_core::AppliedFrontier, Error> {
        frontier(&self.retention.stored.horizon)
    }
    pub(super) fn applied_marks(&self) -> Marks {
        self.frontier
            .iter()
            .map(|(node, sequence)| (*node, sequence.get()))
            .collect()
    }
    /// Adds the checkpoint tables to a file that has none: a new file, or
    /// one written before checkpoints, whose whole history is still retained.
    pub(super) async fn install_retention(&mut self) -> Result<(), Error> {
        for sql in &self.schema.statements {
            self.retention
                .shadow_writer
                .execute(crate::bootstrap::idempotent(sql)?, ())
                .await?;
        }
        let mut live = SetDigest::default();
        for (_, table) in self.retention.checkpointed() {
            self.conn().connection().scan(
                &format!("SELECT * FROM {}", quoted(&table.live)),
                Vec::new(),
                |row| {
                    live.add(&crate::digest::row_hash(&table.logical, row.iter()));
                    Ok(())
                },
            )?;
        }
        self.conn()
            .execute(
                "INSERT INTO rocky_checkpoint VALUES(1,?,?,?)",
                crate::sqlite::params![
                    live.encode(),
                    SetDigest::default().encode(),
                    encode_stored(&Stored::default())?
                ],
            )
            .await?;
        Ok(())
    }
    /// Rereads the digests and checkpoint state from storage.
    pub(super) async fn load_retention(&mut self) -> Result<(), Error> {
        let row = rows(
            self.conn(),
            "SELECT live,shadow,state FROM rocky_checkpoint WHERE id=1",
        )
        .await?;
        let Some(
            [
                SqlValue::Blob(live),
                SqlValue::Blob(shadow),
                SqlValue::Blob(state),
            ],
        ) = row.first().map(Vec::as_slice)
        else {
            return Err(Error::Corrupt("checkpoint state".into()));
        };
        let corrupt = || Error::Corrupt("checkpoint state".into());
        self.retention.live = SetDigest::decode(live).ok_or_else(corrupt)?;
        self.retention.shadow = SetDigest::decode(shadow).ok_or_else(corrupt)?;
        let stored: Stored = serde_json::from_slice(state).map_err(|_| corrupt())?;
        self.retention.stored = Stored::default();
        self.retention.adopt(stored)?;
        // The file writer flushes once at open, before anything is exposed.
        self.retention.record_flushed = false;
        self.retention.prune_wanted = true;
        let stored = &self.retention.stored;
        if let Some((record, _)) = &self.retention.record {
            let covered = marks(&record.frontier);
            if record.scope != self.identity.scope
                || record.definition != self.retention.definition
                || stored
                    .horizon
                    .iter()
                    .any(|(node, sequence)| *sequence > at(&covered, *node))
                || stored.shadow.as_ref().is_some_and(|shadow| {
                    covered
                        .iter()
                        .any(|(node, sequence)| *sequence > at(shadow, *node))
                })
            {
                return Err(Error::Corrupt(
                    "checkpoint record differs from this domain".into(),
                ));
            }
        } else if !stored.horizon.is_empty() {
            return Err(Error::Corrupt("horizon without a checkpoint".into()));
        }
        Ok(())
    }
    /// Writes the digests a step changed; the caller adopts them after the
    /// step's savepoint is released.
    pub(super) async fn settle(&mut self) -> Result<(SetDigest, SetDigest), Error> {
        let slots = self.retention.slots;
        let (live, shadow) = {
            let mut tracker = self.retention.tracker()?;
            let failed = || Error::Corrupt("a changed row could not be read for its digest".into());
            let live = tracker.take(slots.0).ok_or_else(failed)?;
            let shadow = tracker.take(slots.1).ok_or_else(failed)?;
            (live, shadow)
        };
        let mut digests = (
            self.retention.bases.0.unwrap_or(self.retention.live),
            self.retention.bases.1.unwrap_or(self.retention.shadow),
        );
        digests.0.merge(&live);
        digests.1.merge(&shadow);
        if digests != (self.retention.live, self.retention.shadow) {
            self.conn()
                .execute(
                    "UPDATE rocky_checkpoint SET live=?,shadow=? WHERE id=1",
                    crate::sqlite::params![digests.0.encode(), digests.1.encode()],
                )
                .await?;
        }
        Ok(digests)
    }
    pub(super) fn adopt_digests(&mut self, digests: (SetDigest, SetDigest)) {
        (self.retention.live, self.retention.shadow) = digests;
        self.retention.bases = (None, None);
    }
    pub(super) async fn persist(&self, stored: &Stored) -> Result<(), Error> {
        self.conn()
            .execute(
                "UPDATE rocky_checkpoint SET state=? WHERE id=1",
                [encode_stored(stored)?],
            )
            .await?;
        Ok(())
    }
    /// Whether the file writer should run a maintenance step beside a group.
    pub(crate) fn maintenance_due(&self) -> bool {
        let retention = &self.retention;
        if !retention.policy.automatic || retention.hold().is_some() {
            return false;
        }
        if retention.stored.candidate.is_some() {
            return !retention.leased();
        }
        if retention.record.is_some() && retention.record_flushed && retention.prune_wanted {
            return true;
        }
        let base = retention.base().unwrap_or_default();
        let since: u64 = self
            .frontier
            .iter()
            .map(|(node, sequence)| sequence.get().saturating_sub(at(base, *node)))
            .sum();
        since > 0
            && !retention.leased()
            && ((retention.policy.checkpoint_operations > 0
                && since >= retention.policy.checkpoint_operations as u64)
                || self.retained_bytes >= retention.policy.retained_bytes / 2)
    }
    /// A verified checkpoint waits for a flush before the horizon may move.
    pub(crate) fn wants_flush(&self) -> bool {
        let retention = &self.retention;
        retention.policy.automatic
            && retention.record.is_some()
            && !retention.record_flushed
            && retention.prune_wanted
            && retention.stored.candidate.is_none()
    }
    /// One bounded step of whatever is due. Failures stay in the status; a
    /// failed step rolled back alone.
    pub(crate) async fn maintain(&mut self, writes: usize) -> Result<(), Error> {
        let retention = &self.retention;
        if retention.stored.candidate.is_none()
            && retention.record.is_some()
            && retention.record_flushed
            && retention.prune_wanted
        {
            return self.prune_step().await.map(|_| ());
        }
        let budget = (2 * writes).clamp(ADVANCE_BOUNDS.0, ADVANCE_BOUNDS.1);
        match self.checkpoint_step(false, budget).await {
            // Held by an unresolved request; retried beside a later group.
            Err(Error::Rejected(_)) => Ok(()),
            result => result.map(|_| ()),
        }
    }
    pub(super) async fn retention(&mut self, request: Request) -> Result<Response, Error> {
        match request {
            Request::Status => self
                .read_transaction(async |worker| worker.retention_status().await)
                .await
                .map(|status| Response::Status(Box::new(status))),
            Request::Checkpoint => self
                .checkpoint_step(true, ADVANCE_OPERATIONS)
                .await
                .map(Response::Step),
            Request::Prune => self.prune_step().await.map(Response::Step),
            Request::DeclareBehind(peer) => {
                self.validate_peer(peer)?;
                let stored = self
                    .transaction(async |worker| {
                        let mut stored = worker.retention.stored.clone();
                        mark(&mut stored, peer).behind = true;
                        worker.persist(&stored).await?;
                        worker.inject_fault().await?;
                        Ok(stored)
                    })
                    .await?;
                self.retention.adopt(stored)?;
                self.retention.prune_wanted = true;
                Ok(Response::Unit)
            }
            Request::Catchup(peer, state, after) => {
                self.app
                    .admit_read(self.identity.scope, self.identity.caller, &self.policy)
                    .map_err(application)?;
                if let Some(peer) = peer {
                    self.validate_peer(peer)?;
                }
                let (stored, catchup) = self
                    .transaction(async |worker| worker.catch_up(peer, &state, after).await)
                    .await?;
                if let Some(stored) = stored {
                    self.retention.adopt(stored)?;
                    self.retention.prune_wanted = true;
                }
                Ok(Response::Catchup(catchup))
            }
            Request::Export(after) => {
                self.app
                    .admit_read(self.identity.scope, self.identity.caller, &self.policy)
                    .map_err(application)?;
                let page = self
                    .read_transaction(async |worker| worker.export(after).await)
                    .await?;
                self.retention.lease = Some(std::time::Instant::now() + EXPORT_LEASE);
                Ok(Response::Page(Box::new(page)))
            }
            Request::Import(page) => self.import(*page).await.map(Response::Imported),
            Request::AbortImport => {
                let stored = self
                    .transaction(async |worker| {
                        let mut stored = worker.retention.stored.clone();
                        if stored.import.take().is_none() {
                            return Ok(None);
                        }
                        worker.clear_shadow().await?;
                        worker.persist(&stored).await?;
                        worker.inject_fault().await?;
                        Ok(Some(stored))
                    })
                    .await?;
                if let Some(stored) = stored {
                    self.retention.adopt(stored)?;
                }
                Ok(Response::Unit)
            }
            Request::Audit => {
                // Reads all retained history: no time bound, unlike a query.
                self.unbounded = true;
                let result = self
                    .read_transaction(async |worker| worker.audit().await)
                    .await;
                self.unbounded = false;
                result.map(Response::Audit)
            }
            Request::Rejected => self
                .read_transaction(async |worker| worker.rejected_local().await)
                .await
                .map(Response::Rejected),
            Request::Unsettled => self
                .read_transaction(async |worker| worker.unsettled().await)
                .await
                .map(Response::Unsettled),
        }
    }
    pub(super) async fn retention_status(&self) -> Result<RetentionStatus, Error> {
        let retention = &self.retention;
        let hold = match retention.hold() {
            None if self.unresolved_requests().await? => Some(CheckpointHold::UnresolvedRequest),
            hold => hold,
        };
        Ok(RetentionStatus {
            checkpoint: retention
                .record
                .as_ref()
                .map(|(record, commitment)| CheckpointInfo {
                    commitment: *commitment,
                    record: record.clone(),
                }),
            pending: match &retention.stored.candidate {
                Some(candidate) if !candidate.failed => Some(frontier(&candidate.frontier)?),
                _ => None,
            },
            hold,
            horizon: frontier(&retention.stored.horizon)?,
            behind: retention
                .stored
                .peers
                .iter()
                .filter(|(_, mark)| mark.behind)
                .map(|(peer, _)| *peer)
                .collect(),
            retained_operations: self.retained,
            retained_bytes: self.retained_bytes,
            policy: retention.policy,
            peers: self
                .config
                .delivery_targets
                .iter()
                .map(|peer| {
                    let (held, applied) = retention.reported(*peer);
                    Ok(PeerRetention {
                        peer: *peer,
                        full_node: self.config.full_nodes.contains(peer),
                        behind: retention
                            .stored
                            .peers
                            .iter()
                            .any(|(known, mark)| known == peer && mark.behind),
                        held: frontier(&held)?,
                        applied: frontier(&applied)?,
                    })
                })
                .collect::<Result<_, Error>>()?,
        })
    }
    /// Starts a checkpoint of the applied frontier, or replays the next
    /// operations into the checkpoint copy; the last step compares it with
    /// the digest the projection had at that frontier.
    pub(super) async fn checkpoint_step(
        &mut self,
        requested: bool,
        budget: usize,
    ) -> Result<Step, Error> {
        let advanced = self
            .transaction(async |worker| worker.advance(requested, budget).await)
            .await?;
        let Some(advanced) = advanced else {
            return Ok(Step::Done);
        };
        self.retention.adopt(advanced.stored)?;
        self.retention.prune_wanted = true;
        if advanced.failed {
            return Err(Error::Corrupt(
                "the projection differs from the checkpoint recomputed from retained history"
                    .into(),
            ));
        }
        Ok(advanced.step)
    }
    pub(super) async fn advance(
        &mut self,
        requested: bool,
        budget: usize,
    ) -> Result<Option<Advanced>, Error> {
        let mut stored = self.retention.stored.clone();
        if stored.import.is_some() {
            return Err(Error::Rejected("a checkpoint import is in progress".into()));
        }
        let Some(mut shadow) = stored.shadow.clone() else {
            return Err(Error::Rejected(
                "no checkpoint base; import a checkpoint".into(),
            ));
        };
        let applied = self.applied_marks();
        let start = match &stored.candidate {
            None => self
                .retention
                .record
                .as_ref()
                .is_none_or(|(record, _)| marks(&record.frontier) != applied),
            // Only an explicit request retries a checkpoint that failed.
            Some(candidate) if candidate.failed => {
                if !requested {
                    return Ok(None);
                }
                true
            }
            Some(_) => false,
        };
        if stored.candidate.is_none() && !start {
            return Ok(None);
        }
        if start {
            self.release_expired().await?;
            if self.unresolved_requests().await? {
                return Err(Error::Rejected(
                    "an unresolved strong request holds the checkpoint at its sealed cut".into(),
                ));
            }
            let live = self.retention.current()?.0;
            stored.candidate = Some(Candidate {
                frontier: applied,
                live: (live.sum, live.count),
                failed: false,
            });
            self.persist(&stored).await?;
            self.inject_fault().await?;
            return Ok(Some(Advanced {
                stored,
                step: Step::More,
                failed: false,
            }));
        }
        let Some(candidate) = stored.candidate.clone() else {
            return Ok(None);
        };
        let mut history = SetDigest {
            sum: stored.history.0,
            count: stored.history.1,
        };
        let mut replayed = 0;
        let mut progress = true;
        while progress && replayed < budget {
            progress = false;
            for (node, target) in &candidate.frontier {
                if replayed == budget {
                    break;
                }
                let next = at(&shadow, *node) + 1;
                if next > *target {
                    continue;
                }
                let id = OperationId {
                    node: *node,
                    sequence: AuthorSequence::new(next)?,
                };
                let Some((bytes, status)) = self.record(id).await? else {
                    return Err(Error::Corrupt(
                        "an operation after the checkpoint is missing".into(),
                    ));
                };
                if status != "applied" {
                    return Err(Error::Corrupt(
                        "an operation below the applied frontier is not applied".into(),
                    ));
                }
                let op = Envelope::decode(&bytes)?;
                if op.id != id {
                    return Err(Error::Corrupt("operation stored under another key".into()));
                }
                if op
                    .dependencies
                    .iter()
                    .any(|dependency| dependency.sequence.get() > at(&shadow, dependency.node))
                {
                    continue;
                }
                self.replay(&op).await?;
                set(&mut shadow, *node, next);
                stored.clock = stored.clock.max(op.lamport.get());
                history.add(&operation_hash(&bytes));
                replayed += 1;
                progress = true;
            }
        }
        let reached = candidate
            .frontier
            .iter()
            .all(|(node, target)| at(&shadow, *node) == *target);
        if !reached && replayed == 0 {
            return Err(Error::Corrupt(
                "applied history has missing dependencies".into(),
            ));
        }
        stored.shadow = Some(shadow);
        stored.history = (history.sum, history.count);
        let mut failed = false;
        if reached {
            let recomputed = self.retention.current()?.1;
            if (recomputed.sum, recomputed.count) == candidate.live {
                let record = self.record_at(&stored, &candidate, recomputed).await?;
                stored.record = Some(record.encode());
                stored.candidate = None;
            } else {
                failed = true;
                if let Some(candidate) = &mut stored.candidate {
                    candidate.failed = true;
                }
            }
        }
        self.persist(&stored).await?;
        self.inject_fault().await?;
        Ok(Some(Advanced {
            stored,
            step: if reached { Step::Done } else { Step::More },
            failed,
        }))
    }
    /// Applies one applied operation to the checkpoint copy, as replay.
    pub(super) async fn replay(&self, op: &Envelope) -> Result<(), Error> {
        let shadow = self.shadow();
        let mut loaded = Vec::new();
        for statement in self.app.touched(op) {
            loaded.extend(
                storage::collect_rows(
                    shadow
                        .query(&statement.sql, bindings(statement.values))
                        .await?,
                )
                .await?,
            );
        }
        let mut state = self.app.window(loaded).map_err(application)?;
        self.app
            .apply_ready(&mut state, op, AdmissionBoundary::Incoming)
            .map_err(|error| Error::Corrupt(format!("checkpoint replay refused: {error}")))?;
        for statement in self.app.project(&state, op) {
            shadow
                .execute(&statement.sql, bindings(statement.values))
                .await?;
        }
        Ok(())
    }
    /// The complete state at the checkpoint copy's frontier, for a sealed cut.
    pub(super) async fn base_state(&self) -> Result<A::State, Error> {
        self.app
            .window(rows(self.shadow(), &self.schema.projection_query).await?)
            .map_err(application)
    }
    pub(super) async fn record_at(
        &self,
        stored: &Stored,
        candidate: &Candidate,
        state: SetDigest,
    ) -> Result<CheckpointRecord, Error> {
        let (previous, sequence) = match &self.retention.record {
            Some((record, commitment)) => (*commitment, record.sequence + 1),
            None => ([0; 32], 1),
        };
        let count = async |sql: &str| -> Result<(u64, u64), Error> {
            match rows(self.conn(), sql).await?.first().map(Vec::as_slice) {
                Some([SqlValue::Integer(count), SqlValue::Integer(floor)]) => {
                    Ok((*count as u64, *floor as u64))
                }
                _ => Err(Error::Corrupt("handle summary".into())),
            }
        };
        let mutations =
            count("SELECT COUNT(*),COALESCE(MIN(sequence),0) FROM rocky_mutation_handles").await?;
        let commands =
            count("SELECT COUNT(*),COALESCE(MIN(sequence),0) FROM rocky_command_handles").await?;
        let descriptor = self.app.descriptor();
        Ok(CheckpointRecord {
            scope: self.identity.scope,
            definition: self.retention.definition,
            versions: descriptor.operations,
            projection: descriptor.projection,
            frontier: frontier(&candidate.frontier)?,
            clock: stored.clock,
            history: SetDigest {
                sum: stored.history.0,
                count: stored.history.1,
            },
            state,
            node: self.identity.node,
            policy_version: self.app.policy_version(&self.policy),
            key_epoch: 0,
            sequence,
            previous,
            strong: self.strong_anchor().await?,
            handles: Deduplication {
                mutations: mutations.0,
                mutation_floor: mutations.1,
                commands: commands.0,
                command_floor: commands.1,
            },
            superseded_recovery: Vec::new(),
            sole_recovery: Vec::new(),
        })
    }
    pub(super) async fn strong_anchor(&self) -> Result<StrongAnchor, Error> {
        let head = self.local_strong_head().await?;
        let mut unresolved = SetDigest::default();
        let mut result = None;
        let mut confirmed = BTreeSet::new();
        for (package, _, receipt, _, applied) in self.attempts().await? {
            let hash = package.confirmation()?.result;
            if applied {
                confirmed.insert(package.request.id);
                if package.sequence == head.sequence && receipt.is_some() {
                    result = Some(hash);
                }
            } else {
                unresolved.add(&hash);
            }
        }
        for row in rows(self.conn(), "SELECT bytes FROM rocky_requests").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(Error::Corrupt("request row".into()));
            };
            if !confirmed.contains(&rocky_core::StrongRequest::decode(bytes)?.id) {
                unresolved.add(&rocky_core::commitment(
                    "rocky.checkpoint.request.v1",
                    bytes,
                ));
            }
        }
        Ok(StrongAnchor {
            configuration: head.configuration,
            sequence: head.sequence,
            head: head.head,
            result,
            unresolved,
        })
    }
    /// A request without an applied result needs the state of its cut, so
    /// the checkpoint copy must not move past it until it is released.
    pub(super) async fn unresolved_requests(&self) -> Result<bool, Error> {
        Ok(!self.holding_requests().await?.is_empty())
    }
    /// Unreleased requests without an applied result: when this replica
    /// stored each, and whether a submitted attempt has an unknown outcome.
    async fn holding_requests(&self) -> Result<Vec<(RequestId, i64, bool)>, Error> {
        let requests = self.open_requests().await?;
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let mut unknown = BTreeSet::new();
        for (package, ..) in self
            .attempts_where("WHERE applied=0 AND state='unknown'")
            .await?
        {
            unknown.insert(package.request.id);
        }
        let mut holding = Vec::new();
        for (bytes, stored) in requests {
            let id = rocky_core::StrongRequest::decode(&bytes)?.id;
            let stored = match stored {
                SqlValue::Integer(stored) => stored,
                _ => i64::MAX,
            };
            holding.push((id, stored, unknown.contains(&id)));
        }
        Ok(holding)
    }
    /// Releases requests that held checkpoints longer than the policy allows.
    /// One with a submitted attempt of unknown outcome keeps holding: its
    /// result may be confirmed and must then be verified at its cut.
    pub(super) async fn release_expired(&self) -> Result<(), Error> {
        let Some(hold) = self.retention.policy.request_hold else {
            return Ok(());
        };
        let now = now_millis();
        for (id, stored, unknown) in self.holding_requests().await? {
            if !unknown && now.saturating_sub(stored) >= hold.as_millis() as i64 {
                self.mark_released(id).await?;
            }
        }
        Ok(())
    }
    /// Releases one request at once; see `Replica::release_command`.
    pub(super) async fn release_request(&self, id: RequestId) -> Result<(), Error> {
        match self
            .holding_requests()
            .await?
            .into_iter()
            .find(|(held, ..)| *held == id)
        {
            Some((_, _, true)) => Err(Error::Rejected(
                "a submitted attempt has an unknown outcome; reconcile it first".into(),
            )),
            Some(_) => self.mark_released(id).await,
            None => {
                let mut known = self
                    .conn()
                    .query(
                        "SELECT 1 FROM rocky_requests WHERE node=? AND sequence=?",
                        crate::sqlite::params![id.node.0.to_vec(), id.sequence.get() as i64],
                    )
                    .await?;
                match known.next().await? {
                    // Already released, or resolved: nothing holds.
                    Some(_) => Ok(()),
                    None => Err(Error::Rejected("unknown command request".into())),
                }
            }
        }
    }
    async fn mark_released(&self, id: RequestId) -> Result<(), Error> {
        self.mark_hold(id, 1).await
    }
    /// `released` is 1 for a local release, or a [`Settled`] code.
    async fn mark_hold(&self, id: RequestId, released: i64) -> Result<(), Error> {
        self.conn()
            .execute(
                "INSERT INTO rocky_request_holds VALUES(?,?,?,?) ON CONFLICT(node,sequence) DO UPDATE SET released=excluded.released",
                crate::sqlite::params![
                    id.node.0.to_vec(),
                    id.sequence.get() as i64,
                    now_millis(),
                    released
                ],
            )
            .await?;
        Ok(())
    }
    /// Records that the coordinator will never execute `id`. Unlike a
    /// release this holds even with a submitted attempt of unknown outcome:
    /// the coordinator's answer settles it.
    pub(super) async fn settle_request(
        &self,
        id: RequestId,
        settled: crate::strong::Settled,
    ) -> Result<(), Error> {
        let mut known = self
            .conn()
            .query(
                "SELECT 1 FROM rocky_requests WHERE node=? AND sequence=?",
                crate::sqlite::params![id.node.0.to_vec(), id.sequence.get() as i64],
            )
            .await?;
        if known.next().await?.is_none() {
            return Err(Error::Rejected("unknown command request".into()));
        }
        self.mark_hold(id, settled as i64).await
    }
    pub(super) async fn rejected_local(&self) -> Result<Vec<(OperationId, String)>, Error> {
        let mut records = self
            .conn()
            .query(
                "SELECT node,sequence,reason FROM rocky_rejected_local ORDER BY node,sequence",
                (),
            )
            .await?;
        let mut rejected = Vec::new();
        while let Some(row) = records.next().await? {
            rejected.push((
                incoming::operation_id(&row.get::<Vec<u8>>(0)?, row.get(1)?)?,
                row.get(2)?,
            ));
        }
        Ok(rejected)
    }
    pub(super) async fn unsettled(&self) -> Result<Vec<(OperationId, bool, bool)>, Error> {
        let mut records = self
            .conn()
            .query(
                "SELECT node,sequence,origin,status FROM rocky_operations WHERE status<>'applied' ORDER BY node,sequence",
                (),
            )
            .await?;
        let mut unsettled = Vec::new();
        while let Some(row) = records.next().await? {
            unsettled.push((
                incoming::operation_id(&row.get::<Vec<u8>>(0)?, row.get(1)?)?,
                row.get::<String>(2)? == "local",
                row.get::<String>(3)? == "rejected",
            ));
        }
        Ok(unsettled)
    }
}
impl Retention {
    pub(super) fn leased(&self) -> bool {
        self.lease
            .is_some_and(|lease| std::time::Instant::now() < lease)
    }
}
pub(super) fn mark(stored: &mut Stored, peer: NodeId) -> &mut PeerMark {
    let index = match stored.peers.iter().position(|(known, _)| *known == peer) {
        Some(index) => index,
        None => {
            stored.peers.push((peer, PeerMark::default()));
            stored.peers.len() - 1
        }
    };
    &mut stored.peers[index].1
}
pub(super) fn encode_stored(stored: &Stored) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(stored).map_err(|error| Error::Corrupt(error.to_string()))
}
