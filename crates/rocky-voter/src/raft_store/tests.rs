use super::*;
use p2panda_core::PrivateKey;
use rocky_coordinator::{Limits, service::Operation};
use rocky_core::*;
use std::collections::BTreeSet;
fn certified() -> (CertifiedGenesis, PrivateKey, Scope, PrivateKey) {
    let root = PrivateKey::new();
    let directory = PrivateKey::new();
    let executor = PrivateKey::new();
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let genesis = CertifiedGenesis::sign(
        Genesis {
            mesh: scope.mesh,
            configuration: [4; 16],
            voters: (1..=3)
                .map(|id| Voter {
                    id,
                    address: format!("127.0.0.1:{}", 10000 + id),
                    key: PrivateKey::new().public_key(),
                })
                .collect(),
            initial_voters: BTreeSet::from([1, 2, 3]),
            membership_authority: directory.public_key(),
            authorities: vec![Authority {
                scope,
                issuer: directory.public_key(),
            }],
            max_control_lifetime: 86400,
            public: None,
        },
        &root,
    )
    .unwrap();
    (genesis, executor, scope, directory)
}
struct Builder(CertifiedGenesis);
impl openraft::testing::StoreBuilder<Types, Store, Store, tempfile::TempDir> for Builder {
    async fn build(&self) -> Result<(tempfile::TempDir, Store, Store), StorageError<u64>> {
        let directory = tempfile::tempdir().map_err(io_error)?;
        let store = Store::open(directory.path(), self.0.clone(), true, Limits::default())
            .map_err(io_error)?;
        Ok((directory, store.clone(), store))
    }
}
#[test]
fn openraft_storage_contract() {
    openraft::testing::Suite::<Types, Store, Store, Builder, tempfile::TempDir>::test_all(Builder(
        certified().0,
    ))
    .unwrap();
}
#[test]
fn stores_fail_closed_on_corruption_lost_state_and_old_formats() {
    let (genesis, _, scope, _) = certified();
    assert!(
        genesis
            .verify(PrivateKey::new().public_key(), scope.mesh)
            .is_err()
    );
    let mut forged = genesis.clone();
    forged.genesis.configuration = [0; 16];
    assert!(forged.verify(genesis.root, scope.mesh).is_err());
    let open =
        |path: &Path, provision| Store::open(path, genesis.clone(), provision, Limits::default());
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path(), true).unwrap();
    assert!(matches!(
        open(directory.path(), false),
        Err(CoordinationError::Configuration)
    ));
    drop(store);
    let path = directory.path().join("state.cbor");
    let mut data = std::fs::read(&path).unwrap();
    let last = data.len() - 1;
    data[last] ^= 1;
    std::fs::write(&path, data).unwrap();
    assert!(open(directory.path(), false).is_err());
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        open(directory.path(), true),
        Err(CoordinationError::CoordinationStateLost)
    ));
}

#[tokio::test]
async fn missing_committed_prefix_cannot_reopen_at_genesis() {
    use openraft::storage::RaftLogStorageExt;
    let (genesis, ..) = certified();
    let directory = tempfile::tempdir().unwrap();
    let mut store =
        Store::open(directory.path(), genesis.clone(), true, Limits::default()).unwrap();
    let entries: Vec<_> = (0..=1)
        .map(|index| Entry::<Types> {
            log_id: LogId::new(CommittedLeaderId::new(1, 1), index),
            payload: EntryPayload::Blank,
        })
        .collect();
    store.blocking_append(entries.clone()).await.unwrap();
    store.save_committed(Some(entries[1].log_id)).await.unwrap();
    drop(store);
    std::fs::remove_file(directory.path().join("log").join(file_name(0))).unwrap();
    assert!(matches!(
        Store::open(directory.path(), genesis, false, Limits::default()),
        Err(CoordinationError::Corrupt)
    ));
}

