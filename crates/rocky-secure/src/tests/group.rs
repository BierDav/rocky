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
async fn concurrent_authors_share_one_flush_and_keep_their_own_frames() {
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
    assert_eq!(commits, 4);
    assert_eq!(replica.watermark().await.2, 1);
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
    assert_eq!(replica.group_commits().await - before, 2);
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
async fn a_received_page_shares_one_flush_and_refuses_frames_alone() {
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
    assert_eq!(
        target.group_commits().await - before,
        10_usize.div_ceil(crate::replica::group::RECEIVE_REQUESTS)
    );
    assert_eq!(target.watermark().await.2, 1);
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
async fn a_ready_received_frame_never_exposes_pending_custody_to_a_reader() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let source = open(&f, &dir.path().join("ready-source.db"), &PrivateKey::new()).await;
    let target = open(&f, &dir.path().join("ready-target.db"), &PrivateKey::new()).await;
    let frame = source.author([1; 32], insert(&f, 1), vec![]).await.unwrap();
    let hash = frame.id().unwrap();
    let receiver = target.clone();
    let receiving = tokio::spawn(async move { receiver.receive(frame).await });
    let mut observed = false;
    loop {
        let inner = target.inner.lock(crate::replica::Class::Background).await;
        let stored = crate::storage::collect_rows(
            inner
                .conn
                .query(
                    "SELECT status FROM secure_operations WHERE hash=?",
                    [hash.as_bytes().to_vec()],
                )
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        if !stored.is_empty() {
            assert_eq!(
                stored,
                vec![vec![SqlValue::Text("applied".into())]],
                "ready custody became visible before application"
            );
            observed = true;
        }
        drop(inner);
        if receiving.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(receiving.await.unwrap().unwrap(), ReceiveStatus::Applied);
    assert!(observed);
    assert_eq!(target.snapshot(f.public).await.unwrap().rows.len(), 1);
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
    replica.optimize().await;
    drop(replica);
    // Closing the connections checkpoints and removes the WAL.
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
        "inside-group" => GroupFault::ExitAfterRequests(1),
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
        let expected = if committed { 2 } else { 0 };
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
    // Exposed and buffered requests share one flush across their bounded commits.
    let (_, _, flushes) = a.watermark().await;
    let commits = a.group_commits().await;
    let (one, two, three) = tokio::join!(
        a.author_buffered([4; 32], insert(&f, 4), vec![]),
        a.author([5; 32], insert(&f, 5), vec![]),
        a.author_buffered([6; 32], insert(&f, 6), vec![]),
    );
    assert!(one.is_ok() && two.is_ok() && three.is_ok());
    assert_eq!(a.group_commits().await - commits, 2);
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

/// Admission, atomic application and empty settlement without maintenance or flush.
#[tokio::test]
#[ignore = "measurement"]
async fn measure_separated_receive_steps() {
    fn report(site: &str, rows: usize, frames: usize, samples: &mut [std::time::Duration]) {
        samples.sort_unstable();
        let percentile = |n: usize| samples[(samples.len() * n).div_ceil(100) - 1].as_micros();
        println!(
            "separated {site} rows={rows} frames={frames} samples={} p50_us={} p95_us={} p99_us={} max_us={}",
            samples.len(),
            percentile(50),
            percentile(95),
            percentile(99),
            samples.last().unwrap().as_micros(),
        );
    }
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    for count in [1, ROWS_PER_FRAME, MAX_BUNDLE_ROWS] {
        let source = open(
            &f,
            &dir.path().join(format!("source-{count}.db")),
            &PrivateKey::new(),
        )
        .await;
        let writes = (0..count)
            .map(|index| {
                RowWrite::changed(RowChange {
                    scope: f.public,
                    row: rocky_core::commitment(
                        "rocky.measure.bounded-holds",
                        &index.to_le_bytes(),
                    ),
                    before: None,
                    after: Some(f.app().name(&format!("employee-{index}"))),
                })
            })
            .collect();
        let frames = source.author_bundle([1; 32], writes, vec![]).await.unwrap();
        let mut admissions = Vec::new();
        let mut applications = Vec::new();
        let mut settlements = Vec::new();
        for sample in 0..if count == 1 { 64 } else { 12 } {
            let target = open(
                &f,
                &dir.path().join(format!("target-{count}-{sample}.db")),
                &PrivateKey::new(),
            )
            .await;
            let (mut admitted, applied, settled) = target
                .measure_receive_steps_for_test(&frames, 1)
                .await
                .unwrap();
            admissions.append(&mut admitted);
            applications.push(applied);
            settlements.push(settled);
            // One application publishes the whole transaction, never a partial bundle.
            let inner = target.inner.lock(crate::replica::Class::Background).await;
            assert_eq!(
                rows(&inner.conn, "SELECT count(*) FROM secure_rows")
                    .await
                    .unwrap(),
                vec![vec![SqlValue::Integer(count as i64)]],
            );
            drop(inner);
            for frame in &frames {
                assert_eq!(
                    target.receipt_status(frame.id().unwrap()).await.unwrap(),
                    ReceiveStatus::Applied
                );
            }
        }
        report("admission", count, frames.len(), &mut admissions);
        report("application", count, frames.len(), &mut applications);
        report("settle_empty", count, frames.len(), &mut settlements);
        if count == MAX_BUNDLE_ROWS {
            for batch in [2, 4, 8, 16, 32, 64] {
                let mut admissions = Vec::new();
                for sample in 0..12 {
                    let target = open(
                        &f,
                        &dir.path().join(format!("batch-{batch}-{sample}.db")),
                        &PrivateKey::new(),
                    )
                    .await;
                    let (mut admitted, _, _) = target
                        .measure_receive_steps_for_test(&frames, batch)
                        .await
                        .unwrap();
                    admissions.append(&mut admitted);
                }
                report(
                    &format!("admission_batch_{batch}"),
                    count,
                    frames.len(),
                    &mut admissions,
                );
            }
        }
        if count == 1 {
            let target = open(&f, &dir.path().join("warm.db"), &PrivateKey::new()).await;
            target
                .measure_receive_steps_for_test(&frames, 1)
                .await
                .unwrap();
            let mut admissions = Vec::new();
            let mut applications = Vec::new();
            let mut settlements = Vec::new();
            for sample in 1u8..=64 {
                let writes = vec![RowWrite::changed(RowChange {
                    scope: f.public,
                    row: rocky_core::commitment(
                        "rocky.measure.bounded-holds",
                        &0usize.to_le_bytes(),
                    ),
                    before: Some(f.app().name(&format!("employee-{}", sample - 1))),
                    after: Some(f.app().name(&format!("employee-{sample}"))),
                })];
                let frames = source
                    .author_bundle([sample + 1; 32], writes, vec![])
                    .await
                    .unwrap();
                let (mut admitted, applied, settled) = target
                    .measure_receive_steps_for_test(&frames, 1)
                    .await
                    .unwrap();
                admissions.append(&mut admitted);
                applications.push(applied);
                settlements.push(settled);
                assert_eq!(
                    target
                        .receipt_status(frames[0].id().unwrap())
                        .await
                        .unwrap(),
                    ReceiveStatus::Applied
                );
            }
            report("warm_admission", 1, 1, &mut admissions);
            report("warm_application", 1, 1, &mut applications);
            report("warm_settle_empty", 1, 1, &mut settlements);
        }
    }
}

#[tokio::test]
async fn a_failure_between_group_steps_keeps_committed_handles_retryable() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("steps.db"), &PrivateKey::new()).await;
    let initial_commit = replica.watermark().await.0;
    replica
        .inject_group_fault(Some(GroupFault::FailAfterCommits(2)))
        .await;
    let mut tasks = tokio::task::JoinSet::new();
    for row in 1u8..=8 {
        let replica = replica.clone();
        let changes = insert(&f, row);
        tasks.spawn(async move { (row, replica.author([row; 32], changes, vec![]).await) });
    }
    let mut committed = BTreeMap::new();
    let mut failed = 0;
    while let Some(result) = tasks.join_next().await {
        let (row, result) = result.unwrap();
        match result {
            Ok(frame) => {
                committed.insert(row, frame.id().unwrap());
            }
            Err(Error::InjectedFailure) => failed += 1,
            Err(error) => panic!("unexpected step failure: {error}"),
        }
    }
    assert_eq!(committed.len(), 4);
    assert_eq!(failed, 4);
    assert_eq!(replica.snapshot(f.public).await.unwrap().rows.len(), 4);
    let (commit, durable, flushes) = replica.watermark().await;
    assert!(durable >= initial_commit + 2 && durable <= commit);
    assert_eq!(flushes, 1);
    replica.inject_group_fault(None).await;
    for row in 1u8..=8 {
        let frame = replica
            .author([row; 32], insert(&f, row), vec![])
            .await
            .unwrap();
        if let Some(expected) = committed.get(&row) {
            assert_eq!(&frame.id().unwrap(), expected);
        }
    }
    assert_eq!(replica.snapshot(f.public).await.unwrap().rows.len(), 8);
}

#[tokio::test]
async fn a_passive_checkpoint_runs_while_the_replica_is_held() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("passive.db"), &PrivateKey::new()).await;
    replica
        .author_buffered([1; 32], insert(&f, 1), vec![])
        .await
        .unwrap();
    replica.set_wal_limits(0, u64::MAX).await;
    let mut inner = replica.inner.lock(crate::replica::Class::Background).await;
    let before = inner.passive.completed();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (resume, paused) = std::sync::mpsc::channel();
    inner.passive.pause(started, paused);
    inner.checkpoint_if_due().await;
    ready.await.unwrap();
    let (scheduled, polled) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        scheduled.send(()).unwrap();
    });
    polled.await.unwrap();
    assert_eq!(inner.passive.completed(), before);
    resume.send(()).unwrap();
    inner.passive.wait().await;
    assert_eq!(inner.passive.completed(), before + 1);
    // A second check at the same WAL size does not run another checkpoint.
    inner.checkpoint_if_due().await;
    inner.passive.wait().await;
    assert_eq!(inner.passive.completed(), before + 1);
}

