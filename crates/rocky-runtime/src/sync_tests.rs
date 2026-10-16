use super::*;
use crate::tests::{create, disk, fault, mutation};
use rocky_core::CompiledSchema;
use rocky_core::fixtures;
use rocky_core::{ProtocolError, SqlValue};
use tempfile::TempDir;
use todo_app::{Action, StaticPolicy, TodoApp};

async fn open(dir: &TempDir, node: NodeId, policy: StaticPolicy) -> Replica<TodoApp> {
    Replica::open_with_config(
        dir.path().join(if node == fixtures::LAPTOP {
            "laptop.db"
        } else {
            "phone.db"
        }),
        TodoApp,
        policy,
        Identity {
            node,
            ..crate::tests::identity()
        },
        crate::tests::runtime_config(),
    )
    .await
    .unwrap()
}
async fn edit(replica: &Replica<TodoApp>, entity: OperationId, action: Action) -> Envelope {
    replica
        .mutate(mutation(Some(entity), action))
        .unwrap()
        .committed()
        .await
        .unwrap()
}
async fn transfer(from: &Replica<TodoApp>, to: &Replica<TodoApp>, peer: NodeId) {
    loop {
        let page = match from
            .catch_up(None, to.sync_state().await.unwrap())
            .await
            .unwrap()
        {
            Catchup::Operations(page) => page,
            Catchup::BehindPruned(_) => panic!("nothing is pruned here"),
        };
        if page.records.is_empty() {
            break;
        }
        for bytes in page.records {
            let ack = to.receive(bytes).await.unwrap();
            from.acknowledge(peer, ack).await.unwrap();
        }
        to.process_pending().await.unwrap();
    }
}

#[tokio::test]
async fn pending_survives_reopen_clock_observers_and_no_echo() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    let first = create(&laptop, "initial").await;
    let setter = edit(&laptop, first.id, Action::SetTitle("remote".into())).await;
    let mut observer = phone.observe().unwrap();
    observer.next().await.unwrap();
    let ack = phone.receive(setter.encode().unwrap()).await.unwrap();
    assert!(!ack.applied);
    phone.process_pending().await.unwrap();
    assert!(phone.query().await.unwrap().rows.is_empty());
    assert!(!observer.revisions.has_changed().unwrap());
    assert_eq!(phone.sync_state().await.unwrap().received, vec![setter.id]);
    assert!(phone.sync_state().await.unwrap().applied.0.is_empty());
    phone.close().await.unwrap();
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    let local = create(&phone, "own").await;
    assert_eq!(local.id.sequence.get(), 1);
    assert_eq!(local.lamport.get(), setter.lamport.get() + 1);
    let mut slow = phone.observe().unwrap();
    slow.next().await.unwrap();
    phone.receive(first.encode().unwrap()).await.unwrap();
    phone.process_pending().await.unwrap();
    assert_eq!(slow.next().await.unwrap().rows.len(), 2);
    let revision = phone.query().await.unwrap().revision;
    assert!(
        phone
            .receive(setter.encode().unwrap())
            .await
            .unwrap()
            .applied
    );
    phone.process_pending().await.unwrap();
    assert_eq!(phone.query().await.unwrap().revision, revision);
    let state = phone.sync_state().await.unwrap();
    // Everything held is contiguous now: the frontier covers it.
    assert_eq!(state.held, state.applied);
    assert!(state.received.is_empty());
    assert_eq!(
        state.applied.0,
        vec![
            (fixtures::LAPTOP, setter.id.sequence),
            (fixtures::PHONE, local.id.sequence)
        ]
    );
    assert_eq!(
        phone.peer_progress(fixtures::LAPTOP).await.unwrap().pending,
        1
    );
    phone.close().await.unwrap();
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    assert_eq!(phone.sync_state().await.unwrap(), state);
    phone.close().await.unwrap();
    laptop.close().await.unwrap();
}

#[tokio::test]
async fn durable_receipt_before_apply_and_atomic_apply_rollback() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    let op = create(&laptop, "receipt only").await;
    fault(&phone, worker::Fault::FailBeforeCommit).await;
    assert!(matches!(
        phone.receive(op.encode().unwrap()).await,
        Err(Error::InjectedFailure)
    ));
    assert!(phone.sync_state().await.unwrap().received.is_empty());
    phone.receive(op.encode().unwrap()).await.unwrap();
    assert!(phone.query().await.unwrap().rows.is_empty());
    phone.close().await.unwrap();
    // Startup applies durably received ready operations, without a transport queue.
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    assert_eq!(phone.query().await.unwrap().rows[0].title, "receipt only");
    let second = edit(&laptop, op.id, Action::SetTitle("batch final".into())).await;
    let third = edit(&laptop, op.id, Action::SetCompleted(true)).await;
    phone.receive(third.encode().unwrap()).await.unwrap();
    phone.receive(second.encode().unwrap()).await.unwrap();
    let before = phone.query().await.unwrap();
    let mut observer = phone.observe().unwrap();
    observer.next().await.unwrap();
    fault(&phone, worker::Fault::FailBeforeCommit).await;
    assert!(matches!(
        phone.process_pending().await,
        Err(Error::InjectedFailure)
    ));
    assert_eq!(phone.query().await.unwrap(), before);
    assert!(!observer.revisions.has_changed().unwrap());
    assert_eq!(
        phone.sync_state().await.unwrap().applied.0,
        vec![(fixtures::LAPTOP, op.id.sequence)]
    );
    fault(&phone, worker::Fault::RetryBeforeCommit(2)).await;
    phone.process_pending().await.unwrap();
    let after = observer.next().await.unwrap();
    assert_eq!(after.revision.get(), before.revision.get() + 1);
    assert_eq!(after.rows[0].title, "batch final");
    assert!(after.rows[0].completed);
    phone.close().await.unwrap();
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    assert_eq!(phone.query().await.unwrap(), after);
    phone.close().await.unwrap();
    laptop.close().await.unwrap();
}

