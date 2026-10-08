use super::*;
use rocky_coordinator::MemoryCoordinator;

pub(super) fn command_grants(fixture: &Fixture) -> Vec<ResourceGrant> {
    let mut grants = fixture.full();
    for grant in &mut grants {
        grant.rights.extend([Right::Invoke, Right::Execute]);
        grant
            .fields
            .insert(fixture.app().definition().item_id("method", "SetEmployee"));
    }
    grants
}
pub(super) fn coordinator(fixture: &Fixture, config: &SecureConfig) -> MemoryCoordinator {
    let coordinator = MemoryCoordinator::default();
    let node = secure_node_id(config.node.public_key());
    let caller = secure_principal_id(config.credentials.0[0].capability.subject);
    coordinator
        .register(fixture.public.domain, caller, [1; 16], vec![node], 1, 1)
        .unwrap();
    coordinator
        .acquire(fixture.public.domain, caller, node)
        .unwrap();
    coordinator
}

#[tokio::test]
async fn encrypted_commands_recover_lost_reply_and_atomic_multiscope_rollback() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let config = fixture.config(
        &node,
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    // Retain the exact user delegation for reopen and immutable command retries.
    let reopen = SecureConfig {
        publisher: config.publisher,
        manifest: config.manifest.clone(),
        policy: config.policy.clone(),
        node: config.node.clone(),
        credentials: config.credentials.clone(),
        keys: vec![
            KeyMaterial {
                scope: fixture.public,
                generation: 1,
                key: [11; 32],
            },
            KeyMaterial {
                scope: fixture.private,
                generation: 1,
                key: [22; 32],
            },
        ],
    };
    let coordinator = coordinator(&fixture, &config);
    let path = dir.path().join("commands.db");
    let replica = EncryptedReplica::open(&path, fixture.app(), config)
        .await
        .unwrap();
    let command = replica
        .invoke_encrypted(
            [1; 32],
            fixture.private,
            "SetEmployee",
            b"employee-1\nAlice\n731994",
        )
        .await
        .unwrap();
    let original = command.to_bytes().unwrap();
    assert!(!original.windows(6).any(|bytes| bytes == b"731994"));
    assert_eq!(
        replica
            .invoke_encrypted(
                [1; 32],
                fixture.private,
                "SetEmployee",
                b"employee-1\nAlice\n731994"
            )
            .await
            .unwrap()
            .to_bytes()
            .unwrap(),
        original
    );
    assert!(
        replica
            .invoke_encrypted([1; 32], fixture.private, "SetEmployee", b"different")
            .await
            .is_err()
    );
    coordinator.drop_next_reply().unwrap();
    assert!(
        replica
            .execute_encrypted(command.clone(), &coordinator)
            .await
            .is_err()
    );
    assert!(
        replica
            .command_snapshot(fixture.public)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    replica.inject_failure(0, 1).await;
    assert!(
        replica
            .execute_encrypted(command.clone(), &coordinator)
            .await
            .is_err()
    );
    assert!(
        replica
            .command_snapshot(fixture.public)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        replica
            .command_snapshot(fixture.private)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(*replica.observe_revision().borrow(), 0);
    drop(replica);
    let replica = EncryptedReplica::open(&path, fixture.app(), reopen)
        .await
        .unwrap();
    replica.inject_failure(1, 0).await;
    let result = replica
        .execute_encrypted(command.clone(), &coordinator)
        .await
        .unwrap();
    assert_eq!(result.receipt().unwrap().sequence, 1);
    assert_eq!(
        replica
            .command_snapshot(fixture.public)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    assert_eq!(
        replica
            .command_snapshot(fixture.private)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    let revision = *replica.observe_revision().borrow();
    assert_eq!(
        replica
            .execute_encrypted(command, &coordinator)
            .await
            .unwrap()
            .to_bytes()
            .unwrap(),
        result.to_bytes().unwrap()
    );
    assert_eq!(*replica.observe_revision().borrow(), revision);
    let denied = fixture.config(
        &PrivateKey::new(),
        fixture.full(),
        &[fixture.public, fixture.private],
    );
    let denied = EncryptedReplica::open(dir.path().join("denied.db"), fixture.app(), denied)
        .await
        .unwrap();
    assert!(
        denied
            .invoke_encrypted([2; 32], fixture.private, "SetEmployee", b"id\nName\n5")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn encrypted_result_subset_reordering_missing_keys_and_private_artifacts() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let config = fixture.config(
        &node,
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    let coordinator = coordinator(&fixture, &config);
    let executor = EncryptedReplica::open(dir.path().join("executor.db"), fixture.app(), config)
        .await
        .unwrap();
    let first = executor
        .invoke_encrypted(
            [1; 32],
            fixture.private,
            "SetEmployee",
            b"employee\nAlice\n987123",
        )
        .await
        .unwrap();
    let first = executor
        .execute_encrypted(first, &coordinator)
        .await
        .unwrap();
    let second = executor
        .invoke_encrypted(
            [2; 32],
            fixture.private,
            "SetEmployee",
            b"employee\nBob\n567890",
        )
        .await
        .unwrap();
    let second = executor
        .execute_encrypted(second, &coordinator)
        .await
        .unwrap();
    let relay_key = PrivateKey::new();
    let relay_config = fixture.config(
        &relay_key,
        vec![
            grant(fixture.public, &[Right::Custody], &[]),
            grant(fixture.private, &[Right::Custody], &[]),
        ],
        &[],
    );
    let relay = EncryptedReplica::open(dir.path().join("relay.db"), fixture.app(), relay_config)
        .await
        .unwrap();
    relay
        .receive_encrypted_result(second.clone(), &coordinator)
        .await
        .unwrap();
    relay
        .receive_encrypted_result(first.clone(), &coordinator)
        .await
        .unwrap();
    let reader_key = PrivateKey::new();
    let reader_config = fixture.config(
        &reader_key,
        vec![grant(fixture.public, &[Right::Read], &[])],
        &[fixture.public],
    );
    let credentials = reader_config.credentials.clone();
    let reader = EncryptedReplica::open(dir.path().join("reader.db"), fixture.app(), reader_config)
        .await
        .unwrap();
    assert!(
        relay
            .encrypted_result_for(
                reader_key.public_key(),
                &credentials,
                fixture.public.domain,
                1,
                &[fixture.private]
            )
            .await
            .is_err()
    );
    let filtered_first = relay
        .encrypted_result_for(
            reader_key.public_key(),
            &credentials,
            fixture.public.domain,
            1,
            &[fixture.public],
        )
        .await
        .unwrap()
        .unwrap();
    let filtered_second = relay
        .encrypted_result_for(
            reader_key.public_key(),
            &credentials,
            fixture.public.domain,
            2,
            &[fixture.public],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(filtered_first.parts.len(), 1);
    assert!(reader.command_snapshot(fixture.private).await.is_err());
    assert!(
        reader
            .receive_encrypted_result(first.clone(), &coordinator)
            .await
            .is_err()
    );
    reader
        .receive_encrypted_result(filtered_second.clone(), &coordinator)
        .await
        .unwrap();
    assert!(
        !reader
            .command_snapshot(fixture.public)
            .await
            .unwrap()
            .complete
    );
    reader
        .receive_encrypted_result(filtered_first.clone(), &coordinator)
        .await
        .unwrap();
    let snapshot = reader.command_snapshot(fixture.public).await.unwrap();
    assert!(snapshot.complete);
    assert_eq!(
        snapshot.rows.values().next().unwrap(),
        &fixture.app().name("Bob")
    );
    reader.evict_key(fixture.public).await;
    assert!(
        !reader
            .command_snapshot(fixture.public)
            .await
            .unwrap()
            .complete
    );
    reader
        .install_key(KeyMaterial {
            scope: fixture.public,
            generation: 1,
            key: [11; 32],
        })
        .await
        .unwrap();
    assert!(
        reader
            .command_snapshot(fixture.public)
            .await
            .unwrap()
            .complete
    );
    let mut tampered = filtered_first;
    tampered.parts[0].generation += 1;
    assert!(
        reader
            .receive_encrypted_result(tampered, &coordinator)
            .await
            .is_err()
    );
    let inner = relay.inner.lock().await;
    for record in crate::storage::rows(&inner.conn, "SELECT result FROM secure_results")
        .await
        .unwrap()
    {
        let [rocky_core::SqlValue::Blob(bytes)] = record.as_slice() else {
            panic!("invalid result");
        };
        for secret in [b"987123", b"567890", b"Alice".as_slice(), b"Bob".as_slice()] {
            assert!(!bytes.windows(secret.len()).any(|window| window == secret));
        }
    }
    assert!(
        crate::storage::rows(&inner.conn, "SELECT rows FROM secure_command_rows")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn command_private_invocation_acl_fields_and_mixed_denial() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let config = fixture.config(
        &node,
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    let coordinator = coordinator(&fixture, &config);
    let replica = EncryptedReplica::open(dir.path().join("acl.db"), fixture.app(), config)
        .await
        .unwrap();
    assert!(
        replica
            .invoke_encrypted([1; 32], fixture.public, "SetEmployee", b"id\nAlice\n123456")
            .await
            .is_err()
    );
    let command = replica
        .invoke_encrypted(
            [1; 32],
            fixture.private,
            "SetEmployee",
            b"id\nAlice\n123456\nowner-1",
        )
        .await
        .unwrap();
    replica
        .execute_encrypted(command, &coordinator)
        .await
        .unwrap();
    assert_eq!(
        replica
            .command_snapshot(fixture.private)
            .await
            .unwrap()
            .rows
            .values()
            .next()
            .unwrap()[&fixture.app().field("owner")],
        b"owner-1"
    );
    let mut row = fixture.app().name("Alice");
    row.insert(fixture.app().field("owner"), b"owner-1".to_vec());
    assert!(
        replica
            .author(
                [2; 32],
                vec![change(fixture.public, 2, None, Some(row))],
                vec![]
            )
            .await
            .is_err()
    );
    let mut restricted = command_grants(&fixture);
    restricted[1].rights.remove(&Right::Insert);
    let config = fixture.config(
        &PrivateKey::new(),
        restricted,
        &[fixture.public, fixture.private],
    );
    let rejected_coordinator = self::coordinator(&fixture, &config);
    let restricted =
        EncryptedReplica::open(dir.path().join("restricted.db"), fixture.app(), config)
            .await
            .unwrap();
    let command = restricted
        .invoke_encrypted(
            [3; 32],
            fixture.private,
            "SetEmployee",
            b"id\nAlice\n123456",
        )
        .await
        .unwrap();
    assert!(
        restricted
            .execute_encrypted(command, &rejected_coordinator)
            .await
            .is_err()
    );
    assert_eq!(
        rejected_coordinator
            .head(fixture.public.domain)
            .unwrap()
            .sequence,
        0
    );
    assert!(
        restricted
            .command_snapshot(fixture.public)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(*restricted.observe_revision().borrow(), 0);
}

struct Outputs(Employees);
impl rocky_core::RowApplication for Outputs {
    fn manifest(&self) -> rocky_core::ApplicationManifest {
        self.0.definition()
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        self.0.permission_fields()
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        self.0.validate_changes(changes)
    }
}
impl rocky_core::RowCommandApplication for Outputs {
    fn command_resources(
        &self,
        scope: rocky_core::Scope,
        method: &str,
    ) -> Result<Vec<ResourceScope>, String> {
        self.0.command_resources(scope, method)
    }
    fn command_invocation_scope(
        &self,
        scope: rocky_core::Scope,
        method: &str,
    ) -> Result<ResourceScope, String> {
        self.0.command_invocation_scope(scope, method)
    }
    fn execute_command(
        &self,
        method: &str,
        arguments: &[u8],
        state: &rocky_core::CommandRows,
    ) -> Result<rocky_core::RowCommandOutput, String> {
        let mut output = if arguments == b"delete" {
            rocky_core::RowCommandOutput {
                changes: state
                    .iter()
                    .flat_map(|(scope, rows)| {
                        rows.iter().map(|(row, fields)| RowChange {
                            scope: *scope,
                            row: *row,
                            before: Some(fields.clone()),
                            after: None,
                        })
                    })
                    .collect(),
                outputs: vec![],
            }
        } else {
            self.0.execute_command(method, arguments, state)?
        };
        let scope = self.0.scope(
            state.keys().next().ok_or("missing state")?.domain.mesh,
            true,
        );
        output.outputs = [
            rocky_core::CommandOutputKind::Result,
            rocky_core::CommandOutputKind::Job,
            rocky_core::CommandOutputKind::Blob,
            rocky_core::CommandOutputKind::Checkpoint,
        ]
        .into_iter()
        .map(|kind| rocky_core::ScopedCommandOutput {
            scope,
            kind,
            bytes: b"private-artifact-981732".to_vec(),
        })
        .collect();
        Ok(output)
    }
}
#[tokio::test]
async fn command_outputs_jobs_blobs_and_checkpoints_are_filtered_together() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let config = fixture.config(
        &PrivateKey::new(),
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    let coordinator = coordinator(&fixture, &config);
    let executor = EncryptedReplica::open(
        dir.path().join("outputs.db"),
        Outputs(fixture.app()),
        config,
    )
    .await
    .unwrap();
    let command = executor
        .invoke_encrypted(
            [1; 32],
            fixture.private,
            "SetEmployee",
            b"id\nAlice\n123456",
        )
        .await
        .unwrap();
    let result = executor
        .execute_encrypted(command, &coordinator)
        .await
        .unwrap();
    assert_eq!(result.parts.len(), 6);
    let bytes = result.to_bytes().unwrap();
    let secret = b"private-artifact-981732";
    assert!(!bytes.windows(secret.len()).any(|part| part == secret));
    let reader = PrivateKey::new();
    let credentials =
        fixture.credentials(&reader, vec![grant(fixture.public, &[Right::Read], &[])]);
    let filtered = executor
        .encrypted_result_for(
            reader.public_key(),
            &credentials,
            fixture.public.domain,
            1,
            &[fixture.public],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(filtered.parts.len(), 1);
    assert_eq!(filtered.parts[0].kind, PackageKind::Rows);
    let package = filtered.strong_package().unwrap();
    let binding = PackageSet::verify_confirmed_subset(
        &package,
        &filtered.receipt().unwrap(),
        &coordinator,
        &filtered.parts,
    )
    .await
    .unwrap();
    for part in &result.parts {
        assert!(executor.open_package(part, &binding).await.is_ok());
    }
}

#[tokio::test]
async fn native_encrypted_invocation_and_result_application() {
    use network::NativeSession;
    use tokio::net::{TcpListener, TcpStream};
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let executor_key = PrivateKey::new();
    let caller_key = PrivateKey::new();
    let executor_config = fixture.config(
        &executor_key,
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    let caller_config = fixture.config(
        &caller_key,
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    let coordinator = coordinator(&fixture, &executor_config);
    let owner = secure_principal_id(executor_config.credentials.0[0].capability.subject);
    coordinator
        .set_invokers(
            fixture.public.domain,
            owner,
            vec![(
                secure_node_id(caller_key.public_key()),
                secure_principal_id(caller_config.credentials.0[0].capability.subject),
            )],
        )
        .unwrap();
    let executor_credentials = executor_config.credentials.clone();
    let caller_credentials = caller_config.credentials.clone();
    let executor = EncryptedReplica::open(
        dir.path().join("native-executor.db"),
        fixture.app(),
        executor_config,
    )
    .await
    .unwrap();
    let caller = EncryptedReplica::open(
        dir.path().join("native-caller.db"),
        fixture.app(),
        caller_config,
    )
    .await
    .unwrap();
    let command = caller
        .invoke_encrypted(
            [1; 32],
            fixture.private,
            "SetEmployee",
            b"id\nAlice\n123456",
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut session = NativeSession::authenticate(
            stream,
            executor_key.clone(),
            executor_credentials,
            caller_key.public_key(),
            1024 * 1024,
        )
        .await
        .unwrap();
        session
            .serve_command_once(&executor, &coordinator)
            .await
            .unwrap();
    };
    let invoke = async {
        let stream = TcpStream::connect(address).await.unwrap();
        let mut session = NativeSession::authenticate(
            stream,
            caller_key.clone(),
            caller_credentials,
            executor_key.public_key(),
            1024 * 1024,
        )
        .await
        .unwrap();
        let result = session
            .execute_command(
                &caller,
                command,
                vec![fixture.public, fixture.private],
                &coordinator,
            )
            .await
            .unwrap();
        assert_eq!(result.receipt().unwrap().sequence, 1);
    };
    tokio::join!(serve, invoke);
    assert_eq!(
        caller.command_snapshot(fixture.public).await.unwrap().rows,
        executor
            .command_snapshot(fixture.public)
            .await
            .unwrap()
            .rows
    );
    let inner = executor.inner.lock().await;
    let acks = crate::storage::rows(&inner.conn, "SELECT applied FROM secure_result_ack")
        .await
        .unwrap();
    assert_eq!(
        acks,
        vec![
            vec![rocky_core::SqlValue::Integer(1)],
            vec![rocky_core::SqlValue::Integer(1)]
        ]
    );
}

#[tokio::test]
async fn command_companions_wait_for_keys_and_require_proven_deletion() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let config = fixture.config(
        &PrivateKey::new(),
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    let coordinator = coordinator(&fixture, &config);
    let executor = EncryptedReplica::open(
        dir.path().join("deletion.db"),
        Outputs(fixture.app()),
        config,
    )
    .await
    .unwrap();
    let row = rocky_core::commitment("employees.id.v1", b"id");
    assert_eq!(
        executor
            .command_companion(fixture.private, row)
            .await
            .unwrap(),
        Companion::Unavailable
    );
    let command = executor
        .invoke_encrypted(
            [1; 32],
            fixture.private,
            "SetEmployee",
            b"id\nAlice\n123456",
        )
        .await
        .unwrap();
    let first = executor
        .execute_encrypted(command, &coordinator)
        .await
        .unwrap();
    let reader_config = fixture.config(&PrivateKey::new(), fixture.full(), &[fixture.public]);
    let reader = EncryptedReplica::open(
        dir.path().join("keyless-private.db"),
        Outputs(fixture.app()),
        reader_config,
    )
    .await
    .unwrap();
    reader
        .receive_encrypted_result(first, &coordinator)
        .await
        .unwrap();
    assert_eq!(
        reader
            .command_companion(fixture.private, row)
            .await
            .unwrap(),
        Companion::Unavailable
    );
    reader
        .install_key(KeyMaterial {
            scope: fixture.private,
            generation: 1,
            key: [22; 32],
        })
        .await
        .unwrap();
    assert!(
        !reader
            .command_snapshot(fixture.private)
            .await
            .unwrap()
            .complete
    );
    reader
        .reconcile_encrypted_results(&coordinator)
        .await
        .unwrap();
    assert_eq!(
        reader
            .command_companion(fixture.private, row)
            .await
            .unwrap(),
        Companion::Present(fixture.app().salary("123456"))
    );
    let command = executor
        .invoke_encrypted([2; 32], fixture.private, "SetEmployee", b"delete")
        .await
        .unwrap();
    let second = executor
        .execute_encrypted(command, &coordinator)
        .await
        .unwrap();
    reader
        .receive_encrypted_result(second, &coordinator)
        .await
        .unwrap();
    assert_eq!(
        reader
            .command_companion(fixture.private, row)
            .await
            .unwrap(),
        Companion::Deleted
    );
    reader.evict_key(fixture.private).await;
    assert_eq!(
        reader
            .command_companion(fixture.private, row)
            .await
            .unwrap(),
        Companion::Unavailable
    );
    assert_eq!(
        reader
            .command_companion(fixture.public, [99; 32])
            .await
            .unwrap(),
        Companion::Unavailable
    );
}

struct RefuseConfirmation<'a>(&'a MemoryCoordinator);
impl rocky_core::Coordinator for RefuseConfirmation<'_> {
    fn head(
        &self,
        scope: rocky_core::Scope,
    ) -> rocky_core::CoordinationFuture<'_, rocky_core::StrongHead> {
        Box::pin(async move { self.0.head(scope) })
    }
    fn lookup(
        &self,
        scope: rocky_core::Scope,
        id: rocky_core::RequestId,
    ) -> rocky_core::CoordinationFuture<'_, Option<rocky_core::StrongReceipt>> {
        Box::pin(async move { self.0.lookup(scope, id) })
    }
    fn verify<'a>(
        &'a self,
        receipt: &'a rocky_core::StrongReceipt,
    ) -> rocky_core::CoordinationFuture<'a, ()> {
        Box::pin(async move { self.0.verify(receipt) })
    }
    fn confirm(
        &self,
        _: rocky_core::Confirmation,
    ) -> rocky_core::CoordinationFuture<'_, rocky_core::StrongReceipt> {
        Box::pin(async { Err(rocky_core::CoordinationError::Unavailable) })
    }
    fn retire(
        &self,
        scope: rocky_core::Scope,
        node: rocky_core::NodeId,
        through: u64,
    ) -> rocky_core::CoordinationFuture<'_, ()> {
        rocky_core::Coordinator::retire(self.0, scope, node, through)
    }
}
#[tokio::test]
async fn retained_preparation_is_verified_before_confirmation() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let config = fixture.config(
        &PrivateKey::new(),
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    let coordinator = coordinator(&fixture, &config);
    let replica = EncryptedReplica::open(dir.path().join("preparation.db"), fixture.app(), config)
        .await
        .unwrap();
    let command = replica
        .invoke_encrypted(
            [1; 32],
            fixture.private,
            "SetEmployee",
            b"id\nAlice\n123456",
        )
        .await
        .unwrap();
    assert!(
        replica
            .execute_encrypted(command.clone(), &RefuseConfirmation(&coordinator))
            .await
            .is_err()
    );
    let original = {
        let inner = replica.inner.lock().await;
        let mut rows = inner
            .conn
            .query("SELECT result FROM secure_preparations", ())
            .await
            .unwrap();
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<Vec<u8>>(0)
            .unwrap()
    };
    let mut corrupt = ConfirmedCommand::from_bytes(&original).unwrap();
    corrupt.parts[0].generation += 1;
    {
        let inner = replica.inner.lock().await;
        inner
            .conn
            .execute(
                "UPDATE secure_preparations SET result=?",
                [corrupt.to_bytes().unwrap()],
            )
            .await
            .unwrap();
    }
    assert!(
        replica
            .execute_encrypted(command.clone(), &coordinator)
            .await
            .is_err()
    );
    assert_eq!(coordinator.head(fixture.public.domain).unwrap().sequence, 0);
    assert_eq!(*replica.observe_revision().borrow(), 0);
    {
        let inner = replica.inner.lock().await;
        inner
            .conn
            .execute("UPDATE secure_preparations SET result=?", [original])
            .await
            .unwrap();
    }
    replica
        .execute_encrypted(command, &coordinator)
        .await
        .unwrap();
    assert_eq!(coordinator.head(fixture.public.domain).unwrap().sequence, 1);
}

#[tokio::test]
async fn renewed_policy_preserves_applied_command_history_and_denies_old_invocations() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let config = fixture.config(
        &node,
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    let coordinator = coordinator(&fixture, &config);
    let owner = secure_principal_id(config.credentials.0[0].capability.subject);
    let revoked = config.credentials.0[0].id().unwrap();
    let path = dir.path().join("policy.db");
    let replica = EncryptedReplica::open(&path, fixture.app(), config)
        .await
        .unwrap();
    let old = replica
        .invoke_encrypted(
            [1; 32],
            fixture.private,
            "SetEmployee",
            b"id\nAlice\n123456",
        )
        .await
        .unwrap();
    replica
        .execute_encrypted(old.clone(), &coordinator)
        .await
        .unwrap();
    let policy = SignedPolicy::sign(
        PolicyStatement {
            manifest: fixture.app().definition().hash(),
            epoch: 2,
            revoked: [revoked].into(),
        },
        &fixture.publisher,
    )
    .unwrap();
    replica.learn_policy(policy.clone()).await.unwrap();
    assert!(
        replica
            .execute_encrypted(old.clone(), &coordinator)
            .await
            .is_err()
    );
    assert!(replica.command_snapshot(fixture.private).await.is_err());
    drop(replica);
    let user = PrivateKey::new();
    let capability = |subject, kind, delegations| Capability {
        manifest: fixture.app().definition().hash(),
        subject,
        kind,
        grants: command_grants(&fixture),
        not_before: 0,
        expires: u64::MAX,
        epoch: 2,
        delegations,
        extension: None,
    };
    let credentials = CredentialChain(vec![
        SignedCapability::sign(
            capability(user.public_key(), PrincipalKind::User, 1),
            &fixture.publisher,
        )
        .unwrap(),
        SignedCapability::sign(capability(node.public_key(), PrincipalKind::Node, 0), &user)
            .unwrap(),
    ]);
    coordinator
        .set_invokers(
            fixture.public.domain,
            owner,
            vec![(
                secure_node_id(node.public_key()),
                secure_principal_id(user.public_key()),
            )],
        )
        .unwrap();
    let mut config = fixture.config(
        &node,
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    config.policy = policy;
    config.credentials = credentials;
    let replica = EncryptedReplica::open(&path, fixture.app(), config)
        .await
        .unwrap();
    assert_eq!(
        replica
            .command_snapshot(fixture.private)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    assert!(replica.execute_encrypted(old, &coordinator).await.is_err());
    let new = replica
        .invoke_encrypted([2; 32], fixture.private, "SetEmployee", b"id\nBob\n234567")
        .await
        .unwrap();
    assert_eq!(
        replica
            .execute_encrypted(new, &coordinator)
            .await
            .unwrap()
            .receipt()
            .unwrap()
            .sequence,
        2
    );
    assert_eq!(
        replica
            .command_snapshot(fixture.private)
            .await
            .unwrap()
            .rows
            .values()
            .next()
            .unwrap(),
        &fixture.app().salary("234567")
    );
}

#[tokio::test]
async fn confirmed_history_is_audited_once_per_open_and_then_incrementally() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let config = fixture.config(
        &node,
        command_grants(&fixture),
        &[fixture.public, fixture.private],
    );
    let reopen = SecureConfig {
        publisher: config.publisher,
        manifest: config.manifest.clone(),
        policy: config.policy.clone(),
        node: config.node.clone(),
        credentials: config.credentials.clone(),
        keys: vec![
            KeyMaterial {
                scope: fixture.public,
                generation: 1,
                key: [11; 32],
            },
            KeyMaterial {
                scope: fixture.private,
                generation: 1,
                key: [22; 32],
            },
        ],
    };
    let coordinator = coordinator(&fixture, &config);
    let path = dir.path().join("audit.db");
    let replica = EncryptedReplica::open(&path, fixture.app(), config)
        .await
        .unwrap();
    let execute = async |id: u8, input: &[u8]| {
        let command = replica
            .invoke_encrypted([id; 32], fixture.private, "SetEmployee", input)
            .await
            .unwrap();
        replica.execute_encrypted(command, &coordinator).await
    };
    execute(1, b"id\nAlice\n123456").await.unwrap();
    execute(2, b"id\nAlice\n234567").await.unwrap();
    assert!(
        replica
            .command_snapshot(fixture.private)
            .await
            .unwrap()
            .complete
    );
    // The audited prefix is not read again: later commands and snapshots
    // cost the same whatever the length of the retained history.
    {
        let inner = replica.inner.lock().await;
        inner
            .conn
            .execute(
                "UPDATE secure_results SET result=x'00' WHERE sequence=1",
                (),
            )
            .await
            .unwrap();
    }
    execute(3, b"id\nAlice\n345678").await.unwrap();
    let snapshot = replica.command_snapshot(fixture.private).await.unwrap();
    assert!(snapshot.complete);
    assert_eq!(snapshot.rows.len(), 1);
    // The projection is still compared with the audited state on every read.
    {
        let inner = replica.inner.lock().await;
        inner
            .conn
            .execute("UPDATE secure_command_rows SET rows=x'00'", ())
            .await
            .unwrap();
    }
    assert!(replica.command_snapshot(fixture.private).await.is_err());
    // A reopened replica audits the whole retained history again.
    drop(replica);
    let replica = EncryptedReplica::open(&path, fixture.app(), reopen)
        .await
        .unwrap();
    assert!(replica.command_snapshot(fixture.private).await.is_err());
}
