mod admission;
mod apply;
mod author;
mod backup;
pub(super) mod bundle;
mod chains;
pub(crate) mod checkpoint;
mod constraints;
mod coordinated;
mod evict;
mod existence;
mod generation;
pub(super) mod group;
mod held;
mod holdings;
mod horizon;
mod identity;
mod protection;
pub(crate) mod publication;
pub(crate) use horizon::HorizonFinalization;
pub use horizon::{HorizonPending, HorizonProgress};
mod incoming;
mod keys;
pub(crate) mod migrate;
pub(crate) mod moves;
mod offload;
mod partition;
mod queries;
mod reads;
mod rebuild;
mod removal;
pub use removal::local_row_counts;
mod restore;
pub(super) mod roles;
mod session;
mod sql;
mod typed;
use super::store::{SCHEMA, TransactionStore};
use super::wire::{Chain, Extensions, Membership, ROW_FORMAT, RowPayload, author, decrypt};
use super::*;
use crate::{
    database::DatabaseOwner,
    scoped_connection::ScopedConnection,
    storage::{bindings, collect_rows, retryable, rows},
};
pub use admission::MAX_UNCONFIGURED_PEERS;
pub use bundle::{MAX_BUNDLE_BYTES, MAX_BUNDLE_FRAMES, MAX_BUNDLE_ROWS, ROWS_PER_FRAME};
pub use checkpoint::{
    AuditReport, CheckpointPage, CheckpointStatement, CheckpointStatus, CustodySummary,
    HorizonPage, LogPosition, MAX_CHECKPOINT_LOGS, PruneBlock, PruneReport, RebootstrapReport,
    RetentionLimits, RetentionPolicy, SetSummary, SignedCheckpoint,
};
pub use coordinated::{
    BaseReport, CommittedEntry, CoordinatedCommit, CoordinatedEntry, CoordinatedError,
    CoordinatedStatus, CutEvidence, CutProposal, CutRefusal, ENTRY_DEADLINE, EntryPeers,
    MAX_CATCH_UP_ENTRIES, MAX_PAGE_CUTS, PeerFuture, Slot, entry_hash,
};
pub use evict::EvictReport;
pub use existence::{
    EXISTENCE_FORMAT, ExistenceBaseAttachment, ExistenceBasePage, ExistenceEvent, ExistencePage,
    ExistenceRecord, MAX_EXISTENCE_PAGE,
};
pub use generation::{GenerationStatus, GenerationStep, MAX_GENERATIONS, Upgrade};
pub use held::{CurrentValue, HeldChange, HeldField, HeldKind, HeldRow, SignedVoid};
pub use migrate::{PLAN_ROWS, ProjectionPlan};
pub use offload::RemotePage;
use p2panda_core::validate_backlink;
use p2panda_store::operations::LocalOperationStore;
pub use publication::Publication;
pub use queries::Describe;
use rocky_core::{
    Commitment, ResourceScope, Right, RowApplication, RowChange, RowFields, RowSelection, RowWrite,
    SqlValue,
};
pub use roles::{
    FrontierLog, Holding, PeerLag, RangePage, ReplicaRole, ReplicaStatus, RoleConfig,
    ScopeFrontier, ScopeStatus,
};
use serde::{Deserialize, Serialize};
pub use session::{SecureSession, SecureTransaction, SessionStatus, UpdateMode};
pub use sql::{SecureCommit, SecureRows, SecureSql};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, watch};