#[tokio::test]
async fn a_truncating_checkpoint_waits_for_the_replica_writer() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("truncate.db"), &PrivateKey::new()).await;
    replica
        .author_buffered([1; 32], insert(&f, 1), vec![])
        .await
        .unwrap();
    replica.flush().await.unwrap();
    replica.set_wal_limits(0, 0).await;
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    let before = inner.checkpoints;
    let (started, ready) = tokio::sync::oneshot::channel();
    let target = replica.clone();
    let task = tokio::spawn(async move {
        started.send(()).unwrap();
        target.maintain().await
    });
    ready.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    assert_eq!(inner.checkpoints, before);
    drop(inner);
    assert!(task.await.unwrap());
    assert_eq!(replica.checkpoints().await, before + 1);
}

#[tokio::test]
async fn pending_discovery_and_application_are_bounded_across_log_heads() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let target = staged_heads(&f, &dir, "heads", 4).await;
    let mut revision = target.observe_revision();
    let mut applied = 0;
    let mut steps = 0;
    loop {
        crate::sqlite::trace_statements(true);
        let more = target.pending_step_for_test().await.unwrap();
        let trace = crate::sqlite::trace_statements(false);
        assert!(
            trace
                .iter()
                .filter(|sql| sql
                    .starts_with("SELECT sequence,hash,scope,author FROM secure_operations"))
                .count()
                <= 1
        );
        assert!(
            trace
                .iter()
                .all(|sql| !sql.contains("min(sequence) AS sequence")
                    && !sql.ends_with("FROM secure_quarantine"))
        );
        let next = *revision.borrow_and_update();
        assert!(next - applied <= 1, "more than one application in a step");
        applied = next;
        steps += 1;
        assert!(steps < 100);
        if !more {
            break;
        }
    }
    assert!(steps > 4);
    assert_eq!(applied, 4);
    assert_eq!(target.snapshot(f.public).await.unwrap().rows.len(), 4);
}

