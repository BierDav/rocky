use super::*;
use rocky_core::CompiledSchema;
use rocky_core::{SqlValue, StoredApplication, fixtures};

use tempfile::TempDir;
pub(super) async fn rows(
    conn: &crate::sqlite::Connection,
    sql: &str,
) -> Result<Vec<Vec<SqlValue>>, Error> {
    storage::collect_rows(conn.query(sql, ()).await?).await
}
use todo_app::{Action, StaticPolicy, TodoApp, TodoEntry};

async fn open(dir: &TempDir) -> Replica<TodoApp> {
    Replica::open_with_config(
        dir.path().join("laptop.db"),
        TodoApp,
        StaticPolicy::default(),
        crate::tests::identity(),
        crate::tests::runtime_config(),
    )
    .await
    .unwrap()
}
pub(super) fn mutation(entity: Option<OperationId>, action: Action) -> Mutation {
    Mutation {
        entity,
        payload: action.encode().unwrap(),
    }
}
pub(super) async fn create(replica: &Replica<TodoApp>, title: &str) -> Envelope {
    replica
        .mutate(mutation(
            None,
            Action::CreateTodo {
                title: title.into(),
                completed: false,
            },
        ))
        .unwrap()
        .committed()
        .await
        .unwrap()
}
pub(super) async fn fault<A: StoredApplication>(replica: &Replica<A>, fault: worker::Fault) {
    let (reply, result) = oneshot::channel();
    enqueue(&replica.inner.commands, Command::Fault(fault, reply)).unwrap();
    result.await.unwrap().unwrap();
}
pub(super) async fn disk(dir: &TempDir, sql: &str) -> Vec<Vec<SqlValue>> {
    let db = crate::sqlite::Connection::open(&dir.path().join("laptop.db")).unwrap();
    storage::collect_rows(db.query(sql, ()).await.unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn actions_reopen_counters_outbox_and_rebuild() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let deleted = create(&replica, "delete me").await.id;
    let kept = create(&replica, "old title").await.id;
    for (entity, action) in [
        (kept, Action::SetTitle("renamed 🪨".into())),
        (kept, Action::SetCompleted(true)),
        (deleted, Action::DeleteTodo),
    ] {
        replica
            .mutate(mutation(Some(entity), action))
            .unwrap()
            .committed()
            .await
            .unwrap();
    }
    let snapshot = replica.query().await.unwrap();
    assert_eq!(snapshot.revision.get(), 5);
    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(snapshot.rows[0].title, "renamed 🪨");
    assert!(snapshot.rows[0].completed);
    replica.close().await.unwrap();
    let bytes = disk(&dir, "SELECT bytes FROM rocky_operations ORDER BY sequence").await;
    let operations: Vec<_> = bytes
        .into_iter()
        .map(|row| match &row[0] {
            SqlValue::Blob(bytes) => Envelope::decode(bytes).unwrap(),
            _ => panic!("bytes"),
        })
        .collect();
    let replay = todo_app::replay(operations, &StaticPolicy::default()).unwrap();
    assert_eq!(replay.state.entry(deleted), Some(&TodoEntry::Tombstone));
    assert_eq!(replay.state.visible_rows(), snapshot.rows);
    assert_eq!(
        disk(&dir, TodoApp.projection_query()).await,
        TodoApp.projection_rows(&replay.state)
    );
    assert_eq!(
        disk(&dir, "SELECT count(*) FROM rocky_outbox").await,
        vec![vec![SqlValue::Integer(5)]]
    );
    // Fresh on-disk projection rebuilt exclusively from retained operations.
    let rebuilt = crate::sqlite::Connection::open(&dir.path().join("rebuilt.db")).unwrap();
    let conn = rebuilt;
    for sql in TodoApp.schema() {
        conn.execute(*sql, ()).await.unwrap();
    }
    for op in replay
        .applied
        .frontier()
        .0
        .iter()
        .flat_map(|(node, sequence)| {
            (1..=sequence.get()).map(move |n| OperationId {
                node: *node,
                sequence: rocky_core::AuthorSequence::new(n).unwrap(),
            })
        })
    {
        let op = Envelope::decode(replay.applied.canonical_bytes(op).unwrap()).unwrap();
        for statement in TodoApp.project(&replay.state, &op) {
            conn.execute(statement.sql, storage::bindings(statement.values))
                .await
                .unwrap();
        }
    }
    assert_eq!(
        rows(&conn, TodoApp.projection_query()).await.unwrap(),
        TodoApp.projection_rows(&replay.state)
    );
    let reopened = open(&dir).await;
    assert_eq!(reopened.query().await.unwrap(), snapshot);
    let next = create(&reopened, "next").await;
    assert_eq!(next.id.sequence.get(), 6);
    assert_eq!(next.lamport.get(), 6);
    assert_eq!(next.id.node, fixtures::LAPTOP);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn rollback_is_whole_and_never_notifies() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let mut observer = replica.observe().unwrap();
    assert_eq!(observer.next().await.unwrap().revision.get(), 0);
    fault(&replica, worker::Fault::FailBeforeCommit).await;
    assert!(matches!(
        replica
            .mutate(mutation(
                None,
                Action::CreateTodo {
                    title: "failed".into(),
                    completed: false
                }
            ))
            .unwrap()
            .committed()
            .await,
        Err(Error::InjectedFailure)
    ));
    assert_eq!(replica.query().await.unwrap().revision.get(), 0);
    assert!(!observer.revisions.has_changed().unwrap());
    replica.close().await.unwrap();
    for table in ["rocky_operations", "rocky_outbox", "todos"] {
        assert_eq!(
            disk(&dir, &format!("SELECT count(*) FROM {table}")).await,
            vec![vec![SqlValue::Integer(0)]]
        );
    }
    assert_eq!(
        disk(&dir, "SELECT sequence,lamport,revision FROM rocky_replica").await,
        vec![vec![SqlValue::Integer(0); 3]]
    );
    let reopened = open(&dir).await;
    assert_eq!(create(&reopened, "success").await.id.sequence.get(), 1);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn retries_reuse_bytes_and_are_bounded() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    fault(&replica, worker::Fault::RetryBeforeCommit(2)).await;
    let op = create(&replica, "once").await;
    assert_eq!(op.id.sequence.get(), 1);
    fault(
        &replica,
        worker::Fault::RetryBeforeCommit(storage::TRANSACTION_ATTEMPTS),
    )
    .await;
    assert!(matches!(
        replica
            .mutate(mutation(Some(op.id), Action::SetTitle("never".into())))
            .unwrap()
            .committed()
            .await,
        Err(Error::RetryExhausted)
    ));
    assert_eq!(replica.query().await.unwrap().rows[0].title, "once");
    replica.close().await.unwrap();
    assert_eq!(
        disk(&dir, "SELECT bytes FROM rocky_operations").await,
        vec![vec![SqlValue::Blob(op.encode().unwrap())]]
    );
    assert_eq!(
        disk(&dir, "SELECT sequence,lamport,revision FROM rocky_replica").await,
        vec![vec![SqlValue::Integer(1); 3]]
    );
}

#[tokio::test]
async fn registration_race_slow_observer_and_close() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let mut observer = replica.observe().unwrap();
    create(&replica, "between registration and initial read").await;
    assert_eq!(observer.next().await.unwrap().revision.get(), 1);
    for i in 0..20 {
        create(&replica, &format!("row {i}")).await;
    }
    let latest = observer.next().await.unwrap();
    assert_eq!(latest.revision.get(), 21);
    assert_eq!(latest.rows.len(), 21);
    assert_eq!(Arc::strong_count(&replica.inner), 1);
    drop(observer);
    assert_eq!(Arc::strong_count(&replica.inner), 1);
    let mut observer = replica.observe().unwrap();
    observer.next().await.unwrap();
    replica.close().await.unwrap();
    assert!(matches!(observer.next().await, Err(Error::Closed)));
    assert!(matches!(replica.query().await, Err(Error::Closed)));
    // No observer retains the runtime/connection.
    drop(replica);
    let reopened = open(&dir).await;
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_waiter_and_backpressure_do_not_cancel_admitted_work() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let (entered, entered_rx) = oneshot::channel();
    let (resume, resume_rx) = oneshot::channel();
    fault(&replica, worker::Fault::Pause(entered, resume_rx)).await;
    let receipt = replica
        .mutate(mutation(
            None,
            Action::CreateTodo {
                title: "survives cancellation".into(),
                completed: false,
            },
        ))
        .unwrap();
    entered_rx.await.unwrap();
    drop(receipt);
    let mut admitted = Vec::new();
    for _ in 0..32 {
        admitted.push(
            replica
                .mutate(mutation(
                    None,
                    Action::CreateTodo {
                        title: "queued".into(),
                        completed: false,
                    },
                ))
                .unwrap(),
        );
    }
    assert!(matches!(
        replica.mutate(mutation(
            None,
            Action::CreateTodo {
                title: "overflow".into(),
                completed: false
            }
        )),
        Err(Error::Backpressure)
    ));
    resume.send(()).unwrap();
    for receipt in admitted {
        receipt.committed().await.unwrap();
    }
    replica.close().await.unwrap();
    let reopened = open(&dir).await;
    assert_eq!(reopened.query().await.unwrap().rows.len(), 33);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn rejection_incompatible_identity_and_exclusive_open_preserve_data() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let op = create(&replica, "preserve").await;
    assert!(
        Replica::open_with_config(
            dir.path().join("laptop.db"),
            TodoApp,
            StaticPolicy::default(),
            crate::tests::identity(),
            crate::tests::runtime_config(),
        )
        .await
        .is_err()
    );
    replica
        .mutate(mutation(Some(op.id), Action::DeleteTodo))
        .unwrap()
        .committed()
        .await
        .unwrap();
    assert!(
        replica
            .mutate(mutation(Some(op.id), Action::SetTitle("revive".into())))
            .unwrap()
            .committed()
            .await
            .is_err()
    );
    replica.close().await.unwrap();
    let before = disk(&dir, "SELECT bytes FROM rocky_operations ORDER BY sequence").await;
    let identity = Identity {
        node: fixtures::PHONE,
        ..crate::tests::identity()
    };
    assert!(matches!(
        Replica::open_with_config(
            dir.path().join("laptop.db"),
            TodoApp,
            StaticPolicy::default(),
            identity,
            crate::tests::runtime_config(),
        )
        .await,
        Err(Error::Incompatible(_))
    ));
    let mut policy = StaticPolicy::default();
    policy.actions[0] = false;
    let denied = Replica::open_with_config(
        dir.path().join("laptop.db"),
        TodoApp,
        policy,
        crate::tests::identity(),
        crate::tests::runtime_config(),
    )
    .await
    .unwrap();
    assert!(
        denied
            .mutate(mutation(
                None,
                Action::CreateTodo {
                    title: "denied".into(),
                    completed: false
                }
            ))
            .unwrap()
            .committed()
            .await
            .is_err()
    );
    denied.close().await.unwrap();
    assert_eq!(
        disk(&dir, "SELECT bytes FROM rocky_operations ORDER BY sequence").await,
        before
    );
    let db = crate::sqlite::Connection::open(&dir.path().join("laptop.db")).unwrap();
    db.execute(
        "UPDATE rocky_replica SET identity=?",
        [crate::sqlite::Value::Blob(vec![0; 96])],
    )
    .await
    .unwrap();
    assert!(matches!(
        Replica::open_with_config(
            dir.path().join("laptop.db"),
            TodoApp,
            StaticPolicy::default(),
            crate::tests::identity(),
            crate::tests::runtime_config(),
        )
        .await,
        Err(Error::Incompatible(_))
    ));
    assert_eq!(
        disk(&dir, "SELECT bytes FROM rocky_operations ORDER BY sequence").await,
        before
    );
}

