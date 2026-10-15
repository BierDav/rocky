//! Writer subphase attribution, checkpoint-overlap of per-write latency and
//! same-file sync interference. Measurement only: it changes no runtime
//! default and runs with the runtime's instrumentation switch.
use super::gate::{Histogram, open};
use super::{Failure, NODE, scope};
use rocky_core::*;
use rocky_runtime::{CheckpointInterval, Durability, Error, Mutation, RuntimeOptions, WriterStats};
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// A bounded ring of recent per-write service intervals on the monotonic
/// clock. `dropped` counts records evicted from the front once the ring filled,
/// so a truncated sample is always visible in the report.
pub(super) struct Samples {
    ring: VecDeque<(Instant, Duration)>,
    capacity: usize,
    total: u64,
    dropped: u64,
}

impl Samples {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            ring: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
            total: 0,
            dropped: 0,
        }
    }
    pub(super) fn record(&mut self, start: Instant, elapsed: Duration) {
        self.total += 1;
        if self.ring.len() >= self.capacity {
            self.ring.pop_front();
            self.dropped += 1;
        }
        self.ring.push_back((start, elapsed));
    }
    pub(super) fn total(&self) -> u64 {
        self.total
    }
    pub(super) fn dropped(&self) -> u64 {
        self.dropped
    }
    pub(super) fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
    /// Whether any retained sample's `[start, end]` window intersects one of
    /// the checkpoint intervals. Intervals are sequential and non-overlapping,
    /// so a binary search over their starts is exact.
    fn overlaps(start: Instant, end: Instant, intervals: &[CheckpointInterval]) -> bool {
        let index = intervals.partition_point(|interval| interval.start <= end);
        index != 0 && intervals[index - 1].end >= start
    }
    /// Percentile-based overlap report over the retained samples. `n` is the
    /// retained count, not the offered count; `dropped` above says how many
    /// older samples are missing.
    pub(super) fn overlap(&self, intervals: &[CheckpointInterval]) -> Overlap {
        let mut rows: Vec<(Duration, bool)> = self
            .ring
            .iter()
            .map(|(start, elapsed)| {
                let end = *start + *elapsed;
                (*elapsed, Self::overlaps(*start, end, intervals))
            })
            .collect();
        rows.sort_by_key(|(elapsed, _)| *elapsed);
        let n = rows.len();
        let at = |rows: &[(Duration, bool)], per_mille: u64| -> u64 {
            if rows.is_empty() {
                return 0;
            }
            let target = (rows.len() as u64 * per_mille).div_ceil(1000).max(1);
            rows[(target - 1).min(rows.len() as u64 - 1) as usize]
                .0
                .as_micros() as u64
        };
        let p99 = at(&rows, 990);
        let p999 = at(&rows, 999);
        // The tail is the worst `1%` (p99) or `0.1%` (p99.9) of samples by
        // rank, so ties at the percentile value do not inflate the tail count.
        let tail = |rows: &[(Duration, bool)], per_mille: u64| -> (u64, u64) {
            let count = ((rows.len() as u64 * (1000 - per_mille)).div_ceil(1000)).max(1) as usize;
            let slice = &rows[rows.len().saturating_sub(count)..];
            let hit = slice.iter().filter(|(_, overlaps)| *overlaps).count() as u64;
            (slice.len() as u64, hit)
        };
        let (p99_n, p99_hit) = tail(&rows, 990);
        let (p999_n, p999_hit) = tail(&rows, 999);
        let summary = |overlapping: bool| -> Tail {
            let subset: Vec<Duration> = rows
                .iter()
                .filter(|(_, overlaps)| *overlaps == overlapping)
                .map(|(elapsed, _)| *elapsed)
                .collect();
            Tail {
                n: subset.len() as u64,
                p50_us: percentile(&subset, 500),
                p99_us: percentile(&subset, 990),
                p999_us: percentile(&subset, 999),
                max_us: subset.iter().map(Duration::as_micros).max().unwrap_or(0) as u64,
            }
        };
        Overlap {
            n: n as u64,
            overlapping: rows.iter().filter(|(_, overlaps)| *overlaps).count() as u64,
            p50_us: percentile(&rows.iter().map(|(e, _)| *e).collect::<Vec<_>>(), 500),
            p99_us: p99,
            p999_us: p999,
            max_us: rows.last().map(|(e, _)| e.as_micros() as u64).unwrap_or(0),
            p99_samples: p99_n,
            p99_overlapping: p99_hit,
            p999_samples: p999_n,
            p999_overlapping: p999_hit,
            overlap: summary(true),
            no_overlap: summary(false),
        }
    }
}