/// Frames from distinct authors, stored without applying them.
async fn staged_heads(
    f: &Fixture,
    dir: &TempDir,
    name: &str,
    heads: u8,
) -> EncryptedReplica<Employees> {
    let mut frames = Vec::new();
    for row in 1..=heads {
        let source = open(
            f,
            &dir.path().join(format!("{name}-{row}.db")),
            &PrivateKey::new(),
        )
        .await;
        frames.push(
            source
                .author([row; 32], insert(f, row), vec![])
                .await
                .unwrap(),
        );
    }
    let target = open(
        f,
        &dir.path().join(format!("{name}.db")),
        &PrivateKey::new(),
    )
    .await;
    target
        .stage_bundle_before_apply_for_test(&frames)
        .await
        .unwrap();
    target
}

#[tokio::test]
async fn stored_work_invalidates_pending_discovery_before_application() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let target = staged_heads(&f, &dir, "invalidated", 3).await;
    assert!(target.pending_step_for_test().await.unwrap());
    assert!(target.pending_step_for_test().await.unwrap());
    let source = open(&f, &dir.path().join("late.db"), &PrivateKey::new()).await;
    let late = source.author([4; 32], insert(&f, 4), vec![]).await.unwrap();
    target
        .stage_bundle_before_apply_for_test(&[late])
        .await
        .unwrap();
    let revision = target.observe_revision();
    let after_store = *revision.borrow();
    // The old discovered heads are discarded before any application can use them.
    for _ in 0..2 {
        assert!(target.pending_step_for_test().await.unwrap());
        assert_eq!(*revision.borrow(), after_store);
    }
    target.settle().await.unwrap();
    assert_eq!(target.snapshot(f.public).await.unwrap().rows.len(), 4);
}

