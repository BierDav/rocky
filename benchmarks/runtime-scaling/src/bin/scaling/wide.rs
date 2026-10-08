//! Wide-schema sweep: load one domain towards today's quotas and report every
//! cost on the way. A refusal is printed as a result, never raised.
use super::wide_schema::{Policy, WideSchema};
use super::{CALLER, Failure, NODE, futures, line, resident_mib};
use rocky_core::*;
use rocky_runtime::{
    BoundQuery, DomainInstallation, Error, Identity, Mutation, ReadOptions, Replica, Runtime,
    RuntimeConfig, RuntimeOptions,
};
use std::time::{Duration, Instant};

pub(super) fn scope() -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: WideSchema::app_id(),
        domain: DomainId([1; 16]),
    }
}
pub(super) async fn open(
    path: &std::path::Path,
    app: WideSchema,
    options: RuntimeOptions,
) -> Result<Runtime<WideSchema>, Error> {
    let installation = DomainInstallation {
        app,
        policy: Policy {
            scope: scope(),
            caller: CALLER,
        },
        identity: Identity {
            scope: scope(),
            node: NODE,
            caller: CALLER,
        },
        config: RuntimeConfig {
            authors: vec![(NODE, CALLER)],
            delivery_targets: Vec::new(),
            full_nodes: Vec::new(),
        },
    };
    Runtime::open_with_options(path, vec![installation], options).await
}
pub(super) async fn commit(
    replica: &Replica<WideSchema>,
    entity: Option<OperationId>,
    payload: Vec<u8>,
) -> Result<Envelope, Error> {
    loop {
        let mutation = Mutation {
            entity,
            payload: payload.clone(),
        };
        match replica.mutate(mutation) {
            Ok(receipt) => return receipt.committed().await,
            Err(Error::Backpressure) => tokio::task::yield_now().await,
            Err(error) => return Err(error),
        }
    }
}
/// Times `count` steps. A refused step ends the batch and is reported.
async fn timed(
    label: &str,
    count: usize,
    mut step: impl AsyncFnMut(usize) -> Result<(), Error>,
) -> bool {
    let started = Instant::now();
    let mut timings = Vec::new();
    for index in 0..count {
        let step_started = Instant::now();
        if let Err(error) = step(index).await {
            println!("{label}: refused after {index}: {error}");
            return false;
        }
        timings.push(step_started.elapsed());
    }
    if !timings.is_empty() {
        line(label, timings, started.elapsed());
    }
    true
}
struct Load {
    replica: Replica<WideSchema>,
    tables: u16,
    /// Live rows as (table, entity).
    live: Vec<(u16, OperationId)>,
    created: usize,
    operations: usize,
}
impl Load {
    async fn create(&mut self) -> Result<(), Error> {
        let table = (self.created % usize::from(self.tables)) as u16;
        let payload = WideSchema::create(
            table,
            self.created as i64,
            (self.created % 97) as i64,
            &format!("row-{}", self.created),
        );
        let operation = commit(&self.replica, None, payload).await?;
        self.live.push((table, operation.id));
        self.created += 1;
        self.operations += 1;
        Ok(())
    }
    async fn update(&mut self, index: usize, a: i64) -> Result<(), Error> {
        let (table, entity) = self.live[index % self.live.len()];
        commit(&self.replica, Some(entity), WideSchema::set_a(table, a)).await?;
        self.operations += 1;
        Ok(())
    }
    async fn delete_last(&mut self) -> Result<(), Error> {
        if let Some((table, entity)) = self.live.pop() {
            commit(&self.replica, Some(entity), WideSchema::delete(table)).await?;
            self.operations += 1;
        }
        Ok(())
    }
    fn point(&self) -> BoundQuery {
        let (table, entity) = self.live[0];
        BoundQuery {
            sql: format!("SELECT a FROM t{table} WHERE node=? AND sequence=?"),
            values: vec![
                SqlValue::Blob(entity.node.0.to_vec()),
                SqlValue::Integer(entity.sequence.get() as i64),
            ],
        }
    }
    fn range(&self) -> BoundQuery {
        BoundQuery {
            sql: format!(
                "SELECT sequence,a FROM t{} WHERE a>=? AND a<? ORDER BY a",
                self.live[0].0
            ),
            values: vec![SqlValue::Integer(0), SqlValue::Integer(i64::MAX)],
        }
    }
    /// Observers of one row while that row is edited `writes` times.
    async fn observe(&mut self, tag: &str, count: usize, writes: usize) -> Result<(), Error> {
        let options = ReadOptions::new(scope(), 1);
        let mut watchers = Vec::new();
        for _ in 0..count {
            let mut observer = self.replica.observe_read(self.point(), options.clone())?;
            observer.next().await?;
            watchers.push(tokio::sync::Mutex::new(observer));
        }
        let target = -(self.operations as i64) - writes as i64;
        let started = Instant::now();
        let observed = futures(count, |index| {
            let watcher = &watchers[index];
            async move {
                let mut observer = watcher.lock().await;
                loop {
                    if observer.next().await?.rows == vec![vec![SqlValue::Integer(target)]] {
                        return Ok::<_, Error>(started.elapsed());
                    }
                }
            }
        });
        let (replica, (table, entity)) = (self.replica.clone(), self.live[0]);
        let writer = async {
            let mut timings = Vec::new();
            for index in 1..=writes {
                let write_started = Instant::now();
                let value = target + (writes - index) as i64;
                commit(&replica, Some(entity), WideSchema::set_a(table, value)).await?;
                timings.push(write_started.elapsed());
            }
            Ok::<_, Error>(timings)
        };
        let (timings, settled) = tokio::join!(writer, observed);
        let elapsed = started.elapsed();
        self.operations += writes;
        let mut slowest = Duration::ZERO;
        for result in settled {
            slowest = slowest.max(result?);
        }
        line(
            &format!("{tag} observers={count} writes"),
            timings?,
            elapsed,
        );
        println!(
            "{tag} observers={count}: settled after {} ms",
            slowest.as_millis()
        );
        Ok(())
    }
}
pub async fn run(tables: u16, checkpoints: &[usize]) -> Result<(), Failure> {
    let tag = format!("wide tables={tables}");
    let options = RuntimeOptions::default();
    let app = WideSchema::new(tables);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("wide.db");
    let started = Instant::now();
    let runtime = match open(&path, app, options).await {
        Ok(runtime) => runtime,
        Err(error) => {
            println!("{tag} install: refused: {error}");
            return Ok(());
        }
    };
    println!(
        "{tag} install: {} ms, resident {} MiB",
        started.elapsed().as_millis(),
        resident_mib()
    );
    let mut load = Load {
        replica: runtime.domain(scope())?,
        tables,
        live: Vec::new(),
        created: 0,
        operations: 0,
    };
    let read = ReadOptions::new(scope(), 1024);
    for &target in checkpoints {
        let from = load.live.len();
        let loaded = timed(
            &format!("{tag} load rows {from}..{target}"),
            target.saturating_sub(from),
            async |_| load.create().await,
        )
        .await;
        if !loaded {
            break;
        }
        let rows = format!("{tag} rows={target}");
        // Create, edit and delete in equal parts; the live row count is unchanged.
        timed(&format!("{rows} mixed writes"), 60, async |index| {
            match index % 3 {
                0 => load.create().await,
                1 => load.update(index, index as i64).await,
                _ => load.delete_last().await,
            }
        })
        .await;
        for (name, query) in [("point", load.point()), ("range", load.range())] {
            timed(&format!("{rows} {name} read"), 20, async |_| {
                load.replica
                    .read(query.clone(), read.clone())
                    .await
                    .map(|_| ())
            })
            .await;
        }
        for count in [1, 8] {
            if let Err(error) = load.observe(&rows, count, 20).await {
                println!("{rows} observers={count}: refused: {error}");
            }
        }
        println!("{rows}: resident {} MiB", resident_mib());
    }
    let before = load.operations;
    let refusal = loop {
        if let Err(error) = load.create().await {
            break error;
        }
    };
    println!(
        "{tag} quota: {} more creates accepted, then refused at {} operations and {} live rows: {refusal}",
        load.operations - before,
        load.operations,
        load.live.len()
    );
    let rows = load.live.len();
    drop(load);
    runtime.close().await?;
    let started = Instant::now();
    match open(&path, app, options).await {
        Ok(runtime) => {
            println!(
                "{tag} reopen rows={rows}: {} ms, resident {} MiB, file_bytes={}",
                started.elapsed().as_millis(),
                resident_mib(),
                std::fs::read_dir(dir.path())?.try_fold(0, |sum, entry| {
                    Ok::<_, std::io::Error>(sum + entry?.metadata()?.len())
                })?
            );
            runtime.close().await?;
        }
        Err(error) => println!("{tag} reopen rows={rows}: refused: {error}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe's point and range reads are served by the stored tables'
    /// primary key and declared index, through the view that hides tombstones.
    #[tokio::test]
    async fn point_and_range_reads_use_the_declared_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = open(
            &dir.path().join("plans.db"),
            WideSchema::new(3),
            RuntimeOptions::default(),
        )
        .await
        .unwrap();
        let mut load = Load {
            replica: runtime.domain(scope()).unwrap(),
            tables: 3,
            live: Vec::new(),
            created: 0,
            operations: 0,
        };
        for _ in 0..30 {
            load.create().await.unwrap();
        }
        load.delete_last().await.unwrap();
        let options = ReadOptions::new(scope(), 1024);
        let plan = load
            .replica
            .explain_read(load.point(), options.clone())
            .await
            .unwrap();
        assert!(
            plan.iter()
                .all(|step| step.starts_with("SEARCH") && step.contains("sqlite_autoindex")),
            "{plan:?}"
        );
        let plan = load
            .replica
            .explain_read(load.range(), options.clone())
            .await
            .unwrap();
        assert!(
            plan[0].starts_with("SEARCH") && plan[0].contains("_t0_a (a>? AND a<?)"),
            "{plan:?}"
        );
        // Ordered by the indexed column, so a page stops early. The key that
        // the runtime appends for its cursor is sorted only among rows with
        // equal `a`, because the index does not hold the key columns.
        assert!(
            plan[1..]
                .iter()
                .all(|step| step == "USE TEMP B-TREE FOR LAST 2 TERMS OF ORDER BY"),
            "{plan:?}"
        );
        // Table 0 holds every third row; the deleted one was in table 2.
        let range = load
            .replica
            .read(load.range(), options.clone())
            .await
            .unwrap();
        assert_eq!(range.rows.len(), 10);
        let first = load
            .replica
            .read(load.range(), ReadOptions::new(scope(), 4))
            .await
            .unwrap();
        assert_eq!((first.rows.len(), first.next.is_some()), (4, true));
        let deleted = BoundQuery {
            sql: "SELECT COUNT(*) FROM t2".into(),
            values: Vec::new(),
        };
        assert_eq!(
            load.replica
                .read(deleted, options.clone())
                .await
                .unwrap()
                .rows,
            vec![vec![SqlValue::Integer(9)]]
        );
        // Editing a deleted row keeps it deleted: its tombstone is stored.
        let entity = OperationId {
            node: NODE,
            sequence: AuthorSequence::new(30).unwrap(),
        };
        commit(&load.replica, Some(entity), WideSchema::set_a(2, 7))
            .await
            .unwrap();
        let snapshot = load.replica.query().await.unwrap();
        assert_eq!(snapshot.rows.len(), 29);
        runtime.close().await.unwrap();
        let runtime = open(
            &dir.path().join("plans.db"),
            WideSchema::new(3),
            RuntimeOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            runtime.domain(scope()).unwrap().query().await.unwrap(),
            snapshot
        );
        runtime.close().await.unwrap();
    }
}
