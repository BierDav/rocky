use super::*;
use crate::tests::{create, fault, mutation};
use rocky_coordinator::MemoryCoordinator;
use rocky_core::*;
use tempfile::TempDir;
use todo_app::{Action, StaticPolicy, TodoApp, decode_snapshot};
fn coordinator() -> MemoryCoordinator {
    coordinator_with(rocky_coordinator::Limits::default())
}
fn coordinator_with(limits: rocky_coordinator::Limits) -> MemoryCoordinator {
    let c = MemoryCoordinator::with_limits(limits);
    c.register(
        fixtures::SCOPE,
        fixtures::OWNER,
        [7; 16],
        vec![fixtures::LAPTOP, fixtures::PHONE],
        1,
        1,
    )
    .unwrap();
    c.acquire(fixtures::SCOPE, fixtures::OWNER, fixtures::LAPTOP)
        .unwrap();
    c
}
async fn open(dir: &TempDir, node: NodeId) -> Replica<TodoApp> {
    Replica::open_with_config(
        dir.path().join(if node == fixtures::LAPTOP {
            "laptop.db"
        } else {
            "phone.db"
        }),
        TodoApp,
        StaticPolicy::default(),
        Identity {
            node,
            ..crate::tests::identity()
        },
        crate::tests::runtime_config(),
    )
    .await
    .unwrap()
}
async fn request(
    r: &Replica<TodoApp>,
    request: strong::Request,
) -> Result<strong::Response, Error> {
    let (reply, result) = oneshot::channel();
    r.inner
        .commands
        .send(Command::Strong(Box::new(request), reply))
        .await
        .unwrap();
    result.await.unwrap()
}
async fn prepare(r: &Replica<TodoApp>, c: &dyn Coordinator, req: StrongRequest) -> StrongPackage {
    match request(
        r,
        strong::Request::Prepare(Box::new(req), c.head(fixtures::SCOPE).await.unwrap()),
    )
    .await
    .unwrap()
    {
        strong::Response::Package(p) => p,
        _ => panic!("package"),
    }
}
async fn sync(a: &Replica<TodoApp>, b: &Replica<TodoApp>) {
    let Catchup::Operations(page) = a
        .catch_up(None, b.sync_state().await.unwrap())
        .await
        .unwrap()
    else {
        panic!("nothing is pruned here");
    };
    for bytes in page.records {
        b.receive(bytes).await.unwrap();
    }
    b.process_pending().await.unwrap();
}
#[tokio::test]
async fn sealed_phone_cut_replays_older_executor_state_and_peer_evidence() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP).await;
    let phone = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    let op = create(&laptop, "sealed title").await;
    sync(&laptop, &phone).await;
    let req = phone
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    laptop
        .mutate(mutation(Some(op.id), Action::SetTitle("new title".into())))
        .unwrap()
        .committed()
        .await
        .unwrap();
    laptop.receive_request(req.encode().unwrap()).await.unwrap();
    laptop.execute_pending(&c).await.unwrap();
    let final_result = laptop.strong_results().await.unwrap().remove(0);
    assert_eq!(
        decode_snapshot(&final_result.package.rows).unwrap()[0].title,
        "sealed title"
    );
    assert_eq!(laptop.query().await.unwrap().rows[0].title, "new title");
    phone
        .receive_result(
            final_result.package.encode().unwrap(),
            final_result.receipt.encode(),
            &c,
        )
        .await
        .unwrap();
    phone
        .receive_result(
            final_result.package.encode().unwrap(),
            final_result.receipt.encode(),
            &c,
        )
        .await
        .unwrap();
    laptop
        .acknowledge_result(
            fixtures::PHONE,
            final_result.receipt.confirmation.result,
            true,
        )
        .await
        .unwrap();
    assert_eq!(phone.confirmed_publications().await.unwrap().len(), 1);
    assert_eq!(
        laptop.strong_status().await.unwrap().commands[0].state,
        CommandState::PeerApplied
    );
    assert_eq!(phone.strong_status().await.unwrap().sequence, 1);
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 1);
    laptop.close().await.unwrap();
    phone.close().await.unwrap();
}
#[tokio::test]
async fn unavailable_coordination_and_lost_reply_preserve_input_and_retry_identity() {
    let dir = TempDir::new().unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    let c = coordinator();
    create(&r, "original").await;
    let req = r
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    c.set_available(false).unwrap();
    assert!(r.execute_pending(&c).await.is_err());
    create(&r, "later").await;
    assert!(r.confirmed_publications().await.unwrap().is_empty());
    r.close().await.unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    assert_eq!(r.strong_status().await.unwrap().commands[0].request, req);
    c.set_available(true).unwrap();
    c.drop_next_reply().unwrap();
    assert!(r.execute_pending(&c).await.is_err());
    assert_eq!(
        r.strong_status().await.unwrap().commands[0].state,
        CommandState::OutcomeUnknown
    );
    assert!(r.confirmed_publications().await.unwrap().is_empty());
    r.close().await.unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    r.execute_pending(&c).await.unwrap();
    r.execute_pending(&c).await.unwrap();
    let query = r.query().await.unwrap();
    assert_eq!(query.rows.len(), 2);
    assert_eq!(
        decode_snapshot(&r.confirmed_publications().await.unwrap()[0].package.rows)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 1);
    r.close().await.unwrap();
}
#[tokio::test]
async fn preparation_acceptance_and_apply_failures_recover_at_each_boundary() {
    // Boundaries: request stored; attempt prepared; confirmed without the
    // reply; result stored unverified; application failed.
    for boundary in 0..5 {
        let dir = TempDir::new().unwrap();
        let r = open(&dir, fixtures::LAPTOP).await;
        let c = coordinator();
        create(&r, "row").await;
        let req = r
            .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
            .await
            .unwrap();
        if boundary > 0 {
            let p = prepare(&r, &c, req.clone()).await;
            assert!(r.confirmed_publications().await.unwrap().is_empty());
            // The attempt is outcome unknown from its own commit on.
            assert_eq!(
                r.strong_status().await.unwrap().commands[0].state,
                CommandState::OutcomeUnknown
            );
            if boundary > 1 {
                let receipt = c.confirm(p.confirmation().unwrap()).unwrap();
                if boundary > 2 {
                    request(
                        &r,
                        strong::Request::StoreResult(
                            p.encode().unwrap(),
                            receipt.encode(),
                            strong::Verdict::Deferred,
                        ),
                    )
                    .await
                    .unwrap();
                    if boundary > 3 {
                        fault(&r, worker::Fault::FailBeforeCommit).await;
                        assert!(matches!(
                            r.reconcile_strong(&c).await,
                            Err(Error::InjectedFailure)
                        ));
                        assert!(r.confirmed_publications().await.unwrap().is_empty());
                    }
                }
            }
        }
        r.close().await.unwrap();
        let r = open(&dir, fixtures::LAPTOP).await;
        r.execute_pending(&c).await.unwrap();
        assert_eq!(r.confirmed_publications().await.unwrap().len(), 1);
        assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 1);
        r.close().await.unwrap();
    }
}
#[tokio::test]
async fn reordered_results_and_missing_input_resume_after_receiver_restart() {
    let dir = TempDir::new().unwrap();
    let a = open(&dir, fixtures::LAPTOP).await;
    let b = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    create(&a, "first").await;
    a.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    a.execute_pending(&c).await.unwrap();
    create(&a, "second").await;
    a.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    a.execute_pending(&c).await.unwrap();
    let results = a.strong_results().await.unwrap();
    let second = &results[1];
    b.receive_result(
        second.package.encode().unwrap(),
        second.receipt.encode(),
        &c,
    )
    .await
    .unwrap();
    assert!(b.confirmed_publications().await.unwrap().is_empty());
    let first = &results[0];
    b.receive_result(first.package.encode().unwrap(), first.receipt.encode(), &c)
        .await
        .unwrap();
    assert!(b.confirmed_publications().await.unwrap().is_empty());
    b.close().await.unwrap();
    let b = open(&dir, fixtures::PHONE).await;
    sync(&a, &b).await;
    b.reconcile_strong(&c).await.unwrap();
    assert_eq!(b.confirmed_publications().await.unwrap().len(), 2);
    let revision = b.query().await.unwrap().revision;
    b.reconcile_strong(&c).await.unwrap();
    assert_eq!(b.query().await.unwrap().revision, revision);
    a.close().await.unwrap();
    b.close().await.unwrap();
}
#[tokio::test]
async fn mismatch_denial_forgery_and_ledger_loss_never_materialize() {
    let dir = TempDir::new().unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    let c = coordinator();
    create(&r, "row").await;
    let req = r
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    let mut bad = req.clone();
    bad.digest = [0; 32];
    assert!(
        request(
            &r,
            strong::Request::Prepare(Box::new(bad), c.head(fixtures::SCOPE).unwrap())
        )
        .await
        .is_err()
    );
    let p = prepare(&r, &c, req).await;
    let forged = StrongReceipt {
        confirmation: p.confirmation().unwrap(),
        sequence: 1,
    };
    assert!(
        r.receive_result(p.encode().unwrap(), forged.encode(), &c)
            .await
            .is_err()
    );
    assert!(r.confirmed_publications().await.unwrap().is_empty());
    let receipt = c.confirm(p.confirmation().unwrap()).unwrap();
    r.receive_result(p.encode().unwrap(), receipt.encode(), &c)
        .await
        .unwrap();
    let fresh = MemoryCoordinator::default();
    fresh
        .register(
            fixtures::SCOPE,
            fixtures::OWNER,
            [8; 16],
            vec![fixtures::LAPTOP, fixtures::PHONE],
            1,
            1,
        )
        .unwrap();
    fresh
        .acquire(fixtures::SCOPE, fixtures::OWNER, fixtures::LAPTOP)
        .unwrap();
    assert!(matches!(
        r.execute_pending(&fresh).await,
        Err(Error::Coordination(
            CoordinationError::CoordinationStateLost
        ))
    ));
    create(&r, "edits still work").await;
    assert_eq!(r.confirmed_publications().await.unwrap().len(), 1);
    r.close().await.unwrap();
    let denied = StaticPolicy {
        publish: false,
        ..Default::default()
    };
    let r = Replica::open_with_config(
        dir.path().join("laptop.db"),
        TodoApp,
        denied,
        crate::tests::identity(),
        crate::tests::runtime_config(),
    )
    .await
    .unwrap();
    assert!(
        r.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
            .await
            .is_err()
    );
    r.close().await.unwrap();
}
#[tokio::test]
async fn takeover_requires_the_head_and_missing_confirmed_package_blocks_progress() {
    let dir = TempDir::new().unwrap();
    let a = open(&dir, fixtures::LAPTOP).await;
    let b = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    let req = a
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    let p = prepare(&a, &c, req).await;
    let old = c.confirm(p.confirmation().unwrap()).unwrap();
    c.takeover(fixtures::SCOPE, fixtures::OWNER, fixtures::PHONE, 1)
        .unwrap();
    b.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    assert!(matches!(
        b.execute_pending(&c).await,
        Err(Error::StrongPending(_))
    ));
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 1);
    b.receive_result(p.encode().unwrap(), old.encode(), &c)
        .await
        .unwrap();
    b.execute_pending(&c).await.unwrap();
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 2);
    a.receive_result(p.encode().unwrap(), old.encode(), &c)
        .await
        .unwrap();
    a.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    assert!(a.execute_pending(&c).await.is_err());
    a.close().await.unwrap();
    b.close().await.unwrap();
}
struct BarrierCoordinator {
    inner: MemoryCoordinator,
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    resume: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
}
impl Coordinator for BarrierCoordinator {
    fn head(&self, s: Scope) -> CoordinationFuture<'_, StrongHead> {
        Box::pin(async move { self.inner.head(s) })
    }
    fn lookup(&self, s: Scope, id: RequestId) -> CoordinationFuture<'_, Option<StrongReceipt>> {
        Box::pin(async move { self.inner.lookup(s, id) })
    }
    fn verify<'a>(&'a self, r: &'a StrongReceipt) -> CoordinationFuture<'a, ()> {
        Box::pin(async move { self.inner.verify(r) })
    }
    fn confirm(&self, c: Confirmation) -> CoordinationFuture<'_, StrongReceipt> {
        Box::pin(async move {
            let entered = self.entered.lock().unwrap().take();
            let resume = self.resume.lock().unwrap().take();
            if let Some(entered) = entered {
                let _ = entered.send(());
            }
            if let Some(resume) = resume {
                let _ = resume.await;
            }
            self.inner.confirm(c)
        })
    }
    fn retire(&self, s: Scope, node: NodeId, through: u64) -> CoordinationFuture<'_, ()> {
        Coordinator::retire(&self.inner, s, node, through)
    }
}
#[tokio::test]
async fn coordinator_barrier_holds_no_database_transaction_and_new_edits_do_not_change_preparation()
{
    let dir = TempDir::new().unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    create(&r, "sealed").await;
    r.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    let (entered, waiting) = oneshot::channel();
    let (resume, blocked) = oneshot::channel();
    let c = Arc::new(BarrierCoordinator {
        inner: coordinator(),
        entered: std::sync::Mutex::new(Some(entered)),
        resume: std::sync::Mutex::new(Some(blocked)),
    });
    let executing = r.clone();
    let coordinator = c.clone();
    let task = tokio::spawn(async move { executing.execute_pending(coordinator.as_ref()).await });
    waiting.await.unwrap();
    create(&r, "during external confirmation").await;
    assert_eq!(r.query().await.unwrap().rows.len(), 2);
    assert!(r.confirmed_publications().await.unwrap().is_empty());
    resume.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(
        decode_snapshot(&r.confirmed_publications().await.unwrap()[0].package.rows)
            .unwrap()
            .len(),
        1
    );
    r.close().await.unwrap();
}
#[tokio::test]
async fn confirmed_package_loss_blocks_without_rewinding_coordinator() {
    let dir = TempDir::new().unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    let c = coordinator();
    let req = r
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    let p = prepare(&r, &c, req).await;
    c.confirm(p.confirmation().unwrap()).unwrap();
    r.close().await.unwrap();
    {
        let db = crate::sqlite::Connection::open(&dir.path().join("laptop.db")).unwrap();
        db.execute("DELETE FROM rocky_preparations", ())
            .await
            .unwrap();
    }
    let r = open(&dir, fixtures::LAPTOP).await;
    assert!(matches!(
        r.execute_pending(&c).await,
        Err(Error::StrongPending(_))
    ));
    assert!(r.confirmed_publications().await.unwrap().is_empty());
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 1);
    r.close().await.unwrap();
}

