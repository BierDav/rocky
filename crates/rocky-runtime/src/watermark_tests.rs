//! Buffered commits behind the durable watermark: nothing leaves the device
//! before a flush covers it, so power loss can drop only never-exposed work.
use super::*;
use crate::scaling_option_tests::{NODE, PEER, note, open_node, scope};
use crate::worker::Fault;
use inventory_app::InventoryApp;
use rocky_core::*;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn one_file() -> RuntimeOptions {
    RuntimeOptions {
        files: 1,
        ..RuntimeOptions::default()
    }
}
/// Copies the file as it stood at its latest flush here.
async fn snapshots(replica: &Replica<InventoryApp>, directory: &Path) {
    crate::tests::fault(replica, Fault::SnapshotAtFlush(directory.to_path_buf())).await;
}
/// Rows of `domain` as power loss after the latest flush would leave them.
async fn flushed(snapshot: &Path, file: &str, domain: u8, sql: &str) -> Vec<Vec<SqlValue>> {
    let copy = TempDir::new().unwrap();
    for name in [file.to_string(), format!("{file}-wal")] {
        let _ = std::fs::copy(snapshot.join(&name), copy.path().join(&name));
    }
    let conn = crate::scoped_connection::ScopedConnection::new(
        crate::sqlite::Connection::open(&copy.path().join(file)).unwrap(),
        Some(&crate::runtime::namespace(scope(domain))),
        InventoryApp.schema(),
    );
    storage::rows(&conn, sql).await.unwrap()
}
async fn flushed_operations(snapshot: &Path, file: &str) -> Vec<Vec<u8>> {
    flushed(
        snapshot,
        file,
        1,
        "SELECT bytes FROM rocky_operations ORDER BY node,sequence",
    )
    .await
    .into_iter()
    .map(|row| match row.as_slice() {
        [SqlValue::Blob(bytes)] => bytes.clone(),
        other => panic!("{other:?}"),
    })
    .collect()
}

