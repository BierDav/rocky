use super::*;
use crate::Limits;
use p2panda_core::PrivateKey;
use rocky_core::*;
use std::collections::BTreeSet;
fn certified() -> (CertifiedGenesis, PrivateKey, Scope) {
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
            owner: PrincipalId([5; 16]),
            executors: vec![executor.public_key()],
            invokers: vec![(executor.public_key(), PrincipalId([5; 16]))],
            schema: 1,
            handler: 1,
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
    (genesis, executor, scope)
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
    let (genesis, executor, scope) = certified();
    let mut metadata = Metadata::new(genesis.clone()).unwrap();
    let id = genesis.id().unwrap();
    let time = now().unwrap();
    let limits = Limits::default();
    let acquire = |n: u64, time| {
        let mut identity = [0; 32];
        identity[..8].copy_from_slice(&n.to_le_bytes());
        Request::sign_at(id, identity, Operation::Acquire { scope }, &executor, time).unwrap()
    };
    for n in 0..limits.retry_records as u64 {
        metadata.apply_at(time, &acquire(n, time)).unwrap();
    }
    assert_eq!(
        metadata.apply_at(time, &acquire(u64::MAX, time)),
        Err(CoordinationError::Backpressure)
    );
    // A retry within the window returns the retained outcome; a different
    // operation under the same ID conflicts.
    assert!(metadata.apply_at(time, &acquire(0, time)).is_ok());
    let conflict = Request::sign_at(
        id,
        acquire(0, time).id,
        Operation::Takeover {
            scope,
            expected_epoch: 1,
        },
        &executor,
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
    assert!(metadata.apply_at(later, &acquire(1, later)).is_ok());
    assert_eq!(metadata.retry_records(), 1);
}
#[test]
fn stores_fail_closed_on_corruption_lost_state_and_old_formats() {
    let (genesis, _, scope) = certified();
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
    let (genesis, _, _) = certified();
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
        Request::sign_at(genesis.id().unwrap(), id, operation, &authority, time).unwrap()
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
    let (genesis, _, _) = certified();
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
    let (genesis, _, _) = certified();
    for format in [0, FORMAT - 1, FORMAT + 1] {
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
    let (genesis, executor, scope) = certified();
    let voter = PrivateKey::new();
    let root = PrivateKey::new();
    let mut definition = genesis.genesis;
    definition.voters[0].key = voter.public_key();
    let genesis = CertifiedGenesis::sign(definition, &root).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), genesis.clone(), true, Limits::default()).unwrap();
    let time = now().unwrap();
    let request = |key: &PrivateKey, action| {
        Request::sign_at(
            genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Coordinated { scope, action },
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
    let head = store.log_head(scope).await.unwrap();
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
    let head = store.log_head(scope).await.unwrap();
    let append = request(
        &executor,
        Action::Append {
            expected: 0,
            epoch: head.epoch,
            hash: [7; 32],
            deadline: time + 30,
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
        store.log_head(scope).await.unwrap().pending.as_ref(),
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
    assert_eq!(store.log_head(scope).await.unwrap().commit, 1);
    assert_eq!(
        write(&mut store, resolve, 5, time).await,
        Err(CoordinationError::StaleHead)
    );
    assert_eq!(store.log_head(scope).await.unwrap().hashes.len(), 1);
}
