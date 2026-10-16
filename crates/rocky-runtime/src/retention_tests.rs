//! Checkpoints, the pruned prefix, pruning and rebootstrap of the trusted path.
use super::*;
use crate::checkpoint::{Request, Response, Step};
use crate::tests::{create, fault, mutation};
use rocky_core::{SqlValue, fixtures};
use std::os::unix::process::ExitStatusExt;
use tempfile::TempDir;
use todo_app::{Action, StaticPolicy, TodoApp};

const CASE: &str = "ROCKY_RETENTION_CASE";
const DIR: &str = "ROCKY_RETENTION_DIR";

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
fn file(node: NodeId) -> &'static str {
    if node == fixtures::LAPTOP {
        "laptop.db"
    } else {
        "phone.db"
    }
}
async fn open_at(dir: &Path, node: NodeId, options: RuntimeOptions) -> Replica<TodoApp> {
    Replica::open_with_options(
        dir.join(file(node)),
        TodoApp,
        StaticPolicy::default(),
        Identity {
            node,
            ..crate::tests::identity()
        },
        crate::tests::runtime_config(),
        options,
    )
    .await
    .unwrap()
}
async fn open(dir: &TempDir, node: NodeId) -> Replica<TodoApp> {
    open_at(dir.path(), node, manual()).await
}
async fn edit(replica: &Replica<TodoApp>, entity: OperationId, action: Action) -> Envelope {
    replica
        .mutate(mutation(Some(entity), action))
        .unwrap()
        .committed()
        .await
        .unwrap()
}
/// Sends what `to` lacks, with acknowledgments; false if it is behind the pruned_prefix.
async fn sync(from: &Replica<TodoApp>, to: &Replica<TodoApp>, peer: NodeId) -> bool {
    loop {
        let state = to.sync_state().await.unwrap();
        let page = match from.catch_up(Some(peer), state).await.unwrap() {
            Catchup::Operations(page) => page,
            Catchup::BehindPruned(_) => return false,
        };
        if page.records.is_empty() {
            return true;
        }
        for bytes in page.records {
            let ack = to.receive(bytes).await.unwrap();
            from.acknowledge(peer, ack).await.unwrap();
        }
        match to.process_pending().await {
            Ok(()) | Err(Error::Rejected(_)) => {}
            Err(error) => panic!("{error}"),
        }
    }
}
/// Copies `from`'s checkpoint into `to`, page by page over the wire encoding.
async fn rebootstrap(from: &Replica<TodoApp>, to: &Replica<TodoApp>) -> RebootstrapReport {
    let mut cursor = None;
    loop {
        let page = from.checkpoint_page(cursor).await.unwrap();
        let page = CheckpointPage::decode(&page.encode()).unwrap();
        cursor = page.next;
        if let Some(report) = to.import_checkpoint(page).await.unwrap() {
            assert!(cursor.is_none());
            return report;
        }
    }
}
async fn disk(dir: &TempDir, node: NodeId, sql: &str) -> Vec<Vec<SqlValue>> {
    let db = crate::sqlite::Connection::open(&dir.path().join(file(node))).unwrap();
    storage::collect_rows(db.query(sql, ()).await.unwrap())
        .await
        .unwrap()
}
async fn count(dir: &TempDir, node: NodeId, table: &str) -> i64 {
    match disk(dir, node, &format!("SELECT COUNT(*) FROM {table}"))
        .await
        .as_slice()
    {
        [row] => match row.as_slice() {
            [SqlValue::Integer(count)] => *count,
            _ => panic!("count"),
        },
        _ => panic!("count"),
    }
}
fn sequence(frontier: &rocky_core::AppliedFrontier, node: NodeId) -> u64 {
    frontier
        .0
        .iter()
        .find(|(author, _)| *author == node)
        .map_or(0, |(_, sequence)| sequence.get())
}
async fn step(replica: &Replica<TodoApp>, request: Request) -> Result<Step, Error> {
    match replica.retention_request(request).await? {
        Response::Step(step) => Ok(step),
        _ => panic!("step response"),
    }
}

