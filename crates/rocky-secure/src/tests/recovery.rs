//! Recovery that needs no new frame: frames whose keys arrive later,
//! quarantine, durable import failure counts and bounded maintenance.
use super::blockers::named;
use super::*;
use crate::{BlockerKind, WaitReason};
use rocky_core::SqlValue;

type Replica = EncryptedReplica<Employees>;

async fn open(
    f: &Fixture,
    path: &std::path::Path,
    node: &PrivateKey,
    scopes: &[ResourceScope],
) -> Replica {
    EncryptedReplica::open(path, f.app(), f.config(node, f.full(), scopes))
        .await
        .unwrap()
}
async fn write(f: &Fixture, replica: &Replica, scope: ResourceScope, row: u8) -> EncryptedFrame {
    replica
        .author(
            [row; 32],
            vec![change(
                scope,
                row,
                None,
                Some(if scope == f.private {
                    f.app().salary("1")
                } else {
                    f.app().name("row")
                }),
            )],
            vec![],
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn frames_whose_key_arrives_later_apply_without_another_frame() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let source = open(
        &f,
        &dir.path().join("source.db"),
        &PrivateKey::new(),
        &[f.private],
    )
    .await;
    let target = open(&f, &dir.path().join("target.db"), &PrivateKey::new(), &[]).await;
    let first = write(&f, &source, f.private, 1).await;
    let second = write(&f, &source, f.private, 2).await;
    for frame in [&first, &second] {
        assert_eq!(
            target.receive(frame.clone()).await.unwrap(),
            ReceiveStatus::MissingKey
        );
    }
    let label = rocky_core::scope_label(&f.private);
    assert!(
        named(&target.blockers().await.unwrap(), &label, |kind| matches!(
            kind,
            BlockerKind::PendingFrames {
                reason: WaitReason::OpeningKey,
                ..
            }
        ))
        .is_some()
    );
    target
        .install_key(KeyMaterial::new(f.private, 1, [22; 32]))
        .await
        .unwrap();
    for frame in [&first, &second] {
        assert_eq!(
            target.receipt_status(frame.id().unwrap()).await.unwrap(),
            ReceiveStatus::Applied
        );
    }
    assert!(target.blockers().await.unwrap().is_empty());
}

/// A frame whose application fails as its own fault waits alone: another
/// log of the same group applies, and the frame applies once a key, grant,
/// schema or the tip graph changes, not before.
#[tokio::test]
async fn a_quarantined_frame_waits_alone_until_its_cause_changes() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = open(
        &f,
        &dir.path().join("a.db"),
        &PrivateKey::new(),
        &[f.public],
    )
    .await;
    let b = open(
        &f,
        &dir.path().join("b.db"),
        &PrivateKey::new(),
        &[f.public],
    )
    .await;
    let target = open(
        &f,
        &dir.path().join("target.db"),
        &PrivateKey::new(),
        &[f.public],
    )
    .await;
    let failing = write(&f, &a, f.public, 1).await;
    let other = write(&f, &b, f.public, 2).await;
    let failing_id = failing.id().unwrap();
    target.inner.lock().await.poisoned.insert(failing_id);
    for result in target
        .receive_all(vec![failing.clone(), other.clone()])
        .await
    {
        result.unwrap();
    }
    assert_eq!(
        target.receipt_status(other.id().unwrap()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_ne!(
        target.receipt_status(failing_id).await.unwrap(),
        ReceiveStatus::Applied
    );
    // Advancing another log must not retry the failed head in this pass.
    {
        let inner = target.inner.lock().await;
        assert_eq!(
            crate::storage::rows(&inner.conn, "SELECT attempts FROM secure_quarantine")
                .await
                .unwrap(),
            vec![vec![SqlValue::Integer(1)]],
        );
    }
    let label = rocky_core::scope_label(&f.public);
    let quarantined = |blockers: &[crate::Blocker]| {
        named(blockers, &label, |kind| {
            matches!(kind, BlockerKind::Quarantined { count: 1, .. })
        })
        .is_some()
    };
    assert!(quarantined(&target.blockers().await.unwrap()));
    // Fixed, but nothing it depends on changed: it waits for its backoff.
    target.inner.lock().await.poisoned.clear();
    target.recover_locally().await.unwrap();
    assert!(quarantined(&target.blockers().await.unwrap()));
    // A key arrives: every quarantined frame is due again.
    target
        .install_key(KeyMaterial::new(f.private, 1, [22; 32]))
        .await
        .unwrap();
    target.recover_locally().await.unwrap();
    assert_eq!(
        target.receipt_status(failing_id).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert!(target.blockers().await.unwrap().is_empty());
}

#[tokio::test]
async fn import_failures_count_per_scope_across_a_restart() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("target.db");
    let node = PrivateKey::new();
    let checkpoint = [5; 32];
    let target = open(&f, &path, &node, &[f.public, f.private]).await;
    for expected in 1..crate::IMPORT_ATTEMPTS {
        assert_eq!(
            target.import_failed(f.public, checkpoint).await.unwrap(),
            expected
        );
    }
    drop(target);
    let target = open(&f, &path, &node, &[f.public, f.private]).await;
    assert_eq!(
        target.import_failures(f.public, checkpoint).await.unwrap(),
        crate::IMPORT_ATTEMPTS - 1
    );
    assert_eq!(
        target.import_failures(f.private, checkpoint).await.unwrap(),
        0
    );
    assert_eq!(
        target.import_failed(f.public, checkpoint).await.unwrap(),
        crate::IMPORT_ATTEMPTS
    );
}

#[tokio::test]
async fn maintenance_finalizes_one_seal_per_step() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(
        &f,
        &dir.path().join("replica.db"),
        &PrivateKey::new(),
        &[f.public],
    )
    .await;
    write(&f, &replica, f.public, 1).await;
    for _ in 0..3 {
        seal_main(&replica, &f.publisher, f.public.domain).await;
    }
    let status = replica.compaction_status(f.public.domain).await.unwrap();
    assert!(status.uncompacted.is_empty(), "{status:?}");
    let scopes = BTreeSet::from([f.public]);
    for finalized in 1..=3 {
        replica.inner.lock().await.maintain(&scopes).await;
        let status = replica.compaction_status(f.public.domain).await.unwrap();
        assert_eq!(status.uncompacted.len(), finalized, "{status:?}");
    }
}
