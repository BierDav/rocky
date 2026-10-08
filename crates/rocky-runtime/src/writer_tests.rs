//! The file writer: cross-domain groups, savepoints, publication after commit,
//! fairness, reads beside the writer and WAL checkpoints.
use super::*;
use crate::scaling_option_tests::{NODE, note, open_node, pause, scope};
use crate::worker::{Fault, GroupLog};
use inventory_app::InventoryApp;
use rocky_core::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::TempDir;

const FILE: &str = "writer.db";

async fn open(dir: &TempDir, options: RuntimeOptions) -> Runtime<InventoryApp> {
    open_node(&dir.path().join(FILE), NODE, options).await
}
/// Both domains in one file, so their writes share the file writer's groups.
fn one_file() -> RuntimeOptions {
    RuntimeOptions {
        files: 1,
        ..RuntimeOptions::default()
    }
}
async fn record(replica: &Replica<InventoryApp>) -> GroupLog {
    let log = GroupLog::default();
    crate::tests::fault(replica, Fault::RecordGroups(log.clone())).await;
    log
}
fn groups(log: &GroupLog) -> Vec<Vec<(usize, usize)>> {
    log.lock().unwrap().clone()
}
/// Operations of one domain that another connection can see committed.
async fn stored(dir: &TempDir, domain: u8) -> Vec<Vec<SqlValue>> {
    let conn = crate::scoped_connection::ScopedConnection::new(
        crate::sqlite::Connection::open_read_only(&dir.path().join(FILE)).unwrap(),
        Some(&crate::runtime::namespace(scope(domain))),
        InventoryApp.schema(),
    );
    storage::rows(&conn, "SELECT COUNT(*) FROM rocky_operations")
        .await
        .unwrap()
}
fn count(value: i64) -> Vec<Vec<SqlValue>> {
    vec![vec![SqlValue::Integer(value)]]
}

#[tokio::test]
async fn a_failing_step_rolls_back_alone_while_its_group_commits() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, one_file()).await;
    let (a, b) = (
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    );
    let log = record(&a).await;
    let (first, resume) = pause(&a).await;
    let (reply, _injected) = oneshot::channel();
    enqueue(
        &a.inner.commands,
        Command::Fault(Fault::FailBeforeCommit, reply),
    )
    .unwrap();
    let failing = note(&a, 1, "fails");
    let kept = note(&a, 2, "kept");
    let conflicting = note(&a, 100, "reuses the first handle");
    let neighbour = note(&b, 1, "neighbour");
    resume.send(()).unwrap();
    assert_eq!(first.committed().await.unwrap().id.sequence.get(), 1);
    assert!(matches!(
        failing.committed().await,
        Err(Error::InjectedFailure)
    ));
    assert_eq!(kept.committed().await.unwrap().id.sequence.get(), 2);
    assert!(matches!(
        conflicting.committed().await,
        Err(Error::Conflict)
    ));
    assert_eq!(neighbour.committed().await.unwrap().id.sequence.get(), 1);
    // All four steps of both domains shared the group after the paused one.
    let groups = groups(&log);
    assert_eq!(groups.len(), 2);
    let mut domains: Vec<_> = groups[1].iter().map(|(domain, _)| *domain).collect();
    domains.sort();
    assert_eq!(domains, [0, 0, 0, 1]);
    assert!(
        a.lookup_mutation(MutationHandle([1; 32]))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(a.query().await.unwrap().revision.get(), 2);
    assert_eq!(b.query().await.unwrap().revision.get(), 1);
    runtime.close().await.unwrap();
    // Reopen replays both domains' history against counters and projections.
    let runtime = open(&dir, one_file()).await;
    for (domain, revision) in [(1, 2), (2, 1)] {
        let snapshot = runtime
            .domain(scope(domain))
            .unwrap()
            .query()
            .await
            .unwrap();
        assert_eq!(
            (snapshot.rows.len() as u64, snapshot.revision.get()),
            (revision, revision)
        );
    }
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn nothing_of_a_group_is_observable_before_its_commit() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, one_file()).await;
    let (a, b) = (
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    );
    let log = record(&a).await;
    let (first, resume) = pause(&a).await;
    // The next group runs b's step, then pauses inside a's.
    let (entered, waiting) = oneshot::channel();
    let (proceed, held) = oneshot::channel();
    let (reply, _set) = oneshot::channel();
    enqueue(
        &a.inner.commands,
        Command::Fault(Fault::Pause(entered, held), reply),
    )
    .unwrap();
    let paused = note(&a, 1, "paused");
    let mut released = note(&b, 1, "released");
    let mut revisions = b.inner.revisions.clone();
    let mut changes = b.inner.changes.clone();
    resume.send(()).unwrap();
    first.committed().await.unwrap();
    waiting.await.unwrap();
    revisions.borrow_and_update();
    changes.borrow_and_update();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    // b's step has released its savepoint, yet nothing of it is visible.
    assert!(released.result.try_recv().is_err());
    assert!(!revisions.has_changed().unwrap());
    assert!(!changes.has_changed().unwrap());
    assert_eq!(b.inner.revisions.borrow().get(), 0);
    assert_eq!(stored(&dir, 2).await, count(0));
    assert_eq!(stored(&dir, 1).await, count(1));
    proceed.send(()).unwrap();
    assert_eq!(paused.committed().await.unwrap().id.sequence.get(), 2);
    assert_eq!(released.committed().await.unwrap().id.sequence.get(), 1);
    assert_eq!(b.inner.revisions.borrow().get(), 1);
    assert_eq!(stored(&dir, 2).await, count(1));
    assert_eq!(
        groups(&log)[1]
            .iter()
            .map(|(domain, _)| *domain)
            .collect::<Vec<_>>(),
        [1, 0]
    );
    runtime.close().await.unwrap();
}