#[tokio::test]
async fn a_checkpoint_recomputes_the_projection_and_pruning_waits_for_delivery() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let kept = create(&laptop, "kept").await.id;
    let deleted = create(&laptop, "deleted").await.id;
    edit(&laptop, kept, Action::SetTitle("renamed".into())).await;
    edit(&laptop, deleted, Action::DeleteTodo).await;
    let first = laptop.checkpoint().await.unwrap();
    assert_eq!(first.record.sequence, 1);
    assert_eq!(sequence(&first.record.frontier, fixtures::LAPTOP), 4);
    // Rows include the tombstone: deletion evidence is part of the state.
    assert_eq!(
        (first.record.state.count, first.record.history.count),
        (2, 4)
    );
    assert_eq!(first.record.clock, 4);
    assert_eq!(
        CheckpointRecord::decode(&first.record.encode()).unwrap(),
        first.record
    );
    // Nothing is acknowledged: the only other copy would be lost.
    assert!(laptop.prune().await.unwrap().0.is_empty());
    assert_eq!(count(&dir, fixtures::LAPTOP, "rocky_operations").await, 4);
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    assert_eq!(
        sequence(&laptop.prune().await.unwrap(), fixtures::LAPTOP),
        4
    );
    for table in ["rocky_operations", "rocky_peer_ack", "rocky_outbox"] {
        assert_eq!(count(&dir, fixtures::LAPTOP, table).await, 0, "{table}");
    }
    // Handles outlive their operations for retries.
    assert_eq!(
        count(&dir, fixtures::LAPTOP, "rocky_mutation_handles").await,
        4
    );
    let status = laptop.retention_status().await.unwrap();
    assert_eq!((status.retained_operations, status.retained_bytes), (0, 0));
    // A second checkpoint is recomputed from the first.
    edit(&laptop, kept, Action::SetCompleted(true)).await;
    let second = laptop.checkpoint().await.unwrap();
    assert_eq!(second.record.previous, first.commitment);
    assert_eq!(second.record.history.count, 5);
    let before = laptop.query().await.unwrap();
    laptop.close().await.unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    assert_eq!(laptop.query().await.unwrap(), before);
    let state = laptop.sync_state().await.unwrap();
    assert_eq!(sequence(&state.pruned_prefix, fixtures::LAPTOP), 4);
    // The one retained operation lies inside the held frontier.
    assert_eq!(sequence(&state.held, fixtures::LAPTOP), 5);
    assert!(state.received.is_empty());
    assert_eq!(sequence(&state.applied, fixtures::LAPTOP), 5);
    let report = laptop.audit().await.unwrap();
    assert!(report.recomputed);
    assert_eq!((report.rows, report.retained_operations), (2, 1));
    // Replicas at one frontier commit to the same rows and history.
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    let remote = phone.checkpoint().await.unwrap().record;
    assert_eq!(
        (remote.frontier, remote.state, remote.history, remote.clock),
        (
            second.record.frontier,
            second.record.state,
            second.record.history,
            second.record.clock
        )
    );
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