/// Keys are supplied from external secret storage and are never persisted in protocol tables.
/// A scope may hold several generations: the highest seals new writes, the
/// others open what they sealed.
pub struct KeyMaterial {
    pub scope: ResourceScope,
    pub generation: rocky_core::KeyEpoch,
    pub key: [u8; 32],
}
impl KeyMaterial {
    pub fn new(scope: ResourceScope, generation: u64, key: [u8; 32]) -> Self {
        Self {
            scope,
            generation: rocky_core::KeyEpoch::new(generation, &key),
            key,
        }
    }
    /// Uses the group secret's identity for every derived table key.
    pub fn at(scope: ResourceScope, generation: rocky_core::KeyEpoch, key: [u8; 32]) -> Self {
        Self {
            scope,
            generation,
            key,
        }
    }
}
pub struct SecureConfig {
    pub publisher: PublicKey,
    pub manifest: SignedManifest,
    /// The revocations this device verified; the replica keeps the union of
    /// these and every set it learned.
    pub revocations: RevocationSet,
    pub node: PrivateKey,
    pub credentials: CredentialChain,
    pub keys: Vec<KeyMaterial>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveStatus {
    Custody,
    PendingDependencies,
    MissingKey,
    Applied,
    Duplicate,
    Rejected,
    /// At or before this replica's horizon: already part of its checkpoint.
    Pruned,
    /// An owner-signed cut abandoned its coordinated position: consumed
    /// without effect, its author log continues.
    Abandoned,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Companion {
    Present(RowFields),
    Unavailable,
    Deleted,
}
#[derive(Clone, Debug)]
pub struct SecureSnapshot {
    pub revision: u64,
    pub rows: BTreeMap<Commitment, RowFields>,
    pub complete: bool,
}
/// One page of a scope's live rows; `next` continues after the last row read.
#[derive(Clone, Debug)]
pub struct SecurePage {
    pub revision: u64,
    pub rows: BTreeMap<Commitment, RowFields>,
    pub complete: bool,
    pub next: Option<Commitment>,
}
/// Limits of one encrypted read.
pub const MAX_SNAPSHOT_ROWS: usize = 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;
/// Stored rows fetched per query while filling a page.
const PAGE_CHUNK: usize = 256;
#[derive(Clone, Debug)]
pub struct CiphertextPage {
    pub frames: Vec<EncryptedFrame>,
    pub next: Option<Hash>,
    /// Requested scopes with pruned history. A receiver compares
    /// `horizon_page` with what it holds before it relies on this pass.
    pub pruned: Vec<ResourceScope>,
    /// Void records consuming the positions before the frames.
    pub voids: Vec<SignedVoid>,
}
/// A stamp: which write of which author last set a field. Stored as a
/// tuple in every row record, so it carries no field names.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(from = "(u64, Author, u64)", into = "(u64, Author, u64)")]
pub(crate) struct Stamp {
    pub(crate) clock: u64,
    pub(crate) author: Author,
    pub(crate) sequence: u64,
}
impl From<(u64, Author, u64)> for Stamp {
    fn from((clock, author, sequence): (u64, Author, u64)) -> Self {
        Self {
            clock,
            author,
            sequence,
        }
    }
}
impl From<Stamp> for (u64, Author, u64) {
    fn from(stamp: Stamp) -> Self {
        (stamp.clock, stamp.author, stamp.sequence)
    }
}
/// An author's public key as its bytes, in the byte order keys sort by.
/// Decoding a row record does not decompress it; `key` does, to verify.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Author(#[serde(with = "serde_bytes")] [u8; 32]);
impl Author {
    pub(crate) fn key(&self) -> Result<PublicKey, Error> {
        PublicKey::from_bytes(&self.0).map_err(|_| Error::Corrupt("author key".into()))
    }
}
impl From<PublicKey> for Author {
    fn from(key: PublicKey) -> Self {
        Self(*key.as_bytes())
    }
}
impl PartialEq<PublicKey> for Author {
    fn eq(&self, key: &PublicKey) -> bool {
        self.0 == *key.as_bytes()
    }
}
impl std::fmt::Debug for Author {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
/// Decoded signed checkpoints by scope number and kind, with their bytes.
type DecodedCheckpoints = HashMap<(i64, bool), (Vec<u8>, SignedCheckpoint)>;
/// A stored row record. Identifiers and values are CBOR byte strings: the
/// record is stored, digested and transferred with every row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Materialized {
    proof: Option<moves::StoredProof>,
    #[serde(with = "serde_bytes")]
    moved_from: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protected: Option<protection::Protected>,
    #[serde(with = "byte_fields")]
    fields: RowFields,
    #[serde(with = "byte_keys")]
    stamps: BTreeMap<Commitment, Stamp>,
    deleted: bool,
    creation: Hash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "identity::decimal")]
    identity: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "serde_bytes")]
    key: Option<Commitment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<Stamp>,
}
/// A map keyed by commitments, its keys as byte strings.
mod byte_keys {
    use super::*;
    pub(super) fn serialize<S: serde::Serializer, V: Serialize>(
        map: &BTreeMap<Commitment, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_map(
            map.iter()
                .map(|(key, value)| (serde_bytes::Bytes::new(key), value)),
        )
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>, V: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<Commitment, V>, D::Error> {
        let map = BTreeMap::<serde_bytes::ByteArray<32>, V>::deserialize(deserializer)?;
        Ok(map
            .into_iter()
            .map(|(key, value)| (key.into_array(), value))
            .collect())
    }
}
/// Row fields, keys and values as byte strings.
mod byte_fields {
    use super::*;
    pub(super) fn serialize<S: serde::Serializer>(
        fields: &RowFields,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_map(
            fields
                .iter()
                .map(|(key, value)| (serde_bytes::Bytes::new(key), serde_bytes::Bytes::new(value))),
        )
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<RowFields, D::Error> {
        let map = BTreeMap::<serde_bytes::ByteArray<32>, serde_bytes::ByteBuf>::deserialize(
            deserializer,
        )?;
        Ok(map
            .into_iter()
            .map(|(key, value)| (key.into_array(), value.into_vec()))
            .collect())
    }
}
pub(super) struct Inner<A> {
    _owner: DatabaseOwner,
    pub(super) conn: Arc<ScopedConnection>,
    pub(super) app: Arc<A>,
    /// `config.keys` holds the current key of every scope.
    pub(super) config: SecureConfig,
    /// `config.credentials` with the digest this device's frames name.
    pub(super) chain: Chain,
    /// Stored chains decoded recently, by digest.
    chains: std::sync::Mutex<HashMap<Commitment, Chain>>,
    /// The last checkpoint statement decoded per scope number, newest or
    /// verified, with its bytes.
    decoded_checkpoints: std::sync::Mutex<DecodedCheckpoints>,
    node_author: Option<identity::NodeAuthor>,
    pub(crate) credential_signer: Option<PrivateKey>,
    partitions: BTreeMap<ResourceScope, rocky_core::PartitionTuple>,
    /// Earlier key epochs, kept to open retained ciphertext.
    pub(super) history: Vec<KeyMaterial>,
    pub(super) manifest: Commitment,
    pub(super) definition: rocky_core::ApplicationManifest,
    pub(super) tables: HashSet<Commitment>,
    pub(super) fields: HashSet<Commitment>,
    permission_fields: HashSet<Commitment>,
    domains: HashSet<rocky_core::DomainId>,
    pub(super) revision: u64,
    /// Grows whenever policy, credentials or keys change; sessions opened
    /// under an earlier value are refused.
    pub(super) authority: u64,
    pub(super) authority_events: watch::Sender<u64>,
    /// Installed typed tables that stored rows project into.
    typed: Option<Arc<typed::Typed>>,
    /// The shadow tables of a pending projection plan, which rows also
    /// project into.
    shadow: Option<Arc<typed::Typed>>,
    /// Earlier definition generations, all admitted.
    previous: Vec<generation::Previous>,
    /// Removed and re-added tables and fields of the recorded lineage.
    names: generation::Names,
    /// Set while a SQL session commits: a projection must find its rows.
    strict_projection: bool,
    /// Set while a staged publication authors its bundles: one atomic
    /// transaction of any size, so the retained-history bound cannot hold
    /// it back; later writes wait until it is delivered and pruned.
    publishing: bool,
    /// Constrained rows a change may affect, until the frame or commit settles.
    settling: std::sync::Mutex<constraints::Seeds>,
    /// This device's grants as last resolved, for the authority, partition
    /// count and second they were resolved at: resolving the chain against
    /// every registered partition otherwise repeats for each frame and scope.
    local: std::sync::Mutex<Option<LocalGrants>>,
    /// Scopes buffered SQL commits wrote, whose maintenance step `maintain`
    /// runs after the commit answered.
    deferred_maintenance: BTreeSet<ResourceScope>,
    /// Scopes the owner imports from a checkpoint: peers do not push their
    /// history, and maintenance waits until the import ends.
    importing: BTreeSet<ResourceScope>,
    /// Scopes whose import writes verified pages straight into their rows;
    /// operations for them wait until the import ends.
    direct: BTreeSet<ResourceScope>,
    /// This device's verified partition selectors for an authority and
    /// second: every statement of a partitioned schema asks for them.
    local_selectors: std::sync::Mutex<Option<LocalSelectors>>,
    /// Cursors, table events and the authority mirror of this replica.
    pub(super) queries: queries::Queries,
    pub(super) notifications: watch::Sender<u64>,
    pub(super) frames: watch::Sender<()>,
    /// Checkpoint pages stored since open, by any import.
    pub(super) imported: watch::Sender<u64>,
    wal: PathBuf,
    /// Commits with changes since open. Nothing leaves the device until the
    /// flusher's durable watermark covers it.
    pub(super) commit: u64,
    pub(super) flusher: Arc<Flusher>,
    /// `total_changes()` at the last commit, to skip commits that changed nothing.
    changes: i64,
    /// Passive and truncating checkpoint sizes.
    wal_limits: (u64, u64),
    /// WAL size at the last passive checkpoint.
    wal_mark: u64,
    pub(super) limits: RetentionLimits,
    /// The last failure of automatic checkpointing or pruning.
    maintenance_error: Option<String>,
    /// Commit that made each scope's checkpoint current, by scope number.
    verified_at: BTreeMap<i64, u64>,
    /// Peers whose credentials and frontiers are kept whatever the bound.
    configured: BTreeSet<PublicKey>,
    /// When each peer was last served since open, by a growing count; a
    /// peer not served since open counts as the least recent.
    served: (u64, BTreeMap<rocky_credentials::PeerId, u64>),
    /// Verified owner-signed cuts of each coordinated log, in order.
    cuts: BTreeMap<rocky_core::Scope, Vec<coordinated::cut::HeldCut>>,
    #[cfg(test)]
    stage_fault: Option<(checkpoint::Stage, usize)>,
    /// Fails after the cut rollback step with this number commits.
    #[cfg(test)]
    cut_fault: Option<i64>,
    #[cfg(test)]
    plan_fault: Option<(migrate::PlanStage, usize)>,
    /// The stage fault exits the process instead of returning an error.
    #[cfg(test)]
    stage_exit: bool,
    #[cfg(test)]
    pub(super) fail_before_commit: usize,
    #[cfg(test)]
    pub(super) fail_horizon_at: Option<u8>,
    #[cfg(test)]
    pub(super) retry_before_commit: usize,
    #[cfg(test)]
    pub(super) group_fault: Option<group::GroupFault>,
    #[cfg(test)]
    pub(super) group_commits: usize,
    #[cfg(test)]
    pub(super) checkpoints: usize,
    #[cfg(test)]
    pub(super) busy_hook: Option<Box<dyn FnMut(usize) + Send + Sync>>,
}
/// The durable watermark and the device flush that advances it. Shared, so a
/// group's flush runs after the replica lock is released and readers and the
/// next group do not wait behind it.
pub(super) struct Flusher {
    wal: PathBuf,
    /// The last commit known flushed.
    durable: AtomicU64,
    #[cfg(test)]
    path: PathBuf,
    #[cfg(test)]
    flushes: std::sync::atomic::AtomicUsize,
    /// Copies the file at every flush, to simulate losing later commits.
    #[cfg(test)]
    copy: std::sync::Mutex<Option<PathBuf>>,
}
impl Flusher {
    pub(super) fn durable(&self) -> u64 {
        self.durable.load(Ordering::Acquire)
    }
    /// Makes every commit up to `commit` durable with one full flush of the
    /// WAL; a no-op when the watermark already covers it. Commits run at
    /// `synchronous=NORMAL`, so this is the only flush they get. `commit`
    /// must have been read under the replica lock: whatever it counts is
    /// then in the WAL before the flush starts.
    pub(super) fn flush(&self, commit: u64) -> Result<(), Error> {
        if self.durable() >= commit {
            return Ok(());
        }
        // `sync_all` is `F_FULLFSYNC` on Apple platforms, like `fullfsync=1`.
        // Windows requires write access for FlushFileBuffers.
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.wal)
        {
            Ok(wal) => wal.sync_all().map_err(Error::Io)?,
            // Without a WAL every commit is in the database file, which
            // checkpoints flush.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::Io(error)),
        }
        self.durable.fetch_max(commit, Ordering::AcqRel);
        #[cfg(test)]
        {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            let directory = self
                .copy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(directory) = directory {
                for (from, name) in [(&self.path, "copy.db"), (&self.wal, "copy.db-wal")] {
                    let to = directory.join(name);
                    match std::fs::copy(from, &to) {
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            let _ = std::fs::remove_file(&to);
                        }
                        Err(error) => return Err(Error::Io(error)),
                    }
                }
            }
        }
        Ok(())
    }
}
/// A local commit that power loss may still remove. It is durable once
/// `EncryptedReplica::durable_commit` reaches `commit`; `flush` forces that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffered<T> {
    pub value: T,
    pub commit: u64,
}
/// Resolved grants, shared by the callers within one authority and second.
pub(super) type Grants = Arc<Vec<rocky_core::ResourceGrant>>;
/// Grants with the authority, partition count and second they hold for.
type LocalGrants = ((u64, usize, u64), Grants);
pub(super) type Selectors = Arc<Vec<rocky_credentials::PartitionGrant>>;
type LocalSelectors = ((u64, u64), Selectors);