/// A group whose commit fails publishes nothing, fails every waiter and
/// leaves each touched domain's memory equal to storage.
#[tokio::test]
async fn a_failed_commit_fails_the_group_and_restores_memory() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, one_file()).await;
    let (a, b) = (
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    );
    let mut revisions = [a.inner.revisions.clone(), b.inner.revisions.clone()];
    let (first, resume) = pause(&a).await;
    let (reply, _set) = oneshot::channel();
    enqueue(&a.inner.commands, Command::Fault(Fault::FailCommit, reply)).unwrap();
    let lost = [
        note(&a, 1, "lost"),
        note(&a, 2, "lost"),
        note(&b, 1, "lost"),
    ];
    resume.send(()).unwrap();
    first.committed().await.unwrap();
    for revision in &mut revisions {
        revision.borrow_and_update();
    }
    let mut errors = Vec::new();
    for receipt in lost {
        errors.push(receipt.committed().await.unwrap_err());
    }
    assert_eq!(
        errors
            .iter()
            .filter(|error| matches!(error, Error::InjectedFailure))
            .count(),
        1
    );
    assert!(
        errors
            .iter()
            .all(|error| matches!(error, Error::InjectedFailure | Error::Storage(_)))
    );
    assert!(
        revisions
            .iter()
            .all(|revision| !revision.has_changed().unwrap())
    );
    for (replica, revision) in [(&a, 1), (&b, 0)] {
        assert!(
            replica
                .lookup_mutation(MutationHandle([1; 32]))
                .await
                .unwrap()
                .is_none()
        );
        // Reads check memory against the stored revision.
        let snapshot = replica.query().await.unwrap();
        assert_eq!(
            (snapshot.rows.len() as u64, snapshot.revision.get()),
            (revision, revision)
        );
    }
    // Retried handles commit once, at the sequences the failed group used.
    assert_eq!(
        note(&a, 1, "lost")
            .committed()
            .await
            .unwrap()
            .id
            .sequence
            .get(),
        2
    );
    assert_eq!(
        note(&b, 1, "lost")
            .committed()
            .await
            .unwrap()
            .id
            .sequence
            .get(),
        1
    );
    runtime.close().await.unwrap();
    open(&dir, one_file()).await.close().await.unwrap();
}

