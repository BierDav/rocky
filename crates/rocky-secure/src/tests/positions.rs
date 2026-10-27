//! Range indexes preserve signed coordinates through lifecycle changes.
use super::*;
use crate::storage::rows;
use rocky_core::SqlValue;
use rocky_credentials::{AuthorMark, SEAL_FORMAT, SealAction, SealStatement, SignedSeal};
use std::time::{Duration, Instant};

#[tokio::test]
async fn audit_rejects_altered_position_indexes() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(
        dir.path().join("corrupt.db"),
        f.app(),
        f.config(&node, f.full(), &[f.public]),
    )
    .await
    .unwrap();
    replica
        .author(
            [1; 32],
            vec![change(f.public, 1, None, Some(f.app().name("one")))],
            vec![],
        )
        .await
        .unwrap();
    replica.audit(f.public).await.unwrap();
    for (alter, restore) in [
        (
            "UPDATE secure_position_ranges SET end=end+1",
            "UPDATE secure_position_ranges SET end=end-1",
        ),
        (
            "UPDATE secure_log_ranges SET end=end+1",
            "UPDATE secure_log_ranges SET end=end-1",
        ),
        (
            "UPDATE secure_position_sources SET existence=existence+1",
            "UPDATE secure_position_sources SET existence=existence-1",
        ),
    ] {
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(alter, ())
            .await
            .unwrap();
        assert!(
            matches!(replica.audit(f.public).await, Err(Error::Corrupt(_))),
            "{alter}"
        );
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(restore, ())
            .await
            .unwrap();
        replica.audit(f.public).await.unwrap();
    }
}

#[tokio::test]
async fn position_ranges_merge_split_and_rollback_with_duplicate_custody() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(
        dir.path().join("runs.db"),
        f.app(),
        f.config(&node, f.full(), &[f.public]),
    )
    .await
    .unwrap();
    let frame = replica
        .author(
            [1; 32],
            vec![change(f.public, 1, None, Some(f.app().name("one")))],
            vec![],
        )
        .await
        .unwrap();
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    inner.conn.execute("SAVEPOINT ranges", ()).await.unwrap();
    let mut header = frame.operation().unwrap().header;
    let start = header.extensions.author_sequence;
    // Sparse arrival, bridging, splitting, and then reinserting the middle.
    for sequence in [start + 2, start + 1] {
        header.seq_num = sequence - start;
        header.extensions.author_sequence = sequence;
        header.sign(&node);
        crate::replica::positions::insert(&inner.conn, &header, 0, false, true)
            .await
            .unwrap();
    }
    let ranges = || {
        rows(
            &inner.conn,
            "SELECT start,end FROM secure_position_ranges WHERE kind=0",
        )
    };
    assert_eq!(
        ranges().await.unwrap(),
        vec![vec![
            SqlValue::Integer(start as i64),
            SqlValue::Integer((start + 2) as i64)
        ]]
    );
    inner
        .conn
        .execute(
            "DELETE FROM secure_position_sources WHERE log_sequence=1",
            (),
        )
        .await
        .unwrap();
    assert_eq!(ranges().await.unwrap().len(), 2);
    header.seq_num = 1;
    header.extensions.author_sequence = start + 1;
    header.extensions.existence = 3;
    header.sign(&node);
    crate::replica::positions::insert(&inner.conn, &header, 0, false, true)
        .await
        .unwrap();
    crate::replica::positions::insert(&inner.conn, &header, 1, false, true)
        .await
        .unwrap();
    let mut pending = header.clone();
    pending.extensions.existence = 7;
    pending.timestamp += 1;
    pending.sign(&node);
    crate::replica::positions::insert(&inner.conn, &pending, 0, true, false)
        .await
        .unwrap();
    assert_eq!(
        rows(
            &inner.conn,
            "SELECT count,settled_count FROM secure_existence_counts"
        )
        .await
        .unwrap(),
        vec![vec![SqlValue::Integer(7), SqlValue::Integer(3)]]
    );
    inner
        .conn
        .execute(
            "UPDATE secure_position_sources SET pending=0,applied=1 WHERE hash=? AND kind=0",
            [pending.hash().as_bytes().to_vec()],
        )
        .await
        .unwrap();
    assert_eq!(
        rows(
            &inner.conn,
            "SELECT count,settled_count FROM secure_existence_counts"
        )
        .await
        .unwrap(),
        vec![vec![SqlValue::Integer(7), SqlValue::Integer(7)]]
    );
    inner
        .conn
        .execute(
            "DELETE FROM secure_position_sources WHERE hash=? AND kind=0",
            [pending.hash().as_bytes().to_vec()],
        )
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "DELETE FROM secure_position_sources WHERE kind=0 AND log_sequence=1",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows(&inner.conn, "SELECT start,end FROM secure_log_ranges")
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        rows(
            &inner.conn,
            "SELECT count,settled_count FROM secure_existence_counts"
        )
        .await
        .unwrap(),
        vec![vec![SqlValue::Integer(3), SqlValue::Integer(3)]]
    );
    inner.conn.execute("ROLLBACK TO ranges", ()).await.unwrap();
    inner.conn.execute("RELEASE ranges", ()).await.unwrap();
    inner.audit_positions_for_test().await.unwrap();
}