/// Encrypted protocol files use an explicit API; trusted files cannot be opened here.
pub struct EncryptedReplica<A> {
    pub(super) inner: Arc<Mutex<Inner<A>>>,
    queue: Arc<group::Queue>,
    revisions: watch::Receiver<u64>,
    /// The coordinated log this trusted node acknowledges stored entries to.
    domain_log: Arc<coordinated::Trusted>,
}
impl<A> Clone for EncryptedReplica<A> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            queue: self.queue.clone(),
            revisions: self.revisions.clone(),
            domain_log: self.domain_log.clone(),
        }
    }
}
impl<A> EncryptedReplica<A> {
    /// Makes this replica a trusted node of a coordinated log: peers may
    /// store entries on it, which it acknowledges through `log`.
    pub fn set_domain_log(&self, log: Option<Arc<dyn rocky_core::DomainLog>>) {
        *self
            .domain_log
            .log
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = log;
    }
    pub fn domain_log(&self) -> Option<Arc<dyn rocky_core::DomainLog>> {
        self.domain_log
            .log
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// Changes whenever this replica stores another proposer's entry, which
    /// then waits for `resolve_coordinated`.
    pub fn observe_entries(&self) -> watch::Receiver<u64> {
        self.domain_log.stored.subscribe()
    }
}
fn protocol_schema() -> impl Iterator<Item = &'static str> {
    [
        generation::SCHEMA,
        migrate::SCHEMA,
        backup::SCHEMA,
        rebuild::SCHEMA,
        removal::SCHEMA,
        held::SCHEMA,
        coordinated::SCHEMA,
        coordinated::cut::SCHEMA,
        coordinated::proposal::SCHEMA,
        bundle::SCHEMA,
        typed::SCHEMA,
        identity::SCHEMA,
        moves::SCHEMA,
        partition::SCHEMA,
        checkpoint::SCHEMA,
        checkpoint::ARCHIVE_SCHEMA,
        roles::SCHEMA,
        keys::SCHEMA,
        existence::SCHEMA,
        horizon::SCHEMA,
        constraints::SCHEMA,
        chains::SCHEMA,
        publication::SCHEMA,
    ]
    .into_iter()
    .flat_map(|schema| schema.iter().copied())
}