/// A tail summary for one subset of the samples.
pub(super) struct Tail {
    pub(super) n: u64,
    pub(super) p50_us: u64,
    pub(super) p99_us: u64,
    pub(super) p999_us: u64,
    pub(super) max_us: u64,
}

/// The overlap analysis for one run's samples and checkpoint intervals.
pub(super) struct Overlap {
    pub(super) n: u64,
    pub(super) overlapping: u64,
    pub(super) p50_us: u64,
    pub(super) p99_us: u64,
    pub(super) p999_us: u64,
    pub(super) max_us: u64,
    /// The worst 1% of samples by rank and how many of them overlapped a
    /// checkpoint; likewise the worst 0.1%.
    pub(super) p99_samples: u64,
    pub(super) p99_overlapping: u64,
    pub(super) p999_samples: u64,
    pub(super) p999_overlapping: u64,
    /// Latency of the samples that did and did not overlap a checkpoint.
    pub(super) overlap: Tail,
    pub(super) no_overlap: Tail,
}

fn percentile(sorted: &[Duration], per_mille: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let target = (sorted.len() as u64 * per_mille).div_ceil(1000).max(1);
    sorted[(target - 1).min(sorted.len() as u64 - 1) as usize].as_micros() as u64
}

/// SQLite's own page accounting, read from the database header without opening
/// the engine: database size in pages and how many pages are on the freelist.
/// `wal_bytes` is the sidecar file's size.
pub(super) struct StorageFacts {
    pub(super) page_size: u32,
    pub(super) page_count: u32,
    pub(super) freelist_count: u32,
    pub(super) database_bytes: u64,
    pub(super) wal_bytes: u64,
}

pub(super) fn storage_facts(path: &Path) -> Option<StorageFacts> {
    let mut header = [0u8; 100];
    let mut file = std::fs::File::open(path).ok()?;
    file.read_exact(&mut header).ok()?;
    if &header[..16] != b"SQLite format 3\0".as_slice() {
        return None;
    }
    let raw = u16::from_be_bytes([header[16], header[17]]) as u32;
    let page_size = if raw == 1 { 65_536 } else { raw };
    let page_count = u32::from_be_bytes([header[28], header[29], header[30], header[31]]);
    let freelist_count = u32::from_be_bytes([header[36], header[37], header[38], header[39]]);
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    Some(StorageFacts {
        page_size,
        page_count,
        freelist_count,
        database_bytes: u64::from(page_size) * u64::from(page_count),
        wal_bytes: std::fs::metadata(Path::new(&wal)).map_or(0, |m| m.len()),
    })
}

fn open_options(instrument: bool) -> RuntimeOptions {
    RuntimeOptions {
        durability: Durability::Full,
        files: 1,
        group_commit: 128,
        instrument,
        ..RuntimeOptions::default()
    }
}

