//! Encrypted strong retention, bootstrap, identity recovery and bounded reopen.
use super::commands::{command_grants, coordinator};
use super::*;
use rocky_core::Coordinator;

type Replica = EncryptedReplica<Employees>;
fn handle(n: u32) -> Commitment {
    let mut id = [0; 32];
    id[..4].copy_from_slice(&n.to_le_bytes());
    id
}
fn clone_config(c: &SecureConfig) -> SecureConfig {
    SecureConfig {
        publisher: c.publisher,
        manifest: c.manifest.clone(),
        policy: c.policy.clone(),
        node: c.node.clone(),
        credentials: c.credentials.clone(),
        keys: c
            .keys
            .iter()
            .map(|key| KeyMaterial {
                scope: key.scope,
                generation: key.generation,
                key: key.key,
            })
            .collect(),
    }
}
async fn count(replica: &Replica, sql: &str) -> i64 {
    let inner = replica.inner.lock().await;
    let mut result = inner.conn.query(sql, ()).await.unwrap();
    result.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}
async fn retain(f: &Fixture, replica: &Replica, required: &[PublicKey], window: usize) {
    for scope in [f.public, f.private] {
        replica
            .set_retention_policy(
                scope,
                RetentionPolicy {
                    required: required.iter().copied().collect(),
                    window,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
}
async fn command(f: &Fixture, replica: &Replica, c: &dyn Coordinator, n: u32) -> ConfirmedCommand {
    let command = replica
        .invoke_encrypted(
            handle(n),
            f.private,
            "SetEmployee",
            b"employee-1\nAlice\n731994",
        )
        .await
        .unwrap();
    replica.execute_encrypted(command, c).await.unwrap()
}
async fn bootstrap(f: &Fixture, source: &Replica, target: &Replica, config: &SecureConfig) {
    for scope in [f.public, f.private] {
        let signed = source.checkpoint(scope).await.unwrap();
        assert_eq!(signed.statement.format, 6);
        assert_eq!(signed.statement.strong.base, signed.statement.strong.head);
        target.begin_rebootstrap(signed.clone()).await.unwrap();
        let mut cursor = None;
        loop {
            let page = source
                .checkpoint_page(
                    config.node.public_key(),
                    &config.credentials,
                    scope,
                    signed.id().unwrap(),
                    cursor,
                    MAX_SECURE_FRAME,
                )
                .await
                .unwrap();
            cursor = page.next;
            target.stage_checkpoint_page(page).await.unwrap();
            if cursor.is_none() {
                break;
            }
        }
        target.finish_rebootstrap(scope).await.unwrap();
    }
}

#[tokio::test]
async fn executor_and_reader_cross_commands_with_a_window_and_recover_from_the_base() {
    cross_commands_with_a_window(200).await;
}

#[tokio::test]
#[ignore = "soak; run by scripts/check --slow"]
async fn soak_executor_and_reader_cross_4096_commands_with_a_window() {
    cross_commands_with_a_window(4100).await;
}

async fn cross_commands_with_a_window(total: u32) {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a_key = PrivateKey::new();
    let b_key = PrivateKey::new();
    let ac = f.config(&a_key, command_grants(&f), &[f.public, f.private]);
    let bc = f.config(&b_key, command_grants(&f), &[f.public, f.private]);
    let c = rocky_coordinator::MemoryCoordinator::default();
    let owner = super::effects::register(&c, &ac, f.public, &[&bc]);
    let a = EncryptedReplica::open(dir.path().join("a.db"), f.app(), clone_config(&ac))
        .await
        .unwrap();
    let b_path = dir.path().join("b.db");
    let b = EncryptedReplica::open(&b_path, f.app(), clone_config(&bc))
        .await
        .unwrap();
    retain(&f, &a, &[b_key.public_key()], 16).await;
    retain(&f, &b, &[], 16).await;
    let mut size_at_100 = 0;
    let mut receipts = std::collections::VecDeque::new();
    for n in 1..=total {
        let result = command(&f, &a, &c, n).await;
        b.receive_encrypted_result(result.clone(), &c)
            .await
            .unwrap();
        let states = b.result_ack(&result).await.unwrap();
        a.acknowledge_result(b_key.public_key(), &bc.credentials, &result, &states)
            .await
            .unwrap();
        receipts.push_back(
            result
                .receipt()
                .unwrap()
                .confirmation
                .request
                .sequence
                .get(),
        );
        if receipts.len() > 24 {
            c.retire(
                f.public.domain,
                secure_node_id(a_key.public_key()),
                receipts.pop_front().unwrap(),
            )
            .unwrap();
        }
        if n % 100 == 0 {
            for replica in [&a, &b] {
                assert_eq!(
                    count(replica, "SELECT COUNT(*) FROM secure_results").await,
                    16
                );
                assert_eq!(
                    count(replica, "SELECT COUNT(*) FROM secure_preparations").await,
                    0
                );
                assert!(count(replica, "SELECT COUNT(*) FROM secure_commands").await <= 16);
            }
            if n % 1000 == 0 {
                eprintln!("strong retention progress: {n} confirmations");
            }
            let bytes = count(&a, "SELECT SUM(LENGTH(result)) FROM secure_results").await;
            if n == 100 {
                size_at_100 = bytes;
            }
            assert!(bytes <= size_at_100 * 11 / 10);
        }
    }
    assert!(
        a.missing_results(f.public.domain, u64::from(total), 64)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        a.invoke_encrypted(
            handle(1),
            f.private,
            "SetEmployee",
            b"employee-1\nAlice\n731994"
        )
        .await,
        Err(Error::Pruned { .. })
    ));
    assert!(matches!(
        a.invoke_encrypted(handle(1), f.private, "SetEmployee", b"different")
            .await,
        Err(Error::Conflict)
    ));
    let expected = a.command_snapshot(f.public).await.unwrap().rows;
    b.flush().await.unwrap();
    drop(b);
    let b = EncryptedReplica::open(&b_path, f.app(), clone_config(&bc))
        .await
        .unwrap();
    assert_eq!(b.command_snapshot(f.public).await.unwrap().rows, expected);
    assert!(b.command_snapshot(f.private).await.unwrap().complete);
    // A fresh identity verifies recovery from the base, with no genesis prefix.
    let package = a
        .export_recovery(
            b_key.public_key(),
            &bc.credentials,
            vec![f.public, f.private],
            &[9; 32],
            &c,
        )
        .await
        .unwrap();
    let recovered =
        EncryptedReplica::open(dir.path().join("recovered.db"), f.app(), clone_config(&bc))
            .await
            .unwrap();
    recovered
        .import_recovery(&package, &[9; 32], &c)
        .await
        .unwrap();
    assert_eq!(
        recovered.command_snapshot(f.public).await.unwrap().rows,
        expected
    );
    assert!(matches!(
        recovered
            .invoke_encrypted(
                handle(1),
                f.private,
                "SetEmployee",
                b"employee-1\nAlice\n731994"
            )
            .await,
        Err(Error::Pruned { .. })
    ));
    // A peer below the horizon imports current state and becomes the next executor.
    let behind = EncryptedReplica::open(dir.path().join("behind.db"), f.app(), clone_config(&bc))
        .await
        .unwrap();
    assert_eq!(
        behind
            .missing_results(f.public.domain, u64::from(total), 1)
            .await
            .unwrap(),
        vec![1]
    );
    assert!(
        a.encrypted_result_for(
            b_key.public_key(),
            &bc.credentials,
            f.public.domain,
            1,
            &[f.public, f.private]
        )
        .await
        .unwrap()
        .is_none()
    );
    bootstrap(&f, &a, &behind, &bc).await;
    assert!(
        behind
            .missing_results(f.public.domain, u64::from(total), 64)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        behind.command_snapshot(f.public).await.unwrap().rows,
        expected
    );
    c.takeover(
        f.public.domain,
        owner,
        secure_node_id(b_key.public_key()),
        1,
    )
    .unwrap();
    command(&f, &behind, &c, total + 1).await;
    assert_eq!(
        c.head(f.public.domain).unwrap().sequence,
        u64::from(total) + 1
    );
    a.acknowledge_tickets(&c).await.unwrap();
    eprintln!(
        "{total} confirmations: executor/reader retain 16 results, bytes {size_at_100}, no preparations; bootstrap + recovery + next confirmation pass"
    );
}

#[tokio::test]
async fn missing_and_custody_acknowledgments_block_the_domain_horizon_and_prune_is_atomic() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a_key = PrivateKey::new();
    let b_key = PrivateKey::new();
    let ac = f.config(&a_key, command_grants(&f), &[f.public, f.private]);
    let bc = f.config(&b_key, command_grants(&f), &[f.public, f.private]);
    let c = coordinator(&f, &ac);
    let a = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    retain(&f, &a, &[b_key.public_key()], 2).await;
    let mut last = None;
    for n in 1..=12 {
        last = Some(command(&f, &a, &c, n).await);
    }
    let last = last.unwrap();
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_results").await, 12);
    a.acknowledge_result(
        b_key.public_key(),
        &bc.credentials,
        &last,
        &[(f.public, true), (f.private, false)],
    )
    .await
    .unwrap();
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_results").await, 12);
    a.inject_stage_fault(crate::replica::checkpoint::Stage::Prune, 0)
        .await;
    a.acknowledge_result(
        b_key.public_key(),
        &bc.credentials,
        &last,
        &[(f.public, true), (f.private, true)],
    )
    .await
    .unwrap();
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_results").await, 12);
    assert_eq!(
        a.inner
            .lock()
            .await
            .strong_base(f.public)
            .await
            .unwrap()
            .sequence(),
        0
    );
    a.inner
        .lock()
        .await
        .prune_strong(f.public.domain)
        .await
        .unwrap();
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_results").await, 2);
    assert_eq!(
        a.inner
            .lock()
            .await
            .strong_base(f.public)
            .await
            .unwrap()
            .sequence(),
        10
    );
    assert!(a.command_snapshot(f.public).await.unwrap().complete);
    // Behind excludes a missing reader, and the latest result stays even at window zero.
    for scope in [f.public, f.private] {
        a.set_retention_policy(
            scope,
            RetentionPolicy {
                required: [b_key.public_key()].into(),
                behind: [b_key.public_key()].into(),
                window: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    a.inner
        .lock()
        .await
        .prune_strong(f.public.domain)
        .await
        .unwrap();
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_results").await, 1);
}

fn epoch_config(f: &Fixture, node: &PrivateKey, epoch: u64, revoked: &[PublicKey]) -> SecureConfig {
    let user = PrivateKey::new();
    let capability = |subject, kind, delegations| Capability {
        manifest: f.app().definition().hash(),
        subject,
        kind,
        grants: command_grants(f),
        not_before: 0,
        expires: u64::MAX,
        epoch,
        delegations,
        extension: rocky_credentials::Extension::new(rocky_credentials::Extended::default())
            .unwrap(),
    };
    SecureConfig {
        publisher: f.publisher.public_key(),
        manifest: f.manifest.clone(),
        node: node.clone(),
        policy: SignedPolicy::sign(
            PolicyStatement {
                manifest: f.app().definition().hash(),
                epoch,
                revoked: revoked
                    .iter()
                    .map(rocky_credentials::revoked_subject)
                    .collect(),
            },
            &f.publisher,
        )
        .unwrap(),
        credentials: CredentialChain(vec![
            SignedCapability::sign(
                capability(user.public_key(), PrincipalKind::User, 1),
                &f.publisher,
            )
            .unwrap(),
            SignedCapability::sign(capability(node.public_key(), PrincipalKind::Node, 0), &user)
                .unwrap(),
        ]),
        keys: [f.public, f.private]
            .iter()
            .map(|scope| KeyMaterial {
                scope: *scope,
                generation: epoch,
                key: if epoch == 1 {
                    if *scope == f.public {
                        [11; 32]
                    } else {
                        [22; 32]
                    }
                } else {
                    if *scope == f.public {
                        [77; 32]
                    } else {
                        [88; 32]
                    }
                },
            })
            .collect(),
    }
}

#[tokio::test]
async fn writer_enrolled_after_revocation_bootstraps_old_mergeable_and_confirmed_rows() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a_key = PrivateKey::new();
    let b_key = PrivateKey::new();
    let revoked = PrivateKey::new();
    let ac = epoch_config(&f, &a_key, 1, &[]);
    let next = epoch_config(&f, &a_key, 2, &[revoked.public_key()]);
    let bc = epoch_config(&f, &b_key, 2, &[revoked.public_key()]);
    let c = rocky_coordinator::MemoryCoordinator::default();
    let owner = super::effects::register(&c, &ac, f.public, &[&bc]);
    let a = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let frame = a
        .author(
            handle(50),
            vec![change(f.public, 1, None, Some(f.app().name("Old listing")))],
            vec![],
        )
        .await
        .unwrap();
    command(&f, &a, &c, 1).await;
    a.learn_policy(next.policy.clone()).await.unwrap();
    a.renew_credentials(next.credentials.clone()).await.unwrap();
    for key in next.keys {
        a.install_key_epoch(key).await.unwrap();
    }
    let b = EncryptedReplica::open(dir.path().join("b.db"), f.app(), clone_config(&bc))
        .await
        .unwrap();
    assert!(b.receive(frame).await.is_err());
    bootstrap(&f, &a, &b, &bc).await;
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 1);
    assert!(b.command_snapshot(f.public).await.unwrap().complete);
    assert_eq!(
        b.command_snapshot(f.public).await.unwrap().rows,
        a.command_snapshot(f.public).await.unwrap().rows
    );
    b.reconcile_encrypted_results(&c).await.unwrap();
    c.takeover(
        f.public.domain,
        owner,
        secure_node_id(b_key.public_key()),
        1,
    )
    .unwrap();
    command(&f, &b, &c, 2).await;
    assert_eq!(c.head(f.public.domain).unwrap().sequence, 2);
}

#[tokio::test]
async fn redundant_and_stale_preparations_go_but_unknown_outcomes_stay() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let config = f.config(&key, command_grants(&f), &[f.public, f.private]);
    let c = coordinator(&f, &config);
    let a = EncryptedReplica::open(dir.path().join("a.db"), f.app(), config)
        .await
        .unwrap();
    retain(&f, &a, &[], 2).await;
    let invocation = a
        .invoke_encrypted(
            handle(1),
            f.private,
            "SetEmployee",
            b"employee-1\nAlice\n731994",
        )
        .await
        .unwrap();
    a.prepare_encrypted(invocation.clone(), &c).await.unwrap();
    command(&f, &a, &c, 2).await;
    assert_eq!(
        count(&a, "SELECT COUNT(*) FROM secure_preparations").await,
        1
    );
    a.execute_encrypted(invocation, &c).await.unwrap();
    assert_eq!(
        count(&a, "SELECT COUNT(*) FROM secure_preparations").await,
        0
    );
    assert_eq!(
        count(&a, "SELECT COUNT(*) FROM secure_preparation_requests").await,
        0
    );
    let invocation = a
        .invoke_encrypted(
            handle(3),
            f.private,
            "SetEmployee",
            b"employee-1\nAlice\n731994",
        )
        .await
        .unwrap();
    c.drop_next_reply().unwrap();
    assert!(a.execute_encrypted(invocation.clone(), &c).await.is_err());
    a.inner
        .lock()
        .await
        .prune_strong(f.public.domain)
        .await
        .unwrap();
    assert_eq!(
        count(&a, "SELECT COUNT(*) FROM secure_preparations").await,
        1
    );
    a.execute_encrypted(invocation, &c).await.unwrap();
    assert_eq!(
        count(&a, "SELECT COUNT(*) FROM secure_preparations").await,
        0
    );
}

#[tokio::test]
async fn unobserved_and_uncertain_jobs_pin_results_until_a_finished_observation() {
    let dir = TempDir::new().unwrap();
    let publisher = PrivateKey::new();
    let node = PrivateKey::new();
    let app = accounting_app::Alice::new(*publisher.public_key().as_bytes());
    let scope = app.scope(MeshId([1; 16]));
    let jobs = app.jobs_scope(scope.domain.mesh);
    let config = super::effects::config(&app, &publisher, &node, scope, &["CheckEvent"], false);
    let c = rocky_coordinator::MemoryCoordinator::default();
    super::effects::register(&c, &config, scope, &[]);
    let a = EncryptedReplica::open(dir.path().join("a.db"), app, config)
        .await
        .unwrap();
    for scope in [scope, jobs] {
        a.set_retention_policy(
            scope,
            RetentionPolicy {
                window: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    for n in 1..=3 {
        let input = format!("event-{n}\n1\n{}\n1", "ab".repeat(32));
        let command = a
            .invoke_encrypted(handle(n), scope, "CheckEvent", input.as_bytes())
            .await
            .unwrap();
        a.execute_encrypted(command, &c).await.unwrap();
    }
    let job = a.effect_jobs(jobs).await.unwrap().remove(0);
    let attempt = a
        .begin_effect(job.clone(), vec![], false, &c)
        .await
        .unwrap();
    a.inner
        .lock()
        .await
        .prune_strong(scope.domain)
        .await
        .unwrap();
    let mut records = a
        .inner
        .lock()
        .await
        .conn
        .query("SELECT COUNT(*) FROM secure_results", ())
        .await
        .unwrap();
    assert_eq!(
        records
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        3
    );
    a.finish_effect(&attempt, EffectOutcome::Succeeded(vec![]), &c)
        .await
        .unwrap();
    a.inner
        .lock()
        .await
        .prune_strong(scope.domain)
        .await
        .unwrap();
    let mut records = a
        .inner
        .lock()
        .await
        .conn
        .query("SELECT COUNT(*) FROM secure_results", ())
        .await
        .unwrap();
    assert_eq!(
        records
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        2
    );
    assert_eq!(a.effect_state(&job).await.unwrap(), EffectState::Succeeded);
    assert!(a.command_snapshot(scope).await.unwrap().complete);
}

#[tokio::test]
async fn base_corruption_and_a_recovery_suffix_gap_are_refused() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let target_node = PrivateKey::new();
    let config = f.config(&node, command_grants(&f), &[f.public, f.private]);
    let target_config = f.config(&target_node, command_grants(&f), &[f.public, f.private]);
    let c = coordinator(&f, &config);
    let a = EncryptedReplica::open(dir.path().join("a.db"), f.app(), config)
        .await
        .unwrap();
    retain(&f, &a, &[], 2).await;
    for n in 1..=6 {
        command(&f, &a, &c, n).await;
    }
    let checkpoint = a.checkpoint(f.public).await.unwrap();
    assert_eq!(
        (
            checkpoint.statement.strong.results.count,
            checkpoint.statement.strong.commands.count,
            checkpoint.statement.strong.preparations.count
        ),
        (0, 0, 0)
    );
    let mut state = a.inner.lock().await.capture_strong(f.public).await.unwrap();
    state.rows.clear();
    assert!(state.binds(&checkpoint).is_err());
    let mut incompatible = checkpoint.clone();
    incompatible.statement.format = 2;
    assert!(incompatible.verify_signature().is_err());
    {
        let mut inner = a.inner.lock().await;
        let mut base = inner.strong_base(f.public).await.unwrap();
        base.rows.clear();
        inner.put_strong_base(f.public, &base).await.unwrap();
        inner.audited.clear();
    }
    assert!(a.command_snapshot(f.public).await.is_err());
    // Restore the authentic base, then remove the retained predecessor of the latest result.
    {
        let inner = a.inner.lock().await;
        let mut base = inner.strong_base(f.private).await.unwrap();
        let result = base.latest.as_ref().unwrap();
        let part = result
            .parts
            .iter()
            .find(|part| part.scope == f.public && part.kind == PackageKind::Rows)
            .unwrap();
        let projection: crate::command::RowProjection =
            decode(&part.open(&[11; 32]).unwrap()).unwrap();
        base.rows = projection.after;
        inner.put_strong_base(f.public, &base).await.unwrap();
        inner
            .conn
            .execute("DELETE FROM secure_results WHERE sequence=5", ())
            .await
            .unwrap();
    }
    assert!(
        a.export_recovery(
            target_node.public_key(),
            &target_config.credentials,
            vec![f.public, f.private],
            &[9; 32],
            &c
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn offers_refresh_past_the_strong_horizon_and_partial_readers_prune_their_suffix() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a_key = PrivateKey::new();
    let b_key = PrivateKey::new();
    let ac = f.config(&a_key, command_grants(&f), &[f.public, f.private]);
    let bc = f.config(
        &b_key,
        vec![grant(f.public, &[Right::Read, Right::Custody], &[])],
        &[f.public],
    );
    let c = coordinator(&f, &ac);
    let a = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let b = EncryptedReplica::open(dir.path().join("b.db"), f.app(), clone_config(&bc))
        .await
        .unwrap();
    retain(&f, &a, &[], 2).await;
    b.set_retention_policy(
        f.public,
        RetentionPolicy {
            window: 2,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let old = a.checkpoint(f.public).await.unwrap();
    for n in 1..=12 {
        command(&f, &a, &c, n).await;
        let result = a
            .encrypted_result_for(
                b_key.public_key(),
                &bc.credentials,
                f.public.domain,
                n as u64,
                &[f.public],
            )
            .await
            .unwrap()
            .unwrap();
        b.receive_encrypted_result(result, &c).await.unwrap();
    }
    assert_eq!(count(&b, "SELECT COUNT(*) FROM secure_results").await, 2);
    assert!(b.command_snapshot(f.public).await.unwrap().complete);
    let offer = a
        .checkpoint_offer(b_key.public_key(), &bc.credentials, f.public)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(offer.id().unwrap(), old.id().unwrap());
    assert_eq!(offer.statement.strong.base.unwrap().0, 12);
}
