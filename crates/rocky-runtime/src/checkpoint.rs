//! Checkpoints, the pruning pruned prefix and rebootstrap of the trusted path.
//! See `docs/runtime/checkpoints.md`.
use crate::{Command, Error, Replica, SyncState, TransferPage, digest::SetDigest};
use rocky_core::{
    AppliedFrontier, AuthorSequence, Commitment, NodeId, OperationId, Scope, SqlValue,
    StoredApplication, Versions,
};
use tokio::sync::oneshot;

/// When a domain checkpoints and prunes its history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Applied operations after the last checkpoint that start the next one.
    /// Zero starts one only on request or at half of `retained_bytes`.
    pub checkpoint_operations: usize,
    /// Most recent applied operations of each author kept behind a
    /// checkpoint, so a peer that is slightly behind catches up without one.
    pub window: usize,
    /// Hard bound on retained operation bytes. Half of it starts a
    /// checkpoint; at the bound writes and receipts fail with
    /// `Error::Retention` until pruning releases history.
    pub retained_bytes: usize,
    /// Most recent local mutation handles kept for retries after their
    /// operations are pruned. An older handle is forgotten.
    pub handles: usize,
    /// Run checkpoint and prune steps beside writes. Otherwise only
    /// `Replica::checkpoint` and `Replica::prune` run them.
    pub automatic: bool,
    /// How long a delivery target may hold back the pruned prefix before it is
    /// declared behind. None waits for `Replica::declare_behind`.
    pub behind_after: Option<std::time::Duration>,
    /// Full nodes of `RuntimeConfig::full_nodes` that must have applied an
    /// operation before the pruned prefix passes it; other delivery targets then
    /// do not hold it back. Zero waits for every delivery target instead.
    pub custody: usize,
}
impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            checkpoint_operations: 4096,
            window: 1024,
            retained_bytes: 64 * crate::MAX_FRAME_BYTES,
            handles: 65_536,
            automatic: true,
            behind_after: None,
            custody: 0,
        }
    }
}

/// Handles retained for retries when the checkpoint was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Deduplication {
    pub mutations: u64,
    /// Lowest local sequence that still has a mutation handle.
    pub mutation_floor: u64,
}
/// The canonical record of one checkpoint. Everything up to `state` is the
/// same on every replica that has applied `frontier`; the rest is evidence of
/// the replica that made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointRecord {
    pub scope: Scope,
    /// Installed definition: reducer, schema and projection.
    pub definition: Commitment,
    pub versions: Versions,
    pub projection: u32,
    pub frontier: AppliedFrontier,
    /// Highest Lamport time of an operation at or before `frontier`.
    pub clock: u64,
    /// Every operation at or before `frontier`, by encoded bytes.
    pub history: SetDigest,
    /// Every stored row of the checkpoint tables at `frontier`, deletion
    /// evidence included.
    pub state: SetDigest,
    pub node: NodeId,
    pub policy_version: u64,
    /// The trusted path has no keys; always zero.
    pub key_epoch: u64,
    pub sequence: u64,
    /// The record this one was recomputed from; zero for the first.
    pub previous: Commitment,
    pub handles: Deduplication,
    /// Recovery copies this checkpoint replaces, and those it leaves as the
    /// only copy. The trusted path keeps none.
    pub superseded_recovery: Vec<Commitment>,
    pub sole_recovery: Vec<Commitment>,
}
const RECORD_MAGIC: &[u8] = b"rocky.checkpoint.v2";
const NOTICE_MAGIC: &[u8] = b"rocky.pruned.v1";
const PAGE_MAGIC: &[u8] = b"rocky.checkpoint.page.v2";

