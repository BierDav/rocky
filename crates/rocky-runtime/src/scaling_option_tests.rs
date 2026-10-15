use super::*;
use inventory_app::{InventoryApp, Policy};
use rocky_core::*;
use tempfile::TempDir;

pub(super) const NODE: NodeId = NodeId([3; 16]);
pub(super) const PEER: NodeId = NodeId([4; 16]);
const CALLER: PrincipalId = PrincipalId([7; 16]);
pub(super) fn scope(domain: u8) -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain: DomainId([domain; 16]),
    }
}
async fn open(dir: &TempDir, options: RuntimeOptions) -> Runtime<InventoryApp> {
    open_node(&dir.path().join("options.db"), NODE, options).await
}
/// Two domains that accept operations authored by either node.
pub(super) async fn open_node(
    path: &Path,
    node: NodeId,
    options: RuntimeOptions,
) -> Runtime<InventoryApp> {
    Runtime::open_with_options(path, installations(node), options)
        .await
        .unwrap()
}
pub(super) fn installations(node: NodeId) -> Vec<DomainInstallation<InventoryApp>> {
    let caller = CALLER;
    [1, 2]
        .into_iter()
        .map(|domain| DomainInstallation {
            app: InventoryApp,
            policy: Policy {
                scope: scope(domain),
                caller,
                reference: [9; 32],
                note_prefix: None,
            },
            identity: Identity {
                scope: scope(domain),
                node,
                caller,
            },
            config: RuntimeConfig {
                authors: vec![(NODE, caller), (PEER, caller)],
                delivery_targets: Vec::new(),
                full_nodes: Vec::new(),
            },
        })
        .collect()
}
pub(super) fn note(replica: &Replica<InventoryApp>, handle: u8, text: &str) -> MutationReceipt {
    replica
        .mutate_with_handle(
            MutationHandle([handle; 32]),
            Mutation {
                entity: None,
                payload: text.as_bytes().to_vec(),
            },
        )
        .unwrap()
}
/// Holds the worker inside its first transaction so later mutations queue up.
pub(super) async fn pause(
    replica: &Replica<InventoryApp>,
) -> (MutationReceipt, oneshot::Sender<()>) {
    let (entered, waiting) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    crate::tests::fault(replica, worker::Fault::Pause(entered, resumed)).await;
    let first = note(replica, 100, "first");
    waiting.await.unwrap();
    (first, resume)
}
/// Both domains in one file, so their writes share groups.
pub(super) fn grouped() -> RuntimeOptions {
    RuntimeOptions {
        group_commit: 32,
        files: 1,
        ..RuntimeOptions::default()
    }
}
async fn count(replica: &Replica<InventoryApp>, domain: u8) -> ReadSnapshot {
    select(replica, domain, "SELECT COUNT(*) FROM drafts").await
}
async fn select(replica: &Replica<InventoryApp>, domain: u8, sql: &str) -> ReadSnapshot {
    replica
        .read(
            BoundQuery {
                sql: sql.into(),
                values: Vec::new(),
            },
            ReadOptions::new(scope(domain), 1),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn grouped_mutations_keep_order_identity_and_survive_reopen() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, grouped()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let (first, resume) = pause(&replica).await;
    let queued: Vec<_> = (0..6)
        .map(|index| note(&replica, index, &format!("note-{index}")))
        .collect();
    let retry = note(&replica, 0, "note-0");
    let conflicting = note(&replica, 1, "different");
    resume.send(()).unwrap();
    assert_eq!(first.committed().await.unwrap().id.sequence.get(), 1);
    let mut committed = Vec::new();
    for receipt in queued {
        committed.push(receipt.committed().await.unwrap());
    }
    for (index, operation) in committed.iter().enumerate() {
        assert_eq!(operation.id.sequence.get(), index as u64 + 2);
    }
    assert_eq!(retry.committed().await.unwrap(), committed[0]);
    assert!(matches!(
        conflicting.committed().await,
        Err(Error::Conflict)
    ));
    assert_eq!(replica.query().await.unwrap().revision.get(), 7);
    runtime.close().await.unwrap();
    // Reopen replays history and compares it with counters and projection.
    let runtime = open(&dir, grouped()).await;
    let snapshot = runtime.domain(scope(1)).unwrap().query().await.unwrap();
    assert_eq!((snapshot.rows.len(), snapshot.revision.get()), (7, 7));
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn a_busy_step_reruns_only_its_savepoint() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, grouped()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let (first, resume) = pause(&replica).await;
    // Queued behind the paused mutation: the fault hits the following group.
    let (reply, injected) = oneshot::channel();
    enqueue(
        &replica.inner.commands,
        Command::Fault(worker::Fault::RetryBeforeCommit(1), reply),
    )
    .unwrap();
    let queued: Vec<_> = (0..4)
        .map(|index| note(&replica, index, &format!("note-{index}")))
        .collect();
    resume.send(()).unwrap();
    first.committed().await.unwrap();
    injected.await.unwrap().unwrap();
    for (index, receipt) in queued.into_iter().enumerate() {
        let operation = receipt.committed().await.unwrap();
        assert_eq!(operation.id.sequence.get(), index as u64 + 2);
    }
    assert_eq!(replica.query().await.unwrap().revision.get(), 5);
    runtime.close().await.unwrap();
    let runtime = open(&dir, grouped()).await;
    let snapshot = runtime.domain(scope(1)).unwrap().query().await.unwrap();
    assert_eq!((snapshot.rows.len(), snapshot.revision.get()), (5, 5));
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn reads_follow_commits_and_page_instead_of_refusing() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, RuntimeOptions::default()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    note(&replica, 1, "one").committed().await.unwrap();
    let before = count(&replica, 1).await;
    assert_eq!(before, count(&replica, 1).await);
    assert_eq!(before.rows, vec![vec![SqlValue::Integer(1)]]);
    note(&replica, 2, "two").committed().await.unwrap();
    let after = count(&replica, 1).await;
    assert_eq!(after.rows, vec![vec![SqlValue::Integer(2)]]);
    assert_eq!(after.revision.get(), 2);
    let notes = |max_rows| {
        replica.read(
            BoundQuery {
                sql: "SELECT note FROM drafts ORDER BY sequence".into(),
                values: Vec::new(),
            },
            ReadOptions::new(scope(1), max_rows),
        )
    };
    let both = notes(2).await.unwrap();
    assert_eq!((both.rows.len(), both.next), (2, None));
    // More rows than `max_rows` end the page with a cursor, not an error.
    let first = notes(1).await.unwrap();
    assert_eq!(first.rows, vec![vec![SqlValue::Text("one".into())]]);
    let second = replica
        .read(
            BoundQuery {
                sql: "SELECT note FROM drafts ORDER BY sequence".into(),
                values: Vec::new(),
            },
            ReadOptions::new(scope(1), 1).after(first.next.unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(second.rows, vec![vec![SqlValue::Text("two".into())]]);
    assert_eq!(second.next, None);
    runtime.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_domains_share_the_file_writer_and_reopen() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, RuntimeOptions::default()).await;
    let writes = |domain: u8| {
        let replica = runtime.domain(scope(domain)).unwrap();
        async move {
            for index in 0..24u8 {
                note(&replica, index, &format!("{domain}-{index}"))
                    .committed()
                    .await
                    .unwrap();
            }
        }
    };
    tokio::join!(writes(1), writes(2));
    runtime.close().await.unwrap();
    let runtime = open(&dir, RuntimeOptions::default()).await;
    for domain in [1, 2] {
        let replica = runtime.domain(scope(domain)).unwrap();
        assert_eq!(
            count(&replica, domain).await.rows,
            vec![vec![SqlValue::Integer(24)]]
        );
    }
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn reads_follow_incoming_application_and_reopen() {
    let dir = TempDir::new().unwrap();
    let source = open_node(&dir.path().join("peer.db"), PEER, RuntimeOptions::default()).await;
    let remote = note(&source.domain(scope(1)).unwrap(), 1, "remote")
        .committed()
        .await
        .unwrap();
    let runtime = open(&dir, RuntimeOptions::default()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    note(&replica, 1, "local").committed().await.unwrap();
    assert_eq!(count(&replica, 1).await.rows, [[SqlValue::Integer(1)]]);
    // Durable receipt changes completeness but not the applied rows.
    replica.receive(remote.encode().unwrap()).await.unwrap();
    let received = count(&replica, 1).await;
    assert_eq!(received.rows, [[SqlValue::Integer(1)]]);
    assert_eq!(
        received.completeness,
        Completeness::Local {
            pending_operations: 1
        }
    );
    replica.process_pending().await.unwrap();
    let applied = count(&replica, 1).await;
    assert_eq!(applied.rows, [[SqlValue::Integer(2)]]);
    assert_eq!(
        applied.completeness,
        Completeness::Local {
            pending_operations: 0
        }
    );
    runtime.close().await.unwrap();
    let runtime = open(&dir, RuntimeOptions::default()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    assert_eq!(count(&replica, 1).await, applied);
    note(&replica, 2, "later").committed().await.unwrap();
    assert_eq!(count(&replica, 1).await.rows, [[SqlValue::Integer(3)]]);
    runtime.close().await.unwrap();
    source.close().await.unwrap();
}

#[tokio::test]
async fn group_size_and_checkpoint_frames_are_bounded() {
    let dir = TempDir::new().unwrap();
    let defaults = RuntimeOptions::default();
    let group = |group_commit| RuntimeOptions {
        group_commit,
        ..defaults
    };
    let frames = |checkpoint_frames| RuntimeOptions {
        checkpoint_frames,
        ..defaults
    };
    for options in [
        group(0),
        group(crate::runtime::MAX_GROUP_COMMIT + 1),
        frames(crate::runtime::MIN_CHECKPOINT_FRAMES - 1),
        frames(crate::runtime::MAX_CHECKPOINT_FRAMES + 1),
    ] {
        let caller = PrincipalId([7; 16]);
        let result = Runtime::open_with_options(
            dir.path().join("bounded.db"),
            vec![DomainInstallation {
                app: InventoryApp,
                policy: Policy {
                    scope: scope(1),
                    caller,
                    reference: [9; 32],
                    note_prefix: None,
                },
                identity: Identity {
                    scope: scope(1),
                    node: NodeId([3; 16]),
                    caller,
                },
                config: RuntimeConfig::default(),
            }],
            options,
        )
        .await;
        assert!(matches!(result, Err(Error::Quota { .. })));
    }
}