/// Regression: a local write between every settle step restarted the pass,
/// so a receive waiting for settle never returned while writes continued.
#[tokio::test]
async fn local_writes_between_pending_steps_do_not_restart_the_pass() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let target = staged_heads(&f, &dir, "interleaved", 3).await;
    let mut steps = 0u8;
    while target.pending_step_for_test().await.unwrap() {
        steps += 1;
        assert!(steps < 100, "the pending pass never finished");
        target
            .author_buffered([100 + steps; 32], insert(&f, 100 + steps), vec![])
            .await
            .unwrap();
    }
    let rows = target.snapshot(f.public).await.unwrap().rows.len();
    assert_eq!(rows, 3 + usize::from(steps));
}

#[tokio::test]
async fn pending_discovery_resumes_after_reopening() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let source = open(&f, &dir.path().join("resume-source.db"), &PrivateKey::new()).await;
    let frames = vec![
        source.author([1; 32], insert(&f, 1), vec![]).await.unwrap(),
        source.author([2; 32], insert(&f, 2), vec![]).await.unwrap(),
    ];
    let path = dir.path().join("resume.db");
    let node = PrivateKey::new();
    let target = open(&f, &path, &node).await;
    target
        .stage_bundle_before_apply_for_test(&frames)
        .await
        .unwrap();
    assert!(target.pending_step_for_test().await.unwrap());
    drop(target);
    let target = open(&f, &path, &node).await;
    target.settle().await.unwrap();
    assert_eq!(target.snapshot(f.public).await.unwrap().rows.len(), 2);
    for frame in frames {
        assert_eq!(
            target.receipt_status(frame.id().unwrap()).await.unwrap(),
            ReceiveStatus::Applied
        );
    }
}

