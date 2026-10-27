use super::*;
use crate::replica::group::GroupFault;
use crate::storage::rows;
use rocky_core::{RowApplication, SqlValue};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn insert(f: &Fixture, row: u8) -> Vec<RowChange> {
    vec![change(
        f.public,
        row,
        None,
        Some(f.app().name(&format!("row-{row}"))),
    )]
}
async fn open(
    f: &Fixture,
    path: &std::path::Path,
    node: &PrivateKey,
) -> EncryptedReplica<Employees> {
    EncryptedReplica::open(path, f.app(), f.config(node, f.full(), &[f.public]))
        .await
        .unwrap()
}
async fn frames(replica: &EncryptedReplica<Employees>, f: &Fixture, node: &PrivateKey) -> usize {
    let credentials = f.credentials(node, f.full());
    let mut count = 0;
    let mut cursor = None;
    loop {
        let page = replica
            .transfer_page(
                node.public_key(),
                &credentials,
                &[f.public],
                cursor,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        count += page.frames.len();
        cursor = page.next;
        if cursor.is_none() {
            return count;
        }
    }
}

#[tokio::test]
async fn open_verifies_buffered_commits_full_flushes_and_manual_checkpoints() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("a.db"), &PrivateKey::new()).await;
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    for (pragma, expected) in [
        ("PRAGMA journal_mode", SqlValue::Text("wal".into())),
        ("PRAGMA synchronous", SqlValue::Integer(1)),
        ("PRAGMA fullfsync", SqlValue::Integer(1)),
        ("PRAGMA checkpoint_fullfsync", SqlValue::Integer(1)),
        ("PRAGMA wal_autocheckpoint", SqlValue::Integer(0)),
    ] {
        assert_eq!(
            rows(&inner.conn, pragma).await.unwrap(),
            vec![vec![expected]],
            "{pragma}"
        );
    }
}

#[tokio::test]
async fn explicit_wal_flush_covers_buffered_writes_without_changing_contents() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.db");
    let replica = open(&f, &path, &PrivateKey::new()).await;
    let buffered = replica
        .author_buffered([1; 32], insert(&f, 1), vec![])
        .await
        .unwrap();
    assert!(replica.durable_commit().await < buffered.commit);
    let wal = path.with_extension("db-wal");
    let contents = std::fs::read(&wal).unwrap();
    #[cfg(windows)]
    assert_eq!(
        std::fs::File::open(&wal)
            .unwrap()
            .sync_all()
            .unwrap_err()
            .raw_os_error(),
        Some(5)
    );
    assert!(replica.flush().await.unwrap() >= buffered.commit);
    assert_eq!(std::fs::read(wal).unwrap(), contents);
}

#[tokio::test]
async fn concurrent_authors_share_one_commit_and_keep_their_own_frames() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = open(&f, &dir.path().join("a.db"), &node).await;
    let mut observer = replica.observe_revision();
    let before = replica.group_commits().await;
    let mut tasks = tokio::task::JoinSet::new();
    for row in 1..=8u8 {
        let (replica, changes) = (replica.clone(), insert(&f, row));
        tasks.spawn(async move {
            let frame = replica.author([row; 32], changes, vec![]).await;
            (row, frame)
        });
    }
    let mut authored = BTreeMap::new();
    while let Some(result) = tasks.join_next().await {
        let (row, frame) = result.unwrap();
        authored.insert(row, frame.unwrap());
    }
    let commits = replica.group_commits().await - before;
    assert!(commits < 8, "{commits} commits for 8 requests");
    assert_eq!(commits, 1);
    let sequences: BTreeSet<_> = authored
        .values()
        .map(|frame| frame.operation().unwrap().header.seq_num)
        .collect();
    assert_eq!(sequences, (0..8).collect());
    for (row, frame) in &authored {
        // Each caller got the frame stored for its own handle.
        let again = replica
            .author([*row; 32], insert(&f, *row), vec![])
            .await
            .unwrap();
        assert_eq!(again.to_bytes().unwrap(), frame.to_bytes().unwrap());
    }
    assert!(observer.has_changed().unwrap());
    assert_eq!(*observer.borrow_and_update(), 8);
    let snapshot = replica.snapshot(f.public).await.unwrap();
    assert_eq!(snapshot.revision, 8);
    for row in 1..=8u8 {
        assert_eq!(
            snapshot.rows[&[row; 32]],
            f.app().name(&format!("row-{row}"))
        );
    }
    assert_eq!(frames(&replica, &f, &node).await, 8);
}

