use super::syntax::*;
use super::*;

#[tokio::test]
async fn actual_reconstruction_has_one_raw_bounded_slot_and_valid_uncached_fallback() {
    let (l, _, _, replica, signed) = candidate(1).await;
    release(&replica, &signed).await;
    let frame = frames(&replica).await.remove(0);
    let mut other = signed.clone();
    other.statement.root[0] ^= 1;
    let inner = replica.inner.lock().await;
    let before = snapshot(&inner).await;
    for capacity in [0, 1, 32768] {
        inner.migration_syntax_capacity(capacity);
        save(&inner).await;
        for value in [&signed, &other, &signed, &other] {
            signed_bytes(&inner, value.to_bytes().unwrap()).await;
            for _ in 0..2 {
                let returned = reconstruct(&inner, &frame).await.unwrap();
                assert_eq!(descriptor(&returned), value);
            }
            let stats = inner.migration_syntax_stats();
            assert!(stats.2 <= 1 && stats.3 <= capacity);
        }
        let (parses, hits, entries, raw) = inner.migration_syntax_stats();
        if capacity < signed.to_bytes().unwrap().len() {
            assert_eq!((parses, hits, entries, raw), (8, 0, 0, 0));
        } else {
            assert_eq!((parses, hits, entries), (4, 4, 1));
        }
        signed_bytes(&inner, vec![0xff]).await;
        same_error(
            reconstruct(&inner, &frame).await.unwrap_err(),
            crate::credential(SignedMigrationPublication::from_bytes(&[0xff]).unwrap_err()),
        );
        restore(&inner).await;
    }
    inner.migration_syntax_capacity(32768);
    reconstruct(&inner, &frame).await.unwrap();
    inner.migration_syntax_mismatch();
    same_frame(&reconstruct(&inner, &frame).await.unwrap(), &frame);
    assert_eq!(inner.migration_syntax_stats().0, 2);
    assert_eq!(inner.migration_syntax_stats().1, 0);
    assert_eq!(snapshot(&inner).await, before);
    drop(inner);
    replica.audit(l.scope("items")).await.unwrap();
}

#[tokio::test]
async fn a_failed_attempt_keeps_only_syntax_and_retry_reads_and_credits_original_sql_once() {
    let (l, _, _, replica, signed) = candidate_tables(1, true).await;
    release(&replica, &signed).await;
    let frame = frames(&replica).await.remove(0);
    let storage = replica.migration_storage().await.unwrap();
    let mut other = signed.clone();
    other.statement.root[0] ^= 1;
    let mut inner = replica.inner.lock().await;
    inner.migration_syntax_capacity(32768);
    reconstruct(&inner, &frame).await.unwrap();
    inner
        .conn
        .execute(
            "CREATE TEMP TABLE syntax_parent(id INTEGER PRIMARY KEY)",
            (),
        )
        .await
        .unwrap();
    inner.conn.execute("CREATE TEMP TABLE syntax_child(id INTEGER REFERENCES syntax_parent(id) DEFERRABLE INITIALLY DEFERRED)", ()).await.unwrap();
    let before = snapshot(&inner).await;
    let failed: Result<(), Error> = transaction!(inner, |inner| {
        signed_bytes(inner, other.to_bytes().unwrap()).await;
        let changed = reconstruct(inner, &frame).await?;
        assert_eq!(descriptor(&changed), &other);
        same_error(
            inner
                .check_stored_migration_for_test(&changed, now()?)
                .await
                .unwrap_err(),
            crate::credential(rocky_credentials::Error::Denied(
                "invalid tip statement signature".into(),
            )),
        );
        inner
            .conn
            .execute(
                "DELETE FROM secure_migration_frames WHERE hash=?",
                [frame.id()?.as_bytes().to_vec()],
            )
            .await?;
        assert!(reconstruct(inner, &frame).await?.migration.is_none());
        inner
            .conn
            .execute("INSERT INTO syntax_child VALUES(1)", ())
            .await?;
        Ok(())
    });
    let error = failed.unwrap_err();
    assert!(
        error.to_string().contains("FOREIGN KEY constraint failed"),
        "{error}"
    );
    assert_eq!(snapshot(&inner).await, before);
    assert_eq!(inner.migration_syntax_stats().0, 2);
    transaction!(inner, |inner| {
        let restored = reconstruct(inner, &frame).await?;
        same_frame(&restored, &frame);
        let statement = inner
            .check_stored_migration_for_test(&restored, now()?)
            .await?
            .unwrap();
        inner
            .conn
            .execute(
                "DELETE FROM secure_migration_frames WHERE hash=?",
                [frame.id()?.as_bytes().to_vec()],
            )
            .await?;
        assert!(reconstruct(inner, &frame).await?.migration.is_none());
        inner
            .classify_stored_migration_for_test(&statement, &restored)
            .await?;
        inner
            .classify_stored_migration_for_test(&statement, &restored)
            .await?;
        Ok(())
    })
    .unwrap();
    assert_eq!(inner.migration_syntax_stats().0, 3);
    same_frame(&reconstruct(&inner, &frame).await.unwrap(), &frame);
    assert_eq!(snapshot(&inner).await, before);
    drop(inner);
    assert_eq!(replica.migration_storage().await.unwrap(), storage);
    replica.audit(l.scope("items")).await.unwrap();
}
