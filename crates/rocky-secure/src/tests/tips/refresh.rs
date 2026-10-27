//! One refresh borrows graph facts; every header and transaction stays distinct.
use super::*;
use rocky_core::SqlValue;
use rocky_credentials::TipGraph;

#[tokio::test]
async fn held_refresh_does_not_decode_live_or_continued_history() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = open(&f, &dir.path().join("a.db"), &PrivateKey::new()).await;
    main(&f, &a).await;
    for id in 1..=8 {
        insert(&f, &a, id, "history").await;
    }
    for continued in [false, true] {
        if continued {
            advance(&f, &a).await;
            insert(&f, &a, 9, "current").await;
        }
        let before = {
            let inner = a.inner.lock().await;
            inner.stored_header_memo_limits(0, 0);
            inner.stored_header_memo_stats().2
        };
        assert!(page(&a, f.public).await.unwrap().is_empty());
        assert_eq!(
            a.inner.lock().await.stored_header_memo_stats().2,
            before,
            "held lookup decoded unrelated signed history (continued={continued})"
        );
    }
}

#[tokio::test]
async fn abandoned_history_walks_bookmarks_once_per_source_and_refresh() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = open(&f, &dir.path().join("a.db"), &PrivateKey::new()).await;
    let b = open(&f, &dir.path().join("b.db"), &PrivateKey::new()).await;
    main(&f, &a).await;
    let genesis = a.followed_tip().await;
    advance(&f, &a).await;
    b.receive_tip_statements(&a.tip_statements(None, 32).await.unwrap())
        .await
        .unwrap();
    b.follow_bookmark(f.public.domain, "main").await.unwrap();
    let first = insert(&f, &b, 1, "first").await;
    let second = insert(&f, &b, 2, "second").await;
    let from = a.followed_tip().await;
    let continued = seal(&f, &a).await;
    a.receive_tip_statements(&[
        TipStatement::Seal(continued),
        bookmark(&f, "main", Some(from), Some(genesis)),
    ])
    .await
    .unwrap();
    let inputs: Vec<_> = [&first, &second]
        .into_iter()
        .map(|frame| {
            let e = &frame.operation_ref().unwrap().header.extensions;
            (f.public.domain, e.tip, e.author.unwrap(), e.author_sequence)
        })
        .collect();
    let inner = a.inner.lock().await;
    let (direct, shared, stats) = inner.refresh_reach_for_test(&inputs, false);
    assert_eq!(direct, vec![Ok("Held".into()), Ok("Held".into())]);
    assert_eq!(shared, direct);
    assert_eq!(stats.4, 1);
    let fallback = inner.refresh_reach_for_test(&inputs, true);
    assert_eq!(fallback.1, direct);
    assert_eq!(fallback.2.4, inputs.len());
    assert_eq!(inner.refresh_reach_for_test(&inputs, false).2.4, 1);
}

async fn fixture() -> (
    Fixture,
    TempDir,
    Replica,
    Vec<EncryptedFrame>,
    SealStatement,
) {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = open(
        &f,
        &dir.path().join("a.db"),
        &PrivateKey::from_bytes(&[101; 32]),
    )
    .await;
    let b = open(
        &f,
        &dir.path().join("b.db"),
        &PrivateKey::from_bytes(&[102; 32]),
    )
    .await;
    let c = open(
        &f,
        &dir.path().join("c.db"),
        &PrivateKey::from_bytes(&[103; 32]),
    )
    .await;
    main(&f, &a).await;
    let a0 = insert(&f, &a, 1, "covered a").await;
    let b0 = insert(&f, &b, 2, "covered b").await;
    let b1 = insert(&f, &b, 3, "b beyond").await;
    let c0 = insert(&f, &c, 4, "c absent").await;
    for frame in [&b0, &b1, &c0] {
        assert_eq!(
            a.receive(frame.clone()).await.unwrap(),
            ReceiveStatus::Applied
        );
    }
    let mut statement = seal(&f, &a).await.statement;
    statement.action = SealAction::Close;
    statement.frontier = [&a0, &b0]
        .into_iter()
        .map(|frame| {
            let operation = frame.operation_ref().unwrap();
            AuthorMark {
                author: operation.header.extensions.author.unwrap(),
                sequence: operation.header.extensions.author_sequence,
            }
        })
        .collect();
    statement.frontier.sort_by_key(|mark| mark.author);
    assert_eq!(statement.frontier.len(), 2);
    assert_ne!(statement.frontier[0].author, statement.frontier[1].author);
    (f, dir, a, vec![a0, b0, b1, c0], statement)
}