#[tokio::test]
async fn a_buffered_commit_is_never_reported_as_flushed() {
    let dir = TempDir::new().unwrap();
    let runtime = open_node(&dir.path().join("buffered.db"), NODE, one_file()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let first = note(&replica, 1, "buffered").receipt().await.unwrap();
    assert_eq!(first.class, CommitClass::Buffered);
    let lookup = replica
        .lookup_commit(MutationHandle([1; 32]))
        .await
        .unwrap();
    assert_eq!(
        lookup.map(|commit| commit.class),
        Some(CommitClass::Buffered)
    );
    // Awaiting the flush forces one, which covers every earlier commit.
    let second = note(&replica, 2, "flushed").flushed().await.unwrap();
    for (handle, operation) in [(1, &first.operation), (2, &second)] {
        let lookup = replica
            .lookup_commit(MutationHandle([handle; 32]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (&lookup.operation, lookup.class),
            (operation, CommitClass::Flushed)
        );
    }
    // A retried handle reports the class its original commit reached.
    let retried = note(&replica, 1, "buffered").receipt().await.unwrap();
    assert_eq!(retried.class, CommitClass::Flushed);
    replica.flush().await.unwrap();
    runtime.close().await.unwrap();
    let full = open_node(
        &dir.path().join("full.db"),
        NODE,
        RuntimeOptions {
            durability: Durability::Full,
            ..one_file()
        },
    )
    .await;
    let committed = note(&full.domain(scope(1)).unwrap(), 1, "full")
        .receipt()
        .await
        .unwrap();
    assert_eq!(committed.class, CommitClass::Flushed);
    full.close().await.unwrap();
}

/// Every exposure point answers only after a flush covering what it shows:
/// the latest flushed copy of the file already holds it.
#[tokio::test]
async fn nothing_leaves_the_device_before_a_flush_covers_it() {
    let dir = TempDir::new().unwrap();
    let snapshot = dir.path().join("flushed");
    let runtime = open_node(&dir.path().join("author.db"), NODE, one_file()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    snapshots(&replica, &snapshot).await;
    let mut sent = Vec::new();
    let mut expose = async |label: &str, exposed: Vec<Vec<u8>>| {
        let operations = flushed_operations(&snapshot, "author.db").await;
        assert!(!exposed.is_empty(), "{label}");
        for bytes in &exposed {
            assert!(
                operations.contains(bytes),
                "{label} exposed a buffered operation"
            );
        }
        sent.extend(exposed);
    };
    let one = note(&replica, 1, "page").committed().await.unwrap();
    expose("page", replica.operation_page(None).await.unwrap().items).await;
    let two = note(&replica, 2, "missing").committed().await.unwrap();
    expose(
        "missing",
        replica
            .missing_operations(vec![one.id])
            .await
            .unwrap()
            .records,
    )
    .await;
    let three = note(&replica, 3, "operation").committed().await.unwrap();
    expose(
        "operation",
        vec![replica.operation(three.scoped_id()).await.unwrap().unwrap()],
    )
    .await;
    let four = note(&replica, 4, "state").committed().await.unwrap();
    let state = replica.sync_state().await.unwrap();
    assert!(state.holds(four.id));
    expose("state", vec![four.encode().unwrap()]).await;
    let five = note(&replica, 5, "sent").committed().await.unwrap();
    expose(
        "sent",
        vec![replica.operation(five.scoped_id()).await.unwrap().unwrap()],
    )
    .await;
    // A receiver's acknowledgment lets the sender drop its outbox entry, so
    // the receipt is flushed before it is returned.
    let peer_snapshot = dir.path().join("peer-flushed");
    let peer = open_node(&dir.path().join("peer.db"), PEER, one_file()).await;
    let receiving = peer.domain(scope(1)).unwrap();
    snapshots(&receiving, &peer_snapshot).await;
    for bytes in &sent {
        receiving.receive(bytes.clone()).await.unwrap();
        assert!(
            flushed_operations(&peer_snapshot, "peer.db")
                .await
                .contains(bytes)
        );
    }
    drop(two);
    runtime.close().await.unwrap();
    peer.close().await.unwrap();
}

/// Restores the files as they stood at the latest flush, as power loss would.
fn lose_unflushed(snapshot: &Path, live: &Path, file: &str) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(live.join(format!("{file}{suffix}")));
    }
    for suffix in ["", "-wal"] {
        let _ = std::fs::copy(
            snapshot.join(format!("{file}{suffix}")),
            live.join(format!("{file}{suffix}")),
        );
    }
}

/// Local writes stay buffered while incoming pages and strong commands run
/// on the same domain, and each exposure is on the device when it returns.
#[tokio::test]
async fn exposure_stays_behind_the_watermark_with_incoming_work() {
    let dir = TempDir::new().unwrap();
    let snapshot = dir.path().join("flushed");
    let remote = crate::write_concurrency_tests::peer_operations(dir.path(), 12).await;
    let runtime = open_node(&dir.path().join("node.db"), NODE, one_file()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    snapshots(&replica, &snapshot).await;
    // The first copy is taken at the first flush.
    note(&replica, 200, "first").flushed().await.unwrap();
    let on_device = async |bytes: &Vec<u8>| {
        flushed_operations(&snapshot, "node.db")
            .await
            .contains(bytes)
    };
    for round in 0..4u8 {
        let local = note(&replica, 2 * round + 1, "before a page")
            .receipt()
            .await
            .unwrap();
        let local_bytes = local.operation.encode().unwrap();
        assert_eq!(local.class, CommitClass::Buffered);
        assert!(!on_device(&local_bytes).await);
        // A page is acknowledged to its sender, so it is flushed first.
        let page = remote[usize::from(round) * 3..][..3].to_vec();
        for result in replica.receive_page(page.clone()).await.unwrap() {
            assert!(result.unwrap().applied);
        }
        for bytes in page.iter().chain([&local_bytes]) {
            assert!(on_device(bytes).await);
        }
        let covered = replica
            .lookup_commit(MutationHandle([2 * round + 1; 32]))
            .await
            .unwrap();
        assert_eq!(covered.unwrap().class, CommitClass::Flushed);
    }
    runtime.close().await.unwrap();
}

/// Replaces the earlier fork demonstration: losing buffered commits can no
/// longer fork an author log, because nothing above the watermark was sent.
#[tokio::test]
async fn losing_unflushed_commits_drops_only_unexposed_work_and_never_forks() {
    let dir = TempDir::new().unwrap();
    let (live, snapshot): (PathBuf, PathBuf) =
        (dir.path().join("author"), dir.path().join("flushed"));
    let file = live.join("author.db");
    let author = open_node(&file, NODE, one_file()).await;
    let authoring = author.domain(scope(1)).unwrap();
    snapshots(&authoring, &snapshot).await;
    let peer = open_node(&dir.path().join("peer.db"), PEER, one_file()).await;
    let replica = peer.domain(scope(1)).unwrap();
    let kept = note(&authoring, 1, "kept").committed().await.unwrap();
    for bytes in authoring.operation_page(None).await.unwrap().items {
        replica.receive(bytes).await.unwrap();
    }
    replica.process_pending().await.unwrap();
    // Committed and visible locally, but never exposed.
    let lost = note(&authoring, 2, "lost").receipt().await.unwrap();
    assert_eq!(lost.class, CommitClass::Buffered);
    assert_eq!(authoring.query().await.unwrap().rows.len(), 2);
    drop(authoring);
    author.close().await.unwrap();
    lose_unflushed(&snapshot, &live, "author.db");
    let author = open_node(&file, NODE, one_file()).await;
    let authoring = author.domain(scope(1)).unwrap();
    assert_eq!(
        authoring
            .lookup_mutation(MutationHandle([1; 32]))
            .await
            .unwrap(),
        Some(kept.clone())
    );
    assert!(
        authoring
            .lookup_mutation(MutationHandle([2; 32]))
            .await
            .unwrap()
            .is_none()
    );
    let snapshot_rows = authoring.query().await.unwrap();
    assert_eq!(
        (snapshot_rows.rows.len(), snapshot_rows.revision.get()),
        (1, 1)
    );
    // The retried handle commits once, reusing the sequence nobody saw.
    let retried = note(&authoring, 2, "lost").committed().await.unwrap();
    assert_eq!(retried.id, lost.operation.id);
    assert_eq!(
        note(&authoring, 2, "lost").committed().await.unwrap(),
        retried
    );
    let later = note(&authoring, 3, "later").committed().await.unwrap();
    for bytes in authoring.operation_page(Some(kept.id)).await.unwrap().items {
        // No IdentityConflict: the peer never held the lost bytes.
        replica.receive(bytes).await.unwrap();
    }
    replica.process_pending().await.unwrap();
    let notes = |snapshot: Snapshot<(OperationId, String)>| -> Vec<String> {
        snapshot.rows.into_iter().map(|(_, note)| note).collect()
    };
    assert_eq!(
        notes(authoring.query().await.unwrap()),
        ["kept", "lost", "later"]
    );
    assert_eq!(
        notes(replica.query().await.unwrap()),
        ["kept", "lost", "later"]
    );
    assert_eq!(later.id.sequence.get(), 3);
    author.close().await.unwrap();
    peer.close().await.unwrap();
}
