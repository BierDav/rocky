//! Process-death coverage for the file writer. The parent test re-runs this
//! test binary as a child that dies inside a cross-domain group, between its
//! last savepoint and the commit, just after the commit, inside a checkpoint,
//! inside a grouped incoming page, or between the merged steps of a result.
use super::*;
use crate::scaling_option_tests::{NODE, PEER, grouped, note, open_node, pause, scope};
use inventory_app::InventoryApp;
use rocky_core::CompiledSchema;
use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Stdio};
use tempfile::TempDir;

const CASE: &str = "ROCKY_CRASH_CASE";
const DIR: &str = "ROCKY_CRASH_DIR";
const GROUP: u8 = 6;
const STREAM: u16 = 3000;
const SUBMITTERS: u16 = 8;

fn stream_handle(index: u16) -> MutationHandle {
    let mut handle = [0xC5; 32];
    handle[..2].copy_from_slice(&index.to_le_bytes());
    MutationHandle(handle)
}
fn child(case: &str, dir: &TempDir) -> Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["crash_tests::crash_child", "--exact", "--test-threads=1"])
        .env(CASE, case)
        .env(DIR, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Does nothing in an ordinary run; a parent test selects the case to die in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_child() {
    let (Ok(case), Ok(dir)) = (std::env::var(CASE), std::env::var(DIR)) else {
        return;
    };
    let dir = Path::new(&dir);
    if case == "install" {
        crate::layout::ABORT_BEFORE_MARKER.store(true, std::sync::atomic::Ordering::Relaxed);
        open_node(&dir.join("crash.db"), NODE, two_files()).await;
        panic!("the install did not end the process");
    }
    let runtime = open_node(&dir.join("crash.db"), NODE, grouped()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    if case == "stream" {
        let acknowledged = Arc::new(Mutex::new(
            std::fs::File::create(dir.join("acknowledged")).unwrap(),
        ));
        let submitters: Vec<_> = (0..SUBMITTERS)
            .map(|submitter| {
                let (replica, acknowledged) = (replica.clone(), acknowledged.clone());
                tokio::spawn(async move {
                    for index in (submitter..STREAM).step_by(SUBMITTERS as usize) {
                        let mutation = || Mutation {
                            entity: None,
                            payload: format!("stream-{index}").into_bytes(),
                        };
                        let receipt = loop {
                            match replica.mutate_with_handle(stream_handle(index), mutation()) {
                                Err(Error::Backpressure) => tokio::task::yield_now().await,
                                receipt => break receipt.unwrap(),
                            }
                        };
                        receipt.committed().await.unwrap();
                        writeln!(acknowledged.lock().unwrap(), "{index}").unwrap();
                    }
                })
            })
            .collect();
        for submitter in submitters {
            submitter.await.unwrap();
        }
        return;
    }
    if case == "page" {
        let page = crate::write_concurrency_tests::peer_operations(dir, GROUP).await;
        // Dies after half of the page's receipts released their savepoints.
        crate::tests::fault(
            &replica,
            worker::Fault::AbortAfterWrites(GROUP as usize / 2),
        )
        .await;
        let _ = replica.receive_page(page).await;
        panic!("the page did not end the process");
    }
    if case == "result" {
        let coordinator = crate::write_concurrency_tests::coordinator();
        let (package, receipt) =
            crate::write_concurrency_tests::confirmed(&replica, &coordinator).await;
        // Dies applying the result, after it was stored and marked verified.
        crate::tests::fault(&replica, worker::Fault::AbortAfterWrites(1)).await;
        let _ = replica
            .receive_result(package.encode().unwrap(), receipt.encode(), &coordinator)
            .await;
        panic!("the result did not end the process");
    }
    let other = runtime.domain(scope(2)).unwrap();
    if case == "checkpoint" {
        for index in 0..GROUP {
            note(
                group_domain(&replica, &other, index),
                index,
                &format!("note-{index}"),
            )
            .committed()
            .await
            .unwrap();
        }
        // A reader at the newest frame lets the checkpoint copy every frame,
        // then makes it wait to reset the log, where the process dies.
        let reader = crate::sqlite::Connection::open_read_only(&dir.join("crash.db")).unwrap();
        reader.execute("BEGIN", ()).await.unwrap();
        crate::tests::rows(&reader, "SELECT COUNT(*) FROM sqlite_schema")
            .await
            .unwrap();
        let (reply, injected) = oneshot::channel();
        enqueue(
            &replica.inner.commands,
            Command::Fault(worker::Fault::AbortInCheckpoint, reply),
        )
        .unwrap();
        injected.await.unwrap().unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        panic!("the checkpoint did not end the process");
    }
    let fault = match case.as_str() {
        // Steps alternate between the domains, so this dies after steps of both.
        "mid-group" => worker::Fault::AbortAfterWrites(GROUP as usize / 2),
        "before-commit" => worker::Fault::AbortBeforeCommit,
        "after-commit" => worker::Fault::AbortAfterCommit,
        other => panic!("unknown crash case {other}"),
    };
    // One mutation commits alone; the fault then meets the group queued behind it.
    let (first, resume) = pause(&replica).await;
    let (reply, injected) = oneshot::channel();
    enqueue(&replica.inner.commands, Command::Fault(fault, reply)).unwrap();
    let queued: Vec<_> = (0..GROUP)
        .map(|index| {
            note(
                group_domain(&replica, &other, index),
                index,
                &format!("note-{index}"),
            )
        })
        .collect();
    resume.send(()).unwrap();
    first.committed().await.unwrap();
    injected.await.unwrap().unwrap();
    for receipt in queued {
        let _ = receipt.committed().await;
    }
    panic!("the injected fault did not end the process");
}
fn two_files() -> RuntimeOptions {
    RuntimeOptions {
        files: 2,
        ..RuntimeOptions::default()
    }
}
/// Even steps write to the first domain, odd ones to the second.
fn group_domain<'a, A: StoredApplication>(
    first: &'a Replica<A>,
    second: &'a Replica<A>,
    index: u8,
) -> &'a Replica<A> {
    if index.is_multiple_of(2) {
        first
    } else {
        second
    }
}

