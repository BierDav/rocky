mod admission;
mod locked_commit;
pub(crate) use locked_commit::LockedCommit;
mod live_publication;
pub(crate) mod lock;
mod maintenance;
mod membership;
mod wal;
#[cfg(any(test, feature = "qualification"))]
pub use lock::LockMeasurements;
pub(crate) use lock::{Class, Guard, ReplicaLock};
#[cfg(feature = "qualification")]
pub use lock::{HolderMeasurements, Measurements, with_measurement_connection};
#[cfg(all(test, not(target_family = "wasm")))]
pub(crate) use wal::{Hold, Passive};
mod apply;
mod author;
mod authority;
pub use authority::{read_authority_entry, remove_authority_entry, write_authority_entry};
mod blockers;
mod bulk;
pub use bulk::{MigrationMembership, MigrationStorage};
pub(super) mod bundle;
mod chains;
pub(crate) mod checkpoint;
mod compaction;
mod crypto;
pub use compaction::{CompactReport, CompactionStatus, FrameReplay, SealRebootstrap};
#[cfg(any(test, feature = "qualification"))]
mod crash;
#[cfg(any(test, feature = "qualification"))]
pub use crash::CrashPoint;
pub(crate) mod custody;
pub use custody::PartitionCustody;
mod evict;
mod frontiers;
mod generation;
mod schemas;
mod staging;
pub use staging::SchemaStage;
mod inplace;
pub use schemas::{
    InstalledTipSchema, PendingSchema, SchemaConfig, installed_tip_schema,
    last_in_place_switch_changes,
};
pub(super) mod group;
pub(crate) mod headers;
mod held;
mod held_seals;
pub use held_seals::HeldSeal;
mod author_sets;
mod holdings;
mod identity;
mod incoming;
mod intents;
mod keys;
mod metadata;
pub(crate) mod migrate;
pub(crate) mod moves;
mod offload;
mod partition;
mod placements;
pub(super) mod positions;
mod records;
#[cfg(test)]
pub(crate) use records::INTERNED;
mod stamp_authors;
pub(crate) use partition::PartitionOffer;
pub use partition::created_name;
pub use placements::{MAX_PLACEMENTS, Placement, PlacementPage, PlacementRequest};
pub(crate) mod publication;
mod quarantine;
mod queries;
mod reads;
mod rebuild;
mod recovery;
mod removal;
pub(crate) mod replay;
pub use removal::{local_row_counts, local_table_digests};
use replay::Rebuilding;
pub use replay::outcomes::OutcomeDivergence;
mod restore;
mod revoked_heads;
pub(super) mod roles;
mod session;
mod sql;
mod tip_observation;
pub use tip_observation::{
    EarlierEpochWrites, PeerScopeFrontier, TipDevice, TipNodeObservation, TipObservation,
};
pub(crate) mod tips;
pub use tips::{LineNode, MigrationPending, TipStatement};
mod typed;
mod workspace;