/// Fixed-rate open-loop local writes with subphase attribution, bounded
/// latency samples and checkpoint-overlap analysis. Mirrors the Part 2
/// `erp-open-loop` schedule so the latency is
/// comparable, and returns the run's raw facts for the report.
pub(super) async fn erp_open_loop(
    rate: u64,
    seconds: u64,
    tables: u16,
    directory: Option<PathBuf>,
    instrument: bool,
    label: &str,
) -> Result<Outcome, Failure> {
    if rate == 0 || ![100, 250, 500, 1000].contains(&rate) || seconds == 0 || seconds > 1800 {
        return Err("rate must be 100, 250, 500 or 1000 writes/s; duration 1..=1800 s".into());
    }
    let temporary = tempfile::tempdir()?;
    let dir = directory.unwrap_or_else(|| temporary.path().to_path_buf());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("erp.db");
    let options = open_options(instrument);
    let runtime = open(path.clone(), NODE, options, tables).await?;
    let domain = runtime.domain(scope(1))?;
    println!(
        "{label}: rate={rate}/s duration_s={seconds} tables={tables} files=1 durability=Full group=128 instrument={instrument} checkpoint_frames={}",
        options.checkpoint_frames
    );
    let origin = tokio::time::Instant::now() + Duration::from_secs(2);
    let deadline = origin + Duration::from_secs(seconds);
    let cpu_before = super::cpu::process();
    let writer_cpu_before = super::cpu::threads("rocky-writer");
    let stream = runtime.domain(scope(1))?;
    let writer = tokio::spawn(async move {
        let mut histogram = Histogram::new();
        let mut samples = Samples::new(2_000_000);
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
                        tasks.spawn(async move {
                            let sent = Instant::now();
                            let lag = sent.saturating_duration_since(scheduled.into_std());
                            let receipt = request.submit(Mutation { entity: None, payload }).await?;
                            receipt.committed().await?;
                            let elapsed = sent.elapsed();
                            let latency = tokio::time::Instant::now().saturating_duration_since(scheduled);
                            Ok::<_, Error>((slot, latency, sent, elapsed, lag))
                        });
                    } else {
                        dropped += 1;
                    }
                    index += 1;
                }
                result = tasks.join_next(), if !tasks.is_empty() => {
                    let outcome = result.ok_or(Error::Closed)?.map_err(|_| Error::Closed)??;
                    let (slot, latency, sent, elapsed, lag) = outcome;
                    histogram.record(latency);
                    samples.record(sent, elapsed);
                    intervals[slot].1 += 1;
                    if lag > period { intervals[slot].2 += 1; }
                    max_lag = max_lag.max(lag);
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        while let Some(result) = tasks.join_next().await {
            let outcome = result.map_err(|_| Error::Closed)??;
            let (slot, latency, sent, elapsed, lag) = outcome;
            histogram.record(latency);
            samples.record(sent, elapsed);
            intervals[slot].1 += 1;
            if lag > period {
                intervals[slot].2 += 1;
            }
            max_lag = max_lag.max(lag);
        }
        Ok::<_, Error>((histogram, samples, dropped, max_lag, intervals))
    });
    let (histogram, samples, dropped, max_lag, intervals) = writer.await??;
    let stats = domain.writer_stats();
    let (checkpoints, dropped_checkpoints) = if instrument {
        domain.checkpoint_intervals()
    } else {
        (Vec::new(), 0)
    };
    let cpu_after = super::cpu::process();
    let writer_cpu_after = super::cpu::threads("rocky-writer");
    let facts = storage_facts(&path);
    runtime.close().await?;
    let elapsed = Duration::from_secs(seconds);
    let offered = intervals.iter().map(|interval| interval.0).sum();
    let outcome = Outcome {
        histogram,
        samples,
        dropped,
        max_lag,
        offered,
        stats,
        checkpoints,
        dropped_checkpoints,
        cpu: cpu_after.zip(cpu_before).map(|(a, b)| a - b),
        writer_cpu: writer_cpu_after.zip(writer_cpu_before).map(|(a, b)| a - b),
        facts,
    };
    outcome.print(label, elapsed);
    Ok(outcome)
}

