//! A replica killed at a replay crash point reopens and converges: the
//! interrupted apply, rewind, redo or undo deletion left nothing half done.
use super::*;
use crate::CrashPoint;

/// Delivers `before` cleanly, arms `point`, delivers `during` (or seals A
/// for undo deletion), expects the point to fire, drops the replica
/// unclosed, reopens it and delivers everything again.
async fn crash(point: CrashPoint, before: &[usize], during: &[usize]) {
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let root = tempfile::tempdir().unwrap();
    let units = history(&f, root.path(), true).await;
    let frames: Vec<_> = units.iter().flatten().cloned().collect();
    let expected = canonical(&f, &frames);
    let path = root.path().join("receiver.sqlite");
    let node = PrivateKey::new();
    let replica = receiver(&f, &path, &node).await;
    for unit in before {
        for status in replica.receive_all(units[*unit].clone()).await {
            assert_ne!(status.unwrap(), ReceiveStatus::Rejected);
        }
    }
    replica.arm_crash(Some(point)).await;
    if point == CrashPoint::UndoDeletion {
        assert!(replica.seal_replayed_for_test(f.scope("A")).await.is_err());
    } else {
        for unit in during {
            // The interrupted receive fails; nothing of it may remain.
            let _ = replica.receive_all(units[*unit].clone()).await;
        }
    }
    assert_eq!(replica.crashed().await, Some(point));
    let unsealed = replica.unsealed_replayed_transactions().await.unwrap();
    drop(replica);

    // Received frames were kept before the interrupted step: the reopened
    // replica finishes it whole, as a replay of what it was given.
    let replica = receiver(&f, &path, &node).await;
    let given: Vec<_> = before
        .iter()
        .chain(during)
        .flat_map(|unit| units[*unit].clone())
        .collect();
    assert_eq!(
        state(&f, &replica, &canonical(&f, &given)).await,
        canonical(&f, &given)
    );
    if point == CrashPoint::UndoDeletion {
        // The seal did not happen: every undo record is still there.
        assert_eq!(
            replica.unsealed_replayed_transactions().await.unwrap(),
            unsealed
        );
    }
    for status in replica.receive_all(frames).await {
        assert_ne!(status.unwrap(), ReceiveStatus::Rejected);
    }
    assert_eq!(state(&f, &replica, &expected).await, expected);
    replica.seal_replayed_for_test(f.scope("A")).await.unwrap();
    replica.seal_replayed_for_test(f.scope("B")).await.unwrap();
    assert_eq!(state(&f, &replica, &expected).await.rows, expected.rows);
}

#[tokio::test]
async fn a_crash_while_applying_an_intent_reopens_and_converges() {
    crash(CrashPoint::Apply, &[], &[0]).await;
}

#[tokio::test]
async fn a_crash_inside_a_rewind_reopens_and_converges() {
    // W1's doubling applies first; its earlier create rewinds it.
    crash(CrashPoint::Rewind, &[1, 2, 3], &[0]).await;
}

#[tokio::test]
async fn a_crash_between_redone_transactions_reopens_and_converges() {
    crash(CrashPoint::Redo, &[1, 2, 3], &[0]).await;
}

#[tokio::test]
async fn a_crash_while_deleting_undo_records_reopens_and_converges() {
    crash(CrashPoint::UndoDeletion, &[0, 1, 2, 3, 4], &[]).await;
}

#[tokio::test]
async fn a_crash_right_after_a_rewinding_receive_commits_reopens_and_converges() {
    crash(CrashPoint::GroupAfterCommit, &[1, 2, 3], &[0]).await;
}