#[tokio::test]
async fn an_unverifiable_checkpoint_blocks_pruning() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let todo = create(&laptop, "original").await;
    edit(&laptop, todo.id, Action::SetCompleted(true)).await;
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    laptop.close().await.unwrap();
    // The stored history no longer produces the stored projection.
    let forged = Envelope {
        payload: Action::CreateTodo {
            title: "forged".into(),
            completed: false,
        }
        .encode()
        .unwrap(),
        ..todo
    };
    let db = crate::sqlite::Connection::open(&dir.path().join("laptop.db")).unwrap();
    db.execute(
        "UPDATE rocky_operations SET bytes=? WHERE sequence=1",
        [forged.encode().unwrap()],
    )
    .await
    .unwrap();
    drop(db);
    let laptop = open(&dir, fixtures::LAPTOP).await;
    assert!(matches!(laptop.checkpoint().await, Err(Error::Corrupt(_))));
    let status = laptop.retention_status().await.unwrap();
    assert_eq!(status.hold, Some(CheckpointHold::Unverifiable));
    assert!(status.checkpoint.is_none());
    assert!(laptop.prune().await.unwrap().0.is_empty());
    assert_eq!(count(&dir, fixtures::LAPTOP, "rocky_operations").await, 2);
    assert!(matches!(laptop.audit().await, Err(Error::Corrupt(_))));
    laptop.close().await.unwrap();
    // The hold survives a restart; automatic steps do not retry it.
    let laptop = open_at(dir.path(), fixtures::LAPTOP, RuntimeOptions::default()).await;
    create(&laptop, "later").await;
    assert_eq!(
        laptop.retention_status().await.unwrap().hold,
        Some(CheckpointHold::Unverifiable)
    );
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

/// The phone is offline across a prune. Slightly behind, it converges from
/// retained operations; behind the pruned prefix it is told so, imports the
/// checkpoint and converges with its own unsent edits.
#[tokio::test]
async fn an_offline_replica_converges_or_rebootstraps_with_its_local_edits() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let shared = create(&laptop, "shared").await.id;
    let doomed = create(&laptop, "doomed").await.id;
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    // Offline edits on the phone: to a row the laptop keeps, to one it
    // deletes meanwhile, and a row of its own.
    let own = create(&phone, "phone only").await.id;
    edit(&phone, shared, Action::SetCompleted(true)).await;
    let late = edit(&phone, doomed, Action::SetTitle("revived?".into())).await;
    edit(&laptop, doomed, Action::DeleteTodo).await;
    for index in 0..600 {
        create(&laptop, &format!("bulk-{index}")).await;
    }
    // Not acknowledged, not declared behind: the phone still converges.
    laptop.checkpoint().await.unwrap();
    assert_eq!(
        sequence(&laptop.prune().await.unwrap(), fixtures::LAPTOP),
        2
    );
    // By policy the laptop stops waiting for the phone.
    laptop.declare_behind(fixtures::PHONE).await.unwrap();
    let pruned_prefix = laptop.prune().await.unwrap();
    assert_eq!(sequence(&pruned_prefix, fixtures::LAPTOP), 603);
    assert_eq!(count(&dir, fixtures::LAPTOP, "rocky_operations").await, 0);
    let notice = match laptop
        .catch_up(Some(fixtures::PHONE), phone.sync_state().await.unwrap())
        .await
        .unwrap()
    {
        Catchup::BehindPruned(notice) => notice,
        Catchup::Operations(_) => panic!("pruned operations cannot be sent"),
    };
    assert_eq!(PrunedNotice::decode(&notice.encode()).unwrap(), notice);
    assert_eq!(notice.pruned_prefix, pruned_prefix);
    let checkpoint = laptop.retention_status().await.unwrap().checkpoint.unwrap();
    assert_eq!(notice.checkpoint, checkpoint.commitment);
    // The exact receipt set cannot tell a pruned peer from a missing one.
    assert!(
        laptop
            .missing_operations(Vec::new())
            .await
            .unwrap()
            .records
            .is_empty()
    );
    let report = rebootstrap(&laptop, &phone).await;
    assert_eq!(report.checkpoint, checkpoint.commitment);
    assert_eq!(
        report
            .reapplied
            .iter()
            .map(|id| id.sequence.get())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(report.rejected.is_empty() && report.pending.is_empty());
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    assert!(sync(&phone, &laptop, fixtures::LAPTOP).await);
    let (ours, theirs) = (laptop.query().await.unwrap(), phone.query().await.unwrap());
    assert_eq!(ours.rows, theirs.rows);
    assert_eq!(ours.frontier, theirs.frontier);
    assert_eq!(ours.rows.len(), 602);
    assert!(ours.rows.iter().any(|row| row.id == own));
    assert!(
        ours.rows
            .iter()
            .any(|row| row.id == shared && row.completed)
    );
    // The late edit reached both replicas and the row stayed deleted.
    assert!(ours.rows.iter().all(|row| row.id != doomed));
    assert_eq!(late.entity, doomed);
    // The phone caught up, so the laptop waits for it again.
    assert!(laptop.retention_status().await.unwrap().behind.is_empty());
    for replica in [&laptop, &phone] {
        assert!(replica.audit().await.unwrap().recomputed);
    }
    let (ours, theirs) = (
        laptop.checkpoint().await.unwrap().record,
        phone.checkpoint().await.unwrap().record,
    );
    assert_eq!((ours.state, ours.history), (theirs.state, theirs.history));
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
    let phone = open(&dir, fixtures::PHONE).await;
    assert!(phone.audit().await.unwrap().recomputed);
    phone.close().await.unwrap();
}

