use super::*;
use crate::tests::{create, fault, mutation, runtime_config};
use rocky_coordinator::MemoryCoordinator;
use rocky_core::*;
use tempfile::TempDir;
use todo_app::{Action, StaticPolicy, TodoApp};
#[path = "../examples/support/ledger_app.rs"]
mod ledger_app;
use ledger_app::{LedgerApp, payload};

fn coordinator() -> MemoryCoordinator {
    coordinator_with(rocky_coordinator::Limits::default())
}
fn coordinator_with(limits: rocky_coordinator::Limits) -> MemoryCoordinator {
    let coordinator = MemoryCoordinator::with_limits(limits);
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
async fn todo(dir: &TempDir) -> Replica<TodoApp> {
    Replica::open_with_config(
        dir.path().join("test.db"),
        TodoApp,
        StaticPolicy::default(),
        crate::tests::identity(),
        runtime_config(),
    )
    .await
    .unwrap()
}
async fn edit_file(dir: &TempDir, sql: &str) {
    let db = crate::sqlite::Connection::open(&dir.path().join("test.db")).unwrap();
    db.execute(sql, ()).await.unwrap();
}
#[tokio::test]
async fn rejection_isolated_from_independent_work_and_preparation_survives() {
    let dir = TempDir::new().unwrap();
    let replica = todo(&dir).await;
    let coordinator = coordinator();
    let request = replica
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    let mut unrelated = request.clone();
    unrelated.id.node = fixtures::PHONE;
    let package = StrongPackage {
        request: unrelated,
        base: coordinator.head(fixtures::SCOPE).unwrap(),
        sequence: 1,
        rows: vec![0; 4],
        cut_base: Default::default(),
    };
    let forged = StrongReceipt {
        confirmation: package.confirmation().unwrap(),
        sequence: 1,
    };
    assert!(
        replica
            .receive_result(package.encode().unwrap(), forged.encode(), &coordinator)
            .await
            .is_err()
    );
    assert!(
        replica
            .strong_status()
            .await
            .unwrap()
            .verification
            .contains(&(forged.confirmation.result, VerificationState::Rejected))
    );
    replica.execute_pending(&coordinator).await.unwrap();
    assert_eq!(replica.confirmed_publications().await.unwrap().len(), 1);
    replica.close().await.unwrap();
    let replica = todo(&dir).await;
    replica
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    replica.execute_pending(&coordinator).await.unwrap();
    assert_eq!(replica.confirmed_publications().await.unwrap().len(), 2);
    replica.close().await.unwrap();
}
#[tokio::test]
async fn every_strong_ledger_binding_is_checked_offline_on_open() {
    let baseline = TempDir::new().unwrap();
    let replica = todo(&baseline).await;
    let preparer = coordinator();
    for _ in 0..2 {
        replica
            .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
            .await
            .unwrap();
        replica.execute_pending(&preparer).await.unwrap();
    }
    replica.close().await.unwrap();
    for sql in [
        "UPDATE rocky_strong_meta SET head=zeroblob(32)",
        "UPDATE rocky_strong_meta SET command_sequence=command_sequence+1",
        "UPDATE rocky_preparations SET hash=zeroblob(32) WHERE rowid=1",
        "UPDATE rocky_preparations SET receipt=zeroblob(32) WHERE rowid=1",
        "UPDATE rocky_preparations SET verified=0 WHERE rowid=1",
        "UPDATE rocky_preparations SET applied=0 WHERE rowid=1",
        "UPDATE rocky_preparations SET state='unknown' WHERE rowid=1",
        "DELETE FROM rocky_strong_outbox WHERE rowid=1",
        "DELETE FROM rocky_preparations WHERE rowid=1",
        "UPDATE rocky_strong_meta SET sequence=-1",
        "UPDATE rocky_strong_meta SET id=2",
        "UPDATE rocky_strong_meta SET configuration=NULL",
        "DELETE FROM rocky_command_handles WHERE rowid=1",
        "UPDATE rocky_requests SET sequence=9 WHERE rowid=1",
    ] {
        let dir = TempDir::new().unwrap();
        std::fs::copy(baseline.path().join("test.db"), dir.path().join("test.db")).unwrap();
        edit_file(&dir, sql).await;
        let restored = Replica::open_with_config(
            dir.path().join("test.db"),
            TodoApp,
            StaticPolicy::default(),
            crate::tests::identity(),
            runtime_config(),
        )
        .await;
        assert!(matches!(restored, Err(Error::Corrupt(_))), "{sql}");
    }
    let dir = TempDir::new().unwrap();
    let replica = todo(&dir).await;
    let coordinator = coordinator();
    replica
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    replica.execute_pending(&coordinator).await.unwrap();
    replica.close().await.unwrap();
    coordinator.set_available(false).unwrap();
    let replica = todo(&dir).await;
    assert_eq!(replica.query().await.unwrap().strong_head.sequence, 1);
    replica.close().await.unwrap();
}
#[tokio::test]
async fn three_named_peers_have_independent_operation_and_result_obligations() {
    let dir = TempDir::new().unwrap();
    let third = NodeId([9; 16]);
    let fourth = NodeId([10; 16]);
    let mut config = runtime_config();
    config
        .authors
        .extend([(third, fixtures::OWNER), (fourth, fixtures::OWNER)]);
    config.delivery_targets.extend([third, fourth]);
    let policy = StaticPolicy {
        nodes: config.authors.iter().map(|(node, _)| *node).collect(),
        ..Default::default()
    };
    let replica = Replica::open_with_config(
        dir.path().join("test.db"),
        TodoApp,
        policy.clone(),
        crate::tests::identity(),
        config.clone(),
    )
    .await
    .unwrap();
    let operation = create(&replica, "row").await;
    let coordinator = coordinator();
    replica
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    replica.execute_pending(&coordinator).await.unwrap();
    let hash = replica.confirmed_publications().await.unwrap()[0]
        .receipt
        .confirmation
        .result;
    replica
        .acknowledge(
            fixtures::PHONE,
            Acknowledgment {
                id: operation.id,
                applied: true,
            },
        )
        .await
        .unwrap();
    replica
        .acknowledge_result(fixtures::PHONE, hash, true)
        .await
        .unwrap();
    assert_eq!(replica.peer_progress(third).await.unwrap().pending, 1);
    assert_eq!(
        replica.strong_status().await.unwrap().commands[0].state,
        CommandState::ConfirmedLocally
    );
    replica.close().await.unwrap();
    let replica = Replica::open_with_config(
        dir.path().join("test.db"),
        TodoApp,
        policy,
        crate::tests::identity(),
        config,
    )
    .await
    .unwrap();
    for peer in [third, fourth] {
        replica
            .acknowledge(
                peer,
                Acknowledgment {
                    id: operation.id,
                    applied: true,
                },
            )
            .await
            .unwrap();
        replica.acknowledge_result(peer, hash, true).await.unwrap();
    }
    assert_eq!(
        replica.strong_status().await.unwrap().commands[0].state,
        CommandState::PeerApplied
    );
    replica.close().await.unwrap();
    let db = crate::sqlite::Connection::open(&dir.path().join("test.db")).unwrap();
    assert_eq!(
        crate::tests::rows(
            &db,
            "SELECT COUNT(*) FROM rocky_outbox UNION ALL SELECT COUNT(*) FROM rocky_strong_outbox"
        )
        .await
        .unwrap(),
        vec![vec![SqlValue::Integer(0)], vec![SqlValue::Integer(0)]]
    );
}
#[tokio::test]
async fn durable_handles_survive_cancelled_waiters_reopen_and_conflicting_retries() {
    let dir = TempDir::new().unwrap();
    let replica = todo(&dir).await;
    let receipt = replica
        .mutate(mutation(
            None,
            Action::CreateTodo {
                title: "retained".into(),
                completed: false,
            },
        ))
        .unwrap();
    let handle = receipt.handle();
    drop(receipt);
    let command_handle = CommandHandle([3; 32]);
    drop(
        replica
            .submit_command(command_handle, todo_app::PUBLISH_LIST_SNAPSHOT)
            .unwrap(),
    );
    replica.close().await.unwrap();
    let replica = todo(&dir).await;
    let original = replica.lookup_mutation(handle).await.unwrap().unwrap();
    let retry = replica
        .mutate_with_handle(
            handle,
            mutation(
                None,
                Action::CreateTodo {
                    title: "retained".into(),
                    completed: false,
                },
            ),
        )
        .unwrap()
        .committed()
        .await
        .unwrap();
    assert_eq!(original, retry);
    assert!(matches!(
        replica
            .mutate_with_handle(
                handle,
                mutation(
                    None,
                    Action::CreateTodo {
                        title: "different".into(),
                        completed: false
                    }
                )
            )
            .unwrap()
            .committed()
            .await,
        Err(Error::Conflict)
    ));
    let request = replica
        .lookup_command(command_handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        replica
            .submit_command(command_handle, todo_app::PUBLISH_LIST_SNAPSHOT)
            .unwrap()
            .committed()
            .await
            .unwrap(),
        request
    );
    assert!(matches!(
        replica
            .submit_command(command_handle, "Other")
            .unwrap()
            .committed()
            .await,
        Err(Error::Conflict)
    ));
    let stale = replica
        .mutate_with_precondition(
            MutationHandle([4; 32]),
            mutation(Some(original.id), Action::SetTitle("stale".into())),
            Some(LocalRevision::default()),
        )
        .unwrap()
        .committed()
        .await
        .unwrap_err();
    assert_eq!(stale.reason(), FailureReason::Conflict);
    assert_eq!(replica.query().await.unwrap().rows[0].title, "retained");
    replica.close().await.unwrap();
}
#[tokio::test]
async fn local_only_second_app_matches_batched_projection_replay_and_state_admission() {
    let dir = TempDir::new().unwrap();
    let scope = Scope {
        mesh: MeshId([11; 16]),
        app: AppId([12; 16]),
        domain: DomainId([13; 16]),
    };
    let caller = PrincipalId([14; 16]);
    let node = NodeId([15; 16]);
    let remote = NodeId([16; 16]);
    let config = RuntimeConfig {
        authors: vec![(node, caller), (remote, caller)],
        delivery_targets: vec![node, remote],
        full_nodes: Vec::new(),
    };
    let a = Replica::open_with_config(
        dir.path().join("a.db"),
        LedgerApp,
        (scope, caller),
        Identity {
            scope,
            node,
            caller,
        },
        config.clone(),
    )
    .await
    .unwrap();
    let b = Replica::open_with_config(
        dir.path().join("b.db"),
        LedgerApp,
        (scope, caller),
        Identity {
            scope,
            node: remote,
            caller,
        },
        config.clone(),
    )
    .await
    .unwrap();
    let created = a
        .mutate(Mutation {
            entity: None,
            payload: payload(0, 10, caller),
        })
        .unwrap()
        .committed()
        .await
        .unwrap();
    for delta in [-2, 5] {
        a.mutate(Mutation {
            entity: Some(created.id),
            payload: payload(1, delta, caller),
        })
        .unwrap()
        .committed()
        .await
        .unwrap();
    }
    assert!(a.request_command("Unused").await.is_err());
    for bytes in a.operation_page(None).await.unwrap().items {
        b.receive(bytes).await.unwrap();
    }
    fault(&b, worker::Fault::FailBeforeCommit).await;
    assert!(matches!(
        b.process_pending().await,
        Err(Error::InjectedFailure)
    ));
    assert!(b.query().await.unwrap().rows.is_empty());
    assert_eq!(b.query().await.unwrap().revision, LocalRevision::default());
    b.process_pending().await.unwrap();
    assert_eq!(a.query().await.unwrap().rows, b.query().await.unwrap().rows);
    let before = a.query().await.unwrap();
    let denied = a
        .mutate(Mutation {
            entity: Some(created.id),
            payload: payload(2, 1, PrincipalId([99; 16])),
        })
        .unwrap()
        .committed()
        .await
        .unwrap_err();
    assert_eq!(denied.reason(), FailureReason::Denial);
    assert_eq!(a.query().await.unwrap(), before);
    let mut bad = created.clone();
    bad.id.sequence = AuthorSequence::new(4).unwrap();
    bad.entity = created.id;
    bad.lamport = LamportTime::new(4).unwrap();
    bad.dependencies = required_dependencies(bad.id, bad.entity, false).unwrap();
    bad.payload = payload(2, 1, PrincipalId([99; 16]));
    b.receive(bad.encode().unwrap()).await.unwrap();
    assert!(b.process_pending().await.is_err());
    a.close().await.unwrap();
    b.close().await.unwrap();
    let b = Replica::open_with_config(
        dir.path().join("b.db"),
        LedgerApp,
        (scope, caller),
        Identity {
            scope,
            node: remote,
            caller,
        },
        config,
    )
    .await
    .unwrap();
    assert_eq!(b.query().await.unwrap().rows[0].1, 13);
    b.close().await.unwrap();
}
#[tokio::test]
async fn bounded_pages_and_frame_rejection_preserve_committed_state() {
    let dir = TempDir::new().unwrap();
    let replica = todo(&dir).await;
    // More outstanding results than one session window while no peer applies them.
    let coordinator = coordinator_with(rocky_coordinator::Limits {
        window_entries: 64,
        ..Default::default()
    });
    for _ in 0..35 {
        create(&replica, "row").await;
        replica
            .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
            .await
            .unwrap();
    }
    replica.execute_pending(&coordinator).await.unwrap();
    replica.execute_pending(&coordinator).await.unwrap();
    let mut requests = Vec::new();
    let mut cursor = None;
    loop {
        let page = replica.request_page(cursor).await.unwrap();
        assert!(page.items.len() <= 32);
        requests.extend(page.items.iter().map(|r| r.id));
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(requests.len(), 35);
    assert_eq!(
        requests
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        35
    );
    let mut results = Vec::new();
    let mut cursor = None;
    loop {
        let page = replica.result_page(cursor).await.unwrap();
        assert!(page.items.len() <= 32);
        results.extend(page.items.iter().map(|r| r.receipt.confirmation.result));
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(results.len(), 35);
    assert_eq!(
        results
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        35
    );
    let baseline = replica.query().await.unwrap();
    assert!(matches!(
        replica.receive(vec![0; MAX_FRAME_BYTES + 1]).await,
        Err(Error::Quota { .. })
    ));
    assert_eq!(replica.query().await.unwrap(), baseline);
    let (entered, waiting) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    fault(&replica, worker::Fault::Pause(entered, resumed)).await;
    let receipt = replica
        .mutate(mutation(
            None,
            Action::CreateTodo {
                title: "paused".into(),
                completed: false,
            },
        ))
        .unwrap();
    waiting.await.unwrap();
    let mut held = Vec::new();
    for _ in 0..32 {
        let (reply, _) = oneshot::channel();
        let result = replica.inner.commands.try_send(Command::Strong(
            Box::new(strong::Request::StoreCommand(vec![
                0;
                MAX_FRAME_BYTES - 256
            ])),
            reply,
        ));
        if let Err(error) = result {
            held.push(error);
            break;
        }
    }
    assert!(matches!(held.as_slice(), [Error::Backpressure]));
    resume.send(()).unwrap();
    receipt.committed().await.unwrap();
    replica.close().await.unwrap();
}

#[tokio::test]
async fn custom_scope_and_client_only_invoker_persist_and_reopen() {
    let dir = TempDir::new().unwrap();
    let scope = Scope {
        mesh: MeshId([21; 16]),
        app: AppId([22; 16]),
        domain: DomainId([23; 16]),
    };
    let caller = PrincipalId([24; 16]);
    let executor = NodeId([25; 16]);
    let client = NodeId([26; 16]);
    let policy = StaticPolicy {
        scope,
        caller,
        nodes: vec![executor, client],
        ..Default::default()
    };
    let config = RuntimeConfig {
        authors: vec![(executor, caller), (client, caller)],
        delivery_targets: vec![executor, client],
        full_nodes: Vec::new(),
    };
    let a = Replica::open_with_config(
        dir.path().join("a.db"),
        TodoApp,
        policy.clone(),
        Identity {
            scope,
            node: executor,
            caller,
        },
        config.clone(),
    )
    .await
    .unwrap();
    let b = Replica::open_with_config(
        dir.path().join("b.db"),
        TodoApp,
        policy.clone(),
        Identity {
            scope,
            node: client,
            caller,
        },
        config.clone(),
    )
    .await
    .unwrap();
    let coordinator = MemoryCoordinator::default();
    coordinator
        .register(scope, caller, [27; 16], vec![executor], 1, 1)
        .unwrap();
    coordinator
        .set_invokers(scope, caller, vec![(client, caller)])
        .unwrap();
    coordinator.acquire(scope, caller, executor).unwrap();
    let operation = create(&b, "custom author").await;
    a.receive(operation.encode().unwrap()).await.unwrap();
    a.process_pending().await.unwrap();
    let request = b
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    a.receive_request(request.encode().unwrap()).await.unwrap();
    a.execute_pending(&coordinator).await.unwrap();
    assert_eq!(coordinator.head(scope).unwrap().sequence, 1);
    assert!(coordinator.acquire(scope, caller, client).is_err());
    let result = a.confirmed_publications().await.unwrap().remove(0);
    b.receive_result(
        result.package.encode().unwrap(),
        result.receipt.encode(),
        &coordinator,
    )
    .await
    .unwrap();
    a.close().await.unwrap();
    b.close().await.unwrap();
    let b = Replica::open_with_config(
        dir.path().join("b.db"),
        TodoApp,
        policy,
        Identity {
            scope,
            node: client,
            caller,
        },
        config,
    )
    .await
    .unwrap();
    assert_eq!(b.query().await.unwrap().rows[0].title, "custom author");
    assert_eq!(b.query().await.unwrap().strong_head.sequence, 1);
    b.close().await.unwrap();
}

#[tokio::test]
async fn pending_processing_yields_to_queued_query_between_transactions() {
    let dir = TempDir::new().unwrap();
    let a = todo(&dir).await;
    let b = Replica::open_with_config(
        dir.path().join("b.db"),
        TodoApp,
        StaticPolicy::default(),
        Identity {
            node: fixtures::PHONE,
            ..crate::tests::identity()
        },
        runtime_config(),
    )
    .await
    .unwrap();
    for _ in 0..65 {
        let op = create(&a, "row").await;
        b.receive(op.encode().unwrap()).await.unwrap();
    }
    let (entered, waiting) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    fault(&b, worker::Fault::Pause(entered, resumed)).await;
    let processing = b.clone();
    let task = tokio::spawn(async move { processing.process_pending().await });
    waiting.await.unwrap();
    let (reply, result) = oneshot::channel();
    b.inner.commands.send(Command::Query(reply)).await.unwrap();
    resume.send(()).unwrap();
    let snapshot = result.await.unwrap().unwrap();
    assert_eq!(snapshot.rows.len(), 32);
    task.await.unwrap().unwrap();
    assert_eq!(b.query().await.unwrap().rows.len(), 65);
    a.close().await.unwrap();
    b.close().await.unwrap();
}

struct BrokenApp;
impl Application for BrokenApp {
    type State = ledger_app::State;
    type Policy = (Scope, PrincipalId);
    type Error = ledger_app::Denied;
    fn validate(&self, op: &Envelope) -> Result<(), Self::Error> {
        LedgerApp.validate(op)
    }
    fn admit(&self, op: &Envelope, policy: &Self::Policy) -> Result<(), Self::Error> {
        LedgerApp.admit(op, policy)
    }
    fn apply_ready(
        &self,
        state: &mut Self::State,
        op: &Envelope,
        boundary: AdmissionBoundary,
    ) -> Result<(), Self::Error> {
        LedgerApp.apply_ready(state, op, boundary)
    }
}
impl CompiledSchema for BrokenApp {
    fn schema(&self) -> &'static [&'static str] {
        &["CREATE TABLE partial(value INTEGER)", "INVALID DDL"]
    }
    fn projection_query(&self) -> &'static str {
        LedgerApp.projection_query()
    }
    fn checkpoint_tables(&self) -> &'static [&'static str] {
        &[]
    }
}
impl StoredApplication for BrokenApp {
    type Row = (OperationId, i64);
    fn descriptor(&self) -> ApplicationDescriptor {
        ApplicationDescriptor {
            definition: "broken.schema.v1",
            ..LedgerApp.descriptor()
        }
    }
    fn projection_rows(&self, state: &Self::State) -> Vec<Vec<SqlValue>> {
        LedgerApp.projection_rows(state)
    }
    fn touched(&self, op: &Envelope) -> Vec<ProjectionStatement> {
        LedgerApp.touched(op)
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<Self::State, Self::Error> {
        LedgerApp.window(rows)
    }
    fn project(&self, state: &Self::State, op: &Envelope) -> Vec<ProjectionStatement> {
        LedgerApp.project(state, op)
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Self::Error> {
        LedgerApp.decode_rows(rows)
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Self::Policy,
    ) -> Result<(), Self::Error> {
        LedgerApp.admit_read(scope, caller, policy)
    }
}
#[tokio::test]
async fn interrupted_initialization_rolls_back_and_definition_mismatch_writes_no_schema() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.db");
    let identity = crate::tests::identity();
    assert!(
        Replica::open(
            &path,
            BrokenApp,
            (identity.scope, identity.caller),
            identity
        )
        .await
        .is_err()
    );
    {
        let db = crate::sqlite::Connection::open(&path).unwrap();
        assert!(
            crate::tests::rows(
                &db,
                "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'"
            )
            .await
            .unwrap()
            .is_empty()
        );
    }
    let replica = Replica::open(
        &path,
        LedgerApp,
        (identity.scope, identity.caller),
        identity,
    )
    .await
    .unwrap();
    replica.close().await.unwrap();
    assert!(matches!(
        Replica::open(
            &path,
            BrokenApp,
            (identity.scope, identity.caller),
            identity
        )
        .await,
        Err(Error::Incompatible(_))
    ));
    {
        let db = crate::sqlite::Connection::open(&path).unwrap();
        assert!(
            crate::tests::rows(&db, "SELECT name FROM sqlite_schema WHERE name='partial'")
                .await
                .unwrap()
                .is_empty()
        );
    }
    let replica = Replica::open(
        &path,
        LedgerApp,
        (identity.scope, identity.caller),
        identity,
    )
    .await
    .unwrap();
    replica.close().await.unwrap();
}

#[tokio::test]
async fn paired_row_field_grants_filter_whole_rows_and_reject_forbidden_fields() {
    let dir = TempDir::new().unwrap();
    let replica = todo(&dir).await;
    let first = create(&replica, "first").await;
    create(&replica, "hidden").await;
    let coordinator = coordinator();
    replica
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    replica.execute_pending(&coordinator).await.unwrap();
    assert_eq!(replica.confirmed_publications().await.unwrap().len(), 1);
    replica.close().await.unwrap();
    let policy = StaticPolicy {
        version: 2,
        rows: Some(vec![todo_app::RowGrant {
            entity: first.id,
            read: true,
            fields: vec![todo_app::TITLE_FIELD_ID],
            delete: false,
        }]),
        ..Default::default()
    };
    let replica = Replica::open_with_config(
        dir.path().join("test.db"),
        TodoApp,
        policy,
        crate::tests::identity(),
        runtime_config(),
    )
    .await
    .unwrap();
    assert!(replica.confirmed_publications().await.unwrap().is_empty());
    assert_eq!(replica.query().await.unwrap().rows.len(), 1);
    assert!(
        replica
            .mutate(mutation(Some(first.id), Action::SetCompleted(true)))
            .unwrap()
            .committed()
            .await
            .is_err()
    );
    replica
        .mutate(mutation(Some(first.id), Action::SetTitle("allowed".into())))
        .unwrap()
        .committed()
        .await
        .unwrap();
    assert_eq!(replica.query().await.unwrap().rows[0].title, "allowed");
    replica.close().await.unwrap();
}

#[tokio::test]
async fn observer_and_retained_history_quotas_preserve_existing_work() {
    let dir = TempDir::new().unwrap();
    let replica = todo(&dir).await;
    let observers: Vec<_> = (0..MAX_OBSERVERS)
        .map(|_| replica.observe().unwrap())
        .collect();
    assert!(matches!(replica.observe(), Err(Error::Quota { .. })));
    drop(observers);
    drop(replica.observe().unwrap());
    replica.close().await.unwrap();
    // The strong ledger is bounded by bytes; rejection evidence is never pruned.
    let bound = crate::RetentionPolicy::default().retained_bytes;
    edit_file(&dir, &format!("INSERT INTO rocky_rejected_receipts VALUES(zeroblob(32),zeroblob({bound}),x'00','quota fixture')")).await;
    let replica = todo(&dir).await;
    let before = replica.query().await.unwrap();
    let error = replica
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap_err();
    assert_eq!(error.reason(), FailureReason::Quota);
    assert!(matches!(error, Error::Retention { .. }));
    assert!(replica.strong_status().await.unwrap().commands.is_empty());
    assert_eq!(replica.query().await.unwrap(), before);
    create(&replica, "ordinary edits remain independent").await;
    replica.close().await.unwrap();
}
