//! The phase 4 gate: one domain of the wide schema taken to a lifetime of
//! `operations` over `rows` live rows with the default retention policy, so
//! checkpoints and pruning run beside the writes. Reports write cost, memory,
//! retained history and file size along the way, then reopen and audit time.
use super::wide::{commit, open, scope};
use super::wide_schema::WideSchema;
use super::{Failure, NODE, futures, line, resident_mib};
use rocky_core::*;
use rocky_runtime::{Error, Replica, RuntimeOptions};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

const WRITERS: usize = 16;
/// Reports during the edit phase; each spans many checkpoint and prune cycles.
const REPORTS: usize = 12;

/// Bytes of the database file and of its write-ahead log.
fn file_bytes(directory: &Path) -> (u64, u64) {
    let size = |name: &str| std::fs::metadata(directory.join(name)).map_or(0, |file| file.len());
    (size("checkpoints.db"), size("checkpoints.db-wal"))
}
async fn median(
    label: &str,
    count: usize,
    mut step: impl AsyncFnMut(usize) -> Result<(), Error>,
) -> Result<Duration, Failure> {
    let started = Instant::now();
    let mut timings = Vec::with_capacity(count);
    for index in 0..count {
        let step_started = Instant::now();
        step(index).await?;
        timings.push(step_started.elapsed());
    }
    let mut sorted = timings.clone();
    sorted.sort();
    line(label, timings, started.elapsed());
    Ok(sorted[sorted.len() / 2])
}
async fn report(
    tag: &str,
    replica: &Replica<WideSchema>,
    directory: &Path,
    done: usize,
    rate: f64,
) -> Result<(), Failure> {
    let status = replica.retention_status().await?;
    println!(
        "{tag} operations={done}: {rate:.0} writes/s, checkpoint={} retained_operations={} retained_bytes={} file_bytes={} wal_bytes={} resident {} MiB",
        status
            .checkpoint
            .map_or(0, |checkpoint| checkpoint.record.sequence),
        status.retained_operations,
        status.retained_bytes,
        file_bytes(directory).0,
        file_bytes(directory).1,
        resident_mib()
    );
    Ok(())
}
pub async fn run(
    operations: usize,
    rows: usize,
    tables: u16,
    keep: Option<PathBuf>,
) -> Result<(), Failure> {
    let tag = format!("checkpoints rows={rows} tables={tables}");
    let options = RuntimeOptions::default();
    let app = WideSchema::new(tables);
    let temporary = tempfile::tempdir()?;
    let directory = keep.unwrap_or_else(|| temporary.path().to_path_buf());
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("checkpoints.db");
    let started = Instant::now();
    let runtime = open(&path, app, options).await?;
    println!(
        "{tag} install: {} ms, resident {} MiB",
        started.elapsed().as_millis(),
        resident_mib()
    );
    let replica = runtime.domain(scope())?;
    // The table of the row each creating operation made, by its sequence.
    let placed: Vec<AtomicUsize> = (0..rows + WRITERS).map(|_| AtomicUsize::new(0)).collect();
    let created = AtomicUsize::new(0);
    let create = async || -> Result<(), Error> {
        let index = created.fetch_add(1, Ordering::Relaxed);
        let table = (index % usize::from(tables)) as u16;
        let payload = WideSchema::create(
            table,
            index as i64,
            (index % 97) as i64,
            &format!("row-{index}"),
        );
        let operation = commit(&replica, None, payload).await?;
        placed[operation.id.sequence.get() as usize - 1].store(table.into(), Ordering::Relaxed);
        Ok(())
    };
    let empty = median(
        &format!("{tag} empty-domain writes"),
        2000.min(rows),
        async |_| create().await,
    )
    .await?;
    let load_started = Instant::now();
    // Writers in flight may pass `rows` by a few; `placed` has room for them.
    let results = futures(WRITERS, |_| async {
        while created.load(Ordering::Relaxed) < rows {
            create().await?;
        }
        Ok::<_, Error>(())
    })
    .await;
    for result in results {
        result?;
    }
    report(
        &tag,
        &replica,
        &directory,
        rows,
        rows as f64 / load_started.elapsed().as_secs_f64(),
    )
    .await?;
    let edit = async |index: usize| -> Result<(), Error> {
        let sequence = index.wrapping_mul(7919) % rows + 1;
        let id = OperationId {
            node: NODE,
            sequence: AuthorSequence::new(sequence as u64)?,
        };
        let table = placed[sequence - 1].load(Ordering::Relaxed) as u16;
        commit(&replica, Some(id), WideSchema::set_a(table, index as i64))
            .await
            .map(|_| ())
    };
    let next = AtomicUsize::new(rows);
    let span = (operations.saturating_sub(rows) / REPORTS).max(1);
    let mut done = rows;
    while done < operations {
        let until = (done + span).min(operations);
        let started = Instant::now();
        let results = futures(WRITERS, |_| async {
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= until {
                    next.fetch_sub(1, Ordering::Relaxed);
                    return Ok::<_, Error>(());
                }
                edit(index).await?;
            }
        })
        .await;
        for result in results {
            result?;
        }
        next.store(until, Ordering::Relaxed);
        report(
            &tag,
            &replica,
            &directory,
            until,
            (until - done) as f64 / started.elapsed().as_secs_f64(),
        )
        .await?;
        done = until;
    }
    let full = median(&format!("{tag} full-domain writes"), 2000, async |index| {
        edit(operations + index).await
    })
    .await?;
    println!(
        "{tag}: median write {} us empty, {} us after {operations} operations ({:.2}x)",
        empty.as_micros(),
        full.as_micros(),
        full.as_secs_f64() / empty.as_secs_f64()
    );
    let started = Instant::now();
    let checkpoint = replica.checkpoint().await?;
    let pruned_prefix = replica.prune().await?;
    println!(
        "{tag} final checkpoint {} and prune: {} ms, pruned_prefix {:?}",
        checkpoint.record.sequence,
        started.elapsed().as_millis(),
        pruned_prefix
            .0
            .iter()
            .map(|(_, sequence)| sequence.get())
            .collect::<Vec<_>>()
    );
    report(&tag, &replica, &directory, operations + 2000, 0.0).await?;
    runtime.close().await?;
    println!("{tag} closed: file_bytes={}", file_bytes(&directory).0);
    let started = Instant::now();
    let runtime = open(&path, app, options).await?;
    println!(
        "{tag} reopen: {} ms, resident {} MiB",
        started.elapsed().as_millis(),
        resident_mib()
    );
    let replica = runtime.domain(scope())?;
    let started = Instant::now();
    let audit = replica.audit().await?;
    println!(
        "{tag} audit: {} ms, {} rows, {} retained operations, recomputed={}, resident {} MiB",
        started.elapsed().as_millis(),
        audit.rows,
        audit.retained_operations,
        audit.recomputed,
        resident_mib()
    );
    runtime.close().await?;
    Ok(())
}