/// Everything the attribution report prints, kept for the evidence writer.
pub(super) struct Outcome {
    pub(super) histogram: Histogram,
    pub(super) samples: Samples,
    pub(super) dropped: u64,
    pub(super) max_lag: Duration,
    pub(super) offered: u64,
    pub(super) stats: WriterStats,
    pub(super) checkpoints: Vec<CheckpointInterval>,
    pub(super) dropped_checkpoints: u64,
    pub(super) cpu: Option<Duration>,
    pub(super) writer_cpu: Option<Duration>,
    pub(super) facts: Option<StorageFacts>,
}

impl Outcome {
    fn print(&self, label: &str, elapsed: Duration) {
        self.histogram
            .line(&format!("{label} scheduled local writes"), elapsed);
        let completed = self.offered - self.dropped;
        println!(
            "{label}: offered={} completed={completed} dropped_sends={} max_send_lag_ms={:.3}",
            self.offered,
            self.dropped,
            self.max_lag.as_secs_f64() * 1000.0
        );
        // The writer is file-wide; these are its step subphases in µs/write.
        let steps = self.stats.authoring_steps.max(1);
        let per = |micros: u64| micros as f64 / steps as f64;
        let step = self.stats.step_micros.max(1);
        println!(
            "{label} writer steps={} authoring_steps={} incoming_steps={} step_us={} commit_us={} reads_us={} flushes={}",
            self.stats.steps,
            self.stats.authoring_steps,
            self.stats.incoming_steps,
            self.stats.step_micros,
            self.stats.commit_micros,
            self.stats.read_micros,
            self.stats.flushes
        );
        println!(
            "{label} subphases us/write admission={:.2} reduction={:.2} encoding={:.2} sql={:.2} (of step: adm {:.0}% red {:.0}% enc {:.0}% sql {:.0}%)",
            per(self.stats.admission_micros),
            per(self.stats.reduction_micros),
            per(self.stats.encoding_micros),
            per(self.stats.sql_micros),
            self.stats.admission_micros as f64 * 100.0 / step as f64,
            self.stats.reduction_micros as f64 * 100.0 / step as f64,
            self.stats.encoding_micros as f64 * 100.0 / step as f64,
            self.stats.sql_micros as f64 * 100.0 / step as f64
        );
        println!(
            "{label} step attribution us/write authoring={:.2} incoming={:.2} other={:.2}",
            self.stats.authoring_step_micros as f64 / self.stats.authoring_steps.max(1) as f64,
            self.stats.incoming_step_micros as f64 / self.stats.incoming_steps.max(1) as f64,
            (self
                .stats
                .step_micros
                .saturating_sub(self.stats.authoring_step_micros + self.stats.incoming_step_micros))
                as f64
                / self.stats.steps.max(1) as f64
        );
        let passive = self.stats.checkpoints - self.stats.truncations;
        if self.stats.checkpoints > 0 {
            println!(
                "{label} checkpoints={} truncating={} mean_ms={:.1} max_ms={:.1} passive: wal_flush_ms={:.2} copy_ms={:.2} file_flush_ms={:.2} intervals={} dropped_intervals={}",
                self.stats.checkpoints,
                self.stats.truncations,
                self.stats.checkpoint_micros as f64 / 1000.0 / self.stats.checkpoints as f64,
                self.stats.checkpoint_max_micros as f64 / 1000.0,
                self.stats.checkpoint_wal_flush_micros as f64 / 1000.0 / passive.max(1) as f64,
                self.stats.checkpoint_copy_micros as f64 / 1000.0 / passive.max(1) as f64,
                self.stats.checkpoint_flush_micros as f64 / 1000.0 / passive.max(1) as f64,
                self.checkpoints.len(),
                self.dropped_checkpoints
            );
        }
        if !self.samples.is_empty() {
            let overlap = self.samples.overlap(&self.checkpoints);
            println!(
                "{label} samples total={} retained={} dropped_samples={} p50_us={} p99_us={} p99_9_us={} max_us={} overlapping={} ({:.1}%)",
                self.samples.total(),
                overlap.n,
                self.samples.dropped(),
                overlap.p50_us,
                overlap.p99_us,
                overlap.p999_us,
                overlap.max_us,
                overlap.overlapping,
                overlap.overlapping as f64 * 100.0 / overlap.n.max(1) as f64
            );
            println!(
                "{label} tail overlap: p99 samples={} overlapping={} ({:.1}%); p99_9 samples={} overlapping={} ({:.1}%)",
                overlap.p99_samples,
                overlap.p99_overlapping,
                overlap.p99_overlapping as f64 * 100.0 / overlap.p99_samples.max(1) as f64,
                overlap.p999_samples,
                overlap.p999_overlapping,
                overlap.p999_overlapping as f64 * 100.0 / overlap.p999_samples.max(1) as f64
            );
            println!(
                "{label} latency no_checkpoint n={} p50_us={} p99_us={} p99_9_us={} max_us={} | checkpoint n={} p50_us={} p99_us={} p99_9_us={} max_us={}",
                overlap.no_overlap.n,
                overlap.no_overlap.p50_us,
                overlap.no_overlap.p99_us,
                overlap.no_overlap.p999_us,
                overlap.no_overlap.max_us,
                overlap.overlap.n,
                overlap.overlap.p50_us,
                overlap.overlap.p99_us,
                overlap.overlap.p999_us,
                overlap.overlap.max_us
            );
        }
        let cpu_line = |cpu: Option<Duration>, name: &str| match cpu {
            Some(cpu) => format!("{name}={:.3}s", cpu.as_secs_f64()),
            None => format!("{name}=n/a"),
        };
        println!(
            "{label} cpu: {} {} wall={:.3}s process_busy={:.1}%",
            cpu_line(self.cpu, "process_cpu"),
            cpu_line(self.writer_cpu, "writer_thread_cpu"),
            elapsed.as_secs_f64(),
            self.cpu
                .map_or(0.0, |cpu| cpu.as_secs_f64() * 100.0 / elapsed.as_secs_f64())
        );
        if let Some(facts) = &self.facts {
            println!(
                "{label} storage: db_bytes={} wal_bytes={} page_size={} page_count={} freelist_count={} free_pages={:.2}%",
                facts.database_bytes,
                facts.wal_bytes,
                facts.page_size,
                facts.page_count,
                facts.freelist_count,
                facts.freelist_count as f64 * 100.0 / facts.page_count.max(1) as f64
            );
        }
    }
}