#[tokio::test]
async fn cancelled_observer_query_reconciles_latest_revision() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let mut observer = replica.observe().unwrap();
    observer.next().await.unwrap();
    create(&replica, "first").await;
    let (entered, entered_rx) = oneshot::channel();
    let (resume, resume_rx) = oneshot::channel();
    fault(&replica, worker::Fault::Pause(entered, resume_rx)).await;
    let receipt = replica
        .mutate(mutation(
            None,
            Action::CreateTodo {
                title: "second".into(),
                completed: false,
            },
        ))
        .unwrap();
    entered_rx.await.unwrap();
    let mut waiter = Box::pin(observer.next());
    assert!(matches!(
        waiter
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    drop(waiter);
    resume.send(()).unwrap();
    receipt.committed().await.unwrap();
    assert_eq!(observer.next().await.unwrap().revision.get(), 2);
    replica.close().await.unwrap();
}

#[tokio::test]
async fn dropping_last_runtime_drains_work_and_stops_observer() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let mut observer = replica.observe().unwrap();
    observer.next().await.unwrap();
    drop(
        replica
            .mutate(mutation(
                None,
                Action::CreateTodo {
                    title: "detached".into(),
                    completed: false,
                },
            ))
            .unwrap(),
    );
    drop(replica);
    while observer.revisions.changed().await.is_ok() {}
    assert!(matches!(observer.next().await, Err(Error::Closed)));
    let reopened = open(&dir).await;
    assert_eq!(reopened.query().await.unwrap().rows[0].title, "detached");
    reopened.close().await.unwrap();
}

