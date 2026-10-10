//! A SQL writer beside a replica that receives a long catch-up. See
//! `docs/runtime/sql-driver.md#the-writer`.
use super::*;
use crate::network::NativeSession;
use rocky_core::{RowChange, RowIdentity, RowWrite};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The default `transaction_timeout`: from BEGIN to the return of COMMIT.
const WRITE_LIMIT: Duration = Duration::from_secs(5);

/// The `items` author key a node signs its rows with.
fn author(node: &PrivateKey) -> PublicKey {
    PrivateKey::from_bytes(&rocky_core::commitment("items", node.as_bytes())).public_key()
}
/// The author number `Typed::open` assigns `node` for `items`.
fn number(node: &PrivateKey) -> u32 {
    let material = rocky_core::commitment("items", node.as_bytes());
    (u32::from_le_bytes(material[..4].try_into().unwrap()) & 0x7fff_ffff).max(1)
}
fn values(key: usize, done: i64, id: i64) -> Vec<SqlValue> {
    vec![
        text(&format!("k{key}")),
        text("item"),
        SqlValue::Integer(done),
        SqlValue::Integer(id),
        SqlValue::Integer(0),
    ]
}
fn write(t: &Typed, before: Option<&[SqlValue]>, after: &[SqlValue]) -> RowWrite {
    let app = t.app();
    let table = app
        .tables()
        .find(|table| table.sql_name == "items")
        .unwrap();
    RowWrite::changed(RowChange {
        scope: t.items,
        row: app.row_id(t.domain.domain, table, after).unwrap(),
        before: before.map(|values| app.fields(table, values).unwrap()),
        after: Some(app.fields(table, after).unwrap()),
    })
}
/// Commits `writes` as single-row bundles, 64 at a time so they share
/// group commits and flushes.
async fn author_all(replica: &EncryptedReplica<TypedRows>, writes: Vec<RowWrite>, tag: &str) {
    let mut writes = writes.into_iter().enumerate().peekable();
    while writes.peek().is_some() {
        let guard = replica.inner.lock().await;
        let mut tasks = tokio::task::JoinSet::new();
        for (index, write) in writes.by_ref().take(64) {
            let replica = replica.clone();
            let handle = rocky_core::commitment(tag, &index.to_be_bytes());
            let mut request =
                Box::pin(async move { replica.author_bundle(handle, vec![write], vec![]).await });
            // Enqueue in birth order while the leader cannot take the lock.
            std::future::poll_fn(|cx| {
                assert!(request.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            tasks.spawn(request);
        }
        drop(guard);
        while let Some(result) = tasks.join_next().await {
            result.unwrap().unwrap();
        }
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
            let inner = replica.inner.lock().await;
            (inner.config.node.clone(), inner.config.credentials.clone())
        }
    };
    let (source_node, source_credentials) = config(source).await;
    let (target_node, target_credentials) = config(target).await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let target_peer = target_node.public_key();
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
                    let mut session = NativeSession::accept(
                        stream,
                        source_node,
                        source_credentials,
                        crate::tests::unrevoked(),
                        |peer| *peer == target_peer,
                        MAX_SECURE_BYTES,
                    )
                    .await
                    .unwrap();
                    session.serve(&source).await.unwrap();
                })
        })
    };
    let stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.set_nodelay(true).unwrap();
    let mut session = NativeSession::authenticate(
        stream,
        target_node,
        target_credentials,
        crate::tests::unrevoked(),
        source_node_key(source).await,
        MAX_SECURE_BYTES,
    )
    .await
    .unwrap();
    let report = session
        .catch_up(target, scope, MAX_SECURE_FRAME / 2)
        .await
        .unwrap();
    session.close().await.unwrap();
    tokio::task::spawn_blocking(move || server.join().unwrap())
        .await
        .unwrap();
    report
}
async fn source_node_key(replica: &EncryptedReplica<TypedRows>) -> PublicKey {
    replica.inner.lock().await.config.node.public_key()
}

struct Measured {
    catch_up: Duration,
    /// BEGIN to the return of COMMIT, the longest of the writer's commits.
    longest_write: Duration,
    writes: usize,
    /// The longest a probe waited for the replica.
    longest_hold: Duration,
}

