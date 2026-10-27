use super::*;
use crate::storage::{bindings, collect_rows, rows};

#[tokio::test]
async fn storage_accounting_bounds_inputs_and_validates_every_source_table() {
    let (_, _, _, replica, signed) = fixture::logged_candidate(40).await;
    let statement = &signed.statement;
    let (expected, records) = {
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        let totals = rows(&inner.conn,
            "SELECT sum(retained_frames),sum(retained_bytes),sum(length(statement)+coalesce(length(signed),0)+length(writer)+coalesce(length(conflict),0)+length(publication)+length(tip)) FROM secure_migration_publications").await.unwrap();
        let support = rows(&inner.conn,
            "SELECT (SELECT coalesce(sum(length(proof)+length(hash)+length(publication)+16),0) FROM secure_migration_frames)+(SELECT coalesce(sum(length(hash)+length(publication)+24),0) FROM secure_migration_nodes)").await.unwrap();
        let existence = collect_rows(inner.conn.query(
            "SELECT coalesce(sum(length(record)+length(scope)+length(signer)+length(hash)+32),0) FROM secure_existence_log WHERE signer=? AND author=? AND position>=0 AND position<?",
            bindings(vec![SqlValue::Blob(statement.writer.to_vec()), SqlValue::Integer(i64::from(statement.author)), SqlValue::Integer(statement.frames as i64)])).await.unwrap()).await.unwrap();
        let records = rows(&inner.conn,
            "SELECT (SELECT count(*) FROM secure_migration_publications)+(SELECT count(*) FROM secure_migration_frames)+(SELECT count(*) FROM secure_migration_nodes)+(SELECT count(*) FROM secure_existence_log)").await.unwrap();
        let integer = |value: &SqlValue| match value {
            SqlValue::Integer(n) => *n as u64,
            _ => panic!("counter"),
        };
        (
            crate::MigrationStorage {
                frames: integer(&totals[0][0]),
                bytes: integer(&totals[0][1]),
                support_bytes: integer(&totals[0][2])
                    + integer(&support[0][0])
                    + integer(&existence[0][0]),
            },
            integer(&records[0][0]),
        )
    };
    let measurements = replica.lock_measurements();
    measurements.start();
    assert_eq!(replica.migration_storage().await.unwrap(), expected);
    let measured = measurements.stop();
    let work: Vec<_> = measured
        .holders
        .values()
        .flat_map(|holder| {
            holder
                .units
                .get("migration_storage_record")
                .into_iter()
                .flatten()
        })
        .map(|(count, _)| *count)
        .collect();
    assert_eq!(work.iter().sum::<u64>(), records);
    assert!(work.len() > 4 && work.iter().all(|count| *count == 1));

    for (query, invalidates) in [
        ("UPDATE secure_metadata SET revision=revision", false),
        (
            "UPDATE secure_migration_publications SET signed=signed",
            true,
        ),
        ("UPDATE secure_migration_frames SET proof=proof", true),
        ("UPDATE secure_migration_nodes SET hash=hash", true),
        ("UPDATE secure_existence_log SET record=record", true),
    ] {
        let (reached, waiting) = tokio::sync::oneshot::channel();
        let (resume, paused) = tokio::sync::oneshot::channel();
        replica
            .pause_migration_storage_for_test(reached, paused)
            .await;
        let change = async {
            waiting.await.unwrap();
            replica
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .conn
                .execute(query, ())
                .await
                .unwrap();
            resume.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(replica.migration_storage(), change);
        if invalidates {
            assert!(matches!(result, Err(Error::RetryExhausted)), "{query}");
        } else {
            assert_eq!(result.unwrap(), expected);
        }
        assert_eq!(replica.migration_storage().await.unwrap(), expected);
    }

    let (reached, waiting) = tokio::sync::oneshot::channel();
    let (_resume, paused) = tokio::sync::oneshot::channel();
    replica
        .pause_migration_storage_for_test(reached, paused)
        .await;
    tokio::select! {
        result = replica.migration_storage() => panic!("scan returned before interruption: {result:?}"),
        result = waiting => result.unwrap(),
    }
    assert_eq!(replica.migration_storage().await.unwrap(), expected);
}
