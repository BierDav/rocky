use super::*;

#[tokio::test]
async fn checkpoint_birth_headers_survive_resume_and_forward_once_per_stream() {
    let fixture = Typed::new(UpdatePolicy::Changed);
    let directory = TempDir::new().unwrap();
    let source_key = PrivateKey::from_bytes(&[181; 32]);
    let source = fixture
        .open_number(
            &directory.path().join("source.db"),
            &source_key,
            fixture.full(),
            &[fixture.items],
            1,
        )
        .await;
    let writer = sql(&source, &[fixture.items]).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    for index in 0..64 {
        let identity = writer.next_id("items").await.unwrap();
        writer
            .run(
                "INSERT INTO items(key,title,done,id) VALUES(?,'title',0,?)",
                vec![
                    text(&format!("{index:02}")),
                    SqlValue::Integer(identity.integer()),
                ],
            )
            .await
            .unwrap();
    }
    let frames = writer
        .commit([181; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert_eq!(frames.len(), 2);
    drop(writer);
    let signed = crate::tests::compacted_base(&source, &fixture.publisher, fixture.items).await;
    let receiver_key = PrivateKey::from_bytes(&[182; 32]);
    let path = directory.path().join("receiver.db");
    let receiver = fixture
        .open_number(&path, &receiver_key, fixture.full(), &[fixture.items], 2)
        .await;
    crate::tests::exchange_graphs(&source, &receiver, fixture.domain).await;
    let credentials = fixture.credentials_number(&receiver_key, fixture.full(), 2);
    source
        .admit_peer(
            receiver_key.public_key(),
            &credentials,
            receiver.followed_tip().await,
        )
        .await
        .unwrap();
    receiver.begin_rebootstrap(signed.clone()).await.unwrap();
    let page = source
        .checkpoint_page(
            receiver_key.public_key(),
            &credentials,
            fixture.items,
            signed.id().unwrap(),
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert_eq!(
        crate::replica::checkpoint::page_header_count(&page, &fixture.key(fixture.items).key),
        2
    );
    let before = crate::tests::compaction::audit::snapshot(&receiver).await;
    for defect in 0..3 {
        let mut malformed = page.clone();
        crate::replica::checkpoint::forge_page_legend(
            &mut malformed,
            &fixture.key(fixture.items).key,
            &source_key,
            |_, _, origins| match defect {
                0 => origins.clear(),
                1 => origins.push(origins[0].clone()),
                _ => origins.swap(0, 1),
            },
        );
        assert!(
            receiver.stage_checkpoint_page(malformed).await.is_err(),
            "origin defect {defect}"
        );
        assert_eq!(
            crate::tests::compaction::audit::snapshot(&receiver).await,
            before
        );
    }
    let after = page.next;
    assert!(after.is_some());
    receiver.stage_checkpoint_page(page).await.unwrap();
    drop(receiver);
    let receiver = fixture
        .open_number(&path, &receiver_key, fixture.full(), &[fixture.items], 2)
        .await;
    let page = source
        .checkpoint_page(
            receiver_key.public_key(),
            &credentials,
            fixture.items,
            signed.id().unwrap(),
            after,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert_eq!(
        crate::replica::checkpoint::page_header_count(&page, &fixture.key(fixture.items).key),
        0
    );
    let headers = crate::storage::rows(
        &receiver.inner.lock().await.conn,
        "SELECT digest,header FROM secure_creation_headers ORDER BY digest LIMIT 1",
    )
    .await
    .unwrap();
    let [SqlValue::Blob(digest), SqlValue::Blob(original)] = headers[0].as_slice() else {
        panic!("creation header");
    };
    for replacement in [None, Some(vec![0xff])] {
        let inner = receiver.inner.lock().await;
        inner
            .conn
            .execute(
                "DELETE FROM secure_creation_headers WHERE digest=?",
                [digest.clone()],
            )
            .await
            .unwrap();
        if let Some(bytes) = replacement {
            inner
                .conn
                .execute(
                    "INSERT INTO secure_creation_headers VALUES(?,?)",
                    [digest.clone(), bytes],
                )
                .await
                .unwrap();
        }
        drop(inner);
        let before = crate::tests::compaction::audit::snapshot(&receiver).await;
        assert!(receiver.stage_checkpoint_page(page.clone()).await.is_err());
        assert_eq!(
            crate::tests::compaction::audit::snapshot(&receiver).await,
            before
        );
        receiver
            .inner
            .lock()
            .await
            .conn
            .execute(
                "INSERT OR REPLACE INTO secure_creation_headers VALUES(?,?)",
                [digest.clone(), original.clone()],
            )
            .await
            .unwrap();
    }
    let mut cursor = page.next;
    receiver.stage_checkpoint_page(page).await.unwrap();
    while let Some(after) = cursor {
        let page = source
            .checkpoint_page(
                receiver_key.public_key(),
                &credentials,
                fixture.items,
                signed.id().unwrap(),
                Some(after),
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        assert_eq!(
            crate::replica::checkpoint::page_header_count(&page, &fixture.key(fixture.items).key),
            0
        );
        cursor = page.next;
        receiver.stage_checkpoint_page(page).await.unwrap();
    }
    receiver.finish_rebootstrap(fixture.items).await.unwrap();
    receiver.audit(fixture.items).await.unwrap();
    let third_key = PrivateKey::from_bytes(&[183; 32]);
    let third = fixture
        .open_number(
            &directory.path().join("third.db"),
            &third_key,
            fixture.full(),
            &[fixture.items],
            3,
        )
        .await;
    crate::tests::exchange_graphs(&receiver, &third, fixture.domain).await;
    let third_credentials = fixture.credentials_number(&third_key, fixture.full(), 3);
    receiver
        .admit_peer(
            third_key.public_key(),
            &third_credentials,
            third.followed_tip().await,
        )
        .await
        .unwrap();
    third.begin_rebootstrap(signed.clone()).await.unwrap();
    let mut cursor = None;
    let mut headers = 0;
    let mut pages = 0;
    loop {
        let page = receiver
            .checkpoint_page(
                third_key.public_key(),
                &third_credentials,
                fixture.items,
                signed.id().unwrap(),
                cursor,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        headers +=
            crate::replica::checkpoint::page_header_count(&page, &fixture.key(fixture.items).key);
        cursor = page.next;
        pages += 1;
        third.stage_checkpoint_page(page).await.unwrap();
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!((pages, headers), (4, 2));
    third.finish_rebootstrap(fixture.items).await.unwrap();
    third.audit(fixture.items).await.unwrap();
    assert_eq!(
        select(
            &sql(&third, &[fixture.items]).await,
            "SELECT count(*) FROM items"
        )
        .await,
        vec![vec![SqlValue::Integer(64)]]
    );
}