#[test]
fn unsupported_store_versions_refuse_open_and_restore_before_writes() {
    let (genesis, ..) = certified();
    for format in [0, 1, 2, 3, 4, 5, FORMAT - 1, FORMAT + 1] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        drop(Store::open(&source, genesis.clone(), true, Limits::default()).unwrap());
        let backup = directory.path().join("backup");
        Store::backup(&source, genesis.clone(), 1, &backup).unwrap();
        for path in [&source, &backup] {
            let state = path.join("state.cbor");
            let mut hard: Hard = unseal(STATE_LABEL, &read(&state).unwrap()).unwrap();
            hard.format = format;
            std::fs::write(state, seal(STATE_LABEL, &hard).unwrap()).unwrap();
        }
        std::fs::remove_file(source.join("LOCK")).unwrap();
        let before = read(&source.join("state.cbor")).unwrap();
        assert!(matches!(
            Store::open(&source, genesis.clone(), false, Limits::default()),
            Err(CoordinationError::UnsupportedFormat)
        ));
        assert_eq!(read(&source.join("state.cbor")).unwrap(), before);
        assert!(!source.join("LOCK").exists());
        let output = directory.path().join("restored");
        assert!(matches!(
            Store::restore(&backup, &genesis, 1, &output),
            Err(CoordinationError::UnsupportedFormat)
        ));
        assert!(!output.exists());
    }
}

fn invoices() -> rocky_coordinator::counters::Name {
    rocky_coordinator::counters::Name::new("invoices").unwrap()
}
fn reading(scope: Scope) -> Operation {
    Operation::Counter {
        scope,
        counter: invoices(),
    }
}

#[tokio::test]
async fn claims_recover_from_logs_and_snapshot_and_retry_identically() {
    use openraft::storage::RaftLogStorageExt;
    use rocky_coordinator::service::Outcome;
    let (genesis, executor, scope, authority) = certified();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let time = now().unwrap();
    register_control(&mut store, &genesis, scope, &authority, &executor, 0, time).await;
    let request = |operation| {
        self_certified(
            genesis.id().unwrap(),
            rocky_coordinator::protocol::nonce(),
            operation,
            &executor,
            time,
        )
        .unwrap()
    };
    async fn write(
        store: &mut Store,
        request: Request,
        index: u64,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let entry = Entry::<Types> {
            log_id: LogId::new(CommittedLeaderId::new(1, 1), index),
            payload: EntryPayload::Normal(Proposal { time, request }),
        };
        store.blocking_append([entry.clone()]).await.unwrap();
        store.save_committed(Some(entry.log_id)).await.unwrap();
        store.apply([entry]).await.unwrap().remove(0)
    }
    let claim = Operation::claim(scope, invoices(), rocky_coordinator::Claim::new([7], [7]));
    let claimed = write(&mut store, request(claim.clone()), 1, time)
        .await
        .unwrap();
    assert_eq!(
        claimed,
        Outcome::Claimed(rocky_coordinator::Claimed::First(1))
    );
    drop(store);
    store = Store::open(dir.path(), genesis.clone(), false, Limits::default()).unwrap();
    replay_committed_entries_as_openraft_does_on_startup(&mut store, 0..=1).await;
    assert_eq!(
        write(&mut store, request(claim.clone()), 2, time).await,
        Ok(claimed.clone())
    );
    store.build_snapshot().await.unwrap();
    drop(store);
    store = Store::open(dir.path(), genesis.clone(), false, Limits::default()).unwrap();
    assert!(matches!(
        store.read(&request(reading(scope))).await,
        Ok(Outcome::Counter(1))
    ));
    assert_eq!(
        write(&mut store, request(claim), 3, time).await,
        Ok(claimed)
    );
}

