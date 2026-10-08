//! The phase 3 gate: one domain of the wide schema loaded to a target row
//! count, then write, read, paging, observer and memory figures on it.
//! Checkpoints and pruning run beside the load with the default policy.
use super::wide::{commit, open, scope};
use super::wide_schema::WideSchema;
use super::{Failure, NODE, futures, line, resident_mib};
use rocky_core::*;
use rocky_runtime::{
    BoundQuery, CursorKind, Error, ReadCursor, ReadOptions, Replica, RuntimeOptions,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

/// One row in every `SAMPLE` created is remembered for reads and edits.
const SAMPLE: usize = 997;
const LOADERS: usize = 16;

struct Domain {
    replica: Replica<WideSchema>,
    tables: u16,
    created: AtomicUsize,
}
impl Domain {
    /// Row `index` goes to table `index % tables` with `a = index`.
    async fn create(&self) -> Result<(usize, Envelope), Error> {
        let index = self.created.fetch_add(1, Ordering::Relaxed);
        let payload = WideSchema::create(
            self.table(index),
            index as i64,
            (index % 97) as i64,
            &format!("row-{index}"),
        );
        Ok((index, commit(&self.replica, None, payload).await?))
    }
    fn table(&self, index: usize) -> u16 {
        (index % usize::from(self.tables)) as u16
    }
    async fn read(&self, query: BoundQuery, max_rows: usize) -> Result<usize, Error> {
        let options = ReadOptions::new(scope(), max_rows);
        Ok(self.replica.read(query, options).await?.rows.len())
    }
}
fn by_key(table: u16, id: OperationId) -> BoundQuery {
    BoundQuery {
        sql: format!("SELECT a FROM t{table} WHERE node=? AND sequence=?"),
        values: vec![
            SqlValue::Blob(id.node.0.to_vec()),
            SqlValue::Integer(id.sequence.get() as i64),
        ],
    }
}
fn by_index(table: u16, a: usize) -> BoundQuery {
    BoundQuery {
        sql: format!("SELECT sequence FROM t{table} WHERE a=?"),
        values: vec![SqlValue::Integer(a as i64)],
    }
}
fn range(table: u16, from: i64) -> BoundQuery {
    BoundQuery {
        sql: format!("SELECT sequence,a FROM t{table} WHERE a>=? ORDER BY a"),
        values: vec![SqlValue::Integer(from)],
    }
}
async fn timed<T>(
    label: &str,
    count: usize,
    mut step: impl AsyncFnMut(usize) -> Result<T, Error>,
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
/// Sequential writes while `observers` wait on `query`; returns writes/s.
async fn observed(
    domain: &Domain,
    label: &str,
    observers: usize,
    query: Option<BoundQuery>,
    target: (u16, OperationId),
) -> Result<f64, Failure> {
    const WRITES: usize = 2000;
    let mut tasks = Vec::new();
    let deliveries = std::sync::Arc::new(AtomicUsize::new(0));
    for _ in 0..observers {
        let Some(query) = query.clone() else { break };
        let mut observer = domain
            .replica
            .observe_read(query, ReadOptions::new(scope(), 1))?;
        observer.next().await?;
        let deliveries = deliveries.clone();
        tasks.push(tokio::spawn(async move {
            while observer.next().await.is_ok() {
                deliveries.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    let started = Instant::now();
    let mut timings = Vec::with_capacity(WRITES);
    for index in 0..WRITES {
        let step = Instant::now();
        let payload = WideSchema::set_a(target.0, -(index as i64) - 1);
        commit(&domain.replica, Some(target.1), payload).await?;
        timings.push(step.elapsed());
    }
    let elapsed = started.elapsed();
    line(label, timings, elapsed);
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    if observers > 0 {
        println!(
            "{label}: {} snapshot deliveries",
            deliveries.load(Ordering::Relaxed)
        );
    }
    Ok(WRITES as f64 / elapsed.as_secs_f64())
}
pub async fn run(rows: usize, tables: u16, keep: Option<PathBuf>) -> Result<(), Failure> {
    let tag = format!("stored rows={rows} tables={tables}");
    let options = RuntimeOptions::default();
    let app = WideSchema::new(tables);
    let temporary = tempfile::tempdir()?;
    let directory = keep.unwrap_or_else(|| temporary.path().to_path_buf());
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("stored.db");
    let existing = path.exists();
    let started = Instant::now();
    let runtime = open(&path, app, options).await?;
    println!(
        "{tag} {}: {} ms, resident {} MiB",
        if existing { "open" } else { "install" },
        started.elapsed().as_millis(),
        resident_mib()
    );
    let replica = runtime.domain(scope())?;
    let already = replica.sync_state().await?.received.len();
    let domain = Domain {
        replica,
        tables,
        created: AtomicUsize::new(if existing { rows } else { 0 }),
    };
    if existing {
        println!("{tag}: {already} retained operations found; load skipped");
    } else {
        let empty = timed(&format!("{tag} empty-domain writes"), 2000, async |_| {
            domain.create().await
        })
        .await?;
        println!(
            "{tag} empty: median write {} us, resident {} MiB",
            empty.as_micros(),
            resident_mib()
        );
        let mut reported = domain.created.load(Ordering::Relaxed);
        while domain.created.load(Ordering::Relaxed) < rows {
            let started = Instant::now();
            let until = (domain.created.load(Ordering::Relaxed) + rows / 5).min(rows);
            let results = futures(LOADERS, |_| async {
                while domain.created.load(Ordering::Relaxed) < until {
                    domain.create().await?;
                }
                Ok::<_, Error>(())
            })
            .await;
            for result in results {
                result?;
            }
            let created = domain.created.load(Ordering::Relaxed);
            println!(
                "{tag} loaded {created} rows: {:.0} rows/s since {reported}, resident {} MiB",
                (created - reported) as f64 / started.elapsed().as_secs_f64(),
                resident_mib()
            );
            reported = created;
        }
    }
    // Sampled rows: operation `index + 1` created row `index` only for the
    // sequential first writes, so samples are found through the index on `a`.
    let mut samples = Vec::new();
    for index in (0..rows).step_by(SAMPLE).take(200) {
        let table = domain.table(index);
        let found = domain
            .replica
            .read(by_index(table, index), ReadOptions::new(scope(), 1))
            .await?;
        if let Some([SqlValue::Integer(sequence)]) = found.rows.first().map(Vec::as_slice) {
            let id = OperationId {
                node: NODE,
                sequence: AuthorSequence::new(*sequence as u64)?,
            };
            samples.push((table, id, index));
        }
    }
    println!(
        "{tag}: {} sampled rows, resident {} MiB",
        samples.len(),
        resident_mib()
    );
    let full = timed(&format!("{tag} full-domain writes"), 2000, async |_| {
        domain.create().await
    })
    .await?;
    println!("{tag} full: median write {} us", full.as_micros());
    timed(&format!("{tag} full-domain edits"), 2000, async |index| {
        let (table, id, _) = samples[index % samples.len()];
        commit(
            &domain.replica,
            Some(id),
            WideSchema::set_a(table, index as i64),
        )
        .await
    })
    .await?;
    // The edits above moved `a` of the samples; restore it for the reads.
    for (table, id, index) in &samples {
        commit(
            &domain.replica,
            Some(*id),
            WideSchema::set_a(*table, *index as i64),
        )
        .await?;
    }
    let point = timed(&format!("{tag} point read by key"), 1000, async |index| {
        let (table, id, _) = samples[index % samples.len()];
        domain.read(by_key(table, id), 1).await
    })
    .await?;
    let indexed = timed(&format!("{tag} point read by index"), 1000, async |index| {
        let (table, _, a) = samples[index % samples.len()];
        domain.read(by_index(table, a), 1).await
    })
    .await?;
    println!(
        "{tag} point reads: median {} us by key, {} us by index",
        point.as_micros(),
        indexed.as_micros()
    );
    let (table, id, a) = samples[samples.len() / 2];
    for (name, query) in [
        ("key", by_key(table, id)),
        ("index", by_index(table, a)),
        ("range", range(0, 0)),
    ] {
        let plan = domain
            .replica
            .explain_read(query, ReadOptions::new(scope(), 100))
            .await?;
        println!("{tag} plan {name}: {plan:?}");
    }
    // Pages of one table's rows in index order, first by cursor, then with
    // the last key bound into the predicate.
    let per_table = rows / usize::from(tables);
    for page in [10usize, 100, 1000] {
        if page * 4 > per_table {
            println!("{tag} range page={page}: skipped, a table holds {per_table} rows");
            continue;
        }
        let mut cursor = None;
        let first = timed(
            &format!("{tag} range page={page} by cursor, first pages"),
            20,
            async |index| {
                // Restart every fourth page.
                let options = ReadOptions::new(scope(), page);
                let options = match cursor.take().filter(|_| index % 4 != 0) {
                    Some(cursor) => options.after(cursor),
                    None => options,
                };
                let snapshot = domain.replica.read(range(0, 0), options).await?;
                cursor = snapshot.next;
                Ok(snapshot.rows.len())
            },
        )
        .await?;
        let depth = per_table / 2;
        // The ordered query continues by key. A `LIMIT` makes the same query
        // ineligible, so its cursor is a row offset.
        let unkeyed = BoundQuery {
            sql: format!("{} LIMIT -1", range(0, 0).sql),
            ..range(0, 0)
        };
        let mut deep = Vec::new();
        for (name, query, kind) in [
            ("keyset", range(0, 0), CursorKind::Keyset),
            ("offset", unkeyed, CursorKind::Offset),
        ] {
            let mut cursor = domain
                .replica
                .read(query.clone(), ReadOptions::new(scope(), 1))
                .await?
                .next;
            let mut skipped = 1;
            while skipped < depth {
                let Some(at) = cursor.take() else { break };
                let step = (depth - skipped).min(1024);
                let snapshot = domain
                    .replica
                    .read(query.clone(), ReadOptions::new(scope(), step).after(at))
                    .await?;
                skipped += snapshot.rows.len();
                cursor = snapshot.next;
            }
            assert_eq!(cursor.as_ref().map(ReadCursor::kind), Some(kind));
            deep.push(
                timed(
                    &format!("{tag} range page={page} by {name} cursor, {skipped} rows deep"),
                    20,
                    async |_| {
                        let options = ReadOptions::new(scope(), page);
                        let options = match cursor.clone() {
                            Some(cursor) => options.after(cursor),
                            None => options,
                        };
                        Ok(domain
                            .replica
                            .read(query.clone(), options)
                            .await?
                            .rows
                            .len())
                    },
                )
                .await?,
            );
        }
        // Table 0 holds the rows whose `a` is a multiple of the table count.
        let from = (depth * usize::from(tables)) as i64;
        let bound = timed(
            &format!("{tag} range page={page} by bound key, {depth} rows deep"),
            20,
            async |_| domain.read(range(0, from), page).await,
        )
        .await?;
        println!(
            "{tag} range page={page}: median {} us first, {} us deep by keyset cursor, {} us deep by offset cursor, {} us deep by bound key",
            first.as_micros(),
            deep[0].as_micros(),
            deep[1].as_micros(),
            bound.as_micros()
        );
    }
    // Observers: edits to one row of table 0, watched or not.
    let (table, id, _) = samples[0];
    let unrelated = by_key(domain.table(1), samples[1 % samples.len()].1);
    let alone = observed(
        &domain,
        &format!("{tag} edits, no observers"),
        0,
        None,
        (table, id),
    )
    .await?;
    let beside = observed(
        &domain,
        &format!("{tag} edits, 120 observers of another table"),
        120,
        Some(unrelated),
        (table, id),
    )
    .await?;
    let watched = observed(
        &domain,
        &format!("{tag} edits, 120 observers of the edited row"),
        120,
        Some(by_key(table, id)),
        (table, id),
    )
    .await?;
    println!(
        "{tag} observers: {alone:.0} writes/s alone, {beside:.0} with 120 unrelated ({:.0}%), {watched:.0} with 120 on the edited row ({:.0}%)",
        100.0 * beside / alone,
        100.0 * watched / alone
    );
    println!("{tag}: resident {} MiB after all sections", resident_mib());
    drop(domain);
    runtime.close().await?;
    let bytes = std::fs::read_dir(&directory)?.try_fold(0, |sum, entry| {
        Ok::<_, std::io::Error>(sum + entry?.metadata()?.len())
    })?;
    let started = Instant::now();
    let runtime = open(&path, app, options).await?;
    println!(
        "{tag} reopen: {} ms, resident {} MiB, file_bytes={bytes}",
        started.elapsed().as_millis(),
        resident_mib()
    );
    runtime.close().await?;
    Ok(())
}