#[derive(Default)]
struct Writer(Vec<u8>);
impl Writer {
    fn bytes(&mut self, bytes: &[u8]) {
        self.0.extend(bytes);
    }
    fn u32(&mut self, value: u32) {
        self.0.extend(value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.0.extend(value.to_le_bytes());
    }
    fn blob(&mut self, bytes: &[u8]) {
        self.u64(bytes.len() as u64);
        self.bytes(bytes);
    }
    fn scope(&mut self, scope: Scope) {
        for id in [scope.mesh.0, scope.app.0, scope.domain.0] {
            self.bytes(&id);
        }
    }
    fn frontier(&mut self, frontier: &AppliedFrontier) {
        self.u32(frontier.0.len() as u32);
        for (node, sequence) in &frontier.0 {
            self.bytes(&node.0);
            self.u64(sequence.get());
        }
    }
    fn digest(&mut self, digest: &SetDigest) {
        self.bytes(&digest.sum);
        self.u64(digest.count);
    }
    fn commitments(&mut self, values: &[Commitment]) {
        self.u32(values.len() as u32);
        for value in values {
            self.bytes(value);
        }
    }
}
struct Reader<'a>(&'a [u8]);
fn malformed() -> Error {
    Error::Rejected("malformed checkpoint data".into())
}
impl Reader<'_> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (head, tail) = self.0.split_at_checked(N).ok_or_else(malformed)?;
        self.0 = tail;
        head.try_into().map_err(|_| malformed())
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn blob(&mut self) -> Result<Vec<u8>, Error> {
        let length = usize::try_from(self.u64()?).map_err(|_| malformed())?;
        let (head, tail) = self.0.split_at_checked(length).ok_or_else(malformed)?;
        self.0 = tail;
        Ok(head.to_vec())
    }
    fn magic(&mut self, magic: &[u8]) -> Result<(), Error> {
        let (head, tail) = self.0.split_at_checked(magic.len()).ok_or_else(malformed)?;
        self.0 = tail;
        if head == magic {
            Ok(())
        } else {
            Err(malformed())
        }
    }
    fn scope(&mut self) -> Result<Scope, Error> {
        Ok(Scope {
            mesh: rocky_core::MeshId(self.array()?),
            app: rocky_core::AppId(self.array()?),
            domain: rocky_core::DomainId(self.array()?),
        })
    }
    fn frontier(&mut self) -> Result<AppliedFrontier, Error> {
        let count = self.u32()?;
        // A runtime configures at most 64 authors.
        if count > 64 {
            return Err(malformed());
        }
        let mut frontier = Vec::new();
        for _ in 0..count {
            let node = NodeId(self.array()?);
            let sequence = AuthorSequence::new(self.u64()?).map_err(|_| malformed())?;
            if frontier.last().is_some_and(|(last, _)| *last >= node) {
                return Err(malformed());
            }
            frontier.push((node, sequence));
        }
        Ok(AppliedFrontier(frontier))
    }
    fn digest(&mut self) -> Result<SetDigest, Error> {
        Ok(SetDigest {
            sum: self.array()?,
            count: self.u64()?,
        })
    }
    fn commitments(&mut self) -> Result<Vec<Commitment>, Error> {
        let count = self.u32()?;
        if count > 64 {
            return Err(malformed());
        }
        (0..count).map(|_| self.array()).collect()
    }
    fn end(&self) -> Result<(), Error> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(malformed())
        }
    }
}
impl CheckpointRecord {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Writer::default();
        out.bytes(RECORD_MAGIC);
        out.scope(self.scope);
        out.bytes(&self.definition);
        for version in [
            self.versions.protocol,
            self.versions.schema,
            self.versions.operation,
            self.projection,
        ] {
            out.u32(version);
        }
        out.frontier(&self.frontier);
        out.u64(self.clock);
        out.digest(&self.history);
        out.digest(&self.state);
        out.bytes(&self.node.0);
        out.u64(self.policy_version);
        out.u64(self.key_epoch);
        out.u64(self.sequence);
        out.bytes(&self.previous);
        out.u64(self.handles.mutations);
        out.u64(self.handles.mutation_floor);
        out.commitments(&self.superseded_recovery);
        out.commitments(&self.sole_recovery);
        out.0
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut input = Reader(bytes);
        input.magic(RECORD_MAGIC)?;
        let record = Self {
            scope: input.scope()?,
            definition: input.array()?,
            versions: Versions {
                protocol: input.u32()?,
                schema: input.u32()?,
                operation: input.u32()?,
            },
            projection: input.u32()?,
            frontier: input.frontier()?,
            clock: input.u64()?,
            history: input.digest()?,
            state: input.digest()?,
            node: NodeId(input.array()?),
            policy_version: input.u64()?,
            key_epoch: input.u64()?,
            sequence: input.u64()?,
            previous: input.array()?,
            handles: Deduplication {
                mutations: input.u64()?,
                mutation_floor: input.u64()?,
            },
            superseded_recovery: input.commitments()?,
            sole_recovery: input.commitments()?,
        };
        input.end()?;
        Ok(record)
    }
    /// The checkpoint's identity.
    pub fn commitment(&self) -> Commitment {
        rocky_core::commitment("rocky.checkpoint.record.v1", &self.encode())
    }
}

