use openraft::{
    storage::{LogFlushed, RaftLogStorage, RaftStateMachine},
    *,
};
use rocky_coordinator::CoordinationError;
use rocky_coordinator::{certified::*, protocol::Request, service::Outcome, state::Metadata};
use rocky_core::{Commitment, commitment};
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub time: u64,
    pub request: Request,
}

openraft::declare_raft_types!(pub Types: D = Proposal, R = Result<Outcome, CoordinationError>);
pub type Consensus = Raft<Types>;
const FORMAT: u32 = 14;
const RETAINED_SNAPSHOTS: usize = 2;
const MAX_LOGS: usize = 2048;
const MAX_LOG_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Hard {
    format: u32,
    identity: Commitment,
    limits: rocky_coordinator::Limits,
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fence {
    pub index: u64,
    pub learner: bool,
}
const FENCE: &str = "RESTORED";
const FENCE_LABEL: &str = "rocky.voter-fence.v1";
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Backup {
    pub identity: Commitment,
    pub node: u64,
    pub index: u64,
}
const BACKUP: &str = "BACKUP";
const PROVISIONED: &str = "PROVISIONED";
const BACKUP_LABEL: &str = "rocky.voter-backup.v1";
#[derive(Clone)]
struct Image {
    hard: Hard,
    fence: Option<Fence>,
    purge_awaiting_durable_snapshot: Option<LogId<u64>>,
    logs: Arc<BTreeMap<u64, Arc<Entry<Types>>>>,
    snapshot: Option<SnapshotMeta<u64, BasicNode>>,
}
struct Machine {
    applied: Option<LogId<u64>>,
    membership: StoredMembership<u64, BasicNode>,
    metadata: Metadata,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    pub logs: usize,
    pub snapshots: usize,
    pub disk_bytes: u64,
    pub allocated_bytes: u64,
    pub state: rocky_coordinator::Retention,
    pub state_bytes: usize,
    pub retry_records: usize,
}
#[derive(Clone)]
pub struct Store(Arc<Inner>);
#[cfg(test)]
struct SyncGate {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
struct Inner {
    root: PathBuf,
    genesis_id: Commitment,
    image: Mutex<Image>,
    machine: tokio::sync::RwLock<Machine>,
    durable_mutations: Arc<Mutex<()>>,
    node: std::sync::OnceLock<u64>,
    _lock: StoreLock,
    #[cfg(test)]
    sync_gate: std::sync::Mutex<Option<SyncGate>>,
    #[cfg(test)]
    copied_entries: std::sync::atomic::AtomicUsize,
}
struct StoreLock(File);
impl StoreLock {
    fn acquire(root: &Path) -> Result<Self, CoordinationError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("LOCK"))
            .map_err(|_| CoordinationError::Corrupt)?;
        file.try_lock().map_err(|_| CoordinationError::Denied)?;
        Ok(Self(file))
    }
}
impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
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
fn sync_dir(path: &Path) -> std::io::Result<()> {
    sync_directory(
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )
}
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
fn copy_store_without_state(from: &Path, to: &Path) -> Result<(), CoordinationError> {
    let fail = |_| CoordinationError::Corrupt;
    let copy_synced = |source: &Path, target: &Path| {
        std::fs::copy(source, target)
            .and_then(|_| File::open(target))
            .and_then(|f| f.sync_all())
            .map_err(fail)
    };
    std::fs::create_dir_all(to).map_err(fail)?;
    copy_synced(&from.join(PROVISIONED), &to.join(PROVISIONED))?;
    sync_directory(to).map_err(fail)?;
    for dir in ["log", "snapshots"] {
        std::fs::create_dir_all(to.join(dir)).map_err(fail)?;
        for (index, source) in numbered(&from.join(dir))? {
            copy_synced(&source, &to.join(dir).join(file_name(index)))?;
        }
        File::open(to.join(dir))
            .and_then(|d| d.sync_all())
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
#[expect(
    clippy::result_large_err,
    reason = "OpenRaft fixes the storage error type."
)]
impl Store {
    pub fn open(
        path: &Path,
        genesis: CertifiedGenesis,
        provision: bool,
        provisioning_limits: rocky_coordinator::Limits,
    ) -> Result<Self, CoordinationError> {
        check_store_format(path, false)?;
        std::fs::create_dir_all(path).map_err(|_| CoordinationError::Corrupt)?;
        let lock = StoreLock::acquire(path)?;
        let identity = genesis.id()?;
        let marker_path = path.join(PROVISIONED);
        let (logs_dir, snapshots_dir) = (path.join("log"), path.join("snapshots"));
        let state_path = path.join("state.cbor");
        let (image, machine) = if state_path.exists() {
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
            let (mut image, machine) = Self::load(hard, genesis, &logs_dir, &snapshots_dir)?;
            let fence = path.join(FENCE);
            if fence.exists() {
                image.fence = Some(unseal(FENCE_LABEL, &read(&fence)?)?);
            }
            (image, machine)
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
                limits: provisioning_limits,
                vote: None,
                committed: None,
                purged: None,
            };
            let machine = Machine {
                metadata: Metadata::with_limits(genesis, provisioning_limits)?,
                applied: None,
                membership: StoredMembership::default(),
            };
            let image = Image {
                fence: None,
                hard,
                purge_awaiting_durable_snapshot: None,
                logs: Arc::default(),
                snapshot: None,
            };
            for dir in [&logs_dir, &snapshots_dir] {
                std::fs::create_dir_all(dir).map_err(|_| CoordinationError::Corrupt)?;
            }
            atomic_write(&marker_path, &identity, "provision")
                .map_err(|_| CoordinationError::Corrupt)?;
            atomic_write(&state_path, &seal(STATE_LABEL, &image.hard)?, "provision")
                .map_err(|_| CoordinationError::Corrupt)?;
            (image, machine)
        };
        Ok(Self(Arc::new(Inner {
            root: path.to_path_buf(),
            genesis_id: identity,
            image: Mutex::new(image),
            machine: tokio::sync::RwLock::new(machine),
            durable_mutations: Arc::new(Mutex::new(())),
            node: std::sync::OnceLock::new(),
            _lock: lock,
            #[cfg(test)]
            sync_gate: std::sync::Mutex::new(None),
            #[cfg(test)]
            copied_entries: Default::default(),
        })))
    }
    fn load(
        hard: Hard,
        genesis: CertifiedGenesis,
        logs_dir: &Path,
        snapshots_dir: &Path,
    ) -> Result<(Image, Machine), CoordinationError> {
        let purged = hard.purged.map(|id| id.index);
        let snapshot = newest_snapshot_covering(snapshots_dir, &hard)?;
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
        let mut next_index = purged.map_or(0, |p| p + 1);
        for (index, path) in numbered(logs_dir)? {
            let left_by_interrupted_purge = purged.is_some_and(|p| index <= p);
            if left_by_interrupted_purge {
                std::fs::remove_file(&path).map_err(|_| CoordinationError::Corrupt)?;
                continue;
            }
            let entry: Entry<Types> = unseal(LOG_LABEL, &read(&path)?)?;
            if entry.log_id.index != index || index != next_index {
                return Err(CoordinationError::Corrupt);
            }
            next_index = index + 1;
            logs.insert(index, Arc::new(entry));
        }
        if let Some(committed) = hard.committed
            && applied.is_none_or(|id| id.index < committed.index)
            && logs
                .get(&committed.index)
                .is_none_or(|entry| entry.log_id != committed)
        {
            return Err(CoordinationError::Corrupt);
        }
        Ok((
            Image {
                hard,
                fence: None,
                purge_awaiting_durable_snapshot: None,
                logs: Arc::new(logs),
                snapshot: meta,
            },
            Machine {
                applied,
                membership,
                metadata,
            },
        ))
    }
    #[expect(
        clippy::result_large_err,
        reason = "OpenRaft defines the storage error type."
    )]
    fn persist_hard(&self, hard: &Hard, stage: &str) -> Result<(), StorageError<u64>> {
        let data = seal(STATE_LABEL, hard).map_err(io_error)?;
        atomic_write(&self.0.root.join("state.cbor"), &data, stage).map_err(io_error)
    }
    async fn exclusive<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Self) -> Result<T, StorageError<u64>> + Send + 'static,
    ) -> Result<T, StorageError<u64>> {
        let io = self.0.durable_mutations.clone().lock_owned().await;
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _io = io;
            work(&store)
        })
        .await
        .map_err(io_error)?
    }
    async fn update<T: Send + 'static>(
        &self,
        mutate: impl FnOnce(&Self, &mut Image) -> Result<T, StorageError<u64>> + Send + 'static,
    ) -> Result<T, StorageError<u64>> {
        self.exclusive(|store| store.publish(mutate)).await
    }
    #[expect(
        clippy::result_large_err,
        reason = "OpenRaft defines the storage error type."
    )]
    fn publish<T>(
        &self,
        mutate: impl FnOnce(&Self, &mut Image) -> Result<T, StorageError<u64>>,
    ) -> Result<T, StorageError<u64>> {
        let mut image = self.0.image.blocking_lock().clone();
        let result = mutate(self, &mut image)?;
        let mut published = self.0.image.blocking_lock();
        #[cfg(test)]
        {
            let copied = image.logs.iter().filter(|(index, entry)| {
                published
                    .logs
                    .get(index)
                    .is_some_and(|held| !Arc::ptr_eq(held, entry))
            });
            self.0
                .copied_entries
                .fetch_add(copied.count(), std::sync::atomic::Ordering::Relaxed);
        }
        *published = image;
        Ok(result)
    }
    fn logs_dir(&self) -> PathBuf {
        self.0.root.join("log")
    }
    fn snapshots_dir(&self) -> PathBuf {
        self.0.root.join("snapshots")
    }
    #[expect(
        clippy::result_large_err,
        reason = "OpenRaft defines the storage error type."
    )]
    fn purge_files(&self, image: &mut Image, log_id: LogId<u64>) -> Result<(), StorageError<u64>> {
        let mut hard = image.hard.clone();
        hard.purged = Some(log_id);
        self.persist_hard(&hard, "purge.state")?;
        image.hard = hard;
        image.purge_awaiting_durable_snapshot = None;
        let doomed: Vec<_> = image.logs.range(..=log_id.index).map(|(i, _)| *i).collect();
        let logs = Arc::make_mut(&mut image.logs);
        for index in doomed {
            logs.remove(&index);
            std::fs::remove_file(self.logs_dir().join(file_name(index))).map_err(io_error)?;
            crash(&format!("purge.log.removed.{index}"));
        }
        sync_directory(&self.logs_dir()).map_err(io_error)?;
        crash("purge.log.after_sync");
        Ok(())
    }
    #[expect(
        clippy::result_large_err,
        reason = "OpenRaft defines the storage error type."
    )]
    fn finish_purge(&self, image: &mut Image) -> Result<(), StorageError<u64>> {
        if let Some(purge) = image.purge_awaiting_durable_snapshot
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
    #[expect(
        clippy::result_large_err,
        reason = "OpenRaft defines the storage error type."
    )]
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
        request.authenticate(self.0.genesis_id)?;
        request.fresh()?;
        let machine = self.0.machine.read().await;
        machine.metadata.authorize_authenticated(request)
    }
    pub async fn read(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        request.authenticate(self.0.genesis_id)?;
        let machine = self.0.machine.read().await;
        machine.metadata.read_authenticated(request)
    }
    pub async fn authority_time(&self) -> u64 {
        self.0.machine.read().await.metadata.time()
    }
    pub async fn lane(&self, request: &Request) -> Option<p2panda_core::PublicKey> {
        request.authenticate(self.0.genesis_id).ok()?;
        request.fresh().ok()?;
        let machine = self.0.machine.read().await;
        machine.metadata.authorize_authenticated(request).ok()?;
        machine.metadata.tenant_lane(request)
    }
    #[cfg(test)]
    pub(crate) async fn hold_machine(&self) -> impl Sized + '_ {
        self.0.machine.write().await
    }
    pub async fn tenants(&self) -> usize {
        self.0.machine.read().await.metadata.tenants()
    }
    pub fn set_node(&self, node: u64) {
        let _ = self.0.node.set(node);
    }
    pub async fn fence(&self) -> Option<Fence> {
        self.0.image.lock().await.fence
    }
    pub async fn lift_fence(&self) -> Result<(), CoordinationError> {
        self.update(|store, image| {
            let path = store.0.root.join(FENCE);
            if path.exists() {
                std::fs::remove_file(&path).map_err(io_error)?;
                sync_directory(&store.0.root).map_err(io_error)?;
            }
            image.fence = None;
            Ok(())
        })
        .await
        .map_err(|_| CoordinationError::Corrupt)
    }
    #[expect(
        clippy::result_large_err,
        reason = "OpenRaft defines the storage error type."
    )]
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
        let machine = store
            .0
            .machine
            .try_read()
            .map_err(|_| CoordinationError::Corrupt)?;
        let index = [
            image.logs.last_key_value().map(|(index, _)| *index),
            image
                .snapshot
                .as_ref()
                .and_then(|meta| meta.last_log_id)
                .map(|id| id.index),
            machine.applied.map(|id| id.index),
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
        copy_store_without_state(path, out)?;
        atomic_write(
            &out.join("state.cbor"),
            &read(&path.join("state.cbor"))?,
            "backup.state",
        )
        .map_err(|_| CoordinationError::Corrupt)?;
        atomic_write(&out.join(BACKUP), &seal(BACKUP_LABEL, &backup)?, "backup")
            .map_err(|_| CoordinationError::Corrupt)?;
        Ok(backup)
    }
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
        let mut hard: Hard = unseal(STATE_LABEL, &read(&backup.join("state.cbor"))?)?;
        if hard.identity != manifest.identity {
            return Err(CoordinationError::Configuration);
        }
        hard.vote = hard.vote.map(as_follower);
        let fence = Fence {
            index: manifest.index,
            learner: false,
        };
        copy_store_without_state(backup, path)?;
        atomic_write(
            &path.join(FENCE),
            &seal(FENCE_LABEL, &fence)?,
            "restore.fence",
        )
        .map_err(|_| CoordinationError::Corrupt)?;
        atomic_write(
            &path.join("state.cbor"),
            &seal(STATE_LABEL, &hard)?,
            "restore.state",
        )
        .map_err(|_| CoordinationError::Corrupt)?;
        Ok(manifest)
    }
    pub async fn desired_voters(&self) -> std::collections::BTreeSet<u64> {
        self.0.machine.read().await.metadata.desired_voters.clone()
    }
    pub async fn footprint(&self) -> Result<Footprint, CoordinationError> {
        self.exclusive(|store| {
            let logs = store.0.image.blocking_lock().logs.len();
            let mut disk_bytes = 0;
            let mut allocated_bytes = 0;
            let mut snapshots = 0;
            for dir in [
                store.0.root.clone(),
                store.logs_dir(),
                store.snapshots_dir(),
            ] {
                for entry in std::fs::read_dir(&dir).map_err(io_error)? {
                    let metadata = entry.map_err(io_error)?.metadata().map_err(io_error)?;
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
                        snapshots += usize::from(dir == store.snapshots_dir());
                    }
                }
            }
            let machine = store.0.machine.blocking_read();
            Ok(Footprint {
                logs,
                snapshots,
                disk_bytes,
                allocated_bytes,
                state: machine.metadata.retention(),
                state_bytes: machine.metadata.total_used().map_err(io_error)?,
                retry_records: machine.metadata.retry_records(),
            })
        })
        .await
        .map_err(|_| CoordinationError::Corrupt)
    }
}
impl RaftLogReader<Types> for Store {
    async fn try_get_log_entries<R: RangeBounds<u64> + Clone + Send>(
        &mut self,
        range: R,
    ) -> Result<Vec<Entry<Types>>, StorageError<u64>> {
        let image = self.0.image.lock().await;
        let purged = image.purge_awaiting_durable_snapshot.or(image.hard.purged);
        Ok(image
            .logs
            .range(range)
            .filter(|(index, _)| purged.is_none_or(|id| **index > id.index))
            .map(|(_, e)| (**e).clone())
            .collect())
    }
}
#[expect(
    clippy::result_large_err,
    reason = "OpenRaft fixes the storage error type."
)]
impl RaftLogStorage<Types> for Store {
    type LogReader = Self;
    async fn get_log_reader(&mut self) -> Self {
        self.clone()
    }
    async fn get_log_state(&mut self) -> Result<LogState<Types>, StorageError<u64>> {
        let image = self.0.image.lock().await;
        let purged = image.purge_awaiting_durable_snapshot.or(image.hard.purged);
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
        let vote = *vote;
        self.update(move |store, image| {
            let mut hard = image.hard.clone();
            hard.vote = Some(vote);
            store.persist_hard(&hard, "vote")?;
            image.hard = hard;
            Ok(())
        })
        .await
    }
    async fn read_vote(&mut self) -> Result<Option<Vote<u64>>, StorageError<u64>> {
        Ok(self.0.image.lock().await.hard.vote)
    }
    async fn save_committed(
        &mut self,
        committed: Option<LogId<u64>>,
    ) -> Result<(), StorageError<u64>> {
        self.update(move |store, image| {
            let mut hard = image.hard.clone();
            hard.committed = committed;
            store.persist_hard(&hard, "commit")?;
            image.hard = hard;
            Ok(())
        })
        .await
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
        let entries: Vec<_> = entries.into_iter().collect();
        let result = self
            .update(move |store, image| {
                #[cfg(test)]
                if let Some(gate) = store.0.sync_gate.lock().unwrap().take() {
                    let _ = gate.entered.send(());
                    let _ = gate.release.recv();
                }
                let dir = store.logs_dir();
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
                    let index = entry.log_id.index;
                    write_synced(&dir.join(file_name(index)), &data, "log").map_err(io_error)?;
                    crash(&format!("log.renamed.{index}"));
                    written.push(entry);
                }
                sync_directory(&dir).map_err(io_error)?;
                crash("log.after_sync");
                let logs = Arc::make_mut(&mut image.logs);
                for entry in written {
                    logs.insert(entry.log_id.index, Arc::new(entry));
                }
                Ok(())
            })
            .await;
        match result {
            Ok(()) => {
                callback.log_io_completed(Ok(()));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    async fn truncate(&mut self, log_id: LogId<u64>) -> Result<(), StorageError<u64>> {
        self.update(move |store, image| {
            let highest_first: Vec<_> = image
                .logs
                .range(log_id.index..)
                .map(|(i, _)| *i)
                .rev()
                .collect();
            for index in highest_first {
                std::fs::remove_file(store.logs_dir().join(file_name(index))).map_err(io_error)?;
                crash(&format!("truncate.log.removed.{index}"));
                Arc::make_mut(&mut image.logs).remove(&index);
            }
            sync_directory(&store.logs_dir()).map_err(io_error)?;
            crash("truncate.log.after_sync");
            Ok(())
        })
        .await
    }
    async fn purge(&mut self, log_id: LogId<u64>) -> Result<(), StorageError<u64>> {
        self.update(move |store, image| {
            let covered_by_durable_snapshot = image
                .snapshot
                .as_ref()
                .and_then(|meta| meta.last_log_id)
                .is_some_and(|snapshot| snapshot.index >= log_id.index);
            if !covered_by_durable_snapshot {
                image.purge_awaiting_durable_snapshot = Some(log_id);
                return Ok(());
            }
            store.purge_files(image, log_id)
        })
        .await
    }
}
#[expect(
    clippy::result_large_err,
    reason = "OpenRaft fixes the storage error type."
)]
impl RaftStateMachine<Types> for Store {
    type SnapshotBuilder = Self;
    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<u64>>, StoredMembership<u64, BasicNode>), StorageError<u64>> {
        let machine = self.0.machine.read().await;
        Ok((machine.applied, machine.membership.clone()))
    }
    async fn apply<I>(
        &mut self,
        entries: I,
    ) -> Result<Vec<Result<Outcome, CoordinationError>>, StorageError<u64>>
    where
        I: IntoIterator<Item = Entry<Types>> + Send,
        I::IntoIter: Send,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        self.exclusive(move |store| {
            let mut replies = Vec::new();
            for entry in entries {
                let reply = match entry.payload {
                    EntryPayload::Blank => {
                        store.0.machine.blocking_write().applied = Some(entry.log_id);
                        Err(CoordinationError::Unavailable)
                    }
                    EntryPayload::Membership(membership) => {
                        let membership = StoredMembership::new(Some(entry.log_id), membership);
                        store
                            .publish(|store, image| store.observe_membership(image, &membership))?;
                        let mut machine = store.0.machine.blocking_write();
                        machine.membership = membership;
                        machine.applied = Some(entry.log_id);
                        Ok(Outcome::Membership(machine.metadata.desired_voters.clone()))
                    }
                    EntryPayload::Normal(proposal) => {
                        let request = &proposal.request;
                        let authenticated = request.authenticate(store.0.genesis_id);
                        let mut machine = store.0.machine.blocking_write();
                        let reply = machine.metadata.apply_authenticated(
                            proposal.time,
                            request,
                            authenticated,
                        );
                        machine.applied = Some(entry.log_id);
                        reply
                    }
                };
                replies.push(reply);
            }
            crash("apply.after_apply");
            Ok(replies)
        })
        .await
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
        let meta = meta.clone();
        self.exclusive(move |store| {
            let data = snapshot.into_inner();
            let metadata: Metadata = decode(&data).map_err(io_error)?;
            if metadata.genesis.id().map_err(io_error)? != store.0.genesis_id {
                return Err(other("foreign snapshot"));
            }
            store.write_snapshot(store.0.genesis_id, &meta, data, "install_snapshot")?;
            store.publish(|store, image| {
                image.snapshot = Some(meta.clone());
                store.observe_membership(image, &meta.last_membership)?;
                store.finish_purge(image)
            })?;
            let mut machine = store.0.machine.blocking_write();
            *machine = Machine {
                applied: meta.last_log_id,
                membership: meta.last_membership.clone(),
                metadata,
            };
            Ok(())
        })
        .await
    }
    async fn get_current_snapshot(&mut self) -> Result<Option<Snapshot<Types>>, StorageError<u64>> {
        self.exclusive(|store| {
            let Some(meta) = store.0.image.blocking_lock().snapshot.clone() else {
                return Ok(None);
            };
            let path = store.snapshots_dir().join(file_name(snapshot_index(&meta)));
            let file: SnapshotFile =
                unseal(SNAPSHOT_LABEL, &read(&path).map_err(io_error)?).map_err(io_error)?;
            if file.meta != meta {
                return Err(other("snapshot changed"));
            }
            Ok(Some(Snapshot {
                meta: file.meta,
                snapshot: Box::new(Cursor::new(file.metadata.0)),
            }))
        })
        .await
    }
}
#[expect(
    clippy::result_large_err,
    reason = "OpenRaft fixes the storage error type."
)]
impl RaftSnapshotBuilder<Types> for Store {
    async fn build_snapshot(&mut self) -> Result<Snapshot<Types>, StorageError<u64>> {
        self.exclusive(move |store| {
            let (data, meta) = {
                let machine = store.0.machine.blocking_read();
                let data = encode(&machine.metadata).map_err(io_error)?;
                let meta = SnapshotMeta {
                    last_log_id: machine.applied,
                    last_membership: machine.membership.clone(),
                    snapshot_id: format!("{:?}", machine.applied),
                };
                (data, meta)
            };
            store.write_snapshot(store.0.genesis_id, &meta, data.clone(), "snapshot")?;
            store.publish(|store, image| {
                image.snapshot = Some(meta.clone());
                store.finish_purge(image)
            })?;
            Ok(Snapshot {
                meta,
                snapshot: Box::new(Cursor::new(data)),
            })
        })
        .await
    }
}

fn newest_snapshot_covering(
    snapshots_dir: &Path,
    hard: &Hard,
) -> Result<Option<SnapshotFile>, CoordinationError> {
    let purged = hard.purged.map(|id| id.index);
    for (_, path) in numbered(snapshots_dir)?.into_iter().rev() {
        let Ok(file) = unseal::<SnapshotFile>(SNAPSHOT_LABEL, &read(&path)?) else {
            continue;
        };
        if file.format != FORMAT || file.identity != hard.identity {
            return Err(CoordinationError::UnsupportedFormat);
        }
        if purged.is_none_or(|p| file.meta.last_log_id.is_some_and(|id| id.index >= p)) {
            return Ok(Some(file));
        }
    }
    Ok(None)
}

fn as_follower(vote: Vote<u64>) -> Vote<u64> {
    Vote {
        committed: false,
        ..vote
    }
}

#[cfg(test)]
#[path = "raft_store/tests.rs"]
mod tests;