#[tokio::test]
async fn a_late_operation_from_before_the_pruned_prefix_cannot_resurrect_a_deleted_row() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let created = create(&laptop, "short lived").await;
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    let late = edit(&phone, created.id, Action::SetTitle("back again".into())).await;
    edit(&laptop, created.id, Action::DeleteTodo).await;
    laptop.checkpoint().await.unwrap();
    laptop.declare_behind(fixtures::PHONE).await.unwrap();
    assert_eq!(
        sequence(&laptop.prune().await.unwrap(), fixtures::LAPTOP),
        2
    );
    assert_eq!(count(&dir, fixtures::LAPTOP, "rocky_operations").await, 0);
    // The creation itself arrives again: a duplicate, not a new row.
    let ack = laptop.receive(created.encode().unwrap()).await.unwrap();
    assert!(ack.applied);
    // Its dependency is behind the pruned prefix; the tombstone still decides.
    assert!(
        !laptop
            .receive(late.encode().unwrap())
            .await
            .unwrap()
            .applied
    );
    laptop.process_pending().await.unwrap();
    let snapshot = laptop.query().await.unwrap();
    assert!(snapshot.rows.is_empty());
    assert_eq!(sequence(&snapshot.frontier, fixtures::PHONE), 1);
    assert!(laptop.audit().await.unwrap().recomputed);
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

#[tokio::test]
async fn retried_handles_from_before_the_pruned_prefix_do_not_execute_twice() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let handle = MutationHandle([7; 32]);
    let once = || {
        mutation(
            None,
            Action::CreateTodo {
                title: "once".into(),
                completed: false,
            },
        )
    };
    let first = laptop
        .mutate_with_handle(handle, once())
        .unwrap()
        .committed()
        .await
        .unwrap();
    laptop.checkpoint().await.unwrap();
    laptop.declare_behind(fixtures::PHONE).await.unwrap();
    laptop.prune().await.unwrap();
    assert_eq!(count(&dir, fixtures::LAPTOP, "rocky_operations").await, 0);
    let pruned = |error: Error, sequence: u64| {
        assert!(
            matches!(error, Error::Pruned { node, sequence: found }
                if node == fixtures::LAPTOP && found == sequence),
            "{error}"
        );
    };
    pruned(
        laptop
            .mutate_with_handle(handle, once())
            .unwrap()
            .committed()
            .await
            .unwrap_err(),
        first.id.sequence.get(),
    );
    pruned(laptop.lookup_commit(handle).await.unwrap_err(), 1);
    assert!(matches!(
        laptop
            .mutate_with_handle(
                handle,
                mutation(
                    None,
                    Action::CreateTodo {
                        title: "other input".into(),
                        completed: false
                    }
                )
            )
            .unwrap()
            .committed()
            .await,
        Err(Error::Conflict)
    ));
    let snapshot = laptop.query().await.unwrap();
    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(sequence(&snapshot.frontier, fixtures::LAPTOP), 1);
    laptop.close().await.unwrap();
    // The pruned history reopens and audits.
    let laptop = open(&dir, fixtures::LAPTOP).await;
    assert!(laptop.audit().await.unwrap().recomputed);
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