/// A group's flush runs after the replica lock is released, so other callers
/// lead the next group or read meanwhile. Every exposed result still arrives,
/// and only after a flush that covers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exposed_results_arrive_while_other_leaders_and_readers_run_during_the_flush() {
    const AUTHORS: u8 = 48;
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = open(&f, &dir.path().join("a.db"), &node).await;
    let mut tasks = tokio::task::JoinSet::new();
    for row in 1..=AUTHORS {
        let (replica, changes, scope) = (replica.clone(), insert(&f, row), f.public);
        tasks.spawn(async move {
            let frame = replica.author([row; 32], changes, vec![]).await.unwrap();
            // Returned frames may be sent: the watermark covers them, and a
            // reader beside the writers sees a committed state.
            let durable = replica.durable_commit().await;
            let rows = replica.snapshot(scope).await.unwrap().rows.len();
            (frame.operation().unwrap().header.seq_num, durable, rows)
        });
    }
    let mut sequences = BTreeSet::new();
    while let Some(result) = tasks.join_next().await {
        let (sequence, durable, rows) = result.unwrap();
        assert!(durable > 0 && rows > 0);
        sequences.insert(sequence);
    }
    assert_eq!(sequences, (0..u64::from(AUTHORS)).collect());
    let (commit, durable, flushes) = replica.watermark().await;
    assert!(durable >= commit);
    assert!(flushes <= usize::from(AUTHORS));
    assert_eq!(frames(&replica, &f, &node).await, usize::from(AUTHORS));
}

#[tokio::test]
async fn failing_request_in_a_group_rolls_back_alone() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = open(&f, &dir.path().join("a.db"), &node).await;
    let before = replica.group_commits().await;
    // The first request in the group fails before its savepoint is released.
    replica.inject_failure(0, 1).await;
    let conflicting = vec![change(
        f.public,
        2,
        Some(f.app().name("missing")),
        Some(f.app().name("row-2")),
    )];
    let (first, second, third) = tokio::join!(
        replica.author([1; 32], insert(&f, 1), vec![]),
        replica.author([2; 32], conflicting, vec![]),
        replica.author([3; 32], insert(&f, 3), vec![]),
    );
    assert!(matches!(first, Err(Error::InjectedFailure)));
    assert!(matches!(second, Err(Error::Conflict)));
    assert_eq!(third.unwrap().operation().unwrap().header.seq_num, 0);
    assert_eq!(replica.group_commits().await - before, 1);
    let snapshot = replica.snapshot(f.public).await.unwrap();
    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(snapshot.revision, 1);
    // The failed handles were not recorded; a retry commits them once.
    let retried = replica
        .author([1; 32], insert(&f, 1), vec![])
        .await
        .unwrap();
    assert_eq!(retried.operation().unwrap().header.seq_num, 1);
    assert_eq!(frames(&replica, &f, &node).await, 2);
}

#[tokio::test]
async fn nothing_is_observable_when_the_group_does_not_commit() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = open(&f, &dir.path().join("a.db"), &node).await;
    let observer = replica.observe_revision();
    replica
        .inject_group_fault(Some(GroupFault::FailBeforeCommit))
        .await;
    let (first, second) = tokio::join!(
        replica.author([1; 32], insert(&f, 1), vec![]),
        replica.author([2; 32], insert(&f, 2), vec![]),
    );
    assert!(first.is_err() && second.is_err());
    assert!(!observer.has_changed().unwrap());
    assert!(replica.snapshot(f.public).await.unwrap().rows.is_empty());
    assert_eq!(frames(&replica, &f, &node).await, 0);
    let (stored, memory) = replica.stored_and_memory_revision().await;
    assert_eq!((stored, memory), (0, 0));
    assert_eq!(replica.group_commits().await, 0);
    replica.inject_group_fault(None).await;
    let frame = replica
        .author([2; 32], insert(&f, 2), vec![])
        .await
        .unwrap();
    assert_eq!(frame.operation().unwrap().header.seq_num, 0);

    // Incoming frames follow the same rule.
    let target_key = PrivateKey::new();
    let target = open(&f, &dir.path().join("b.db"), &target_key).await;
    let observer = target.observe_revision();
    target
        .inject_group_fault(Some(GroupFault::FailBeforeCommit))
        .await;
    assert!(target.receive(frame.clone()).await.is_err());
    assert!(!observer.has_changed().unwrap());
    assert!(target.receipt_status(frame.id().unwrap()).await.is_err());
    assert_eq!(frames(&target, &f, &target_key).await, 0);
    target.inject_group_fault(None).await;
    assert_eq!(target.receive(frame).await.unwrap(), ReceiveStatus::Applied);
}