async fn page(replica: &Replica, scope: ResourceScope) -> Result<Vec<EncryptedFrame>, Error> {
    let (key, credentials) = {
        let inner = replica.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    let (frames, next) = replica
        .held_frame_page(key, &credentials, scope, None, MAX_SECURE_FRAME)
        .await?;
    assert!(next.is_none());
    Ok(frames)
}
fn encoded(frames: &[EncryptedFrame]) -> BTreeSet<Vec<u8>> {
    frames
        .iter()
        .map(|frame| frame.to_bytes().unwrap())
        .collect()
}
async fn snapshot(replica: &Replica) -> Vec<Vec<Vec<SqlValue>>> {
    let inner = replica.inner.lock().await;
    let mut result = Vec::new();
    for table in [
        "secure_operations",
        "secure_position_sources",
        "secure_outbox",
        "secure_held",
        "secure_held_frames",
        "secure_held_resolved",
        "secure_row_records",
        "secure_scope_state",
        "secure_metadata",
        "secure_identity_marks",
        "secure_identity_own",
        "secure_identity_summary",
        "secure_identity_ledger",
        "secure_admission_context",
        "secure_admission_entries",
        "secure_admission_claims",
        "secure_admission_carries",
    ] {
        result.push(
            rows(&inner.conn, &format!("SELECT * FROM {table} ORDER BY 1"))
                .await
                .unwrap(),
        );
    }
    result
}

#[tokio::test]
async fn metadata_close_is_discovered_lazily_per_author_and_sequence() {
    let (f, _dir, a, frames, statement) = fixture().await;
    let close = SignedSeal::sign(statement, &f.publisher).unwrap();
    let before_rows = a.snapshot(f.public).await.unwrap().rows;
    a.receive_tip_statements(&[TipStatement::Seal(close)])
        .await
        .unwrap();
    assert_eq!(
        scalar(
            &a,
            "SELECT count(*) FROM secure_operations WHERE status='applied'"
        )
        .await,
        4
    );
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM secure_held_frames").await,
        0
    );
    let checks: Vec<_> = frames
        .iter()
        .map(|frame| {
            let op = frame.operation_ref().unwrap();
            (
                f.public.domain,
                op.header.extensions.tip,
                op.header.extensions.author.unwrap(),
                op.header.extensions.author_sequence,
            )
        })
        .collect();
    let inner = a.inner.lock().await;
    let (direct, reused, stats) = inner.refresh_reach_for_test(&checks, false);
    assert_eq!(direct, reused);
    assert_eq!(
        reused,
        ["Sealed", "Sealed", "Held", "Held"].map(|value| Ok(value.into()))
    );
    assert!(stats.0 > 0 && stats.1 > 0);
    assert!(stats.2 <= 1024 && stats.3 <= 1024 * 1024);
    assert_eq!(inner.refresh_reach_for_test(&checks, true).1, direct);
    drop(inner);
    assert_eq!(
        encoded(&page(&a, f.public).await.unwrap()),
        encoded(&frames[2..])
    );
    assert_eq!(
        scalar(
            &a,
            "SELECT count(*) FROM secure_operations WHERE status='applied'"
        )
        .await,
        2
    );
    assert_eq!(
        a.snapshot(f.public).await.unwrap().rows,
        before_rows,
        "holding records original writes without rewriting projection"
    );
    let once = snapshot(&a).await;
    assert_eq!(
        encoded(&page(&a, f.public).await.unwrap()),
        encoded(&frames[2..])
    );
    assert_eq!(snapshot(&a).await, once);
}

