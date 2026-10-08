//! Flush placement of the encrypted command, effect and recovery methods:
//! whatever they hand to the coordinator, an effect provider, a peer or a
//! recovery export is on the device before they return.
use super::group::lose_unflushed;
use super::*;
use accounting_app::Alice;
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{MeshId, RowApplication};

/// The same node, delegation and keys again, as a restart supplies them.
fn again(config: &SecureConfig) -> SecureConfig {
    SecureConfig {
        publisher: config.publisher,
        manifest: config.manifest.clone(),
        policy: config.policy.clone(),
        node: config.node.clone(),
        credentials: config.credentials.clone(),
        keys: config
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
/// Runs an exposing call that writes. What it committed must be flushed
/// before it returns.
async fn flushed<A: RowApplication, T>(
    label: &str,
    replica: &EncryptedReplica<A>,
    call: impl Future<Output = T>,
) -> T {
    let (before, _, _) = replica.watermark().await;
    let value = call.await;
    let (commit, durable, _) = replica.watermark().await;
    assert!(commit > before, "{label} committed nothing");
    assert!(durable >= commit, "{label} returned above the watermark");
    value
}
/// Runs an exposing call on a replica that holds unflushed commits.
async fn covers<A: RowApplication, T>(
    label: &str,
    replica: &EncryptedReplica<A>,
    call: impl Future<Output = T>,
) -> T {
    let (commit, durable, _) = replica.watermark().await;
    assert!(durable < commit, "{label} had nothing to flush");
    let value = call.await;
    let (commit, durable, _) = replica.watermark().await;
    assert!(durable >= commit, "{label} returned above the watermark");
    value
}
fn row(f: &Fixture, row: u8) -> Vec<RowChange> {
    vec![change(
        f.public,
        row,
        None,
        Some(f.app().name(&format!("row-{row}"))),
    )]
}

#[tokio::test]
async fn encrypted_command_steps_are_flushed_before_they_are_exposed() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let config = f.config(&node, commands::command_grants(&f), &[f.public, f.private]);
    let reopen = again(&config);
    let c = commands::coordinator(&f, &config);
    let (path, copy) = (dir.path().join("commands.db"), dir.path().join("copy"));
    std::fs::create_dir_all(&copy).unwrap();
    let replica = EncryptedReplica::open(&path, f.app(), config)
        .await
        .unwrap();
    replica.copy_at_flush(Some(copy.clone())).await;
    let arguments = b"employee-1\nAlice\n731994";
    // The invocation goes to the executor and the coordinator.
    replica
        .author_buffered([1; 32], row(&f, 1), vec![])
        .await
        .unwrap();
    let command = flushed(
        "invoke",
        &replica,
        replica.invoke_encrypted([1; 32], f.private, "SetEmployee", arguments),
    )
    .await
    .unwrap();
    // The prepared package is confirmed with the coordinator.
    replica
        .author_buffered([2; 32], row(&f, 2), vec![])
        .await
        .unwrap();
    let prepared = covers(
        "prepare",
        &replica,
        replica.prepare_encrypted(command.clone(), &c),
    )
    .await
    .unwrap();
    replica
        .author_buffered([3; 32], row(&f, 3), vec![])
        .await
        .unwrap();
    let result = flushed(
        "confirm",
        &replica,
        replica.confirm_encrypted(prepared, &c, &DurabilityPolicy::AsyncReplication, &[]),
    )
    .await
    .unwrap();
    // A retained result is handed to peers.
    replica
        .author_buffered([4; 32], row(&f, 4), vec![])
        .await
        .unwrap();
    covers(
        "execute",
        &replica,
        replica.execute_encrypted(command.clone(), &c),
    )
    .await
    .unwrap();
    // Never exposed: lost with the power.
    replica
        .author_buffered([5; 32], row(&f, 5), vec![])
        .await
        .unwrap();
    drop(replica);
    lose_unflushed(&copy, &path);
    let replica = EncryptedReplica::open(&path, f.app(), reopen)
        .await
        .unwrap();
    assert_eq!(replica.snapshot(f.public).await.unwrap().rows.len(), 4);
    assert_eq!(
        replica
            .command_snapshot(f.private)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    // The confirmed result survived; the retry does not confirm again.
    assert_eq!(
        replica
            .execute_encrypted(command, &c)
            .await
            .unwrap()
            .to_bytes()
            .unwrap(),
        result.to_bytes().unwrap()
    );
    assert_eq!(c.head(f.public.domain).unwrap().sequence, 1);
}

#[tokio::test]
async fn effect_attempts_and_outcomes_are_flushed_before_the_provider_or_caller_sees_them() {
    let dir = TempDir::new().unwrap();
    let publisher = PrivateKey::new();
    let node = PrivateKey::new();
    let app = || Alice::new(*publisher.public_key().as_bytes());
    let scope = app().scope(MeshId([1; 16]));
    let jobs_scope = app().jobs_scope(scope.domain.mesh);
    let config = effects::config(&app(), &publisher, &node, scope, &["CheckEvent"], false);
    let (reopen, last) = (again(&config), again(&config));
    let c = MemoryCoordinator::default();
    effects::register(&c, &config, scope, &[]);
    let (path, copy) = (dir.path().join("alice.db"), dir.path().join("copy"));
    std::fs::create_dir_all(&copy).unwrap();
    let replica = EncryptedReplica::open(&path, app(), config).await.unwrap();
    replica.copy_at_flush(Some(copy.clone())).await;
    let arguments = format!("event\n1\n{}\n1", "ab".repeat(32));
    let command = replica
        .invoke_encrypted([1; 32], scope, "CheckEvent", arguments.as_bytes())
        .await
        .unwrap();
    let prepared = replica.prepare_encrypted(command, &c).await.unwrap();
    replica
        .confirm_encrypted(prepared, &c, &DurabilityPolicy::AsyncReplication, &[])
        .await
        .unwrap();
    let job = replica.effect_jobs(jobs_scope).await.unwrap().remove(0);
    // The provider may be called as soon as the attempt is returned.
    let attempt = flushed(
        "begin_effect",
        &replica,
        replica.begin_effect(job.clone(), b"recorded input".to_vec(), false, &c),
    )
    .await
    .unwrap();
    drop(replica);
    lose_unflushed(&copy, &path);
    let replica = EncryptedReplica::open(&path, app(), reopen).await.unwrap();
    replica.copy_at_flush(Some(copy.clone())).await;
    // The started attempt is still known: the job is never sent as new.
    assert_eq!(
        replica.effect_state(&job).await.unwrap(),
        EffectState::Uncertain
    );
    assert!(
        replica
            .begin_effect(job.clone(), vec![], false, &c)
            .await
            .is_err()
    );
    // The caller acts on the recorded outcome.
    flushed(
        "finish_effect",
        &replica,
        replica.finish_effect(
            &attempt,
            EffectOutcome::Succeeded(b"provider receipt".to_vec()),
            &c,
        ),
    )
    .await
    .unwrap();
    drop(replica);
    lose_unflushed(&copy, &path);
    let replica = EncryptedReplica::open(&path, app(), last).await.unwrap();
    assert_eq!(
        replica.effect_state(&job).await.unwrap(),
        EffectState::Succeeded
    );
    assert!(
        replica
            .dispatch_effect(job, vec![], true, &c, |_| async {
                panic!("a completed job must never dispatch")
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn recovery_exports_and_custody_are_flushed_before_they_are_exposed() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a, b) = (PrivateKey::new(), PrivateKey::new());
    let ac = f.config(&a, recovery::grants(&f), &[f.public, f.private]);
    let bc = f.config(&b, recovery::grants(&f), &[]);
    let custody = again(&bc);
    let credentials = bc.credentials.clone();
    let (c, _) = recovery::coordination(&f, &[&ac, &bc]);
    let (path, copy) = (dir.path().join("b.db"), dir.path().join("copy"));
    std::fs::create_dir_all(&copy).unwrap();
    let source = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let custodian = EncryptedReplica::open(&path, f.app(), bc).await.unwrap();
    custodian.copy_at_flush(Some(copy.clone())).await;
    // An export carries every commit of its scopes off the device.
    let buffered = source
        .author_buffered([1; 32], row(&f, 1), vec![])
        .await
        .unwrap();
    let package = covers(
        "export_recovery",
        &source,
        source.export_recovery(b.public_key(), &credentials, vec![f.public], &[7; 32], &c),
    )
    .await
    .unwrap();
    assert!(source.durable_commit().await >= buffered.commit);
    // The source may drop its copy once the custodian reports custody.
    flushed(
        "retain_recovery",
        &custodian,
        custodian.retain_recovery(&package, &[7; 32]),
    )
    .await
    .unwrap();
    let id = package.commitment().unwrap();
    drop(custodian);
    lose_unflushed(&copy, &path);
    let custodian = EncryptedReplica::open(&path, f.app(), custody)
        .await
        .unwrap();
    assert!(custodian.retained_recovery(id).await.unwrap().is_some());
    let (commit, durable, _) = custodian.watermark().await;
    assert!(durable >= commit);
}
