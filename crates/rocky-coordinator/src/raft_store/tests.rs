use super::*;
use crate::Limits;
use p2panda_core::PrivateKey;
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
fn retry_records_expire_and_are_bounded_per_signer() {
    let (genesis, _, scope, directory) = certified();
    let mut metadata = Metadata::new(genesis.clone()).unwrap();
    let id = genesis.id().unwrap();
    let time = now().unwrap();
    let limits = Limits::default();
    // Membership requests keep their outcome, refusals included.
    let acquire = |n: u64, time| {
        let mut identity = [0; 32];
        identity[..8].copy_from_slice(&n.to_le_bytes());
        let operation = Operation::Membership {
            voters: BTreeSet::from([1, 2, 3]),
            expected: BTreeSet::from([1, 2, 3]),
            expected_revision: 0,
        };
        crate::machine::self_certified(id, identity, operation, &directory, time).unwrap()
    };
    for n in 0..limits.retry_records as u64 {
        let _ = metadata.apply_at(time, &acquire(n, time));
    }
    assert_eq!(
        metadata.apply_at(time, &acquire(u64::MAX, time)),
        Err(CoordinationError::Backpressure)
    );
    // A retry within the window returns the retained outcome; a different
    // operation under the same ID conflicts.
    assert!(metadata.apply_at(time, &acquire(0, time)).is_ok());
    let conflict = crate::machine::self_certified(
        id,
        acquire(0, time).id,
        Operation::Close { scope },
        &directory,
        time,
    )
    .unwrap();
    assert_eq!(
        metadata.apply_at(time, &conflict),
        Err(CoordinationError::RequestConflict)
    );
    // After the window the records are gone and their signed requests are too old.
    let later = time + limits.retry_window + 1;
    assert_eq!(
        metadata.apply_at(later, &acquire(0, time)),
        Err(CoordinationError::Expired)
    );
    assert_eq!(metadata.retry_records(), 0);
    let _ = metadata.apply_at(later, &acquire(1, later));
    assert_eq!(metadata.retry_records(), 1);
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

#[test]
fn membership_replays_cannot_reverse_a_later_configuration_after_retry_expiry() {
    let (genesis, ..) = certified();
    // Use a genesis with a known membership signing key.
    let authority = PrivateKey::new();
    let root = PrivateKey::new();
    let mut config = genesis.genesis;
    config.membership_authority = authority.public_key();
    let genesis = CertifiedGenesis::sign(config, &root).unwrap();
    let mut metadata = Metadata::new(genesis.clone()).unwrap();
    let time = now().unwrap();
    let plan = Operation::Membership {
        voters: [1, 2].into(),
        expected: [1, 2, 3].into(),
        expected_revision: 0,
    };
    let sign = |id, operation, time| {
        crate::machine::self_certified(genesis.id().unwrap(), id, operation, &authority, time)
            .unwrap()
    };
    metadata
        .apply_at(time, &sign([1; 32], plan.clone(), time))
        .unwrap();
    metadata
        .apply_at(
            time,
            &sign(
                [2; 32],
                Operation::Membership {
                    voters: [1, 2, 3].into(),
                    expected: [1, 2].into(),
                    expected_revision: 1,
                },
                time,
            ),
        )
        .unwrap();
    let later = time + Limits::default().retry_window + 1;
    assert_eq!(
        metadata.apply_at(later, &sign([1; 32], plan, later)),
        Err(CoordinationError::Configuration)
    );
    assert_eq!(metadata.desired_voters, [1, 2, 3].into());
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
    use crate::coordinated::{Action, Result as LogResult};
    use crate::machine::Outcome;
    use openraft::storage::RaftLogStorageExt;
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
        crate::machine::self_certified(
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
    use crate::coordinated::{Action, Result as LogResult};
    use crate::machine::Outcome;
    use openraft::storage::RaftLogStorageExt;
    use rocky_credentials::{
        LogPoint, Recovery, SEAL_FORMAT, SealAction, SealStatement, SignedSeal,
    };
    let (genesis, executor, scope, authority) = certified();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let time = now().unwrap();
    let request = |key: &PrivateKey, action| {
        crate::machine::self_certified(
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

#[tokio::test]
async fn head_and_preflight_do_not_wait_for_a_blocked_log_sync() {
    use openraft::storage::RaftLogStorageExt;
    let (genesis, executor, scope, _) = certified();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let log = tip_log(scope);
    let request = crate::machine::self_certified(
        genesis.id().unwrap(),
        [1; 32],
        Operation::Coordinated {
            log,
            action: crate::coordinated::Action::Head,
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
async fn client_follows_authenticated_leader_hints_and_receives_clock_skew() {
    use crate::{
        service::Reply,
        transport::{self, Envelope, Message},
    };
    let (original, executor, _, _) = certified();
    let mut genesis = original.genesis;
    let mut listeners = Vec::new();
    let mut keys = Vec::new();
    for voter in &mut genesis.voters {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let key = PrivateKey::new();
        voter.address = listener.local_addr().unwrap().to_string();
        voter.key = key.public_key();
        listeners.push(listener);
        keys.push(key);
    }
    let root = PrivateKey::new();
    let genesis = CertifiedGenesis::sign(genesis, &root).unwrap();
    let identity = genesis.id().unwrap();
    let visited = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut servers = Vec::new();
    for (index, (listener, key)) in listeners.into_iter().zip(keys).enumerate() {
        let visited = visited.clone();
        servers.push(tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let envelope = transport::read(&mut stream).await.unwrap();
                envelope.authenticate(identity).unwrap();
                let Message::Client(request) = decode(&envelope.body).unwrap() else {
                    panic!("client");
                };
                request.authenticate(identity).unwrap();
                visited.lock().unwrap().push(index + 1);
                let result = if index == 0 {
                    Err(CoordinationError::Unavailable)
                } else {
                    request.fresh().map(|()| Outcome::Closed)
                };
                let response = Envelope::from_body(
                    identity,
                    envelope.nonce,
                    encode(&Reply {
                        leader: Some(3),
                        result,
                    })
                    .unwrap(),
                    &key,
                )
                .unwrap();
                transport::write(&mut stream, &response).await.unwrap();
            }
        }));
    }
    let client = crate::client::NetworkCoordinator::open(
        genesis.clone(),
        root.public_key(),
        genesis.genesis.mesh,
        executor.clone(),
    )
    .unwrap();
    for _ in 0..2 {
        assert_eq!(
            client.call([1; 32], Operation::Usage).await.unwrap(),
            Outcome::Closed
        );
    }
    assert_eq!(*visited.lock().unwrap(), vec![1, 3, 3]);
    let time = now().unwrap();
    let request =
        self_certified(identity, [2; 32], Operation::Usage, &executor, time + 60).unwrap();
    let reply: Reply = transport::exchange(
        &genesis.genesis.voters[2],
        identity,
        &executor,
        Message::Client(Box::new(request)),
    )
    .await
    .unwrap();
    assert!(
        matches!(reply.result, Err(CoordinationError::ClockSkew { coordinator_time }) if coordinator_time.abs_diff(time) <= 1)
    );
    assert_eq!(
        client.call([2; 32], Operation::Usage).await.unwrap(),
        Outcome::Closed
    );
    assert_eq!(*visited.lock().unwrap(), vec![1, 3, 3, 3, 3]);
    for server in servers {
        server.abort();
    }
}