fn ready_without_waiting<T>(future: impl std::future::Future<Output = T>) -> Option<T> {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => Some(value),
        std::task::Poll::Pending => None,
    }
}
#[tokio::test]
async fn requests_authorize_while_a_log_write_holds_the_image() {
    let (genesis, executor, scope, _) = certified();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let request = self_certified(
        genesis.id().unwrap(),
        [1; 32],
        reading(scope),
        &executor,
        now().unwrap(),
    )
    .unwrap();
    let held_as_by_a_publishing_append_or_vote = store.0.image.lock().await;
    let answers = (
        ready_without_waiting(store.preflight(&request)),
        ready_without_waiting(store.read(&request)).map(|read| format!("{read:?}")),
        ready_without_waiting(store.lane(&request)),
    );
    drop(held_as_by_a_publishing_append_or_vote);
    let free = (
        store.preflight(&request).await,
        format!("{:?}", store.read(&request).await),
        store.lane(&request).await,
    );
    assert_eq!(answers, (Some(free.0), Some(free.1), Some(free.2)));
}
#[tokio::test]
async fn updates_share_log_entries_and_apply_metadata_in_place() {
    use openraft::storage::RaftLogStorageExt;
    let (genesis, executor, scope, authority) = certified();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let time = now().unwrap();
    register_control(&mut store, &genesis, scope, &authority, &executor, 0, time).await;
    for index in 1..16 {
        let request = self_certified(
            genesis.id().unwrap(),
            rocky_coordinator::protocol::nonce(),
            reading(scope),
            &executor,
            time,
        )
        .unwrap();
        let entry = Entry::<Types> {
            log_id: LogId::new(CommittedLeaderId::new(1, 1), index),
            payload: EntryPayload::Normal(Proposal { time, request }),
        };
        store.blocking_append([entry.clone()]).await.unwrap();
        store.save_committed(Some(entry.log_id)).await.unwrap();
        assert!(matches!(
            store.apply([entry]).await.unwrap().remove(0),
            Ok(Outcome::Counter(_))
        ));
    }
    store.save_vote(&Vote::new(2, 1)).await.unwrap();
    assert_eq!(store.try_get_log_entries(0..16).await.unwrap().len(), 16);
    assert_eq!(
        store
            .0
            .copied_entries
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "every update shares the entries it keeps and apply copies no metadata"
    );
}

#[tokio::test]
async fn reads_and_preflight_do_not_wait_for_a_blocked_log_sync() {
    use openraft::storage::RaftLogStorageExt;
    let (genesis, executor, scope, authority) = certified();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let time = now().unwrap();
    register_control(&mut store, &genesis, scope, &authority, &executor, 0, time).await;
    let request = self_certified(
        genesis.id().unwrap(),
        [1; 32],
        reading(scope),
        &executor,
        time,
    )
    .unwrap();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    *store.0.sync_gate.lock().unwrap() = Some(SyncGate {
        entered,
        release: gate,
    });
    let mut writer = store.clone();
    let entry = Entry::<Types> {
        log_id: LogId::new(CommittedLeaderId::new(1, 1), 1),
        payload: EntryPayload::Blank,
    };
    let write = tokio::spawn(async move { writer.blocking_append([entry]).await });
    waiting.await.unwrap();
    let started = std::time::Instant::now();
    let read = tokio::time::timeout(std::time::Duration::from_millis(100), async {
        store.preflight(&request).await.unwrap();
        store.read(&request).await.unwrap()
    })
    .await;
    let elapsed = started.elapsed();
    assert!(!write.is_finished());
    release.send(()).unwrap();
    write.await.unwrap().unwrap();
    assert!(matches!(read.unwrap(), Outcome::Counter(0)));
    println!("read + preflight during blocked disk sync: {elapsed:?}");
}

#[tokio::test]
async fn snapshots_larger_than_32_mib_can_be_saved_and_loaded() {
    let (genesis, _, _, _) = certified();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let large_payload = vec![0x42u8; LARGE_SNAPSHOT_BYTES];
    let meta = SnapshotMeta {
        last_log_id: None,
        last_membership: openraft::StoredMembership::default(),
        snapshot_id: "large-snapshot-test".into(),
    };
    store
        .write_snapshot(
            genesis.id().unwrap(),
            &meta,
            large_payload.clone(),
            "large_snapshot_test",
        )
        .expect("writing a >32 MiB snapshot should succeed under 1 GiB store limit");
    let path = store.snapshots_dir().join(file_name(snapshot_index(&meta)));
    let raw = read(&path).expect("reading snapshot file should succeed");
    assert!(raw.len() > LARGE_SNAPSHOT_BYTES);

    let unsealed: SnapshotFile =
        unseal(SNAPSHOT_LABEL, &raw).expect("unsealing >32 MiB snapshot should succeed");
    assert_eq!(unsealed.meta, meta);
    assert_eq!(unsealed.metadata.0.len(), LARGE_SNAPSHOT_BYTES);
}