/// The wire signal for a peer behind the pruned prefix: the sender has declared
/// the operations at or before `pruned_prefix` prunable and can no longer supply
/// them. The peer imports `checkpoint` and syncs operations after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrunedNotice {
    pub scope: Scope,
    pub node: NodeId,
    pub pruned_prefix: AppliedFrontier,
    pub checkpoint: Commitment,
    pub sequence: u64,
}
impl PrunedNotice {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Writer::default();
        out.bytes(NOTICE_MAGIC);
        out.scope(self.scope);
        out.bytes(&self.node.0);
        out.frontier(&self.pruned_prefix);
        out.bytes(&self.checkpoint);
        out.u64(self.sequence);
        out.0
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut input = Reader(bytes);
        input.magic(NOTICE_MAGIC)?;
        let notice = Self {
            scope: input.scope()?,
            node: NodeId(input.array()?),
            pruned_prefix: input.frontier()?,
            checkpoint: input.array()?,
            sequence: input.u64()?,
        };
        input.end()?;
        Ok(notice)
    }
}
/// What a peer gets when it asks for the operations it lacks.
#[derive(Clone, Debug)]
pub enum Catchup {
    Operations(TransferPage),
    BehindPruned(PrunedNotice),
}

/// A position in a checkpoint's rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckpointCursor {
    pub checkpoint: Commitment,
    pub table: u32,
    pub position: i64,
}
/// One bounded page of a checkpoint's rows. The first page carries the
/// record. `rows` are stored rows of the application's checkpoint table
/// number `table`; `start` is the position the page was read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointPage {
    pub start: CheckpointCursor,
    pub record: Option<Vec<u8>>,
    pub table: u32,
    pub rows: Vec<Vec<SqlValue>>,
    pub next: Option<CheckpointCursor>,
}
impl CheckpointPage {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Writer::default();
        let cursor = |out: &mut Writer, cursor: &CheckpointCursor| {
            out.bytes(&cursor.checkpoint);
            out.u32(cursor.table);
            out.u64(cursor.position as u64);
        };
        out.bytes(PAGE_MAGIC);
        cursor(&mut out, &self.start);
        out.0.push(u8::from(self.record.is_some()));
        if let Some(record) = &self.record {
            out.blob(record);
        }
        out.u32(self.table);
        out.u32(self.rows.len() as u32);
        for row in &self.rows {
            out.u32(row.len() as u32);
            for value in row {
                match value {
                    SqlValue::Null => out.0.push(0),
                    SqlValue::Integer(value) => {
                        out.0.push(1);
                        out.u64(*value as u64);
                    }
                    SqlValue::Text(value) => {
                        out.0.push(2);
                        out.blob(value.as_bytes());
                    }
                    SqlValue::Blob(value) => {
                        out.0.push(3);
                        out.blob(value);
                    }
                    SqlValue::Real(value) => {
                        out.0.push(4);
                        out.u64(value.bits());
                    }
                }
            }
        }
        out.0.push(u8::from(self.next.is_some()));
        if let Some(next) = &self.next {
            cursor(&mut out, next);
        }
        out.0
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut input = Reader(bytes);
        fn cursor(input: &mut Reader<'_>) -> Result<CheckpointCursor, Error> {
            Ok(CheckpointCursor {
                checkpoint: input.array()?,
                table: input.u32()?,
                position: input.u64()? as i64,
            })
        }
        fn flag(input: &mut Reader<'_>) -> Result<bool, Error> {
            match input.u8()? {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(malformed()),
            }
        }
        input.magic(PAGE_MAGIC)?;
        let start = cursor(&mut input)?;
        let record = if flag(&mut input)? {
            Some(input.blob()?)
        } else {
            None
        };
        let table = input.u32()?;
        let mut rows = Vec::new();
        for _ in 0..input.u32()? {
            let mut row = Vec::new();
            for _ in 0..input.u32()? {
                row.push(match input.u8()? {
                    0 => SqlValue::Null,
                    1 => SqlValue::Integer(input.u64()? as i64),
                    2 => SqlValue::Text(String::from_utf8(input.blob()?).map_err(|_| malformed())?),
                    3 => SqlValue::Blob(input.blob()?),
                    4 => SqlValue::Real(
                        rocky_core::Real::from_bits(input.u64()?).ok_or_else(malformed)?,
                    ),
                    _ => return Err(malformed()),
                });
            }
            rows.push(row);
        }
        let next = if flag(&mut input)? {
            Some(cursor(&mut input)?)
        } else {
            None
        };
        input.end()?;
        Ok(Self {
            start,
            record,
            table,
            rows,
            next,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointInfo {
    pub commitment: Commitment,
    pub record: CheckpointRecord,
}
/// Why no new checkpoint can be made now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointHold {
    /// The last checkpoint did not recompute to the stored projection.
    Unverifiable,
    /// A checkpoint import is in progress.
    Rebootstrap,
    /// An aborted import left no base to recompute from.
    NoBase,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionStatus {
    /// The latest verified checkpoint.
    pub checkpoint: Option<CheckpointInfo>,
    /// The frontier of a checkpoint being recomputed.
    pub pending: Option<AppliedFrontier>,
    pub hold: Option<CheckpointHold>,
    /// Operations at or before it are pruned or about to be.
    pub pruned_prefix: AppliedFrontier,
    /// Delivery targets declared behind the pruned prefix.
    pub behind: Vec<NodeId>,
    pub retained_operations: usize,
    pub retained_bytes: usize,
    pub policy: RetentionPolicy,
    /// What each delivery target last reported in a catch-up.
    pub peers: Vec<PeerRetention>,
}
/// A delivery target's last reported frontiers. Compare `applied` with the
/// local applied frontier for its lag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRetention {
    pub peer: NodeId,
    pub full_node: bool,
    pub behind: bool,
    pub held: AppliedFrontier,
    pub applied: AppliedFrontier,
}
/// What a checkpoint import did with the operations the checkpoint lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebootstrapReport {
    pub checkpoint: Commitment,
    /// Local operations after the checkpoint, applied again on top of it.
    pub reapplied: Vec<OperationId>,
    /// Local operations the application refused on top of it, with reasons.
    pub rejected: Vec<(OperationId, String)>,
    /// Retained operations still waiting for a dependency.
    pub pending: Vec<OperationId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditReport {
    pub checkpoint: Option<Commitment>,
    /// The projection was recomputed from the checkpoint and retained
    /// operations. False while a hold prevents it.
    pub recomputed: bool,
    pub rows: u64,
    pub retained_operations: usize,
}

pub(crate) enum Request {
    Status,
    /// One step of creating and recomputing a checkpoint.
    Checkpoint,
    /// One step of declaring the pruned prefix and deleting behind it.
    Prune,
    DeclareBehind(NodeId),
    Catchup(Option<NodeId>, Box<SyncState>, Option<OperationId>),
    Export(Option<CheckpointCursor>),
    Import(Box<CheckpointPage>),
    AbortImport,
    /// History, counters and ledgers; rows are compared off the writer.
    Audit,
    Rejected,
    Unsettled,
}
impl Request {
    pub(crate) fn writes(&self) -> bool {
        matches!(
            self,
            Self::Checkpoint
                | Self::Prune
                | Self::DeclareBehind(_)
                | Self::Catchup(..)
                | Self::Import(_)
                | Self::AbortImport
        )
    }
    /// A notice, an operation page and checkpoint rows reach a peer.
    pub(crate) fn exposes(&self) -> bool {
        matches!(self, Self::Catchup(..) | Self::Export(_))
    }
    pub(crate) fn weight(&self) -> usize {
        match self {
            Self::Catchup(_, state, _) => state
                .received
                .len()
                .saturating_add(state.held.0.len())
                .saturating_add(state.applied.0.len())
                .saturating_mul(24),
            Self::Import(page) => page.rows.iter().flatten().fold(0, |weight, value| {
                weight
                    + match value {
                        SqlValue::Text(text) => text.len(),
                        SqlValue::Blob(blob) => blob.len(),
                        _ => 8,
                    }
            }),
            _ => 0,
        }
    }
}
/// Whether a stepped request has more to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    More,
    Done,
}
pub(crate) enum Response {
    Unit,
    Status(Box<RetentionStatus>),
    Step(Step),
    Catchup(Catchup),
    Page(Box<CheckpointPage>),
    /// Set when the import finished and switched the projection.
    Imported(Option<Commitment>),
    Audit(usize),
    Rejected(Vec<(OperationId, String)>),
    /// Retained operations not applied: (identity, local, rejected).
    Unsettled(Vec<(OperationId, bool, bool)>),
}
/// Rows one audit pass compares off the writer thread.
#[derive(Clone)]
pub(crate) struct RowAudit {
    /// Physical name of the domain's `rocky_checkpoint` table.
    pub(crate) marker: String,
    /// Logical name with the physical live and checkpoint tables.
    pub(crate) tables: Vec<(String, String, String)>,
}

impl<A> Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    pub(crate) async fn retention_request(&self, request: Request) -> Result<Response, Error> {
        let (reply, result) = oneshot::channel();
        self.inner
            .commands
            .send(Command::Retention(Box::new(request), reply))
            .await?;
        result.await.map_err(|_| Error::Closed)?
    }
    async fn steps(&self, request: impl Fn() -> Request) -> Result<(), Error> {
        loop {
            match self.retention_request(request()).await? {
                Response::Step(Step::More) => tokio::task::yield_now().await,
                Response::Step(Step::Done) => return Ok(()),
                _ => return Err(Error::Corrupt("retention step".into())),
            }
        }
    }
    pub async fn retention_status(&self) -> Result<RetentionStatus, Error> {
        match self.retention_request(Request::Status).await? {
            Response::Status(status) => Ok(*status),
            _ => Err(Error::Corrupt("retention status".into())),
        }
    }
    /// Makes a checkpoint of everything applied now and recomputes it from
    /// the previous one in bounded steps, yielding between them. Fails with
    /// `Corrupt` when the recomputed rows differ from the stored projection.
    pub async fn checkpoint(&self) -> Result<CheckpointInfo, Error> {
        self.steps(|| Request::Checkpoint).await?;
        self.retention_status()
            .await?
            .checkpoint
            .ok_or_else(|| Error::Corrupt("checkpoint missing after its last step".into()))
    }
    /// Advances the pruned prefix as far as the checkpoint, the retained window
    /// and delivery acknowledgments allow, and deletes behind it in bounded
    /// steps. Returns the pruned prefix.
    pub async fn prune(&self) -> Result<AppliedFrontier, Error> {
        self.flush().await?;
        self.steps(|| Request::Prune).await?;
        Ok(self.retention_status().await?.pruned_prefix)
    }
    /// Stops waiting for `peer` to acknowledge history. It will be told to
    /// import a checkpoint once it asks for operations behind the pruned prefix.
    pub async fn declare_behind(&self, peer: NodeId) -> Result<(), Error> {
        self.retention_request(Request::DeclareBehind(peer))
            .await
            .map(|_| ())
    }
    /// The operations a peer lacks, given its `sync_state`, or the notice
    /// that it is behind the pruned_prefix. `peer` names a delivery target, whose
    /// applied frontier then counts as acknowledgment.
    pub async fn catch_up(&self, peer: Option<NodeId>, state: SyncState) -> Result<Catchup, Error> {
        self.catch_up_after(peer, state, None).await
    }
    /// One bounded page of a range pass: the operations the peer lacks after
    /// `after`, with `next` set while more remain. Start each pass without a
    /// cursor and with the peer's current state.
    pub async fn catch_up_after(
        &self,
        peer: Option<NodeId>,
        mut state: SyncState,
        after: Option<OperationId>,
    ) -> Result<Catchup, Error> {
        if state.received.len() > crate::replication::MAX_SYNC_EXCEPTIONS
            || state.held.0.len() > 64
            || state.applied.0.len() > 64
        {
            return Err(Error::Quota {
                resource: "sync state",
                limit: crate::replication::MAX_SYNC_EXCEPTIONS,
            });
        }
        // The applied frontier already covers what the peer holds below it.
        state.received.retain(|id| {
            !state
                .applied
                .0
                .iter()
                .any(|(node, applied)| *node == id.node && id.sequence <= *applied)
        });
        match self
            .retention_request(Request::Catchup(peer, Box::new(state), after))
            .await?
        {
            Response::Catchup(catchup) => Ok(catchup),
            _ => Err(Error::Corrupt("catch-up response".into())),
        }
    }
    /// One page of the latest checkpoint's rows. A checkpoint made
    /// meanwhile ends the pass with `Rejected`; start again without a cursor.
    pub async fn checkpoint_page(
        &self,
        after: Option<CheckpointCursor>,
    ) -> Result<CheckpointPage, Error> {
        match self.retention_request(Request::Export(after)).await? {
            Response::Page(page) => Ok(*page),
            _ => Err(Error::Corrupt("checkpoint page".into())),
        }
    }
    /// Imports one page of a peer's checkpoint. The last page verifies the
    /// rows against the record, replaces the projection and applies again
    /// the retained operations the checkpoint lacks; its report accounts for
    /// every local one. Until then the replica keeps working on its own
    /// state.
    pub async fn import_checkpoint(
        &self,
        page: CheckpointPage,
    ) -> Result<Option<RebootstrapReport>, Error> {
        let checkpoint = match self
            .retention_request(Request::Import(Box::new(page)))
            .await?
        {
            Response::Imported(Some(checkpoint)) => checkpoint,
            Response::Imported(None) => return Ok(None),
            _ => return Err(Error::Corrupt("checkpoint import".into())),
        };
        let unsettled = |response| match response {
            Response::Unsettled(operations) => Ok(operations),
            _ => Err(Error::Corrupt("unsettled operations".into())),
        };
        let before = unsettled(self.retention_request(Request::Unsettled).await?)?;
        match self.process_pending().await {
            Ok(()) | Err(Error::Rejected(_)) => {}
            Err(error) => return Err(error),
        }
        let after = unsettled(self.retention_request(Request::Unsettled).await?)?;
        let rejected = self.rejected_local_operations().await?;
        Ok(Some(RebootstrapReport {
            checkpoint,
            reapplied: before
                .iter()
                .filter(|(id, local, _)| *local && !after.iter().any(|(other, _, _)| other == id))
                .map(|(id, _, _)| *id)
                .collect(),
            rejected,
            pending: after
                .into_iter()
                .filter(|(_, _, rejected)| !rejected)
                .map(|(id, _, _)| id)
                .collect(),
        }))
    }
    /// Drops a partly imported checkpoint. The replica keeps its projection
    /// but has no base for checkpoints of its own until an import completes.
    pub async fn abort_rebootstrap(&self) -> Result<(), Error> {
        self.retention_request(Request::AbortImport)
            .await
            .map(|_| ())
    }
    /// Local operations the application refused when they were applied again
    /// after a checkpoint import, with its reasons.
    pub async fn rejected_local_operations(&self) -> Result<Vec<(OperationId, String)>, Error> {
        match self.retention_request(Request::Rejected).await? {
            Response::Rejected(operations) => Ok(operations),
            _ => Err(Error::Corrupt("rejected operations".into())),
        }
    }
    /// Full verification, in bounded memory: recomputes the projection from
    /// the checkpoint and retained operations, compares stored rows with
    /// their digests, and checks retained history, handles and outgoing work.
    pub async fn audit(&self) -> Result<AuditReport, Error> {
        let retained = match self.retention_request(Request::Audit).await? {
            Response::Audit(retained) => retained,
            _ => return Err(Error::Corrupt("audit response".into())),
        };
        let recomputed = match self.checkpoint().await {
            Ok(_) => true,
            Err(Error::Rejected(_)) => false,
            Err(error) => return Err(error),
        };
        let rows = self.inner.reads()?.audit_rows().await?;
        let status = self.retention_status().await?;
        Ok(AuditReport {
            checkpoint: status.checkpoint.map(|checkpoint| checkpoint.commitment),
            recomputed,
            rows,
            retained_operations: retained,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_core::fixtures;

    #[test]
    fn records_notices_and_pages_round_trip_and_reject_trailing_bytes() {
        let frontier = AppliedFrontier(vec![
            (fixtures::LAPTOP, AuthorSequence::new(7).unwrap()),
            (fixtures::PHONE, AuthorSequence::new(3).unwrap()),
        ]);
        let record = CheckpointRecord {
            scope: fixtures::SCOPE,
            definition: [1; 32],
            versions: Versions::V1,
            projection: 1,
            frontier: frontier.clone(),
            clock: 11,
            history: SetDigest {
                sum: [2; 32],
                count: 10,
            },
            state: SetDigest {
                sum: [3; 32],
                count: 4,
            },
            node: fixtures::LAPTOP,
            policy_version: 1,
            key_epoch: 0,
            sequence: 2,
            previous: [4; 32],
            handles: Deduplication {
                mutations: 7,
                mutation_floor: 1,
            },
            superseded_recovery: Vec::new(),
            sole_recovery: Vec::new(),
        };
        let bytes = record.encode();
        assert_eq!(CheckpointRecord::decode(&bytes).unwrap(), record);
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(CheckpointRecord::decode(&longer).is_err());
        assert!(CheckpointRecord::decode(&bytes[..bytes.len() - 1]).is_err());
        // One current format: the previous record, with its strong anchor, is refused.
        let mut previous = bytes.clone();
        previous[b"rocky.checkpoint.v".len()] = b'1';
        assert!(CheckpointRecord::decode(&previous).is_err());
        let notice = PrunedNotice {
            scope: fixtures::SCOPE,
            node: fixtures::LAPTOP,
            pruned_prefix: frontier,
            checkpoint: record.commitment(),
            sequence: 2,
        };
        assert_eq!(PrunedNotice::decode(&notice.encode()).unwrap(), notice);
        let mut unsupported = notice.encode();
        unsupported[b"rocky.pruned.v".len()] = b'2';
        assert!(PrunedNotice::decode(&unsupported).is_err());
        let start = CheckpointCursor {
            checkpoint: record.commitment(),
            table: 0,
            position: 0,
        };
        let page = CheckpointPage {
            start,
            record: Some(bytes),
            table: 3,
            rows: vec![vec![
                SqlValue::Null,
                SqlValue::Integer(-4),
                SqlValue::Text("ü".into()),
                SqlValue::Blob(vec![9]),
            ]],
            next: Some(CheckpointCursor {
                position: 12,
                ..start
            }),
        };
        assert_eq!(CheckpointPage::decode(&page.encode()).unwrap(), page);
    }
}
