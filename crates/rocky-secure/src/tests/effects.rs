use super::*;
use accounting_app::{Accounting, Alice};
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{MeshId, RowApplication, RowCommandApplication};
use std::sync::atomic::{AtomicUsize, Ordering};
pub(super) fn config<A: RowCommandApplication>(
    app: &A,
    publisher: &PrivateKey,
    node: &PrivateKey,
    scope: ResourceScope,
    methods: &[&str],
    read_only: bool,
) -> SecureConfig {
    let fields: BTreeSet<_> = app
        .manifest()
        .fields
        .iter()
        .map(|field| field.id)
        .chain(
            methods
                .iter()
                .map(|method| app.manifest().item_id("method", method)),
        )
        .collect();
    let scopes = if app.manifest().name == "alice" {
        app.command_resources(scope.domain, "CheckEvent").unwrap()
    } else {
        vec![scope]
    };
    let grants: Vec<_> = scopes
        .iter()
        .map(|scope| ResourceGrant {
            scope: *scope,
            rows: RowSelection::All,
            rights: if read_only {
                [Right::Read].into()
            } else {
                [
                    Right::Read,
                    Right::Insert,
                    Right::Update,
                    Right::Delete,
                    Right::Invoke,
                    Right::Execute,
                    Right::Custody,
                ]
                .into()
            },
            fields: fields.clone(),
        })
        .collect();
    let invocation_scope = scope;
    let user = PrivateKey::new();
    let cap = Capability {
        manifest: app.manifest().hash(),
        subject: user.public_key(),
        kind: PrincipalKind::User,
        grants: grants.clone(),
        not_before: crate::tests::lifetime().0,
        expires: crate::tests::lifetime().1,
        delegations: 1,
        extension: rocky_credentials::Extension::new(rocky_credentials::Extended::default())
            .unwrap(),
    };
    let root = SignedCapability::sign(cap.clone(), publisher).unwrap();
    let leaf = SignedCapability::sign(
        Capability {
            subject: node.public_key(),
            kind: PrincipalKind::Node,
            delegations: 0,
            ..cap
        },
        &user,
    )
    .unwrap();
    SecureConfig {
        publisher: publisher.public_key(),
        manifest: SignedManifest::sign(&app.manifest(), publisher).unwrap(),
        revocations: RevocationSet::new(),
        node: node.clone(),
        credentials: CredentialChain(vec![root, leaf]),
        keys: scopes
            .into_iter()
            .map(|scope| KeyMaterial {
                scope,
                generation: 1,
                key: if scope == invocation_scope {
                    [11; 32]
                } else {
                    [12; 32]
                },
            })
            .collect(),
    }
}
pub(super) fn register(
    c: &MemoryCoordinator,
    config: &SecureConfig,
    scope: ResourceScope,
    extra: &[&SecureConfig],
) -> rocky_core::PrincipalId {
    let owner = secure_principal_id(config.credentials.0[0].capability.subject);
    let mut nodes = vec![secure_node_id(config.node.public_key())];
    nodes.extend(extra.iter().map(|c| secure_node_id(c.node.public_key())));
    c.register(scope.domain, owner, [1; 16], nodes, 1, 1)
        .unwrap();
    let mut invokers = vec![(secure_node_id(config.node.public_key()), owner)];
    invokers.extend(extra.iter().map(|config| {
        (
            secure_node_id(config.node.public_key()),
            secure_principal_id(config.credentials.0[0].capability.subject),
        )
    }));
    c.set_invokers(scope.domain, owner, invokers).unwrap();
    c.acquire(
        scope.domain,
        owner,
        secure_node_id(config.node.public_key()),
    )
    .unwrap();
    owner
}
#[tokio::test]
async fn uncertain_send_survives_recovery_and_retries_with_stable_job_identity() {
    let dir = TempDir::new().unwrap();
    let publisher = PrivateKey::new();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let app = Alice::new(*publisher.public_key().as_bytes());
    let scope = app.scope(MeshId([1; 16]));
    let jobs_scope = app.jobs_scope(scope.domain.mesh);
    let ac = config(&app, &publisher, &a, scope, &["CheckEvent"], false);
    let bc = config(&app, &publisher, &b, scope, &["CheckEvent"], false);
    let creds = bc.credentials.clone();
    let c = MemoryCoordinator::default();
    let owner = register(&c, &ac, scope, &[&bc]);
    let source = EncryptedReplica::open(dir.path().join("alice.db"), app, ac)
        .await
        .unwrap();
    let args = format!("event\n1\n{}\n1", "ab".repeat(32));
    let command = source
        .invoke_encrypted([1; 32], scope, "CheckEvent", args.as_bytes())
        .await
        .unwrap();
    // Preparation alone exposes no runnable job.
    let prepared = source.prepare_encrypted(command, &c).await.unwrap();
    assert!(source.effect_jobs(jobs_scope).await.unwrap().is_empty());
    source
        .confirm_encrypted(prepared, &c, &DurabilityPolicy::AsyncReplication, &[])
        .await
        .unwrap();
    let job = source.effect_jobs(jobs_scope).await.unwrap().remove(0);
    assert_eq!(
        source.effect_state(&job).await.unwrap(),
        EffectState::Pending
    );
    let attempt = source
        .begin_effect(
            job.clone(),
            b"recorded time/random/human choice".to_vec(),
            false,
            &c,
        )
        .await
        .unwrap();
    // The provider may have succeeded here; no outcome was recorded before the process died.
    assert_eq!(
        source.effect_state(&job).await.unwrap(),
        EffectState::Uncertain
    );
    let package = source
        .export_recovery(
            b.public_key(),
            &creds,
            vec![scope, jobs_scope],
            &[9; 32],
            &c,
        )
        .await
        .unwrap();
    let target = EncryptedReplica::open(
        dir.path().join("recovered.db"),
        Alice::new(*publisher.public_key().as_bytes()),
        bc,
    )
    .await
    .unwrap();
    target
        .import_recovery(&package, &[9; 32], &c)
        .await
        .unwrap();
    c.takeover(scope.domain, owner, secure_node_id(b.public_key()), 1)
        .unwrap();
    let recovered = target.effect_jobs(jobs_scope).await.unwrap().remove(0);
    assert_eq!(recovered.id, job.id);
    assert_eq!(
        target.effect_state(&recovered).await.unwrap(),
        EffectState::Uncertain
    );
    assert!(
        target
            .begin_effect(recovered.clone(), vec![], false, &c)
            .await
            .is_err()
    );
    assert!(
        source
            .begin_effect(job.clone(), vec![], true, &c)
            .await
            .is_err()
    );
    // Fencing cannot retract a previously started send; the old node may still record its outcome.
    source
        .finish_effect(
            &attempt,
            EffectOutcome::Succeeded(b"provider receipt".to_vec()),
            &c,
        )
        .await
        .unwrap();
    let calls = AtomicUsize::new(0);
    let id = recovered.id;
    let call_count = &calls;
    target
        .dispatch_effect(recovered.clone(), vec![], true, &c, |job| async move {
            assert_eq!(job.id, id);
            call_count.fetch_add(1, Ordering::SeqCst);
            assert_eq!(job.payload(), b"event\n1\n1");
            EffectOutcome::Succeeded(b"same provider idempotency receipt".to_vec())
        })
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        target.effect_state(&recovered).await.unwrap(),
        EffectState::Succeeded
    );
    assert!(
        target
            .dispatch_effect(recovered, vec![], true, &c, |_| async {
                panic!("a completed job must never dispatch")
            })
            .await
            .is_err()
    );
}
#[tokio::test]
async fn alice_receipt_does_not_authorize_accounting_and_current_conditions_are_rechecked() {
    let dir = TempDir::new().unwrap();
    let publisher = PrivateKey::new();
    let node = PrivateKey::new();
    let app = Accounting::new(*publisher.public_key().as_bytes());
    let scope = app.scope(MeshId([1; 16]));
    let ac = config(
        &app,
        &publisher,
        &node,
        scope,
        &["SetEvent", "ReviewEvent"],
        false,
    );
    let reader_key = PrivateKey::new();
    let reader_config = config(&app, &publisher, &reader_key, scope, &["ReviewEvent"], true);
    let reader_creds = reader_config.credentials.clone();
    let c = MemoryCoordinator::default();
    register(&c, &ac, scope, &[]);
    let accounting = EncryptedReplica::open(dir.path().join("accounting.db"), app, ac)
        .await
        .unwrap();
    let command = accounting
        .invoke_encrypted([1; 32], scope, "SetEvent", b"event\n1\n1")
        .await
        .unwrap();
    let result = accounting.execute_encrypted(command, &c).await.unwrap();
    let reader = EncryptedReplica::open(
        dir.path().join("reader.db"),
        Accounting::new(*publisher.public_key().as_bytes()),
        reader_config,
    )
    .await
    .unwrap();
    let authorized = accounting
        .encrypted_result_for(
            reader_key.public_key(),
            &reader_creds,
            scope.domain,
            1,
            &[scope],
        )
        .await
        .unwrap()
        .unwrap();
    reader
        .receive_encrypted_result(authorized, &c)
        .await
        .unwrap();
    let snapshot = reader.command_snapshot(scope).await.unwrap();
    assert!(snapshot.complete);
    // Alice's recorded observation comes from an authorized Accounting read; it is not a write grant.
    let alice_node = PrivateKey::new();
    let alice_app = Alice::new(*publisher.public_key().as_bytes());
    let alice_scope = alice_app.scope(scope.domain.mesh);
    let jobs = alice_app.jobs_scope(scope.domain.mesh);
    let config = config(
        &alice_app,
        &publisher,
        &alice_node,
        alice_scope,
        &["CheckEvent"],
        false,
    );
    register(&c, &config, alice_scope, &[]);
    let alice = EncryptedReplica::open(dir.path().join("alice.db"), alice_app, config)
        .await
        .unwrap();
    let head = result
        .receipt()
        .unwrap()
        .confirmation
        .new_head
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let args = format!("event\n1\n{head}\n1");
    let command = alice
        .invoke_encrypted([2; 32], alice_scope, "CheckEvent", args.as_bytes())
        .await
        .unwrap();
    alice.execute_encrypted(command, &c).await.unwrap();
    let job = alice.effect_jobs(jobs).await.unwrap().remove(0);
    assert!(
        reader
            .invoke_encrypted([2; 32], scope, "ReviewEvent", job.payload())
            .await
            .is_err()
    );
    let advance = accounting
        .invoke_encrypted([3; 32], scope, "SetEvent", b"event\n2\n0")
        .await
        .unwrap();
    accounting.execute_encrypted(advance, &c).await.unwrap();
    let accounting_ref = &accounting;
    let coordinator_ref = &c;
    let outcome = alice
        .dispatch_effect(job.clone(), vec![], false, &c, |job| async move {
            let command = accounting_ref
                .invoke_encrypted([4; 32], scope, "ReviewEvent", job.payload())
                .await
                .unwrap();
            match accounting_ref
                .execute_encrypted(command, coordinator_ref)
                .await
            {
                Ok(_) => EffectOutcome::Succeeded(vec![]),
                Err(error) => EffectOutcome::Failed(error.to_string().into_bytes()),
            }
        })
        .await
        .unwrap();
    assert!(matches!(outcome, EffectOutcome::Failed(_)));
    assert_eq!(alice.effect_state(&job).await.unwrap(), EffectState::Failed);
    // Revocation denies further job starts even while old keys remain installed.
    let root = alice.inner.lock().await.config.credentials.0[0]
        .id()
        .unwrap();
    alice
        .learn_revocations(&crate::tests::revoking(
            &publisher,
            Alice::new(*publisher.public_key().as_bytes())
                .manifest()
                .app,
            &[],
            &[root],
        ))
        .await
        .unwrap();
    assert!(alice.begin_effect(job, vec![], true, &c).await.is_err());
}