#[tokio::test]
async fn receiver_defers_unverified_receipt_through_outage_and_restart() {
    let dir = TempDir::new().unwrap();
    let a = open(&dir, fixtures::LAPTOP).await;
    let b = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    a.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    a.execute_pending(&c).await.unwrap();
    let result = a.strong_results().await.unwrap().remove(0);
    c.set_available(false).unwrap();
    b.receive_result(
        result.package.encode().unwrap(),
        result.receipt.encode(),
        &c,
    )
    .await
    .unwrap();
    assert!(b.confirmed_publications().await.unwrap().is_empty());
    assert_eq!(
        b.strong_status().await.unwrap().received,
        vec![(result.receipt.confirmation.result, false)]
    );
    a.acknowledge_result(fixtures::PHONE, result.receipt.confirmation.result, false)
        .await
        .unwrap();
    assert!(a.strong_status().await.unwrap().commands[0].peer_received);
    assert_eq!(
        a.strong_status().await.unwrap().commands[0].state,
        CommandState::ConfirmedLocally
    );
    b.close().await.unwrap();
    let b = open(&dir, fixtures::PHONE).await;
    b.reconcile_strong(&c).await.unwrap();
    assert!(b.confirmed_publications().await.unwrap().is_empty());
    c.set_available(true).unwrap();
    b.reconcile_strong(&c).await.unwrap();
    assert_eq!(b.confirmed_publications().await.unwrap().len(), 1);
    a.close().await.unwrap();
    b.close().await.unwrap();
}
#[tokio::test]
async fn malformed_binding_foreign_scope_and_generation_are_rejected_before_application() {
    let dir = TempDir::new().unwrap();
    let a = open(&dir, fixtures::LAPTOP).await;
    let b = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    a.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    a.execute_pending(&c).await.unwrap();
    let result = a.strong_results().await.unwrap().remove(0);
    let mut changed = result.package.clone();
    changed.rows = vec![1, 0, 0, 0];
    assert!(
        b.receive_result(changed.encode().unwrap(), result.receipt.encode(), &c)
            .await
            .is_err()
    );
    for kind in 0..3 {
        let mut p = result.package.clone();
        match kind {
            0 => {
                p.request.scope.domain = DomainId([0; 16]);
                p.base.scope = p.request.scope;
            }
            1 => {
                p.request.schema = 2;
                p.base.schema = 2;
            }
            _ => p.request.caller = PrincipalId([0; 16]),
        };
        let receipt = StrongReceipt {
            confirmation: p.confirmation().unwrap(),
            sequence: 1,
        };
        assert!(
            b.receive_result(p.encode().unwrap(), receipt.encode(), &c)
                .await
                .is_err()
        );
    }
    assert!(b.confirmed_publications().await.unwrap().is_empty());
    assert!(b.strong_status().await.unwrap().commands.is_empty());
    a.close().await.unwrap();
    b.close().await.unwrap();
}
#[tokio::test]
async fn non_dependency_closed_cut_is_rejected() {
    let dir = TempDir::new().unwrap();
    let a = open(&dir, fixtures::LAPTOP).await;
    let b = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    let op = create(&a, "row").await;
    sync(&a, &b).await;
    b.mutate(mutation(Some(op.id), Action::SetTitle("phone edit".into())))
        .unwrap()
        .committed()
        .await
        .unwrap();
    sync(&b, &a).await;
    let mut req = b
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    req.frontier.0.retain(|(node, _)| *node == fixtures::PHONE);
    assert!(matches!(
        request(
            &a,
            strong::Request::Prepare(Box::new(req), c.head(fixtures::SCOPE).unwrap())
        )
        .await,
        Err(Error::Rejected(_))
    ));
    assert!(a.confirmed_publications().await.unwrap().is_empty());
    a.close().await.unwrap();
    b.close().await.unwrap();
}

