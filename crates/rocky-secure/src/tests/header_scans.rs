//! Paged domain header scans: bounded per acquisition, restarted on change.
use super::compaction::{follow_main, insert};
use super::generations::Lineage;
use super::*;
use crate::replica::Class;

#[tokio::test]
async fn header_scans_page_their_reads_and_restart_on_a_changed_source() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let alice = lineage
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 3, None)
        .await
        .unwrap();
    follow_main(&lineage, &alice).await;
    for key in ["a", "b", "c", "d", "e"] {
        insert(&alice, key, "title").await;
    }
    let unpaged = alice
        .inner
        .lock(Class::Background)
        .await
        .seal_frontier(lineage.domain, false)
        .await
        .unwrap();

    let measurements = alice.lock_measurements();
    measurements.start();
    assert_eq!(
        alice.seal_frontier(lineage.domain, false).await.unwrap(),
        unpaged
    );
    let observation = alice.tip_observation(lineage.domain).await.unwrap();
    assert!(observation.operations_since_seal >= 5);
    let pages: Vec<u64> = measurements
        .stop()
        .holders
        .values()
        .flat_map(|holder| holder.units.get("domain_header").into_iter().flatten())
        .map(|(count, _)| *count)
        .collect();
    assert!(pages.len() > 2, "{pages:?}");
    assert!(
        pages
            .iter()
            .all(|count| *count <= crate::replica::headers::HEADER_ROWS as u64)
    );
    // Rows one uninterrupted frontier scan reads.
    let total: u64 = {
        let measurements = alice.lock_measurements();
        measurements.start();
        alice.seal_frontier(lineage.domain, false).await.unwrap();
        measurements
            .stop()
            .holders
            .values()
            .flat_map(|holder| holder.units.get("domain_header").into_iter().flatten())
            .map(|(count, _)| *count)
            .sum()
    };

    for (sql, invalidates) in [
        ("UPDATE secure_metadata SET revision=revision", false),
        ("UPDATE secure_operations SET status=status", true),
    ] {
        let (reached, waiting) = tokio::sync::oneshot::channel();
        let (resume, paused) = tokio::sync::oneshot::channel();
        alice.pause_header_scan_for_test(reached, paused).await;
        let scan = alice.seal_frontier(lineage.domain, false);
        let change = async {
            waiting.await.unwrap();
            let mut inner = alice.inner.lock(Class::Background).await;
            inner.conn.execute(sql, ()).await.unwrap();
            inner.note_commit().await;
            drop(inner);
            resume.send(()).unwrap();
        };
        let measurements = alice.lock_measurements();
        measurements.start();
        let (result, ()) = tokio::join!(scan, change);
        // A changed source restarts the scan instead of acting on it.
        assert_eq!(result.unwrap(), unpaged, "{sql}");
        let pages: u64 = measurements
            .stop()
            .holders
            .values()
            .flat_map(|holder| holder.units.get("domain_header").into_iter().flatten())
            .map(|(count, _)| *count)
            .sum();
        assert_eq!(pages > total, invalidates, "{sql}: {pages} of {total}");
    }
}