/// Same-file sync interference: fixed-rate local authoring on domain 1 while a
/// preloaded peer backlog is applied to domain 2 of the same file. Uses only
/// the existing trusted `operation_page`/`receive_page`/`process_pending` APIs.
pub(super) async fn erp_sync(
    rate: u64,
    seconds: u64,
    tables: u16,
    backlog: usize,
    directory: Option<PathBuf>,
) -> Result<(), Failure> {
    let temporary = tempfile::tempdir()?;
    let dir = directory.unwrap_or_else(|| temporary.path().to_path_buf());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("erp.db");
    let source_path = dir.join("peer.db");
    let runtime = open(path.clone(), NODE, open_options(true), tables).await?;
    let source = open(
        source_path,
        NodeId([4; 16]),
        RuntimeOptions {
            durability: Durability::Buffered,
            files: 1,
            // The peer only supplies a backlog; keep its history so the whole
            // preloaded range is available to page. The file under test keeps
            // the runtime defaults.
            retention: rocky_runtime::RetentionPolicy {
                automatic: false,
                checkpoint_operations: 1_000_000,
                window: 1_000_000,
                retained_bytes: 1 << 30,
                ..rocky_runtime::RetentionPolicy::default()
            },
            ..RuntimeOptions::default()
        },
        tables,
    )
    .await?;
    println!(
        "erp sync: rate={rate}/s duration_s={seconds} tables={tables} backlog={backlog} files=1 durability=Full instrument=true"
    );
    println!("erp sync: preloading backlog on the peer (excluded from the measured window)");
    let preload_started = Instant::now();
    let peer = source.domain(scope(2))?;
    for index in 0..backlog {
        peer.submit(Mutation {
            entity: None,
            payload: format!("backlog-{index}").into_bytes(),
        })
        .await?
        .committed()
        .await?;
    }
    println!(
        "erp sync: preloaded {backlog} operations in {:.1}s",
        preload_started.elapsed().as_secs_f64()
    );
    let origin = tokio::time::Instant::now() + Duration::from_secs(2);
    let deadline = origin + Duration::from_secs(seconds);
    let cpu_before = super::cpu::process();
    let writer_cpu_before = super::cpu::threads("rocky-writer");
    let stats_before = runtime.domain(scope(1))?.writer_stats();
    let feeding = Arc::new(AtomicBool::new(false));
    let local = runtime.domain(scope(1))?;
    let local_feeding = feeding.clone();
    let local_task = tokio::spawn(async move {
        let mut histogram = Histogram::new();
        let mut samples = Samples::new(2_000_000);
        let mut during = Samples::new(2_000_000);
        let mut after = Samples::new(2_000_000);
        let period = Duration::from_nanos(1_000_000_000 / rate);
        const MAX_IN_FLIGHT: usize = 1024;
        let mut tasks = tokio::task::JoinSet::new();
        let mut dropped = 0u64;
        let mut index = 0usize;
        loop {
            let scheduled = origin + period.saturating_mul(index as u32);
            tokio::select! {
                _ = tokio::time::sleep_until(scheduled), if scheduled < deadline => {
                    if tasks.len() < MAX_IN_FLIGHT {
                        let request = local.clone();
                        let payload = format!("local-{index}").into_bytes();
                        tasks.spawn(async move {
                            let sent = Instant::now();
                            let receipt = request.submit(Mutation { entity: None, payload }).await?;
                            receipt.committed().await?;
                            Ok::<_, Error>((sent, sent.elapsed()))
                        });
                    } else {
                        dropped += 1;
                    }
                    index += 1;
                }
                result = tasks.join_next(), if !tasks.is_empty() => {
                    let outcome = result.ok_or(Error::Closed)?.map_err(|_| Error::Closed)??;
                    histogram.record(outcome.1);
                    samples.record(outcome.0, outcome.1);
                    if local_feeding.load(Relaxed) {
                        during.record(outcome.0, outcome.1);
                    } else {
                        after.record(outcome.0, outcome.1);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        while let Some(result) = tasks.join_next().await {
            let outcome = result.map_err(|_| Error::Closed)??;
            histogram.record(outcome.1);
            samples.record(outcome.0, outcome.1);
            if local_feeding.load(Relaxed) {
                during.record(outcome.0, outcome.1);
            } else {
                after.record(outcome.0, outcome.1);
            }
        }
        Ok::<_, Error>((histogram, samples, during, after, dropped))
    });
    let feeder_source = source.domain(scope(2))?;
    let feeder_target = runtime.domain(scope(2))?;
    let feeder_feeding = feeding.clone();
    let feeder = tokio::spawn(async move {
        let mut cursor: Option<OperationId> = None;
        let mut pages = 0u64;
        let mut received = 0u64;
        // Drain the backlog in the same window as the local writes.
        tokio::time::sleep_until(origin).await;
        feeder_feeding.store(true, Relaxed);
        let started = Instant::now();
        loop {
            let page = feeder_source.operation_page(cursor).await?.items;
            let Some(newest) = page.last() else {
                break;
            };
            cursor = Some(Envelope::decode(newest)?.id);
            let mut pending = false;
            for result in feeder_target.receive_page(page).await? {
                pending |= !result?.applied;
                received += 1;
            }
            if pending {
                feeder_target.process_pending().await?;
            }
            pages += 1;
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        feeder_feeding.store(false, Relaxed);
        Ok::<_, Error>((started.elapsed(), pages, received))
    });
    let (histogram, samples, during, after, dropped) = local_task.await??;
    let (feed_elapsed, pages, received) = feeder.await??;
    let stats = runtime.domain(scope(1))?.writer_stats();
    let (checkpoints, dropped_checkpoints) = runtime.domain(scope(1))?.checkpoint_intervals();
    let cpu_after = super::cpu::process();
    let writer_cpu_after = super::cpu::threads("rocky-writer");
    let facts = storage_facts(&path);
    let elapsed = Duration::from_secs(seconds);
    histogram.line("erp sync local writes", elapsed);
    println!(
        "erp sync: backlog_apply_s={:.1} pages={pages} received={received} dropped_sends={dropped}",
        feed_elapsed.as_secs_f64()
    );
    let steps = stats.authoring_steps.max(1);
    println!(
        "erp sync writer delta: groups={} steps={} authoring_steps={} incoming_steps={} step_us={} authoring_step_us={:.0} incoming_step_us={:.0} commit_us={}",
        stats.groups - stats_before.groups,
        stats.steps - stats_before.steps,
        stats.authoring_steps - stats_before.authoring_steps,
        stats.incoming_steps - stats_before.incoming_steps,
        stats.step_micros - stats_before.step_micros,
        (stats.authoring_step_micros - stats_before.authoring_step_micros) as f64 / steps as f64,
        (stats.incoming_step_micros - stats_before.incoming_step_micros) as f64
            / (stats.incoming_steps - stats_before.incoming_steps).max(1) as f64,
        stats.commit_micros - stats_before.commit_micros
    );
    let overlap = samples.overlap(&checkpoints);
    println!(
        "erp sync local samples n={} p50_us={} p99_us={} p99_9_us={} max_us={} overlapping_checkpoint={} ({:.1}%) dropped_samples={}",
        overlap.n,
        overlap.p50_us,
        overlap.p99_us,
        overlap.p999_us,
        overlap.max_us,
        overlap.overlapping,
        overlap.overlapping as f64 * 100.0 / overlap.n.max(1) as f64,
        samples.dropped()
    );
    println!(
        "erp sync tail overlap: p99 samples={} overlapping={} ({:.1}%); p99_9 samples={} overlapping={} ({:.1}%); intervals={} dropped_intervals={}",
        overlap.p99_samples,
        overlap.p99_overlapping,
        overlap.p99_overlapping as f64 * 100.0 / overlap.p99_samples.max(1) as f64,
        overlap.p999_samples,
        overlap.p999_overlapping,
        overlap.p999_overlapping as f64 * 100.0 / overlap.p999_samples.max(1) as f64,
        checkpoints.len(),
        dropped_checkpoints
    );
    let during_overlap = during.overlap(&checkpoints);
    println!(
        "erp sync during-backlog local samples n={} p50_us={} p99_us={} p99_9_us={} max_us={}",
        during_overlap.n,
        during_overlap.p50_us,
        during_overlap.p99_us,
        during_overlap.p999_us,
        during_overlap.max_us
    );
    let after_overlap = after.overlap(&checkpoints);
    println!(
        "erp sync after-backlog local samples n={} p50_us={} p99_us={} p99_9_us={} max_us={}",
        after_overlap.n,
        after_overlap.p50_us,
        after_overlap.p99_us,
        after_overlap.p999_us,
        after_overlap.max_us
    );
    println!(
        "erp sync cpu: process_cpu={} writer_thread_cpu={} wall={:.3}s",
        cpu_after
            .zip(cpu_before)
            .map(|(a, b)| format!("{:.3}s", (a - b).as_secs_f64()))
            .unwrap_or_else(|| "n/a".into()),
        writer_cpu_after
            .zip(writer_cpu_before)
            .map(|(a, b)| format!("{:.3}s", (a - b).as_secs_f64()))
            .unwrap_or_else(|| "n/a".into()),
        elapsed.as_secs_f64()
    );
    if let Some(facts) = facts {
        println!(
            "erp sync storage: db_bytes={} wal_bytes={} page_count={} freelist_count={}",
            facts.database_bytes, facts.wal_bytes, facts.page_count, facts.freelist_count
        );
    }
    runtime.close().await?;
    source.close().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_runtime::CheckpointKind;

    fn interval(start: Instant, millis: u64) -> CheckpointInterval {
        CheckpointInterval {
            start,
            end: start + Duration::from_millis(millis),
            kind: CheckpointKind::Passive,
            copy_micros: millis * 1000,
            flush_micros: 0,
        }
    }

    #[test]
    fn samples_are_bounded_and_count_dropped() {
        let mut samples = Samples::new(3);
        let base = Instant::now();
        for index in 0..5u64 {
            samples.record(
                base + Duration::from_micros(index),
                Duration::from_micros(index),
            );
        }
        assert_eq!(samples.total(), 5);
        assert_eq!(samples.dropped(), 2);
        let overlap = samples.overlap(&[]);
        assert_eq!(overlap.n, 3);
        // Only the newest three survive: durations 2, 3, 4 µs.
        assert_eq!(overlap.max_us, 4);
    }

    #[test]
    fn overlap_is_detected_on_the_monotonic_clock() {
        let base = Instant::now();
        let intervals = [interval(base + Duration::from_millis(10), 5)];
        let mut samples = Samples::new(16);
        // Before, inside and after the checkpoint.
        samples.record(base, Duration::from_micros(100));
        samples.record(base + Duration::from_millis(12), Duration::from_micros(100));
        samples.record(base + Duration::from_millis(20), Duration::from_micros(100));
        // A long write that starts before and ends inside the checkpoint window.
        samples.record(base + Duration::from_millis(9), Duration::from_millis(2));
        let overlap = samples.overlap(&intervals);
        assert_eq!(overlap.n, 4);
        assert_eq!(overlap.overlapping, 2);
        assert_eq!(overlap.no_overlap.n, 2);
        assert_eq!(overlap.overlap.n, 2);
    }

    #[test]
    fn p99_share_counts_the_tail_that_overlapped() {
        let base = Instant::now();
        let intervals = [interval(base + Duration::from_millis(100), 50)];
        let mut samples = Samples::new(200);
        // 99 fast writes before the checkpoint, then one very slow write inside.
        for index in 0..99u64 {
            samples.record(
                base + Duration::from_micros(index),
                Duration::from_micros(10),
            );
        }
        samples.record(base + Duration::from_millis(110), Duration::from_millis(20));
        let overlap = samples.overlap(&intervals);
        assert_eq!(overlap.n, 100);
        assert_eq!(overlap.p99_samples, 1);
        assert_eq!(overlap.p99_overlapping, 1);
        assert_eq!(overlap.p999_samples, 1);
        assert_eq!(overlap.p999_overlapping, 1);
    }

    #[test]
    fn storage_facts_read_the_sqlite_header() {
        // A 4 KiB page size with 3 pages and one free page, in the header.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("h.db");
        let mut header = vec![0u8; 100];
        header[..16].copy_from_slice(b"SQLite format 3\0");
        header[16..18].copy_from_slice(&4096u16.to_be_bytes());
        header[28..32].copy_from_slice(&3u32.to_be_bytes());
        header[36..40].copy_from_slice(&1u32.to_be_bytes());
        std::fs::write(&path, &header).unwrap();
        let facts = storage_facts(&path).unwrap();
        assert_eq!(facts.page_size, 4096);
        assert_eq!(facts.page_count, 3);
        assert_eq!(facts.freelist_count, 1);
        assert_eq!(facts.database_bytes, 12_288);
        assert_eq!(facts.wal_bytes, 0);
    }
}