const LARGE_SNAPSHOT_BYTES: usize = 33 * 1024 * 1024;

async fn replay_committed_entries_as_openraft_does_on_startup(
    store: &mut Store,
    indexes: std::ops::RangeInclusive<u64>,
) {
    let entries = store.try_get_log_entries(indexes).await.unwrap();
    store.apply(entries).await.unwrap();
}

pub(crate) fn self_certificate(
    key: &p2panda_core::PrivateKey,
) -> rocky_credentials::SignedCapability {
    self_certificate_in(
        key,
        rocky_credentials::DatabaseContext {
            publisher: key.public_key(),
            scope: Scope {
                mesh: MeshId([1; 16]),
                app: AppId([2; 16]),
                domain: DomainId([3; 16]),
            },
        },
    )
}
#[cfg(test)]
pub(crate) fn self_certificate_in(
    key: &p2panda_core::PrivateKey,
    database: rocky_credentials::DatabaseContext,
) -> rocky_credentials::SignedCapability {
    use rocky_credentials::{Capability, Extended, Extension, PeerId};
    let capability = Capability {
        subject: key.public_key(),
        kind: rocky_core::PrincipalKind::Node,
        grants: Vec::new(),
        not_before: 0,
        expires: rocky_credentials::NEVER,
        delegations: 0,
        extension: Extension::new(Extended {
            node: Some(PeerId::of(key.public_key())),
            database: Some(database),
            sequence: Some(0),
            possession: Some(
                rocky_credentials::NodePossession::sign(key, key.public_key(), database)
                    .expect("possession"),
            ),
            ..Extended::default()
        })
        .expect("extension"),
    };
    rocky_credentials::SignedCapability::sign(capability, key).expect("certificate")
}
#[cfg(test)]
pub(crate) fn self_certified(
    genesis: Commitment,
    id: Commitment,
    operation: Operation,
    key: &p2panda_core::PrivateKey,
    time: u64,
) -> Result<Request, CoordinationError> {
    Request::certified(
        genesis,
        id,
        operation,
        (key, Some(self_certificate(key))),
        time,
    )
}

async fn register_control(
    store: &mut Store,
    genesis: &CertifiedGenesis,
    scope: Scope,
    authority: &PrivateKey,
    executor: &PrivateKey,
    index: u64,
    time: u64,
) {
    use openraft::storage::RaftLogStorageExt;
    let certificate = rocky_coordinator::certified::ControlCertificate::sign(
        rocky_coordinator::certified::DomainControl {
            scope,
            configuration: genesis.genesis.configuration,
            revision: 1,
            admins: vec![executor.public_key()],
            publisher: Some(Box::new(executor.public_key())),
            not_before: time.saturating_sub(1),
            expires: time + 3600,
        },
        authority,
    )
    .unwrap();
    let request = self_certified(
        genesis.id().unwrap(),
        rocky_coordinator::protocol::nonce(),
        Operation::Control(Box::new(certificate)),
        authority,
        time,
    )
    .unwrap();
    let entry = Entry::<Types> {
        log_id: LogId::new(CommittedLeaderId::new(1, 1), index),
        payload: EntryPayload::Normal(Proposal { time, request }),
    };
    store.blocking_append([entry.clone()]).await.unwrap();
    store.save_committed(Some(entry.log_id)).await.unwrap();
    store.apply([entry]).await.unwrap().remove(0).unwrap();
}