#[cfg(test)]
thread_local! {
    static HELD_AT_ONCE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HASHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(crate) fn held_at_once(records: usize) {
    HELD_AT_ONCE.with(|held| held.set(held.get().max(records)));
}
#[cfg(test)]
pub(crate) fn take_held_at_once() -> usize {
    HELD_AT_ONCE.with(|held| held.replace(0))
}
#[cfg(test)]
pub(crate) fn hashed(statements: usize) {
    HASHED.with(|hashed| hashed.set(hashed.get() + statements));
}
#[cfg(test)]
pub(crate) fn take_hashed() -> usize {
    HASHED.with(|hashed| hashed.replace(0))
}
use super::store::{SCHEMA, TransactionStore};
use super::wire::{Chain, Extensions, Membership, ROW_FORMAT, RowPayload, author, decrypt};
use super::*;
use crate::writer::{WriterId, stored_writer, writer, writer_key, writer_value};
use crate::{
    database::DatabaseOwner,
    scoped_connection::ScopedConnection,
    storage::{bindings, collect_rows, retryable, rows},
};
#[cfg(test)]
pub(crate) use admission::MAX_REMEMBERED_PEERS;
pub use admission::MAX_UNCONFIGURED_PEERS;
pub use bundle::{MAX_BUNDLE_BYTES, MAX_BUNDLE_FRAMES, MAX_BUNDLE_ROWS, ROWS_PER_FRAME};
pub use checkpoint::{
    AuditReport, CheckpointPage, CheckpointState, CheckpointStatement, CheckpointStatus,
    CoveredMark, CustodySummary, LogPosition, MAX_CHECKPOINT_LOGS, MAX_SEALED_OUTCOMES, PruneBlock,
    PruneReport, PrunedPage, RebootstrapReport, RetentionLimits, RetentionPolicy, SealedOutcome,
    SealedOutcomes, SealedTransaction, SetSummary, SignedCheckpoint,
};
pub use evict::EvictReport;
pub use generation::{EngineStatus, GenerationStatus};
pub use held::{CurrentValue, HeldChange, HeldField, HeldKind, HeldRow, SignedVoid};
pub use migrate::ProjectionPlan;
pub use offload::RemotePage;
use p2panda_core::validate_backlink;
use p2panda_store::operations::LocalOperationStore;
#[cfg(feature = "qualification")]
pub use publication::CHUNK_PAGES;
pub use publication::{PUBLICATION_CHUNK_BUNDLES, Publication};
use rocky_core::{
    Blocker, BlockerKind, ClearsBy, Commitment, MAX_CLOCK_AHEAD, ResourceScope, Right,
    RowApplication, RowChange, RowFields, RowSelection, RowWrite, SqlValue, TipId, WaitReason,
};
pub use roles::{
    FrontierLog, Holding, PeerLag, RangePage, ReplicaRole, ReplicaStatus, RoleConfig,
    ScopeFrontier, ScopeStatus, StateAdvice,
};
use serde::{Deserialize, Serialize};
pub use session::{SecureSession, SecureTransaction, SessionStatus, UpdateMode};
#[cfg(any(test, feature = "test-support"))]
pub use sql::ManualLeaseClock;
pub use sql::{SecureCommit, SecureRows, SecureSql};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::watch;

#[derive(Clone)]
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
    pub revocations: RevocationSet,
    pub node: PrivateKey,
    pub credentials: CredentialChain,
    pub keys: Vec<KeyMaterial>,
}
pub struct OpenConfig {
    security: SecureConfig,
    table_scopes: Option<Vec<ResourceScope>>,
    lease_clock: sql::LeaseClock,
}
impl From<SecureConfig> for OpenConfig {
    fn from(security: SecureConfig) -> Self {
        Self {
            security,
            table_scopes: None,
            lease_clock: sql::LeaseClock::Wall,
        }
    }
}
impl SecureConfig {
    pub fn with_table_scopes(self, scopes: Vec<ResourceScope>) -> OpenConfig {
        OpenConfig {
            table_scopes: Some(scopes),
            ..self.into()
        }
    }
}
impl OpenConfig {
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_manual_lease_clock(self, clock: &ManualLeaseClock) -> Self {
        Self {
            lease_clock: clock.lease_clock(),
            ..self
        }
    }
    fn families(
        &self,
        manifest: &rocky_core::ApplicationManifest,
    ) -> Result<rocky_credentials::GrantFamilies, Error> {
        match &self.table_scopes {
            Some(scopes) => {
                rocky_credentials::GrantFamilies::new(manifest, scopes).map_err(credential)
            }
            None => config_families(&self.security, manifest),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveStatus {
    Custody,
    PendingDependencies,
    MissingKey,
    Applied,
    Held,
    Duplicate,
    Rejected,
    Pruned,
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
#[derive(Clone, Debug)]
pub struct SecurePage {
    pub revision: u64,
    pub rows: BTreeMap<Commitment, RowFields>,
    pub complete: bool,
    pub next: Option<Commitment>,
}
pub const MAX_SNAPSHOT_ROWS: usize = 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;
const PAGE_CHUNK: usize = 256;
const OPENED_COMMIT: u64 = 1;
const OPENED_DURABLE: u64 = 0;
const _: () = assert!(
    OPENED_COMMIT > OPENED_DURABLE,
    "a reopened file may hold commits a crashed process never flushed, so its first commit starts ahead of the durable mark"
);
#[derive(Clone, Debug)]
pub struct CiphertextPage {
    pub frames: Vec<EncryptedFrame>,
    pub next: Option<Hash>,
    pub pruned: Vec<ResourceScope>,
    pub voids: Vec<SignedVoid>,
}
pub(crate) use rocky_core::SecureStamp as Stamp;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct AuthorKey(#[serde(with = "serde_bytes")] [u8; 32]);
impl AuthorKey {
    fn key(&self) -> Result<PublicKey, Error> {
        PublicKey::from_bytes(&self.0).map_err(|_| Error::Corrupt("author key".into()))
    }
}
impl From<PublicKey> for AuthorKey {
    fn from(key: PublicKey) -> Self {
        Self(*key.as_bytes())
    }
}

type DecodedCheckpoints = HashMap<(i64, bool), (Vec<u8>, SignedCheckpoint)>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Materialized {
    proof: Option<moves::StoredProof>,
    #[serde(with = "serde_bytes")]
    moved_from: Option<Vec<u8>>,
    #[serde(with = "byte_fields")]
    fields: RowFields,
    #[serde(with = "byte_keys")]
    stamps: BTreeMap<Commitment, Stamp>,
    authors: stamp_authors::Authors,
    deleted: bool,
    creation: Hash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<rocky_core::RowIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "serde_bytes")]
    key: Option<Commitment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<Stamp>,
}
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
type AppliedHead = (Hash, p2panda_core::Header<Extensions>);
type AppliedHeads = HashMap<ResourceScope, (TipId, Vec<AppliedHead>)>;

pub(super) struct Inner<A> {
    pub(super) conn: Arc<ScopedConnection>,
    pub(super) app: Arc<A>,
    pub(super) config: SecureConfig,
    pub(super) chain: Chain,
    chains: std::sync::Mutex<HashMap<Commitment, Chain>>,
    crypto: Arc<crypto::Cache>,
    families: rocky_credentials::GrantFamilies,
    decoded_checkpoints: std::sync::Mutex<DecodedCheckpoints>,
    tip_frontiers: std::sync::Mutex<frontiers::Frontiers>,
    applied_heads: std::sync::Mutex<AppliedHeads>,
    latest_replayed_clock: std::sync::Mutex<Option<u64>>,
    pub(crate) readiness: blockers::Readiness,
    metadata: metadata::Metadata,
    held_refresh: std::sync::Mutex<BTreeMap<ResourceScope, Commitment>>,
    author_sets: std::sync::Mutex<author_sets::Cache>,
    record_dictionary: std::sync::Mutex<records::Dictionary>,
    seeded_ledgers: std::sync::Mutex<Option<identity::SeededLedgers>>,
    #[cfg(test)]
    frontier_headers: std::sync::Mutex<tips::StoredHeaders>,
    migration_syntax: std::sync::Mutex<bulk::syntax::StoredDescriptorSyntax>,
    #[cfg(test)]
    frontier_queries: AtomicU64,
    #[cfg(test)]
    frontier_decodes: AtomicU64,
    #[cfg(test)]
    position_oracles: positions::OracleStats,
    #[cfg(test)]
    pub(crate) compare_finalization: bool,
    #[cfg(test)]
    pub(crate) finalization_oracle_time: std::time::Duration,
    #[cfg(test)]
    pub(crate) finalization_time: std::time::Duration,
    node_author: Option<identity::NodeAuthor>,
    pub(crate) credential_signer: Option<PrivateKey>,
    partitions: BTreeMap<ResourceScope, rocky_core::PartitionTuple>,
    pub(crate) partition_names: BTreeMap<ResourceScope, rocky_core::PartitionName>,
    pub(super) history: Vec<KeyMaterial>,
    pub(super) manifest: Commitment,
    pub(super) graphs: BTreeMap<rocky_core::Scope, Arc<rocky_credentials::TipGraph>>,
    catalog: BTreeMap<Commitment, rocky_core::ApplicationManifest>,
    path_fields: std::sync::Mutex<BTreeMap<(TipId, TipId), schemas::PathFields>>,
    graph_cache: std::sync::Mutex<tips::GraphCache>,
    pub(super) workspace: workspace::Workspace,
    line_nodes: BTreeMap<TipId, identity::NodeAuthor>,
    pub(super) definition: rocky_core::ApplicationManifest,
    pub(super) engine: rocky_core::EngineIdentity,
    pub(super) tables: HashSet<Commitment>,
    pub(super) fields: HashSet<Commitment>,
    permission_fields: HashSet<Commitment>,
    domains: HashSet<rocky_core::DomainId>,
    pub(super) revision: u64,
    pub(super) authority: u64,
    membership_events: watch::Sender<()>,
    pub(super) authority_events: watch::Sender<u64>,
    recovery_events: watch::Sender<i64>,
    rebuilding_tip: bool,
    opening_revocations: Option<RevocationSet>,
    tip_base: bool,
    publishing: bool,
    local: std::sync::Mutex<Option<LocalGrants>>,
    deferred_maintenance: BTreeSet<ResourceScope>,
    importing: BTreeSet<ResourceScope>,
    waits: BTreeMap<Hash, WaitReason>,
    blocked: incoming::Blocked,
    settled: std::sync::Mutex<Option<BTreeSet<Hash>>>,
    #[cfg(test)]
    pending_attempts: u64,
    ahead: BTreeMap<Hash, (PublicKey, u64)>,
    earlier_epoch: AtomicBool,
    epochs_scanned: Option<TipId>,
    rows_written: AtomicU64,
    frames_written: AtomicU64,
    applied_frames: u64,
    pending_pass: incoming::PendingPass,
    follow_pass: moves::FollowPass,
    context_epoch: AtomicU64,
    direct: BTreeSet<ResourceScope>,
    rederive_marked: bool,
    local_selectors: std::sync::Mutex<Option<LocalSelectors>>,
    pub(super) notifications: watch::Sender<u64>,
    pub(super) frames: watch::Sender<()>,
    pub(super) imported: watch::Sender<u64>,
    wal: PathBuf,
    pub(super) commit: u64,
    pub(super) flusher: Arc<Flusher>,
    changes: u64,
    pub(super) wal_limits: (u64, u64),
    pub(super) limits: RetentionLimits,
    maintenance_error: Option<String>,
    maintenance: maintenance::Pass,
    maintenance_backoff: Option<(u64, u32)>,
    verified_at: BTreeMap<i64, u64>,
    pub(super) known_peers: admission::KnownPeers,
    #[cfg(test)]
    checkpoint_export_pause: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    #[cfg(test)]
    checkpoint_store_pause: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    #[cfg(test)]
    migration_storage_pause: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    #[cfg(test)]
    header_scan_pause: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    #[cfg(test)]
    held_listing_pause: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    #[cfg(test)]
    held_token_pause: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    #[cfg(test)]
    base_prepare_pause: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    #[cfg(test)]
    stage_fault: Option<(checkpoint::Stage, usize)>,
    #[cfg(any(test, feature = "qualification"))]
    crash: std::sync::Mutex<(Option<CrashPoint>, Option<CrashPoint>)>,
    #[cfg(test)]
    publication_fault: Option<u8>,
    #[cfg(test)]
    schema_stage_pause: Option<staging::StagePause>,
    #[cfg(test)]
    plan_fault: bool,
    #[cfg(test)]
    switch_fault: bool,
    #[cfg(test)]
    withdrawal_reload_fault: bool,
    #[cfg(test)]
    stage_exit: bool,
    #[cfg(test)]
    pub(super) fail_before_commit: usize,
    #[cfg(test)]
    pub(super) fail_sql_commit: bool,
    #[cfg(test)]
    pub(super) unchecked_replayed_admission: bool,
    #[cfg(test)]
    pub(super) retry_before_commit: usize,
    #[cfg(test)]
    pub(super) group_fault: Option<group::GroupFault>,
    #[cfg(test)]
    receive_gate: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    #[cfg(test)]
    pub(super) poisoned: BTreeSet<Hash>,
    #[cfg(test)]
    pub(super) group_commits: usize,
    #[cfg(test)]
    pub(super) checkpoints: usize,
    #[cfg(test)]
    pub(super) audit_global_passes: usize,
    #[cfg(test)]
    pub(super) busy_hook: Option<Box<dyn FnMut(usize) + Send + Sync>>,
    pub(super) file: wal::OwnedFile,
}
pub(super) struct Flusher {
    #[cfg(any(test, feature = "qualification"))]
    measurements: LockMeasurements,
    #[cfg(test)]
    pause: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
    wal: PathBuf,
    durable: AtomicU64,
    #[cfg(test)]
    path: PathBuf,
    #[cfg(test)]
    flushes: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    failures: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    copy: std::sync::Mutex<Option<PathBuf>>,
}
impl Flusher {
    async fn run(self: Arc<Self>, commit: LockedCommit) -> Result<(), Error> {
        if self.durable() >= commit.count() {
            #[cfg(feature = "qualification")]
            self.measurements.flush(std::time::Duration::ZERO);
            return Ok(());
        }
        #[cfg(feature = "qualification")]
        let started = web_time::Instant::now();
        #[cfg(feature = "qualification")]
        let measurements = self.measurements.clone();
        let result = rocky_platform::task::spawn_blocking(move || self.flush(commit))
            .await
            .map_err(|_| Error::Closed)?;
        #[cfg(feature = "qualification")]
        measurements.flush(started.elapsed());
        result
    }

    pub(super) fn durable(&self) -> u64 {
        self.durable.load(Ordering::Acquire)
    }
    pub(super) fn flush(&self, commit: LockedCommit) -> Result<(), Error> {
        let commit = commit.count();
        if self.durable() >= commit {
            return Ok(());
        }
        #[cfg(test)]
        if self
            .failures
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(Error::Io(std::io::Error::other(
                "injected device flush failure",
            )));
        }
        #[cfg(test)]
        if let Some((entered, resume)) = self.pause.lock().unwrap().take() {
            let _ = entered.send(());
            let _ = resume.recv();
        }
        rocky_platform::fs::sync(&self.wal).map_err(Error::Io)?;
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
                    match rocky_platform::fs::copy(from, &to) {
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            let _ = rocky_platform::fs::remove_file(&to);
                        }
                        Err(error) => return Err(Error::Io(error)),
                    }
                }
            }
        }
        Ok(())
    }
}
async fn flush_guard<'a, A: RowApplication>(
    inner: Guard<tokio::sync::MutexGuard<'a, Inner<A>>>,
    owner: &'a ReplicaLock<Inner<A>>,
) -> Result<Guard<tokio::sync::MutexGuard<'a, Inner<A>>>, Error> {
    let authority = inner.authority;
    let inner = flushed(inner, owner).await?;
    if inner.authority != authority {
        return Err(Error::Conflict);
    }
    Ok(inner)
}
async fn flushed<'a, A: RowApplication>(
    mut inner: Guard<tokio::sync::MutexGuard<'a, Inner<A>>>,
    owner: &'a ReplicaLock<Inner<A>>,
) -> Result<Guard<tokio::sync::MutexGuard<'a, Inner<A>>>, Error> {
    loop {
        let (flusher, commit) = (inner.flusher.clone(), inner.locked_commit());
        if flusher.durable() >= commit.count() {
            let context = inner.quarantine_context();
            inner.recovery_events.send_if_modified(|current| {
                let changed = *current != context;
                *current = context;
                changed
            });
            return Ok(inner);
        }
        drop(inner);
        flusher.run(commit).await?;
        inner = owner.lock(Class::Background).await;
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffered<T> {
    pub value: T,
    pub commit: u64,
}
pub(super) type Grants = Arc<Vec<rocky_core::ResourceGrant>>;
type LocalGrants = ((u64, usize, u64), Grants);
pub(super) type Selectors = Arc<Vec<rocky_credentials::PartitionGrant>>;
type LocalSelectors = ((u64, u64), Selectors);

pub struct EncryptedReplica<A> {
    pub(super) inner: Arc<ReplicaLock<Inner<A>>>,
    queue: Arc<group::Queue>,
    revisions: watch::Receiver<u64>,
    node: PublicKey,
    lease_clock: sql::LeaseClock,
}
impl<A> Clone for EncryptedReplica<A> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            queue: self.queue.clone(),
            revisions: self.revisions.clone(),
            node: self.node,
            lease_clock: self.lease_clock.clone(),
        }
    }
}
impl<A> EncryptedReplica<A> {
    #[cfg(any(test, feature = "qualification"))]
    pub fn lock_measurements(&self) -> LockMeasurements {
        self.inner.measurements()
    }
}
pub(super) fn clock_value(clock: u64) -> Result<SqlValue, Error> {
    i64::try_from(clock)
        .map(SqlValue::Integer)
        .map_err(|_| denied("clock beyond its bound"))
}
pub(super) fn stored_clock(clock: i64) -> Result<u64, Error> {
    u64::try_from(clock).map_err(|_| Error::Corrupt("stored clock".into()))
}
fn protocol_schema() -> impl Iterator<Item = String> {
    [
        schemas::SCHEMA,
        staging::SCHEMA,
        tips::SCHEMA,
        migrate::SCHEMA,
        rebuild::SCHEMA,
        removal::SCHEMA,
        held::SCHEMA,
        revoked_heads::SCHEMA,
        placements::SCHEMA,
        bundle::SCHEMA,
        replay::SCHEMA,
        replay::BASE_SCHEMA,
        replay::outcomes::SCHEMA,
        typed::SCHEMA,
        identity::SCHEMA,
        moves::SCHEMA,
        records::SCHEMA,
        records::SHAPE_SCHEMA,
        author_sets::SCHEMA,
        partition::SCHEMA,
        checkpoint::SCHEMA,
        checkpoint::ARCHIVE_SCHEMA,
        checkpoint::PULLED_SCHEMA,
        checkpoint::FAILURE_SCHEMA,
        compaction::SCHEMA,
        compaction::TRANSITION_SCHEMA,
        compaction::HISTORY_SCHEMA,
        roles::SCHEMA,
        custody::SCHEMA,
        keys::SCHEMA,
        chains::SCHEMA,
        publication::SCHEMA,
        publication::INTENT_SCHEMA,
        bulk::SCHEMA,
        positions::SCHEMA,
        quarantine::SCHEMA,
        authority::SCHEMA,
    ]
    .into_iter()
    .flat_map(|schema| schema.iter().map(|sql| (*sql).to_owned()))
    .chain(positions::triggers())
}

