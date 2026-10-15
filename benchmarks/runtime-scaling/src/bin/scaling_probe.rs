//! Scaling sweeps for the local runtime; see docs/runtime/scaling-limits.md.
//! Run a release build: `cargo run --release -p rocky-runtime-bench --bin scaling_probe -- all`.
use inventory_app::{InventoryApp, Policy};
use rocky_core::*;
use rocky_runtime::{
    BoundQuery, DomainInstallation, Durability, Identity, Mutation, ReadOptions, Replica, Runtime,
    RuntimeConfig, RuntimeOptions,
};
use std::time::{Duration, Instant};

#[path = "scaling/attribution.rs"]
mod attribution;
#[path = "scaling/checkpoints.rs"]
mod checkpoints;
#[path = "scaling/cpu.rs"]
mod cpu;
#[path = "scaling/encrypted.rs"]
mod encrypted;
#[path = "scaling/gate.rs"]
mod gate;
#[path = "scaling/hardening.rs"]
mod hardening;
#[path = "scaling/roles.rs"]
mod roles;
#[path = "scaling/soak.rs"]
mod soak;
#[path = "scaling/stored.rs"]
mod stored;
#[path = "scaling/wide.rs"]
mod wide;
#[path = "scaling/wide_encrypted.rs"]
mod wide_encrypted;
#[path = "scaling/wide_matrix.rs"]
mod wide_matrix;
#[path = "scaling/wide_schema.rs"]
mod wide_schema;

type Failure = Box<dyn std::error::Error>;

/// A fixture credential's validity: from a day ago for 300 days, inside the
/// longest a chain may live.
fn lifetime() -> (u64, u64) {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    (
        time - rocky_secure::credentials::DAY,
        time + 300 * rocky_secure::credentials::DAY,
    )
}