const RESTORE_CRASH_DIR: &str = "ROCKY_VOTER_RESTORE_CRASH_DIR";
const RESTORE_STEPS: [&str; 6] = [
    "restore.fence.before_sync",
    "restore.fence.before_rename",
    "restore.fence.after_sync",
    "restore.state.before_sync",
    "restore.state.before_rename",
    "restore.state.after_sync",
];

#[test]
fn restore_crash_child() {
    let Ok(dir) = std::env::var(RESTORE_CRASH_DIR) else {
        return;
    };
    let dir = Path::new(&dir);
    let genesis: CertifiedGenesis = decode(&read(&dir.join("genesis.cbor")).unwrap()).unwrap();
    let _ = Store::restore(&dir.join("backup"), &genesis, 1, &dir.join("restored"));
}

#[tokio::test]
async fn an_interrupted_restore_never_opens_unfenced_or_with_the_backup_leader_vote() {
    use openraft::storage::RaftLogStorageExt;
    use std::os::unix::process::ExitStatusExt;
    let (genesis, ..) = certified();
    for step in RESTORE_STEPS.into_iter().map(Some).chain([None]) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let mut store = Store::open(&source, genesis.clone(), true, Limits::default()).unwrap();
        store.save_vote(&Vote::new_committed(2, 1)).await.unwrap();
        store
            .blocking_append((0..=1).map(|index| Entry::<Types> {
                log_id: LogId::new(CommittedLeaderId::new(2, 1), index),
                payload: EntryPayload::Blank,
            }))
            .await
            .unwrap();
        drop(store);
        let backup = directory.path().join("backup");
        let manifest = Store::backup(&source, genesis.clone(), 1, &backup).unwrap();
        let restored = directory.path().join("restored");
        match step {
            Some(step) => {
                std::fs::write(
                    directory.path().join("genesis.cbor"),
                    encode(&genesis).unwrap(),
                )
                .unwrap();
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "raft_store::tests::restore_crash_child",
                        "--exact",
                        "--test-threads=1",
                    ])
                    .env(RESTORE_CRASH_DIR, directory.path())
                    .env("ROCKY_COORDINATOR_CRASH", step)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap();
                assert_eq!(
                    status.signal(),
                    Some(6),
                    "the restore must reach {step}: {status:?}"
                );
            }
            None => assert_eq!(
                Store::restore(&backup, &genesis, 1, &restored).unwrap(),
                manifest
            ),
        }
        for provision in [false, true] {
            let Ok(store) = Store::open(&restored, genesis.clone(), provision, Limits::default())
            else {
                assert!(
                    step.is_some_and(|step| step != "restore.state.after_sync"),
                    "a restore whose state file is synced must open"
                );
                continue;
            };
            let image = store.0.image.lock().await;
            assert_eq!(
                image.fence,
                Some(Fence {
                    index: manifest.index,
                    learner: false,
                }),
                "a restore interrupted at {step:?} opened without its fence"
            );
            assert!(
                image.hard.vote.is_some_and(|vote| !vote.committed),
                "a restore interrupted at {step:?} kept the backup's leader vote"
            );
        }
    }
}

#[test]
fn a_dropped_store_unlocks_while_a_spawned_process_still_holds_its_lock_descriptor() {
    let (genesis, ..) = certified();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let store = Store::open(&source, genesis.clone(), true, Limits::default()).unwrap();
    let mut inheritor = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(store.0._lock.0.try_clone().unwrap())
        .spawn()
        .unwrap();
    drop(store);
    let reopened = Store::open(&source, genesis.clone(), false, Limits::default()).map(drop);
    let backup = Store::backup(&source, genesis, 1, &directory.path().join("backup")).map(drop);
    drop(inheritor.stdin.take());
    assert!(inheritor.wait().unwrap().success());
    assert_eq!(
        reopened,
        Ok(()),
        "a reopen must not wait for a process that inherited the lock descriptor"
    );
    assert_eq!(
        backup,
        Ok(()),
        "a backup must not wait for a process that inherited the lock descriptor"
    );
}

