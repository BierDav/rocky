//! A SQL writer beside a replica that receives a long catch-up.
use super::*;
use crate::network::link::Link;
use std::sync::atomic::{AtomicBool, Ordering};

/// Creates `rows` items, one SQL transaction each.
async fn create(replica: &EncryptedReplica<TypedRows>, both: &[ResourceScope], rows: usize) {
    let session = sql(replica, both).await;
    for key in 1..=rows {
        insert(
            &session,
            "INSERT INTO items VALUES(?,'item',0)",
            vec![text(&format!("k{key}"))],
        )
        .await
        .unwrap();
    }
}
/// Ticks every item, one SQL transaction each.
async fn tick(replica: &EncryptedReplica<TypedRows>, both: &[ResourceScope], rows: usize) {
    let session = sql(replica, both).await;
    for key in 1..=rows {
        let ticked = session
            .run(
                "UPDATE items SET done=1 WHERE key=?",
                vec![text(&format!("k{key}"))],
            )
            .await
            .unwrap();
        assert_eq!(ticked.changes, 1);
    }
}
/// `source` serves one scope's catch-up to `target` over loopback TCP, on
/// its own thread and runtime as a remote peer would.
async fn pull(
    source: &EncryptedReplica<TypedRows>,
    target: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
) -> crate::network::CatchUp {
    let mut total = crate::network::CatchUp::default();
    for _ in 0..16 {
        let report = pull_once(source, target, scope).await;
        assert!(!report.behind);
        total.frames += report.frames;
        total.pages += report.pages;
        total.bytes += report.bytes;
        total.lag = report.lag;
        if !report.exhausted {
            return total;
        }
        assert!(report.frames > 0, "exhausted session made no progress");
    }
    panic!("catch-up did not finish within 16 sessions")
}
async fn pull_once(
    source: &EncryptedReplica<TypedRows>,
    target: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
) -> crate::network::CatchUp {
    let config = |replica: &EncryptedReplica<TypedRows>| {
        let replica = replica.clone();
        async move {
            let inner = replica.inner.lock(crate::replica::Class::Background).await;
            (inner.config.node.clone(), inner.config.credentials.clone())
        }
    };
    let (source_node, source_credentials) = config(source).await;
    let (target_node, target_credentials) = config(target).await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = {
        let source = source.clone();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    listener.set_nonblocking(true).unwrap();
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    let (stream, _) = listener.accept().await.unwrap();
                    stream.set_nodelay(true).unwrap();
                    let mut link =
                        Link::open(stream, &source, source_node, source_credentials, None)
                            .await
                            .unwrap();
                    link.ready(false).await.unwrap();
                    let (served, mut incoming) = tokio::sync::mpsc::channel(1);
                    let driven = rocky_platform::task::spawn(link.drive(served));
                    let mut session = incoming.recv().await.unwrap();
                    session.serve(&source).await.unwrap();
                    session.close().await.unwrap();
                    driven
                        .await
                        .unwrap()
                        .expect("the client ends the link once its channel closed");
                })
        })
    };
    let stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.set_nodelay(true).unwrap();
    let mut link = Link::open(
        stream,
        target,
        target_node,
        target_credentials,
        Some(source_node_key(source).await),
    )
    .await
    .unwrap();
    link.ready(false).await.unwrap();
    let handle = link.channels.handle.clone();
    let driven = rocky_platform::task::spawn(link.drive(tokio::sync::mpsc::channel(1).0));
    let mut session = handle.open().await.unwrap();
    let report = session
        .catch_up(target, scope, MAX_SECURE_FRAME / 2)
        .await
        .unwrap();
    session.close().await.unwrap();
    driven.abort();
    rocky_platform::task::spawn_blocking(move || server.join().unwrap())
        .await
        .unwrap();
    report
}
async fn source_node_key(replica: &EncryptedReplica<TypedRows>) -> PublicKey {
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .node
        .public_key()
}

