//! Filtered foreign carries retain provenance without an original grant.
use super::*;

#[tokio::test]
async fn a_filtered_foreign_carry_records_one_observation_below_the_destinations_marks() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::build_with(true, PrivateKey::from_bytes(&[210; 32]));
    let writer = keyed(&f, &root.path().join("writer"), [211; 32], 1).await;
    let mover = keyed(&f, &root.path().join("mover"), [212; 32], 2).await;
    let path = root.path().join("reader");
    let mut reader = keyed(&f, &path, [213; 32], 3).await;
    let original = writer
        .author([1; 32], vec![f.titled("A", 1, 1, "carried").change], vec![])
        .await
        .unwrap();
    let high = writer
        .author([2; 32], vec![f.titled("B", 2, 1, "high").change], vec![])
        .await
        .unwrap();
    for replica in [&mover, &reader] {
        for frame in [&original, &high] {
            assert_eq!(
                replica.receive(frame.clone()).await.unwrap(),
                ReceiveStatus::Applied
            );
        }
    }
    let bundle = mover
        .session([f.scope("A"), f.scope("B")].into())
        .await
        .unwrap()
        .move_row(
            [3; 32],
            "items",
            RowIdentity::new(1, 1).unwrap(),
            &[SqlValue::Text("A".into())],
            &[SqlValue::Text("B".into())],
        )
        .await
        .unwrap();
    let carry = bundle
        .iter()
        .find(|frame| frame.scope().unwrap() == f.scope("B"))
        .unwrap();
    assert_eq!(carry.operation().unwrap().header.extensions.author, Some(2));
    let (absent, statements) = removed_path(
        &f,
        writer.followed_tip().await,
        vec![
            AuthorMark {
                author: 1,
                sequence: 1,
            },
            AuthorMark {
                author: 2,
                sequence: 1,
            },
        ],
    );
    reader.receive_tip_statements(&statements).await.unwrap();
    reader
        .inner
        .lock()
        .await
        .put_identity_mark(
            f.scope("B"),
            &crate::replica::checkpoint::IdentityMark {
                author: 1,
                signer: original.operation().unwrap().header.public_key,
                sequence: 2,
            },
            crate::replica::Raise::Statement { own: 2 },
        )
        .await
        .unwrap();
    switch(&f, &reader, "dev", &absent).await;
    for frame in &bundle {
        reader.receive(frame.clone()).await.unwrap();
    }
    let query =
        "SELECT bytes,signature,chain FROM secure_admission_entries WHERE frame=? AND kind=0";
    let receipt = crate::storage::collect_rows(
        reader
            .inner
            .lock()
            .await
            .conn
            .query(query, [carry.id().unwrap().as_bytes().to_vec()])
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.len(), 1);
    assert_eq!(
        count(
            &reader,
            "SELECT 1 FROM secure_admission_entries WHERE kind=2"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &reader,
            "SELECT 1 FROM secure_admission_claims WHERE kind=1"
        )
        .await,
        0
    );
    let prefix = audit::journal(&reader).await;
    switch(&f, &reader, "main", &f.schema).await;
    let expected = items(&f, &mover).await;
    assert_eq!(items(&f, &reader).await, expected);
    assert_eq!(
        count(
            &reader,
            "SELECT 1 FROM secure_admission_entries WHERE kind=2"
        )
        .await,
        1
    );
    assert_eq!(
        count(&reader, "SELECT 1 FROM secure_admission_carries").await,
        1
    );
    assert_eq!(
        count(
            &reader,
            "SELECT 1 FROM secure_admission_claims WHERE kind=1"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &reader,
            "SELECT 1 FROM secure_admission_claims WHERE kind=0"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &reader,
            "SELECT 1 FROM secure_admission_entries WHERE kind=1"
        )
        .await,
        0
    );
    assert_eq!(
        crate::storage::collect_rows(
            reader
                .inner
                .lock()
                .await
                .conn
                .query(query, [carry.id().unwrap().as_bytes().to_vec()])
                .await
                .unwrap()
        )
        .await
        .unwrap(),
        receipt
    );
    audit::refuses_missing_continuation(&reader, f.scope("B"), prefix, "carry has no observation")
        .await;
    reader.audit(f.scope("A")).await.unwrap();
    reader.audit(f.scope("B")).await.unwrap();
    let journal = rows(
        &reader.inner.lock().await.conn,
        "SELECT * FROM secure_admission_entries ORDER BY entry",
    )
    .await
    .unwrap();
    let config = security(&f, &reader).await;
    drop(reader);
    reader = EncryptedReplica::open(&path, f.app(), config)
        .await
        .unwrap();
    for frame in &bundle {
        reader.receive(frame.clone()).await.unwrap();
    }
    reader.rebuild_projection(f.scope("B")).await.unwrap();
    reader.audit(f.scope("B")).await.unwrap();
    assert_eq!(items(&f, &reader).await, expected);
    assert_eq!(
        rows(
            &reader.inner.lock().await.conn,
            "SELECT * FROM secure_admission_entries ORDER BY entry"
        )
        .await
        .unwrap(),
        journal
    );
}