/// Refuse another persisted format before opening a writer or changing SDK settings.
pub async fn check_replica_format(path: &Path) -> Result<(), Error> {
    rocky_storage::database::check_file_format(path).await?;
    if !std::fs::metadata(path).is_ok_and(|file| file.len() > 0) {
        return Ok(());
    }
    let raw = crate::sqlite::Connection::open_read_only(path)?;
    let conn = ScopedConnection::new(raw, None, SCHEMA);
    let tables = rows(
        &conn,
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .await?;
    if tables.is_empty() {
        return Ok(());
    }
    if !tables.contains(&vec![SqlValue::Text("secure_metadata".into())]) {
        return Err(Error::Incompatible(
            "unsupported encrypted replica format".into(),
        ));
    }
    let records = rows(&conn, "SELECT identity FROM secure_metadata WHERE id=1").await?;
    let Some([SqlValue::Blob(bytes)]) = records.first().map(Vec::as_slice) else {
        return Err(Error::Incompatible(
            "unsupported encrypted replica format".into(),
        ));
    };
    let (label, _, _): (String, Commitment, PublicKey) = decode(bytes)?;
    if label != generation::FILE_IDENTITY {
        return Err(Error::Incompatible(
            "unsupported encrypted replica format".into(),
        ));
    }
    if tables.contains(&vec![SqlValue::Text("secure_typed".into())]) {
        for record in rows(&conn, "SELECT descriptor FROM secure_typed").await? {
            if let [SqlValue::Blob(bytes)] = record.as_slice() {
                rocky_core::LogicalSchema::check_stored_row_identity(bytes)
                    .map_err(|error| Error::Incompatible(error.to_string()))?;
            }
        }
    }
    if tables.contains(&vec![SqlValue::Text("secure_checkpoints".into())]) {
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        #[derive(Deserialize)]
        struct Version {
            statement: Format,
        }
        for record in rows(
            &conn,
            "SELECT signed FROM secure_checkpoints WHERE signed IS NOT NULL",
        )
        .await?
        {
            if let [SqlValue::Blob(bytes)] = record.as_slice() {
                let version: Version = p2panda_core::cbor::decode_cbor(bytes.as_slice())
                    .map_err(|_| denied("invalid checkpoint"))?;
                if version.statement.format != checkpoint::CHECKPOINT_FORMAT {
                    return Err(denied("unsupported checkpoint format"));
                }
            }
        }
    }
    let schema: Vec<_> = SCHEMA.iter().copied().chain(protocol_schema()).collect();
    for (table, _) in rocky_storage::scoped_connection::tables(&schema) {
        if !tables.contains(&vec![SqlValue::Text(table.clone())]) {
            return Err(Error::Corrupt(format!(
                "missing secure storage table {table}"
            )));
        }
    }
    Ok(())
}

/// Checks a configuration against its verified manifest: the revocations,
/// this node's credentials for that manifest, and keys it may read whole.
pub(super) fn check_config(
    config: &SecureConfig,
    manifest: &rocky_core::ApplicationManifest,
) -> Result<(), Error> {
    config
        .revocations
        .verify(config.publisher, manifest.app)
        .map_err(credential)?;
    let (grants, selectors) = config
        .credentials
        .verify_selectors(
            config.publisher,
            manifest.hash(),
            config.node.public_key(),
            &config.revocations,
            now()?,
        )
        .map_err(credential)?;
    let mut seen_scopes = BTreeSet::new();
    let mut seen_keys = BTreeSet::new();
    let mut counts = BTreeMap::<ResourceScope, usize>::new();
    for key in &config.keys {
        let count = counts.entry(key.scope).or_default();
        *count += 1;
        if *count > 256 {
            return Err(denied("key epoch capacity reached"));
        }
        if !key.generation.valid()
            || !seen_scopes.insert((key.scope, key.generation))
            || !seen_keys.insert(key.key)
            || (!grants.iter().any(|grant| {
                grant.scope == key.scope
                    && grant.rows == RowSelection::All
                    && grant.rights.contains(&Right::Read)
            }) && !selectors.iter().any(|grant| {
                grant.domain == key.scope.domain
                    && grant.table == key.scope.table
                    && grant.rows == RowSelection::All
                    && grant.rights.contains(&Right::Read)
            }))
        {
            return Err(denied(
                "encryption key does not have whole-scope read authority",
            ));
        }
    }
    Ok(())
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn open(path: impl AsRef<Path>, app: A, config: SecureConfig) -> Result<Self, Error> {
        Self::open_upgrading(path, app, config, None).await
    }
    /// Opens a file installed with an earlier generation of the configured
    /// definition, activating `upgrade` atomically first, or a file already
    /// at it. A pending projection plan finishes before this returns. See
    /// `docs/runtime/generations.md`.
    pub async fn open_upgrading(
        path: impl AsRef<Path>,
        app: A,
        mut config: SecureConfig,
        upgrade: Option<Upgrade>,
    ) -> Result<Self, Error> {
        let manifest = config
            .manifest
            .verify(config.publisher)
            .map_err(credential)?;
        if app.manifest().hash() != manifest.hash() {
            return Err(denied("compiled application differs from signed manifest"));
        }
        check_config(&config, &manifest)?;
        let (current, history) = keys::split(std::mem::take(&mut config.keys));
        config.keys = current;
        check_replica_format(path.as_ref()).await?;
        let (owner, raw) = DatabaseOwner::open(path.as_ref()).await?;
        let conn = Arc::new(ScopedConnection::new(raw, None, SCHEMA));
        configure(&conn).await?;
        let identity = generation::identity(manifest.hash(), config.node.public_key())?;
        let mut wal = path.as_ref().as_os_str().to_owned();
        wal.push("-wal");
        let (notifications, _) = watch::channel(0);
        let wal = PathBuf::from(wal);
        let flusher = Arc::new(Flusher {
            wal: wal.clone(),
            durable: AtomicU64::new(0),
            #[cfg(test)]
            path: path.as_ref().to_path_buf(),
            #[cfg(test)]
            flushes: Default::default(),
            #[cfg(test)]
            copy: Default::default(),
        });
        let mut inner = Inner {
            _owner: owner,
            conn,
            permission_fields: app.permission_fields().into_iter().collect(),
            app: Arc::new(app),
            chain: Chain::new(config.credentials.clone())?,
            chains: Default::default(),
            decoded_checkpoints: Default::default(),
            node_author: Some(identity::NodeAuthor::new(&config)?),
            config,
            credential_signer: None,
            partitions: BTreeMap::new(),
            history,
            manifest: manifest.hash(),
            tables: manifest.entities.iter().map(|item| item.id).collect(),
            fields: manifest.fields.iter().map(|item| item.id).collect(),
            domains: manifest.domains.iter().map(|item| item.domain).collect(),
            definition: manifest,
            revision: 0,
            authority: 1,
            authority_events: watch::channel(1).0,
            typed: None,
            shadow: None,
            previous: Vec::new(),
            names: generation::Names::default(),
            strict_projection: false,
            publishing: false,
            settling: Default::default(),
            local: Default::default(),
            local_selectors: Default::default(),
            deferred_maintenance: BTreeSet::new(),
            importing: BTreeSet::new(),
            direct: BTreeSet::new(),
            queries: Default::default(),
            notifications,
            frames: watch::channel(()).0,
            imported: watch::channel(0).0,
            wal,
            // A reopened file may hold commits a crashed process never flushed.
            commit: 1,
            flusher,
            changes: 0,
            wal_limits: (group::WAL_CHECKPOINT_BYTES, group::WAL_LIMIT_BYTES),
            wal_mark: 0,
            limits: RetentionLimits::default(),
            maintenance_error: None,
            verified_at: BTreeMap::new(),
            configured: BTreeSet::new(),
            served: (0, BTreeMap::new()),
            cuts: BTreeMap::new(),
            #[cfg(test)]
            stage_fault: None,
            #[cfg(test)]
            cut_fault: None,
            #[cfg(test)]
            plan_fault: None,
            #[cfg(test)]
            stage_exit: false,
            #[cfg(test)]
            fail_before_commit: 0,
            #[cfg(test)]
            fail_horizon_at: None,
            #[cfg(test)]
            retry_before_commit: 0,
            #[cfg(test)]
            group_fault: None,
            #[cfg(test)]
            group_commits: 0,
            #[cfg(test)]
            checkpoints: 0,
            #[cfg(test)]
            busy_hook: None,
        };
        // Schema, metadata and later additive tables install in one transaction.
        transaction!(&mut inner, |inner| {
            let tables = rows(
                &inner.conn,
                "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .await?;
            if tables.is_empty() {
                for sql in SCHEMA {
                    inner.conn.execute(sql, ()).await?;
                }
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_metadata VALUES(1,?,?,0)",
                        [
                            identity.clone(),
                            inner.config.revocations.to_bytes().map_err(credential)?,
                        ],
                    )
                    .await?;
            }
            if tables.is_empty() {
                for sql in protocol_schema() {
                    inner.conn.execute(sql, ()).await?;
                }
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_retained VALUES(1,0,0,0,?)",
                        [checkpoint::Lt::new().to_bytes()],
                    )
                    .await?;
            }
            if tables.is_empty() {
                inner.seed(upgrade.as_ref()).await?;
            } else {
                inner.load_generations().await?;
                let stored = inner.stored_identity().await?;
                let activated = inner.activate(&stored, upgrade.as_ref()).await?;
                inner
                    .load_revocations((!activated).then_some(identity.as_slice()))
                    .await?;
            }
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET identity=? WHERE id=1",
                    [identity.clone()],
                )
                .await?;
            inner.bind_node_author(&inner.config.credentials).await?;
            Ok(())
        })?;
        inner.load_generations().await?;
        inner.finish_plan().await?;
        inner.load_typed().await?;
        inner.load_partitions().await?;
        inner.revision = inner.stored_revision().await?;
        inner.notifications.send_replace(inner.revision);
        let revisions = inner.notifications.subscribe();
        inner.local_grants(now()?)?;
        inner.validate_keys(now()?)?;
        inner.adopt_keys().await?;
        inner.load_direct().await?;
        inner.load_cuts().await?;
        // A rollback a crash interrupted resumes at its durable cursor,
        // before the projection is checked against history.
        while inner.cut_step().await? {}
        inner.pull_again().await?;
        inner.verify_open().await?;
        let replica = Self {
            inner: Arc::new(Mutex::new(inner)),
            queue: Arc::default(),
            revisions,
            domain_log: Arc::default(),
        };
        replica.settle().await?;
        Ok(replica)
    }
    /// Wakes after a frame group commits, including keyless custody receipt.
    /// Duplicate incoming frames do not wake it; exports still require a flush.
    pub async fn observe_frames(&self) -> watch::Receiver<()> {
        self.inner.lock().await.frames.subscribe()
    }
    /// Ends authority-bound network links without polling.
    pub async fn observe_authority(&self) -> watch::Receiver<u64> {
        self.inner.lock().await.authority_events.subscribe()
    }
    pub fn observe_revision(&self) -> watch::Receiver<u64> {
        self.revisions.clone()
    }
    /// Changes after every stored checkpoint page, so import progress can
    /// be shown as it moves.
    pub async fn observe_imports(&self) -> watch::Receiver<u64> {
        self.inner.lock().await.imported.subscribe()
    }
    /// Concurrent calls share one transaction and one flush; each request
    /// keeps its own savepoint, admission checks and result. The frame may be
    /// sent anywhere, so it is returned only after it is flushed.
    pub async fn author(
        &self,
        handle: Commitment,
        changes: Vec<RowChange>,
        dependencies: Vec<Hash>,
    ) -> Result<EncryptedFrame, Error> {
        match self
            .author_with(handle, changes, dependencies, true)
            .await?
        {
            (group::Done::Author(frame), _) => Ok(frame),
            // The handle did commit once; its frame is no longer retained.
            (group::Done::Pruned(_), _) => Err(denied(
                "mutation handle was committed before the pruning horizon",
            )),
            _ => Err(denied("missing authoring result")),
        }
    }
    /// Authors without waiting for a flush. Only the operation id is returned:
    /// the frame leaves the device through `author` with the same handle or
    /// a transfer page, both of which flush first.
    pub async fn author_buffered(
        &self,
        handle: Commitment,
        changes: Vec<RowChange>,
        dependencies: Vec<Hash>,
    ) -> Result<Buffered<Hash>, Error> {
        match self
            .author_with(handle, changes, dependencies, false)
            .await?
        {
            (group::Done::Author(frame), commit) => Ok(Buffered {
                value: frame.id()?,
                commit,
            }),
            (group::Done::Pruned(value), commit) => Ok(Buffered { value, commit }),
            _ => Err(denied("missing authoring result")),
        }
    }
    async fn author_with(
        &self,
        handle: Commitment,
        changes: Vec<RowChange>,
        dependencies: Vec<Hash>,
        exposed: bool,
    ) -> Result<(group::Done, u64), Error> {
        let encoded = encode(&(&changes, &dependencies))?;
        let input = rocky_core::commitment("rocky.secure-mutation.v1", &encoded);
        let work = group::Work::Author {
            handle,
            input,
            changes,
            dependencies,
        };
        self.submit(vec![(work, encoded.len())], exposed)
            .await
            .pop()
            .unwrap_or_else(|| Err(denied("missing authoring result")))
    }
    /// The status may be acknowledged to the sender, so it is returned only
    /// after the receipt is flushed.
    pub async fn receive(&self, frame: EncryptedFrame) -> Result<ReceiveStatus, Error> {
        self.receive_all(vec![frame])
            .await
            .pop()
            .unwrap_or_else(|| Err(denied("missing receive result")))
    }
    /// Receives frames that arrived together, such as one transfer page, in
    /// shared transactions and one flush. Results are in input order; each
    /// frame is admitted or refused on its own.
    pub async fn receive_all(
        &self,
        frames: Vec<EncryptedFrame>,
    ) -> Vec<Result<ReceiveStatus, Error>> {
        self.receive_with(frames, true)
            .await
            .into_iter()
            .map(|result| result.map(|status| status.value))
            .collect()
    }
    /// Like `receive_all` without the flush. Do not acknowledge these frames
    /// to their sender before `flush` or `durable_commit` covers `commit`.
    pub async fn receive_buffered(
        &self,
        frames: Vec<EncryptedFrame>,
    ) -> Vec<Result<Buffered<ReceiveStatus>, Error>> {
        self.receive_with(frames, false).await
    }
    async fn receive_with(
        &self,
        frames: Vec<EncryptedFrame>,
        exposed: bool,
    ) -> Vec<Result<Buffered<ReceiveStatus>, Error>> {
        let mut work = Vec::new();
        let mut positions = Vec::new();
        let mut results: Vec<Option<Result<Buffered<ReceiveStatus>, Error>>> =
            (0..frames.len()).map(|_| None).collect();
        for (index, frame) in frames.into_iter().enumerate() {
            if let Err(error) = self.discover_partition(&frame).await {
                results[index] = Some(Err(error));
                continue;
            }
            let weight = frame.weight();
            positions.push(index);
            work.push((group::Work::Receive(frame), weight));
        }
        for (index, result) in positions.into_iter().zip(self.submit(work, exposed).await) {
            results[index] = Some(match result {
                Ok((group::Done::Receive(value), commit)) => Ok(Buffered { value, commit }),
                Ok(_) => Err(denied("missing receive result")),
                Err(error) => Err(error),
            });
        }
        results
            .into_iter()
            .map(|result| result.unwrap_or_else(|| Err(denied("missing receive result"))))
            .collect()
    }
    /// Flushes every commit so far with one device flush; returns the
    /// durable watermark. The flush runs on a blocking thread without the
    /// replica, so statements and reads proceed meanwhile.
    pub async fn flush(&self) -> Result<u64, Error> {
        let (flusher, commit) = {
            let inner = self.inner.lock().await;
            (inner.flusher.clone(), inner.commit)
        };
        if flusher.durable() < commit {
            let flushing = flusher.clone();
            tokio::task::spawn_blocking(move || flushing.flush(commit))
                .await
                .map_err(|_| Error::Closed)??;
        }
        Ok(flusher.durable())
    }
    /// Runs the maintenance buffered SQL commits left for later: one bounded
    /// checkpoint or pruning step for the scopes they wrote, and a WAL
    /// checkpoint when one is due. A commit acknowledged `Flushed` runs its
    /// own step before it answers.
    pub async fn maintain(&self) {
        let mut inner = self.inner.lock().await;
        let scopes = std::mem::take(&mut inner.deferred_maintenance);
        if !scopes.is_empty() {
            inner.maintain(&scopes).await;
        }
        inner.checkpoint_if_due().await;
    }
    /// Names the scopes the owner is about to import from a checkpoint,
    /// replacing the previous set. A scope with a staged import counts too.
    pub async fn set_importing(&self, scopes: BTreeSet<ResourceScope>) {
        self.inner.lock().await.importing = scopes;
    }
    /// The last commit known to survive power loss.
    pub async fn durable_commit(&self) -> u64 {
        self.inner.lock().await.flusher.durable()
    }
    pub async fn install_key(&self, key: KeyMaterial) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        if inner
            .config
            .keys
            .iter()
            .any(|existing| existing.scope == key.scope)
        {
            return Err(denied(
                "key already installed; rotate with install_key_epoch",
            ));
        }
        let retained = collect_rows(
            inner
                .conn
                .query(
                    "SELECT generation,key_hash FROM secure_keys WHERE scope=?",
                    [encode(&key.scope)?],
                )
                .await?,
        )
        .await?;
        if let Some(record) = retained.first()
            && record
                != &vec![
                    SqlValue::Blob(encode(&key.generation)?),
                    SqlValue::Blob(rocky_core::commitment("rocky.scope-key.v1", &key.key).to_vec()),
                ]
        {
            return Err(denied("key differs from retained crypto state"));
        }
        inner.config.keys.push(key);
        if let Err(error) = inner.validate_keys(now()?) {
            inner.config.keys.pop();
            return Err(error);
        }
        drop(inner);
        self.settle().await
    }
    pub async fn evict_key(&self, scope: ResourceScope) {
        let mut inner = self.inner.lock().await;
        inner.config.keys.retain(|key| key.scope != scope);
        inner.history.retain(|key| key.scope != scope);
        inner.bump_authority();
    }
    /// Whether a mutation handle committed, whatever its writes were. A
    /// caller that lost a commit's answer checks this before planning the
    /// writes again. Handles are retained as `RetentionLimits` describes.
    /// A handle whose write an owner-signed cut abandoned reports that.
    pub async fn handle_committed(&self, handle: Commitment) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        let mut result = inner
            .conn
            .query(
                "SELECT hash FROM secure_handles WHERE handle=?",
                [handle.to_vec()],
            )
            .await?;
        let Some(row) = result.next().await? else {
            return Ok(false);
        };
        let hash = Hash::try_from(row.get::<Vec<u8>>(0)?.as_slice())
            .map_err(|_| denied("invalid handle hash"))?;
        drop(result);
        inner.refuse_abandoned(hash).await?;
        Ok(true)
    }
    /// Flushes first: the status may be acknowledged to the sender.
    pub async fn receipt_status(&self, id: Hash) -> Result<ReceiveStatus, Error> {
        let mut inner = self.inner.lock().await;
        inner.flush()?;
        inner.status(id).await
    }
    /// Merges `revocations` into the set this replica holds: the union of
    /// the statements that verify, never a refusal for order or overlap. A
    /// statement that fails verification refuses the whole set and changes
    /// nothing. Returns the statements it added.
    pub async fn learn_revocations(
        &self,
        revocations: &RevocationSet,
    ) -> Result<Vec<SignedRevocation>, Error> {
        let mut inner = self.inner.lock().await;
        let mut merged = inner.config.revocations.clone();
        let added = merged
            .merge(
                revocations,
                inner.config.publisher,
                inner.definition.app,
                now()?,
            )
            .map_err(credential)?;
        if merged == inner.config.revocations {
            return Ok(added);
        }
        let bytes = merged.to_bytes().map_err(credential)?;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revocations=?,revision=revision+1 WHERE id=1",
                    [bytes.clone()],
                )
                .await?;
            Ok(())
        })?;
        inner.config.revocations = merged;
        inner.bump_authority();
        inner.publish_revision().await?;
        drop(inner);
        self.settle().await?;
        Ok(added)
    }
    /// Merges the statements a session peer sent, except one that revokes
    /// `own`, this device's chain: a replica never holds the statement that
    /// revokes itself, which its owner records instead. Returns that
    /// statement, verified, if one came.
    pub(crate) async fn learn_session_revocations(
        &self,
        statements: Vec<SignedRevocation>,
        own: &CredentialChain,
    ) -> Result<Option<SignedRevocation>, Error> {
        let (publisher, app) = {
            let inner = self.inner.lock().await;
            (inner.config.publisher, inner.definition.app)
        };
        let mut revoking = None;
        let mut others = Vec::new();
        for signed in statements {
            signed.verify(publisher, app).map_err(credential)?;
            let single = RevocationSet::try_from(vec![signed.clone()]).map_err(credential)?;
            if single.revoking(own).map_err(credential)?.is_some() {
                revoking.get_or_insert(signed);
            } else {
                others.push(signed);
            }
        }
        if !others.is_empty() {
            let set = RevocationSet::try_from(others).map_err(credential)?;
            self.learn_revocations(&set).await?;
        }
        Ok(revoking)
    }
    /// The revocations this replica holds.
    pub async fn revocations(&self) -> RevocationSet {
        self.inner.lock().await.config.revocations.clone()
    }
    /// The digest of the live revocations, which a session hello carries.
    pub async fn revocation_digest(&self) -> Result<Commitment, Error> {
        self.inner.lock().await.revocation_digest()
    }
    #[cfg(test)]
    pub(crate) async fn applied_ack_count(&self) -> usize {
        rows(
            &self.inner.lock().await.conn,
            "SELECT hash FROM secure_ack WHERE applied=1",
        )
        .await
        .unwrap()
        .len()
    }
    #[cfg(test)]
    pub(crate) async fn inject_failure(&self, retries: usize, failures: usize) {
        let mut inner = self.inner.lock().await;
        inner.retry_before_commit = retries;
        inner.fail_before_commit = failures;
    }
    #[cfg(test)]
    pub(crate) async fn inject_group_fault(&self, fault: Option<group::GroupFault>) {
        self.inner.lock().await.group_fault = fault;
    }
    #[cfg(test)]
    pub(crate) async fn group_commits(&self) -> usize {
        self.inner.lock().await.group_commits
    }
    #[cfg(test)]
    pub(crate) async fn checkpoints(&self) -> usize {
        self.inner.lock().await.checkpoints
    }
    #[cfg(test)]
    pub(crate) async fn set_wal_limits(&self, checkpoint: u64, limit: u64) {
        self.inner.lock().await.wal_limits = (checkpoint, limit);
    }
    #[cfg(test)]
    pub(crate) async fn set_busy_hook(&self, hook: Option<Box<dyn FnMut(usize) + Send + Sync>>) {
        self.inner.lock().await.busy_hook = hook;
    }
    #[cfg(test)]
    pub(crate) async fn watermark(&self) -> (u64, u64, usize) {
        let inner = self.inner.lock().await;
        (
            inner.commit,
            inner.flusher.durable(),
            inner.flusher.flushes.load(Ordering::Relaxed),
        )
    }
    #[cfg(test)]
    pub(crate) async fn copy_at_flush(&self, directory: Option<PathBuf>) {
        *self
            .inner
            .lock()
            .await
            .flusher
            .copy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = directory;
    }
    #[cfg(test)]
    pub(crate) async fn stored_and_memory_revision(&self) -> (u64, u64) {
        let inner = self.inner.lock().await;
        (inner.stored_revision().await.unwrap(), inner.revision)
    }
}
impl<A: RowApplication> Inner<A> {
    /// The digest of the live revocations this replica holds.
    pub(crate) fn revocation_digest(&self) -> Result<Commitment, Error> {
        self.config.revocations.digest(now()?).map_err(credential)
    }
    pub(super) fn local_grants(&self, time: u64) -> Result<Grants, Error> {
        let key = (self.authority, self.partitions.len(), time);
        let mut local = self
            .local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((resolved, grants)) = local.as_ref()
            && *resolved == key
        {
            return Ok(grants.clone());
        }
        let grants = Arc::new(self.resolve_chain(
            &self.config.credentials,
            self.config.node.public_key(),
            &self.config.revocations,
            time,
        )?);
        *local = Some((key, grants.clone()));
        Ok(grants)
    }
    /// This device's partition selectors under its current credentials,
    /// verified once per authority and second like `local_grants`.
    pub(super) fn local_selectors(&self, time: u64) -> Result<Selectors, Error> {
        let key = (self.authority, time);
        let mut local = self
            .local_selectors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((resolved, selectors)) = local.as_ref()
            && *resolved == key
        {
            return Ok(selectors.clone());
        }
        let (_, selectors) = self
            .config
            .credentials
            .verify_selectors(
                self.config.publisher,
                self.manifest,
                self.config.node.public_key(),
                &self.config.revocations,
                time,
            )
            .map_err(credential)?;
        let selectors = Arc::new(selectors);
        *local = Some((key, selectors.clone()));
        Ok(selectors)
    }
    pub(super) fn can_read_key(
        &self,
        scope: ResourceScope,
        grants: &[rocky_core::ResourceGrant],
    ) -> bool {
        grants.iter().any(|grant| {
            grant.scope == scope
                && grant.rows == RowSelection::All
                && grant.rights.contains(&Right::Read)
        })
    }
    fn validate_keys(&self, time: u64) -> Result<(), Error> {
        // `can_read_key` for every key at once.
        let readable: BTreeSet<_> = self
            .local_grants(time)?
            .iter()
            .filter(|grant| grant.rows == RowSelection::All && grant.rights.contains(&Right::Read))
            .map(|grant| grant.scope)
            .collect();
        let mut scopes = BTreeSet::new();
        let mut epochs = BTreeSet::new();
        let mut secrets = BTreeSet::new();
        let mut counts = BTreeMap::<ResourceScope, usize>::new();
        for key in self.opening_keys() {
            let current = self
                .config
                .keys
                .iter()
                .find(|current| current.scope == key.scope)
                .map(|current| current.generation);
            let count = counts.entry(key.scope).or_default();
            *count += 1;
            if *count > 256 {
                return Err(denied("key epoch capacity reached"));
            }
            if !key.generation.valid()
                || !epochs.insert((key.scope, key.generation))
                || (current == Some(key.generation) && !scopes.insert(key.scope))
                || current.is_none_or(|current| current < key.generation)
                || !secrets.insert(key.key)
                || !readable.contains(&key.scope)
            {
                return Err(denied(
                    "key requires read permission for every row in its encryption scope",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn verify_frame(
        &self,
        frame: &EncryptedFrame,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        let operation = frame.operation()?;
        let extension = &operation.header.extensions;
        // Only bytes are shared by digest: the chain is verified here, for
        // every frame, against the current revocations.
        let credentials = frame.chain()?.credentials();
        let manifest = &self.definition;
        self.admits(extension.manifest)?;
        if extension.scope.domain.app != manifest.app
            || !self.domains.contains(&extension.scope.domain.domain)
            || !self.tables.contains(&extension.scope.table)
            || credentials
                .0
                .first()
                .is_some_and(|root| root.capability.manifest != extension.manifest)
        {
            return Err(denied("wrong app/domain/table/manifest"));
        }
        self.check_authenticated_partition(extension.scope, extension.partition.as_ref())?;
        let number = extension
            .author
            .ok_or_else(|| denied("node author number missing"))?;
        let grants = match &extension.partition {
            Some(tuple) => credentials
                .verify_partition_author(
                    self.config.publisher,
                    extension.manifest,
                    operation.header.public_key,
                    number,
                    extension.scope,
                    tuple,
                    self.revocations_for(extension.manifest, &self.config.revocations)?,
                    time,
                )
                .map_err(credential)?,
            None => credentials
                .verify_author(
                    self.config.publisher,
                    extension.manifest,
                    operation.header.public_key,
                    number,
                    extension.scope.table,
                    self.revocations_for(extension.manifest, &self.config.revocations)?,
                    time,
                )
                .map_err(credential)?,
        };
        if !grants.iter().any(|grant| {
            grant.scope == extension.scope
                && grant
                    .rights
                    .iter()
                    .any(|right| [Right::Update, Right::Insert, Right::Delete].contains(right))
        }) {
            return Err(denied("no write authority for encrypted operation"));
        }
        Ok(grants)
    }
    async fn row(
        &self,
        scope: ResourceScope,
        row: Commitment,
    ) -> Result<Option<Materialized>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT fields FROM secure_rows WHERE scope=? AND row=?",
                [encode(&scope)?, row.to_vec()],
            )
            .await?;
        result
            .next()
            .await?
            .map(|row| decode(&row.get::<Vec<u8>>(0)?))
            .transpose()
    }
    async fn frame(&self, id: Hash) -> Result<Option<EncryptedFrame>, Error> {
        let store = TransactionStore(self.conn.clone());
        match store.get_raw_operation(id).await? {
            Some((header, body)) => Ok(Some(
                self.stored_frame(header, body.unwrap_or_default()).await?,
            )),
            None => Ok(None),
        }
    }
    async fn status(&self, id: Hash) -> Result<ReceiveStatus, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT status,header FROM secure_operations WHERE hash=?",
                [id.as_bytes().to_vec()],
            )
            .await?;
        let Some(row) = result.next().await? else {
            // A pruned operation is known as applied while its stub exists.
            return match self.stub(id).await? {
                Some(_) => Ok(ReceiveStatus::Applied),
                None => Err(denied("missing stored operation")),
            };
        };
        match row.get::<String>(0)?.as_str() {
            "applied" => Ok(ReceiveStatus::Applied),
            "abandoned" => Ok(ReceiveStatus::Abandoned),
            "rejected" => Ok(ReceiveStatus::Rejected),
            "pending" => {
                let header: p2panda_core::Header<Extensions> = decode(&row.get::<Vec<u8>>(1)?)?;
                if self
                    .opening_key(header.extensions.scope, header.extensions.generation)
                    .is_some()
                {
                    Ok(ReceiveStatus::PendingDependencies)
                } else if self.can_read_key(header.extensions.scope, &self.local_grants(now()?)?) {
                    Ok(ReceiveStatus::MissingKey)
                } else {
                    Ok(ReceiveStatus::Custody)
                }
            }
            _ => Err(denied("invalid stored status")),
        }
    }
    async fn lookup_handle(
        &self,
        handle: Commitment,
        input: Commitment,
    ) -> Result<Option<Result<EncryptedFrame, Hash>>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT input,hash FROM secure_handles WHERE handle=?",
                [handle.to_vec()],
            )
            .await?;
        let Some(row) = result.next().await? else {
            return Ok(None);
        };
        let stored = row.get::<Vec<u8>>(0)?;
        let hash = Hash::try_from(row.get::<Vec<u8>>(1)?.as_slice())
            .map_err(|_| denied("invalid handle hash"))?;
        drop(result);
        self.refuse_abandoned(hash).await?;
        if stored != input {
            return Err(Error::Conflict);
        }
        // A handle outlives its pruned operation inside `RetentionLimits::handles`.
        Ok(Some(self.frame(hash).await?.ok_or(hash)))
    }
    /// Backpressure once retained history reaches the hard bound; automatic
    /// checkpointing and pruning start at the soft bound and make room.
    async fn check_budget(&self, incoming: usize) -> Result<(), Error> {
        if self.publishing {
            return Ok(());
        }
        let (_, bytes) = self.retained().await?;
        let bytes = bytes.saturating_sub(self.staged_bytes().await?);
        if bytes.saturating_add(incoming as u64) > self.limits.hard_bytes as u64 {
            return Err(Error::Backpressure);
        }
        Ok(())
    }
    /// Counts a committed write transaction for the durable watermark. An
    /// unreadable counter is treated as a change.
    pub(super) async fn note_commit(&mut self) {
        match rows(&self.conn, "SELECT total_changes()").await.as_deref() {
            Ok([row]) if row.as_slice() == [SqlValue::Integer(self.changes)] => {}
            Ok([row]) => {
                if let [SqlValue::Integer(total)] = row.as_slice() {
                    self.changes = *total;
                }
                self.commit += 1;
            }
            _ => self.commit += 1,
        }
    }
    /// Makes every commit so far durable; see `Flusher::flush`.
    pub(crate) fn flush(&mut self) -> Result<(), Error> {
        self.flusher.flush(self.commit)
    }
    /// Adopts the committed revision; observers are woken only by a change.
    pub(super) async fn publish_revision(&mut self) -> Result<(), Error> {
        let revision = self.stored_revision().await?;
        self.revision = revision;
        self.notifications.send_if_modified(|current| {
            let changed = *current != revision;
            *current = revision;
            changed
        });
        self.announce();
        Ok(())
    }
    /// The replica's file.
    pub(super) fn path(&self) -> &Path {
        self._owner.path()
    }
    async fn stored_identity(&self) -> Result<Vec<u8>, Error> {
        match rows(
            &self.conn,
            "SELECT identity FROM secure_metadata WHERE id=1",
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            Some([SqlValue::Blob(identity)]) => Ok(identity.clone()),
            _ => Err(denied("missing secure metadata")),
        }
    }
    /// Checks an existing file's identity, when given, and holds the union
    /// of the retained and the supplied revocations.
    async fn load_revocations(&mut self, identity: Option<&[u8]>) -> Result<(), Error> {
        let mut metadata = self
            .conn
            .query(
                "SELECT identity,revocations FROM secure_metadata WHERE id=1",
                (),
            )
            .await?;
        let row = metadata
            .next()
            .await?
            .ok_or_else(|| denied("missing secure metadata"))?;
        if identity.is_some_and(|identity| row.get::<Vec<u8>>(0).ok().as_deref() != Some(identity))
        {
            return Err(denied("secure file identity mismatch"));
        }
        let stored = RevocationSet::from_bytes(&row.get::<Vec<u8>>(1)?).map_err(credential)?;
        let mut merged = self.config.revocations.clone();
        merged
            .merge(&stored, self.config.publisher, self.definition.app, now()?)
            .map_err(credential)?;
        if merged != stored {
            self.conn
                .execute(
                    "UPDATE secure_metadata SET revocations=?,revision=revision+1 WHERE id=1",
                    [merged.to_bytes().map_err(credential)?],
                )
                .await?;
        }
        self.config.revocations = merged;
        *self
            .local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *self
            .local_selectors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        Ok(())
    }
}