pub async fn check_replica_format(path: &Path) -> Result<(), Error> {
    rocky_storage::database::check_file_format(path).await?;
    if !rocky_platform::fs::len(path).is_ok_and(|len| len > 0) {
        return Ok(());
    }
    let raw = crate::sqlite::Connection::open_read_only(path)?;
    let conn = ScopedConnection::new(raw, None, SCHEMA);
    let tables = rows(
        &conn,
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .await?;
    if tables.is_empty()
        || tables
            .iter()
            .all(|row| row == &[SqlValue::Text(authority::TABLE.into())])
    {
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
    let (label, _, _): (String, rocky_core::TipId, PublicKey) = decode(bytes)?;
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
    let schema: Vec<_> = SCHEMA
        .iter()
        .map(|sql| (*sql).to_owned())
        .chain(protocol_schema())
        .collect();
    for (table, _) in rocky_storage::scoped_connection::tables(&schema) {
        if !tables.contains(&vec![SqlValue::Text(table.clone())]) {
            return Err(Error::Corrupt(format!(
                "missing secure storage table {table}"
            )));
        }
    }
    positions::check_schema(&conn).await?;
    compaction::check_incremental_schema(&conn).await?;
    if rows(&conn, "SELECT 1 FROM secure_admission_context WHERE id=1")
        .await?
        .is_empty()
    {
        return Err(Error::Corrupt(
            "admission evidence context is missing".into(),
        ));
    }
    Ok(())
}

fn config_families(
    config: &SecureConfig,
    manifest: &rocky_core::ApplicationManifest,
) -> Result<rocky_credentials::GrantFamilies, Error> {
    let unpartitioned: BTreeSet<_> = manifest
        .entities
        .iter()
        .filter(|entity| {
            serde_json::from_str::<serde_json::Value>(&entity.shape)
                .ok()
                .is_some_and(|shape| shape["partition_by"].as_array().is_some_and(Vec::is_empty))
        })
        .map(|entity| entity.id)
        .collect();
    let scopes: BTreeSet<_> = config
        .keys
        .iter()
        .filter(|key| unpartitioned.contains(&key.scope.table))
        .map(|key| key.scope)
        .chain(
            config
                .credentials
                .0
                .iter()
                .flat_map(|link| link.capability.grants.iter())
                .map(|grant| grant.scope)
                .filter(|scope| unpartitioned.contains(&scope.table)),
        )
        .collect();
    rocky_credentials::GrantFamilies::new(manifest, &scopes.into_iter().collect::<Vec<_>>())
        .map_err(credential)
}
fn check_config_with_families(
    config: &SecureConfig,
    manifest: &rocky_core::ApplicationManifest,
    families: &rocky_credentials::GrantFamilies,
) -> Result<(), Error> {
    if config
        .credentials
        .database_context(config.publisher)
        .map_err(credential)?
        .scope
        .app
        != manifest.app
    {
        return Err(denied("credentials belong to another application"));
    }
    config
        .revocations
        .verify_in(
            config
                .credentials
                .database_context(config.publisher)
                .map_err(credential)?,
        )
        .map_err(credential)?;
    let (grants, selectors) = config
        .credentials
        .verified_with_families(config.publisher, &config.revocations, now()?, families)
        .map_err(credential)?
        .selectors(config.node.public_key(), &config.revocations, now()?)
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
    pub async fn open(
        path: impl AsRef<Path>,
        app: A,
        config: impl Into<OpenConfig>,
    ) -> Result<Self, Error> {
        Self::open_configured(path, app, config.into(), None).await
    }
    pub async fn open_following(
        path: impl AsRef<Path>,
        app: A,
        config: impl Into<OpenConfig>,
        graph: rocky_credentials::TipGraph,
        bookmark: Option<String>,
    ) -> Result<Self, Error> {
        Self::open_configured(path, app, config.into(), Some((graph, bookmark))).await
    }
    async fn open_configured(
        path: impl AsRef<Path>,
        app: A,
        config: OpenConfig,
        bootstrap: Option<(rocky_credentials::TipGraph, Option<String>)>,
    ) -> Result<Self, Error> {
        let manifest = app.manifest();
        let families = config.families(&manifest)?;
        let lease_clock = config.lease_clock;
        let mut config = config.security;
        let genesis = config
            .manifest
            .verify_historic(config.publisher)
            .map_err(credential)?;
        if genesis.publisher != manifest.publisher
            || genesis.app != manifest.app
            || genesis.domains != manifest.domains
        {
            return Err(denied("compiled application differs from the domain root"));
        }
        if let Some(certificate) = config.credentials.certificate() {
            config
                .revocations
                .link(certificate.clone())
                .map_err(credential)?;
        }
        check_config_with_families(&config, &manifest, &families)?;
        let (current, history) = keys::split(std::mem::take(&mut config.keys));
        config.keys = current;
        check_replica_format(path.as_ref()).await?;
        let (owner, raw) = DatabaseOwner::open(path.as_ref()).await?;
        let conn = Arc::new(ScopedConnection::new(raw, None, SCHEMA));
        configure(&conn).await?;
        let mut identity = generation::identity(
            rocky_credentials::genesis_tip(manifest.hash()),
            config.node.public_key(),
        )?;
        let mut wal = path.as_ref().as_os_str().to_owned();
        wal.push("-wal");
        let (notifications, _) = watch::channel(0);
        let wal = PathBuf::from(wal);
        let flusher = Arc::new(Flusher {
            #[cfg(test)]
            pause: Default::default(),
            #[cfg(any(test, feature = "qualification"))]
            measurements: LockMeasurements::default(),
            wal: wal.clone(),
            durable: AtomicU64::new(OPENED_DURABLE),
            #[cfg(test)]
            path: path.as_ref().to_path_buf(),
            #[cfg(test)]
            flushes: Default::default(),
            #[cfg(test)]
            failures: Default::default(),
            #[cfg(test)]
            copy: Default::default(),
        });
        let file = wal::OwnedFile::open(owner)?;
        let readiness = blockers::Readiness::new(&conn)?;
        let metadata = metadata::Metadata::new(&conn)?;
        let mut inner = Inner {
            conn,
            permission_fields: app.permission_fields().into_iter().collect(),
            app: Arc::new(app),
            chain: Chain::new(config.credentials.clone())?,
            chains: Default::default(),
            crypto: Default::default(),
            decoded_checkpoints: Default::default(),
            tip_frontiers: Default::default(),
            applied_heads: Default::default(),
            latest_replayed_clock: Default::default(),
            readiness,
            metadata,
            held_refresh: Default::default(),
            record_dictionary: Default::default(),
            seeded_ledgers: Default::default(),
            author_sets: Default::default(),
            #[cfg(test)]
            frontier_headers: Default::default(),
            migration_syntax: Default::default(),
            #[cfg(test)]
            frontier_queries: AtomicU64::new(0),
            #[cfg(test)]
            frontier_decodes: AtomicU64::new(0),
            #[cfg(test)]
            position_oracles: Default::default(),
            #[cfg(test)]
            compare_finalization: true,
            #[cfg(test)]
            finalization_oracle_time: Default::default(),
            #[cfg(test)]
            finalization_time: Default::default(),
            node_author: Some(identity::NodeAuthor::new(&config)?),
            families,
            config,
            credential_signer: None,
            partitions: BTreeMap::new(),
            partition_names: BTreeMap::new(),
            history,
            manifest: manifest.hash(),
            workspace: workspace::Workspace::new(rocky_credentials::genesis_tip(manifest.hash())),
            graphs: BTreeMap::new(),
            catalog: BTreeMap::new(),
            path_fields: Default::default(),
            graph_cache: Default::default(),
            line_nodes: BTreeMap::new(),
            tables: manifest.entities.iter().map(|item| item.id).collect(),
            fields: manifest.fields.iter().map(|item| item.id).collect(),
            domains: manifest.domains.iter().map(|item| item.domain).collect(),
            definition: manifest,
            revision: 0,
            authority: 1,
            membership_events: watch::channel(()).0,
            authority_events: watch::channel(1).0,
            recovery_events: watch::channel(0).0,
            rebuilding_tip: false,
            engine: rocky_core::EngineIdentity::current(),
            opening_revocations: None,
            tip_base: false,
            publishing: false,
            local: Default::default(),
            local_selectors: Default::default(),
            deferred_maintenance: BTreeSet::new(),
            importing: BTreeSet::new(),
            waits: BTreeMap::new(),
            blocked: Default::default(),
            settled: std::sync::Mutex::new(Some(BTreeSet::new())),
            #[cfg(test)]
            pending_attempts: 0,
            ahead: BTreeMap::new(),
            earlier_epoch: AtomicBool::new(false),
            epochs_scanned: None,
            rows_written: AtomicU64::new(0),
            frames_written: AtomicU64::new(0),
            applied_frames: 0,
            pending_pass: Default::default(),
            follow_pass: Default::default(),
            context_epoch: AtomicU64::new(0),
            direct: BTreeSet::new(),
            rederive_marked: true,
            notifications,
            frames: watch::channel(()).0,
            imported: watch::channel(0).0,
            wal,
            commit: OPENED_COMMIT,
            flusher: flusher.clone(),
            changes: 0,
            wal_limits: (group::WAL_CHECKPOINT_BYTES, group::WAL_LIMIT_BYTES),
            file,
            limits: RetentionLimits::default(),
            maintenance_error: None,
            maintenance: Default::default(),
            maintenance_backoff: None,
            verified_at: BTreeMap::new(),
            known_peers: Default::default(),
            #[cfg(test)]
            checkpoint_export_pause: None,
            #[cfg(test)]
            checkpoint_store_pause: None,
            #[cfg(test)]
            header_scan_pause: None,
            #[cfg(test)]
            held_listing_pause: None,
            #[cfg(test)]
            migration_storage_pause: None,
            #[cfg(test)]
            held_token_pause: None,
            #[cfg(test)]
            base_prepare_pause: None,
            #[cfg(test)]
            stage_fault: None,
            #[cfg(any(test, feature = "qualification"))]
            crash: Default::default(),
            #[cfg(test)]
            #[cfg(test)]
            publication_fault: None,
            #[cfg(test)]
            schema_stage_pause: None,
            #[cfg(test)]
            plan_fault: false,
            #[cfg(test)]
            switch_fault: false,
            #[cfg(test)]
            withdrawal_reload_fault: false,
            #[cfg(test)]
            stage_exit: false,
            #[cfg(test)]
            fail_before_commit: 0,
            #[cfg(test)]
            fail_sql_commit: false,
            #[cfg(test)]
            unchecked_replayed_admission: false,
            #[cfg(test)]
            retry_before_commit: 0,
            #[cfg(test)]
            group_fault: None,
            #[cfg(test)]
            receive_gate: None,
            #[cfg(test)]
            poisoned: BTreeSet::new(),
            #[cfg(test)]
            group_commits: 0,
            #[cfg(test)]
            checkpoints: 0,
            #[cfg(test)]
            audit_global_passes: 0,
            #[cfg(test)]
            busy_hook: None,
        };
        let supplied_revocations = inner.config.revocations.clone();
        transaction!(&mut inner, |inner| {
            let tables = rows(
                &inner.conn,
                "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .await?;
            let fresh = tables.is_empty()
                || tables
                    .iter()
                    .all(|row| row == &[SqlValue::Text(authority::TABLE.into())]);
            if fresh {
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
            if fresh {
                for sql in protocol_schema() {
                    inner.conn.execute(sql, ()).await?;
                }
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_retained VALUES(1,0,0,0,?)",
                        [checkpoint::LtHash16::new().to_bytes()],
                    )
                    .await?;
                inner.create_admission_context().await?;
            }
            if fresh {
                if inner.manifest != genesis.hash() && bootstrap.is_none() {
                    return Err(denied(
                        "a new replica must install its authenticated genesis first",
                    ));
                }
                inner.store_schema(&inner.definition, &[]).await?;
                inner.store_schema(&genesis, &[]).await?;
                let (grants, selectors) = inner
                    .config
                    .credentials
                    .verified_with_families(
                        inner.config.publisher,
                        &inner.config.revocations,
                        now()?,
                        inner.grant_families(),
                    )
                    .map_err(credential)?
                    .selectors(
                        inner.config.node.public_key(),
                        &inner.config.revocations,
                        now()?,
                    )
                    .map_err(credential)?;
                let scopes: BTreeSet<_> = grants
                    .iter()
                    .map(|grant| grant.scope.domain)
                    .chain(selectors.iter().map(|grant| grant.domain))
                    .chain(inner.config.keys.iter().map(|key| key.scope.domain))
                    .collect();
                if inner.definition.domains.iter().any(|rule| {
                    !scopes.iter().any(|scope| {
                        scope.app == inner.definition.app && scope.domain == rule.domain
                    })
                }) {
                    return Err(denied(
                        "each installed domain requires an authenticated scope",
                    ));
                }
                for scope in scopes {
                    inner
                        .conn
                        .execute(
                            "INSERT INTO secure_tip_graphs VALUES(?,?)",
                            [encode(&scope)?, genesis.hash().to_vec()],
                        )
                        .await?;
                }
                if let Some((graph, bookmark)) = &bootstrap {
                    if graph.genesis_schema != genesis.hash() {
                        return Err(denied("enrollment genesis mismatch"));
                    }
                    let statements = tips::graph_statements(graph);
                    for statement in &statements {
                        statement.verify(inner.config.publisher, graph, None)?;
                    }
                    let mut graph = graph.clone();
                    graph
                        .exclude_revoked(&inner.config.revocations)
                        .map_err(credential)?;
                    let target = match bookmark {
                        Some(name) => graph
                            .bookmark(name)
                            .map_err(credential)?
                            .ok_or_else(|| denied("enrollment bookmark missing"))?,
                        None => graph.genesis(),
                    };
                    if graph.schema(target).map_err(credential)? != inner.manifest {
                        return Err(denied("enrollment schema mismatch"));
                    }
                    for statement in statements {
                        inner.conn.execute("INSERT OR IGNORE INTO secure_tip_statements(digest,statement) VALUES(?,?)", [statement.id()?.to_vec(), encode(&statement)?]).await?;
                    }
                    if let Some(name) = bookmark {
                        inner.invalidate_context();
                        inner.workspace.tip = target;
                        inner.workspace.followed = Some((graph.scope, name.clone()));
                        inner.store_followed(graph.scope, name, target).await?;
                        identity = generation::identity(target, inner.config.node.public_key())?;
                    }
                }
            } else {
                inner.load_schemas().await?;
                inner.load_tip_graphs().await?;
                if inner.workspace.followed.is_some() {
                    identity =
                        generation::identity(inner.workspace.tip, inner.config.node.public_key())?;
                }
                inner
                    .load_revocations(&supplied_revocations, Some(identity.as_slice()))
                    .await?;
            }
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET identity=? WHERE id=1",
                    [identity.clone()],
                )
                .await?;
            inner
                .conn
                .execute(
                    "INSERT OR IGNORE INTO secure_custody_bootstrap VALUES(1,?)",
                    [inner.config.manifest.to_bytes().map_err(credential)?],
                )
                .await?;
            identity::check_node_chain(&inner.config.credentials)?;
            Ok(())
        })?;
        inner.load_schemas().await?;
        inner.load_tip_graphs().await?;
        if !inner.schema_switching().await?
            && let Err(replay::BrokenReference(violation)) = inner.finish_or_discard_plan().await?
        {
            tracing::warn!(
                %violation,
                "a recorded projection rebuild would break a foreign key and is discarded"
            );
        }
        inner.load_typed().await?;
        inner.load_partitions().await?;
        inner.load_custody_peers().await?;
        inner.revision = inner.stored_revision().await?;
        inner.notifications.send_replace(inner.revision);
        let revisions = inner.notifications.subscribe();
        inner.local_grants(now()?)?;
        inner.validate_keys(now()?)?;
        inner.adopt_keys().await?;
        inner.load_direct().await?;
        inner.rebuild_voided().await?;
        inner.verify_open().await?;
        let opening_revocations = inner.opening_revocations.take();
        let node = inner.config.node.public_key();
        let replica = Self {
            inner: Arc::new(ReplicaLock::new(
                inner,
                #[cfg(any(test, feature = "qualification"))]
                flusher.measurements.clone(),
            )),
            queue: Arc::default(),
            revisions,
            node,
            lease_clock,
        };
        if let Some(revocations) = opening_revocations {
            replica.learn_revocations(&revocations).await?;
        } else {
            replica.settle().await?;
        }
        Ok(replica)
    }
    #[cfg(test)]
    pub(crate) async fn maintenance_queued(&self, scope: ResourceScope) -> bool {
        self.inner
            .lock(Class::Background)
            .await
            .deferred_maintenance
            .contains(&scope)
    }
    #[cfg(test)]
    #[cfg_attr(
        target_family = "wasm",
        expect(
            dead_code,
            reason = "Flush pause hooks are exercised only by native thread tests."
        )
    )]
    pub(crate) async fn pause_flush(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (resume, resumed) = std::sync::mpsc::channel();
        *self
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .flusher
            .pause
            .lock()
            .unwrap() = Some((entered, resumed));
        (waiting, resume)
    }
    pub async fn observe_frames(&self) -> watch::Receiver<()> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .frames
            .subscribe()
    }
    pub async fn observe_membership(&self) -> watch::Receiver<()> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .membership_events
            .subscribe()
    }
    pub async fn observe_authority(&self) -> watch::Receiver<u64> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .authority_events
            .subscribe()
    }
    pub async fn observe_recovery(&self) -> watch::Receiver<i64> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .recovery_events
            .subscribe()
    }
    pub async fn clock_wait(&self) -> Result<Option<std::time::Duration>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let Some(stamp) = inner.ahead.values().map(|(_, stamp)| *stamp).min() else {
            return Ok(None);
        };
        let due = stamp.saturating_sub(MAX_CLOCK_AHEAD.as_micros() as u64);
        Ok(Some(std::time::Duration::from_micros(
            due.saturating_sub(micros()?),
        )))
    }
    pub fn observe_revision(&self) -> watch::Receiver<u64> {
        self.revisions.clone()
    }
    pub async fn observe_imports(&self) -> watch::Receiver<u64> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .imported
            .subscribe()
    }
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
            (group::Done::Pruned(_), _) => Err(denied(
                "mutation handle was committed before the pruned prefix",
            )),
            _ => Err(denied("missing authoring result")),
        }
    }
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
    pub async fn receive(&self, frame: EncryptedFrame) -> Result<ReceiveStatus, Error> {
        self.receive_all(vec![frame])
            .await
            .pop()
            .unwrap_or_else(|| Err(denied("missing receive result")))
    }
    pub async fn receive_all(
        &self,
        frames: Vec<EncryptedFrame>,
    ) -> Vec<Result<ReceiveStatus, Error>> {
        self.receive_with(frames, true, false)
            .await
            .into_iter()
            .map(|result| result.map(|status| status.value))
            .collect()
    }
    pub async fn receive_buffered(
        &self,
        frames: Vec<EncryptedFrame>,
    ) -> Vec<Result<Buffered<ReceiveStatus>, Error>> {
        self.receive_with(frames, false, false).await
    }
    pub(super) async fn receive_with(
        &self,
        frames: Vec<EncryptedFrame>,
        exposed: bool,
        proofs: bool,
    ) -> Vec<Result<Buffered<ReceiveStatus>, Error>> {
        let count = frames.len();
        let frames = match self.preopen(frames).await {
            Ok(frames) => frames,
            Err(error) => return (0..count).map(|_| Err(group::shared(&error))).collect(),
        };
        if let Err(error) = self.learn_carried_membership(&frames).await {
            return (0..count).map(|_| Err(group::shared(&error))).collect();
        }
        let mut work = Vec::new();
        let mut positions = Vec::new();
        let mut results: Vec<Option<Result<Buffered<ReceiveStatus>, Error>>> =
            (0..frames.len()).map(|_| None).collect();
        for (index, frame) in frames.into_iter().enumerate() {
            if let Some(membership) = &frame.migration {
                if !proofs {
                    results[index] = Some(Err(denied("proof-bearing receive required")));
                    continue;
                }
                if let Some(signed) = &membership.descriptor {
                    if signed.statement.publication != membership.publication {
                        results[index] = Some(Err(denied("migration descriptor identity")));
                        continue;
                    }
                    if let Err(error) = self.admit_migration_publication((**signed).clone()).await {
                        results[index] = Some(Err(error));
                        continue;
                    }
                }
            }
            let verified = {
                let inner = self.inner.lock(crate::replica::Class::Background).await;
                match now() {
                    Ok(time) => inner.check_migration_frame(&frame, time).await.map(|_| ()),
                    Err(error) => Err(error),
                }
            };
            if let Err(error) = verified {
                results[index] = Some(Err(error));
                continue;
            }
            let contradicted = match frame.operation_ref() {
                Ok(operation) => {
                    self.inner
                        .lock(crate::replica::Class::Background)
                        .await
                        .refuse_no_frames_contradiction_before_queueing(&operation.header)
                        .await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = contradicted {
                results[index] = Some(Err(error));
                continue;
            }
            let weight = frame.weight();
            positions.push(index);
            work.push((group::Work::Receive(frame), weight));
        }
        #[cfg(test)]
        {
            let gate = self
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .receive_gate
                .take();
            if let Some((reached, resume)) = gate {
                let _ = reached.send(());
                let _ = resume.await;
            }
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
    pub async fn flush(&self) -> Result<u64, Error> {
        let (flusher, commit) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            (inner.flusher.clone(), inner.locked_commit())
        };
        if flusher.durable() < commit.count() {
            let flushing = flusher.clone();
            rocky_platform::task::spawn_blocking(move || flushing.flush(commit))
                .await
                .map_err(|_| Error::Closed)??;
        }
        Ok(flusher.durable())
    }
    pub async fn optimize(&self) {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let _ = inner.conn.execute("PRAGMA optimize", ()).await;
        let passive = inner.file.passive.clone();
        drop(inner);
        passive.wait().await;
    }
    pub async fn close(self) {
        let passive = self
            .inner
            .lock(Class::Background)
            .await
            .file
            .passive
            .clone();
        drop(self);
        passive.wait().await;
    }
    pub async fn maintain(&self) -> bool {
        loop {
            let mut inner = self.inner.lock(Class::Background).await;
            let foreign_commit_not_durable =
                inner.flusher.durable() < inner.commit && inner.maintenance.commit != inner.commit;
            if foreign_commit_not_durable {
                return false;
            }
            let Some(scope) = inner.deferred_maintenance.first().copied() else {
                inner.checkpoint_if_due().await;
                return true;
            };
            if !inner.maintain(scope).await {
                inner.deferred_maintenance.remove(&scope);
            }
            drop(inner);
            rocky_platform::task::yield_now().await;
        }
    }
    pub(crate) async fn maintain_scopes(&self, mut scopes: BTreeSet<ResourceScope>) {
        while let Some(scope) = scopes.pop_first() {
            loop {
                let mut inner = self.inner.lock(Class::Background).await;
                let more = inner.maintain(scope).await;
                drop(inner);
                if !more {
                    break;
                }
                rocky_platform::task::yield_now().await;
            }
        }
    }
    pub async fn set_importing(&self, scopes: BTreeSet<ResourceScope>) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .importing = scopes;
    }
    pub async fn durable_commit(&self) -> u64 {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .flusher
            .durable()
    }
    pub async fn install_key(&self, key: KeyMaterial) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
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
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.config.keys.retain(|key| key.scope != scope);
        inner.history.retain(|key| key.scope != scope);
        inner.bump_authority();
    }
    pub async fn handle_committed(&self, handle: Commitment) -> Result<bool, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut result = inner
            .conn
            .query(
                "SELECT 1 FROM secure_handles WHERE handle=?",
                [handle.to_vec()],
            )
            .await?;
        Ok(result.next().await?.is_some())
    }
    pub async fn receipt_status(&self, id: Hash) -> Result<ReceiveStatus, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner = flushed(inner, &self.inner).await?;
        inner.status(id).await
    }
    pub async fn learn_revocations(
        &self,
        revocations: &RevocationSet,
    ) -> Result<Vec<SignedRevocation>, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let added = inner.merge_revocations(revocations).await?;
        drop(inner);
        self.settle().await?;
        Ok(added)
    }
    pub(crate) async fn learn_session_revocations(
        &self,
        statements: Vec<SignedRevocation>,
        own: &CredentialChain,
    ) -> Result<Option<SignedRevocation>, Error> {
        let (publisher, app) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            (inner.config.publisher, inner.definition.app)
        };
        let mut revoking = None;
        let mut others = Vec::new();
        for signed in statements {
            signed.verify_historic(publisher, app).map_err(credential)?;
            let single = RevocationSet::try_from(vec![signed.clone()]).map_err(credential)?;
            single
                .verify_in(own.database_context(publisher).map_err(credential)?)
                .map_err(credential)?;
            if single.revoking_holder(own).map_err(credential)?.is_some() {
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
    pub async fn revocations(&self) -> RevocationSet {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .config
            .revocations
            .clone()
    }
    pub async fn revocation_digest(&self) -> Result<Commitment, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .revocation_digest()
    }
    #[cfg(test)]
    pub(crate) async fn inject_failure(&self, retries: usize, failures: usize) {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.retry_before_commit = retries;
        inner.fail_before_commit = failures;
    }
    #[cfg(test)]
    pub(crate) async fn retry_open_revocations_for_test(
        &self,
        supplied: RevocationSet,
    ) -> Result<RevocationSet, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.config.revocations = supplied.clone();
        transaction!(inner, |inner| {
            inner.load_revocations(&supplied, None).await?;
            inner.injected_commit_fault()?;
            Ok(())
        })?;
        Ok(inner
            .opening_revocations
            .take()
            .unwrap_or_else(|| inner.config.revocations.clone()))
    }
    #[cfg(test)]
    pub(crate) async fn inject_group_fault(&self, fault: Option<group::GroupFault>) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .group_fault = fault;
    }
    #[cfg(test)]
    pub(crate) async fn gate_next_receive(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached, at_gate) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .receive_gate = Some((reached, resumed));
        (at_gate, resume)
    }
    #[cfg(test)]
    pub(crate) async fn group_commits(&self) -> usize {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .group_commits
    }
    #[cfg(test)]
    pub(crate) async fn checkpoints(&self) -> usize {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .checkpoints
    }
    #[cfg(test)]
    pub(crate) async fn set_wal_limits(&self, checkpoint: u64, limit: u64) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .wal_limits = (checkpoint, limit);
    }
    #[cfg(test)]
    pub(crate) async fn set_busy_hook(&self, hook: Option<Box<dyn FnMut(usize) + Send + Sync>>) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .busy_hook = hook;
    }
    #[cfg(test)]
    pub(crate) async fn watermark(&self) -> (u64, u64, usize) {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
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
            .lock(crate::replica::Class::Background)
            .await
            .flusher
            .copy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = directory;
    }
    #[cfg(test)]
    pub(crate) async fn stored_and_memory_revision(&self) -> (u64, u64) {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        (inner.stored_revision().await.unwrap(), inner.revision)
    }
    #[cfg(test)]
    pub(crate) async fn install_pulled_for_test(
        &self,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let id = inner
            .find_scope_id(scope)
            .await?
            .ok_or_else(|| denied("unknown scope"))?;
        transaction!(inner, |inner| {
            Ok(inner.install_pulled(scope, id).await?
                == checkpoint::pulled::PulledInstall::Installed)
        })
    }
    #[cfg(test)]
    pub(crate) async fn set_catalog_entry_for_test(
        &self,
        manifest: Commitment,
        definition: Option<rocky_core::ApplicationManifest>,
    ) -> Option<rocky_core::ApplicationManifest> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.invalidate_context();
        match definition {
            Some(definition) => inner.catalog.insert(manifest, definition),
            None => inner.catalog.remove(&manifest),
        }
    }
}
impl<A: RowApplication> Inner<A> {
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
            .verified_with_families(
                self.config.publisher,
                &self.config.revocations,
                time,
                self.grant_families(),
            )
            .map_err(credential)?
            .selectors(
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
        let reach = self.frame_reach(frame)?;
        if matches!(reach, tips::FrameReach::Unfollowed) {
            return Err(denied(
                "frame belongs to a tip this replica does not follow",
            ));
        }
        self.verify_frame_authority(frame, time)
    }
    pub(super) fn verify_frame_authority(
        &self,
        frame: &EncryptedFrame,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        let operation = frame.operation_ref()?;
        let frame_manifest = self.manifest_of_tip(operation.header.extensions.tip)?;
        if !self.catalog.contains_key(&frame_manifest)
            && self.frame_reach(frame)? == tips::FrameReach::Held
        {
            return Err(denied("held source schema is not installed"));
        }
        self.verify_header_authority(&operation.header, frame.chain()?.credentials(), time)
    }
    pub(super) fn historical_frame_authority(
        &self,
        frame: &EncryptedFrame,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        let header = &frame.operation_ref()?.header;
        self.header_authority(
            header,
            frame.chain()?.credentials(),
            &RevocationSet::default(),
            header.timestamp / 1_000_000,
        )
    }
    pub(super) fn verify_header_authority(
        &self,
        header: &p2panda_core::Header<Extensions>,
        credentials: &CredentialChain,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        self.header_authority(header, credentials, &self.config.revocations, time)
    }
    pub(super) fn placement_authority(
        &self,
        header: &p2panda_core::Header<Extensions>,
        credentials: &CredentialChain,
        time: u64,
    ) -> Result<(), Error> {
        let extension = &header.extensions;
        if !self.partitioned(extension.scope) || self.partitions.contains_key(&extension.scope) {
            return self
                .verify_header_authority(header, credentials, time)
                .map(|_| ());
        }
        self.check_header_table(extension)?;
        if !extension.authored {
            return Err(denied("node author missing"));
        }
        let revocations = &self.config.revocations;
        self.verified_chain(credentials, revocations, time)?
            .unseen_partition_author(header.public_key, extension.scope, revocations, time)
            .map_err(credential)
    }
    fn check_header_table(&self, extension: &Extensions) -> Result<(), Error> {
        let manifest = &self.definition;
        let source = self.catalog.get(&self.manifest_of_tip(extension.tip)?);
        if extension.scope.domain.app != manifest.app
            || !self.domains.contains(&extension.scope.domain.domain)
            || source.is_some_and(|definition| {
                !definition
                    .entities
                    .iter()
                    .any(|table| table.id == extension.scope.table)
            })
        {
            return Err(denied("wrong app/domain/table/manifest"));
        }
        Ok(())
    }
    fn header_authority(
        &self,
        header: &p2panda_core::Header<Extensions>,
        credentials: &CredentialChain,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        let extension = &header.extensions;
        self.check_header_table(extension)?;
        let partition = self.authenticated_partition(extension.scope)?;
        if !extension.authored {
            return Err(denied("node author missing"));
        }
        let verified = self.verified_chain(credentials, revocations, time)?;
        let grants = match partition {
            Some(tuple) => verified.partition_author(
                header.public_key,
                extension.scope,
                tuple,
                revocations,
                time,
            ),
            None => verified.author(header.public_key, extension.scope.table, revocations, time),
        }
        .map_err(credential)?;
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
                "SELECT fields FROM secure_row_records WHERE scope=? AND row=?",
                [encode(&scope)?, row.to_vec()],
            )
            .await?;
        match result.next().await? {
            Some(row) => {
                let bytes = row.get::<Vec<u8>>(0)?;
                Ok(Some(self.decode_record(&bytes).await?))
            }
            None => Ok(None),
        }
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
            return match self.stub(id).await? {
                Some(_) => Ok(ReceiveStatus::Applied),
                None => Err(denied("missing stored operation")),
            };
        };
        match row.get::<String>(0)?.as_str() {
            "applied" => Ok(ReceiveStatus::Applied),
            "held" => Ok(ReceiveStatus::Held),
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
        if stored != input {
            return Err(Error::Conflict);
        }
        Ok(Some(self.frame(hash).await?.ok_or(hash)))
    }
    async fn check_budget(&self, incoming: usize) -> Result<(), Error> {
        if self.publishing {
            return Ok(());
        }
        let (_, bytes) = self.retained().await?;
        let bytes = bytes
            .checked_sub(self.migration_bytes().await?)
            .ok_or_else(|| Error::Corrupt("migration accounting exceeds history".into()))?;
        if bytes
            .checked_add(incoming as u64)
            .is_none_or(|total| total > self.limits.hard_bytes as u64)
        {
            return Err(Error::Backpressure);
        }
        Ok(())
    }
    pub(super) async fn note_commit(&mut self) {
        match self.conn.connection().total_changes() {
            Ok(total) if total == self.changes => {}
            Ok(total) => {
                self.changes = total;
                self.commit += 1;
            }
            Err(_) => self.commit += 1,
        }
        self.readiness.invalidate();
        self.refresh_readers().await;
    }
    #[cfg(test)]
    pub(crate) fn flush(&mut self) -> Result<(), Error> {
        self.flusher.flush(self.locked_commit())
    }
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
    pub(super) fn path(&self) -> &Path {
        self.file.path()
    }
    async fn load_revocations(
        &mut self,
        supplied: &RevocationSet,
        identity: Option<&[u8]>,
    ) -> Result<(), Error> {
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
        stored
            .verify_in(
                self.config
                    .credentials
                    .database_context(self.config.publisher)
                    .map_err(credential)?,
            )
            .map_err(credential)?;
        let mut merged = supplied.clone();
        merged
            .merge(&stored, self.config.publisher, self.definition.app, now()?)
            .map_err(credential)?;
        self.opening_revocations = (merged != stored).then_some(merged);
        self.invalidate_context();
        self.config.revocations = stored;
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

enum PragmaValue {
    Text(&'static str),
    Integer(i64),
}
impl PragmaValue {
    fn sql(&self) -> SqlValue {
        match self {
            Self::Text(text) => SqlValue::Text((*text).into()),
            Self::Integer(value) => SqlValue::Integer(*value),
        }
    }
}
type CheckedPragma = (&'static str, &'static str, PragmaValue);
const COMMITS_BUFFERED_UNTIL_A_FULL_DEVICE_FLUSH: [CheckedPragma; 4] = [
    (
        "PRAGMA journal_mode=wal",
        "PRAGMA journal_mode",
        PragmaValue::Text("wal"),
    ),
    (
        "PRAGMA synchronous=NORMAL",
        "PRAGMA synchronous",
        PragmaValue::Integer(1),
    ),
    (
        "PRAGMA fullfsync=1",
        "PRAGMA fullfsync",
        PragmaValue::Integer(1),
    ),
    (
        "PRAGMA checkpoint_fullfsync=1",
        "PRAGMA checkpoint_fullfsync",
        PragmaValue::Integer(1),
    ),
];
const TYPED_TABLE_FOREIGN_KEYS_ENFORCED: (&str, &str) =
    ("PRAGMA foreign_keys=ON", "PRAGMA foreign_keys");
const REPLICA_OWNS_WAL_CHECKPOINTS_AND_BUSY_RETRIES: [(&str, &str); 2] = [
    ("PRAGMA wal_autocheckpoint=0", "PRAGMA wal_autocheckpoint"),
    ("PRAGMA busy_timeout=0", "PRAGMA busy_timeout"),
];
async fn configure(conn: &ScopedConnection) -> Result<(), Error> {
    for (set, read, expected) in &COMMITS_BUFFERED_UNTIL_A_FULL_DEVICE_FLUSH {
        let expected = expected.sql();
        rows(conn, set).await?;
        if rows(conn, read).await? != vec![vec![expected.clone()]] {
            return Err(Error::Durability(format!("{read} is not {expected:?}")));
        }
    }
    let (set, read) = TYPED_TABLE_FOREIGN_KEYS_ENFORCED;
    rows(conn, set).await?;
    if rows(conn, read).await? != vec![vec![SqlValue::Integer(1)]] {
        return Err(Error::Incompatible("foreign keys are not enforced".into()));
    }
    for (set, read) in REPLICA_OWNS_WAL_CHECKPOINTS_AND_BUSY_RETRIES {
        rows(conn, set).await?;
        if rows(conn, read).await? != vec![vec![SqlValue::Integer(0)]] {
            return Err(Error::Incompatible(format!("{read} is not 0")));
        }
    }
    for (name, value) in [
        ("cache_size", group::CACHE_KIB / 2),
        ("temp.cache_size", group::CACHE_KIB / 2),
        ("journal_size_limit", group::WAL_CHECKPOINT_BYTES as i64),
    ] {
        rows(conn, &format!("PRAGMA {name}={value}")).await?;
    }
    Ok(())
}

impl<A: RowApplication> Inner<A> {
    async fn merge_revocations(
        &mut self,
        revocations: &RevocationSet,
    ) -> Result<Vec<SignedRevocation>, Error> {
        revocations
            .verify_in(
                self.config
                    .credentials
                    .database_context(self.config.publisher)
                    .map_err(credential)?,
            )
            .map_err(credential)?;
        let mut merged = self.config.revocations.clone();
        let added = merged
            .merge(
                revocations,
                self.config.publisher,
                self.definition.app,
                now()?,
            )
            .map_err(credential)?;
        if merged == self.config.revocations {
            return Ok(added);
        }
        let authority_changed = !added.is_empty()
            || self.config.revocations.certificates().any(|certificate| {
                let node = certificate.capability.subject;
                merged.ends_node_of(certificate.issuer, node, false)
                    != self
                        .config
                        .revocations
                        .ends_node_of(certificate.issuer, node, false)
            });
        let membership_changed = merged
            .certificates()
            .ne(self.config.revocations.certificates());
        let update = if authority_changed {
            "UPDATE secure_metadata SET revocations=?,revision=revision+1 WHERE id=1"
        } else {
            "UPDATE secure_metadata SET revocations=? WHERE id=1"
        };
        let bytes = merged.to_bytes().map_err(credential)?;
        let old_graphs = self.graphs.clone();
        let old_tip = self.workspace.tip;
        let old_author = self.node_author.clone();
        let old_nodes = self.line_nodes.clone();
        let old_followed = self.workspace.followed.clone();
        let old_revocations = self.config.revocations.clone();
        let result = transaction!(self, |inner| {
            inner.invalidate_context();
            inner.config.revocations = merged.clone();
            inner.graphs = old_graphs.clone();
            inner.workspace.tip = old_tip;
            inner.node_author = old_author.clone();
            inner.line_nodes = old_nodes.clone();
            inner.workspace.followed = old_followed.clone();
            inner.conn.execute(update, [bytes.clone()]).await?;
            if authority_changed {
                inner.resolve_revoked(&merged).await?;
            }
            Ok(())
        });
        if let Err(error) = result {
            self.invalidate_context();
            self.graphs = old_graphs;
            self.workspace.tip = old_tip;
            self.node_author = old_author;
            self.line_nodes = old_nodes;
            self.workspace.followed = old_followed;
            self.config.revocations = old_revocations;
            return Err(error);
        }
        self.invalidate_context();
        self.config.revocations = merged;
        if authority_changed {
            self.bump_authority();
        }
        if authority_changed {
            self.publish_revision().await?;
        }
        if membership_changed {
            self.membership_events.send_replace(());
        }
        Ok(added)
    }
}
