//! Receive floors stay concrete: an honest author's partitions arrive in any
//! order, and only exact claims refuse a birth across partitions.
use super::*;
use crate::storage::rows;

mod carry_checkpoint;
mod carry_order;
mod filtered;

async fn items(f: &Fixture, replica: &EncryptedReplica<TypedRows>) -> Vec<Vec<SqlValue>> {
    replica
        .session([f.scope("A"), f.scope("B")].into())
        .await
        .unwrap()
        .sql(Default::default())
        .await
        .unwrap()
        .run("SELECT list,key,title FROM items ORDER BY list,key", vec![])
        .await
        .unwrap()
        .rows
}

async fn count(replica: &EncryptedReplica<TypedRows>, sql: &str) -> usize {
    rows(&replica.inner.lock().await.conn, sql)
        .await
        .unwrap()
        .len()
}

/// `Fixture::write` for another author number.
fn written(f: &Fixture, name: &str, author: u32, sequence: u32) -> RowWrite {
    let app = f.app();
    let table = app.tables().next().unwrap();
    let values = vec![
        SqlValue::Text(name.into()),
        SqlValue::Text(format!("key{sequence}-author{author}")),
        SqlValue::Integer(20),
        SqlValue::Integer(RowIdentity::new(author, sequence).unwrap().integer()),
        SqlValue::Integer(0),
    ];
    RowWrite::changed(RowChange {
        scope: f.scope(name),
        row: app.row_id(f.domain.domain, table, &values).unwrap(),
        before: None,
        after: Some(app.fields(table, &values).unwrap()),
    })
}

async fn keyed(
    f: &Fixture,
    path: &Path,
    node: [u8; 32],
    number: u32,
) -> EncryptedReplica<TypedRows> {
    let replica = f
        .open(
            path,
            PrivateKey::from_bytes(&node),
            f.grant(None, &all()),
            Some((PrivateKey::new(), number)),
        )
        .await;
    f.key(&replica, "A", [1; 32]).await;
    f.key(&replica, "B", [2; 32]).await;
    replica
}

#[tokio::test]
async fn an_authors_separate_partition_transactions_converge_in_every_delivery_order() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::constrained();
    let a = keyed(&f, &root.path().join("a"), [141; 32], 1).await;
    let author = |handle: u8, name: &str, sequence: u32| {
        let change = f.titled(name, sequence, 1, &format!("t{sequence}")).change;
        let a = a.clone();
        async move { a.author([handle; 32], vec![change], vec![]).await.unwrap() }
    };
    // Table-wide allocation interleaves sequences across partition logs.
    let first = author(1, "B", 1).await;
    let second = author(2, "A", 2).await;
    let third = author(3, "B", 3).await;
    let expected = items(&f, &a).await;
    assert_eq!(expected.len(), 3);
    for (name, order, number) in [
        ("x", [&second, &first, &third], 2),
        ("y", [&first, &third, &second], 3),
    ] {
        let receiver = keyed(
            &f,
            &root.path().join(name),
            [142 + number as u8; 32],
            number,
        )
        .await;
        for frame in order {
            receiver.receive(frame.clone()).await.unwrap();
        }
        assert_eq!(items(&f, &receiver).await, expected, "{name}");
        assert_eq!(
            count(
                &receiver,
                "SELECT 1 FROM secure_admission_claims WHERE kind=0"
            )
            .await,
            3
        );
        assert_eq!(count(&receiver, "SELECT 1 FROM secure_applied a JOIN secure_operations o ON o.hash=a.hash WHERE o.status='applied'").await, 3);
    }
    // A summary of A at mark 2 does not refuse B's earlier honest birth, and
    // is recorded only at A's own scope.
    let z = keyed(&f, &root.path().join("z"), [150; 32], 4).await;
    let signer = a.inner.lock().await.config.node.public_key();
    {
        let mut inner = z.inner.lock().await;
        let mark = crate::replica::checkpoint::IdentityMark {
            author: 1,
            signer,
            sequence: 2,
        };
        transaction!(inner, |inner| inner
            .put_identity_mark(
                f.scope("A"),
                &mark,
                crate::replica::Raise::Statement { own: mark.sequence }
            )
            .await)
        .unwrap();
    }
    for frame in [&first, &third] {
        z.receive(frame.clone()).await.unwrap();
    }
    assert_eq!(items(&f, &z).await.len(), 2);
    let summary = rows(
        &z.inner.lock().await.conn,
        "SELECT scope FROM secure_identity_summary",
    )
    .await
    .unwrap();
    assert_eq!(
        summary,
        vec![vec![SqlValue::Blob(encode(&f.scope("A")).unwrap())]]
    );
    z.audit(f.scope("B")).await.unwrap();
}