/// A creator authors `operations / 2` rows and an updater ticks each of
/// them, so each update depends on another log's frame. The updater's log
/// sorts first and arrives first: its frames wait until the creations they
/// name arrive, the shape of a device catching up on two others' edits.
/// The reader receives every operation while a SQL writer commits one row
/// per transaction and a probe measures how long the replica is held.
async fn catch_up_beside_a_writer(operations: usize) -> Measured {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let (creator, updater) = loop {
        let (creator, updater) = (PrivateKey::new(), PrivateKey::new());
        if author(&updater) < author(&creator) {
            break (creator, updater);
        }
    };
    let rows = operations / 2;
    let source = t
        .open(&dir.path().join("creator.db"), &creator, t.full(), &both)
        .await;
    let created: Vec<_> = (1..=rows)
        .map(|key| {
            let id = RowIdentity::new(number(&creator), key as u32)
                .unwrap()
                .integer();
            values(key, 0, id)
        })
        .collect();
    let started = Instant::now();
    author_all(
        &source,
        created.iter().map(|after| write(&t, None, after)).collect(),
        "create",
    )
    .await;
    let editor = t
        .open(&dir.path().join("updater.db"), &updater, t.full(), &both)
        .await;
    assert_eq!(pull(&source, &editor, t.items).await.frames, rows);
    author_all(
        &editor,
        created
            .iter()
            .map(|before| {
                let mut after = before.clone();
                after[2] = SqlValue::Integer(1);
                write(&t, Some(before), &after)
            })
            .collect(),
        "tick",
    )
    .await;
    println!(
        "P03 authored operations={operations} ms={}",
        started.elapsed().as_millis()
    );

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
    let started = Instant::now();
    let catch_up = async {
        let report = pull(&editor, &reader, t.items).await;
        assert_eq!(report.frames, operations);
        assert_eq!(report.lag, 0);
        let elapsed = started.elapsed();
        done.store(true, Ordering::SeqCst);
        completed.notify_one();
        elapsed
    };
    let writer = async {
        let (mut longest, mut writes) = (Duration::ZERO, 0);
        let mut incoming = 0;
        while !done.load(Ordering::SeqCst) {
            tokio::select! {
                () = completed.notified() => break,
                changed = frames.changed() => changed.unwrap(),
            }
            // Receive pages hold at most 32 frames. Queue BEGIN directly
            // after every second receipt, including while pending apply runs.
            incoming += 1;
            if incoming % 2 != 0 {
                continue;
            }
            let asked = Instant::now();
            insert(
                &session,
                "INSERT INTO salaries VALUES(?,1)",
                vec![text(&format!("w{writes}"))],
            )
            .await
            .unwrap();
            longest = longest.max(asked.elapsed());
            writes += 1;
            tokio::task::yield_now().await;
        }
        (longest, writes)
    };
    let probe = async {
        let mut longest = Duration::ZERO;
        while !done.load(Ordering::SeqCst) {
            let asked = Instant::now();
            drop(reader.inner.lock().await);
            longest = longest.max(asked.elapsed());
            tokio::task::yield_now().await;
        }
        longest
    };
    let (catch_up, (longest_write, writes), longest_hold) = tokio::join!(catch_up, writer, probe);
    // Every operation applied exactly once: one row per key, each ticked.
    let applied = select(&session, "SELECT count(*),sum(done) FROM items").await;
    assert_eq!(
        applied,
        vec![vec![
            SqlValue::Integer(rows as i64),
            SqlValue::Integer(rows as i64)
        ]]
    );
    assert_eq!(
        rows_of(&reader, "SELECT sum(applied) FROM secure_scope_state").await,
        vec![vec![SqlValue::Integer((operations + writes) as i64)]]
    );
    let measured = Measured {
        catch_up,
        longest_write,
        writes,
        longest_hold,
    };
    println!(
        "P03 catch_up operations={operations} catch_up_ms={} writes={} longest_write_ms={} longest_hold_ms={}",
        measured.catch_up.as_millis(),
        measured.writes,
        measured.longest_write.as_millis(),
        measured.longest_hold.as_millis(),
    );
    measured
}

#[tokio::test]
async fn a_writer_commits_within_the_write_limit_during_a_catch_up() {
    let measured = catch_up_beside_a_writer(640).await;
    assert!(measured.writes > 0);
    assert!(measured.longest_write < WRITE_LIMIT);
}