#[tokio::test]
async fn a_received_page_shares_one_commit_and_refuses_frames_alone() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let source = open(&f, &dir.path().join("a.db"), &PrivateKey::new()).await;
    let mut page = Vec::new();
    for row in 1..=10u8 {
        page.push(
            source
                .author([row; 32], insert(&f, row), vec![])
                .await
                .unwrap(),
        );
    }
    let mut body = page[3].body_bytes().to_vec();
    let last = body.len() - 1;
    body[last] ^= 1;
    let tampered = page[3].with_body(body);
    page.insert(4, tampered);
    let target_key = PrivateKey::new();
    let target = open(&f, &dir.path().join("b.db"), &target_key).await;
    let before = target.group_commits().await;
    let statuses = target.receive_all(page).await;
    assert_eq!(target.group_commits().await - before, 1);
    assert_eq!(statuses.len(), 11);
    for (index, status) in statuses.into_iter().enumerate() {
        if index == 4 {
            assert!(status.is_err());
        } else {
            assert_eq!(status.unwrap(), ReceiveStatus::Applied);
        }
    }
    assert_eq!(target.snapshot(f.public).await.unwrap().rows.len(), 10);
    assert_eq!(frames(&target, &f, &target_key).await, 10);
}

#[tokio::test]
async fn busy_file_retries_whole_transaction_then_reports_exhaustion() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.db");
    let node = PrivateKey::new();
    let replica = open(&f, &path, &node).await;
    let blocker = rusqlite::Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = attempts.clone();
    let held = std::sync::Mutex::new(Some(blocker));
    replica
        .set_busy_hook(Some(Box::new(move |attempt| {
            seen.fetch_add(1, Ordering::SeqCst);
            if attempt == 2
                && let Some(blocker) = held.lock().unwrap().take()
            {
                blocker.execute_batch("COMMIT").unwrap();
            }
        })))
        .await;
    let frame = replica
        .author([1; 32], insert(&f, 1), vec![])
        .await
        .unwrap();
    assert_eq!(frame.operation().unwrap().header.seq_num, 0);
    assert_eq!(attempts.load(Ordering::SeqCst), 3);

    let blocker = rusqlite::Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    attempts.store(0, Ordering::SeqCst);
    let mut observer = replica.observe_revision();
    assert_eq!(*observer.borrow_and_update(), 1);
    let refused = replica.author([2; 32], insert(&f, 2), vec![]).await;
    assert!(matches!(refused, Err(Error::RetryExhausted)), "{refused:?}");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        crate::storage::TRANSACTION_ATTEMPTS
    );
    assert!(!observer.has_changed().unwrap());
    drop(blocker);
    let frame = replica
        .author([2; 32], insert(&f, 2), vec![])
        .await
        .unwrap();
    assert_eq!(frame.operation().unwrap().header.seq_num, 1);
    assert_eq!(frames(&replica, &f, &node).await, 2);
}

#[tokio::test]
async fn wal_stays_bounded_under_sustained_authoring() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.db");
    let node = PrivateKey::new();
    let replica = open(&f, &path, &node).await;
    let (checkpoint, limit) = (64 * 1024, 256 * 1024);
    replica.set_wal_limits(checkpoint, limit).await;
    let wal = dir.path().join("a.db-wal");
    let mut largest = 0;
    for index in 0..200u32 {
        let row = rocky_core::commitment("rocky.test.row", &index.to_le_bytes());
        replica
            .author(
                row,
                vec![RowChange {
                    scope: f.public,
                    row,
                    before: None,
                    after: Some(f.app().name(&format!("row-{index}"))),
                }],
                vec![],
            )
            .await
            .unwrap();
        largest = largest.max(std::fs::metadata(&wal).unwrap().len());
    }
    assert!(replica.checkpoints().await > 0);
    // One group's frames may land after the last check.
    assert!(largest <= limit + 128 * 1024, "WAL reached {largest} bytes");
    drop(replica);
    // Closing the only connection checkpoints and removes the WAL.
    assert!(std::fs::metadata(&wal).map_or(0, |wal| wal.len()) == 0);
    let replica = open(&f, &path, &node).await;
    assert_eq!(replica.snapshot(f.public).await.unwrap().rows.len(), 200);
}