#[tokio::test]
async fn groups_are_bounded_and_fair_across_domains() {
    let dir = TempDir::new().unwrap();
    let runtime = open(
        &dir,
        RuntimeOptions {
            group_commit: 4,
            ..one_file()
        },
    )
    .await;
    let (a, b) = (
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    );
    let log = record(&a).await;
    // Light load: an awaited write commits alone, without waiting for company.
    for handle in 1..=3 {
        note(&a, handle, "alone").committed().await.unwrap();
    }
    assert_eq!(groups(&log), vec![vec![(0, 256 + 5)]; 3]);
    let (first, resume) = pause(&a).await;
    let flood: Vec<_> = (10..34).map(|handle| note(&a, handle, "flood")).collect();
    let late: Vec<_> = (1..=2).map(|handle| note(&b, handle, "late")).collect();
    resume.send(()).unwrap();
    first.committed().await.unwrap();
    for receipt in flood.into_iter().chain(late) {
        receipt.committed().await.unwrap();
    }
    let groups = groups(&log);
    assert!(groups.iter().all(|group| group.len() <= 4), "{groups:?}");
    // The busy domain's backlog did not delay the other domain's writes.
    let first_after_pause = &groups[4];
    assert_eq!(
        first_after_pause
            .iter()
            .filter(|(domain, _)| *domain == 1)
            .count(),
        2
    );
    assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 3 + 1 + 24 + 2);
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn groups_are_bounded_by_queued_bytes() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, one_file()).await;
    let (a, b) = (
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    );
    let log = record(&a).await;
    let (first, resume) = pause(&a).await;
    let mut replies = Vec::new();
    for replica in [&a, &b] {
        for _ in 0..3 {
            let (reply, result) = oneshot::channel();
            replica
                .inner
                .commands
                .try_send(Command::Strong(
                    Box::new(strong::Request::StoreCommand(vec![
                        0;
                        MAX_FRAME_BYTES - 256
                    ])),
                    reply,
                ))
                .unwrap();
            replies.push(result);
        }
    }
    resume.send(()).unwrap();
    first.committed().await.unwrap();
    for reply in replies {
        // Undecodable requests fail in their own savepoints.
        assert!(reply.await.unwrap().is_err());
    }
    let groups = groups(&log);
    let sizes: Vec<_> = groups[1..].iter().map(Vec::len).collect();
    assert_eq!(sizes, [4, 2]);
    for group in &groups {
        let bytes: usize = group.iter().map(|(_, weight)| weight).sum();
        assert!(bytes <= crate::writer::MAX_GROUP_BYTES || group.len() == 1);
    }
    runtime.close().await.unwrap();
}

/// Reads run on the read-only connection between groups: the reported
/// revision is the one their rows and memory-derived fields belong to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_report_the_revision_their_rows_were_read_at() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, one_file()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let writers: Vec<_> = (0..4)
        .map(|writer| {
            let replica = replica.clone();
            tokio::spawn(async move {
                for index in 0..60 {
                    loop {
                        match replica.mutate(Mutation {
                            entity: None,
                            payload: format!("{writer}-{index}").into_bytes(),
                        }) {
                            Ok(receipt) => {
                                receipt.committed().await.unwrap();
                                break;
                            }
                            Err(Error::Backpressure) => tokio::task::yield_now().await,
                            Err(error) => panic!("{error}"),
                        };
                    }
                }
            })
        })
        .collect();
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (replica, done) = (replica.clone(), done.clone());
            tokio::spawn(async move {
                let mut checked = 0;
                while !done.load(Ordering::Relaxed) || checked == 0 {
                    let snapshot = replica.query().await.unwrap();
                    let revision = snapshot.revision.get();
                    assert_eq!(snapshot.rows.len() as u64, revision);
                    let frontier: Vec<_> = snapshot
                        .frontier
                        .0
                        .iter()
                        .map(|(node, sequence)| (*node, sequence.get()))
                        .collect();
                    assert_eq!(
                        frontier,
                        (revision > 0)
                            .then_some((NODE, revision))
                            .into_iter()
                            .collect::<Vec<_>>()
                    );
                    let read = replica
                        .read(
                            BoundQuery {
                                sql: "SELECT COUNT(*) FROM drafts".into(),
                                values: Vec::new(),
                            },
                            ReadOptions::new(scope(1), 1),
                        )
                        .await
                        .unwrap();
                    assert_eq!(read.rows, count(read.revision.get() as i64));
                    assert_eq!(
                        read.completeness,
                        Completeness::Local {
                            pending_operations: 0
                        }
                    );
                    checked += 1;
                }
                checked
            })
        })
        .collect();
    for writer in writers {
        writer.await.unwrap();
    }
    done.store(true, Ordering::Relaxed);
    for reader in readers {
        assert!(reader.await.unwrap() > 0);
    }
    assert_eq!(replica.query().await.unwrap().revision.get(), 240);
    runtime.close().await.unwrap();
}

