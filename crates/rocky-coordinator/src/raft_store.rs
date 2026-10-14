//! Durable Raft storage: a small hard-state file, one file per log entry and
//! at most two snapshot files. The state machine is applied in memory and
//! recovered from the newest snapshot plus committed log replay.
use crate::{certified::*, machine::*};
use fs2::FileExt;
use openraft::{
    storage::{LogFlushed, RaftLogStorage, RaftStateMachine},
    *,
};
use rocky_core::{Commitment, CoordinationError, commitment};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Cursor, Write},
    ops::RangeBounds,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

openraft::declare_raft_types!(pub Types: D = Proposal, R = Result<Outcome, CoordinationError>);
pub type Consensus = Raft<Types>;
/// The storage format this build reads; older images are refused.
const FORMAT: u32 = 3;
const RETAINED_SNAPSHOTS: usize = 2;
const MAX_LOGS: usize = 2048;
const MAX_LOG_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Hard {
    format: u32,
    identity: Commitment,
    /// Budgets fixed at provisioning, so a replay without snapshot is identical.
    limits: crate::Limits,
    vote: Option<Vote<u64>>,
    committed: Option<LogId<u64>>,
    purged: Option<LogId<u64>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotFile {
    format: u32,
    identity: Commitment,
    meta: SnapshotMeta<u64, BasicNode>,
    metadata: Encoded,
}
/// CBOR byte strings avoid expanding nested encoded snapshots into integer arrays.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Encoded(#[serde(with = "encoded_bytes")] Vec<u8>);
mod encoded_bytes {
    use serde::{
        Deserializer, Serializer,
        de::{Error, Visitor},
    };
    pub fn serialize<S: Serializer>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(data)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        struct Bytes;
        impl<'de> Visitor<'de> for Bytes {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an encoded byte string")
            }
            fn visit_bytes<E: Error>(self, bytes: &[u8]) -> Result<Vec<u8>, E> {
                Ok(bytes.to_vec())
            }
            fn visit_byte_buf<E: Error>(self, bytes: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(bytes)
            }
        }
        deserializer.deserialize_byte_buf(Bytes)
    }
}
/// A restored voter neither votes nor campaigns until the membership
/// authority has re-added it: first as a learner, at a log index beyond
/// everything its backup held.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fence {
    pub index: u64,
    pub learner: bool,
}
const FENCE: &str = "RESTORED";
const FENCE_LABEL: &str = "rocky.voter-fence.v1";
/// What a voter backup holds, beside a stopped voter's files.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Backup {
    pub identity: Commitment,
    pub node: u64,
    /// The last log index the copy holds, applied or not.
    pub index: u64,
}
const BACKUP: &str = "BACKUP";
const BACKUP_LABEL: &str = "rocky.voter-backup.v1";
struct Image {
    hard: Hard,
    fence: Option<Fence>,
    pending_purge: Option<LogId<u64>>,
    logs: BTreeMap<u64, Entry<Types>>,
    applied: Option<LogId<u64>>,
    membership: StoredMembership<u64, BasicNode>,
    metadata: Metadata,
    snapshot: Option<SnapshotMeta<u64, BasicNode>>,
}
/// Retained consensus state, by count and on-disk bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    pub logs: usize,
    pub snapshots: usize,
    pub disk_bytes: u64,
    pub allocated_bytes: u64,
    pub state: crate::Retention,
    /// Domain controls, sessions and tenant records charged to the state budget.
    pub state_bytes: usize,
    pub retry_records: usize,
}
#[derive(Clone)]
pub struct Store(Arc<Inner>);
struct Inner {
    root: PathBuf,
    image: Mutex<Image>,
    /// This voter's ID, for its fence.
    node: std::sync::OnceLock<u64>,
    _lock: File,
}
fn io_error(error: impl std::error::Error + 'static) -> StorageError<u64> {
    StorageIOError::write_state_machine(&error).into()
}
fn other(message: &str) -> StorageError<u64> {
    io_error(std::io::Error::other(message.to_string()))
}
pub(crate) fn crash(stage: &str) {
    if std::env::var("ROCKY_COORDINATOR_CRASH").as_deref() == Ok(stage) {
        std::process::abort();
    }
    if let Ok(path) = std::env::var("ROCKY_COORDINATOR_FAULT_FILE")
        && std::fs::read_to_string(&path).is_ok_and(|value| value == stage)
    {
        let _ = std::fs::remove_file(path);
        std::process::abort();
    }
}
fn sync_directory(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}
/// Syncs the directory containing `path`.
fn sync_dir(path: &Path) -> std::io::Result<()> {
    sync_directory(
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )
}
/// Writes and syncs `data` beside `path`, then renames it into place.
fn write_synced(path: &Path, data: &[u8], stage: &str) -> std::io::Result<()> {
    let mut name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("missing file name"))?
        .to_os_string();
    name.push(".pending");
    let temporary = path.with_file_name(name);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(data)?;
    crash(&format!("{stage}.before_sync"));
    file.sync_all()?;
    crash(&format!("{stage}.before_rename"));
    std::fs::rename(&temporary, path)
}
pub(crate) fn atomic_write(path: &Path, data: &[u8], stage: &str) -> std::io::Result<()> {
    write_synced(path, data, stage)?;
    sync_dir(path)?;
    crash(&format!("{stage}.after_sync"));
    Ok(())
}
fn seal<T: Serialize>(label: &str, value: &T) -> Result<Vec<u8>, CoordinationError> {
    let payload = encode(value)?;
    let data = encode(&(commitment(label, &payload), Encoded(payload)))?;
    if data.len() > MAX_STORE {
        return Err(CoordinationError::Backpressure);
    }
    Ok(data)
}
fn unseal<T: Serialize + DeserializeOwned>(
    label: &str,
    data: &[u8],
) -> Result<T, CoordinationError> {
    let (hash, payload): (Commitment, Encoded) = decode(data)?;
    if hash != commitment(label, &payload.0) {
        return Err(CoordinationError::Corrupt);
    }
    decode(&payload.0)
}
fn read(path: &Path) -> Result<Vec<u8>, CoordinationError> {
    std::fs::read(path).map_err(|_| CoordinationError::Corrupt)
}
/// Numbered files of a directory in ascending order; interrupted writes are removed.
fn numbered(dir: &Path) -> Result<Vec<(u64, PathBuf)>, CoordinationError> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|_| CoordinationError::Corrupt)? {
        let path = entry.map_err(|_| CoordinationError::Corrupt)?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(CoordinationError::Corrupt)?;
        if name.ends_with(".pending") {
            std::fs::remove_file(&path).map_err(|_| CoordinationError::Corrupt)?;
            continue;
        }
        let index = name
            .strip_suffix(".cbor")
            .and_then(|n| n.parse::<u64>().ok())
            .ok_or(CoordinationError::Corrupt)?;
        files.push((index, path));
    }
    files.sort();
    Ok(files)
}
/// Copies a store's provisioning marker, hard state, logs and snapshots.
fn copy_store(from: &Path, to: &Path) -> Result<(), CoordinationError> {
    let fail = |_| CoordinationError::Corrupt;
    for dir in ["", "log", "snapshots"] {
        std::fs::create_dir_all(to.join(dir)).map_err(fail)?;
    }
    for name in ["PROVISIONED", "state.cbor"] {
        std::fs::copy(from.join(name), to.join(name)).map_err(fail)?;
    }
    for dir in ["log", "snapshots"] {
        for (index, source) in numbered(&from.join(dir))? {
            std::fs::copy(&source, to.join(dir).join(file_name(index))).map_err(fail)?;
        }
        File::open(to.join(dir))
            .and_then(|d| d.sync_all())
            .map_err(fail)?;
    }
    for name in ["PROVISIONED", "state.cbor"] {
        File::open(to.join(name))
            .and_then(|f| f.sync_all())
            .map_err(fail)?;
    }
    sync_directory(to).map_err(fail)
}
fn file_name(index: u64) -> String {
    format!("{index:020}.cbor")
}
const STATE_LABEL: &str = "rocky.raft-state.v2";
const LOG_LABEL: &str = "rocky.raft-log.v2";
const SNAPSHOT_LABEL: &str = "rocky.raft-snapshot.v2";
fn snapshot_index(meta: &SnapshotMeta<u64, BasicNode>) -> u64 {
    meta.last_log_id.map_or(0, |id| id.index + 1)
}
fn check_store_format(path: &Path, backup: bool) -> Result<(), CoordinationError> {
    if path.exists() {
        for entry in std::fs::read_dir(path).map_err(|_| CoordinationError::Corrupt)? {
            let entry = entry.map_err(|_| CoordinationError::Corrupt)?;
            if ![
                "LOCK",
                "PROVISIONED",
                "state.cbor",
                "log",
                "snapshots",
                FENCE,
                "state.cbor.pending",
                "PROVISIONED.pending",
                "RESTORED.pending",
            ]
            .iter()
            .any(|name| entry.file_name() == *name)
                && !(backup && entry.file_name() == BACKUP)
            {
                return Err(CoordinationError::UnsupportedFormat);
            }
        }
        let state = path.join("state.cbor");
        if state.exists() {
            let data = read(&state)?;
            let (hash, payload): (Commitment, Encoded) = decode(&data)?;
            if hash != commitment(STATE_LABEL, &payload.0) {
                return Err(CoordinationError::Corrupt);
            }
            #[derive(Deserialize)]
            struct Header {
                format: u32,
            }
            let header: Header = p2panda_core::cbor::decode_cbor(payload.0.as_slice())
                .map_err(|_| CoordinationError::Corrupt)?;
            if header.format != FORMAT {
                return Err(CoordinationError::UnsupportedFormat);
            }
            for directory in ["snapshots", "log"] {
                for entry in std::fs::read_dir(path.join(directory))
                    .map_err(|_| CoordinationError::Corrupt)?
                {
                    let entry = entry.map_err(|_| CoordinationError::Corrupt)?;
                    if !entry.file_name().to_string_lossy().ends_with(".cbor") {
                        continue;
                    }
                    let bytes = read(&entry.path())?;
                    if directory == "snapshots" {
                        if let Ok(snapshot) = unseal::<SnapshotFile>(SNAPSHOT_LABEL, &bytes)
                            && snapshot.format != FORMAT
                        {
                            return Err(CoordinationError::UnsupportedFormat);
                        }
                    } else {
                        let _: Entry<Types> = unseal(LOG_LABEL, &bytes)?;
                    }
                }
            }
        }
    }
    Ok(())
}
impl Store {
    pub fn open(
        path: &Path,
        genesis: CertifiedGenesis,
        provision: bool,
        limits: crate::Limits,
    ) -> Result<Self, CoordinationError> {
        check_store_format(path, false)?;
        std::fs::create_dir_all(path).map_err(|_| CoordinationError::Corrupt)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.join("LOCK"))
            .map_err(|_| CoordinationError::Corrupt)?;
        lock.try_lock_exclusive()
            .map_err(|_| CoordinationError::Denied)?;
        let identity = genesis.id()?;
        let marker_path = path.join("PROVISIONED");
        let (logs_dir, snapshots_dir) = (path.join("log"), path.join("snapshots"));
        let state_path = path.join("state.cbor");
        let image = if state_path.exists() {
            let hard: Hard = unseal(STATE_LABEL, &read(&state_path)?)?;
            if hard.format != FORMAT {
                return Err(CoordinationError::UnsupportedFormat);
            }
            if hard.identity != identity {
                return Err(CoordinationError::Configuration);
            }
            if std::fs::read(&marker_path).map_err(|_| CoordinationError::CoordinationStateLost)?
                != identity
            {
                return Err(CoordinationError::Configuration);
            }
            let mut image = Self::load(hard, genesis, &logs_dir, &snapshots_dir)?;
            let fence = path.join(FENCE);
            if fence.exists() {
                image.fence = Some(unseal(FENCE_LABEL, &read(&fence)?)?);
            }
            image
        } else {
            let leftovers = [&logs_dir, &snapshots_dir].into_iter().any(|dir| {
                std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
            });
            if !provision || marker_path.exists() || leftovers {
                return Err(CoordinationError::CoordinationStateLost);
            }
            let hard = Hard {
                format: FORMAT,
                identity,
                limits,
                vote: None,
                committed: None,
                purged: None,
            };
            let image = Image {
                metadata: Metadata::with_limits(genesis, limits)?,
                fence: None,
                hard,
                pending_purge: None,
                logs: BTreeMap::new(),
                applied: None,
                membership: StoredMembership::default(),
                snapshot: None,
            };
            for dir in [&logs_dir, &snapshots_dir] {
                std::fs::create_dir_all(dir).map_err(|_| CoordinationError::Corrupt)?;
            }
            // The marker must outlive a lost image; interruption during provisioning fails closed.
            atomic_write(&marker_path, &identity, "provision")
                .map_err(|_| CoordinationError::Corrupt)?;
            atomic_write(&state_path, &seal(STATE_LABEL, &image.hard)?, "provision")
                .map_err(|_| CoordinationError::Corrupt)?;
            image
        };
        Ok(Self(Arc::new(Inner {
            root: path.to_path_buf(),
            image: Mutex::new(image),
            node: std::sync::OnceLock::new(),
            _lock: lock,
        })))
    }
    fn load(
        hard: Hard,
        genesis: CertifiedGenesis,
        logs_dir: &Path,
        snapshots_dir: &Path,
    ) -> Result<Image, CoordinationError> {
        let purged = hard.purged.map(|id| id.index);
        // The newest readable snapshot that covers every purged entry.
        let mut snapshot = None;
        for (_, path) in numbered(snapshots_dir)?.into_iter().rev() {
            let Ok(file) = unseal::<SnapshotFile>(SNAPSHOT_LABEL, &read(&path)?) else {
                continue;
            };
            if file.format != FORMAT || file.identity != hard.identity {
                return Err(CoordinationError::UnsupportedFormat);
            }
            if purged.is_none_or(|p| file.meta.last_log_id.is_some_and(|id| id.index >= p)) {
                snapshot = Some(file);
                break;
            }
        }
        if snapshot.is_none() && purged.is_some() {
            return Err(CoordinationError::Corrupt);
        }
        let (metadata, applied, membership, meta) = match snapshot {
            Some(file) => {
                let metadata: Metadata = decode(&file.metadata.0)?;
                if metadata.genesis.id()? != hard.identity {
                    return Err(CoordinationError::Configuration);
                }
                (
                    metadata,
                    file.meta.last_log_id,
                    file.meta.last_membership.clone(),
                    Some(file.meta),
                )
            }
            None => (
                Metadata::with_limits(genesis, hard.limits)?,
                None,
                StoredMembership::default(),
                None,
            ),
        };
        let mut logs = BTreeMap::new();
        for (index, path) in numbered(logs_dir)? {
            if purged.is_some_and(|p| index <= p) {
                // A purge interrupted after its durable record.
                std::fs::remove_file(&path).map_err(|_| CoordinationError::Corrupt)?;
                continue;
            }
            let entry: Entry<Types> = unseal(LOG_LABEL, &read(&path)?)?;
            if entry.log_id.index != index
                || logs
                    .last_key_value()
                    .is_some_and(|(last, _): (&u64, _)| last + 1 != index)
            {
                return Err(CoordinationError::Corrupt);
            }
            logs.insert(index, entry);
        }
        if let Some(committed) = hard.committed
            && applied.is_none_or(|id| id.index < committed.index)
        {
            let first = applied.map_or(0, |id| id.index + 1);
            let covered = logs.contains_key(&first) || (applied.is_none() && logs.contains_key(&1));
            if !covered || !logs.contains_key(&committed.index) {
                return Err(CoordinationError::Corrupt);
            }
        }
        Ok(Image {
            hard,
            fence: None,
            pending_purge: None,
            logs,
            applied,
            membership,
            metadata,
            snapshot: meta,
        })
    }
    #[allow(clippy::result_large_err)] // OpenRaft defines the storage error type.
    fn persist_hard(&self, hard: &Hard, stage: &str) -> Result<(), StorageError<u64>> {
        let data = seal(STATE_LABEL, hard).map_err(io_error)?;
        atomic_write(&self.0.root.join("state.cbor"), &data, stage).map_err(io_error)
    }
    fn logs_dir(&self) -> PathBuf {
        self.0.root.join("log")
    }
    fn snapshots_dir(&self) -> PathBuf {
        self.0.root.join("snapshots")
    }
    /// Durably records the purge, then removes the purged entry files.
    #[allow(clippy::result_large_err)] // OpenRaft defines the storage error type.
    fn purge_files(&self, image: &mut Image, log_id: LogId<u64>) -> Result<(), StorageError<u64>> {
        let mut hard = image.hard.clone();
        hard.purged = Some(log_id);
        self.persist_hard(&hard, "purge")?;
        image.hard = hard;
        image.pending_purge = None;
        let doomed: Vec<_> = image.logs.range(..=log_id.index).map(|(i, _)| *i).collect();
        for index in doomed {
            image.logs.remove(&index);
            std::fs::remove_file(self.logs_dir().join(file_name(index))).map_err(io_error)?;
        }
        sync_directory(&self.logs_dir()).map_err(io_error)
    }
    #[allow(clippy::result_large_err)] // OpenRaft defines the storage error type.
    fn finish_purge(&self, image: &mut Image) -> Result<(), StorageError<u64>> {
        if let Some(purge) = image.pending_purge
            && image
                .snapshot
                .as_ref()
                .and_then(|meta| meta.last_log_id)
                .is_some_and(|id| id.index >= purge.index)
        {
            self.purge_files(image, purge)?;
        }
        Ok(())
    }
    /// Durably writes a snapshot of `metadata`, keeping the newest two.
    #[allow(clippy::result_large_err)] // OpenRaft defines the storage error type.
    fn write_snapshot(
        &self,
        identity: Commitment,
        meta: &SnapshotMeta<u64, BasicNode>,
        metadata: Vec<u8>,
        stage: &str,
    ) -> Result<(), StorageError<u64>> {
        let file = SnapshotFile {
            format: FORMAT,
            identity,
            meta: meta.clone(),
            metadata: Encoded(metadata),
        };
        let data = seal(SNAPSHOT_LABEL, &file).map_err(io_error)?;
        let dir = self.snapshots_dir();
        atomic_write(&dir.join(file_name(snapshot_index(meta))), &data, stage).map_err(io_error)?;
        let files = numbered(&dir).map_err(io_error)?;
        for (_, path) in files.iter().rev().skip(RETAINED_SNAPSHOTS) {
            std::fs::remove_file(path).map_err(io_error)?;
        }
        Ok(())
    }
    pub async fn preflight(&self, request: &Request) -> Result<(), CoordinationError> {
        self.0.image.lock().await.metadata.preflight(request)
    }
    pub async fn read(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        self.0.image.lock().await.metadata.read(request)
    }
    pub(crate) async fn pending_logs(
        &self,
    ) -> Vec<(rocky_core::Scope, crate::coordinated::Pending)> {
        self.0.image.lock().await.metadata.pending_logs()
    }
    pub(crate) async fn log_head(
        &self,
        scope: rocky_core::Scope,
    ) -> Result<crate::coordinated::Head, CoordinationError> {
        self.0.image.lock().await.metadata.log_head(scope)
    }
    /// The committed authority time.
    pub async fn time(&self) -> u64 {
        self.0.image.lock().await.metadata.time()
    }
    /// Unauthorized requests share the unregistered lane, never the target's.
    pub async fn lane(&self, request: &Request) -> Option<p2panda_core::PublicKey> {
        let image = self.0.image.lock().await;
        image.metadata.preflight(request).ok()?;
        image.metadata.tenant_lane(request)
    }
    pub async fn tenants(&self) -> usize {
        self.0.image.lock().await.metadata.tenants()
    }
    pub fn set_node(&self, node: u64) {
        let _ = self.0.node.set(node);
    }
    pub async fn fence(&self) -> Option<Fence> {
        self.0.image.lock().await.fence
    }
    /// Removes the fence of a re-added voter.
    pub async fn lift_fence(&self) -> Result<(), CoordinationError> {
        let mut image = self.0.image.lock().await;
        let path = self.0.root.join(FENCE);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|_| CoordinationError::Corrupt)?;
            sync_directory(&self.0.root).map_err(|_| CoordinationError::Corrupt)?;
        }
        image.fence = None;
        Ok(())
    }
    /// Records that a membership beyond the fence names this voter a learner.
    #[allow(clippy::result_large_err)] // OpenRaft defines the storage error type.
    fn observe_membership(
        &self,
        image: &mut Image,
        membership: &StoredMembership<u64, BasicNode>,
    ) -> Result<(), StorageError<u64>> {
        let (Some(node), Some(mut fence)) = (self.0.node.get(), image.fence) else {
            return Ok(());
        };
        let config = membership.membership();
        if fence.learner
            || membership.log_id().is_none_or(|id| id.index <= fence.index)
            || config.get_node(node).is_none()
            || config.voter_ids().any(|id| id == *node)
        {
            return Ok(());
        }
        fence.learner = true;
        let data = seal(FENCE_LABEL, &fence).map_err(io_error)?;
        atomic_write(&self.0.root.join(FENCE), &data, "fence").map_err(io_error)?;
        image.fence = Some(fence);
        Ok(())
    }
    /// Copies the files of the stopped voter at `path` into the new
    /// directory `out`. The store lock refuses a running voter.
    pub fn backup(
        path: &Path,
        genesis: CertifiedGenesis,
        node: u64,
        out: &Path,
    ) -> Result<Backup, CoordinationError> {
        let limits = genesis.limits();
        let store = Self::open(path, genesis, false, limits)?;
        let image = store
            .0
            .image
            .try_lock()
            .map_err(|_| CoordinationError::Corrupt)?;
        let index = [
            image.logs.last_key_value().map(|(index, _)| *index),
            image
                .snapshot
                .as_ref()
                .and_then(|meta| meta.last_log_id)
                .map(|id| id.index),
            image.applied.map(|id| id.index),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0);
        let backup = Backup {
            identity: image.hard.identity,
            node,
            index,
        };
        if out.exists() {
            return Err(CoordinationError::Configuration);
        }
        copy_store(path, out)?;
        atomic_write(&out.join(BACKUP), &seal(BACKUP_LABEL, &backup)?, "backup")
            .map_err(|_| CoordinationError::Corrupt)?;
        Ok(backup)
    }
    /// Restores a backup of voter `node` into the empty `path`, fenced.
    pub fn restore(
        backup: &Path,
        genesis: &CertifiedGenesis,
        node: u64,
        path: &Path,
    ) -> Result<Backup, CoordinationError> {
        let manifest: Backup = unseal(BACKUP_LABEL, &read(&backup.join(BACKUP))?)?;
        if manifest.identity != genesis.id()? || manifest.node != node {
            return Err(CoordinationError::Configuration);
        }
        if std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some()) {
            return Err(CoordinationError::Configuration);
        }
        check_store_format(backup, true)?;
        copy_store(backup, path)?;
        // The backup's own leadership is stale: its log may lack entries it
        // committed later, so it restarts as a follower of its last term.
        let state = path.join("state.cbor");
        let mut hard: Hard = unseal(STATE_LABEL, &read(&state)?)?;
        if hard.identity != manifest.identity {
            return Err(CoordinationError::Configuration);
        }
        hard.vote = hard.vote.map(|vote| Vote {
            committed: false,
            ..vote
        });
        atomic_write(&state, &seal(STATE_LABEL, &hard)?, "restore")
            .map_err(|_| CoordinationError::Corrupt)?;
        let fence = Fence {
            index: manifest.index,
            learner: false,
        };
        atomic_write(&path.join(FENCE), &seal(FENCE_LABEL, &fence)?, "restore")
            .map_err(|_| CoordinationError::Corrupt)?;
        Ok(manifest)
    }
    pub async fn desired_voters(&self) -> std::collections::BTreeSet<u64> {
        self.0.image.lock().await.metadata.desired_voters.clone()
    }
    /// Retained state and the bytes it occupies on disk.
    pub async fn footprint(&self) -> Result<Footprint, CoordinationError> {
        let image = self.0.image.lock().await;
        let mut disk_bytes = 0;
        let mut allocated_bytes = 0;
        let mut snapshots = 0;
        for dir in [self.0.root.clone(), self.logs_dir(), self.snapshots_dir()] {
            for entry in std::fs::read_dir(&dir).map_err(|_| CoordinationError::Corrupt)? {
                let metadata = entry
                    .map_err(|_| CoordinationError::Corrupt)?
                    .metadata()
                    .map_err(|_| CoordinationError::Corrupt)?;
                if metadata.is_file() {
                    disk_bytes += metadata.len();
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        allocated_bytes += metadata.blocks() * 512;
                    }
                    #[cfg(not(unix))]
                    {
                        allocated_bytes += metadata.len();
                    }
                    snapshots += usize::from(dir == self.snapshots_dir());
                }
            }
        }
        Ok(Footprint {
            logs: image.logs.len(),
            snapshots,
            disk_bytes,
            allocated_bytes,
            state: image.metadata.retention(),
            state_bytes: image.metadata.total_used()?,
            retry_records: image.metadata.retry_records(),
        })
    }
}
impl RaftLogReader<Types> for Store {
    async fn try_get_log_entries<R: RangeBounds<u64> + Clone + Send>(
        &mut self,
        range: R,
    ) -> Result<Vec<Entry<Types>>, StorageError<u64>> {
        let image = self.0.image.lock().await;
        let purged = image.pending_purge.or(image.hard.purged);
        Ok(image
            .logs
            .range(range)
            .filter(|(index, _)| purged.is_none_or(|id| **index > id.index))
            .map(|(_, e)| e.clone())
            .collect())
    }
}
impl RaftLogStorage<Types> for Store {
    type LogReader = Self;
    async fn get_log_reader(&mut self) -> Self {
        self.clone()
    }
    async fn get_log_state(&mut self) -> Result<LogState<Types>, StorageError<u64>> {
        let image = self.0.image.lock().await;
        let purged = image.pending_purge.or(image.hard.purged);
        Ok(LogState {
            last_purged_log_id: purged,
            last_log_id: image
                .logs
                .last_key_value()
                .map(|(_, e)| e.log_id)
                .filter(|id| purged.is_none_or(|p| id.index > p.index))
                .or(purged),
        })
    }
    async fn save_vote(&mut self, vote: &Vote<u64>) -> Result<(), StorageError<u64>> {
        let mut image = self.0.image.lock().await;
        let mut hard = image.hard.clone();
        hard.vote = Some(*vote);
        self.persist_hard(&hard, "vote")?;
        image.hard = hard;
        Ok(())
    }
    async fn read_vote(&mut self) -> Result<Option<Vote<u64>>, StorageError<u64>> {
        Ok(self.0.image.lock().await.hard.vote)
    }
    async fn save_committed(
        &mut self,
        committed: Option<LogId<u64>>,
    ) -> Result<(), StorageError<u64>> {
        let mut image = self.0.image.lock().await;
        let mut hard = image.hard.clone();
        hard.committed = committed;
        self.persist_hard(&hard, "commit")?;
        image.hard = hard;
        Ok(())
    }
    async fn read_committed(&mut self) -> Result<Option<LogId<u64>>, StorageError<u64>> {
        Ok(self.0.image.lock().await.hard.committed)
    }
    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<Types>,
    ) -> Result<(), StorageError<u64>>
    where
        I: IntoIterator<Item = Entry<Types>> + Send,
        I::IntoIter: Send,
    {
        let mut image = self.0.image.lock().await;
        let dir = self.logs_dir();
        let mut written = Vec::new();
        for entry in entries {
            if !image.logs.contains_key(&entry.log_id.index)
                && image.logs.len() + written.len() >= MAX_LOGS
            {
                return Err(other("consensus log capacity exhausted"));
            }
            let data = seal(LOG_LABEL, &entry).map_err(io_error)?;
            if data.len() > MAX_LOG_BYTES {
                return Err(other("consensus entry capacity exhausted"));
            }
            write_synced(&dir.join(file_name(entry.log_id.index)), &data, "log")
                .map_err(io_error)?;
            written.push(entry);
        }
        sync_directory(&dir).map_err(io_error)?;
        crash("log.after_sync");
        for entry in written {
            image.logs.insert(entry.log_id.index, entry);
        }
        callback.log_io_completed(Ok(()));
        Ok(())
    }
    async fn truncate(&mut self, log_id: LogId<u64>) -> Result<(), StorageError<u64>> {
        let mut image = self.0.image.lock().await;
        // Highest first, so an interruption leaves a contiguous prefix.
        let doomed: Vec<_> = image
            .logs
            .range(log_id.index..)
            .map(|(i, _)| *i)
            .rev()
            .collect();
        for index in doomed {
            std::fs::remove_file(self.logs_dir().join(file_name(index))).map_err(io_error)?;
            image.logs.remove(&index);
        }
        sync_directory(&self.logs_dir()).map_err(io_error)
    }
    async fn purge(&mut self, log_id: LogId<u64>) -> Result<(), StorageError<u64>> {
        let mut image = self.0.image.lock().await;
        if image
            .snapshot
            .as_ref()
            .and_then(|meta| meta.last_log_id)
            .is_none_or(|snapshot| snapshot.index < log_id.index)
        {
            // Raft can purge before installing its incoming snapshot. Keep recovery
            // entries until that snapshot is durable.
            image.pending_purge = Some(log_id);
            return Ok(());
        }
        self.purge_files(&mut image, log_id)
    }
}
impl RaftStateMachine<Types> for Store {
    type SnapshotBuilder = Self;
    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<u64>>, StoredMembership<u64, BasicNode>), StorageError<u64>> {
        let image = self.0.image.lock().await;
        Ok((image.applied, image.membership.clone()))
    }
    async fn apply<I>(
        &mut self,
        entries: I,
    ) -> Result<Vec<Result<Outcome, CoordinationError>>, StorageError<u64>>
    where
        I: IntoIterator<Item = Entry<Types>> + Send,
        I::IntoIter: Send,
    {
        let mut image = self.0.image.lock().await;
        let mut replies = Vec::new();
        for entry in entries {
            let reply = match entry.payload {
                EntryPayload::Blank => Err(CoordinationError::Unavailable),
                EntryPayload::Membership(membership) => {
                    image.membership = StoredMembership::new(Some(entry.log_id), membership);
                    let membership = image.membership.clone();
                    self.observe_membership(&mut image, &membership)?;
                    Ok(Outcome::Membership(image.metadata.desired_voters.clone()))
                }
                EntryPayload::Normal(proposal) => {
                    image.metadata.apply_at(proposal.time, &proposal.request)
                }
            };
            image.applied = Some(entry.log_id);
            replies.push(reply);
        }
        // Applied state is durable through committed entries and snapshots.
        crash("apply.after_apply");
        Ok(replies)
    }
    async fn get_snapshot_builder(&mut self) -> Self {
        self.clone()
    }
    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<u64>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }
    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<u64, BasicNode>,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<u64>> {
        let data = snapshot.into_inner();
        let metadata: Metadata = decode(&data).map_err(io_error)?;
        let mut image = self.0.image.lock().await;
        if metadata.genesis.id().map_err(io_error)? != image.hard.identity {
            return Err(other("foreign snapshot"));
        }
        self.write_snapshot(image.hard.identity, meta, data, "install_snapshot")?;
        image.metadata = metadata;
        image.applied = meta.last_log_id;
        image.membership = meta.last_membership.clone();
        image.snapshot = Some(meta.clone());
        self.observe_membership(&mut image, &meta.last_membership)?;
        self.finish_purge(&mut image)
    }
    async fn get_current_snapshot(&mut self) -> Result<Option<Snapshot<Types>>, StorageError<u64>> {
        let image = self.0.image.lock().await;
        let Some(meta) = &image.snapshot else {
            return Ok(None);
        };
        let path = self.snapshots_dir().join(file_name(snapshot_index(meta)));
        let file: SnapshotFile =
            unseal(SNAPSHOT_LABEL, &read(&path).map_err(io_error)?).map_err(io_error)?;
        if file.meta != *meta {
            return Err(other("snapshot changed"));
        }
        Ok(Some(Snapshot {
            meta: file.meta,
            snapshot: Box::new(Cursor::new(file.metadata.0)),
        }))
    }
}
impl RaftSnapshotBuilder<Types> for Store {
    async fn build_snapshot(&mut self) -> Result<Snapshot<Types>, StorageError<u64>> {
        let mut image = self.0.image.lock().await;
        let data = encode(&image.metadata).map_err(io_error)?;
        let meta = SnapshotMeta {
            last_log_id: image.applied,
            last_membership: image.membership.clone(),
            snapshot_id: format!("{:?}", image.applied),
        };
        self.write_snapshot(image.hard.identity, &meta, data.clone(), "snapshot")?;
        image.snapshot = Some(meta.clone());
        self.finish_purge(&mut image)?;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }
}

#[cfg(test)]
#[path = "raft_store/probe.rs"]
mod probe;
#[cfg(test)]
#[path = "raft_store/tests.rs"]
mod tests;