#[tokio::test]
#[ignore = "combined receive calibration; run in release mode with --ignored --nocapture"]
async fn measure_combined_receive_steps() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    for batch in [1, 2, 4] {
        let source = open(
            &f,
            &dir.path().join(format!("combined-source-{batch}.db")),
            &PrivateKey::new(),
        )
        .await;
        let mut first = Vec::new();
        for row in 1..=batch {
            // A separate scope of author handles per calibration size.
            first.push(
                source
                    .author([batch * 16 + row; 32], insert(&f, batch * 16 + row), vec![])
                    .await
                    .unwrap(),
            );
        }
        let mut cold = Vec::new();
        for sample in 0..64 {
            let target = open(
                &f,
                &dir.path()
                    .join(format!("combined-cold-{batch}-{sample}.db")),
                &PrivateKey::new(),
            )
            .await;
            cold.push(
                target
                    .measure_ready_receive_for_test(&first)
                    .await
                    .unwrap()
                    .as_micros() as u64,
            );
        }
        let target = open(
            &f,
            &dir.path().join(format!("combined-warm-{batch}.db")),
            &PrivateKey::new(),
        )
        .await;
        target.measure_ready_receive_for_test(&first).await.unwrap();
        let mut previous: Vec<_> = (1..=batch)
            .map(|row| f.app().name(&format!("row-{}", batch * 16 + row)))
            .collect();
        let mut warm = Vec::new();
        for sample in 1..=128u8 {
            let mut frames = Vec::new();
            for row in 1..=batch {
                let next = f.app().name(&format!("warm-{batch}-{row}-{sample}"));
                let mut handle = [0; 32];
                handle[..3].copy_from_slice(&[batch, row, sample]);
                let frame = source
                    .author(
                        handle,
                        vec![change(
                            f.public,
                            batch * 16 + row,
                            Some(previous[usize::from(row - 1)].clone()),
                            Some(next.clone()),
                        )],
                        vec![],
                    )
                    .await
                    .unwrap();
                previous[usize::from(row - 1)] = next;
                frames.push(frame);
            }
            warm.push(
                target
                    .measure_ready_receive_for_test(&frames)
                    .await
                    .unwrap()
                    .as_micros() as u64,
            );
        }
        let snapshot = target.snapshot(f.public).await.unwrap();
        for row in 1..=batch {
            assert_eq!(
                snapshot.rows[&[batch * 16 + row; 32]],
                previous[usize::from(row - 1)]
            );
        }
        for (profile, mut samples) in [("cold", cold), ("warm", warm)] {
            samples.sort_unstable();
            let percentile = |n: usize| samples[(samples.len() * n).div_ceil(100) - 1];
            eprintln!(
                "combined_receive profile={profile} input_rows={batch} samples={} p50_us={} p95_us={} p99_us={} max_us={}",
                samples.len(),
                percentile(50),
                percentile(95),
                percentile(99),
                samples.last().unwrap()
            );
        }
    }
}

#[tokio::test]
async fn receive_steps_isolate_indivisible_frames_and_keep_one_flush() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let source = open(
        &f,
        &dir.path().join("bounded-source.db"),
        &PrivateKey::new(),
    )
    .await;
    let target = open(
        &f,
        &dir.path().join("bounded-target.db"),
        &PrivateKey::new(),
    )
    .await;
    let limit = crate::replica::group::RECEIVE_REQUESTS;
    let mut page = Vec::new();
    for row in 1..=limit as u8 {
        page.push(
            source
                .author([row; 32], insert(&f, row), vec![])
                .await
                .unwrap(),
        );
    }
    page.push(
        source
            .author(
                [100; 32],
                [insert(&f, 100), insert(&f, 101)].concat(),
                vec![],
            )
            .await
            .unwrap(),
    );
    for row in limit as u8 + 1..=2 * limit as u8 {
        page.push(
            source
                .author([row; 32], insert(&f, row), vec![])
                .await
                .unwrap(),
        );
    }
    let measurements = target.lock_measurements();
    measurements.start();
    let before = target.group_commits().await;
    let statuses = target.receive_all(page).await;
    assert!(
        statuses
            .into_iter()
            .all(|status| matches!(status, Ok(ReceiveStatus::Applied)))
    );
    assert_eq!(target.group_commits().await - before, 3);
    assert_eq!(target.watermark().await.2, 1);
    assert_eq!(
        target.snapshot(f.public).await.unwrap().rows.len(),
        2 * limit + 2
    );
    let measured = measurements.stop();
    let mut received: Vec<_> = measured
        .holders
        .values()
        .flat_map(|holder| {
            holder
                .units
                .get("received_frame")
                .into_iter()
                .flatten()
                .map(|(count, _)| *count)
        })
        .collect();
    received.sort_unstable();
    assert_eq!(received, [1, limit as u64, limit as u64]);
    let oversized: usize = measured
        .holders
        .values()
        .map(|holder| holder.oversized_request_us.len())
        .sum();
    assert_eq!(
        oversized, 1,
        "ordinary grouped frames are not oversized requests"
    );
}

