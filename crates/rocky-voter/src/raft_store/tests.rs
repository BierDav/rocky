use super::*;
use p2panda_core::PrivateKey;
use rocky_coordinator::{Limits, service::Operation};
use rocky_core::*;
use std::collections::BTreeSet;
/// A genesis whose directory key also administers membership, and the
/// trusted node of its one domain.
fn certified() -> (CertifiedGenesis, PrivateKey, Scope, PrivateKey) {
    let root = PrivateKey::new();
    let directory = PrivateKey::new();
    let executor = PrivateKey::new();
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let time = now().unwrap();
    let control = ControlCertificate::sign(
        DomainControl {
            scope,
            configuration: [4; 16],
            revision: 1,
            trusted: vec![executor.public_key()],
            publisher: None,
            not_before: time - 1,
            expires: time + 7200,
        },
        &directory,
    )
    .unwrap();
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
            directory_definition: [6; 32],
            directory: Some(control),
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
    forged.genesis.directory_definition = [0; 32];
    assert!(forged.verify(genesis.root, scope.mesh).is_err());
    let open =
        |path: &Path, provision| Store::open(path, genesis.clone(), provision, Limits::default());
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path(), true).unwrap();
    assert!(matches!(
        open(directory.path(), false),
        Err(CoordinationError::Denied)
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
    let entries: Vec<_> = (1..=2)
        .map(|index| Entry::<Types> {
            log_id: LogId::new(CommittedLeaderId::new(1, 1), index),
            payload: EntryPayload::Blank,
        })
        .collect();
    store.blocking_append(entries.clone()).await.unwrap();
    store.save_committed(Some(entries[1].log_id)).await.unwrap();
    drop(store);
    std::fs::remove_file(directory.path().join("log").join(file_name(1))).unwrap();
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

#[tokio::test]
async fn coordinated_pending_and_resolution_recover_from_logs_and_snapshot() {
    use openraft::storage::RaftLogStorageExt;
    use rocky_coordinator::coordinated::{Action, Result as LogResult};
    use rocky_coordinator::service::Outcome;
    let (genesis, executor, scope, _) = certified();
    let voter = PrivateKey::new();
    let root = PrivateKey::new();
    let mut definition = genesis.genesis;
    definition.voters[0].key = voter.public_key();
    let genesis = CertifiedGenesis::sign(definition, &root).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let time = now().unwrap();
    let request = |key: &PrivateKey, action| {
        self_certified(
            genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Coordinated {
                log: tip_log(scope),
                action,
            },
            key,
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
    let head = store.log_head(tip_log(scope)).await.unwrap();
    write(
        &mut store,
        request(
            &executor,
            Action::Join {
                position: 0,
                running_hash: head.running_hash,
                sequence: 1,
            },
        ),
        1,
        time,
    )
    .await
    .unwrap();
    let head = store.log_head(tip_log(scope)).await.unwrap();
    let append = request(
        &executor,
        Action::Append {
            expected: 0,
            epoch: head.epoch,
            hash: [7; 32],
            duration: 30,
            sequence: 2,
        },
    );
    let assigned = write(&mut store, append.clone(), 2, time).await.unwrap();
    let Outcome::Coordinated(LogResult::Assigned(pending)) = &assigned else {
        panic!("assignment")
    };
    let pending = pending.as_ref();
    drop(store);
    store = Store::open(dir.path(), genesis.clone(), false, Limits::default()).unwrap();
    // OpenRaft replays committed entries through its state-machine interface on startup.
    let entries = store.try_get_log_entries(1..=2).await.unwrap();
    store.apply(entries).await.unwrap();
    assert_eq!(
        store
            .log_head(tip_log(scope))
            .await
            .unwrap()
            .pending
            .as_ref(),
        Some(pending)
    );
    assert_eq!(write(&mut store, append, 3, time).await.unwrap(), assigned);
    let resolve = request(
        &voter,
        Action::Resolve {
            pending: pending.clone(),
            acknowledgments: vec![request(
                &executor,
                Action::Acknowledge {
                    pending: pending.clone(),
                },
            )],
        },
    );
    let committed = write(&mut store, resolve.clone(), 4, time).await.unwrap();
    assert!(matches!(
        committed,
        Outcome::Coordinated(LogResult::Committed { position: 1, .. })
    ));
    store.build_snapshot().await.unwrap();
    drop(store);
    store = Store::open(dir.path(), genesis, false, Limits::default()).unwrap();
    assert_eq!(store.log_head(tip_log(scope)).await.unwrap().commit, 1);
    assert_eq!(
        write(&mut store, resolve, 5, time).await,
        Err(CoordinationError::StaleHead)
    );
    assert_eq!(
        store.log_head(tip_log(scope)).await.unwrap().hashes.len(),
        1
    );
}

#[tokio::test]
async fn a_recovery_survives_log_replay_and_snapshot_and_retries_identically() {
    use openraft::storage::RaftLogStorageExt;
    use rocky_coordinator::coordinated::{Action, Result as LogResult};
    use rocky_coordinator::service::Outcome;
    use rocky_credentials::{
        LogPoint, Recovery, SEAL_FORMAT, SealAction, SealStatement, SignedSeal,
    };
    let (genesis, executor, scope, authority) = certified();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let time = now().unwrap();
    let request = |key: &PrivateKey, action| {
        self_certified(
            genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Coordinated {
                log: tip_log(scope),
                action,
            },
            key,
            time,
        )
        .unwrap()
    };
    async fn write(store: &mut Store, request: Request, index: u64, time: u64) -> Outcome {
        let entry = Entry::<Types> {
            log_id: LogId::new(CommittedLeaderId::new(1, 1), index),
            payload: EntryPayload::Normal(Proposal { time, request }),
        };
        store.blocking_append([entry.clone()]).await.unwrap();
        store.save_committed(Some(entry.log_id)).await.unwrap();
        store.apply([entry]).await.unwrap().remove(0).unwrap()
    }
    let head = store.log_head(tip_log(scope)).await.unwrap();
    let anchor = LogPoint {
        position: 0,
        running: head.genesis,
    };
    let seal = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: tip_log(scope).scope,
            tip: tip_log(scope).tip,
            frontier: Vec::new(),
            action: SealAction::Continue { schema: [5; 32] },
            recovery: Some(Recovery {
                retained: anchor,
                replaced: anchor,
            }),
        },
        &authority,
    )
    .unwrap();
    let recover = Action::Recover {
        seal: seal.to_bytes().unwrap(),
    };
    let admitted = write(&mut store, request(&executor, recover.clone()), 1, time).await;
    let Outcome::Coordinated(LogResult::Committed { position: 1, .. }) = admitted else {
        panic!("recovery {admitted:?}")
    };
    let after = store.log_head(tip_log(scope)).await.unwrap();
    drop(store);
    store = Store::open(dir.path(), genesis.clone(), false, Limits::default()).unwrap();
    let entries = store.try_get_log_entries(1..=1).await.unwrap();
    store.apply(entries).await.unwrap();
    assert_eq!(store.log_head(tip_log(scope)).await.unwrap(), after);
    store.build_snapshot().await.unwrap();
    drop(store);
    store = Store::open(dir.path(), genesis.clone(), false, Limits::default()).unwrap();
    let head = store.log_head(tip_log(scope)).await.unwrap();
    assert_eq!(head, after);
    assert_eq!(head.terminal.unwrap().digest, seal.id().unwrap());
    assert_eq!(
        write(&mut store, request(&executor, recover), 2, time).await,
        admitted
    );
}

/// Polls `future` once: whether it completes without waiting on a lock.
fn ready<T>(future: impl std::future::Future<Output = T>) -> Option<T> {
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
        Operation::Coordinated {
            log: tip_log(scope),
            action: rocky_coordinator::coordinated::Action::Head,
        },
        &executor,
        now().unwrap(),
    )
    .unwrap();
    // As a log append or vote holds it while it publishes.
    let held = store.0.image.lock().await;
    let answers = (
        ready(store.preflight(&request)),
        ready(store.read(&request)).map(|read| format!("{read:?}")),
        ready(store.lane(&request)),
    );
    drop(held);
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
    let (genesis, executor, scope, _) = certified();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let time = now().unwrap();
    for index in 1..=16 {
        let request = self_certified(
            genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Coordinated {
                log: tip_log(scope),
                action: rocky_coordinator::coordinated::Action::Head,
            },
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
            Ok(Outcome::CoordinatedHead(_))
        ));
    }
    store.save_vote(&Vote::new(2, 1)).await.unwrap();
    assert_eq!(store.try_get_log_entries(1..=16).await.unwrap().len(), 16);
    // Every update shared the entries it kept; apply changes no metadata copy.
    assert_eq!(store.0.copies.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[tokio::test]
async fn head_and_preflight_do_not_wait_for_a_blocked_log_sync() {
    use openraft::storage::RaftLogStorageExt;
    let (genesis, executor, scope, _) = certified();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let log = tip_log(scope);
    let request = self_certified(
        genesis.id().unwrap(),
        [1; 32],
        Operation::Coordinated {
            log,
            action: rocky_coordinator::coordinated::Action::Head,
        },
        &executor,
        now().unwrap(),
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
        store.log_head(log).await.unwrap()
    })
    .await;
    let elapsed = started.elapsed();
    assert!(!write.is_finished());
    release.send(()).unwrap();
    write.await.unwrap().unwrap();
    assert_eq!(read.unwrap().commit, 0);
    println!("head + preflight during blocked disk sync: {elapsed:?}");
}

#[tokio::test]
async fn snapshots_larger_than_32_mib_can_be_saved_and_loaded() {
    let (genesis, _, _, _) = certified();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();

    // 33 MiB payload exceeds the former 32 MiB MAX_STORE cap
    let large_payload = vec![0x42u8; 33 * 1024 * 1024];
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

    // Verify reading and unsealing the large snapshot
    let path = store.snapshots_dir().join(file_name(snapshot_index(&meta)));
    let raw = read(&path).expect("reading snapshot file should succeed");
    assert!(raw.len() > 33 * 1024 * 1024);

    let unsealed: SnapshotFile =
        unseal(SNAPSHOT_LABEL, &raw).expect("unsealing >32 MiB snapshot should succeed");
    assert_eq!(unsealed.meta, meta);
    assert_eq!(unsealed.metadata.0.len(), 33 * 1024 * 1024);
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
            ..Extended::default()
        })
        .expect("extension"),
    };
    rocky_credentials::SignedCapability::sign(capability, key).expect("certificate")
}
/// A request of a node that presents its own certificate.
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

fn tip_log(scope: Scope) -> TipLog {
    TipLog {
        scope,
        tip: TipId([9; 32]),
    }
}