const CRASH_PHASES: [&str; 3] = ["inside-group", "before-commit", "after-commit"];

#[tokio::test]
#[ignore = "invoked by process_exit_inside_a_group_leaves_each_handle_whole_or_absent"]
async fn group_crash_child() {
    let path = std::env::var("ROCKY_GROUP_CRASH_PATH").unwrap();
    let fault = match std::env::var("ROCKY_GROUP_CRASH_PHASE").unwrap().as_str() {
        "inside-group" => GroupFault::ExitAfterRequests(2),
        "before-commit" => GroupFault::ExitBeforeCommit,
        _ => GroupFault::ExitAfterCommit,
    };
    let f = Fixture::with_publisher(PrivateKey::from_bytes(&[41; 32]));
    let replica = open(&f, path.as_ref(), &PrivateKey::from_bytes(&[42; 32])).await;
    replica.inject_group_fault(Some(fault)).await;
    let _ = tokio::join!(
        replica.author([1; 32], insert(&f, 1), vec![]),
        replica.author([2; 32], insert(&f, 2), vec![]),
        replica.author([3; 32], insert(&f, 3), vec![]),
        replica.author([4; 32], insert(&f, 4), vec![]),
    );
    // The fault exits the process; reaching this line is a failure.
    std::process::exit(1);
}

#[tokio::test]
async fn process_exit_inside_a_group_leaves_each_handle_whole_or_absent() {
    for phase in CRASH_PHASES {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("crash.db");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::group::group_crash_child", "--ignored"])
            .env("ROCKY_GROUP_CRASH_PATH", &path)
            .env("ROCKY_GROUP_CRASH_PHASE", phase)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77), "{phase}");
        let f = Fixture::with_publisher(PrivateKey::from_bytes(&[41; 32]));
        let node = PrivateKey::from_bytes(&[42; 32]);
        // Open verifies history, projection and dependency closure.
        let replica = open(&f, &path, &node).await;
        let committed = phase == "after-commit";
        let expected = if committed { 4 } else { 0 };
        let snapshot = replica.snapshot(f.public).await.unwrap();
        assert_eq!(snapshot.rows.len(), expected, "{phase}");
        assert_eq!(snapshot.revision, expected as u64, "{phase}");
        assert_eq!(frames(&replica, &f, &node).await, expected, "{phase}");
        // Retrying every handle yields one operation each, numbered without gaps.
        let mut sequences = BTreeSet::new();
        for row in 1..=4u8 {
            let frame = replica
                .author([row; 32], insert(&f, row), vec![])
                .await
                .unwrap();
            sequences.insert(frame.operation().unwrap().header.seq_num);
        }
        assert_eq!(sequences, (0..4).collect(), "{phase}");
        assert_eq!(frames(&replica, &f, &node).await, 4, "{phase}");
    }
}
/// Public futures stay `Send`, so callers can spawn them.
#[allow(dead_code)]
fn public_futures_are_send(
    replica: EncryptedReplica<Employees>,
    f: Fixture,
    frame: EncryptedFrame,
) {
    fn send<T: Send>(_: T) {}
    send(replica.author([1; 32], insert(&f, 1), vec![]));
    send(replica.receive(frame.clone()));
    send(replica.receive_all(vec![frame]));
    send(replica.learn_revocations(&f.policy.clone()));
}