/// With `behind_after`, a delivery target that holds back the pruned_prefix that
/// long is declared behind; contact restarts its time.
#[tokio::test]
async fn a_target_holding_back_the_pruned_prefix_too_long_is_declared_behind() {
    let dir = TempDir::new().unwrap();
    let timed = |after| RuntimeOptions {
        retention: RetentionPolicy {
            behind_after: Some(after),
            ..manual().retention
        },
        ..RuntimeOptions::default()
    };
    let hour = std::time::Duration::from_secs(3600);
    let laptop = open_at(dir.path(), fixtures::LAPTOP, timed(hour)).await;
    let phone = open(&dir, fixtures::PHONE).await;
    create(&phone, "unsent").await;
    for index in 0..10 {
        create(&laptop, &format!("todo-{index}")).await;
    }
    laptop.checkpoint().await.unwrap();
    // Within the hour the phone still holds pruning back.
    assert!(laptop.prune().await.unwrap().0.is_empty());
    assert!(laptop.retention_status().await.unwrap().behind.is_empty());
    laptop.close().await.unwrap();
    let laptop = open_at(
        dir.path(),
        fixtures::LAPTOP,
        timed(std::time::Duration::ZERO),
    )
    .await;
    assert_eq!(
        sequence(&laptop.prune().await.unwrap(), fixtures::LAPTOP),
        10
    );
    assert_eq!(
        laptop.retention_status().await.unwrap().behind,
        [fixtures::PHONE]
    );
    assert_eq!(count(&dir, fixtures::LAPTOP, "rocky_operations").await, 0);
    assert!(!sync(&laptop, &phone, fixtures::PHONE).await);
    let report = rebootstrap(&laptop, &phone).await;
    assert_eq!(report.reapplied.len(), 1);
    // Contact clears the mark; the phone is waited for again.
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    assert!(sync(&phone, &laptop, fixtures::LAPTOP).await);
    assert!(laptop.retention_status().await.unwrap().behind.is_empty());
    assert_eq!(laptop.query().await.unwrap().rows.len(), 11);
    assert!(laptop.audit().await.unwrap().recomputed);
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

#[tokio::test]
async fn retained_bytes_trigger_checkpoints_and_then_push_back() {
    let dir = TempDir::new().unwrap();
    let bounded = |automatic| RuntimeOptions {
        retention: RetentionPolicy {
            checkpoint_operations: 0,
            window: 0,
            retained_bytes: 16 * 1024,
            handles: 4,
            automatic,
            ..RetentionPolicy::default()
        },
        ..RuntimeOptions::default()
    };
    // The phone never acknowledges: history cannot be released.
    let laptop = open_at(dir.path(), fixtures::LAPTOP, bounded(true)).await;
    let mut written = 0;
    let error = loop {
        match laptop
            .mutate(mutation(
                None,
                Action::CreateTodo {
                    title: format!("todo-{written}"),
                    completed: false,
                },
            ))
            .unwrap()
            .committed()
            .await
        {
            Ok(_) => written += 1,
            Err(error) => break error,
        }
        assert!(written < 1000, "the bound never pushed back");
    };
    assert!(matches!(error, Error::Retention { .. }), "{error}");
    assert!(error.is_retryable());
    assert_eq!(error.reason(), FailureReason::Quota);
    let status = laptop.retention_status().await.unwrap();
    // Half the bound started checkpoints; nothing was lost or pruned.
    assert!(status.checkpoint.is_some());
    assert!(status.pruned_prefix.0.is_empty());
    assert_eq!(status.retained_operations, written);
    assert_eq!(laptop.query().await.unwrap().rows.len(), written);
    // Declared behind, the phone no longer holds history back.
    laptop.declare_behind(fixtures::PHONE).await.unwrap();
    laptop.checkpoint().await.unwrap();
    laptop.prune().await.unwrap();
    for index in 0..written {
        create(&laptop, &format!("again-{index}")).await;
    }
    let status = laptop.retention_status().await.unwrap();
    assert!(status.retained_bytes <= status.policy.retained_bytes);
    assert_eq!(laptop.query().await.unwrap().rows.len(), 2 * written);
    laptop.checkpoint().await.unwrap();
    laptop.prune().await.unwrap();
    // Only the newest handles remain for retries.
    assert_eq!(
        count(&dir, fixtures::LAPTOP, "rocky_mutation_handles").await,
        4
    );
    assert!(laptop.audit().await.unwrap().recomputed);
    laptop.close().await.unwrap();
}

/// A failure injected into every step of checkpoint creation, pruning and
/// rebootstrap rolls that step back alone; the file reopens, passes the
/// audit and the step then completes.
#[tokio::test]
async fn a_failure_at_any_step_leaves_a_file_that_reopens_and_audits() {
    let dir = TempDir::new().unwrap();
    let mut laptop = open(&dir, fixtures::LAPTOP).await;
    let mut phone = open(&dir, fixtures::PHONE).await;
    let first = create(&laptop, "first").await.id;
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    let unsent = edit(&phone, first, Action::SetCompleted(true)).await;
    for index in 0..600 {
        create(&laptop, &format!("todo-{index}")).await;
    }
    laptop.declare_behind(fixtures::PHONE).await.unwrap();
    let mut failures = 0;
    for request in [|| Request::Checkpoint, || Request::Prune] {
        loop {
            fault(&laptop, worker::Fault::FailBeforeCommit).await;
            assert!(matches!(
                step(&laptop, request()).await,
                Err(Error::InjectedFailure)
            ));
            failures += 1;
            laptop.close().await.unwrap();
            laptop = open(&dir, fixtures::LAPTOP).await;
            let before = laptop.query().await.unwrap();
            assert_eq!(before.rows.len(), 601);
            laptop.flush().await.unwrap();
            if step(&laptop, request()).await.unwrap() == Step::Done {
                break;
            }
        }
        assert!(matches!(
            laptop.retention_request(Request::Audit).await,
            Ok(Response::Audit(_))
        ));
    }
    // Candidate, replay and verification, then pruned prefix and deletion.
    assert!(failures >= 5, "{failures}");
    assert_eq!(count(&dir, fixtures::LAPTOP, "rocky_operations").await, 0);
    assert!(laptop.audit().await.unwrap().recomputed);
    // Every import page, the last of which switches the projection.
    let mut cursor = None;
    let mut pages = 0;
    let report = loop {
        let page = laptop.checkpoint_page(cursor).await.unwrap();
        cursor = page.next;
        pages += 1;
        fault(&phone, worker::Fault::FailBeforeCommit).await;
        assert!(matches!(
            phone.import_checkpoint(page.clone()).await,
            Err(Error::InjectedFailure)
        ));
        phone.close().await.unwrap();
        phone = open(&dir, fixtures::PHONE).await;
        // Until the switch the phone keeps its own state.
        assert_eq!(phone.query().await.unwrap().rows.len(), 1);
        assert!(!phone.audit().await.unwrap().recomputed || pages == 1);
        if let Some(report) = phone.import_checkpoint(page).await.unwrap() {
            break report;
        }
    };
    assert!(pages >= 2 && cursor.is_none());
    assert_eq!(report.reapplied, [unsent.id]);
    assert!(sync(&phone, &laptop, fixtures::LAPTOP).await);
    assert_eq!(
        laptop.query().await.unwrap().rows,
        phone.query().await.unwrap().rows
    );
    for replica in [&laptop, &phone] {
        assert!(replica.audit().await.unwrap().recomputed);
    }
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

#[tokio::test]
async fn an_aborted_import_keeps_the_projection_and_a_superseded_export_restarts() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    for index in 0..600 {
        create(&laptop, &format!("todo-{index}")).await;
    }
    let local = create(&phone, "mine").await.id;
    laptop.checkpoint().await.unwrap();
    let first = laptop.checkpoint_page(None).await.unwrap();
    assert!(
        phone
            .import_checkpoint(first.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        phone.retention_status().await.unwrap().hold,
        Some(CheckpointHold::Rebootstrap)
    );
    // A gap is refused; a repeated page is not staged twice.
    let mut gap = first.clone();
    gap.record = None;
    gap.start.position += 100_000;
    assert!(matches!(
        phone.import_checkpoint(gap).await,
        Err(Error::Rejected(_))
    ));
    let mut repeated = first.clone();
    repeated.record = None;
    assert!(phone.import_checkpoint(repeated).await.unwrap().is_none());
    phone.abort_rebootstrap().await.unwrap();
    assert_eq!(
        phone.retention_status().await.unwrap().hold,
        Some(CheckpointHold::NoBase)
    );
    assert_eq!(phone.query().await.unwrap().rows[0].id, local);
    assert!(!phone.audit().await.unwrap().recomputed);
    // The laptop moved on: the old pass ends, a new one imports fully.
    create(&laptop, "newer").await;
    laptop.checkpoint().await.unwrap();
    assert!(matches!(
        laptop.checkpoint_page(first.next).await,
        Err(Error::Rejected(_))
    ));
    let mut tampered = laptop.checkpoint_page(None).await.unwrap();
    tampered.rows[0][2] = SqlValue::Text("not what was committed".into());
    assert!(
        phone
            .import_checkpoint(tampered.clone())
            .await
            .unwrap()
            .is_none()
    );
    let rest = laptop.checkpoint_page(tampered.next).await.unwrap();
    assert!(rest.next.is_none());
    // The rows do not match the record: nothing is switched.
    assert!(matches!(
        phone.import_checkpoint(rest).await,
        Err(Error::Corrupt(_))
    ));
    assert_eq!(phone.query().await.unwrap().rows.len(), 1);
    let report = rebootstrap(&laptop, &phone).await;
    assert_eq!(report.reapplied, [local]);
    assert_eq!(phone.query().await.unwrap().rows.len(), 602);
    assert!(phone.audit().await.unwrap().recomputed);
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

fn child(case: &str, dir: &TempDir) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "retention_tests::crash_child",
            "--exact",
            "--test-threads=1",
        ])
        .env(CASE, case)
        .env(DIR, dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap()
}
async fn seed(dir: &Path) -> (Replica<TodoApp>, Replica<TodoApp>) {
    let laptop = open_at(dir, fixtures::LAPTOP, manual()).await;
    let phone = open_at(dir, fixtures::PHONE, manual()).await;
    for index in 0..600 {
        create(&laptop, &format!("todo-{index}")).await;
    }
    create(&phone, "unsent").await;
    laptop.declare_behind(fixtures::PHONE).await.unwrap();
    (laptop, phone)
}
/// Does nothing in an ordinary run; the parent selects the step to die in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_child() {
    let (Ok(case), Ok(dir)) = (std::env::var(CASE), std::env::var(DIR)) else {
        return;
    };
    let dir = Path::new(&dir);
    let (laptop, phone) = seed(dir).await;
    let die = async |replica: &Replica<TodoApp>| {
        fault(replica, worker::Fault::AbortAfterWrites(1)).await;
    };
    let steps = |request: fn() -> Request, count: usize| {
        let laptop = laptop.clone();
        async move {
            for _ in 0..count {
                step(&laptop, request()).await.unwrap();
            }
        }
    };
    match case.as_str() {
        "candidate" => {
            die(&laptop).await;
            let _ = step(&laptop, Request::Checkpoint).await;
        }
        "replay" => {
            steps(|| Request::Checkpoint, 2).await;
            die(&laptop).await;
            let _ = step(&laptop, Request::Checkpoint).await;
        }
        "prune" => {
            laptop.checkpoint().await.unwrap();
            laptop.flush().await.unwrap();
            die(&laptop).await;
            let _ = step(&laptop, Request::Prune).await;
        }
        "stage" | "switch" => {
            laptop.checkpoint().await.unwrap();
            let first = laptop.checkpoint_page(None).await.unwrap();
            if case == "stage" {
                die(&phone).await;
                let _ = phone.import_checkpoint(first).await;
            } else {
                let last = laptop.checkpoint_page(first.next).await.unwrap();
                assert!(last.next.is_none());
                phone.import_checkpoint(first).await.unwrap();
                phone.flush().await.unwrap();
                die(&phone).await;
                let _ = phone.import_checkpoint(last).await;
            }
        }
        other => panic!("unknown crash case {other}"),
    }
    panic!("the injected fault did not end the process");
}

