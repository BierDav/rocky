//! Historical concrete addresses remain bound after their table is removed.
use super::*;
use crate::storage::bindings;

#[tokio::test]
async fn restoration_requires_the_retained_partition_tuple_without_a_current_table() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::build_with(true, PrivateKey::from_bytes(&[187; 32]));
    let key = PrivateKey::from_bytes(&[188; 32]);
    let writer = keyed(&f, &root.path().join("writer"), [188; 32], 1).await;
    let scope = f.scope("A");
    let frame = writer
        .author(
            [1; 32],
            vec![f.titled("A", 1, 1, "historical").change],
            vec![],
        )
        .await
        .unwrap();
    let header = frame.operation_ref().unwrap().header.clone();
    // Real exposure prevents local removal from voiding the original before
    // the retained-frame corruption control reaches the historical verifier.
    let peer = keyed(&f, &root.path().join("peer"), [189; 32], 2).await;
    let credentials = security(&f, &peer).await.credentials;
    let page = writer
        .range_page(
            PrivateKey::from_bytes(&[189; 32]).public_key(),
            &credentials,
            &crate::replica::ScopeFrontier {
                scope,
                logs: vec![],
            },
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert_eq!(page.frames.len(), 1);
    assert_eq!(header.public_key, key.public_key());
    let (absent, statements) = removed_path(
        &f,
        writer.followed_tip().await,
        vec![AuthorMark {
            author: 1,
            sequence: 0,
        }],
    );
    writer.receive_tip_statements(&statements).await.unwrap();
    switch(&f, &writer, "dev", &absent).await;
    assert!(!writer.inner.lock().await.tables.contains(&scope.table));
    assert_eq!(count(&writer, "SELECT 1 FROM secure_operations").await, 1);
    let logs = rows(&writer.inner.lock().await.conn, "SELECT * FROM secure_logs")
        .await
        .unwrap();
    // Privileged stored-envelope corruption, not an admissible frame:
    // keep SQL hash/index/guard coherent so only the historical tuple is bad.
    let mut malformed = header.clone();
    malformed.extensions.partition = None;
    malformed.sign(&key);
    let old = header.hash().as_bytes().to_vec();
    let changed = malformed.hash().as_bytes().to_vec();
    writer
        .merge_restoration_for_test(scope, (0, Some(malformed.hash())))
        .await
        .unwrap();
    let inner = writer.inner.lock().await;
    inner
        .conn
        .execute("DELETE FROM secure_logs", ())
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "UPDATE secure_operations SET hash=?,header=? WHERE hash=?",
            [changed.clone(), malformed.to_bytes(), old.clone()],
        )
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "UPDATE secure_applied SET hash=? WHERE hash=?",
            [changed.clone(), old.clone()],
        )
        .await
        .unwrap();
    drop(inner);
    let before = crate::tests::compaction::audit::snapshot(&writer).await;
    let error = writer.restored(scope).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("restoration partition tuple missing"),
        "{error}"
    );
    assert_eq!(
        crate::tests::compaction::audit::snapshot(&writer).await,
        before
    );
    let inner = writer.inner.lock().await;
    inner
        .conn
        .execute(
            "UPDATE secure_operations SET hash=?,header=? WHERE hash=?",
            [old.clone(), header.to_bytes(), changed.clone()],
        )
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "UPDATE secure_applied SET hash=? WHERE hash=?",
            [old.clone(), changed],
        )
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "UPDATE secure_evicted_logs SET hash=? WHERE scope=?",
            [old, encode(&scope).unwrap()],
        )
        .await
        .unwrap();
    drop(inner);
    // Restore the injected obligation along with the rest of the corruption
    // fixture; this is not a claim of normal guard discharge.
    writer
        .inner
        .lock()
        .await
        .conn
        .execute(
            "DELETE FROM secure_evicted_logs WHERE scope=?",
            [encode(&scope).unwrap()],
        )
        .await
        .unwrap();
    let inner = writer.inner.lock().await;
    for log in logs {
        inner
            .conn
            .execute("INSERT INTO secure_logs VALUES(?,?,?,?,?,?)", bindings(log))
            .await
            .unwrap();
    }
    drop(inner);
    writer.audit(scope).await.unwrap();
}
