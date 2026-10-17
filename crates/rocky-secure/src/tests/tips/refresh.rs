//! One refresh borrows graph facts; every header and transaction stays distinct.
use super::*;
use rocky_core::SqlValue;
use rocky_credentials::TipGraph;

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
        "secure_outbox",
        "secure_held",
        "secure_held_frames",
        "secure_held_resolved",
        "secure_rows",
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
async fn refresh_keeps_lazy_header_graph_and_body_errors() {
    let (f, _dir, a, frames, _) = fixture().await;
    assert!(page(&a, f.public).await.unwrap().is_empty());
    let selected = frames
        .iter()
        .max_by_key(|frame| {
            let op = frame.operation_ref().unwrap();
            (op.header.public_key, op.header.seq_num)
        })
        .unwrap();
    let hash = selected.id().unwrap().as_bytes().to_vec();
    let original = selected.operation_ref().unwrap().header.clone();
    let mut absent_author = original.clone();
    absent_author.extensions.author = None;
    let mut unknown_domain = original.clone();
    unknown_domain.extensions.scope.domain.app.0[0] ^= 1;
    for (header, expected) in [
        (absent_author, "node author number missing"),
        (unknown_domain, "unknown tip graph"),
    ] {
        a.inner
            .lock()
            .await
            .conn
            .execute(
                "UPDATE secure_operations SET header=? WHERE hash=?",
                [crate::encode(&header).unwrap(), hash.clone()],
            )
            .await
            .unwrap();
        let before = snapshot(&a).await;
        assert!(
            matches!(page(&a, f.public).await, Err(Error::Rejected(message)) if message == expected)
        );
        assert_eq!(snapshot(&a).await, before);
    }
    let mut unknown_tip = original.clone();
    unknown_tip.extensions.tip = TipId([209; 32]);
    a.inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET header=?,body=? WHERE hash=?",
            crate::storage::bindings(vec![
                SqlValue::Blob(crate::encode(&unknown_tip).unwrap()),
                SqlValue::Blob(vec![0xff]),
                SqlValue::Blob(hash.clone()),
            ]),
        )
        .await
        .unwrap();
    let inputs = [(
        f.public.domain,
        unknown_tip.extensions.tip,
        original.extensions.author.unwrap(),
        original.extensions.author_sequence,
    )];
    let checks = a.inner.lock().await.refresh_reach_for_test(&inputs, false);
    assert_eq!(checks.0, vec![Ok("Unfollowed".into())]);
    assert_eq!(checks.1, checks.0);
    assert!(
        page(&a, f.public).await.unwrap().is_empty(),
        "non-Held header must not open the corrupt body"
    );
    a.inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET header=? WHERE hash=?",
            [selected.header_bytes().to_vec(), hash.clone()],
        )
        .await
        .unwrap();
    assert!(
        page(&a, f.public).await.unwrap().is_empty(),
        "valid live header must also skip its corrupt body"
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
    a.audit(f.public).await.unwrap();
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