/// Commits run at `synchronous=NORMAL`; `Inner::flush` makes them durable
/// before anything leaves the device. Flushes and checkpoints use the full
/// device flush. Verified at open.
async fn configure(conn: &ScopedConnection) -> Result<(), Error> {
    let durability = [
        (
            "PRAGMA journal_mode=wal",
            "PRAGMA journal_mode",
            SqlValue::Text("wal".into()),
        ),
        (
            "PRAGMA synchronous=NORMAL",
            "PRAGMA synchronous",
            SqlValue::Integer(1),
        ),
        (
            "PRAGMA fullfsync=1",
            "PRAGMA fullfsync",
            SqlValue::Integer(1),
        ),
        (
            "PRAGMA checkpoint_fullfsync=1",
            "PRAGMA checkpoint_fullfsync",
            SqlValue::Integer(1),
        ),
    ];
    for (set, read, expected) in durability {
        rows(conn, set).await?;
        if rows(conn, read).await? != vec![vec![expected.clone()]] {
            return Err(Error::Durability(format!("{read} is not {expected:?}")));
        }
    }
    // Typed tables keep real foreign keys; Rocky's own tables declare none.
    rows(conn, "PRAGMA foreign_keys=ON").await?;
    if rows(conn, "PRAGMA foreign_keys").await? != vec![vec![SqlValue::Integer(1)]] {
        return Err(Error::Incompatible("foreign keys are not enforced".into()));
    }
    // Checkpoints run between groups; a busy file is retried by the replica, not by SQLite.
    for (set, read) in [
        ("PRAGMA wal_autocheckpoint=0", "PRAGMA wal_autocheckpoint"),
        ("PRAGMA busy_timeout=0", "PRAGMA busy_timeout"),
    ] {
        rows(conn, set).await?;
        if rows(conn, read).await? != vec![vec![SqlValue::Integer(0)]] {
            return Err(Error::Incompatible(format!("{read} is not 0")));
        }
    }
    Ok(())
}
