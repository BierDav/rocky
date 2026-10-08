//! Combined trusted storage soak; qualification gaps are reported explicitly.
use super::wide_schema::{Policy, WideSchema, domain_id};
use super::{CALLER, Failure, NODE, resident_mib};
use rocky_core::{Envelope, MeshId, OperationId, Scope, SqlValue};
use rocky_runtime::{
    BoundQuery, DomainInstallation, Durability, Error, Identity, Mutation, ReadOptions, Replica,
    Runtime, RuntimeConfig, RuntimeOptions,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

#[path = "soak/support.rs"]
mod support;
#[path = "soak/workload.rs"]
mod workload;
pub use support::Config;
use support::{Counters, Latency};
#[cfg(test)]
use workload::table;
use workload::{Writer, commit, open, range, reads, scope};

pub async fn audit_existing(config: Config) -> Result<(), Failure> {
    let path = config.directory.join("soak.db");
    if !path.is_file() {
        return Err("no soak database to audit".into());
    }
    let started = Instant::now();
    let runtime = open(
        &path,
        WideSchema::new(config.tables).with_domains(3),
        config.durability,
        config.instrument,
    )
    .await?;
    println!("soak recovery reopen_ms={}", started.elapsed().as_millis());
    for domain in 1..=3 {
        let replica = runtime.domain(scope(domain))?;
        let audit = replica.audit().await?;
        let applied = replica
            .sync_state()
            .await?
            .applied
            .0
            .iter()
            .map(|(_, sequence)| sequence.get())
            .sum::<u64>();
        println!(
            "soak recovery audit domain={domain} rows={} applied={applied} retained={} recomputed={}",
            audit.rows, audit.retained_operations, audit.recomputed
        );
    }
    runtime.close().await?;
    Ok(())
}

fn histogram(label: &str, latency: &Mutex<Latency>) -> Result<(), Failure> {
    let mut histogram = latency.lock().map_err(|_| "latency lock poisoned")?;
    println!(
        "soak {label}: n={} p50_us<={} p99_us<={} max_us={}",
        histogram.count,
        histogram.percentile(50),
        histogram.percentile(99),
        histogram.max_us
    );
    *histogram = Latency::new();
    Ok(())
}

pub async fn run(config: Config) -> Result<(), Failure> {
    std::fs::create_dir_all(&config.directory)?;
    let path = config.directory.join("soak.db");
    if path.exists() {
        return Err(
            "soak requires a fresh directory; refusing to reuse an existing database".into(),
        );
    }
    println!(
        "soak os={} arch={} durability={:?} tables={} rows={} domains=3 observers={} minimum_seconds={} minimum_operations={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        config.durability,
        config.tables,
        config.rows,
        config.observers,
        config.seconds,
        config.operations
    );
    println!(
        "soak gaps: strong/effects, encrypted, topology, process faults, second hardware and power-loss are NOT qualified by this runner"
    );
    let app = WideSchema::new(config.tables).with_domains(3);
    let installed = Instant::now();
    let runtime = open(&path, app, config.durability, config.instrument).await?;
    println!(
        "soak install_ms={} resident_mib={}",
        installed.elapsed().as_millis(),
        resident_mib()
    );
    let stop = Arc::new(AtomicBool::new(false));
    let counters: Vec<_> = (0..3).map(|_| Arc::new(Counters::default())).collect();
    let latencies: Vec<_> = (0..3)
        .map(|_| Arc::new(Mutex::new(Latency::new())))
        .collect();
    commit(
        &runtime.domain(scope(1))?,
        None,
        WideSchema::create(0, 0, 0, "sentinel"),
        &counters[0],
        &latencies[0],
    )
    .await?;
    counters[0].creates.fetch_add(1, Relaxed);
    let point = Arc::new(Mutex::new(Latency::new()));
    let ranges = Arc::new(Mutex::new(Latency::new()));
    // Per-interval service time of each observer delivery: the read of the
    // subscribed range, plus any wait for the change notification. Under
    // continuous churn the wait is small against the read, so the histogram is
    // the observer-read cost; it is reset and printed every interval.
    let deliveries = Arc::new(Mutex::new(Latency::new()));
    let reader = tokio::spawn(reads(
        runtime.domain(scope(1))?,
        stop.clone(),
        point.clone(),
        ranges.clone(),
    ));
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let mut observers = Vec::new();
    for index in 0..config.observers {
        let mut observer = runtime.domain(scope(1))?.observe_read(
            range((index % usize::from(config.tables.min(50))) as u16),
            ReadOptions::new(scope(1), 32),
        )?;
        observer.next().await?;
        let mut receiver = receiver.clone();
        let counter = counters[0].clone();
        let deliveries = deliveries.clone();
        observers.push(tokio::spawn(async move {
            loop {
                let started = Instant::now();
                tokio::select! {
                    result = observer.next() => {
                        result?;
                        if let Ok(mut histogram) = deliveries.lock() {
                            histogram.record(started.elapsed());
                        }
                        counter.observations.fetch_add(1, Relaxed);
                    }
                    _ = receiver.changed() => return Ok::<_, Error>(()),
                }
            }
        }));
    }
    let started = Instant::now();
    let mut writers = Vec::new();
    for domain in 1..=3 {
        let (clients, rows) = if domain == 1 {
            (16.min(config.rows - 1), config.rows - 1)
        } else {
            (2, (config.rows / 100).max(16))
        };
        for client in 0..clients {
            writers.push(tokio::spawn(
                Writer {
                    replica: runtime.domain(scope(domain))?,
                    counters: counters[usize::from(domain - 1)].clone(),
                    latency: latencies[usize::from(domain - 1)].clone(),
                    stop: stop.clone(),
                    tables: config.tables,
                    rows,
                    client,
                    clients,
                }
                .run(),
            ));
        }
    }
    let replica = runtime.domain(scope(1))?;
    let mut previous = (Instant::now(), 0, replica.writer_stats());
    let mut previous_observations = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(config.interval)).await;
        // Surface a failed task before spending hours waiting for its counter.
        if writers.iter().any(tokio::task::JoinHandle::is_finished)
            || reader.is_finished()
            || observers.iter().any(tokio::task::JoinHandle::is_finished)
        {
            stop.store(true, Relaxed);
            shutdown.send(true)?;
            for task in writers {
                task.await??;
            }
            reader.await??;
            for task in observers {
                task.await??;
            }
            return Err("soak task stopped unexpectedly".into());
        }
        let now = Instant::now();
        let written = counters[0].writes.load(Relaxed);
        let observations = counters[0].observations.load(Relaxed);
        let stats = replica.writer_stats();
        let retention = replica.retention_status().await?;
        let size =
            |name: &str| std::fs::metadata(config.directory.join(name)).map_or(0, |m| m.len());
        println!(
            "soak t_s={} main_operations={} writes_per_s={:.0} observer_deliveries_per_s={:.0} live_rows={} observations={} resident_mib={} file_bytes={} wal_bytes={} retained={} checkpoint={} step_us_per_write={:.2} commit_us={} checkpoint_copy_us={} checkpoint_flush_us={} queue={}",
            started.elapsed().as_secs(),
            written,
            (written - previous.1) as f64 / (now - previous.0).as_secs_f64(),
            (observations - previous_observations) as f64 / (now - previous.0).as_secs_f64(),
            counters[0]
                .creates
                .load(Relaxed)
                .saturating_sub(counters[0].deletes.load(Relaxed)),
            observations,
            resident_mib(),
            size("soak.db"),
            size("soak.db-wal"),
            retention.retained_operations,
            retention.checkpoint.map_or(0, |c| c.record.sequence),
            (stats.step_micros - previous.2.step_micros) as f64
                / (stats.steps - previous.2.steps).max(1) as f64,
            stats.commit_micros - previous.2.commit_micros,
            stats.checkpoint_copy_micros - previous.2.checkpoint_copy_micros,
            stats.checkpoint_flush_micros - previous.2.checkpoint_flush_micros,
            replica.queue_depth().commands
        );
        if config.instrument {
            let per_write = |now: u64, before: u64| {
                (now.saturating_sub(before)) as f64
                    / (stats.authoring_steps - previous.2.authoring_steps).max(1) as f64
            };
            println!(
                "soak subphases t_s={} admission_us_per_write={:.2} reduction_us_per_write={:.2} encoding_us_per_write={:.2} sql_us_per_write={:.2} authoring_steps={} incoming_steps={} authoring_us_per_step={:.2} incoming_us_per_step={:.2} checkpoints_delta={}",
                started.elapsed().as_secs(),
                per_write(stats.admission_micros, previous.2.admission_micros),
                per_write(stats.reduction_micros, previous.2.reduction_micros),
                per_write(stats.encoding_micros, previous.2.encoding_micros),
                per_write(stats.sql_micros, previous.2.sql_micros),
                stats.authoring_steps,
                stats.incoming_steps,
                (stats.authoring_step_micros - previous.2.authoring_step_micros) as f64
                    / (stats.authoring_steps - previous.2.authoring_steps).max(1) as f64,
                (stats.incoming_step_micros - previous.2.incoming_step_micros) as f64
                    / (stats.incoming_steps - previous.2.incoming_steps).max(1) as f64,
                stats.checkpoints - previous.2.checkpoints
            );
        }
        if let Some(facts) = crate::attribution::storage_facts(&path) {
            println!(
                "soak storage t_s={} page_count={} freelist_count={} free_pct={:.2} db_bytes={} wal_bytes={}",
                started.elapsed().as_secs(),
                facts.page_count,
                facts.freelist_count,
                facts.freelist_count as f64 * 100.0 / facts.page_count.max(1) as f64,
                facts.database_bytes,
                facts.wal_bytes
            );
        }
        for (index, latency) in latencies.iter().enumerate() {
            histogram(&format!("domain={} writes", index + 1), latency)?;
        }
        histogram("point reads", &point)?;
        histogram("range two pages", &ranges)?;
        histogram("observer deliveries", &deliveries)?;
        previous = (now, written, stats);
        previous_observations = observations;
        if started.elapsed().as_secs() >= config.seconds && written >= config.operations {
            break;
        }
    }
    stop.store(true, Relaxed);
    shutdown.send(true)?;
    for writer in writers {
        writer.await??;
    }
    reader.await??;
    for observer in observers {
        observer.await??;
    }
    for domain in 1..=3 {
        let replica = runtime.domain(scope(domain))?;
        replica.checkpoint().await?;
        replica.prune().await?;
    }
    drop(replica);
    runtime.close().await?;
    let reopened = Instant::now();
    let runtime = open(&path, app, config.durability, config.instrument).await?;
    println!(
        "soak reopen_ms={} resident_mib={}",
        reopened.elapsed().as_millis(),
        resident_mib()
    );
    for domain in 1..=3 {
        let replica = runtime.domain(scope(domain))?;
        let audit = replica.audit().await?;
        let mut live = 0u64;
        for table in 0..config.tables {
            let snapshot = replica
                .read(
                    BoundQuery {
                        sql: format!("SELECT COUNT(*) FROM t{table}"),
                        values: vec![],
                    },
                    ReadOptions::new(scope(domain), 1),
                )
                .await?;
            match snapshot.rows.as_slice() {
                [row] => match row.as_slice() {
                    [SqlValue::Integer(count)] if *count >= 0 => live += *count as u64,
                    _ => return Err("invalid live count".into()),
                },
                _ => return Err("missing live count".into()),
            }
        }
        let counter = &counters[usize::from(domain - 1)];
        let expected = counter.creates.load(Relaxed) - counter.deletes.load(Relaxed);
        let frontier = replica.sync_state().await?.applied;
        let applied = frontier
            .0
            .iter()
            .filter(|(node, _)| *node == NODE)
            .map(|(_, sequence)| sequence.get())
            .sum::<u64>();
        if applied != counter.writes.load(Relaxed)
            || audit.rows as u64 != counter.creates.load(Relaxed)
        {
            return Err(format!(
                "domain {domain}: acknowledged operation or tombstone count changed after reopen"
            )
            .into());
        }
        if live != expected {
            return Err(format!("domain {domain}: live rows {live}, expected {expected}; deletion or persistence failure").into());
        }
        println!(
            "soak audit domain={domain} rows={} live={live} operations={} retained={} recomputed={}",
            audit.rows,
            counter.writes.load(Relaxed),
            audit.retained_operations,
            audit.recomputed
        );
    }
    runtime.close().await?;
    println!(
        "soak storage_slice=passed elapsed_s={} qualification_gate=pending",
        started.elapsed().as_secs()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workload_rejects_invalid_sizes_and_keeps_cold_tables_small() {
        assert!(Config::parse(["--rows", "0"].map(str::to_owned).into_iter()).is_err());
        assert!(Config::parse(["--tables", "2001"].map(str::to_owned).into_iter()).is_err());
        assert!(Config::parse(["--interval", "0"].map(str::to_owned).into_iter()).is_err());
        for index in 0..2000 {
            assert_eq!(table(index, 2000), index as u16);
        }
        for index in 2000..10_000 {
            assert!(table(index, 2000) < 50);
        }
    }
    #[test]
    fn latency_percentiles_bound_samples_including_long_stalls() {
        let mut latency = Latency::new();
        for _ in 0..99 {
            latency.record(Duration::from_micros(10));
        }
        latency.record(Duration::from_secs(5));
        assert_eq!(latency.percentile(50), 15);
        assert_eq!(latency.percentile(99), 15);
        assert_eq!(latency.percentile(100), 5_000_000);
    }
    #[tokio::test]
    async fn mixed_soak_reopens_and_audits_three_domains() {
        let directory = tempfile::tempdir().unwrap();
        run(Config {
            directory: directory.path().into(),
            seconds: 0,
            operations: 512,
            rows: 32,
            tables: 3,
            observers: 8,
            interval: 1,
            durability: Durability::Buffered,
            instrument: false,
        })
        .await
        .unwrap();
    }
}
