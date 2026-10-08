//! Phase 5 gate: sustained local writes on 16 domains at full durability,
//! with strong commands and incoming pages running beside them; see
//! docs/runtime/write-concurrency.md.
use super::{CALLER, Failure, NODE, Wide, installation, resident_mib, scope};
use rocky_core::*;
use rocky_runtime::{
    CommandHandle, DomainInstallation, Durability, Error, Mutation, Replica, Runtime,
    RuntimeConfig, RuntimeOptions, WriterStats,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
};
use std::time::{Duration, Instant};

const PEER: NodeId = NodeId([4; 16]);
/// Domains with `CLIENTS` closed-loop writers each.
const DOMAINS: u8 = 16;
const CLIENTS: usize = 8;
/// Rows each local client creates before it only edits them.
const ROWS: usize = 256;
/// Two more domains in the same file run strong commands beside one paced
/// local writer each. A command seals, replays and names every operation of
/// its domain, so it cannot run on a domain that takes hundreds of writes a
/// second: the result frame holds at most 43,690 operation identities.
const STRONG: [u8; 2] = [17, 18];
const STRONG_ROWS: usize = 16;
const STRONG_PAUSE: Duration = Duration::from_millis(250);
const STRONG_WRITE_PAUSE: Duration = Duration::from_secs(1);
/// Domains that also receive a peer's operations, a page per tick.
const INCOMING: [u8; 4] = [3, 4, 5, 6];
const INCOMING_BATCH: usize = 320;
const INCOMING_TICK: Duration = Duration::from_millis(100);

/// Latencies in 100 µs buckets up to one second, and the exact maximum.
pub(super) struct Histogram {
    buckets: Vec<u64>,
    count: u64,
    max: Duration,
}
impl Histogram {
    pub(super) fn new() -> Self {
        Self {
            buckets: vec![0; 10_001],
            count: 0,
            max: Duration::ZERO,
        }
    }
    pub(super) fn record(&mut self, elapsed: Duration) {
        let bucket = (elapsed.as_micros() / 100).min(10_000) as usize;
        self.buckets[bucket] += 1;
        self.count += 1;
        self.max = self.max.max(elapsed);
    }
    pub(super) fn merge(&mut self, other: &Self) {
        for (mine, theirs) in self.buckets.iter_mut().zip(&other.buckets) {
            *mine += theirs;
        }
        self.count += other.count;
        self.max = self.max.max(other.max);
    }
    /// Upper bound of the bucket holding the quantile, in microseconds.
    pub(super) fn at(&self, percent: u64) -> u64 {
        self.at_per_mille(percent.saturating_mul(10))
    }
    pub(super) fn at_per_mille(&self, per_mille: u64) -> u64 {
        let target = (self.count * per_mille).div_ceil(1000).max(1);
        let mut seen = 0;
        for (bucket, count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= target {
                if bucket == self.buckets.len() - 1 {
                    return self.max.as_micros() as u64;
                }
                return (bucket as u64 + 1) * 100;
            }
        }
        self.max.as_micros() as u64
    }
    pub(super) fn line(&self, label: &str, elapsed: Duration) {
        println!(
            "{label}: n={} throughput={:.0}/s p50_us<={} p99_us<={} max_us={}",
            self.count,
            self.count as f64 / elapsed.as_secs_f64(),
            self.at(50),
            self.at(99),
            self.max.as_micros()
        );
        println!("{label}: p99_9_us<={}", self.at_per_mille(999));
    }
}

