use super::*;
use crate::tests::fault;
use inventory_app::{InventoryApp, Policy, RESERVE, Reservation};
use rocky_coordinator::MemoryCoordinator;
use rocky_core::*;
use tempfile::TempDir;

fn scope(domain: u8) -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain: DomainId([domain; 16]),
    }
}
fn identity(domain: u8, node: u8) -> Identity {
    Identity {
        scope: scope(domain),
        node: NodeId([node; 16]),
        caller: PrincipalId([7; 16]),
    }
}
fn config() -> RuntimeConfig {
    RuntimeConfig {
        authors: vec![
            (NodeId([3; 16]), PrincipalId([7; 16])),
            (NodeId([4; 16]), PrincipalId([7; 16])),
        ],
        delivery_targets: vec![NodeId([4; 16])],
        full_nodes: Vec::new(),
    }
}
fn installs(node: u8) -> Vec<DomainInstallation<InventoryApp>> {
    [1, 2]
        .into_iter()
        .map(|domain| DomainInstallation {
            app: InventoryApp,
            policy: policy(domain),
            identity: identity(domain, node),
            config: config(),
        })
        .collect()
}
fn policy(domain: u8) -> Policy {
    Policy {
        scope: scope(domain),
        caller: PrincipalId([7; 16]),
        reference: [9; 32],
        note_prefix: None,
    }
}
async fn open(dir: &TempDir, node: u8) -> Runtime<InventoryApp> {
    Runtime::open(dir.path().join(format!("{node}.db")), installs(node))
        .await
        .unwrap()
}
fn coordinator() -> MemoryCoordinator {
    let coordinator = MemoryCoordinator::default();
    for domain in [1, 2] {
        coordinator
            .register(
                scope(domain),
                PrincipalId([7; 16]),
                [6; 16],
                vec![NodeId([3; 16]), NodeId([4; 16])],
                1,
                1,
            )
            .unwrap();
        coordinator
            .acquire(scope(domain), PrincipalId([7; 16]), NodeId([3; 16]))
            .unwrap();
    }
    coordinator
}
fn query(table: &str) -> BoundQuery {
    BoundQuery {
        sql: format!("SELECT * FROM {table}"),
        values: Vec::new(),
    }
}
fn options(domain: u8) -> ReadOptions {
    ReadOptions::new(scope(domain), 128)
}
async fn note(replica: &Replica<InventoryApp>, text: &str) -> Envelope {
    replica
        .mutate(Mutation {
            entity: None,
            payload: text.as_bytes().to_vec(),
        })
        .unwrap()
        .committed()
        .await
        .unwrap()
}
async fn reserve(replica: &Replica<InventoryApp>, id: u8, quantity: u64) -> StrongRequest {
    replica
        .submit_command_with_arguments(
            CommandHandle([id; 32]),
            RESERVE,
            quantity.to_le_bytes().to_vec(),
            [9; 32],
        )
        .unwrap()
        .committed()
        .await
        .unwrap()
}
/// Domains in one file share the file writer and its groups, but keep their
/// own revisions and author cursors.
#[tokio::test]
async fn shared_file_domains_share_groups_with_independent_revisions_and_author_cursors() {
    let dir = TempDir::new().unwrap();
    let one_file = RuntimeOptions {
        files: 1,
        ..RuntimeOptions::default()
    };
    let open = async |dir: &TempDir| {
        Runtime::open_with_options(dir.path().join("3.db"), installs(3), one_file)
            .await
            .unwrap()
    };
    let runtime = open(&dir).await;
    let a = runtime.domain(scope(1)).unwrap();
    let b = runtime.domain(scope(2)).unwrap();
    let (entered, paused) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    fault(&a, worker::Fault::Pause(entered, resumed)).await;
    let mutation = |text: &str| Mutation {
        entity: None,
        payload: text.as_bytes().to_vec(),
    };
    let receipt = a.mutate(mutation("paused")).unwrap();
    paused.await.unwrap();
    // The writer holds the file's only write transaction; b's write waits for
    // the next group together with a's second one.
    let mut independent = b.mutate(mutation("independent")).unwrap();
    let second = a.mutate(mutation("second")).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(independent.result.try_recv().is_err());
    assert_eq!(b.inner.revisions.borrow().get(), 0);
    resume.send(()).unwrap();
    let first = receipt.committed().await.unwrap();
    let independent = independent.committed().await.unwrap();
    let second = second.committed().await.unwrap();
    assert_eq!(
        [
            first.id.sequence,
            independent.id.sequence,
            second.id.sequence
        ]
        .map(|s| s.get()),
        [1, 1, 2]
    );
    assert_ne!(first.scoped_id(), independent.scoped_id());
    assert_eq!(a.query().await.unwrap().revision.get(), 2);
    assert_eq!(b.query().await.unwrap().revision.get(), 1);
    assert!(matches!(
        a.mutate_scoped(
            MutationHandle([1; 32]),
            Some(independent.scoped_id()),
            b"bad".to_vec(),
            scope(1)
        ),
        Err(Error::Protocol(ProtocolError::ForeignScope))
    ));
    runtime.close().await.unwrap();
    let runtime = open(&dir).await;
    for (domain, revision) in [(1, 2), (2, 1)] {
        assert_eq!(
            runtime
                .domain(scope(domain))
                .unwrap()
                .query()
                .await
                .unwrap()
                .revision
                .get(),
            revision
        );
    }
    runtime.close().await.unwrap();
}
#[tokio::test]
async fn shared_transactions_rollback_retry_and_cancel_waiters_with_stable_identity() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let handle = MutationHandle([8; 32]);
    fault(&replica, worker::Fault::FailBeforeCommit).await;
    let failed = replica
        .mutate_with_handle(
            handle,
            Mutation {
                entity: None,
                payload: b"note".to_vec(),
            },
        )
        .unwrap()
        .committed()
        .await;
    assert!(matches!(failed, Err(Error::InjectedFailure)));
    assert_eq!(replica.query().await.unwrap().revision.get(), 0);
    assert!(replica.lookup_mutation(handle).await.unwrap().is_none());
    fault(&replica, worker::Fault::RetryBeforeCommit(2)).await;
    let receipt = replica
        .mutate_with_handle(
            handle,
            Mutation {
                entity: None,
                payload: b"note".to_vec(),
            },
        )
        .unwrap();
    drop(receipt);
    let committed = replica.query().await.unwrap();
    assert_eq!(committed.rows.len(), 1);
    let progress = replica
        .track_mutation(handle, &[NodeId([4; 16])])
        .await
        .unwrap();
    assert_eq!(progress.committed.unwrap().operation.sequence.get(), 1);
    assert!(!progress.peers[0].received);
    runtime.close().await.unwrap();
    let runtime = open(&dir, 3).await;
    assert!(
        runtime
            .domain(scope(1))
            .unwrap()
            .lookup_mutation(handle)
            .await
            .unwrap()
            .is_some()
    );
    runtime.close().await.unwrap();
}
#[tokio::test]
async fn reservations_order_predecessors_record_rejections_and_preserve_drafts() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let coordinator = coordinator();
    let draft = note(&replica, "before cut").await;
    let first = reserve(&replica, 1, 7).await;
    assert_eq!(reserve(&replica, 1, 7).await, first);
    assert!(matches!(
        replica
            .submit_command_with_arguments(
                CommandHandle([1; 32]),
                RESERVE,
                8u64.to_le_bytes().to_vec(),
                [9; 32]
            )
            .unwrap()
            .committed()
            .await,
        Err(Error::Conflict)
    ));
    reserve(&replica, 2, 4).await;
    replica
        .mutate(Mutation {
            entity: Some(draft.id),
            payload: b"after cut".to_vec(),
        })
        .unwrap()
        .committed()
        .await
        .unwrap();
    let (left, right) = tokio::join!(
        replica.execute_pending(&coordinator),
        replica.execute_pending(&coordinator)
    );
    left.unwrap();
    right.unwrap();
    let publications = replica.confirmed_publications().await.unwrap();
    assert_eq!(publications.len(), 2);
    let accepted = Reservation::decode(&publications[0].package.rows).unwrap();
    let rejected = Reservation::decode(&publications[1].package.rows).unwrap();
    assert!(accepted.accepted);
    assert!(!rejected.accepted);
    assert_eq!(rejected.remaining, 3);
    assert_eq!(rejected.jobs.len(), 1);
    assert_eq!(replica.query().await.unwrap().rows[0].1, "after cut");
    let tracked = replica
        .track_command(CommandHandle([2; 32]), &[NodeId([4; 16])])
        .await
        .unwrap();
    assert_eq!(tracked.state, Some(CommandState::ConfirmedLocally));
    assert!(
        !Reservation::decode(&tracked.outcome.unwrap())
            .unwrap()
            .accepted
    );
    runtime.close().await.unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    assert_eq!(
        replica.read(query("stock"), options(1)).await.unwrap().rows[0][0],
        SqlValue::Integer(3)
    );
    assert_eq!(
        replica
            .read(query("jobs"), options(1))
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    runtime.close().await.unwrap();
}
#[tokio::test]
async fn result_apply_rolls_back_projection_jobs_outbox_and_revision_together() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let coordinator = coordinator();
    let request = reserve(&replica, 1, 2).await;
    let package = match replica
        .strong_request(strong::Request::Prepare(
            Box::new(request),
            coordinator.head(scope(1)).unwrap(),
        ))
        .await
        .unwrap()
    {
        strong::Response::Package(package) => package,
        _ => panic!("package"),
    };
    let receipt = coordinator
        .confirm(package.confirmation().unwrap())
        .unwrap();
    replica
        .strong_request(strong::Request::StoreResult(
            package.encode().unwrap(),
            receipt.encode(),
            strong::Verdict::Deferred,
        ))
        .await
        .unwrap();
    replica
        .strong_request(strong::Request::Verify(receipt.confirmation.result))
        .await
        .unwrap();
    fault(&replica, worker::Fault::FailBeforeCommit).await;
    assert!(matches!(
        replica.strong_request(strong::Request::Apply).await,
        Err(Error::InjectedFailure)
    ));
    assert_eq!(replica.query().await.unwrap().revision.get(), 0);
    assert_eq!(
        replica.read(query("stock"), options(1)).await.unwrap().rows[0][0],
        SqlValue::Integer(10)
    );
    assert!(
        replica
            .read(query("jobs"), options(1))
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    runtime.close().await.unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    fault(&replica, worker::Fault::RetryBeforeCommit(2)).await;
    replica.reconcile_strong(&coordinator).await.unwrap();
    assert_eq!(replica.query().await.unwrap().revision.get(), 1);
    assert_eq!(
        replica
            .read(query("jobs"), options(1))
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    runtime.close().await.unwrap();
}
#[tokio::test]
async fn newer_head_reordered_results_and_missing_inputs_have_precise_read_evidence() {
    let dir = TempDir::new().unwrap();
    let source = open(&dir, 3).await;
    let target = open(&dir, 4).await;
    let a = source.domain(scope(1)).unwrap();
    let b = target.domain(scope(1)).unwrap();
    let coordinator = coordinator();
    let draft = note(&a, "required").await;
    for (id, amount) in [(1, 3), (2, 2)] {
        reserve(&a, id, amount).await;
        a.execute_pending(&coordinator).await.unwrap();
    }
    assert!(matches!(
        b.read_strong(query("stock"), options(1), &coordinator)
            .await
            .unwrap(),
        StrongRead::Pending {
            reason: PendingReason::MissingConfirmedPackage,
            ..
        }
    ));
    let mut results = a.confirmed_publications().await.unwrap();
    results.reverse();
    for result in results {
        b.receive_result(
            result.package.encode().unwrap(),
            result.receipt.encode(),
            &coordinator,
        )
        .await
        .unwrap();
    }
    assert_eq!(b.query().await.unwrap().strong_head.sequence, 0);
    assert!(matches!(
        b.read_strong(query("stock"), options(1), &coordinator)
            .await
            .unwrap(),
        StrongRead::Pending {
            reason: PendingReason::MissingInput,
            ..
        }
    ));
    b.receive(draft.encode().unwrap()).await.unwrap();
    let pending = b.read(query("drafts"), options(1)).await.unwrap();
    assert_eq!(
        pending.completeness,
        Completeness::Local {
            pending_operations: 1
        }
    );
    b.process_pending().await.unwrap();
    b.reconcile_strong(&coordinator).await.unwrap();
    match b
        .read_strong(query("stock"), options(1), &coordinator)
        .await
        .unwrap()
    {
        StrongRead::Ready(snapshot) => {
            assert_eq!(snapshot.rows[0][0], SqlValue::Integer(5));
            assert!(matches!(
                snapshot.completeness,
                Completeness::CoordinatorHead { sequence: 2, .. }
            ));
        }
        other => panic!("{other:?}"),
    }
    coordinator.set_available(false).unwrap();
    assert!(matches!(
        b.read_strong(query("stock"), options(1), &coordinator)
            .await
            .unwrap(),
        StrongRead::Unavailable
    ));
    source.close().await.unwrap();
    target.close().await.unwrap();
}
#[tokio::test]
async fn reads_are_bound_scoped_bounded_and_observations_coalesce() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let a = runtime.domain(scope(1)).unwrap();
    let b = runtime.domain(scope(2)).unwrap();
    let selected = BoundQuery {
        sql: "SELECT note FROM drafts WHERE note = ? ORDER BY sequence".into(),
        values: vec![SqlValue::Text("selected".into())],
    };
    let mut observer = a.observe_read(selected.clone(), options(1)).unwrap();
    assert!(observer.next().await.unwrap().rows.is_empty());
    for text in ["selected", "other", "selected"] {
        note(&a, text).await;
    }
    let result = observer.next().await.unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.revision.get(), 3);
    note(&b, "other domain").await;
    assert_eq!(
        a.read(selected, options(1)).await.unwrap().revision.get(),
        3
    );
    assert!(matches!(
        a.read(query("drafts"), options(2)).await,
        Err(Error::Protocol(ProtocolError::ForeignScope))
    ));
    for sql in [
        "UPDATE drafts SET note='bad'",
        "CREATE TABLE nope(x)",
        "ATTACH ':memory:' AS other",
        "SELECT * FROM rocky_operations",
        "SELECT * FROM \"rocky_replica\"",
        "SELECT * FROM sqlite_schema",
        "SELECT * FROM drafts; DELETE FROM drafts",
        "WITH x AS (SELECT * FROM drafts) SELECT * FROM x",
        "SELECT * FROM",
        "SELECT load_extension(?)",
        "SELECT \"randomblob\"(1000000)",
        "SELECT * FROM 'sqlite_schema'",
        "SELECT * FROM drafts, drafts",
        "SELECT * FROM drafts --comment",
        "SELECT * FROM drafts /* comment */",
    ] {
        assert!(
            matches!(
                a.read(
                    BoundQuery {
                        sql: sql.into(),
                        values: Vec::new()
                    },
                    options(1)
                )
                .await,
                Err(Error::Rejected(_))
            ),
            "{sql}"
        );
    }
    let page = a
        .read(
            query("drafts"),
            ReadOptions {
                max_rows: 1,
                ..options(1)
            },
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(page.next.is_some());
    runtime.close().await.unwrap();
}
#[tokio::test]
async fn manifest_reopen_is_immutable_and_receiving_data_never_installs_handlers() {
    let mut manifest = inventory_app::manifest();
    let hash = manifest.hash();
    manifest.entities.reverse();
    assert_eq!(manifest.hash(), hash);
    manifest.fields[0].shape.push_str(" changed");
    assert_ne!(manifest.hash(), hash);
    manifest.validate().unwrap();
    manifest.methods[0].id = [0; 32];
    assert_eq!(manifest.validate(), Err(ProtocolError::InvalidManifest));
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let a = runtime.domain(scope(1)).unwrap();
    assert!(
        a.submit_command(CommandHandle([1; 32]), "uninstalled")
            .unwrap()
            .committed()
            .await
            .is_err()
    );
    runtime.close().await.unwrap();
    let mut changed = installs(3);
    changed.pop();
    assert!(matches!(
        Runtime::open(dir.path().join("3.db"), changed).await,
        Err(Error::Incompatible(_))
    ));
    let runtime = open(&dir, 3).await;
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_progress_reads_recover_and_named_peer_evidence_coalesces() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let handle = MutationHandle([55; 32]);
    let mut observer = replica
        .observe_progress(TrackedHandle::Mutation(handle), vec![NodeId([4; 16])])
        .unwrap();
    assert!(matches!(
        observer.next().await.unwrap(),
        TrackingSnapshot::Mutation(MutationProgress {
            committed: None,
            ..
        })
    ));
    // Poll a pending next then cancel it, without timing-based sleeps.
    {
        let mut pending = std::pin::pin!(observer.next());
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    let receipt = replica
        .mutate_with_handle(
            handle,
            Mutation {
                entity: None,
                payload: b"tracked".to_vec(),
            },
        )
        .unwrap();
    let operation = receipt.committed().await.unwrap();
    replica
        .acknowledge(
            NodeId([4; 16]),
            Acknowledgment {
                id: operation.id,
                applied: false,
            },
        )
        .await
        .unwrap();
    replica
        .acknowledge(
            NodeId([4; 16]),
            Acknowledgment {
                id: operation.id,
                applied: true,
            },
        )
        .await
        .unwrap();
    match observer.next().await.unwrap() {
        TrackingSnapshot::Mutation(progress) => {
            assert!(progress.committed.is_some());
            assert!(progress.peers[0].received);
            assert!(progress.peers[0].applied);
        }
        _ => panic!("mutation tracking"),
    }
    let handle = CommandHandle([56; 32]);
    let mut commands = replica
        .observe_progress(TrackedHandle::Command(handle), vec![NodeId([4; 16])])
        .unwrap();
    assert!(matches!(
        commands.next().await.unwrap(),
        TrackingSnapshot::Command(CommandTracking { request: None, .. })
    ));
    drop(
        replica
            .submit_command_with_arguments(handle, RESERVE, 1u64.to_le_bytes().to_vec(), [9; 32])
            .unwrap(),
    );
    let coordinator = coordinator();
    replica.execute_pending(&coordinator).await.unwrap();
    let package = replica
        .confirmed_publications()
        .await
        .unwrap()
        .pop()
        .unwrap();
    replica
        .acknowledge_result(NodeId([4; 16]), package.receipt.confirmation.result, true)
        .await
        .unwrap();
    match commands.next().await.unwrap() {
        TrackingSnapshot::Command(progress) => {
            assert_eq!(progress.state, Some(CommandState::PeerApplied));
            assert!(progress.outcome.is_some());
            assert!(progress.peers[0].applied);
        }
        _ => panic!("command tracking"),
    }
    runtime.close().await.unwrap();
}
#[tokio::test]
async fn caller_supplied_exact_inputs_are_durable_even_before_inputs_arrive() {
    let dir = TempDir::new().unwrap();
    let source = open(&dir, 3).await;
    let target = open(&dir, 4).await;
    let a = source.domain(scope(1)).unwrap();
    let b = target.domain(scope(1)).unwrap();
    let draft = note(&a, "remote input").await;
    let original = reserve(&a, 1, 1).await;
    let invocation = CommandInvocation {
        method: RESERVE.into(),
        version: 1,
        arguments: 2u64.to_le_bytes().to_vec(),
        policy: [9; 32],
        inputs: Some(SealedInputs {
            frontier: original.frontier.clone(),
            digest: original.digest,
        }),
    };
    let request = b
        .invoke_command(CommandHandle([70; 32]), invocation.clone())
        .unwrap()
        .committed()
        .await
        .unwrap();
    assert_eq!(request.frontier, original.frontier);
    assert_eq!(request.digest, original.digest);
    let mut changed = invocation.clone();
    changed.inputs.as_mut().unwrap().digest = [1; 32];
    assert!(matches!(
        b.invoke_command(CommandHandle([70; 32]), changed)
            .unwrap()
            .committed()
            .await,
        Err(Error::Conflict)
    ));
    let coordinator = coordinator();
    coordinator
        .takeover(scope(1), PrincipalId([7; 16]), NodeId([4; 16]), 1)
        .unwrap();
    assert!(matches!(
        b.execute_pending(&coordinator).await,
        Err(Error::StrongPending(PendingReason::MissingInput))
    ));
    b.receive(draft.encode().unwrap()).await.unwrap();
    b.process_pending().await.unwrap();
    b.execute_pending(&coordinator).await.unwrap();
    assert_eq!(
        b.track_command(CommandHandle([70; 32]), &[])
            .await
            .unwrap()
            .state,
        Some(CommandState::ConfirmedLocally)
    );
    source.close().await.unwrap();
    target.close().await.unwrap();
}

#[tokio::test]
async fn dynamic_sql_sees_only_current_authorized_rows_and_strong_corruption_is_rejected() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    note(&replica, "public note").await;
    note(&replica, "private note").await;
    let coordinator = coordinator();
    reserve(&replica, 1, 2).await;
    replica.execute_pending(&coordinator).await.unwrap();
    runtime.close().await.unwrap();
    let mut subset = installs(3);
    subset[0].policy.note_prefix = Some("public".into());
    let runtime = Runtime::open(dir.path().join("3.db"), subset)
        .await
        .unwrap();
    let replica = runtime.domain(scope(1)).unwrap();
    assert_eq!(replica.query().await.unwrap().rows.len(), 1);
    let query = BoundQuery {
        sql: "SELECT COUNT(*) FROM drafts".into(),
        values: Vec::new(),
    };
    assert_eq!(
        replica.read(query, options(1)).await.unwrap().rows,
        vec![vec![SqlValue::Integer(1)]]
    );
    let query = BoundQuery {
        sql: "SELECT note FROM drafts WHERE note = ?".into(),
        values: vec![SqlValue::Text("private note".into())],
    };
    assert!(
        replica
            .read(query, options(1))
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    runtime.close().await.unwrap();
    let db = crate::sqlite::Connection::open(&dir.path().join("3.db")).unwrap();
    let conn = crate::scoped_connection::ScopedConnection::new(
        db,
        Some(&crate::runtime::namespace(scope(1))),
        InventoryApp.schema(),
    );
    conn.execute("UPDATE stock SET remaining=9", ())
        .await
        .unwrap();
    drop(conn);
    assert!(matches!(
        Runtime::open(dir.path().join("3.db"), installs(3)).await,
        Err(Error::Corrupt(_))
    ));
}
/// While the file writer holds its group open, another connection's write
/// is refused as retryable busy and its reads see only committed state.
#[tokio::test]
async fn scoped_busy_writes_and_pending_receipts_are_observable() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let (entered, paused) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    fault(&replica, worker::Fault::Pause(entered, resumed)).await;
    let receipt = replica
        .mutate(Mutation {
            entity: None,
            payload: b"conflicting commit".to_vec(),
        })
        .unwrap();
    paused.await.unwrap();
    let db = crate::sqlite::Connection::open(&dir.path().join("3.db")).unwrap();
    let other = crate::scoped_connection::ScopedConnection::new(
        db,
        Some(&crate::runtime::namespace(scope(1))),
        InventoryApp.schema(),
    );
    let error = Error::from(other.execute("BEGIN IMMEDIATE", ()).await.unwrap_err());
    assert!(storage::retryable(&error));
    assert!(error.is_retryable());
    assert_eq!(
        storage::rows(&other, "SELECT COUNT(*) FROM rocky_operations")
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(0)]]
    );
    drop(other);
    resume.send(()).unwrap();
    let operation = receipt.committed().await.unwrap();
    let target = open(&dir, 4).await;
    let receiving = target.domain(scope(1)).unwrap();
    let mut observer = receiving.observe_read(query("drafts"), options(1)).unwrap();
    let initial = observer.next().await.unwrap();
    receiving
        .receive(operation.encode().unwrap())
        .await
        .unwrap();
    let pending = observer.next().await.unwrap();
    assert_eq!(pending.revision, initial.revision);
    assert_eq!(
        pending.completeness,
        Completeness::Local {
            pending_operations: 1
        }
    );
    receiving.process_pending().await.unwrap();
    assert_eq!(observer.next().await.unwrap().rows.len(), 1);
    runtime.close().await.unwrap();
    target.close().await.unwrap();
}