/// Inventory semantics with a manifest that declares more than two domains.
#[derive(Clone, Copy)]
struct Wide(u8, Option<&'static [&'static str]>, u16);
impl Application for Wide {
    type State = <InventoryApp as Application>::State;
    type Error = <InventoryApp as Application>::Error;
    type Policy = Policy;
    fn validate(&self, op: &Envelope) -> Result<(), Self::Error> {
        Application::validate(&InventoryApp, op)
    }
    fn admit(&self, op: &Envelope, policy: &Policy) -> Result<(), Self::Error> {
        Application::admit(&InventoryApp, op, policy)
    }
    fn apply_ready(
        &self,
        state: &mut Self::State,
        op: &Envelope,
        boundary: AdmissionBoundary,
    ) -> Result<(), Self::Error> {
        InventoryApp.apply_ready(state, op, boundary)
    }
}
impl CompiledSchema for Wide {
    fn schema(&self) -> &'static [&'static str] {
        self.1.unwrap_or_else(|| InventoryApp.schema())
    }
    fn projection_query(&self) -> &'static str {
        InventoryApp.projection_query()
    }
    fn checkpoint_tables(&self) -> &'static [&'static str] {
        InventoryApp.checkpoint_tables()
    }
}
impl StoredApplication for Wide {
    type Row = <InventoryApp as StoredApplication>::Row;
    fn descriptor(&self) -> ApplicationDescriptor {
        InventoryApp.descriptor()
    }
    fn manifest(&self) -> Option<ApplicationManifest> {
        let mut manifest = inventory_app::manifest();
        manifest.domains = (1..=self.0)
            .map(|domain| DomainRule {
                domain: DomainId([domain; 16]),
                author_log: AuthorLogScope::Domain,
                rule: "probe".into(),
            })
            .collect();
        for index in 0..self.2 {
            let name = format!("t{index}");
            manifest.entities.push(DefinitionItem {
                id: manifest.item_id("entity", &name),
                name,
                shape: "probe-row".into(),
            });
        }
        Some(manifest)
    }
    fn projection_rows(&self, state: &Self::State) -> Vec<Vec<SqlValue>> {
        InventoryApp.projection_rows(state)
    }
    fn touched(&self, op: &Envelope) -> Vec<ProjectionStatement> {
        InventoryApp.touched(op)
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<Self::State, Self::Error> {
        InventoryApp.window(rows)
    }
    fn project(&self, state: &Self::State, op: &Envelope) -> Vec<ProjectionStatement> {
        InventoryApp.project(state, op)
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Self::Error> {
        InventoryApp.decode_rows(rows)
    }
    fn read_views(
        &self,
        context: AdmissionContext,
        policy: &Policy,
    ) -> Result<Vec<ReadView>, Self::Error> {
        InventoryApp.read_views(context, policy)
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Policy,
    ) -> Result<(), Self::Error> {
        InventoryApp.admit_read(scope, caller, policy)
    }
}
const CALLER: PrincipalId = PrincipalId([7; 16]);
const NODE: NodeId = NodeId([3; 16]);
fn scope(domain: u8) -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain: DomainId([domain; 16]),
    }
}
fn installation(domains: u8, domain: u8) -> DomainInstallation<Wide> {
    DomainInstallation {
        app: Wide(domains, None, 0),
        policy: Policy {
            scope: scope(domain),
            caller: CALLER,
            reference: [9; 32],
            note_prefix: None,
        },
        identity: Identity {
            scope: scope(domain),
            node: NODE,
            caller: CALLER,
        },
        config: RuntimeConfig {
            authors: vec![(NODE, CALLER)],
            delivery_targets: Vec::new(),
            full_nodes: Vec::new(),
        },
    }
}
async fn shared(
    dir: &std::path::Path,
    domains: u8,
    options: RuntimeOptions,
) -> Result<Runtime<Wide>, Failure> {
    Ok(Runtime::open_with_options(
        dir.join("shared.db"),
        (1..=domains).map(|d| installation(domains, d)).collect(),
        options,
    )
    .await?)
}
/// `clients` closed-loop writers share one replica; each awaits its commit.
async fn write(
    replica: &Replica<Wide>,
    clients: usize,
    writes: usize,
    label: &str,
) -> Result<Vec<Duration>, rocky_runtime::Error> {
    let client = |client: usize| async move {
        let mut timings = Vec::new();
        for index in 0..writes / clients {
            let started = Instant::now();
            let mutation = Mutation {
                entity: None,
                payload: format!("{label}-{client}-{index}").into_bytes(),
            };
            replica.submit(mutation).await?.committed().await?;
            timings.push(started.elapsed());
        }
        Ok::<_, rocky_runtime::Error>(timings)
    };
    let mut all = Vec::new();
    for result in futures(clients, client).await {
        all.extend(result?);
    }
    Ok(all)
}
async fn futures<F, T>(count: usize, make: impl Fn(usize) -> F) -> Vec<T>
where
    F: Future<Output = T>,
{
    let mut pending: Vec<_> = (0..count).map(|i| Box::pin(make(i))).collect();
    let mut done: Vec<Option<T>> = (0..count).map(|_| None).collect();
    std::future::poll_fn(|context| {
        let mut ready = true;
        for (slot, future) in done.iter_mut().zip(pending.iter_mut()) {
            if slot.is_none() {
                match future.as_mut().poll(context) {
                    std::task::Poll::Ready(value) => *slot = Some(value),
                    std::task::Poll::Pending => ready = false,
                }
            }
        }
        if ready {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    done.into_iter().flatten().collect()
}
fn line(label: &str, mut timings: Vec<Duration>, elapsed: Duration) {
    timings.sort();
    let at = |q: usize| timings[(timings.len() * q / 100).min(timings.len() - 1)].as_micros();
    println!(
        "{label}: n={} throughput={:.0}/s p50_us={} p99_us={} max_us={}",
        timings.len(),
        timings.len() as f64 / elapsed.as_secs_f64(),
        at(50),
        at(99),
        timings.last().map_or(0, Duration::as_micros)
    );
}
/// Each file writer groups waiting writes across its domains by default;
/// `+single` commits each step alone. `buffered` is the default durability;
/// `full` flushes every group.
fn profiles() -> Vec<(&'static str, RuntimeOptions)> {
    let buffered = RuntimeOptions::default();
    let full = RuntimeOptions {
        durability: Durability::Full,
        ..buffered
    };
    let single = |base| RuntimeOptions {
        group_commit: 1,
        ..base
    };
    vec![
        ("buffered", buffered),
        ("buffered+single", single(buffered)),
        ("full", full),
        ("full+single", single(full)),
    ]
}
/// Same-file domains, one closed-loop client per domain.
async fn domains(clients: usize) -> Result<(), Failure> {
    for (profile, options) in profiles() {
        let options = RuntimeOptions {
            files: 1,
            ..options
        };
        for count in [1u8, 2, 4, 8, 16] {
            let dir = tempfile::tempdir()?;
            let runtime = shared(dir.path(), count, options).await?;
            let replicas: Vec<_> = (1..=count)
                .map(|d| runtime.domain(scope(d)))
                .collect::<Result<_, _>>()?;
            let started = Instant::now();
            let results = futures(count as usize, |i| write(&replicas[i], clients, 200, "d")).await;
            let elapsed = started.elapsed();
            let mut timings = Vec::new();
            for result in results {
                timings.extend(result?);
            }
            line(
                &format!("same-file {profile} domains={count} clients/domain={clients}"),
                timings,
                elapsed,
            );
            runtime.close().await?;
        }
    }
    Ok(())
}
/// Sixteen domains of one runtime spread over 1 or 4 files, each file with
/// its own writer, at both durabilities; `clients` writers per domain.
async fn layouts(clients: usize) -> Result<(), Failure> {
    for (profile, durability) in [
        ("buffered", Durability::Buffered),
        ("full", Durability::Full),
    ] {
        for files in [1usize, 4] {
            let options = RuntimeOptions {
                durability,
                files,
                ..RuntimeOptions::default()
            };
            let dir = tempfile::tempdir()?;
            let runtime = shared(dir.path(), 16, options).await?;
            let replicas: Vec<_> = (1..=16)
                .map(|d| runtime.domain(scope(d)))
                .collect::<Result<_, _>>()?;
            let started = Instant::now();
            let results = futures(16, |i| write(&replicas[i], clients, 200, "l")).await;
            let elapsed = started.elapsed();
            let mut timings = Vec::new();
            for result in results {
                timings.extend(result?);
            }
            line(
                &format!("layout {profile} domains=16 files={files} clients/domain={clients}"),
                timings,
                elapsed,
            );
            runtime.close().await?;
        }
    }
    Ok(())
}
/// One runtime, and so one file and writer, per domain.
async fn files() -> Result<(), Failure> {
    for count in [1u8, 2, 4, 8, 16] {
        let dir = tempfile::tempdir()?;
        let mut runtimes = Vec::new();
        for domain in 1..=count {
            let path = dir.path().join(format!("{domain}.db"));
            let options = RuntimeOptions {
                durability: Durability::Full,
                ..RuntimeOptions::default()
            };
            runtimes.push(
                Runtime::open_with_options(path, vec![installation(count, domain)], options)
                    .await?,
            );
        }
        let replicas: Vec<_> = runtimes
            .iter()
            .zip(1..=count)
            .map(|(runtime, d)| runtime.domain(scope(d)))
            .collect::<Result<_, _>>()?;
        let started = Instant::now();
        let results = futures(count as usize, |i| write(&replicas[i], 1, 200, "f")).await;
        let elapsed = started.elapsed();
        let mut timings = Vec::new();
        for result in results {
            timings.extend(result?);
        }
        line(
            &format!("file-per-domain full files={count}"),
            timings,
            elapsed,
        );
        for runtime in runtimes {
            runtime.close().await?;
        }
    }
    Ok(())
}
/// One domain, many concurrent clients: what batching can and cannot recover.
async fn clients() -> Result<(), Failure> {
    for (profile, options) in profiles() {
        for count in [1usize, 8, 32] {
            let dir = tempfile::tempdir()?;
            let runtime = shared(dir.path(), 1, options).await?;
            let replica = runtime.domain(scope(1))?;
            let started = Instant::now();
            let timings = write(&replica, count, 960, "c").await?;
            line(
                &format!("one-domain {profile} clients={count}"),
                timings,
                started.elapsed(),
            );
            runtime.close().await?;
        }
    }
    Ok(())
}
/// Cost of a write, read and reopen as retained history and state grow.
async fn history() -> Result<(), Failure> {
    let options = RuntimeOptions::default();
    let dir = tempfile::tempdir()?;
    let runtime = shared(dir.path(), 1, options).await?;
    let replica = runtime.domain(scope(1))?;
    let query = || BoundQuery {
        sql: "SELECT COUNT(*) FROM drafts".into(),
        values: vec![],
    };
    let read_options = ReadOptions::new(scope(1), 1);
    let mut total = 0;
    for target in [500usize, 1000, 2000, 4000] {
        let started = Instant::now();
        let timings = write(&replica, 1, target - total, "h").await?;
        line(
            &format!("history buffered writes {total}..{target}"),
            timings,
            started.elapsed(),
        );
        total = target;
        let started = Instant::now();
        let mut reads = Vec::new();
        for _ in 0..20 {
            let read_started = Instant::now();
            replica.read(query(), read_options.clone()).await?;
            reads.push(read_started.elapsed());
        }
        line(
            &format!("history read rows={total}"),
            reads,
            started.elapsed(),
        );
    }
    let overflow = write(&replica, 1, 200, "o").await;
    println!(
        "history writes beyond 4000: {}",
        overflow.map_or_else(|error| format!("refused: {error}"), |_| "accepted".into())
    );
    runtime.close().await?;
    let started = Instant::now();
    let runtime = shared(dir.path(), 1, options).await?;
    println!(
        "history reopen rows={total}: {} ms, file_bytes={}",
        started.elapsed().as_millis(),
        std::fs::read_dir(dir.path())?.try_fold(0, |sum, entry| Ok::<_, std::io::Error>(
            sum + entry?.metadata()?.len()
        ))?
    );
    runtime.close().await?;
    Ok(())
}
/// Observer fan-out: observers of one query share one read per change.
async fn observers() -> Result<(), Failure> {
    for rows in [100usize, 2000] {
        for count in [1usize, 8, 32, 120] {
            let dir = tempfile::tempdir()?;
            let runtime = shared(dir.path(), 1, RuntimeOptions::default()).await?;
            let replica = runtime.domain(scope(1))?;
            write(&replica, 1, rows, "seed").await?;
            let mut watchers = Vec::new();
            for _ in 0..count {
                let mut observer = replica.observe_read(
                    BoundQuery {
                        sql: "SELECT COUNT(*) FROM drafts".into(),
                        values: vec![],
                    },
                    ReadOptions::new(scope(1), 1),
                )?;
                observer.next().await?;
                watchers.push(observer);
            }
            let target = (rows + 50) as i64;
            let started = Instant::now();
            let cells: Vec<_> = watchers.into_iter().map(tokio::sync::Mutex::new).collect();
            let observed = futures(count, |i| {
                let cell = &cells[i];
                async move {
                    let mut observer = cell.lock().await;
                    let mut deliveries = 0usize;
                    loop {
                        let snapshot = observer.next().await?;
                        deliveries += 1;
                        if snapshot.rows == vec![vec![SqlValue::Integer(target)]] {
                            return Ok::<_, rocky_runtime::Error>((Instant::now(), deliveries));
                        }
                    }
                }
            });
            let (timings, finished) = tokio::join!(write(&replica, 1, 50, "w"), observed);
            let write_elapsed = started.elapsed();
            let mut settled = Duration::ZERO;
            let mut deliveries = 0;
            for result in finished {
                let (at, count) = result?;
                settled = settled.max(at - started);
                deliveries += count;
            }
            line(
                &format!("observers={count} rows={rows} writes"),
                timings?,
                write_elapsed,
            );
            println!(
                "observers={count} rows={rows}: settled after {} ms, {deliveries} snapshot deliveries",
                settled.as_millis()
            );
            runtime.close().await?;
        }
    }
    Ok(())
}
fn resident_mib() -> u64 {
    let pid = std::process::id().to_string();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map_or(0, |kib| kib / 1024)
}
/// Growth to 100,000 operations, where the former 4,096-operation quota
/// stood: checkpoints and pruning now run beside the writes.
async fn retention() -> Result<(), Failure> {
    let options = RuntimeOptions::default();
    let dir = tempfile::tempdir()?;
    let runtime = shared(dir.path(), 1, options).await?;
    let replica = runtime.domain(scope(1))?;
    let mut total = 0;
    for target in [4_000usize, 16_000, 64_000, 100_000] {
        write(&replica, 1, target - total - 500, "r").await?;
        let started = Instant::now();
        let timings = write(&replica, 1, 500, "r").await?;
        line(
            &format!("retention last 500 writes before {target} operations"),
            timings,
            started.elapsed(),
        );
        println!(
            "retention operations={target}: resident {} MiB",
            resident_mib()
        );
        total = target;
    }
    runtime.close().await?;
    let started = Instant::now();
    let runtime = shared(dir.path(), 1, options).await?;
    println!(
        "retention reopen operations={total}: {} ms, resident {} MiB, file_bytes={}",
        started.elapsed().as_millis(),
        resident_mib(),
        std::fs::read_dir(dir.path())?.try_fold(0, |sum, entry| Ok::<_, std::io::Error>(
            sum + entry?.metadata()?.len()
        ))?
    );
    runtime.close().await?;
    Ok(())
}
#[tokio::main]
async fn main() -> Result<(), Failure> {
    let section = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    if section == "soak" {
        return soak::run(soak::Config::parse(std::env::args().skip(2))?).await;
    }
    if section == "soak-audit" {
        return soak::audit_existing(soak::Config::parse(std::env::args().skip(2))?).await;
    }
    let all = section == "all";
    if all || section == "domains" {
        domains(1).await?;
        domains(8).await?;
    }
    if all || section == "files" {
        files().await?;
    }
    if all || section == "layouts" {
        layouts(1).await?;
        layouts(8).await?;
    }
    if all || section == "clients" {
        clients().await?;
    }
    if all || section == "history" {
        history().await?;
    }
    if all || section == "observers" {
        observers().await?;
    }
    if matches!(section.as_str(), "strong" | "strong-cut") {
        return Err("this probe mode was removed with strong commands".into());
    }
    if all || section == "retention" {
        retention().await?;
    }
    if section == "wide-matrix" {
        wide_matrix::run(
            std::env::args()
                .nth(2)
                .unwrap_or_else(|| "local/wide-baseline".into())
                .into(),
            std::env::args()
                .nth(3)
                .map(|value| value.parse())
                .transpose()?,
            std::env::args()
                .nth(4)
                .map(|value| value.parse())
                .transpose()?,
        )
        .await?;
    }
    if all || section == "wide" {
        // `wide [tables]`: resident memory is process-wide, so one table count
        // per process gives the clean figure.
        let tables = match std::env::args().nth(2) {
            Some(tables) => vec![tables.parse::<u16>()?.clamp(1, wide_schema::MAX_TABLES)],
            None => vec![20, 200, wide_schema::MAX_TABLES],
        };
        for tables in tables {
            wide::run(tables, &[500, 2000, 3500]).await?;
        }
    }
    // `stored [rows] [tables] [directory]`: the phase 3 gate. Not part of
    // `all`: loading 500,000 rows takes minutes. With a directory the file is
    // kept, and a second run measures it from a fresh process.
    if section == "stored" {
        let rows = match std::env::args().nth(2) {
            Some(rows) => rows.parse()?,
            None => 500_000,
        };
        let tables = match std::env::args().nth(3) {
            Some(tables) => tables.parse::<u16>()?.clamp(1, wide_schema::MAX_TABLES),
            None => wide_schema::MAX_TABLES,
        };
        stored::run(rows, tables, std::env::args().nth(4).map(Into::into)).await?;
    }
    // `checkpoints [operations] [rows] [tables] [directory]`: the phase 4
    // gate. Not part of `all`: two million operations take several minutes.
    if section == "checkpoints" {
        let number = |position: usize, default: usize| match std::env::args().nth(position) {
            Some(value) => value.parse(),
            None => Ok(default),
        };
        let tables = match std::env::args().nth(4) {
            Some(tables) => tables.parse::<u16>()?.clamp(1, wide_schema::MAX_TABLES),
            None => 20,
        };
        checkpoints::run(
            number(2, 2_000_000)?,
            number(3, 500_000)?,
            tables,
            std::env::args().nth(5).map(Into::into),
        )
        .await?;
    }
    // `gate [seconds] [files] [checkpoint_frames] [group] [directory]`: the
    // phase 5 gate. Not part of `all`: the qualifying run takes ten minutes.
    if section == "gate" {
        let argument = |position: usize| std::env::args().nth(position);
        gate::run(
            argument(2)
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(600),
            argument(3)
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(1),
            argument(4).map(|value| value.parse()).transpose()?,
            argument(5).map(|value| value.parse()).transpose()?,
            argument(6).map(Into::into),
            argument(7)
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(0),
        )
        .await?;
    }
    // Fixed-rate, full-durability local writes. `erp-overhead` runs matched
    // control/measured pairs to bound histogram collection cost.
    if section == "erp-open-loop" || section == "erp-overhead" || section == "erp-bulk" {
        let number = |position: usize, default: u64| match std::env::args().nth(position) {
            Some(value) => value.parse(),
            None => Ok(default),
        };
        let seconds = number(
            2,
            if section == "erp-overhead" {
                30
            } else if section == "erp-bulk" {
                180
            } else {
                60
            },
        )?;
        let tables = number(3, 2000)? as u16;
        let root = std::env::args().nth(4).map(Into::into);
        if section == "erp-bulk" {
            let directory = root;
            gate::erp_open_loop(250, seconds, tables, directory, true, true).await?;
        } else {
            for rate in [100, 250, 500, 1000] {
                if section == "erp-overhead" {
                    for collect_latency in [false, true] {
                        let tag = if collect_latency {
                            "measured"
                        } else {
                            "control"
                        };
                        let directory = root.as_ref().map(|path: &std::path::PathBuf| {
                            path.join(format!("rate-{rate}-{tag}"))
                        });
                        gate::erp_open_loop(
                            rate,
                            seconds,
                            tables,
                            directory,
                            collect_latency,
                            false,
                        )
                        .await?;
                    }
                } else {
                    let directory = root
                        .as_ref()
                        .map(|path: &std::path::PathBuf| path.join(format!("rate-{rate}")));
                    gate::erp_open_loop(rate, seconds, tables, directory, true, false).await?;
                }
            }
        }
    }
    // Fixed-rate, full-durability local writes. `erp-overhead` runs matched
    // control/measured pairs to bound histogram collection cost.
    if section == "erp-attrib" || section == "erp-instrument-overhead" {
        let number = |position: usize, default: u64| match std::env::args().nth(position) {
            Some(value) => value.parse(),
            None => Ok(default),
        };
        let seconds = number(2, if section == "erp-attrib" { 60 } else { 30 })?;
        let tables = number(3, 2000)? as u16;
        let root = std::env::args().nth(4).map(Into::into);
        for rate in [100u64, 250, 500, 1000] {
            if section == "erp-attrib" {
                let directory = root
                    .as_ref()
                    .map(|path: &std::path::PathBuf| path.join(format!("rate-{rate}")));
                attribution::erp_open_loop(rate, seconds, tables, directory, true, "erp attrib")
                    .await?;
            } else {
                for (instrument, tag) in [(false, "control"), (true, "measured")] {
                    let directory = root
                        .as_ref()
                        .map(|path: &std::path::PathBuf| path.join(format!("rate-{rate}-{tag}")));
                    attribution::erp_open_loop(rate, seconds, tables, directory, instrument, tag)
                        .await?;
                }
            }
        }
    }
    // `erp-sync [rate] [seconds] [tables] [backlog] [directory]`: same-file
    // authoring while a preloaded peer backlog is applied to another domain.
    if section == "erp-sync" {
        let number = |position: usize, default: u64| match std::env::args().nth(position) {
            Some(value) => value.parse(),
            None => Ok(default),
        };
        let rate = number(2, 250)?;
        let seconds = number(3, 120)?;
        let tables = number(4, 2000)? as u16;
        let backlog = number(5, 50_000)? as usize;
        let directory = std::env::args().nth(6).map(Into::into);
        attribution::erp_sync(rate, seconds, tables, backlog, directory).await?;
    }
    if section == "roles" {
        // `roles [rows] [behind]`: the phase 7 gate over native TCP.
        let number = |position: usize, default: usize| match std::env::args().nth(position) {
            Some(value) => value.parse(),
            None => Ok(default),
        };
        roles::run(number(2, 500_000)?, number(3, 10_000)?).await?;
    }
    if section == "wide-encrypted" {
        wide_encrypted::run().await?;
    }
    if all || section == "encrypted" {
        encrypted::run().await?;
    }
    if all || section == "hardening" {
        hardening::run().await?;
    }
    Ok(())
}