#[tokio::test]
async fn concurrent_edits_delete_race_and_full_merge_replay_converge() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    let op = create(&laptop, "original").await;
    transfer(&laptop, &phone, fixtures::PHONE).await;
    let title = edit(&laptop, op.id, Action::SetTitle("laptop title".into())).await;
    let completed = edit(&phone, op.id, Action::SetCompleted(true)).await;
    phone.receive(title.encode().unwrap()).await.unwrap();
    laptop.receive(completed.encode().unwrap()).await.unwrap();
    phone.process_pending().await.unwrap();
    laptop.process_pending().await.unwrap();
    assert_eq!(
        laptop.query().await.unwrap().rows,
        phone.query().await.unwrap().rows
    );
    assert_eq!(phone.query().await.unwrap().rows[0].title, "laptop title");
    assert!(phone.query().await.unwrap().rows[0].completed);
    let left = edit(&laptop, op.id, Action::SetTitle("left".into())).await;
    let right = edit(&phone, op.id, Action::SetTitle("right".into())).await;
    assert_eq!(left.lamport, right.lamport);
    transfer(&laptop, &phone, fixtures::PHONE).await;
    transfer(&phone, &laptop, fixtures::LAPTOP).await;
    assert_eq!(laptop.query().await.unwrap().rows[0].title, "right");
    edit(&phone, op.id, Action::SetTitle("cannot revive".into())).await;
    edit(&laptop, op.id, Action::DeleteTodo).await;
    transfer(&phone, &laptop, fixtures::LAPTOP).await;
    transfer(&laptop, &phone, fixtures::PHONE).await;
    assert!(phone.query().await.unwrap().rows.is_empty());
    assert_eq!(
        laptop.sync_state().await.unwrap(),
        phone.sync_state().await.unwrap()
    );
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    let bytes = disk(
        &dir,
        "SELECT bytes FROM rocky_operations ORDER BY node,sequence",
    )
    .await;
    let operations: Vec<_> = bytes
        .iter()
        .map(|r| match &r[0] {
            SqlValue::Blob(b) => Envelope::decode(b).unwrap(),
            _ => panic!("bytes"),
        })
        .collect();
    let replay = todo_app::replay(operations, &StaticPolicy::default()).unwrap();
    assert_eq!(
        replay.state.entry(op.id),
        Some(&todo_app::TodoEntry::Tombstone)
    );
    assert_eq!(
        disk(&dir, TodoApp.projection_query()).await,
        TodoApp.projection_rows(&replay.state)
    );
    assert_eq!(
        laptop.sync_state().await.unwrap().applied,
        replay.applied.frontier()
    );
    // Phone's retained internal stamps/tombstones are also checked on reopen.
    assert_eq!(
        laptop.query().await.unwrap().rows,
        phone.query().await.unwrap().rows
    );
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}

#[tokio::test]
async fn acknowledgments_persist_are_monotonic_and_history_survives_outbox_clear() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    let first = create(&laptop, "first").await;
    let second = create(&laptop, "second").await;
    laptop
        .acknowledge(
            fixtures::PHONE,
            Acknowledgment {
                id: second.id,
                applied: true,
            },
        )
        .await
        .unwrap();
    let progress = laptop.peer_progress(fixtures::PHONE).await.unwrap();
    assert_eq!(progress.pending, 1);
    assert!(progress.frontier.0.is_empty());
    laptop.close().await.unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    assert_eq!(
        laptop.peer_progress(fixtures::PHONE).await.unwrap(),
        progress
    );
    laptop
        .acknowledge(
            fixtures::PHONE,
            Acknowledgment {
                id: first.id,
                applied: true,
            },
        )
        .await
        .unwrap();
    laptop
        .acknowledge(
            fixtures::PHONE,
            Acknowledgment {
                id: second.id,
                applied: false,
            },
        )
        .await
        .unwrap();
    let progress = laptop.peer_progress(fixtures::PHONE).await.unwrap();
    assert_eq!(progress.pending, 0);
    assert_eq!(progress.applied, 2);
    assert_eq!(
        progress.frontier.0,
        vec![(fixtures::LAPTOP, second.id.sequence)]
    );
    let page = laptop.missing_operations(vec![]).await.unwrap();
    assert_eq!(
        page.records,
        vec![first.encode().unwrap(), second.encode().unwrap()]
    );
    laptop.close().await.unwrap();
    assert_eq!(
        disk(&dir, "SELECT count(*) FROM rocky_outbox").await,
        vec![vec![SqlValue::Integer(0)]]
    );
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    assert_eq!(
        laptop.peer_progress(fixtures::PHONE).await.unwrap(),
        progress
    );
    laptop.close().await.unwrap();
}

