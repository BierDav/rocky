//! Privileged higher-log fixtures must still bridge the exact guarded hash.
use super::*;
use crate::storage::bindings;

#[tokio::test]
async fn a_signed_higher_fork_refuses_both_completion_callers_until_its_exact_bridge_returns() {
    let f = super::negatives::filled().await;
    let scope = f.lineage.scope("items");
    let original = f.frames[2].operation_ref().unwrap().header.clone();
    let mut fork = original.clone();
    fork.timestamp += 1;
    fork.sign(&f.key);
    let mut higher = original.clone();
    higher.seq_num += 1;
    higher.timestamp += 2;
    higher.backlink = Some(fork.hash());
    higher.sign(&f.key);
    // These signed retained/log records are privileged negative injections.
    // Unknown own frames above G are still refused by ordinary intake.
    f.replica.inner.lock(crate::replica::Class::Background).await.conn.execute(
        "INSERT INTO secure_operations(hash,scope,author,sequence,header,body,chain,status) SELECT ?1,scope,author,?2,?3,body,chain,status FROM secure_operations WHERE hash=?4",
        bindings(vec![SqlValue::Blob(higher.hash().as_bytes().to_vec()), SqlValue::Integer(higher.seq_num as i64), SqlValue::Blob(higher.to_bytes()), SqlValue::Blob(original.hash().as_bytes().to_vec())]),
    ).await.unwrap();
    f.replica.inner.lock(crate::replica::Class::Background).await.conn.execute(
        "INSERT INTO secure_logs SELECT id,?1,?2,?3,?4,-1 FROM secure_scope_state WHERE scope=?5",
        bindings(vec![SqlValue::Blob(f.key.public_key().as_bytes().to_vec()), SqlValue::Integer(higher.seq_num as i64), SqlValue::Blob(higher.hash().as_bytes().to_vec()), SqlValue::Integer(higher.timestamp as i64), SqlValue::Blob(crate::encode(&scope).unwrap())]),
    ).await.unwrap();
    super::negatives::preserved(&f, false).await;
    let before = super::super::audit::snapshot(&f.replica).await;
    f.waits().await;
    assert_eq!(super::super::audit::snapshot(&f.replica).await, before);
    let old_hash = higher.hash();
    higher.backlink = Some(original.hash());
    higher.sign(&f.key);
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET hash=?,header=? WHERE hash=?",
            [
                higher.hash().as_bytes().to_vec(),
                higher.to_bytes(),
                old_hash.as_bytes().to_vec(),
            ],
        )
        .await
        .unwrap();
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_logs SET hash=? WHERE author=?",
            [
                higher.hash().as_bytes().to_vec(),
                f.key.public_key().as_bytes().to_vec(),
            ],
        )
        .await
        .unwrap();
    let held = held(&f.replica).await;
    f.replica.restored(scope).await.unwrap();
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    assert_eq!(self::held(&f.replica).await, held);
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_applied").await,
        0
    );
    // Restore the privileged store extension before the ordinary audit.
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "DELETE FROM secure_operations WHERE hash=?",
            [higher.hash().as_bytes().to_vec()],
        )
        .await
        .unwrap();
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "DELETE FROM secure_logs WHERE author=?",
            [f.key.public_key().as_bytes().to_vec()],
        )
        .await
        .unwrap();
    f.replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn admitted_historical_frames_do_not_need_a_fresh_unexpired_grant() {
    let f = super::negatives::filled().await;
    let scope = f.lineage.scope("items");
    let held = held(&f.replica).await;
    crate::CLOCK_SKEW.with(|skew| skew.set(skew.get() + 301 * rocky_credentials::DAY));
    // Exercise the shared author-admission caller within a rolled-back write
    // transaction: current-authority gates outside it still reject expired grants.
    let inner = f
        .replica
        .inner
        .lock(crate::replica::Class::Background)
        .await;
    inner.conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    let result = inner.admit_author(scope, None).await;
    let cleared = rows(&inner.conn, "SELECT count(*) FROM secure_evicted_logs")
        .await
        .unwrap();
    inner.conn.execute("ROLLBACK", ()).await.unwrap();
    drop(inner);
    // Restore the process clock before current-authority and audit checks.
    crate::CLOCK_SKEW.with(|skew| skew.set(skew.get() - 301 * rocky_credentials::DAY));
    result.unwrap();
    assert_eq!(cleared, vec![vec![SqlValue::Integer(0)]]);
    f.replica.restored(scope).await.unwrap();
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    assert_eq!(self::held(&f.replica).await, held);
    f.replica.audit(scope).await.unwrap();
}