/// Reopen checks sequence, clock, revision bound and projection against the
/// replayed history of both domains; this adds the per-handle and exact
/// revision checks. `handles` are those of the first domain.
async fn recovered(
    dir: &TempDir,
    handles: &[MutationHandle],
) -> (Runtime<InventoryApp>, Vec<Option<Envelope>>) {
    let runtime = open_node(&dir.path().join("crash.db"), NODE, grouped()).await;
    let found = consistent(&runtime.domain(scope(1)).unwrap(), handles).await;
    (runtime, found)
}
/// Every handle is committed or absent, and committed ones are exactly the
/// domain's sequences, rows and revisions.
async fn consistent(
    replica: &Replica<InventoryApp>,
    handles: &[MutationHandle],
) -> Vec<Option<Envelope>> {
    let mut found = Vec::new();
    for handle in handles {
        found.push(replica.lookup_mutation(*handle).await.unwrap());
    }
    let sequences: BTreeSet<_> = found
        .iter()
        .flatten()
        .map(|operation| operation.id.sequence.get())
        .collect();
    let committed = sequences.len() as u64;
    assert!(sequences.into_iter().eq(1..=committed));
    let snapshot = replica.query().await.unwrap();
    assert_eq!(
        (snapshot.rows.len() as u64, snapshot.revision.get()),
        (committed, committed)
    );
    found
}

