use super::*;

async fn buffered(
    replica: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
) -> SecureSql<TypedRows> {
    replica
        .session([scope].into())
        .await
        .unwrap()
        .sql(SqlOptions {
            acknowledge: crate::CommitClass::Buffered,
            ..SqlOptions::default()
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn failed_flush_keeps_sql_maintenance_queued_until_retry() {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
    let replica = t
        .open(
            &dir.path().join("failed.db"),
            &PrivateKey::new(),
            t.full(),
            &[t.items],
        )
        .await;
    let sql = buffered(&replica, t.items).await;
    insert(&sql, "INSERT INTO items VALUES('a','first',0)", vec![])
        .await
        .unwrap();
    replica.fail_flushes(1).await;
    assert!(replica.flush().await.is_err());
    assert!(!replica.maintain().await);
    assert!(replica.maintenance_queued(t.items).await);
    replica.flush().await.unwrap();
    assert!(replica.maintain().await);
    assert!(!replica.maintenance_queued(t.items).await);
}

#[tokio::test]
async fn commit_during_flush_keeps_current_state_out_of_maintenance() {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
    let replica = t
        .open(
            &dir.path().join("concurrent.db"),
            &PrivateKey::new(),
            t.full(),
            &[t.items],
        )
        .await;
    let sql = buffered(&replica, t.items).await;
    insert(&sql, "INSERT INTO items VALUES('a','first',0)", vec![])
        .await
        .unwrap();
    let (entered, resume) = replica.pause_flush().await;
    let flush = replica.flush();
    tokio::pin!(flush);
    tokio::select! {
        result = &mut flush => panic!("flush did not pause: {result:?}"),
        result = entered => result.unwrap(),
    }
    let write = sql
        .run("UPDATE items SET title='second' WHERE key='a'", vec![])
        .await;
    resume.send(()).unwrap();
    write.unwrap();
    flush.await.unwrap();
    assert!(!replica.maintain().await);
    assert!(replica.maintenance_queued(t.items).await);
    replica.flush().await.unwrap();
    assert!(replica.maintain().await);
    assert!(!replica.maintenance_queued(t.items).await);
    assert_eq!(
        select(&sql, "SELECT title FROM items").await,
        vec![vec![text("second")]]
    );
}
