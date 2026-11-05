//! Diagnostic release probe; excluded from correctness tiers.
use super::*;

#[tokio::test]
#[ignore = "release measurement; ROCKY_BASE_ROWS and ROCKY_BASE_SAMPLES select the fixture"]
#[cfg(not(target_family = "wasm"))]
async fn measure_replayed_base_compaction() {
    let count = std::env::var("ROCKY_BASE_ROWS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2048);
    let samples = std::env::var("ROCKY_BASE_SAMPLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5);
    let mut all_holds = Vec::new();
    let mut elapsed = Vec::new();
    for sample in 0..samples {
        let lineage = Lineage::new();
        let dir = Scratch::new().await;
        let replica = open(&lineage, &dir, "measure", &PrivateKey::new()).await;
        follow_main(&lineage, &replica).await;
        let sql = session(&replica).await;
        for from in (0..count).step_by(64) {
            sql.run("BEGIN", vec![]).await.unwrap();
            for index in from..(from + 64).min(count) {
                identity_insert(
                    &sql,
                    "INSERT INTO items(key,title) VALUES(?,?)",
                    vec![text(&format!("k{index}")), text(&"x".repeat(128))],
                )
                .await
                .unwrap();
            }
            sql.run("COMMIT", vec![]).await.unwrap();
        }
        seal(&lineage, &replica).await;
        replica.finalize_path(lineage.domain).await.unwrap();
        let measurements = replica.lock_measurements();
        measurements.start();
        let start = std::time::Instant::now();
        replica.compact(lineage.domain, 0).await.unwrap();
        elapsed.push(start.elapsed().as_micros() as u64);
        let measured = measurements.stop();
        let mut holds = Vec::new();
        for ((site, class), holder) in measured.holders {
            if class == "Background" {
                holds.extend(holder.hold_us);
            }
            for (unit, work) in holder.units {
                if unit.starts_with("transition_") {
                    println!(
                        "sample={sample} site={site} unit={unit} count={} max_per_hold={}",
                        work.iter().map(|(count, _)| count).sum::<u64>(),
                        work.iter().map(|(count, _)| count).max().unwrap_or(&0)
                    );
                }
            }
        }
        println!(
            "sample={sample} rows={count} elapsed_us={} holds={} max_hold_us={}",
            elapsed.last().unwrap(),
            holds.len(),
            holds.iter().max().unwrap_or(&0)
        );
        all_holds.extend(holds);
    }
    fn percentiles(mut values: Vec<u64>) -> Vec<u64> {
        values.sort_unstable();
        [50, 95, 99, 100]
            .into_iter()
            .map(|p| values[(values.len() * p).div_ceil(100).saturating_sub(1)])
            .collect()
    }
    println!(
        "rows={count} samples={samples} hold_us_p50_p95_p99_max={:?} elapsed_us_p50_p95_p99_max={:?}",
        percentiles(all_holds),
        percentiles(elapsed)
    );
}
