//! Genuine recovery-abandoned originals remain dispositions, never Applied.
use super::*;
use crate::storage::rows as sql_rows;

#[tokio::test]
async fn an_evicted_original_returns_abandoned_and_completes_its_exact_guard() {
    let stuck = Stuck::with_pending(false).await;
    let f = &stuck.fixture;
    let author = &stuck.author;
    let scope = f.lineage.scope("items");
    let mut originals = stored_frames(
        &author.1,
        "SELECT header,body FROM secure_operations WHERE status='applied' ORDER BY sequence",
    )
    .await;
    originals
        .retain(|frame| frame.operation_ref().unwrap().header.public_key == author.0.public_key());
    assert_eq!(originals.len(), 1);
    let original = originals[0].clone();
    assert_eq!(
        original.operation_ref().unwrap().header.public_key,
        author.0.public_key()
    );
    assert_eq!(original.operation_ref().unwrap().header.seq_num, 0);
    author
        .1
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
            full_nodes: [rocky_credentials::PeerId::of(stuck.carol.0.public_key())].into(),
            custody: 1,
        })
        .await
        .unwrap();
    author.1.subscribe(scope).await.unwrap();
    author
        .1
        .acknowledge_frontier(
            stuck.carol.0.public_key(),
            &f.lineage.credentials(3, &stuck.carol.0),
            &stuck.carol.1.frontier(scope).await.unwrap(),
        )
        .await
        .unwrap();
    assert!(author.1.evict(scope).await.unwrap().operations > 0);
    author.1.set_role(Default::default()).await.unwrap();
    let guarded = sql_rows(
        &author.1.inner.lock().await.conn,
        "SELECT sequence,hash FROM secure_evicted_logs",
    )
    .await
    .unwrap();
    assert_eq!(
        guarded,
        vec![vec![
            SqlValue::Integer(0),
            SqlValue::Blob(original.id().unwrap().as_bytes().to_vec())
        ]]
    );
    stuck.recover().await;
    share(&f.alice.1, &author.1).await;
    author.1.restored(scope).await.unwrap();
    assert_eq!(
        sql_rows(
            &author.1.inner.lock().await.conn,
            "SELECT sequence,hash FROM secure_evicted_logs"
        )
        .await
        .unwrap(),
        guarded
    );
    assert_eq!(
        author.1.receive(original.clone()).await.unwrap(),
        crate::ReceiveStatus::Abandoned
    );
    let before = sql_rows(
        &author.1.inner.lock().await.conn,
        "SELECT hash,header,body,status FROM secure_operations WHERE status='abandoned'",
    )
    .await
    .unwrap();
    assert_eq!(
        before,
        vec![vec![
            SqlValue::Blob(original.id().unwrap().as_bytes().to_vec()),
            SqlValue::Blob(original.header_bytes().to_vec()),
            SqlValue::Blob(original.body_bytes().to_vec()),
            SqlValue::Text("abandoned".into())
        ]]
    );
    author.1.restored(scope).await.unwrap();
    assert!(
        sql_rows(
            &author.1.inner.lock().await.conn,
            "SELECT * FROM secure_evicted_logs"
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        sql_rows(
            &author.1.inner.lock().await.conn,
            "SELECT hash,header,body,status FROM secure_operations WHERE status='abandoned'"
        )
        .await
        .unwrap(),
        before
    );
    assert!(sql_rows(&author.1.inner.lock().await.conn, "SELECT 1 FROM secure_applied a JOIN secure_operations o ON o.hash=a.hash WHERE o.status='abandoned'").await.unwrap().is_empty());
    sync(&f.lineage, &f.alice.1, &author.1, &author.0).await;
    assert_eq!(
        rows(&author.1).await,
        [row("0001", "kept"), row("0002", "continued")]
    );
    author.1.audit(scope).await.unwrap();
    let Stuck {
        fixture,
        author: (key, replica),
        carol,
    } = stuck;
    drop(carol);
    drop(replica);
    let reopened = open(&fixture.lineage, &fixture.dir, "alice", &key).await;
    assert_eq!(
        sql_rows(
            &reopened.inner.lock().await.conn,
            "SELECT hash,header,body,status FROM secure_operations WHERE status='abandoned'"
        )
        .await
        .unwrap(),
        before
    );
    assert_eq!(
        rows(&reopened).await,
        [row("0001", "kept"), row("0002", "continued")]
    );
    reopened.audit(scope).await.unwrap();
}
