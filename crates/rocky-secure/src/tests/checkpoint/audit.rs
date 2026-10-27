//! Global keyless records remain in the batch's domain even when omitted.
use super::*;

#[tokio::test]
async fn omitted_keyless_no_frames_records_are_checked_on_every_batch() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let (a, _) = open(&f, &dir.path().join("a.db"), &key).await;
    a.author(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    assert!(a.checkpoint(f.private).await.unwrap().statement.no_frames());
    assert!(a.key_epochs(f.private).await.is_empty());
    a.audit_scopes(&[f.public]).await.unwrap();
    let inner = a.inner.lock(crate::replica::Class::Background).await;
    inner.conn.execute("INSERT INTO secure_checkpoint_pages SELECT id,X'00',NULL,X'00' FROM secure_scope_state WHERE scope=?",[encode(&f.private).unwrap()]).await.unwrap();
    drop(inner);
    assert!(
        a.audit_scopes(&[f.public])
            .await
            .unwrap_err()
            .to_string()
            .contains("no-frames checkpoint")
    );
    a.inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute("DELETE FROM secure_checkpoint_pages", ())
        .await
        .unwrap();
    a.audit_scopes(&[f.public]).await.unwrap();
    let reports = a.audit_scopes(&[f.private, f.public]).await.unwrap();
    assert_eq!(reports[0].0, f.private);
    assert_eq!(
        (
            reports[0].1.operations,
            reports[0].1.rows,
            reports[0].1.checkpoint,
            reports[0].1.replayed
        ),
        (0, 0, None, 0)
    );
}