/// Fixed-rate schedule for an ERP-style local write stream. Latency includes
/// time spent behind schedule, so a busy caller cannot hide queueing.
pub async fn erp_open_loop(
    rate: u64,
    seconds: u64,
    tables: u16,
    directory: Option<std::path::PathBuf>,
    collect_latency: bool,
    bulk_burst: bool,
) -> Result<(), Failure> {
    if rate == 0 || ![100, 250, 500, 1000].contains(&rate) || seconds == 0 || seconds > 1800 {
        return Err(
            "open-loop rate must be 100, 250, 500 or 1000 writes/s; duration must be 1..=1800 seconds".into(),
        );
    }
    let temporary = tempfile::tempdir()?;
    let dir = directory.unwrap_or_else(|| temporary.path().to_path_buf());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("erp.db");
    let options = open_options(1, Durability::Full, Some(128), None);
    let runtime = open(path, NODE, options, tables).await?;
    let coordinator = Arc::new(rocky_coordinator::MemoryCoordinator::default());
    for domain in STRONG {
        coordinator.register(scope(domain), CALLER, [6; 16], vec![NODE], 1, 1)?;
        coordinator.acquire(scope(domain), CALLER, NODE)?;
    }
    println!(
        "erp open_loop rate={rate}/s duration_s={seconds} tables={tables} files=1 durability=Full group=128 checkpoint_frames={}",
        options.checkpoint_frames
    );
    let origin = tokio::time::Instant::now() + Duration::from_secs(2);
    let deadline = origin + Duration::from_secs(seconds);
    let bulk_active = Arc::new(AtomicBool::new(false));
    let stream = runtime.domain(scope(1))?;
    let background_bulk_state = bulk_active.clone();
    let writer = tokio::spawn(async move {
        let mut histogram = collect_latency.then(Histogram::new);
        let mut during_bulk = collect_latency.then(Histogram::new);
        let period = Duration::from_nanos(1_000_000_000 / rate);
        const MAX_IN_FLIGHT: usize = 1024;
        let mut tasks = tokio::task::JoinSet::new();
        let mut intervals = vec![(0u64, 0u64, 0u64); seconds.div_ceil(10) as usize];
        let mut dropped = 0u64;
        let mut max_lag = Duration::ZERO;
        let mut index = 0usize;
        loop {
            let scheduled = origin + period.saturating_mul(index as u32);
            tokio::select! {
                _ = tokio::time::sleep_until(scheduled), if scheduled < deadline => {
                    let slot = ((scheduled - origin).as_secs() / 10) as usize;
                    intervals[slot].0 += 1;
                    if tasks.len() < MAX_IN_FLIGHT {
                        let request = stream.clone();
                        let payload = format!("erp-{index}").into_bytes();
                        let was_during_bulk = background_bulk_state.load(Relaxed);
                        tasks.spawn(async move {
                            let sent = tokio::time::Instant::now();
                            let lag = sent.saturating_duration_since(scheduled);
                            let receipt = request.submit(Mutation { entity: None, payload }).await?;
                            receipt.committed().await?;
                            let latency = collect_latency.then(|| tokio::time::Instant::now().saturating_duration_since(scheduled));
                            Ok::<_, Error>((slot, latency, lag, was_during_bulk))
                        });
                    } else {
                        dropped += 1;
                    }
                    index += 1;
                }
                result = tasks.join_next(), if !tasks.is_empty() => {
                    let outcome = result.ok_or(Error::Closed)?.map_err(|_| Error::Closed)??;
                    let (slot, latency, lag, was_during_bulk) = outcome;
                    if let (Some(histogram), Some(latency)) = (&mut histogram, latency) { histogram.record(latency); }
                    if was_during_bulk
                        && let (Some(histogram), Some(latency)) = (&mut during_bulk, latency)
                    {
                        histogram.record(latency);
                    }
                    intervals[slot].1 += 1;
                    if lag > period { intervals[slot].2 += 1; }
                    max_lag = max_lag.max(lag);
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        while let Some(result) = tasks.join_next().await {
            let (slot, latency, lag, was_during_bulk) = result.map_err(|_| Error::Closed)??;
            if let (Some(histogram), Some(latency)) = (&mut histogram, latency) {
                histogram.record(latency);
            }
            if was_during_bulk && let (Some(histogram), Some(latency)) = (&mut during_bulk, latency)
            {
                histogram.record(latency);
            }
            intervals[slot].1 += 1;
            if lag > period {
                intervals[slot].2 += 1;
            }
            max_lag = max_lag.max(lag);
        }
        Ok::<_, Error>((histogram, during_bulk, dropped, max_lag, intervals))
    });
    let bulk = if bulk_burst {
        let bulk_runtime = runtime.domain(scope(2))?;
        let bulk_active = bulk_active.clone();
        Some(tokio::spawn(async move {
            tokio::time::sleep_until(origin + Duration::from_secs(5)).await;
            bulk_active.store(true, Relaxed);
            let started = Instant::now();
            for index in 0..50_000usize {
                let receipt = bulk_runtime
                    .submit(Mutation {
                        entity: None,
                        payload: format!("erp-bulk-{index}").into_bytes(),
                    })
                    .await?;
                receipt.committed().await?;
            }
            bulk_active.store(false, Relaxed);
            Ok::<_, Error>(started.elapsed())
        }))
    } else {
        None
    };
    let strong_runtime = runtime.domain(scope(STRONG[0]))?;
    let strong_coordinator = coordinator.clone();
    let strong = tokio::spawn(async move {
        let mut histogram = Histogram::new();
        let mut index = 0u64;
        while tokio::time::Instant::now() < deadline {
            let started = Instant::now();
            let mut handle = [STRONG[0]; 32];
            handle[..8].copy_from_slice(&index.to_le_bytes());
            strong_runtime
                .submit_command_with_arguments(
                    CommandHandle(handle),
                    inventory_app::RESERVE,
                    1u64.to_le_bytes().to_vec(),
                    [9; 32],
                )?
                .committed()
                .await?;
            strong_runtime
                .execute_pending(strong_coordinator.as_ref())
                .await?;
            histogram.record(started.elapsed());
            index += 1;
            tokio::time::sleep(STRONG_PAUSE).await;
        }
        Ok::<_, Error>(histogram)
    });
    let (local, during_bulk, dropped, max_lag, intervals) = writer.await??;
    let strong = strong.await??;
    let bulk_duration = if let Some(bulk) = bulk {
        Some(bulk.await??)
    } else {
        None
    };
    let elapsed = Duration::from_secs(seconds);
    if let Some(local) = local {
        local.line("erp scheduled local writes", elapsed);
    }
    if let Some(during_bulk) = during_bulk {
        during_bulk.line("erp background writes during bulk", elapsed);
    }
    if let Some(duration) = bulk_duration {
        println!("erp bulk writes=50000 duration_ms={}", duration.as_millis());
    }
    let offered = intervals.iter().map(|interval| interval.0).sum::<u64>();
    let completed = intervals.iter().map(|interval| interval.1).sum::<u64>();
    println!(
        "erp stream offered={offered} completed={completed} achieved_per_s={:.1}",
        completed as f64 / elapsed.as_secs_f64()
    );
    println!(
        "erp schedule dropped_sends={dropped} max_send_lag_ms={:.3}",
        max_lag.as_secs_f64() * 1000.0
    );
    for (index, (offered, completed, late)) in intervals.iter().enumerate() {
        let expected = (rate * 10).min(rate * (seconds.saturating_sub(index as u64 * 10)));
        println!(
            "erp interval={}s offered={offered} completed={completed} late_sends={late} behind={}",
            index * 10,
            completed < &expected || offered < &expected || *late > 0
        );
    }
    strong.line("erp strong commands", elapsed);
    let stats = runtime.domain(scope(1))?.writer_stats();
    println!(
        "erp writer groups={} steps={} step_us={} commit_us={} checkpoints={} checkpoint_us={} checkpoint_copy_us={} checkpoint_file_flush_us={}",
        stats.groups,
        stats.steps,
        stats.step_micros,
        stats.commit_micros,
        stats.checkpoints,
        stats.checkpoint_micros,
        stats.checkpoint_copy_micros,
        stats.checkpoint_flush_micros
    );
    runtime.close().await?;
    Ok(())
}

pub(super) fn open_options(
    files: usize,
    durability: Durability,
    group: Option<usize>,
    checkpoint_frames: Option<u32>,
) -> RuntimeOptions {
    let defaults = RuntimeOptions::default();
    RuntimeOptions {
        durability,
        files,
        group_commit: group.unwrap_or(defaults.group_commit),
        checkpoint_frames: checkpoint_frames.unwrap_or(defaults.checkpoint_frames),
        ..defaults
    }
}
pub(super) async fn open(
    path: std::path::PathBuf,
    node: NodeId,
    options: RuntimeOptions,
    tables: u16,
) -> Result<Runtime<Wide>, Error> {
    let schema = if tables > 0 {
        let wide = super::wide_schema::WideSchema::new(tables);
        Some(&*Box::leak(
            super::InventoryApp
                .schema()
                .iter()
                .chain(wide.schema())
                .copied()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ))
    } else {
        None
    };
    let installations = (1..=DOMAINS + STRONG.len() as u8)
        .map(|domain| {
            let base = installation(DOMAINS + STRONG.len() as u8, domain);
            DomainInstallation {
                app: Wide(
                    DOMAINS + STRONG.len() as u8,
                    if domain == 1 { schema } else { None },
                    tables,
                ),
                identity: rocky_runtime::Identity {
                    node,
                    ..base.identity
                },
                config: RuntimeConfig {
                    authors: vec![(NODE, CALLER), (PEER, CALLER)],
                    delivery_targets: Vec::new(),
                    full_nodes: Vec::new(),
                },
                ..base
            }
        })
        .collect();
    Runtime::open_with_options(path, installations, options).await
}
/// Creates `rows` rows, then edits them in turn, until the deadline, with
/// `pause` between writes.
async fn client(
    replica: Replica<Wide>,
    label: String,
    rows: usize,
    pause: Duration,
    deadline: Instant,
    written: Arc<AtomicU64>,
) -> Result<Histogram, Error> {
    let mut timings = Histogram::new();
    let mut entities = Vec::with_capacity(rows);
    let mut index = 0usize;
    while Instant::now() < deadline {
        let started = Instant::now();
        let entity = (entities.len() == rows).then(|| entities[index % rows]);
        let receipt = replica
            .submit(Mutation {
                entity,
                payload: format!("{label}-{index}").into_bytes(),
            })
            .await?;
        let operation = receipt.committed().await?;
        timings.record(started.elapsed());
        if entity.is_none() {
            entities.push(operation.id);
        }
        index += 1;
        written.fetch_add(1, Relaxed);
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
    }
    Ok(timings)
}
/// One command at a time, published and executed, with a pause between.
async fn commands(
    replica: Replica<Wide>,
    domain: u8,
    coordinator: Arc<rocky_coordinator::MemoryCoordinator>,
    deadline: Instant,
) -> Result<Histogram, Error> {
    let mut timings = Histogram::new();
    let mut index = 0u64;
    while Instant::now() < deadline {
        let started = Instant::now();
        let mut handle = [domain; 32];
        handle[..8].copy_from_slice(&index.to_le_bytes());
        replica
            .submit_command_with_arguments(
                CommandHandle(handle),
                inventory_app::RESERVE,
                1u64.to_le_bytes().to_vec(),
                [9; 32],
            )?
            .committed()
            .await?;
        replica.execute_pending(coordinator.as_ref()).await?;
        timings.record(started.elapsed());
        index += 1;
        tokio::time::sleep(STRONG_PAUSE).await;
    }
    Ok(timings)
}
/// The peer authors a batch, then hands it over a page per tick. Returns the
/// page latencies and the operations received.
async fn incoming(
    from: Replica<Wide>,
    to: Replica<Wide>,
    domain: u8,
    deadline: Instant,
) -> Result<(Histogram, u64), Error> {
    let mut timings = Histogram::new();
    let mut entities = Vec::with_capacity(ROWS);
    let (mut index, mut received) = (0usize, 0u64);
    let mut cursor = None;
    let mut tick = tokio::time::interval(INCOMING_TICK);
    while Instant::now() < deadline {
        let mut receipts = Vec::with_capacity(INCOMING_BATCH);
        for _ in 0..INCOMING_BATCH {
            let entity = (entities.len() == ROWS).then(|| entities[index % ROWS]);
            let mutation = Mutation {
                entity,
                payload: format!("peer-{domain}-{index}").into_bytes(),
            };
            let operation = from.submit(mutation).await?.committed().await?;
            if entity.is_none() {
                entities.push(operation.id);
            }
            receipts.push(operation.id);
            index += 1;
        }
        let Some(last) = receipts.last().copied() else {
            break;
        };
        while cursor != Some(last) && Instant::now() < deadline {
            tick.tick().await;
            let page = from.operation_page(cursor).await?.items;
            let Some(newest) = page.last() else { break };
            cursor = Some(Envelope::decode(newest)?.id);
            let started = Instant::now();
            let mut pending = false;
            for result in to.receive_page(page).await? {
                pending |= !result?.applied;
                received += 1;
            }
            if pending {
                to.process_pending().await?;
            }
            timings.record(started.elapsed());
        }
    }
    Ok((timings, received))
}
fn wal_bytes(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("gate"))
        .filter(|entry| entry.file_name().to_string_lossy().ends_with("-wal"))
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}
fn total(stats: &[WriterStats]) -> WriterStats {
    let mut sum = WriterStats::default();
    for stats in stats {
        sum.groups += stats.groups;
        sum.steps += stats.steps;
        sum.flushes += stats.flushes;
        sum.step_micros += stats.step_micros;
        sum.commit_micros += stats.commit_micros;
        sum.read_micros += stats.read_micros;
        sum.authoring_steps += stats.authoring_steps;
        sum.incoming_steps += stats.incoming_steps;
        sum.authoring_step_micros += stats.authoring_step_micros;
        sum.incoming_step_micros += stats.incoming_step_micros;
        sum.checkpoints += stats.checkpoints;
        sum.checkpoint_micros += stats.checkpoint_micros;
        sum.checkpoint_max_micros = sum.checkpoint_max_micros.max(stats.checkpoint_max_micros);
        sum.checkpoint_wal_flush_micros += stats.checkpoint_wal_flush_micros;
        sum.checkpoint_copy_micros += stats.checkpoint_copy_micros;
        sum.checkpoint_flush_micros += stats.checkpoint_flush_micros;
        sum.truncations += stats.truncations;
        sum.truncate_micros += stats.truncate_micros;
        sum.wal_frames_max = sum.wal_frames_max.max(stats.wal_frames_max);
    }
    sum
}
/// `gate [seconds] [files] [checkpoint_frames] [group] [directory]`. Fails on
/// the first storage error. The last three override `RuntimeOptions`.
pub async fn run(
    seconds: u64,
    files: usize,
    checkpoint_frames: Option<u32>,
    group: Option<usize>,
    directory: Option<std::path::PathBuf>,
    tables: u16,
) -> Result<(), Failure> {
    let temporary = tempfile::tempdir()?;
    let dir = directory.unwrap_or_else(|| temporary.path().to_path_buf());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("gate.db");
    println!(
        "gate wide_tables={tables}: domain 1 owns the wide physical schema; other domains use sparse schemas"
    );
    let options = open_options(files, Durability::Full, group, checkpoint_frames);
    let runtime = open(path.clone(), NODE, options, tables).await?;
    // The peer only supplies operations; its file is not under test.
    let source = open(
        dir.join("peer.db"),
        PEER,
        open_options(1, Durability::Buffered, None, None),
        tables,
    )
    .await?;
    let coordinator = Arc::new(rocky_coordinator::MemoryCoordinator::default());
    for domain in STRONG {
        coordinator.register(scope(domain), CALLER, [6; 16], vec![NODE], 1, 1)?;
        coordinator.acquire(scope(domain), CALLER, NODE)?;
    }
    println!(
        "gate: {seconds} s, {DOMAINS} domains x {CLIENTS} clients, files={files}, group={}, \
         checkpoint_frames={}, full durability, incoming on {INCOMING:?}, strong on two \
         more domains {STRONG:?}",
        options.group_commit, options.checkpoint_frames
    );
    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    let written = Arc::new(AtomicU64::new(0));
    let mut writers = Vec::new();
    for domain in 1..=DOMAINS {
        for index in 0..CLIENTS {
            writers.push(tokio::spawn(client(
                runtime.domain(scope(domain))?,
                format!("{domain}-{index}"),
                ROWS,
                Duration::ZERO,
                deadline,
                written.clone(),
            )));
        }
    }
    let paced = Arc::new(AtomicU64::new(0));
    let mut light = Vec::new();
    for domain in STRONG {
        light.push(tokio::spawn(client(
            runtime.domain(scope(domain))?,
            format!("{domain}-light"),
            STRONG_ROWS,
            STRONG_WRITE_PAUSE,
            deadline,
            paced.clone(),
        )));
    }
    let mut strong = Vec::new();
    for domain in STRONG {
        strong.push(tokio::spawn(commands(
            runtime.domain(scope(domain))?,
            domain,
            coordinator.clone(),
            deadline,
        )));
    }
    let mut feeders = Vec::new();
    for domain in INCOMING {
        feeders.push(tokio::spawn(incoming(
            source.domain(scope(domain))?,
            runtime.domain(scope(domain))?,
            domain,
            deadline,
        )));
    }
    // One replica per file reports that file's writer.
    let probes: Vec<_> = (1..=DOMAINS.min(files as u8))
        .map(|domain| runtime.domain(scope(domain)))
        .collect::<Result<_, _>>()?;
    let depth = runtime.domain(scope(DOMAINS))?;
    let stats = || total(&probes.iter().map(Replica::writer_stats).collect::<Vec<_>>());
    let wal_peak = Arc::new(Mutex::new(0u64));
    let mut previous = (0u64, stats(), Instant::now());
    let mut slowest = f64::MAX;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(10).min(deadline - Instant::now())).await;
        let (count, now, at) = (written.load(Relaxed), stats(), Instant::now());
        let interval = (at - previous.2).as_secs_f64();
        let micros = interval * 1e6 * files as f64;
        let rate = (count - previous.0) as f64 / interval;
        let wal = wal_bytes(&dir);
        if let Ok(mut peak) = wal_peak.lock() {
            *peak = (*peak).max(wal);
        }
        if interval > 5.0 {
            slowest = slowest.min(rate);
        }
        println!(
            "gate t={:>4.0}s writes={rate:.0}/s steps/group={:.1} flushes={:.0}/s busy: steps {:.0}% commit {:.0}% reads {:.0}% checkpoints {} ({:.0}% of time) wal={} MiB queue={} resident={} MiB",
            (at - started).as_secs_f64(),
            (now.steps - previous.1.steps) as f64 / (now.groups - previous.1.groups).max(1) as f64,
            (now.flushes - previous.1.flushes) as f64 / interval,
            (now.step_micros - previous.1.step_micros) as f64 * 100.0 / micros,
            (now.commit_micros - previous.1.commit_micros) as f64 * 100.0 / micros,
            (now.read_micros - previous.1.read_micros) as f64 * 100.0 / micros,
            now.checkpoints - previous.1.checkpoints,
            (now.checkpoint_micros - previous.1.checkpoint_micros) as f64 * 100.0 / micros,
            wal / (1024 * 1024),
            depth.queue_depth().commands,
            resident_mib()
        );
        previous = (count, now, at);
    }
    let mut local = Histogram::new();
    for writer in writers {
        local.merge(&writer.await??);
    }
    for writer in light {
        writer.await??;
    }
    let mut strong_timings = Histogram::new();
    for task in strong {
        strong_timings.merge(&task.await??);
    }
    let mut pages = Histogram::new();
    let mut received = 0;
    for feeder in feeders {
        let (timings, count) = feeder.await??;
        pages.merge(&timings);
        received += count;
    }
    let elapsed = started.elapsed();
    local.line("gate local writes", elapsed);
    strong_timings.line("gate strong commands", elapsed);
    pages.line("gate incoming pages", elapsed);
    let finished = stats();
    let passive = (finished.checkpoints - finished.truncations).max(1) as f64;
    println!(
        "gate checkpoints: passive={} wal_flush_ms={:.1} copy_ms={:.1} file_flush_ms={:.1} per checkpoint; truncating={} mean_ms={:.1}",
        finished.checkpoints - finished.truncations,
        finished.checkpoint_wal_flush_micros as f64 / 1000.0 / passive,
        finished.checkpoint_copy_micros as f64 / 1000.0 / passive,
        finished.checkpoint_flush_micros as f64 / 1000.0 / passive,
        finished.truncations,
        finished.truncate_micros as f64 / 1000.0 / finished.truncations.max(1) as f64
    );
    println!(
        "gate incoming operations: {received} ({:.0}/s); slowest 10 s interval {slowest:.0} writes/s",
        received as f64 / elapsed.as_secs_f64()
    );
    println!(
        "gate writer: groups={} steps={} flushes={} steps {:.0}% commit {:.0}% reads {:.0}% of {files} thread(s); checkpoints={} mean_ms={:.1} max_ms={:.1}; wal max {} MiB by frames, {} MiB on disk",
        finished.groups,
        finished.steps,
        finished.flushes,
        finished.step_micros as f64 * 100.0 / (elapsed.as_micros() as f64 * files as f64),
        finished.commit_micros as f64 * 100.0 / (elapsed.as_micros() as f64 * files as f64),
        finished.read_micros as f64 * 100.0 / (elapsed.as_micros() as f64 * files as f64),
        finished.checkpoints,
        finished.checkpoint_micros as f64 / 1000.0 / finished.checkpoints.max(1) as f64,
        finished.checkpoint_max_micros as f64 / 1000.0,
        finished.wal_frames_max * 4096 / (1024 * 1024),
        wal_peak.lock().map_or(0, |peak| *peak) / (1024 * 1024)
    );
    drop((probes, depth));
    runtime.close().await?;
    source.close().await?;
    let reopening = Instant::now();
    let runtime = open(path, NODE, options, tables).await?;
    let mut operations = 0;
    for domain in 1..=DOMAINS + STRONG.len() as u8 {
        let frontier = runtime.domain(scope(domain))?.sync_state().await?.applied;
        operations += frontier.0.iter().map(|(_, s)| s.get()).sum::<u64>();
    }
    println!(
        "gate reopen: {} ms, {operations} applied operations, files {} MB",
        reopening.elapsed().as_millis(),
        std::fs::read_dir(&dir)?
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("gate"))
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum::<u64>()
            / 1_000_000
    );
    runtime.close().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_latency_never_reports_a_one_second_upper_bound() {
        let mut histogram = Histogram::new();
        for _ in 0..98 {
            histogram.record(Duration::from_millis(10));
        }
        histogram.record(Duration::from_secs(2));
        histogram.record(Duration::from_secs(5));
        assert_eq!(histogram.at(50), 10_100);
        assert_eq!(histogram.at(99), 5_000_000);
    }

    #[test]
    fn reports_the_999th_quantile_from_bounded_buckets() {
        let mut histogram = Histogram::new();
        for _ in 0..998 {
            histogram.record(Duration::from_millis(10));
        }
        histogram.record(Duration::from_secs(2));
        assert_eq!(histogram.at_per_mille(999), 2_000_000);
    }
}
