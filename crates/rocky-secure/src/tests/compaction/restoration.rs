//! Restoration uses exact original frames, independently of target projection.
use super::*;
use crate::storage::rows;
mod fixture;
mod higher;
mod links;
mod negatives;
mod roots;
mod voids;
use fixture::*;

#[tokio::test]
async fn an_endpoint_first_held_prefix_waits_across_reopen_until_the_last_gap_arrives() {
    let mut f = HeldPrefix::new(3).await;
    let scope = f.lineage.scope("items");
    assert_eq!(
        f.replica.receive(f.frames[2].clone()).await.unwrap(),
        crate::ReceiveStatus::Held
    );
    f.waits().await;
    f = f.reopen().await;
    f.waits().await;
    assert_eq!(
        f.replica.receive(f.frames[0].clone()).await.unwrap(),
        crate::ReceiveStatus::Held
    );
    f.waits().await;
    f.replica.receive(f.frames[1].clone()).await.unwrap();
    let before = held(&f.replica).await;
    f.replica.restored(scope).await.unwrap();
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    assert_eq!(held(&f.replica).await, before);
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_applied").await,
        0
    );
    assert!(titles(&f.replica).await.is_empty());
    f.replica.audit(scope).await.unwrap();
    f = f.reopen().await;
    assert_eq!(held(&f.replica).await, before);
    insert(&f.replica, "new", "target line").await;
    assert_eq!(
        titles(&f.replica).await,
        vec![vec![text("new"), text("target line")]]
    );
    f.replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn a_failed_completion_transaction_preserves_every_authoritative_table() {
    let mut f = HeldPrefix::new(3).await;
    let scope = f.lineage.scope("items");
    for frame in &f.frames {
        f.replica.receive(frame.clone()).await.unwrap();
    }
    let before = super::audit::snapshot(&f.replica).await;
    f.replica.inner.lock(crate::replica::Class::Background).await.conn.execute("CREATE TEMP TRIGGER restoration_commit_failure AFTER DELETE ON secure_evicted_logs BEGIN SELECT RAISE(ABORT,'restoration after delete'); END", ()).await.unwrap();
    assert!(
        f.replica
            .restored(scope)
            .await
            .unwrap_err()
            .to_string()
            .contains("restoration after delete")
    );
    assert_eq!(super::audit::snapshot(&f.replica).await, before);
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute("DROP TRIGGER restoration_commit_failure", ())
        .await
        .unwrap();
    let held = held(&f.replica).await;
    f.replica.restored(scope).await.unwrap();
    let committed = super::audit::snapshot(&f.replica).await;
    f.replica.restored(scope).await.unwrap();
    assert_eq!(super::audit::snapshot(&f.replica).await, committed);
    assert_eq!(self::held(&f.replica).await, held);
    f.replica.audit(scope).await.unwrap();
    f = f.reopen().await;
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    f.replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn author_admission_clears_only_with_the_whole_write_transaction() {
    let mut f = negatives::filled().await;
    let scope = f.lineage.scope("items");
    let before = super::audit::snapshot(&f.replica).await;
    f.replica.inner.lock(crate::replica::Class::Background).await.conn.execute("CREATE TEMP TRIGGER restoration_author_failure AFTER DELETE ON secure_evicted_logs BEGIN SELECT RAISE(ABORT,'author restoration after delete'); END", ()).await.unwrap();
    let error = identity_insert(
        &session(&f.replica).await,
        "INSERT INTO items(key,title) VALUES('new','target line')",
        vec![],
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("author restoration after delete"),
        "{error}"
    );
    assert_eq!(super::audit::snapshot(&f.replica).await, before);
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute("DROP TRIGGER restoration_author_failure", ())
        .await
        .unwrap();
    let held = held(&f.replica).await;
    insert(&f.replica, "new", "target line").await;
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    let original = self::held(&f.replica)
        .await
        .into_iter()
        .filter(|row| row[3] == SqlValue::Text("held".into()))
        .collect::<Vec<_>>();
    assert_eq!(original, held);
    f.replica.audit(scope).await.unwrap();
    f = f.reopen().await;
    assert_eq!(
        titles(&f.replica).await,
        vec![vec![text("new"), text("target line")]]
    );
    f.replica.audit(scope).await.unwrap();
}
