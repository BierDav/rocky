//! A strong cut is bounded by the operations after its checkpoint base.
use super::*;
use crate::tests::{create, mutation};
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{AppliedFrontier, AuthorSequence, MAX_CUT_OPERATIONS, fixtures};
use tempfile::TempDir;
use todo_app::{Action, StaticPolicy, TodoApp, decode_snapshot};

/// No retained window and no automatic steps: a test drives each one.
fn manual() -> RuntimeOptions {
    RuntimeOptions {
        retention: RetentionPolicy {
            window: 0,
            automatic: false,
            ..RetentionPolicy::default()
        },
        ..RuntimeOptions::default()
    }
}
fn coordinator() -> MemoryCoordinator {
    let coordinator = MemoryCoordinator::default();
    coordinator
        .register(
            fixtures::SCOPE,
            fixtures::OWNER,
            [7; 16],
            vec![fixtures::LAPTOP, fixtures::PHONE],
            1,
            1,
        )
        .unwrap();
    coordinator
        .acquire(fixtures::SCOPE, fixtures::OWNER, fixtures::LAPTOP)
        .unwrap();
    coordinator
}
fn path(dir: &TempDir, node: NodeId) -> std::path::PathBuf {
    dir.path().join(if node == fixtures::LAPTOP {
        "laptop.db"
    } else {
        "phone.db"
    })
}
async fn open(dir: &TempDir, node: NodeId) -> Replica<TodoApp> {
    Replica::open_with_options(
        path(dir, node),
        TodoApp,
        StaticPolicy::default(),
        Identity {
            node,
            ..crate::tests::identity()
        },
        crate::tests::runtime_config(),
        manual(),
    )
    .await
    .unwrap()
}
/// Writes `count` title edits, letting group commit batch them.
async fn edits(replica: &Replica<TodoApp>, entity: OperationId, count: usize) {
    let mut receipts = Vec::new();
    for index in 0..count {
        receipts.push(
            replica
                .submit(mutation(
                    Some(entity),
                    Action::SetTitle(format!("title {index}")),
                ))
                .await
                .unwrap(),
        );
        if receipts.len() == 1024 {
            for receipt in receipts.drain(..) {
                receipt.committed().await.unwrap();
            }
        }
    }
    for receipt in receipts {
        receipt.committed().await.unwrap();
    }
}
/// Sends what `to` lacks, with acknowledgments.
async fn sync(from: &Replica<TodoApp>, to: &Replica<TodoApp>, peer: NodeId) {
    loop {
        let state = to.sync_state().await.unwrap();
        let Catchup::Operations(page) = from.catch_up(Some(peer), state).await.unwrap() else {
            panic!("behind the horizon");
        };
        if page.records.is_empty() {
            return;
        }
        for bytes in page.records {
            let ack = to.receive(bytes).await.unwrap();
            from.acknowledge(peer, ack).await.unwrap();
        }
        to.process_pending().await.unwrap();
    }
}
async fn rebootstrap(from: &Replica<TodoApp>, to: &Replica<TodoApp>) {
    let mut cursor = None;
    loop {
        let page = from.checkpoint_page(cursor).await.unwrap();
        let page = CheckpointPage::decode(&page.encode()).unwrap();
        cursor = page.next;
        if to.import_checkpoint(page).await.unwrap().is_some() {
            return;
        }
    }
}
/// Overwrites every retained operation of an idle replica's file with bytes
/// no decoder accepts, so loading any of them fails.
async fn poison_operations(dir: &TempDir, node: NodeId) {
    let db = crate::sqlite::Connection::open(&path(dir, node)).unwrap();
    db.execute("UPDATE rocky_operations SET bytes=x'00'", ())
        .await
        .unwrap();
}
fn frontier(entries: &[(NodeId, u64)]) -> AppliedFrontier {
    AppliedFrontier(
        entries
            .iter()
            .map(|(node, sequence)| (*node, AuthorSequence::new(*sequence).unwrap()))
            .collect(),
    )
}