/// Process death inside each step: the uncommitted step is gone, the file
/// opens, passes the audit, and the work completes afterwards.
#[tokio::test]
async fn death_inside_any_step_leaves_a_file_that_opens_and_audits() {
    for case in ["candidate", "replay", "prune", "stage", "switch"] {
        let dir = TempDir::new().unwrap();
        let status = child(case, &dir).wait().unwrap();
        assert_eq!(status.signal(), Some(6), "{case} must abort: {status:?}");
        let laptop = open(&dir, fixtures::LAPTOP).await;
        let phone = open(&dir, fixtures::PHONE).await;
        assert_eq!(laptop.query().await.unwrap().rows.len(), 600, "{case}");
        assert_eq!(phone.query().await.unwrap().rows.len(), 1, "{case}");
        for replica in [&laptop, &phone] {
            assert!(matches!(
                replica.retention_request(Request::Audit).await,
                Ok(Response::Audit(_))
            ));
        }
        assert!(laptop.audit().await.unwrap().recomputed, "{case}");
        laptop.prune().await.unwrap();
        assert_eq!(count(&dir, fixtures::LAPTOP, "rocky_operations").await, 0);
        let report = rebootstrap(&laptop, &phone).await;
        assert_eq!(report.reapplied.len(), 1, "{case}");
        assert!(sync(&phone, &laptop, fixtures::LAPTOP).await);
        assert_eq!(laptop.query().await.unwrap().rows.len(), 601, "{case}");
        for replica in [&laptop, &phone] {
            assert!(replica.audit().await.unwrap().recomputed, "{case}");
        }
        laptop.close().await.unwrap();
        phone.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_definite_local_drop_after_pruning_keeps_the_trusted_author_log_running() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let created = create(&laptop, "pruned").await;
    edit(&laptop, created.id, Action::DeleteTodo).await;
    laptop.checkpoint().await.unwrap();
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    laptop.prune().await.unwrap();
    assert_eq!(count(&dir, fixtures::LAPTOP, "rocky_operations").await, 0);
    let dropped = edit(&laptop, created.id, Action::SetTitle("gone".into())).await;
    assert_eq!(dropped.id.sequence.get(), 3);
    let continued = create(&laptop, "continued").await;
    assert_eq!(continued.id.sequence.get(), 4);
    assert!(sync(&laptop, &phone, fixtures::PHONE).await);
    for replica in [&laptop, &phone] {
        let snapshot = replica.query().await.unwrap();
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(sequence(&snapshot.frontier, fixtures::LAPTOP), 4);
        assert!(replica.audit().await.unwrap().recomputed);
    }
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}
