use super::*;
use std::time::{Duration, Instant};

#[tokio::test]
async fn finalization_100000_rows_matches_full_rebuild_for_quiet_and_300_row_cuts() {
    let lineage = Lineage::pinned(201);
    let dir = TempDir::new().unwrap();
    let replica = open(&lineage, &dir, "large", &PrivateKey::from_bytes(&[202; 32])).await;
    follow_main(&lineage, &replica).await;
    let writer = session(&replica).await;
    let seeded = Instant::now();
    for chunk in 0..100 {
        writer.run("BEGIN", vec![]).await.unwrap();
        for row in chunk * 1000..(chunk + 1) * 1000 {
            identity_insert(
                &writer,
                "INSERT INTO items(key,title) VALUES(?,?)",
                vec![text(&format!("k{row:06}")), text("original")],
            )
            .await
            .unwrap();
        }
        writer.run("COMMIT", vec![]).await.unwrap();
    }
    println!(
        "FINALIZATION seed_rows=100000 seed_seconds={:.6}",
        seeded.elapsed().as_secs_f64()
    );
    {
        let inner = replica.inner.lock().await;
        inner.conn.execute("CREATE TEMP TABLE finalization_rows_written(scope INTEGER NOT NULL,row BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID", ()).await.unwrap();
        for (event, reference) in [
            ("INSERT", "NEW"),
            ("UPDATE OF fields", "NEW"),
            ("DELETE", "OLD"),
        ] {
            inner.conn.execute(format!("CREATE TEMP TRIGGER finalization_writes_{} AFTER {event} ON secure_rows BEGIN INSERT INTO finalization_rows_written SELECT {reference}.scope,{reference}.row WHERE NOT EXISTS(SELECT 1 FROM finalization_rows_written WHERE scope={reference}.scope AND row={reference}.row); END", event.split_whitespace().next().unwrap()), ()).await.unwrap();
        }
    }
    let before = timings(&replica).await;
    let started = Instant::now();
    seal(&lineage, &replica).await;
    replica.finalize_path(lineage.domain).await.unwrap();
    let elapsed = started.elapsed();
    let after = timings(&replica).await;
    let quiet = after.0 - before.0;
    println!(
        "FINALIZATION quiet_ms={:.3} oracle_seconds={:.6} publication_total_seconds={:.6}",
        quiet.as_secs_f64() * 1000.,
        (after.1 - before.1).as_secs_f64(),
        elapsed.as_secs_f64()
    );
    assert!(quiet <= Duration::from_millis(200), "quiet: {quiet:?}");
    assert_eq!(
        scalar(&replica, "SELECT count(*) FROM finalization_rows_written").await,
        0,
        "quiet finalization must write no authoritative row"
    );
    assert!(after.1 > before.1, "full rebuild comparison must run");
    assert_eq!(scalar(&replica, "SELECT count(*) FROM items").await, 100000);
    let frontier = replica.seal_frontier(lineage.domain, false).await.unwrap();
    let tip = replica.followed_tip().await;
    let position = scalar(&replica, "SELECT max(position) FROM secure_applied").await;
    for operation in 0..10 {
        writer
            .run(
                "UPDATE items SET title='excluded edit' WHERE key>=? AND key<?",
                vec![
                    text(&format!("k{:06}", operation * 30)),
                    text(&format!("k{:06}", (operation + 1) * 30)),
                ],
            )
            .await
            .unwrap();
    }
    assert_eq!(
        scalar(
            &replica,
            &format!("SELECT count(*) FROM secure_applied WHERE position>{position}")
        )
        .await,
        10
    );
    assert_eq!(
        scalar(
            &replica,
            &format!(
                "SELECT count(DISTINCT row) FROM secure_row_positions WHERE position>{position}"
            )
        )
        .await,
        300
    );
    replica
        .inner
        .lock()
        .await
        .conn
        .execute("DELETE FROM finalization_rows_written", ())
        .await
        .unwrap();
    let before = timings(&replica).await;
    let started = Instant::now();
    publish_seal(&lineage, &replica, sign_seal(&lineage, tip, frontier)).await;
    replica.finalize_path(lineage.domain).await.unwrap();
    let elapsed = started.elapsed();
    let after = timings(&replica).await;
    let changed = after.0 - before.0;
    println!(
        "FINALIZATION excluded_operations=10 touched_rows=300 incremental_ms={:.3} oracle_seconds={:.6} publication_total_seconds={:.6}",
        changed.as_secs_f64() * 1000.,
        (after.1 - before.1).as_secs_f64(),
        elapsed.as_secs_f64()
    );
    assert!(changed <= Duration::from_secs(1), "300 rows: {changed:?}");
    assert_eq!(
        scalar(&replica, "SELECT count(*) FROM finalization_rows_written").await,
        300,
        "other authoritative rows must remain untouched"
    );
    assert!(after.1 > before.1);
    assert_eq!(
        scalar(
            &replica,
            "SELECT count(*) FROM items WHERE title='excluded edit'"
        )
        .await,
        300
    );
    assert_eq!(scalar(&replica, "SELECT count(*) FROM items").await, 100000);
    replica.audit(lineage.scope("items")).await.unwrap();
}

