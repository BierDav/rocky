use super::*;
use crate::tests::fault;
use inventory_app::{InventoryApp, Policy};
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
async fn manifest_reopen_is_immutable() {
    let mut manifest = inventory_app::manifest();
    let hash = manifest.hash();
    manifest.entities.reverse();
    assert_eq!(manifest.hash(), hash);
    manifest.fields[0].shape.push_str(" changed");
    assert_ne!(manifest.hash(), hash);
    manifest.validate().unwrap();
    manifest.operations[0].id = [0; 32];
    assert_eq!(manifest.validate(), Err(ProtocolError::InvalidManifest));
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
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
        .observe_progress(handle, vec![NodeId([4; 16])])
        .unwrap();
    assert!(matches!(
        observer.next().await.unwrap(),
        MutationProgress {
            committed: None,
            ..
        }
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
    let progress = observer.next().await.unwrap();
    assert!(progress.committed.is_some());
    assert!(progress.peers[0].received);
    assert!(progress.peers[0].applied);
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn dynamic_sql_sees_only_current_authorized_rows() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, 3).await;
    let replica = runtime.domain(scope(1)).unwrap();
    note(&replica, "public note").await;
    note(&replica, "private note").await;
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