#[tokio::test]
async fn http_webhook_retries_provider_key_after_send_success_record_failure() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    let dir = TempDir::new().unwrap();
    let publisher = PrivateKey::new();
    let node = PrivateKey::new();
    let app = Alice::new(*publisher.public_key().as_bytes());
    let scope = app.scope(MeshId([1; 16]));
    let jobs = app.jobs_scope(scope.domain.mesh);
    let config = config(&app, &publisher, &node, scope, &["CheckEvent"], false);
    let c = MemoryCoordinator::default();
    register(&c, &config, scope, &[]);
    let replica = EncryptedReplica::open(dir.path().join("alice.db"), app, config)
        .await
        .unwrap();
    let args = format!("event\n1\n{}\n1", "ab".repeat(32));
    let command = replica
        .invoke_encrypted([1; 32], scope, "CheckEvent", args.as_bytes())
        .await
        .unwrap();
    replica.execute_encrypted(command, &c).await.unwrap();
    let job = replica.effect_jobs(jobs).await.unwrap().remove(0);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let key = job
        .id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let server = async {
        let mut seen = BTreeSet::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).await.unwrap();
            let text = String::from_utf8(request).unwrap();
            let provider_key = text
                .lines()
                .find_map(|line| line.strip_prefix("Idempotency-Key: "))
                .unwrap()
                .to_string();
            assert_eq!(provider_key, key);
            seen.insert(provider_key);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\nprovider-result").await.unwrap();
        }
        assert_eq!(seen.len(), 1); // Two sends represent one provider-side business effect.
    };
    let replica_ref = &replica;
    let client = async {
        for retry in [false, true] {
            let result=replica.dispatch_effect(job.clone(),b"recorded provider choice".to_vec(),retry,&c,|job|async move {
                // Re-enter a read during dispatch to prove the runtime lock has been released.
                assert!(replica_ref.command_snapshot(scope).await.unwrap().complete);
                let mut stream=TcpStream::connect(address).await.unwrap();let key=job.id.iter().map(|byte|format!("{byte:02x}")).collect::<String>();let request=format!("POST /review HTTP/1.1\r\nHost: localhost\r\nIdempotency-Key: {key}\r\nContent-Length: {}\r\n\r\n",job.payload().len());stream.write_all(request.as_bytes()).await.unwrap();stream.write_all(job.payload()).await.unwrap();stream.shutdown().await.unwrap();let mut response=Vec::new();stream.read_to_end(&mut response).await.unwrap();
                if !retry { replica_ref.inject_failure(0,1).await; }
                EffectOutcome::Succeeded(response)
            }).await;
            if !retry {
                assert!(result.is_err());
                assert_eq!(
                    replica.effect_state(&job).await.unwrap(),
                    EffectState::Uncertain
                );
            } else {
                result.unwrap();
                assert_eq!(
                    replica.effect_state(&job).await.unwrap(),
                    EffectState::Succeeded
                );
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(server, client);
    })
    .await
    .unwrap();
}