#[tokio::test]
async fn strong_read_fetches_the_discovered_chain_and_exact_inputs_from_a_replica() {
    let dir = TempDir::new().unwrap();
    let source = open(&dir, 3).await;
    let target = open(&dir, 4).await;
    let a = source.domain(scope(1)).unwrap();
    let b = target.domain(scope(1)).unwrap();
    let coordinator = coordinator();
    note(&a, "required remote note").await;
    for id in [1, 2] {
        reserve(&a, id, 2).await;
        a.execute_pending(&coordinator).await.unwrap();
    }
    match b
        .read_strong_from(query("stock"), options(1), &coordinator, &a)
        .await
        .unwrap()
    {
        StrongRead::Ready(snapshot) => {
            assert_eq!(snapshot.rows[0][0], SqlValue::Integer(6));
            assert_eq!(snapshot.strong_head.sequence, 2);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(b.query().await.unwrap().rows[0].1, "required remote note");
    source.close().await.unwrap();
    target.close().await.unwrap();
}
/// Version 2 of the reservation is typed; version 1 keeps its retained
/// bytes; a version outside the horizon is refused, not translated.
#[tokio::test]
async fn typed_reservation_versions_share_one_head_and_refuse_other_versions() {
    use inventory_app::{Remaining, Reserve, Shortfall};
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let coordinator = coordinator();
    let invoke = async |id: u8, version: u32, arguments: Vec<u8>| {
        replica
            .invoke_command(
                CommandHandle([id; 32]),
                CommandInvocation {
                    method: RESERVE.into(),
                    version,
                    arguments,
                    policy: [9; 32],
                    inputs: None,
                },
            )
            .unwrap()
            .committed()
            .await
    };
    // Retained version 1 bytes still execute beside typed version 2.
    reserve(&replica, 1, 4).await;
    let typed = inventory_app::reserve(5).unwrap();
    let accepted = invoke(2, 2, typed.clone()).await.unwrap();
    let rejected = invoke(3, 2, inventory_app::reserve(2).unwrap())
        .await
        .unwrap();
    assert_eq!((accepted.version, rejected.version), (2, 2));
    replica.execute_pending(&coordinator).await.unwrap();
    let outcomes: Vec<_> = replica
        .confirmed_publications()
        .await
        .unwrap()
        .iter()
        .map(|publication| {
            Reservation::decode(&publication.package.rows)
                .unwrap()
                .outcome()
        })
        .collect();
    // A rejection is a confirmed result at its own sequence, not an error.
    assert_eq!(
        outcomes,
        [Ok(Remaining(6)), Ok(Remaining(1)), Err(Shortfall(1))]
    );
    assert_eq!(coordinator.head(scope(1)).unwrap().sequence, 3);

    // The typed arguments bind their descriptor: other bytes are refused.
    let descriptor = inventory_app::commands().get(RESERVE, 2).unwrap();
    assert_eq!(Reserve::decode_arguments(descriptor, &typed).unwrap().0, 5);
    for (id, version, arguments) in [
        (4, 2, 5u64.to_le_bytes().to_vec()),
        (5, 1, typed.clone()),
        (6, 2, inventory_app::reserve(0).unwrap()),
    ] {
        assert!(invoke(id, version, arguments).await.is_err());
    }
    let unsupported = invoke(7, 3, typed).await.unwrap_err();
    assert!(
        unsupported
            .to_string()
            .contains("outside the support horizon"),
        "{unsupported}"
    );
    replica.execute_pending(&coordinator).await.unwrap();
    assert_eq!(coordinator.head(scope(1)).unwrap().sequence, 3);
    runtime.close().await.unwrap();
}