#[tokio::test]
async fn failed_later_hold_rolls_back_the_whole_refresh_then_retry_and_reopen_agree() {
    let (f, dir, a, frames, statement) = fixture().await;
    let close = SignedSeal::sign(statement, &f.publisher).unwrap();
    a.receive_tip_statements(&[TipStatement::Seal(close.clone())])
        .await
        .unwrap();
    let later = frames[2..]
        .iter()
        .max_by_key(|frame| frame.operation_ref().unwrap().header.public_key)
        .unwrap()
        .id()
        .unwrap();
    let first = frames[2..]
        .iter()
        .min_by_key(|frame| frame.operation_ref().unwrap().header.public_key)
        .unwrap()
        .id()
        .unwrap();
    let first_hex: String = first
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let hex: String = later
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    a.inner.lock().await.conn.execute(&format!("CREATE TEMP TRIGGER fail_later_hold BEFORE UPDATE OF status ON secure_operations WHEN NEW.hash=X'{hex}' AND NEW.status='held' BEGIN SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM secure_held_frames WHERE hash=X'{first_hex}') THEN RAISE(ABORT,'first candidate not held') END; SELECT RAISE(ABORT,'later hold failure'); END"), ()).await.unwrap();
    let before = snapshot(&a).await;
    let error = page(&a, f.public).await.unwrap_err();
    assert!(error.to_string().contains("later hold failure"), "{error}");
    assert_eq!(snapshot(&a).await, before);
    a.inner
        .lock()
        .await
        .conn
        .execute("DROP TRIGGER fail_later_hold", ())
        .await
        .unwrap();
    assert_eq!(
        encoded(&page(&a, f.public).await.unwrap()),
        encoded(&frames[2..])
    );
    let (next, mut statements) = branch(&f, &close);
    statements.push(bookmark(&f, "main", Some(close.statement.tip), Some(next)));
    a.receive_tip_statements(&statements).await.unwrap();
    assert_eq!(a.followed_tip().await, next);
    assert_eq!(a.snapshot(f.public).await.unwrap().rows.len(), 2);
    assert_eq!(
        encoded(&page(&a, f.public).await.unwrap()),
        encoded(&frames[2..])
    );
    a.audit(f.public).await.unwrap();
    let key = a.inner.lock().await.config.node.clone();
    drop(a);
    let reopened = open(&f, &dir.path().join("a.db"), &key).await;
    assert_eq!(reopened.followed_tip().await, next);
    assert_eq!(
        encoded(&page(&reopened, f.public).await.unwrap()),
        encoded(&frames[2..])
    );
    reopened.audit(f.public).await.unwrap();
}

#[tokio::test]
async fn a_new_refresh_uses_the_new_winning_close_after_an_issuer_continue() {
    let (f, _dir, a, frames, mut statement) = fixture().await;
    let close = SignedSeal::sign(statement.clone(), &f.publisher).unwrap();
    let schema = a
        .tip_graph(f.public.domain)
        .await
        .unwrap()
        .schema(statement.tip)
        .unwrap();
    statement.action = SealAction::Continue { schema };
    let issuer = PrivateKey::from_bytes(&[104; 32]);
    let chain = CredentialChain(vec![
        Authority::owner(&f.publisher)
            .issue_issuer(issuer.public_key(), None, validity())
            .unwrap(),
    ]);
    let continued = SignedSeal::endorse(statement, &issuer, &chain).unwrap();
    a.receive_tip_statements(&[TipStatement::Seal(continued)])
        .await
        .unwrap();
    assert!(page(&a, f.public).await.unwrap().is_empty());
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM secure_held_frames").await,
        0
    );
    a.receive_tip_statements(&[TipStatement::Seal(close.clone())])
        .await
        .unwrap();
    assert_eq!(
        a.tip_graph(f.public.domain)
            .await
            .unwrap()
            .seal(close.statement.tip)
            .unwrap(),
        Some(&close)
    );
    assert_eq!(
        encoded(&page(&a, f.public).await.unwrap()),
        encoded(&frames[2..])
    );
}