/// Another connection holding the write lock makes the file writer retry
/// `BEGIN IMMEDIATE` with backoff, then report `RetryExhausted` after the
/// 318 ms bound. Nothing commits while the lock is held.
#[tokio::test]
async fn a_file_held_by_another_connection_is_retried_then_exhausted() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    // Answered after the writer's opening flush.
    replica.query().await.unwrap();
    let other = crate::sqlite::Connection::open(&dir.path().join("laptop.db")).unwrap();
    let todo = |title: &str| {
        mutation(
            None,
            Action::CreateTodo {
                title: title.into(),
                completed: false,
            },
        )
    };
    other.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    let receipt = replica.mutate(todo("after release")).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    other.execute("ROLLBACK", ()).await.unwrap();
    assert_eq!(receipt.committed().await.unwrap().id.sequence.get(), 1);
    other.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    let started = std::time::Instant::now();
    let error = replica
        .mutate(todo("never"))
        .unwrap()
        .committed()
        .await
        .unwrap_err();
    let waited = started.elapsed();
    assert!(matches!(error, Error::RetryExhausted), "{error}");
    assert!(error.is_retryable());
    assert!(
        waited >= std::time::Duration::from_millis(318),
        "{waited:?}"
    );
    other.execute("ROLLBACK", ()).await.unwrap();
    let snapshot = replica.query().await.unwrap();
    assert_eq!((snapshot.rows.len(), snapshot.revision.get()), (1, 1));
    replica.close().await.unwrap();
}

pub(super) fn runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        authors: vec![
            (fixtures::LAPTOP, fixtures::OWNER),
            (fixtures::PHONE, fixtures::OWNER),
        ],
        delivery_targets: vec![fixtures::LAPTOP, fixtures::PHONE],
        full_nodes: Vec::new(),
    }
}

pub(super) fn identity() -> Identity {
    Identity {
        scope: fixtures::SCOPE,
        node: fixtures::LAPTOP,
        caller: fixtures::OWNER,
    }
}