#[tokio::test]
#[ignore = "phase 3 measurement; run with --ignored --nocapture"]
async fn soak_a_writer_commits_within_the_write_limit_during_a_10000_operation_catch_up() {
    // `ROCKY_CATCH_UP_OPERATIONS` measures another size.
    let operations =
        std::env::var("ROCKY_CATCH_UP_OPERATIONS").map_or(10_000, |count| count.parse().unwrap());
    let measured = catch_up_beside_a_writer(operations).await;
    assert!(measured.writes > 0);
    assert!(measured.longest_write < WRITE_LIMIT);
}

/// A catch-up interrupted between two apply transactions, at any of them,
/// resumes when the replica opens again; every operation applies once.
#[tokio::test]
async fn an_interrupted_catch_up_resumes_and_applies_every_operation_once() {
    interrupted_catch_up(false).await;
}

#[tokio::test]
async fn retrying_a_receipt_resumes_an_interrupted_catch_up_without_reopening() {
    interrupted_catch_up(true).await;
}

async fn interrupted_catch_up(retry_receipt: bool) {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let (creator, updater) = loop {
        let (creator, updater) = (PrivateKey::new(), PrivateKey::new());
        if author(&updater) < author(&creator) {
            break (creator, updater);
        }
    };
    let rows = 160;
    let source = t
        .open(&dir.path().join("creator.db"), &creator, t.full(), &both)
        .await;
    let created: Vec<_> = (1..=rows)
        .map(|key| {
            let id = RowIdentity::new(number(&creator), key as u32)
                .unwrap()
                .integer();
            values(key, 0, id)
        })
        .collect();
    author_all(
        &source,
        created.iter().map(|after| write(&t, None, after)).collect(),
        "create",
    )
    .await;
    let editor = t
        .open(&dir.path().join("updater.db"), &updater, t.full(), &both)
        .await;
    assert_eq!(pull(&source, &editor, t.items).await.frames, rows);
    author_all(
        &editor,
        created
            .iter()
            .map(|before| {
                let mut after = before.clone();
                after[2] = SqlValue::Integer(1);
                write(&t, Some(before), &after)
            })
            .collect(),
        "tick",
    )
    .await;
    // Every operation in log order, the updater's ticks first.
    let mut frames: Vec<_> = rows_of(
        &editor,
        "SELECT header,body FROM secure_operations ORDER BY author,sequence",
    )
    .await
    .into_iter()
    .map(|record| match record.as_slice() {
        [SqlValue::Blob(header), SqlValue::Blob(body)] => EncryptedFrame {
            header: header.clone(),
            body: body.clone(),
        },
        _ => panic!("stored operation"),
    })
    .collect();
    assert_eq!(frames.len(), 2 * rows);
    // Without the first creation, every other operation waits.
    let first = frames.remove(rows);
    assert_eq!(first.operation().unwrap().header.seq_num, 0);

    let reader_node = PrivateKey::new();
    let path = dir.path().join("reader.db");
    let open = || {
        EncryptedReplica::open(
            &path,
            t.app(),
            SecureConfig {
                publisher: t.publisher.public_key(),
                manifest: t.manifest.clone(),
                revocations: t.policy.clone(),
                node: reader_node.clone(),
                credentials: t.credentials(&reader_node, t.full()),
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
    for page in frames.chunks(32) {
        for status in reader.receive_all(page.to_vec()).await {
            assert_eq!(status.unwrap(), ReceiveStatus::PendingDependencies);
        }
    }
    // The first creation's group applies what it can; the pass stops
    // before the next transaction.
    crate::APPLY_BUDGET.with(|budget| budget.set(Some(0)));
    assert!(reader.receive(first.clone()).await.is_err());
    let mut current = retry_receipt.then_some(reader);
    let mut interruptions = 1;
    loop {
        // Each reopen or retried receipt commits one more apply transaction.
        crate::APPLY_BUDGET.with(|budget| budget.set(Some(usize::from(!retry_receipt))));
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
                crate::APPLY_BUDGET.with(|budget| budget.set(None));
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
                // Each application counts once in its scope.
                assert_eq!(
                    rows_of(&reader, "SELECT sum(applied) FROM secure_scope_state").await,
                    vec![vec![SqlValue::Integer(2 * rows)]]
                );
                break;
            }
        }
    }
    // 320 operations take at least five transactions of 64.
    assert!(interruptions >= 4, "{interruptions} interruptions");
}
async fn rows_of(replica: &EncryptedReplica<TypedRows>, sql: &str) -> Vec<Vec<SqlValue>> {
    crate::storage::rows(&replica.inner.lock().await.conn, sql)
        .await
        .unwrap()
}