/// The completion gate: more than 100,000 lifetime operations, pruned to a
/// recent checkpoint. The executor names its base and only the operations
/// after it; a replica that rebootstrapped from the checkpoint verifies the
/// same result; the bound is reached by operations after the base only.
#[tokio::test]
#[ignore = "soak; run by scripts/check --slow"]
async fn soak_a_long_lived_domain_executes_and_verifies_a_cut_since_its_checkpoint() {
    const LIFETIME: u64 = 100_001;
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    let entity = create(&laptop, "first").await.id;
    edits(&laptop, entity, LIFETIME as usize - 1).await;
    let checkpoint = laptop.checkpoint().await.unwrap();
    let base = frontier(&[(fixtures::LAPTOP, LIFETIME)]);
    assert_eq!(checkpoint.record.frontier, base);
    // The phone never received anything: release it and prune everything.
    laptop.declare_behind(fixtures::PHONE).await.unwrap();
    assert_eq!(laptop.prune().await.unwrap(), base);
    assert_eq!(
        laptop.retention_status().await.unwrap().retained_operations,
        0
    );
    rebootstrap(&laptop, &phone).await;
    edits(&laptop, entity, 3).await;
    sync(&laptop, &phone, fixtures::PHONE).await;
    // Sealed on the phone, executed on the laptop: the lifetime sum of the
    // cut is far beyond what a frame of identities could name.
    let request = phone
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    assert_eq!(
        request.frontier,
        frontier(&[(fixtures::LAPTOP, LIFETIME + 3)])
    );
    laptop
        .receive_request(request.encode().unwrap())
        .await
        .unwrap();
    laptop.execute_pending(&c).await.unwrap();
    let executed = laptop.confirmed_publications().await.unwrap().remove(0);
    assert_eq!(executed.package.cut_base, base);
    let required: Vec<_> = executed
        .package
        .required()
        .unwrap()
        .iter()
        .map(|id| id.sequence.get())
        .collect();
    assert_eq!(required, [LIFETIME + 1, LIFETIME + 2, LIFETIME + 3]);
    assert_eq!(
        decode_snapshot(&executed.package.rows).unwrap()[0].title,
        "title 2"
    );
    // The rebootstrapped phone rebuilds the cut from the same base.
    phone
        .receive_result(
            executed.package.encode().unwrap(),
            executed.receipt.encode(),
            &c,
        )
        .await
        .unwrap();
    let verified = phone.confirmed_publications().await.unwrap();
    assert_eq!(verified.len(), 1);
    assert_eq!(verified[0].receipt, executed.receipt);
    assert_eq!(verified[0].package, executed.package);
    // One more than the bound after the base is refused with the limit.
    edits(&laptop, entity, MAX_CUT_OPERATIONS - 3 + 1).await;
    let over = laptop
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    let error = laptop.execute_pending(&c).await.unwrap_err();
    assert!(
        matches!(
            error,
            Error::Quota {
                resource: "strong cut",
                limit: MAX_CUT_OPERATIONS
            }
        ),
        "{error}"
    );
    assert_eq!(error.reason(), FailureReason::Quota);
    // A newer base brings the same domain back under the bound.
    laptop.release_command(over.id).await.unwrap();
    let newer = laptop.checkpoint().await.unwrap().record.frontier;
    laptop
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    laptop.execute_pending(&c).await.unwrap();
    let latest = laptop.confirmed_publications().await.unwrap().remove(1);
    assert_eq!(latest.package.cut_base, newer);
    assert!(latest.package.required().unwrap().is_empty());
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

/// A base past the cut, or none at all, is pending before any operation is
/// loaded: every retained operation is unreadable, and nothing reads one.
#[tokio::test]
async fn an_unknown_or_pruned_base_is_refused_before_any_operation_is_loaded() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    // Pruned past: the laptop checkpoints beyond a cut it has not seen yet.
    create(&laptop, "sealed").await;
    sync(&laptop, &phone, fixtures::PHONE).await;
    let sealed = phone
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    create(&laptop, "after the cut").await;
    laptop.checkpoint().await.unwrap();
    laptop
        .receive_request(sealed.encode().unwrap())
        .await
        .unwrap();
    poison_operations(&dir, fixtures::LAPTOP).await;
    assert!(matches!(
        laptop.execute_pending(&c).await,
        Err(Error::StrongPending(PendingReason::CutBase))
    ));
    assert!(laptop.confirmed_publications().await.unwrap().is_empty());
    laptop.release_command(sealed.id).await.unwrap();
    // Control: a cut after the base loads its operations and fails on them.
    create(&laptop, "loaded").await;
    poison_operations(&dir, fixtures::LAPTOP).await;
    laptop
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    assert!(matches!(
        laptop.execute_pending(&c).await,
        Err(Error::Protocol(_))
    ));
    laptop.close().await.unwrap();
    // Unknown: an aborted import leaves the laptop without any base.
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    create(&laptop, "local").await;
    for index in 0..600 {
        create(&phone, &format!("todo-{index}")).await;
    }
    phone.checkpoint().await.unwrap();
    let first = phone.checkpoint_page(None).await.unwrap();
    assert!(first.next.is_some());
    assert!(laptop.import_checkpoint(first).await.unwrap().is_none());
    laptop.abort_rebootstrap().await.unwrap();
    assert_eq!(
        laptop.retention_status().await.unwrap().hold,
        Some(CheckpointHold::NoBase)
    );
    laptop
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    poison_operations(&dir, fixtures::LAPTOP).await;
    assert!(matches!(
        laptop.execute_pending(&c).await,
        Err(Error::StrongPending(PendingReason::CutBase))
    ));
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

/// An operation of the cut this replica rejected is held, so sync never
/// supplies it again. The count of held operations includes it, and the
/// request is refused instead of waiting forever on `MissingInput`.
#[tokio::test]
async fn a_rejected_operation_inside_a_cut_is_refused_not_left_missing() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    let entity = create(&phone, "phone's").await.id;
    edits(&phone, entity, 1).await;
    let request = phone
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    assert_eq!(request.frontier, frontier(&[(fixtures::PHONE, 2)]));
    // The laptop receives both, then its reducer refuses the edit (injected:
    // the phone applied what the laptop rejects).
    let state = laptop.sync_state().await.unwrap();
    let Catchup::Operations(page) = phone.catch_up(None, state).await.unwrap() else {
        panic!("nothing is pruned here");
    };
    for bytes in &page.records {
        laptop.receive(bytes.clone()).await.unwrap();
    }
    laptop.close().await.unwrap();
    let db = crate::sqlite::Connection::open(&path(&dir, fixtures::LAPTOP)).unwrap();
    db.execute(
        "UPDATE rocky_operations SET status='rejected' WHERE node=? AND sequence=2",
        [fixtures::PHONE.0.to_vec()],
    )
    .await
    .unwrap();
    drop(db);
    let laptop = open(&dir, fixtures::LAPTOP).await;
    laptop.process_pending().await.unwrap();
    assert_eq!(
        laptop.sync_state().await.unwrap().applied,
        frontier(&[(fixtures::PHONE, 1)])
    );
    laptop
        .receive_request(request.encode().unwrap())
        .await
        .unwrap();
    for _ in 0..2 {
        match laptop.execute_pending(&c).await {
            Err(Error::Rejected(reason)) => assert!(reason.contains("rejected"), "{reason}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Offering the operations again changes nothing: the edit is held
        // as rejected and refused, the create is a duplicate.
        let mut refused = 0;
        for bytes in &page.records {
            match laptop.receive(bytes.clone()).await {
                Ok(_) => {}
                Err(Error::Rejected(_)) => refused += 1,
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(refused, 1);
    }
    // An explicit release ends its hold on checkpoints.
    laptop.release_command(request.id).await.unwrap();
    laptop.checkpoint().await.unwrap();
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

/// A verifier rebuilds the cut from its own base. One that checkpointed and
/// pruned past the executor's base, but not past the cut, verifies the same
/// result without the operations the package names.
#[tokio::test]
async fn a_replica_pruned_behind_the_cut_verifies_from_its_own_base() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    let entity = create(&laptop, "first").await.id;
    edits(&laptop, entity, 2).await;
    laptop.checkpoint().await.unwrap();
    edits(&laptop, entity, 3).await;
    sync(&laptop, &phone, fixtures::PHONE).await;
    phone.checkpoint().await.unwrap();
    phone.declare_behind(fixtures::LAPTOP).await.unwrap();
    assert_eq!(
        phone.prune().await.unwrap(),
        frontier(&[(fixtures::LAPTOP, 6)])
    );
    assert_eq!(
        phone.retention_status().await.unwrap().retained_operations,
        0
    );
    laptop
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    laptop.execute_pending(&c).await.unwrap();
    let executed = laptop.confirmed_publications().await.unwrap().remove(0);
    assert_eq!(
        executed.package.cut_base,
        frontier(&[(fixtures::LAPTOP, 3)])
    );
    assert_eq!(executed.package.required().unwrap().len(), 3);
    phone
        .receive_result(
            executed.package.encode().unwrap(),
            executed.receipt.encode(),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(
        phone.confirmed_publications().await.unwrap()[0].receipt,
        executed.receipt
    );
    // The audit replays the applied result from the pruned base as well.
    phone.audit().await.unwrap();
    laptop.audit().await.unwrap();
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}