/// A creator authors `operations / 2` rows and an updater ticks each of
/// them, so each update depends on another log's frame. The updater's log
/// sorts first and arrives first: its frames wait until the creations they
/// name arrive, the shape of a device catching up on two others' edits.
/// The reader receives every operation while a SQL writer commits one row
/// per transaction.
async fn catch_up_beside_a_writer(operations: usize) -> usize {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let (creator, updater) = loop {
        let (creator, updater) = (PrivateKey::new(), PrivateKey::new());
        if updater.public_key() < creator.public_key() {
            break (creator, updater);
        }
    };
    let rows = operations / 2;
    let source = t
        .open(&dir.path().join("creator.db"), &creator, t.full(), &both)
        .await;
    create(&source, &both, rows).await;
    let editor = t
        .open(&dir.path().join("updater.db"), &updater, t.full(), &both)
        .await;
    assert_eq!(pull(&source, &editor, t.items).await.frames, rows);
    tick(&editor, &both, rows).await;
    let reader = t
        .open(
            &dir.path().join("reader.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let session = sql(&reader, &both).await;
    let mut frames = reader.observe_frames().await;
    let done = AtomicBool::new(false);
    let completed = tokio::sync::Notify::new();
    let catch_up = async {
        let report = pull(&editor, &reader, t.items).await;
        assert_eq!(report.frames, operations);
        assert_eq!(report.lag, 0);
        done.store(true, Ordering::SeqCst);
        completed.notify_one();
    };
    let writer = async {
        let mut writes = 0;
        let mut incoming = 0;
        while !done.load(Ordering::SeqCst) {
            tokio::select! {
                () = completed.notified() => break,
                changed = frames.changed() => changed.unwrap(),
            }
            incoming += 1;
            if incoming % RECEIPTS_PER_QUEUED_WRITE != 0 {
                continue;
            }
            insert(
                &session,
                "INSERT INTO salaries VALUES(?,1)",
                vec![text(&format!("w{writes}"))],
            )
            .await
            .unwrap();
            writes += 1;
            rocky_platform::task::yield_now().await;
        }
        writes
    };
    let ((), writes) = tokio::join!(catch_up, writer);
    let applied = select(&session, "SELECT count(*),sum(done) FROM items").await;
    assert_eq!(
        applied,
        vec![vec![
            SqlValue::Integer(rows as i64),
            SqlValue::Integer(rows as i64)
        ]],
        "every operation applied exactly once: one row per key, each ticked"
    );
    assert_eq!(
        rows_of(&reader, "SELECT sum(applied) FROM secure_scope_state").await,
        vec![vec![SqlValue::Integer((operations + writes) as i64)]]
    );
    writes
}

/// Receive pages hold at most 32 frames; a write is queued directly after every
/// second receipt, including while a pending apply runs.
const RECEIPTS_PER_QUEUED_WRITE: usize = 2;

/// Six receive pages of operations, enough to retain concurrent writes and
/// cross-log dependencies.
const SIX_RECEIVE_PAGES_OF_OPERATIONS: usize = 6 * 32;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_writer_commits_during_a_catch_up() {
    assert!(catch_up_beside_a_writer(SIX_RECEIVE_PAGES_OF_OPERATIONS).await > 0);
}

/// Lets each later receipt or open commit at most `transactions` apply
/// transactions before the injected fault; `None` lifts the limit.
fn limit_apply_transactions(transactions: Option<usize>) {
    crate::APPLY_BUDGET.with(|budget| budget.set(transactions));
}

/// A catch-up interrupted between two apply transactions, at any of them,
/// resumes when the replica opens again; every operation applies once.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_interrupted_catch_up_resumes_and_applies_every_operation_once() {
    interrupted_catch_up(false).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn retrying_a_receipt_resumes_an_interrupted_catch_up_without_reopening() {
    interrupted_catch_up(true).await;
}

async fn interrupted_catch_up(retry_receipt: bool) {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let (creator, updater) = loop {
        let (creator, updater) = (PrivateKey::new(), PrivateKey::new());
        if updater.public_key() < creator.public_key() {
            break (creator, updater);
        }
    };
    let rows = 4;
    let source = t
        .open(&dir.path().join("creator.db"), &creator, t.full(), &both)
        .await;
    create(&source, &both, rows).await;
    let editor = t
        .open(&dir.path().join("updater.db"), &updater, t.full(), &both)
        .await;
    assert_eq!(pull(&source, &editor, t.items).await.frames, rows);
    tick(&editor, &both, rows).await;
    let mut updater_ticks_then_creations_in_log_order = crate::tests::stored_frames(
        &editor,
        "SELECT header,body FROM secure_operations ORDER BY author,sequence",
    )
    .await;
    assert_eq!(updater_ticks_then_creations_in_log_order.len(), 2 * rows);
    let first = updater_ticks_then_creations_in_log_order.remove(rows);
    assert_eq!(first.operation().unwrap().header.seq_num, 0);

    let reader_node = PrivateKey::new();
    let path = dir.path().join("reader.db");
    let reader_credentials = t.credentials(&reader_node, t.full());
    let open = || {
        EncryptedReplica::open(
            &path,
            t.app(),
            SecureConfig {
                publisher: t.publisher.public_key(),
                manifest: t.manifest.clone(),
                revocations: t.policy.clone(),
                node: reader_node.clone(),
                credentials: reader_credentials.clone(),
                keys: both.iter().map(|scope| t.key(*scope)).collect(),
            },
        )
    };
    let reader = open().await.unwrap();
    reader
        .install_tables(
            t.schema.clone(),
            t.domain,
            [
                ("items".to_string(), t.items.encryption),
                ("salaries".to_string(), t.salaries.encryption),
            ]
            .into(),
        )
        .await
        .unwrap();
    for status in reader
        .receive_all(updater_ticks_then_creations_in_log_order)
        .await
    {
        assert_eq!(
            status.unwrap(),
            ReceiveStatus::PendingDependencies,
            "without the first creation every other operation waits"
        );
    }
    limit_apply_transactions(Some(0));
    assert!(
        reader.receive(first.clone()).await.is_err(),
        "receipt commits custody, then the injected fault stops before application"
    );
    let mut current = retry_receipt.then_some(reader);
    let mut interruptions = 1;
    loop {
        limit_apply_transactions(Some(1));
        let resumed = if let Some(reader) = current.take() {
            match reader.receive(first.clone()).await {
                Ok(ReceiveStatus::Duplicate) => Ok(reader),
                Ok(status) => panic!("retried receipt: {status:?}"),
                Err(error) => {
                    current = Some(reader);
                    Err(error)
                }
            }
        } else {
            open().await
        };
        match resumed {
            Err(Error::InjectedFailure) => interruptions += 1,
            Err(error) => panic!("{error}"),
            Ok(reader) => {
                limit_apply_transactions(None);
                let rows = rows as i64;
                assert_eq!(
                    rows_of(&reader, "SELECT count(*),sum(done) FROM items").await,
                    vec![vec![SqlValue::Integer(rows), SqlValue::Integer(rows)]]
                );
                assert_eq!(
                    rows_of(
                        &reader,
                        "SELECT count(*) FROM secure_operations WHERE status<>'applied'"
                    )
                    .await,
                    vec![vec![SqlValue::Integer(0)]]
                );
                assert_eq!(
                    rows_of(&reader, "SELECT sum(applied) FROM secure_scope_state").await,
                    vec![vec![SqlValue::Integer(2 * rows)]],
                    "each application counts once in its scope"
                );
                break;
            }
        }
    }
    assert!(
        interruptions >= 7,
        "the eight operations take separate application transactions: {interruptions} interruptions"
    );
}
/// Regression: a SQL commit between every settle step restarted the pending
/// pass, so a receive waiting for it never returned while writes continued.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn sql_commits_between_pending_steps_do_not_restart_the_pass() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let creator = PrivateKey::new();
    let source = t
        .open(&dir.path().join("creator.db"), &creator, t.full(), &both)
        .await;
    create(&source, &both, 3).await;
    let frames = crate::tests::stored_frames(
        &source,
        "SELECT header,body FROM secure_operations ORDER BY author,sequence",
    )
    .await;
    let reader = t
        .open(
            &dir.path().join("reader.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    reader
        .stage_bundle_before_apply_for_test(&frames)
        .await
        .unwrap();
    let session = sql(&reader, &both).await;
    let mut steps = 0;
    while reader.pending_step_for_test().await.unwrap() {
        steps += 1;
        assert!(steps < 100, "the pending pass never finished");
        insert(
            &session,
            "INSERT INTO salaries VALUES(?,1)",
            vec![text(&format!("w{steps}"))],
        )
        .await
        .unwrap();
    }
    assert_eq!(
        rows_of(
            &reader,
            "SELECT count(*) FROM secure_operations WHERE status<>'applied'"
        )
        .await,
        vec![vec![SqlValue::Integer(0)]]
    );
}
async fn rows_of(replica: &EncryptedReplica<TypedRows>, sql: &str) -> Vec<Vec<SqlValue>> {
    crate::storage::rows(
        &replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        sql,
    )
    .await
    .unwrap()
}