#[tokio::test]
async fn positions_10000_waiting_seal_and_invalidated_frontier_take_five_milliseconds() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(
        dir.path().join("10000.db"),
        f.app(),
        f.config(&node, f.full(), &[f.public]),
    )
    .await
    .unwrap();
    let mut last = None;
    for n in 0_u64..10_000 {
        let mut row = [0; 32];
        row[..8].copy_from_slice(&n.to_be_bytes());
        let frame = replica
            .author(
                row,
                vec![RowChange {
                    scope: f.public,
                    row,
                    before: None,
                    after: Some(f.app().name("row")),
                }],
                vec![],
            )
            .await
            .unwrap();
        last = Some(frame.operation().unwrap().header);
    }
    let last = last.unwrap();
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    assert_eq!(
        rows(&inner.conn, "SELECT count(*) FROM secure_operations")
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(10_000)]]
    );
    assert_eq!(
        rows(&inner.conn, "SELECT count(*) FROM secure_position_ranges")
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(1)]]
    );
    assert_eq!(
        rows(&inner.conn, "SELECT count(*) FROM secure_log_ranges")
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(1)]]
    );
    let waiting = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: f.public.domain,
            tip: last.extensions.tip,
            frontier: vec![AuthorMark {
                author: last.extensions.author.unwrap(),
                sequence: last.extensions.author_sequence + 1,
            }],
            action: SealAction::Continue {
                schema: f.app().definition().hash(),
            },
            recovery: None,
        },
        &f.publisher,
    )
    .unwrap();
    let scopes = [(
        f.public,
        inner.position_scope_id_for_test(f.public).await.unwrap(),
    )];
    let mut readiness = Duration::ZERO;
    let mut frontier = Duration::ZERO;
    let before = inner.frontier_cache_stats().3;
    for _ in 0..10 {
        let oracle = inner.position_oracle_stats().2;
        let clock = Instant::now();
        assert!(
            inner
                .seal_waits_for_test(&waiting, &scopes)
                .await
                .unwrap()
                .is_some()
        );
        let elapsed = clock.elapsed().saturating_sub(Duration::from_nanos(
            inner.position_oracle_stats().2 - oracle,
        ));
        readiness = readiness.max(elapsed);
        inner.invalidate_frontiers();
        let oracle = inner.position_oracle_stats().2;
        let clock = Instant::now();
        assert_eq!(inner.frontier(f.public).await.unwrap().logs[0].held, 10_000);
        let elapsed = clock.elapsed().saturating_sub(Duration::from_nanos(
            inner.position_oracle_stats().2 - oracle,
        ));
        frontier = frontier.max(elapsed);
    }
    eprintln!(
        "POSITIONS frames=10000 seal_waits_max={readiness:?} invalidated_frontier_max={frontier:?} oracle={:?}",
        inner.position_oracle_stats()
    );
    assert_eq!(
        inner.frontier_cache_stats().3,
        before,
        "normal queries decoded a header"
    );
    assert!(readiness <= Duration::from_millis(5), "{readiness:?}");
    assert!(frontier <= Duration::from_millis(5), "{frontier:?}");
    inner.audit_positions_for_test().await.unwrap();
}

#[tokio::test]
async fn audit_rejects_a_range_table_with_the_wrong_shape() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(
        dir.path().join("shape.db"),
        f.app(),
        f.config(&node, f.full(), &[f.public]),
    )
    .await
    .unwrap();
    replica
        .author(
            [1; 32],
            vec![change(f.public, 1, None, Some(f.app().name("one")))],
            vec![],
        )
        .await
        .unwrap();
    {
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute("DROP TABLE secure_position_ranges", ())
            .await
            .unwrap();
        inner
            .conn
            .execute("CREATE TABLE secure_position_ranges(value INTEGER)", ())
            .await
            .unwrap();
        inner
            .conn
            .execute("INSERT INTO secure_position_ranges VALUES(1)", ())
            .await
            .unwrap();
    }
    assert!(matches!(
        replica.audit(f.public).await,
        Err(Error::Corrupt(_))
    ));
}