/// A statement or read transaction left open would pin the WAL; after reads
/// another connection can checkpoint and reset the whole log.
#[tokio::test]
async fn reads_leave_no_transaction_or_cursor_open() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, one_file()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    note(&replica, 1, "one").committed().await.unwrap();
    replica.query().await.unwrap();
    replica
        .read(
            BoundQuery {
                sql: "SELECT note FROM drafts".into(),
                values: Vec::new(),
            },
            ReadOptions::new(scope(1), 8),
        )
        .await
        .unwrap();
    replica
        .lookup_mutation(MutationHandle([1; 32]))
        .await
        .unwrap()
        .unwrap();
    replica.strong_status().await.unwrap();
    replica.sync_state().await.unwrap();
    let other = crate::sqlite::Connection::open(&dir.path().join(FILE)).unwrap();
    assert_eq!(
        crate::tests::rows(&other, "PRAGMA wal_checkpoint(TRUNCATE)")
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(0); 3]]
    );
    runtime.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_wal_stays_bounded_under_sustained_writes_with_readers() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, one_file()).await;
    // Small thresholds, so a short test crosses them many times.
    const FRAMES: i64 = 1000;
    crate::tests::fault(
        &runtime.domain(scope(1)).unwrap(),
        Fault::CheckpointFrames(FRAMES),
    )
    .await;
    let path = dir.path().join(FILE);
    let mut wal = path.clone().into_os_string();
    wal.push("-wal");
    let wal = std::path::PathBuf::from(wal);
    let done = Arc::new(AtomicBool::new(false));
    // Another connection keeps taking short read transactions.
    let reader = {
        let (path, done) = (path.clone(), done.clone());
        tokio::spawn(async move {
            let conn = crate::sqlite::Connection::open_read_only(&path).unwrap();
            while !done.load(Ordering::Relaxed) {
                conn.execute("BEGIN", ()).await.unwrap();
                crate::tests::rows(&conn, "SELECT COUNT(*) FROM sqlite_schema")
                    .await
                    .unwrap();
                conn.execute("COMMIT", ()).await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
    };
    let largest = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let monitor = {
        let (wal, done, largest) = (wal.clone(), done.clone(), largest.clone());
        tokio::spawn(async move {
            while !done.load(Ordering::Relaxed) {
                let size = std::fs::metadata(&wal).map_or(0, |wal| wal.len());
                largest.fetch_max(size, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        })
    };
    let clients: Vec<_> = (0..8u8)
        .map(|client| {
            let replica = runtime.domain(scope(client % 2 + 1)).unwrap();
            tokio::spawn(async move {
                for index in 0..600 {
                    loop {
                        match replica.mutate(Mutation {
                            entity: None,
                            payload: format!("{client}-{index}").into_bytes(),
                        }) {
                            Ok(receipt) => {
                                receipt.committed().await.unwrap();
                                break;
                            }
                            Err(Error::Backpressure) => tokio::task::yield_now().await,
                            Err(error) => panic!("{error}"),
                        };
                    }
                    if index % 50 == 0 {
                        replica.query().await.unwrap();
                    }
                }
            })
        })
        .collect();
    for client in clients {
        client.await.unwrap();
    }
    done.store(true, Ordering::Relaxed);
    reader.await.unwrap();
    monitor.await.unwrap();
    let frame = 4096 + 24;
    let bound = (3 * FRAMES as u64) * frame;
    let largest = largest.load(Ordering::Relaxed);
    assert!(largest > 0 && largest <= bound, "{largest} > {bound}");
    runtime.close().await.unwrap();
    // A clean close checkpoints and truncates the log.
    assert!(std::fs::metadata(&wal).map_or(0, |wal| wal.len()) == 0);
}