/// The paused first mutation (handle 100) always survives; the group's steps
/// survive together or not at all, in both domains.
async fn group_dies(case: &str, survives: bool) {
    let dir = TempDir::new().unwrap();
    let status = child(case, &dir).wait().unwrap();
    assert_eq!(status.signal(), Some(6), "child must abort: {status:?}");
    let handle = |index: u8| MutationHandle([index; 32]);
    let first: Vec<_> = std::iter::once(100)
        .chain((0..GROUP).step_by(2))
        .map(handle)
        .collect();
    let second: Vec<_> = (1..GROUP).step_by(2).map(handle).collect();
    let (runtime, found) = recovered(&dir, &first).await;
    let other = consistent(&runtime.domain(scope(2)).unwrap(), &second).await;
    assert!(found[0].is_some());
    assert!(
        found[1..]
            .iter()
            .chain(&other)
            .all(|found| found.is_some() == survives)
    );
    // Retrying each handle returns the committed operation or commits it once.
    let replicas = [
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    ];
    for index in 0..GROUP {
        let replica = group_domain(&replicas[0], &replicas[1], index);
        let before = if index % 2 == 0 {
            &found[1 + usize::from(index / 2)]
        } else {
            &other[usize::from(index / 2)]
        };
        let retried = note(replica, index, &format!("note-{index}"))
            .committed()
            .await
            .unwrap();
        let expected = u64::from(index / 2) + if index % 2 == 0 { 2 } else { 1 };
        assert_eq!(retried.id.sequence.get(), expected);
        assert!(before.as_ref().is_none_or(|before| *before == retried));
    }
    runtime.close().await.unwrap();
    let (runtime, found) = recovered(&dir, &first).await;
    let other = consistent(&runtime.domain(scope(2)).unwrap(), &second).await;
    assert!(found.iter().chain(&other).all(Option::is_some));
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn death_inside_a_group_leaves_every_handle_absent() {
    group_dies("mid-group", false).await;
}

#[tokio::test]
async fn death_between_the_last_savepoint_and_commit_leaves_every_handle_absent() {
    group_dies("before-commit", false).await;
}

#[tokio::test]
async fn death_after_a_group_commit_keeps_every_handle() {
    group_dies("after-commit", true).await;
}

#[tokio::test]
async fn death_inside_a_checkpoint_keeps_every_committed_handle() {
    let dir = TempDir::new().unwrap();
    let status = child("checkpoint", &dir).wait().unwrap();
    assert_eq!(status.signal(), Some(6), "child must abort: {status:?}");
    // It died after copying the log into the database file, before resetting it.
    let copy = dir.path().join("copy.db");
    std::fs::copy(dir.path().join("crash.db"), &copy).unwrap();
    let copied = crate::scoped_connection::ScopedConnection::new(
        crate::sqlite::Connection::open(&copy).unwrap(),
        Some(&crate::runtime::namespace(scope(1))),
        InventoryApp.schema(),
    );
    assert_eq!(
        crate::storage::rows(&copied, "SELECT COUNT(*) FROM rocky_operations")
            .await
            .unwrap(),
        vec![vec![rocky_core::SqlValue::Integer(i64::from(GROUP / 2))]]
    );
    let mut wal = dir.path().join("crash.db").into_os_string();
    wal.push("-wal");
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    let handle = |index: u8| MutationHandle([index; 32]);
    let first: Vec<_> = (0..GROUP).step_by(2).map(handle).collect();
    let second: Vec<_> = (1..GROUP).step_by(2).map(handle).collect();
    let (runtime, found) = recovered(&dir, &first).await;
    let other = consistent(&runtime.domain(scope(2)).unwrap(), &second).await;
    assert!(found.iter().chain(&other).all(Option::is_some));
    runtime.close().await.unwrap();
}

/// A page dies with half of its receipts written: none of them survives, and
/// the sender's retry is received as new.
#[tokio::test]
async fn death_inside_an_incoming_page_leaves_none_of_its_records() {
    let dir = TempDir::new().unwrap();
    let status = child("page", &dir).wait().unwrap();
    assert_eq!(status.signal(), Some(6), "child must abort: {status:?}");
    let source = open_node(&dir.path().join("peer.db"), PEER, grouped()).await;
    let page = source
        .domain(scope(1))
        .unwrap()
        .operation_page(None)
        .await
        .unwrap()
        .items;
    assert_eq!(page.len(), usize::from(GROUP));
    let (runtime, _) = recovered(&dir, &[]).await;
    let replica = runtime.domain(scope(1)).unwrap();
    assert!(replica.sync_state().await.unwrap().received.is_empty());
    for result in replica.receive_page(page).await.unwrap() {
        assert!(result.unwrap().applied);
    }
    runtime.close().await.unwrap();
    let runtime = open_node(&dir.path().join("crash.db"), NODE, grouped()).await;
    let snapshot = runtime.domain(scope(1)).unwrap().query().await.unwrap();
    assert_eq!(snapshot.rows.len(), usize::from(GROUP));
    runtime.close().await.unwrap();
    source.close().await.unwrap();
}

/// A result dies between being stored, verified and applied: none of the
/// three survives, as if the process had died before the result arrived.
#[tokio::test]
async fn death_between_the_merged_steps_of_a_result_leaves_none_of_them() {
    let dir = TempDir::new().unwrap();
    let status = child("result", &dir).wait().unwrap();
    assert_eq!(status.signal(), Some(6), "child must abort: {status:?}");
    let (runtime, _) = recovered(&dir, &[]).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let strong = replica.strong_status().await.unwrap();
    assert!(strong.received.is_empty() && strong.verification.is_empty());
    assert_eq!(strong.sequence, 0);
    assert_eq!(strong.commands[0].state, CommandState::OutcomeUnknown);
    // The coordinator died with the child; a new one at the same
    // configuration confirms the retained attempt again.
    let coordinator = crate::write_concurrency_tests::coordinator();
    replica.execute_pending(&coordinator).await.unwrap();
    assert_eq!(replica.confirmed_publications().await.unwrap().len(), 1);
    runtime.close().await.unwrap();
    let runtime = open_node(&dir.path().join("crash.db"), NODE, grouped()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    assert_eq!(replica.strong_status().await.unwrap().sequence, 1);
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn killed_writers_keep_every_acknowledged_group() {
    let handles: Vec<_> = (0..STREAM).map(stream_handle).collect();
    for threshold in [1, 64, 500] {
        let dir = TempDir::new().unwrap();
        let mut child = child("stream", &dir);
        let acknowledged = || {
            let text = std::fs::read_to_string(dir.path().join("acknowledged")).unwrap_or_default();
            // A line without its newline may be a torn final write.
            let complete = text.rfind('\n').map_or("", |end| &text[..end]);
            complete
                .lines()
                .map(|line| line.parse::<usize>().unwrap())
                .collect::<Vec<_>>()
        };
        while acknowledged().len() < threshold {
            assert!(child.try_wait().unwrap().is_none(), "child ended early");
            tokio::task::yield_now().await;
        }
        child.kill().unwrap();
        child.wait().unwrap();
        let (runtime, found) = recovered(&dir, &handles).await;
        let acknowledged = acknowledged();
        assert!(acknowledged.len() >= threshold);
        for index in acknowledged {
            assert!(found[index].is_some(), "acknowledged write {index} lost");
        }
        runtime.close().await.unwrap();
    }
}

/// Death after the second file is installed and before file 0 records the
/// set: the next open resumes the install instead of refusing or losing it.
#[tokio::test]
async fn death_between_installing_files_resumes_the_install() {
    let dir = TempDir::new().unwrap();
    let status = child("install", &dir).wait().unwrap();
    assert_eq!(status.signal(), Some(6), "child must abort: {status:?}");
    let path = dir.path().join("crash.db");
    assert!(
        std::fs::metadata(crate::layout::file_path(&path, 1))
            .unwrap()
            .len()
            > 0
    );
    let runtime = open_node(&path, NODE, two_files()).await;
    for domain in [1, 2] {
        let replica = runtime.domain(scope(domain)).unwrap();
        note(&replica, 1, "after resume").committed().await.unwrap();
    }
    runtime.close().await.unwrap();
    let runtime = open_node(&path, NODE, two_files()).await;
    for domain in [1, 2] {
        let snapshot = runtime
            .domain(scope(domain))
            .unwrap()
            .query()
            .await
            .unwrap();
        assert_eq!(snapshot.revision.get(), 1);
    }
    runtime.close().await.unwrap();
}