#[tokio::test]
async fn buffered_commits_are_flushed_before_they_leave_the_device() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a_key, b_key) = (PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir.path().join("a.db"), &a_key).await;
    let b = open(&f, &dir.path().join("b.db"), &b_key).await;
    let covered = async |replica: &EncryptedReplica<Employees>| {
        let (commit, durable, _) = replica.watermark().await;
        durable >= commit
    };
    let first = a
        .author_buffered([1; 32], insert(&f, 1), vec![])
        .await
        .unwrap();
    assert!(!covered(&a).await);
    assert!(a.durable_commit().await < first.commit);
    // A transfer page flushes before it reads.
    let page = a
        .transfer_page(
            b_key.public_key(),
            &f.credentials(&b_key, f.full()),
            &[f.public],
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(covered(&a).await);
    assert!(a.durable_commit().await >= first.commit);
    assert_eq!(page.frames[0].id().unwrap(), first.value);
    // A buffered receipt is not covered until a status is read for the sender.
    let received = b.receive_buffered(page.frames.clone()).await;
    assert_eq!(received[0].as_ref().unwrap().value, ReceiveStatus::Applied);
    assert!(!covered(&b).await);
    assert_eq!(
        b.receipt_status(first.value).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert!(covered(&b).await);
    // Recording a peer's acknowledgment flushes before the peer hears back.
    a.author_buffered([2; 32], insert(&f, 2), vec![])
        .await
        .unwrap();
    assert!(!covered(&a).await);
    a.acknowledge(
        b_key.public_key(),
        &f.credentials(&b_key, f.full()),
        first.value,
        true,
    )
    .await
    .unwrap();
    assert!(covered(&a).await);
    // `author` returns only flushed frames; the handle keeps its operation.
    let buffered = a
        .author_buffered([3; 32], insert(&f, 3), vec![])
        .await
        .unwrap();
    assert!(!covered(&a).await);
    let frame = a.author([3; 32], insert(&f, 3), vec![]).await.unwrap();
    assert_eq!(frame.id().unwrap(), buffered.value);
    assert!(covered(&a).await);
    // `receive_all` answers only after its receipts are flushed.
    let page = a
        .transfer_page(
            b_key.public_key(),
            &f.credentials(&b_key, f.full()),
            &[f.public],
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(
        page.frames
            .iter()
            .any(|page| page.id().unwrap() == frame.id().unwrap())
    );
    for status in b.receive_all(page.frames).await {
        status.unwrap();
    }
    assert!(covered(&b).await);
    assert_eq!(
        b.receipt_status(frame.id().unwrap()).await.unwrap(),
        ReceiveStatus::Applied
    );
    // Exposed and buffered requests in one group share one commit and one flush.
    let (_, _, flushes) = a.watermark().await;
    let commits = a.group_commits().await;
    let (one, two, three) = tokio::join!(
        a.author_buffered([4; 32], insert(&f, 4), vec![]),
        a.author([5; 32], insert(&f, 5), vec![]),
        a.author_buffered([6; 32], insert(&f, 6), vec![]),
    );
    assert!(one.is_ok() && two.is_ok() && three.is_ok());
    assert_eq!(a.group_commits().await - commits, 1);
    assert_eq!(a.watermark().await.2 - flushes, 1);
    assert!(covered(&a).await);
}

/// Replaces the files at `path` with the copy taken at the last flush, as a
/// power loss that drops every later buffered commit would.
pub(super) fn lose_unflushed(copy: &std::path::Path, path: &std::path::Path) {
    let wal = std::path::PathBuf::from(format!("{}-wal", path.display()));
    let shm = std::path::PathBuf::from(format!("{}-shm", path.display()));
    std::fs::copy(copy.join("copy.db"), path).unwrap();
    let _ = std::fs::remove_file(&wal);
    let _ = std::fs::remove_file(&shm);
    if copy.join("copy.db-wal").exists() {
        std::fs::copy(copy.join("copy.db-wal"), &wal).unwrap();
    }
}

#[tokio::test]
async fn power_loss_keeps_everything_exposed_and_drops_only_unexposed_work() {
    let f = Fixture::with_publisher(PrivateKey::from_bytes(&[61; 32]));
    let dir = TempDir::new().unwrap();
    let (a_key, b_key) = (PrivateKey::from_bytes(&[62; 32]), PrivateKey::new());
    let (a_copy, b_copy) = (dir.path().join("a-copy"), dir.path().join("b-copy"));
    std::fs::create_dir_all(&a_copy).unwrap();
    std::fs::create_dir_all(&b_copy).unwrap();
    let (a_path, b_path) = (dir.path().join("a.db"), dir.path().join("b.db"));
    let a = open(&f, &a_path, &a_key).await;
    let b = open(&f, &b_path, &b_key).await;
    a.copy_at_flush(Some(a_copy.clone())).await;
    b.copy_at_flush(Some(b_copy.clone())).await;
    let pull = async |from: &EncryptedReplica<Employees>, to: &EncryptedReplica<Employees>| {
        let page = from
            .transfer_page(
                b_key.public_key(),
                &f.credentials(&b_key, f.full()),
                &[f.public],
                None,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        to.receive_all(page.frames).await
    };
    // Row 1 is buffered, then covered by the flush that exposes row 2.
    a.author_buffered([1; 32], insert(&f, 1), vec![])
        .await
        .unwrap();
    let exposed = a.author([2; 32], insert(&f, 2), vec![]).await.unwrap();
    for status in pull(&a, &b).await {
        assert_eq!(status.unwrap(), ReceiveStatus::Applied);
    }
    // Never exposed: rows 3 and 4 at A, and an unacknowledged receipt at B.
    let lost = a
        .author_buffered([3; 32], insert(&f, 3), vec![])
        .await
        .unwrap();
    a.author_buffered([4; 32], insert(&f, 4), vec![])
        .await
        .unwrap();
    let c_key = PrivateKey::new();
    let c = open(&f, &dir.path().join("c.db"), &c_key).await;
    let other = c.author([9; 32], insert(&f, 9), vec![]).await.unwrap();
    b.receive_buffered(vec![other.clone()]).await[0]
        .as_ref()
        .unwrap();
    drop((a, b));
    lose_unflushed(&a_copy, &a_path);
    lose_unflushed(&b_copy, &b_path);

    let a = open(&f, &a_path, &a_key).await;
    let b = open(&f, &b_path, &b_key).await;
    let rows = a.snapshot(f.public).await.unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert!(rows.rows.contains_key(&[1; 32]) && rows.rows.contains_key(&[2; 32]));
    assert_eq!(rows.revision, 2);
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 2);
    assert!(b.receipt_status(other.id().unwrap()).await.is_err());
    // An exposed handle keeps its frame; a lost one commits exactly once.
    assert_eq!(
        a.author([2; 32], insert(&f, 2), vec![])
            .await
            .unwrap()
            .to_bytes()
            .unwrap(),
        exposed.to_bytes().unwrap()
    );
    let retried = a.author([3; 32], insert(&f, 3), vec![]).await.unwrap();
    assert_eq!(retried.operation().unwrap().header.seq_num, 2);
    assert_ne!(retried.id().unwrap(), lost.value);
    assert_eq!(
        a.author([3; 32], insert(&f, 3), vec![])
            .await
            .unwrap()
            .id()
            .unwrap(),
        retried.id().unwrap()
    );
    assert_eq!(frames(&a, &f, &a_key).await, 3);
    // The peer converges without a fork; the sender re-offers what B lost.
    let statuses: Vec<_> = pull(&a, &b).await.into_iter().map(Result::unwrap).collect();
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == ReceiveStatus::Applied)
            .count(),
        1
    );
    assert_eq!(b.receive(other).await.unwrap(), ReceiveStatus::Applied);
    let (a_rows, b_rows) = (
        a.snapshot(f.public).await.unwrap().rows,
        b.snapshot(f.public).await.unwrap().rows,
    );
    assert_eq!(a_rows.len(), 3);
    assert!(
        a_rows
            .iter()
            .all(|(row, fields)| b_rows.get(row) == Some(fields))
    );
}