const CUT_CRASH_DIR: &str = "ROCKY_VOTER_CUT_CRASH_DIR";

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum Cut {
    Truncate(u64),
    Purge(u64),
    Append(u64, u64),
}

struct CutScenario {
    name: &'static str,
    last: Option<u64>,
    committed: Option<u64>,
    snapshot_at: Option<u64>,
    purged: Option<u64>,
    cut: Cut,
}

const CUT_SCENARIOS: [CutScenario; 5] = [
    CutScenario {
        name: "conflict truncation right after the purged prefix",
        last: Some(6),
        committed: Some(3),
        snapshot_at: Some(3),
        purged: Some(3),
        cut: Cut::Truncate(4),
    },
    CutScenario {
        name: "conflict truncation of a never purged log",
        last: Some(3),
        committed: None,
        snapshot_at: None,
        purged: None,
        cut: Cut::Truncate(0),
    },
    CutScenario {
        name: "purge below a committed but unapplied entry",
        last: Some(6),
        committed: Some(6),
        snapshot_at: Some(5),
        purged: None,
        cut: Cut::Purge(3),
    },
    CutScenario {
        name: "first append to a fresh log",
        last: None,
        committed: None,
        snapshot_at: None,
        purged: None,
        cut: Cut::Append(0, 3),
    },
    CutScenario {
        name: "append right after the purged prefix",
        last: Some(3),
        committed: Some(3),
        snapshot_at: Some(3),
        purged: Some(3),
        cut: Cut::Append(4, 6),
    },
];

fn term_one(index: u64) -> LogId<u64> {
    LogId::new(CommittedLeaderId::new(1, 1), index)
}

fn blank(index: u64) -> Entry<Types> {
    Entry {
        log_id: term_one(index),
        payload: EntryPayload::Blank,
    }
}

impl Cut {
    async fn run(self, store: &mut Store) {
        use openraft::storage::RaftLogStorageExt;
        match self {
            Cut::Truncate(from) => store.truncate(term_one(from)).await.unwrap(),
            Cut::Purge(upto) => store.purge(term_one(upto)).await.unwrap(),
            Cut::Append(from, to) => store.blocking_append((from..=to).map(blank)).await.unwrap(),
        }
    }

    fn crash_steps(self, logs: &BTreeSet<u64>) -> Vec<String> {
        match self {
            Cut::Truncate(from) => logs
                .range(from..)
                .rev()
                .map(|index| format!("truncate.log.removed.{index}"))
                .chain(["truncate.log.after_sync".to_string()])
                .collect(),
            Cut::Purge(upto) => [
                "purge.state.before_sync",
                "purge.state.before_rename",
                "purge.state.after_sync",
            ]
            .map(String::from)
            .into_iter()
            .chain(
                logs.range(..=upto)
                    .map(|index| format!("purge.log.removed.{index}")),
            )
            .chain(["purge.log.after_sync".to_string()])
            .collect(),
            Cut::Append(from, to) => (from..=to)
                .map(|index| format!("log.renamed.{index}"))
                .chain(["log.after_sync".to_string()])
                .collect(),
        }
    }

    fn kept(self, logs: &BTreeSet<u64>) -> BTreeSet<u64> {
        match self {
            Cut::Truncate(from) => logs.range(..from).copied().collect(),
            Cut::Purge(upto) => logs.range(upto + 1..).copied().collect(),
            Cut::Append(from, to) => logs.iter().copied().chain(from..=to).collect(),
        }
    }
}