#[tokio::test]
async fn rejected_frames_and_late_invalid_clock_never_advance_frontier() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    let first = create(&laptop, "allowed").await;
    let second = edit(&laptop, first.id, Action::SetTitle("successor".into())).await;
    let third = create(&laptop, "depends on rejected successor").await;
    phone.receive(third.encode().unwrap()).await.unwrap();
    let mut bad = second.clone();
    bad.lamport = first.lamport;
    phone.receive(bad.encode().unwrap()).await.unwrap();
    phone.receive(first.encode().unwrap()).await.unwrap();
    assert!(matches!(
        phone.process_pending().await,
        Err(Error::Rejected(_))
    ));
    assert_eq!(
        phone.sync_state().await.unwrap().applied.0,
        vec![(fixtures::LAPTOP, first.id.sequence)]
    );
    let state = phone.sync_state().await.unwrap();
    assert!(state.holds(first.id) && state.received.is_empty());
    assert!(phone.receive(third.encode().unwrap()).await.is_err());
    assert!(phone.receive(bad.encode().unwrap()).await.is_err());
    assert!(matches!(
        phone.receive(second.encode().unwrap()).await,
        Err(Error::Protocol(ProtocolError::IdentityConflict(_)))
    ));
    let before = phone.query().await.unwrap();
    for change in 0..7 {
        let mut invalid = first.clone();
        let bytes = match change {
            0 => vec![0; 70_000],
            1 => {
                invalid.scope.domain.0 = [0; 16];
                invalid.encode().unwrap()
            }
            2 => {
                invalid.caller.0 = [0; 16];
                invalid.encode().unwrap()
            }
            3 => {
                invalid.id.node.0 = [0; 16];
                invalid.entity = invalid.id;
                invalid.encode().unwrap()
            }
            4 => {
                let mut b = invalid.encode().unwrap();
                b[64..68].copy_from_slice(&2u32.to_le_bytes());
                b
            }
            5 => {
                let mut b = invalid.encode().unwrap();
                b.push(0);
                b
            }
            _ => {
                invalid.payload = Action::CreateTodo {
                    title: "conflict".into(),
                    completed: false,
                }
                .encode()
                .unwrap();
                invalid.encode().unwrap()
            }
        };
        assert!(phone.receive(bytes).await.is_err());
        assert_eq!(phone.query().await.unwrap(), before);
    }
    phone.close().await.unwrap();
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    assert_eq!(phone.query().await.unwrap(), before);
    phone.close().await.unwrap();
    let mut policy = StaticPolicy::default();
    policy.actions[0] = false;
    let denied_dir = TempDir::new().unwrap();
    let denied = open(&denied_dir, fixtures::PHONE, policy).await;
    assert!(matches!(
        denied.receive(first.encode().unwrap()).await,
        Err(Error::Application(_))
    ));
    assert!(denied.sync_state().await.unwrap().received.is_empty());
    assert!(denied.query().await.unwrap().rows.is_empty());
    denied.close().await.unwrap();
    laptop.close().await.unwrap();
}

#[tokio::test]
async fn cross_author_dependency_and_successor_survive_pending_restart() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    let phone = open(&dir, fixtures::PHONE, StaticPolicy::default()).await;
    let first = create(&laptop, "cross author").await;
    transfer(&laptop, &phone, fixtures::PHONE).await;
    let setter = edit(&phone, first.id, Action::SetCompleted(true)).await;
    let successor = edit(
        &phone,
        first.id,
        Action::SetTitle("after completion".into()),
    )
    .await;
    laptop.receive(successor.encode().unwrap()).await.unwrap();
    laptop.close().await.unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, StaticPolicy::default()).await;
    assert_eq!(laptop.query().await.unwrap().rows[0].title, "cross author");
    assert!(!laptop.query().await.unwrap().rows[0].completed);
    let state = laptop.sync_state().await.unwrap();
    assert!(state.holds(successor.id) && !state.holds(setter.id));
    laptop.receive(setter.encode().unwrap()).await.unwrap();
    laptop.process_pending().await.unwrap();
    let snapshot = laptop.query().await.unwrap();
    assert_eq!(snapshot.rows[0].title, "after completion");
    assert!(snapshot.rows[0].completed);
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}