async fn timings(replica: &Replica) -> (Duration, Duration) {
    let inner = replica.inner.lock().await;
    assert!(inner.compare_finalization);
    (inner.finalization_time, inner.finalization_oracle_time)
}

#[tokio::test]
async fn failed_incremental_finalization_rolls_back_rows_positions_drops_and_revision() {
    let lineage = Lineage::pinned(205);
    let dir = TempDir::new().unwrap();
    let replica = open(
        &lineage,
        &dir,
        "rollback",
        &PrivateKey::from_bytes(&[206; 32]),
    )
    .await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "live", "original").await;
    insert(&replica, "dead", "original").await;
    delete(&replica, "dead").await;
    let frontier = replica.seal_frontier(lineage.domain, false).await.unwrap();
    let tip = replica.followed_tip().await;
    update(&replica, "live", "excluded").await;
    let scope = lineage.scope("items");
    replica.defer_finalization_for_test(scope, true).await;
    publish_seal(&lineage, &replica, sign_seal(&lineage, tip, frontier)).await;
    replica.defer_finalization_for_test(scope, false).await;
    let before = super::audit::snapshot(&replica).await;
    replica.inject_failure(0, 1).await;
    assert!(matches!(
        replica.finalize_path(lineage.domain).await,
        Err(Error::InjectedFailure)
    ));
    assert_eq!(super::audit::snapshot(&replica).await, before);
    replica.finalize_path(lineage.domain).await.unwrap();
    assert_eq!(
        titles(&replica).await,
        vec![vec![text("live"), text("excluded")]]
    );
    assert_eq!(
        scalar(&replica, "SELECT count(*) FROM secure_finalization_rows").await,
        0
    );
    replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn audit_refuses_missing_row_positions_and_forged_birth_order() {
    let lineage = Lineage::pinned(207);
    let dir = TempDir::new().unwrap();
    let replica = open(
        &lineage,
        &dir,
        "positions",
        &PrivateKey::from_bytes(&[208; 32]),
    )
    .await;
    insert(&replica, "row", "original").await;
    let scope = lineage.scope("items");
    let inner = replica.inner.lock().await;
    let saved = crate::storage::rows(&inner.conn, "SELECT * FROM secure_row_positions")
        .await
        .unwrap();
    assert_eq!(saved.len(), 1);
    inner
        .conn
        .execute("DELETE FROM secure_row_positions", ())
        .await
        .unwrap();
    drop(inner);
    assert!(
        replica
            .audit(scope)
            .await
            .unwrap_err()
            .to_string()
            .contains("row positions differ")
    );
    let inner = replica.inner.lock().await;
    inner
        .conn
        .execute(
            "INSERT INTO secure_row_positions VALUES(?,?,?)",
            crate::storage::bindings(saved[0].clone()),
        )
        .await
        .unwrap();
    inner
        .conn
        .execute("UPDATE secure_birth_positions SET sequence=sequence+1", ())
        .await
        .unwrap();
    drop(inner);
    assert!(
        replica
            .audit(scope)
            .await
            .unwrap_err()
            .to_string()
            .contains("birth positions differ")
    );
    replica
        .inner
        .lock()
        .await
        .conn
        .execute("UPDATE secure_birth_positions SET sequence=sequence-1", ())
        .await
        .unwrap();
    replica
        .inner
        .lock()
        .await
        .conn
        .execute("UPDATE secure_scope_shapes SET records=records+1", ())
        .await
        .unwrap();
    assert!(
        replica
            .audit(scope)
            .await
            .unwrap_err()
            .to_string()
            .contains("scope shape counts differ")
    );
    replica
        .inner
        .lock()
        .await
        .conn
        .execute("UPDATE secure_scope_shapes SET records=records-1", ())
        .await
        .unwrap();
    replica.audit(scope).await.unwrap();
}