#[tokio::test]
async fn a_known_origin_refuses_its_reuse_in_another_partition() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::constrained();
    let a = keyed(&f, &root.path().join("a"), [151; 32], 1).await;
    let receiver = keyed(&f, &root.path().join("x"), [152; 32], 2).await;
    for sequence in 1..=5 {
        let frame = a
            .author(
                [sequence as u8; 32],
                vec![f.titled("A", sequence, 1, &format!("a{sequence}")).change],
                vec![],
            )
            .await
            .unwrap();
        receiver.receive(frame).await.unwrap();
    }
    // The same author key, in a file whose own allocation lies elsewhere,
    // signs a second origin of (1,5) in B; its fillers in A stay unsent.
    let forger = keyed(&f, &root.path().join("forger"), [151; 32], 1).await;
    for sequence in 1..5 {
        forger
            .author(
                [sequence as u8 + 20; 32],
                vec![f.titled("A", sequence, 1, &format!("f{sequence}")).change],
                vec![],
            )
            .await
            .unwrap();
    }
    let reuse = forger
        .author([3; 32], vec![f.titled("B", 5, 1, "reuse").change], vec![])
        .await
        .unwrap();
    let before = items(&f, &receiver).await;
    // Only the exact kind-0 claim on A refuses it: B's concrete mark is zero.
    let hash = reuse.operation().unwrap().hash;
    let _ = receiver.receive(reuse).await;
    let status = rows(
        &receiver.inner.lock().await.conn,
        &format!(
            "SELECT status FROM secure_operations WHERE hash=X'{}'",
            hash.to_hex()
        ),
    )
    .await
    .unwrap();
    assert_eq!(status, vec![vec![SqlValue::Text("rejected".into())]]);
    assert_eq!(items(&f, &receiver).await, before);
    assert_eq!(
        count(
            &receiver,
            "SELECT 1 FROM secure_admission_claims WHERE kind=0"
        )
        .await,
        5
    );
    receiver.audit(f.scope("A")).await.unwrap();
}

#[tokio::test]
async fn a_revoked_authors_unproved_partition_header_is_omitted_without_its_tuple() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let writer = f
        .open(
            &root.path().join("writer"),
            PrivateKey::new(),
            f.grant(None, &all()),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    let other = f
        .open(
            &root.path().join("other"),
            PrivateKey::new(),
            f.grant(None, &all()),
            Some((PrivateKey::new(), 2)),
        )
        .await;
    let reader_key = PrivateKey::new();
    let reader = f
        .open(
            &root.path().join("reader"),
            reader_key.clone(),
            f.grant(Some(&["A"]), &[Right::Read]),
            None,
        )
        .await;
    for replica in [&writer, &other] {
        f.key(replica, "A", [1; 32]).await;
        f.key(replica, "secret-partition", [2; 32]).await;
    }
    f.key(&reader, "A", [1; 32]).await;
    let visible = writer
        .author_bundle([1; 32], vec![f.write("A", 1, 10)], vec![])
        .await
        .unwrap()
        .remove(0);
    let hidden = other
        .author_bundle([2; 32], vec![written(&f, "secret-partition", 2, 1)], vec![])
        .await
        .unwrap()
        .remove(0);
    let eligible = writer
        .author_bundle([3; 32], vec![f.write("secret-partition", 2, 30)], vec![])
        .await
        .unwrap()
        .remove(0);
    writer.receive(hidden.clone()).await.unwrap();
    assert_eq!(
        reader.receive(visible).await.unwrap(),
        ReceiveStatus::Applied
    );
    seal_main(&writer, &f.publisher, f.domain).await;
    exchange_graphs(&writer, &reader, f.domain).await;
    let credentials = reader.inner.lock().await.config.credentials.clone();
    let mut requests = reader.wanted_placements().await.unwrap();
    requests.sort_by_key(|request| request.author);
    let [eligible_request, revoked_request] = requests.as_slice() else {
        panic!("{requests:?}")
    };
    assert_eq!(
        revoked_request.author,
        hidden
            .operation()
            .unwrap()
            .header
            .extensions
            .author
            .unwrap()
    );
    assert_eq!(
        eligible_request.author,
        eligible
            .operation()
            .unwrap()
            .header
            .extensions
            .author
            .unwrap()
    );
    let page = |request| {
        let (writer, credentials, scopes) =
            (writer.clone(), credentials.clone(), [f.scope("A")].into());
        let reader = reader_key.public_key();
        async move {
            writer
                .placement_page(reader, &credentials, &scopes, request)
                .await
                .unwrap()
        }
    };
    assert!(matches!(
        page(*revoked_request).await,
        crate::PlacementPage::CustodyRequired
    ));
    let grant = other.inner.lock().await.config.credentials.0[0]
        .id()
        .unwrap();
    writer
        .learn_revocations(&crate::tests::revoking(
            &f.publisher,
            f.schema.app_id(),
            &[],
            &[grant],
        ))
        .await
        .unwrap();
    // Revoked candidates alone answer like an empty position: no header,
    // chain or tuple, and no custody demand that would reveal a partition.
    let omitted = page(*revoked_request).await;
    assert!(
        matches!(&omitted, crate::PlacementPage::Headers { headers, chains } if headers.is_empty() && chains.is_empty()),
        "{omitted:?}"
    );
    assert!(matches!(
        page(*eligible_request).await,
        crate::PlacementPage::CustodyRequired
    ));
    assert_eq!(
        reader
            .receive_placements(*revoked_request, omitted)
            .await
            .unwrap(),
        0
    );
}