/// Release-mode throughput of flushed and buffered paths:
/// `cargo test --release -p rocky-secure --lib tests::group::measure -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "measurement"]
async fn measure_group_commit_throughput() {
    let f = Arc::new(Fixture::new());
    let dir = TempDir::new().unwrap();
    let mut next = 0u32;
    let row = |index: u32| {
        let id = rocky_core::commitment("rocky.measure.row", &index.to_le_bytes());
        (
            id,
            vec![RowChange {
                scope: f.public,
                row: id,
                before: None,
                after: Some(f.app().name(&format!("row-{index}"))),
            }],
        )
    };
    let mut sent = Vec::new();
    for (clients, buffered) in [(1, false), (8, false), (1, true), (8, true)] {
        let replica = open(
            &f,
            &dir.path().join(format!("a{next}.db")),
            &PrivateKey::new(),
        )
        .await;
        let total = 480u32;
        let started = std::time::Instant::now();
        let mut tasks = tokio::task::JoinSet::new();
        for client in 0..clients {
            let replica = replica.clone();
            let work: Vec<_> = (client..total)
                .step_by(clients as usize)
                .map(|i| row(next + i))
                .collect();
            tasks.spawn(async move {
                for (handle, changes) in work {
                    if buffered {
                        replica
                            .author_buffered(handle, changes, vec![])
                            .await
                            .unwrap();
                    } else {
                        replica.author(handle, changes, vec![]).await.unwrap();
                    }
                }
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        replica.flush().await.unwrap();
        let elapsed = started.elapsed();
        println!(
            "author clients={clients} buffered={buffered}: {:.0}/s, {} group commits, {} flushes",
            f64::from(total) / elapsed.as_secs_f64(),
            replica.group_commits().await,
            replica.watermark().await.2
        );
        next += total;
        if sent.is_empty() {
            let mut cursor = None;
            loop {
                let key = PrivateKey::new();
                let page = replica
                    .transfer_page(
                        key.public_key(),
                        &f.credentials(&key, f.full()),
                        &[f.public],
                        cursor,
                        MAX_SECURE_FRAME,
                    )
                    .await
                    .unwrap();
                sent.extend(page.frames);
                cursor = page.next;
                if cursor.is_none() {
                    break;
                }
            }
        }
    }
    // Deliver in log order. Pages come in hash order, and each pending pass
    // re-verifies every frame still waiting for its predecessor.
    sent.sort_by_key(|frame| frame.operation().unwrap().header.seq_num);
    for mode in ["receive", "receive_buffered", "receive_all page"] {
        let target = open(
            &f,
            &dir.path().join(format!("{mode}.db")),
            &PrivateKey::new(),
        )
        .await;
        let started = std::time::Instant::now();
        match mode {
            "receive" => {
                for frame in &sent {
                    target.receive(frame.clone()).await.unwrap();
                }
            }
            "receive_buffered" => {
                for frame in &sent {
                    target.receive_buffered(vec![frame.clone()]).await[0]
                        .as_ref()
                        .unwrap();
                }
                target.flush().await.unwrap();
            }
            _ => {
                for page in sent.chunks(32) {
                    for status in target.receive_all(page.to_vec()).await {
                        status.unwrap();
                    }
                }
            }
        }
        println!(
            "incoming {mode}: {:.0}/s over {} frames, {} flushes",
            sent.len() as f64 / started.elapsed().as_secs_f64(),
            sent.len(),
            target.watermark().await.2
        );
    }
}

#[tokio::test]
async fn incoming_frames_open_and_verify_their_chain_once_and_revocations_miss_the_cache() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let author = open(&f, &dir.path().join("a.db"), &PrivateKey::new()).await;
    let member = open(&f, &dir.path().join("b.db"), &PrivateKey::new()).await;
    let first = author
        .author([71; 32], insert(&f, 1), vec![])
        .await
        .unwrap();
    let second = author
        .author([72; 32], insert(&f, 2), vec![])
        .await
        .unwrap();
    let before = member.crypto_stats().await;
    assert_eq!(
        member.receive(first.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    let after = member.crypto_stats().await;
    assert_eq!(after, (before.0 + 1, before.1 + 1));
    assert_eq!(
        member.receive(first.clone()).await.unwrap(),
        ReceiveStatus::Duplicate
    );
    let revoked = revoking(
        &f.publisher,
        f.app().manifest().app,
        &[],
        &[first.chain().unwrap().credentials().0[0].id().unwrap()],
    );
    member.learn_revocations(&revoked).await.unwrap();
    assert!(member.receive(second).await.is_err());
}

#[tokio::test]
async fn a_paused_group_flush_leaves_the_replica_and_executor_available() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("a.db"), &PrivateKey::new()).await;
    let (entered, resume) = replica.pause_flush().await;
    let author = replica.author([91; 32], insert(&f, 1), vec![]);
    tokio::pin!(author);
    tokio::select! {
        result = &mut author => panic!("reply preceded the flush: {result:?}"),
        result = entered => result.unwrap(),
    }
    let available = tokio::time::timeout(
        std::time::Duration::ZERO,
        replica.inner.lock(crate::replica::Class::Background),
    )
    .await
    .is_ok();
    resume.send(()).unwrap();
    assert!(available, "fsync held the replica lock");
    assert!(author.await.is_ok());
}
