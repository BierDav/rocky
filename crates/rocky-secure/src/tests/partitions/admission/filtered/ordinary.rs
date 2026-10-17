//! Unresolved concrete floor and cross-partition summary ordering.
use super::*;

#[tokio::test]
async fn a_removed_partitioned_table_reopens_with_its_admitted_history() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::build_with(true, PrivateKey::from_bytes(&[226; 32]));
    let writer = keyed(&f, &root.path().join("writer"), [227; 32], 1).await;
    let path = root.path().join("reader");
    let reader = keyed(&f, &path, [228; 32], 2).await;
    let frame = writer
        .author(
            [1; 32],
            vec![f.titled("A", 1, 1, "retained").change],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        reader.receive(frame.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    let (absent, statements) = removed_path(
        &f,
        writer.followed_tip().await,
        vec![AuthorMark {
            author: 1,
            sequence: 0,
        }],
    );
    reader.receive_tip_statements(&statements).await.unwrap();
    switch(&f, &reader, "dev", &absent).await;
    reader.audit(f.scope("A")).await.unwrap();
    let config = security(&f, &reader).await;
    drop(reader);
    let reader = EncryptedReplica::open(
        &path,
        TypedRows::new(absent, vec![f.domain.domain]).unwrap(),
        config,
    )
    .await
    .unwrap();
    reader.audit(f.scope("A")).await.unwrap();
    switch(&f, &reader, "main", &f.schema).await;
    assert_eq!(items(&f, &reader).await, items(&f, &writer).await);
    assert_eq!(
        count(
            &reader,
            "SELECT 1 FROM secure_admission_entries WHERE kind=1"
        )
        .await,
        0
    );
    reader.audit(f.scope("A")).await.unwrap();
}

#[tokio::test]
async fn an_unresolved_partition_birth_restores_past_another_partitions_summary() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::build_with(true, PrivateKey::from_bytes(&[206; 32]));
    let writer = keyed(&f, &root.path().join("writer"), [207; 32], 1).await;
    let early = writer
        .author([1; 32], vec![f.titled("B", 1, 1, "early").change], vec![])
        .await
        .unwrap();
    let later = writer
        .author([2; 32], vec![f.titled("A", 2, 1, "later").change], vec![])
        .await
        .unwrap();
    let (absent, statements) = removed_path(
        &f,
        writer.followed_tip().await,
        vec![AuthorMark {
            author: 1,
            sequence: 0,
        }],
    );
    let absent_app = TypedRows::new(absent.clone(), vec![f.domain.domain]).unwrap();
    for (index, before) in [true, false].into_iter().enumerate() {
        let path = root.path().join(format!("reader{index}"));
        let mut reader = keyed(&f, &path, [208 + index as u8; 32], 2 + index as u32).await;
        // A summary-only partition raises allocation P without proving
        // which individual identities it once held. B's own floor stays 0.
        reader
            .inner
            .lock()
            .await
            .put_identity_mark(
                f.scope("A"),
                &crate::replica::checkpoint::IdentityMark {
                    author: 1,
                    signer: later.operation().unwrap().header.public_key,
                    sequence: 2,
                },
                crate::replica::Raise::Own,
            )
            .await
            .unwrap();
        reader.receive_tip_statements(&statements).await.unwrap();
        switch(&f, &reader, "dev", &absent).await;
        if before {
            let error = reader
                .register_partition(f.scope("new"), f.tuple("new"))
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains("invalid partition address"),
                "{error}"
            );
            let original = early.operation().unwrap().header;
            let plaintext = crate::wire::decrypt(
                &[2; 32],
                &encode(&original.extensions).unwrap(),
                early.body_bytes(),
            )
            .unwrap();
            let mut extension = original.extensions.clone();
            extension.tip = reader.followed_tip().await;
            let wrong_source = crate::wire::author(
                &PrivateKey::from_bytes(&[207; 32]),
                extension,
                early.chain().unwrap().clone(),
                &[2; 32],
                (original.seq_num, original.backlink, original.timestamp),
                original.previous,
                &plaintext,
            )
            .unwrap();
            let before_rows = rows(
                &reader.inner.lock().await.conn,
                "SELECT * FROM secure_admission_entries ORDER BY entry",
            )
            .await
            .unwrap();
            let error = reader.receive(wrong_source).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("wrong app/domain/table/manifest"),
                "{error}"
            );
            assert_eq!(
                rows(
                    &reader.inner.lock().await.conn,
                    "SELECT * FROM secure_admission_entries ORDER BY entry"
                )
                .await
                .unwrap(),
                before_rows
            );
        }
        let summary = || async {
            reader
                .inner
                .lock()
                .await
                .put_identity_mark(
                    f.scope("A"),
                    &crate::replica::checkpoint::IdentityMark {
                        author: 1,
                        signer: later.operation().unwrap().header.public_key,
                        sequence: 2,
                    },
                    crate::replica::Raise::Statement { own: 2 },
                )
                .await
                .unwrap();
        };
        if before {
            summary().await;
        }
        reader.receive(early.clone()).await.unwrap();
        if !before {
            summary().await;
        }
        let query =
            "SELECT bytes,signature,chain FROM secure_admission_entries WHERE frame=? AND kind=0";
        let receipt = crate::storage::collect_rows(
            reader
                .inner
                .lock()
                .await
                .conn
                .query(query, [early.id().unwrap().as_bytes().to_vec()])
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(receipt.len(), 1);
        assert_eq!(
            count(&reader, "SELECT 1 FROM secure_admission_claims").await,
            0
        );
        assert_eq!(
            count(&reader, "SELECT 1 FROM secure_identity_partitions").await,
            1
        );
        let config = security(&f, &reader).await;
        reader.audit(f.scope("B")).await.unwrap();
        drop(reader);
        reader = EncryptedReplica::open(&path, absent_app.clone(), config)
            .await
            .unwrap();
        let prefix = audit::journal(&reader).await;
        switch(&f, &reader, "main", &f.schema).await;
        assert_eq!(
            items(&f, &reader).await,
            vec![vec![
                SqlValue::Text("B".into()),
                SqlValue::Text("key1-author1".into()),
                SqlValue::Text("early".into())
            ]]
        );
        assert_eq!(
            count(
                &reader,
                "SELECT 1 FROM secure_admission_entries WHERE kind=1"
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
            1
        );
        assert_eq!(
            crate::storage::collect_rows(
                reader
                    .inner
                    .lock()
                    .await
                    .conn
                    .query(query, [early.id().unwrap().as_bytes().to_vec()])
                    .await
                    .unwrap()
            )
            .await
            .unwrap(),
            receipt
        );
        let normalized = crate::storage::collect_rows(reader.inner.lock().await.conn.query(
            "SELECT t.scope FROM secure_admission_claims c JOIN secure_identity_tables t ON t.id=c.allocation WHERE c.identity=?",
            [RowIdentity::new(1, 1).unwrap().integer()],
        ).await.unwrap()).await.unwrap();
        assert_eq!(
            normalized,
            vec![vec![SqlValue::Blob(
                encode(&ResourceScope {
                    domain: f.domain,
                    table: f.scope("B").table,
                    encryption: [9; 32]
                })
                .unwrap()
            )]]
        );
        audit::refuses_missing_continuation(
            &reader,
            f.scope("B"),
            prefix,
            "restoration has no grant",
        )
        .await;
        reader.rebuild_projection(f.scope("B")).await.unwrap();
        reader.receive(early.clone()).await.unwrap();
        assert_eq!(
            count(
                &reader,
                "SELECT 1 FROM secure_admission_entries WHERE kind=1"
            )
            .await,
            1
        );
        reader.audit(f.scope("A")).await.unwrap();
        reader.audit(f.scope("B")).await.unwrap();
    }
}