fn log_files(store: &Path) -> BTreeMap<u64, Vec<u8>> {
    numbered(&store.join("log"))
        .unwrap()
        .into_iter()
        .map(|(index, path)| (index, read(&path).unwrap()))
        .collect()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn first_retained_index(purged: Option<LogId<u64>>) -> u64 {
    purged.map_or(0, |id| id.index + 1)
}

impl CutScenario {
    async fn prepare(&self, path: &Path, genesis: &CertifiedGenesis) {
        use openraft::storage::RaftLogStorageExt;
        let mut store = Store::open(path, genesis.clone(), true, Limits::default()).unwrap();
        if let Some(last) = self.last {
            store.blocking_append((0..=last).map(blank)).await.unwrap();
        }
        store
            .save_committed(self.committed.map(term_one))
            .await
            .unwrap();
        if let Some(snapshot_at) = self.snapshot_at {
            store.apply((0..=snapshot_at).map(blank)).await.unwrap();
            store.build_snapshot().await.unwrap();
        }
        if let Some(purged) = self.purged {
            store.purge(term_one(purged)).await.unwrap();
        }
    }

    async fn check_reopened(
        &self,
        path: &Path,
        genesis: &CertifiedGenesis,
        before: &BTreeSet<u64>,
        step: Option<&str>,
        lost_changes: &[u64],
    ) {
        let context = format!(
            "{} crashed at {step:?} and lost the changes to {lost_changes:?}",
            self.name
        );
        let on_disk: Hard = unseal(STATE_LABEL, &read(&path.join("state.cbor")).unwrap()).unwrap();
        let holed = log_files(path)
            .into_keys()
            .filter(|index| on_disk.purged.is_none_or(|purged| *index > purged.index))
            .zip(first_retained_index(on_disk.purged)..)
            .any(|(index, expected)| index != expected);
        let mut store = match Store::open(path, genesis.clone(), false, Limits::default()) {
            Ok(store) => store,
            Err(error) => {
                assert_eq!(error, CoordinationError::Corrupt, "{context}");
                assert!(holed, "{context}: a store without a hole was refused");
                assert!(
                    !lost_changes.is_empty(),
                    "{context}: a process crash must leave a store that opens"
                );
                return;
            }
        };
        assert!(!holed, "{context}: a store with a hole in its log opened");
        let (logs, hard, snapshot) = {
            let image = store.0.image.lock().await;
            (
                image.logs.clone(),
                image.hard.clone(),
                image.snapshot.clone(),
            )
        };
        assert_eq!(
            hard.committed,
            self.committed.map(term_one),
            "{context}: the commit index moved"
        );
        let cut_purge = match self.cut {
            Cut::Purge(upto) => Some(term_one(upto)),
            Cut::Truncate(_) | Cut::Append(..) => None,
        };
        assert!(
            hard.purged == self.purged.map(term_one) || hard.purged == cut_purge,
            "{context}: purged {:?}",
            hard.purged
        );
        assert!(
            hard.purged.is_none_or(|purged| snapshot
                .and_then(|meta| meta.last_log_id)
                .is_some_and(|covered| covered.index >= purged.index)),
            "{context}: the purged prefix is not covered by the snapshot"
        );
        let start = first_retained_index(hard.purged);
        assert!(
            logs.keys().copied().eq(start..start + logs.len() as u64),
            "{context}: the reopened log {:?} has a hole after {:?}",
            logs.keys().collect::<Vec<_>>(),
            hard.purged
        );
        assert!(
            logs.values()
                .all(|entry| entry.log_id == term_one(entry.log_id.index)),
            "{context}: a log entry changed identity"
        );
        assert!(
            hard.committed.is_none_or(
                |committed| (start..=committed.index).all(|index| logs.contains_key(&index))
            ),
            "{context}: a committed entry was lost"
        );
        if let Cut::Truncate(from) = self.cut {
            assert!(
                logs.range(from..).all(|(index, _)| hard
                    .committed
                    .is_none_or(|committed| *index > committed.index)),
                "{context}: a truncated entry reappeared as committed"
            );
        }
        assert!(
            log_files(path)
                .into_keys()
                .all(|index| hard.purged.is_none_or(|purged| index > purged.index)),
            "{context}: a purged log file survived the reopen"
        );
        let state = store.get_log_state().await.unwrap();
        let visible = store.try_get_log_entries(..).await.unwrap();
        assert_eq!(state.last_purged_log_id, hard.purged, "{context}");
        assert_eq!(
            state.last_log_id,
            visible.last().map(|entry| entry.log_id).or(hard.purged),
            "{context}"
        );
        assert_eq!(visible.len(), logs.len(), "{context}");
        if step.is_none_or(|step| step.ends_with(".after_sync")) {
            assert_eq!(
                logs.keys().copied().collect::<BTreeSet<_>>(),
                self.cut.kept(before),
                "{context}: a cut whose directory sync completed was not durable"
            );
        }
        self.cut.run(&mut store).await;
        drop(store);
        let store = Store::open(path, genesis.clone(), false, Limits::default())
            .unwrap_or_else(|error| panic!("{context}: the retried cut left {error:?}"));
        assert_eq!(
            store
                .0
                .image
                .lock()
                .await
                .logs
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            self.cut.kept(before),
            "{context}: retrying the cut did not finish it"
        );
    }
}

#[tokio::test]
async fn log_cut_crash_child() {
    let Ok(dir) = std::env::var(CUT_CRASH_DIR) else {
        return;
    };
    let dir = Path::new(&dir);
    let genesis: CertifiedGenesis = decode(&read(&dir.join("genesis.cbor")).unwrap()).unwrap();
    let cut: Cut = decode(&read(&dir.join("cut.cbor")).unwrap()).unwrap();
    let mut store = Store::open(&dir.join("store"), genesis, false, Limits::default()).unwrap();
    cut.run(&mut store).await;
}

#[tokio::test]
async fn an_interrupted_append_truncation_or_purge_reopens_without_holes_or_lost_commits() {
    use std::os::unix::process::ExitStatusExt;
    let (genesis, ..) = certified();
    for scenario in &CUT_SCENARIOS {
        let template = tempfile::tempdir().unwrap();
        let prepared = template.path().join("store");
        scenario.prepare(&prepared, &genesis).await;
        let files = log_files(&prepared);
        let before: BTreeSet<u64> = files.keys().copied().collect();
        let steps = scenario.cut.crash_steps(&before);
        for step in steps.iter().map(|step| Some(step.as_str())).chain([None]) {
            let directory = tempfile::tempdir().unwrap();
            let crashed = directory.path().join("store");
            copy_tree(&prepared, &crashed);
            std::fs::write(
                directory.path().join("genesis.cbor"),
                encode(&genesis).unwrap(),
            )
            .unwrap();
            std::fs::write(
                directory.path().join("cut.cbor"),
                encode(&scenario.cut).unwrap(),
            )
            .unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "raft_store::tests::log_cut_crash_child",
                    "--exact",
                    "--test-threads=1",
                ])
                .env(CUT_CRASH_DIR, directory.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if let Some(step) = step {
                child.env("ROCKY_COORDINATOR_CRASH", step);
            }
            let status = child.status().unwrap();
            match step {
                Some(step) => assert_eq!(
                    status.signal(),
                    Some(6),
                    "{} must reach {step}: {status:?}",
                    scenario.name
                ),
                None => assert!(status.success(), "{}: {status:?}", scenario.name),
            }
            let after: BTreeSet<u64> = log_files(&crashed).into_keys().collect();
            let changed: Vec<u64> = before.symmetric_difference(&after).copied().collect();
            let directory_synced = step.is_none_or(|step| step.ends_with(".after_sync"));
            let unsynced = if directory_synced {
                Vec::new()
            } else {
                changed
            };
            for lost in 0..1usize << unsynced.len() {
                let lost_changes: Vec<u64> = unsynced
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| lost >> bit & 1 == 1)
                    .map(|(_, index)| *index)
                    .collect();
                let variant = directory.path().join(format!("lost-{lost}"));
                copy_tree(&crashed, &variant);
                for index in &lost_changes {
                    let file = variant.join("log").join(file_name(*index));
                    match files.get(index) {
                        Some(data) => std::fs::write(file, data).unwrap(),
                        None => std::fs::remove_file(file).unwrap(),
                    }
                }
                scenario
                    .check_reopened(&variant, &genesis, &before, step, &lost_changes)
                    .await;
            }
        }
    }
}