#[tokio::test]
async fn executor_pass_limit_does_not_starve_commands_after_confirmed_history() {
    let dir = TempDir::new().unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    // More outstanding results than one session window while Phone never applies them.
    let c = coordinator_with(rocky_coordinator::Limits {
        window_entries: 64,
        ..Default::default()
    });
    for _ in 0..33 {
        r.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
            .await
            .unwrap();
    }
    r.execute_pending(&c).await.unwrap();
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 32);
    r.execute_pending(&c).await.unwrap();
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 33);
    assert_eq!(r.confirmed_publications().await.unwrap().len(), 33);
    r.close().await.unwrap();
}

const DAY: u64 = 86_400;
#[tokio::test]
async fn an_unconfirmed_request_expires_and_is_never_prepared() {
    let dir = TempDir::new().unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    let c = coordinator();
    let request = r
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    // The executor is away for longer than the admission window.
    c.set_time(c.time().unwrap() + DAY + 60).unwrap();
    r.execute_pending(&c).await.unwrap();
    r.execute_pending(&c).await.unwrap();
    let status = r.strong_status().await.unwrap();
    assert_eq!(status.commands[0].request, request);
    assert_eq!(status.commands[0].state, CommandState::Expired);
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 0);
    // It no longer holds checkpoints.
    assert_eq!(r.retention_status().await.unwrap().hold, None);
    r.close().await.unwrap();
}
#[tokio::test]
async fn a_lost_reply_retired_before_reconciliation_never_executes_twice() {
    let dir = TempDir::new().unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    let c = coordinator();
    r.request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    c.drop_next_reply().unwrap();
    assert!(r.execute_pending(&c).await.is_err());
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 1);
    // The client crashes and returns after the response horizon.
    r.close().await.unwrap();
    c.set_time(c.time().unwrap() + 7 * DAY + 60).unwrap();
    let r = open(&dir, fixtures::LAPTOP).await;
    r.execute_pending(&c).await.unwrap();
    let status = r.strong_status().await.unwrap();
    assert_eq!(status.commands[0].state, CommandState::Retired);
    // Neither re-prepared under the old identity nor replaced by a new one.
    assert_eq!(c.head(fixtures::SCOPE).unwrap().sequence, 1);
    assert_eq!(status.commands.len(), 1);
    assert!(r.confirmed_publications().await.unwrap().is_empty());
    r.close().await.unwrap();
}
#[tokio::test]
async fn results_applied_by_every_target_are_acknowledged_for_retirement() {
    let dir = TempDir::new().unwrap();
    let a = open(&dir, fixtures::LAPTOP).await;
    let b = open(&dir, fixtures::PHONE).await;
    let c = coordinator();
    let first = a
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    a.execute_pending(&c).await.unwrap();
    // Phone still verifies the receipt online, so it is not retired yet.
    assert!(matches!(
        c.status(fixtures::SCOPE, first.id).unwrap(),
        rocky_coordinator::Status::Confirmed(_)
    ));
    let result = a.strong_results().await.unwrap().remove(0);
    b.receive_result(
        result.package.encode().unwrap(),
        result.receipt.encode(),
        &c,
    )
    .await
    .unwrap();
    a.acknowledge_result(fixtures::PHONE, result.receipt.confirmation.result, true)
        .await
        .unwrap();
    a.reconcile_strong(&c).await.unwrap();
    assert_eq!(
        c.status(fixtures::SCOPE, first.id).unwrap(),
        rocky_coordinator::Status::Retired
    );
    assert_eq!(
        a.strong_status().await.unwrap().commands[0].state,
        CommandState::PeerApplied
    );
    a.close().await.unwrap();
    b.close().await.unwrap();
}