#[tokio::test]
async fn refresh_checks_index_coordinates_and_only_opens_candidate_frames() {
    let (f, _dir, a, frames, statement) = fixture().await;
    assert!(page(&a, f.public).await.unwrap().is_empty());
    let selected = &frames[2];
    let hash = selected.id().unwrap().as_bytes().to_vec();
    let mut unknown_domain = f.public;
    unknown_domain.domain.app.0[0] ^= 1;
    for (column, value, original) in [
        (
            "author",
            SqlValue::Integer(-1),
            SqlValue::Integer(i64::from(
                selected
                    .operation_ref()
                    .unwrap()
                    .header
                    .extensions
                    .author
                    .unwrap(),
            )),
        ),
        (
            "scope",
            SqlValue::Blob(crate::encode(&unknown_domain).unwrap()),
            SqlValue::Blob(crate::encode(&f.public).unwrap()),
        ),
    ] {
        let sql = format!("UPDATE secure_position_sources SET {column}=? WHERE hash=? AND kind=0");
        a.inner
            .lock()
            .await
            .conn
            .execute(
                &sql,
                crate::storage::bindings(vec![value, SqlValue::Blob(hash.clone())]),
            )
            .await
            .unwrap();
        // Direct SQL corruption bypasses the replica's refresh generations.
        a.inner.lock().await.invalidate_held_refresh();
        let before = snapshot(&a).await;
        let error = page(&a, f.public).await.unwrap_err();
        if column == "author" {
            assert!(matches!(error, Error::Corrupt(message) if message == "closed source author"));
        } else {
            assert!(matches!(error, Error::Rejected(message) if message == "unknown tip graph"));
        }
        assert_eq!(snapshot(&a).await, before);
        a.inner
            .lock()
            .await
            .conn
            .execute(
                &sql,
                crate::storage::bindings(vec![original, SqlValue::Blob(hash.clone())]),
            )
            .await
            .unwrap();
    }
    a.inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET body=? WHERE hash=?",
            [vec![0xff], hash.clone()],
        )
        .await
        .unwrap();
    assert!(
        page(&a, f.public).await.unwrap().is_empty(),
        "live indexed positions must not open unrelated corrupt bodies"
    );
    a.inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET body=? WHERE hash=?",
            [selected.body_bytes().to_vec(), hash.clone()],
        )
        .await
        .unwrap();
    a.audit(f.public).await.unwrap();
    a.receive_tip_statements(&[TipStatement::Seal(
        SignedSeal::sign(statement, &f.publisher).unwrap(),
    )])
    .await
    .unwrap();
    a.inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET body=? WHERE hash=?",
            [vec![0xff], hash.clone()],
        )
        .await
        .unwrap();
    let before = snapshot(&a).await;
    assert!(
        page(&a, f.public).await.is_err(),
        "held candidates must open and verify original frames"
    );
    assert_eq!(snapshot(&a).await, before);
    a.inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET header=?,body=? WHERE hash=?",
            crate::storage::bindings(vec![
                SqlValue::Blob(vec![0xff]),
                SqlValue::Blob(selected.body_bytes().to_vec()),
                SqlValue::Blob(hash.clone()),
            ]),
        )
        .await
        .unwrap();
    assert!(
        page(&a, f.public).await.is_err(),
        "candidate headers remain verified"
    );
    a.inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET header=?,body=? WHERE hash=?",
            crate::storage::bindings(vec![
                SqlValue::Blob(selected.header_bytes().to_vec()),
                SqlValue::Blob(selected.body_bytes().to_vec()),
                SqlValue::Blob(hash),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(
        encoded(&page(&a, f.public).await.unwrap()),
        encoded(&frames[2..])
    );
}

#[tokio::test]
async fn identical_genesis_ids_still_query_each_domain_and_empty_refresh_is_lazy() {
    let (f, _dir, a, frames, statement) = fixture().await;
    let close = SignedSeal::sign(statement, &f.publisher).unwrap();
    a.receive_tip_statements(&[TipStatement::Seal(close.clone())])
        .await
        .unwrap();
    let mut other = f.public.domain;
    other.domain.0[0] ^= 1;
    let mut inner = a.inner.lock().await;
    let original = &inner.graphs[&f.public.domain];
    let graph = TipGraph::new(other, original.genesis_schema);
    assert_eq!(graph.genesis(), original.genesis());
    inner.graphs.insert(other, graph);
    let op = frames[0].operation_ref().unwrap();
    let inputs = [
        (
            f.public.domain,
            op.header.extensions.tip,
            op.header.extensions.author.unwrap(),
            0,
        ),
        (
            other,
            op.header.extensions.tip,
            op.header.extensions.author.unwrap(),
            0,
        ),
    ];
    let (direct, shared, _) = inner.refresh_reach_for_test(&inputs, false);
    assert_eq!(direct, shared);
    assert_eq!(shared, vec![Ok("Sealed".into()), Ok("Live".into())]);
    inner.graphs.remove(&other);
    drop(inner);
    let dir = TempDir::new().unwrap();
    let empty = open(&f, &dir.path().join("empty.db"), &PrivateKey::new()).await;
    let mut malformed = TipGraph::new(
        other,
        f.manifest.verify(f.publisher.public_key()).unwrap().hash(),
    );
    let mut bad = close;
    bad.statement.format = 0;
    malformed.seals.push(bad);
    empty.inner.lock().await.graphs.insert(other, malformed);
    assert!(
        page(&empty, f.public).await.unwrap().is_empty(),
        "unqueried malformed graph is not eagerly validated"
    );
}