#[tokio::test]
async fn receive_retry_keeps_committed_steps_and_resumes_the_failed_suffix() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let source = open(&f, &dir.path().join("retry-source.db"), &PrivateKey::new()).await;
    let target = open(&f, &dir.path().join("retry-target.db"), &PrivateKey::new()).await;
    let limit = crate::replica::group::RECEIVE_REQUESTS;
    let mut page = Vec::new();
    for row in 1..=2 * limit as u8 + 1 {
        page.push(
            source
                .author([row; 32], insert(&f, row), vec![])
                .await
                .unwrap(),
        );
    }
    target
        .inject_group_fault(Some(GroupFault::FailAfterCommits(1)))
        .await;
    let statuses = target.receive_all(page.clone()).await;
    assert!(
        statuses[..limit]
            .iter()
            .all(|status| matches!(status, Ok(ReceiveStatus::Applied)))
    );
    assert!(statuses[limit..].iter().all(Result::is_err));
    assert_eq!(target.snapshot(f.public).await.unwrap().rows.len(), limit);
    target.inject_group_fault(None).await;
    let statuses = target.receive_all(page).await;
    assert!(
        statuses[..limit]
            .iter()
            .all(|status| matches!(status, Ok(ReceiveStatus::Duplicate)))
    );
    assert!(
        statuses[limit..]
            .iter()
            .all(|status| matches!(status, Ok(ReceiveStatus::Applied)))
    );
    assert_eq!(
        target.snapshot(f.public).await.unwrap().rows.len(),
        2 * limit + 1
    );
}

#[tokio::test]
async fn reclamation_releases_between_page_steps_and_resumes_after_reopen() {
    use crate::replica::group::{RECLAIM_PAGES, RECLAIM_STEP};
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("reclamation.db");
    let node = PrivateKey::new();
    let replica = open(&f, &path, &node).await;
    replica
        .author([1; 32], insert(&f, 1), vec![])
        .await
        .unwrap();
    {
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute("CREATE TABLE reclamation_fixture(bytes BLOB)", ())
            .await
            .unwrap();
        inner.conn.execute(format!("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<{}) INSERT INTO reclamation_fixture SELECT zeroblob(4096) FROM n", RECLAIM_STEP + RECLAIM_PAGES), ()).await.unwrap();
        inner
            .conn
            .execute("DROP TABLE reclamation_fixture", ())
            .await
            .unwrap();
    }
    let free = |replica: EncryptedReplica<Employees>| async move {
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        let result = rows(&inner.conn, "PRAGMA freelist_count").await.unwrap();
        let [SqlValue::Integer(free)] = result[0].as_slice() else {
            panic!("free pages")
        };
        *free
    };
    let initial = free(replica.clone()).await;
    assert!(initial > RECLAIM_STEP + RECLAIM_PAGES);
    let measurements = replica.lock_measurements();
    measurements.start();
    replica.reclaim_free_pages_for_test().await;
    let remaining = free(replica.clone()).await;
    assert!((1..=RECLAIM_STEP).contains(&(initial - remaining)));
    assert!(
        remaining >= RECLAIM_PAGES,
        "fixture must cross several steps"
    );
    let measured = measurements.stop();
    let counts: Vec<_> = measured
        .holders
        .values()
        .flat_map(|holder| {
            holder
                .units
                .get("reclaimed_page")
                .into_iter()
                .flatten()
                .map(|(count, _)| *count)
        })
        .collect();
    assert_eq!(counts, [(initial - remaining) as u64]);
    drop(replica);
    let replica = open(&f, &path, &node).await;
    // Open may reclaim the durable remainder before returning.
    let mut previous = free(replica.clone()).await;
    assert!(previous <= remaining);
    while previous >= RECLAIM_PAGES {
        replica.reclaim_free_pages_for_test().await;
        let next = free(replica.clone()).await;
        assert!((1..=RECLAIM_STEP).contains(&(previous - next)));
        previous = next;
    }
    assert_eq!(replica.snapshot(f.public).await.unwrap().rows.len(), 1);
}
