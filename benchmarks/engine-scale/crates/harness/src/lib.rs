//! One workload for every engine; see ../../README.md. An engine crate implements
//! `Engine` and `Conn` and calls `run::<E>()` from `main`.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Failure = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Failure>;

/// Every row carries this node; the primary key is `(node, seq)`.
pub const NODE: [u8; 16] = [3; 16];
/// Each domain has its own protocol tables and owns tables with `table % DOMAINS == domain`.
pub const DOMAINS: u64 = 8;
pub const LOAD_CHUNK: usize = 1000;
pub const OP_BYTES: usize = 256;

pub struct Config {
    pub dir: PathBuf,
    pub tables: u64,
    /// Full durability: every commit reaches stable storage (F_FULLFSYNC on macOS).
    pub durable: bool,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub seq: i64,
    pub a: i64,
    pub b: i64,
    pub c: String,
    pub lamport: i64,
}

pub trait Engine: Send + Sync + Sized + 'static {
    const NAME: &'static str;
    type Conn: Conn;
    fn version() -> String;
    /// Create the store in an empty `dir` with the full schema.
    fn install(config: &Config) -> Result<Self>;
    fn open(config: &Config) -> Result<Self>;
    /// Called on the thread that will use the connection.
    fn connect(&self) -> Result<Self::Conn>;
}

/// Each method except `load` and the reads is exactly one committed transaction.
/// Busy or conflict errors are retried inside the implementation and counted.
pub trait Conn {
    /// Bulk-load `rows` into data table `table` as one transaction.
    fn load(&mut self, table: u64, rows: &[Row]) -> Result<()>;
    fn insert(&mut self, table: u64, row: &Row) -> Result<()>;
    /// Set `a` and `c` of an existing row; `false` when the row is missing.
    fn update(&mut self, table: u64, seq: i64, a: i64, c: &str) -> Result<bool>;
    /// `(a, b, c)` by primary key.
    fn point(&mut self, table: u64, seq: i64) -> Result<Option<(i64, i64, String)>>;
    /// Fetch `(seq, c)` of every row with this `a` through the index; returns the row count.
    fn by_a(&mut self, table: u64, a: i64) -> Result<usize>;
    /// Fetch `(seq, c)` for `low <= b <= high` through the index, at most `limit` rows.
    fn range_b(&mut self, table: u64, low: i64, high: i64, limit: usize) -> Result<usize>;
    /// A Rocky-shaped write: upsert `row`, append the operation and its outbox
    /// entry, and advance the domain's replica row, atomically.
    fn mutation(&mut self, domain: u64, table: u64, op: i64, bytes: &[u8], row: &Row)
    -> Result<()>;
    /// Fold the journal into the main file, where the engine has such a step.
    fn checkpoint(&mut self) -> Result<()>;
    fn retries(&self) -> u64;
}

pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
/// Distinct `a` values per table, so an equality read returns about 20 rows.
pub fn a_cardinality(per_table: u64) -> u64 {
    (per_table / 20).max(1)
}
pub fn row(table: u64, seq: i64, per_table: u64) -> Row {
    Row {
        seq,
        a: (mix(table << 40 ^ seq as u64) % a_cardinality(per_table)) as i64,
        b: seq,
        c: format!("row {seq:>28}"),
        lamport: seq,
    }
}

struct Random(u64);
impl Random {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = mix(self.0);
        self.0 % bound.max(1)
    }
}

pub fn resident_mib() -> u64 {
    let pid = std::process::id().to_string();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map_or(0, |kib| kib / 1024)
}
fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_bytes(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

struct Out {
    prefix: String,
    file: Option<std::fs::File>,
}
impl Out {
    fn line(&mut self, metric: &str, fields: String) {
        let line = format!("{{{},\"metric\":\"{metric}\",{fields}}}", self.prefix);
        println!("{line}");
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{line}");
        }
    }
    fn value(&mut self, metric: &str, value: u64, unit: &str) {
        self.line(metric, format!("\"value\":{value},\"unit\":\"{unit}\""));
    }
    fn latency(&mut self, metric: &str, mut micros: Vec<u64>) {
        micros.sort_unstable();
        let at = |q: usize| {
            micros
                .get((micros.len() * q / 100).min(micros.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        self.line(
            metric,
            format!(
                "\"n\":{},\"p50_us\":{},\"p99_us\":{},\"max_us\":{}",
                micros.len(),
                at(50),
                at(99),
                micros.last().copied().unwrap_or(0)
            ),
        );
    }
}

fn timed<T>(micros: &mut Vec<u64>, body: impl FnOnce() -> Result<T>) -> Result<T> {
    let started = Instant::now();
    let value = body()?;
    micros.push(started.elapsed().as_micros() as u64);
    Ok(value)
}

/// Unique operation sequences across runs on the same store.
fn op_base() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_micros() as i64)
}

#[derive(Default)]
struct Seen {
    reads: Vec<u64>,
    writes: Vec<u64>,
    error: Option<String>,
}
/// Another thread reading and writing in domain 1 while the main thread works.
/// It writes sequence -1, which the load never touches.
struct Bystander {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Seen>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Bystander {
    fn start<E: Engine>(engine: Arc<E>, tables: u64, per_table: u64) -> Self {
        let table = 1 % tables;
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (stopped, shared) = (stop.clone(), seen.clone());
        let thread = std::thread::spawn(move || {
            let work = || -> Result<()> {
                let mut conn = engine.connect()?;
                let mut op = op_base() + (1 << 50);
                let bytes = [7u8; OP_BYTES];
                while !stopped.load(Ordering::Relaxed) {
                    let started = Instant::now();
                    conn.point(table, -1)?;
                    let read = started.elapsed().as_micros() as u64;
                    op += 1;
                    let started = Instant::now();
                    conn.mutation(1, table, op, &bytes, &row(table, -1, per_table))?;
                    let write = started.elapsed().as_micros() as u64;
                    if let Ok(mut seen) = shared.lock() {
                        seen.reads.push(read);
                        seen.writes.push(write);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            };
            if let Err(error) = work()
                && let Ok(mut seen) = shared.lock()
            {
                seen.error = Some(error.to_string());
            }
        });
        Self {
            stop,
            seen,
            thread: Some(thread),
        }
    }
    fn take(&self, out: &mut Out, label: &str) {
        if let Ok(mut seen) = self.seen.lock() {
            let seen = std::mem::take(&mut *seen);
            out.latency(&format!("{label}_bystander_read"), seen.reads);
            out.latency(&format!("{label}_bystander_write"), seen.writes);
            if let Some(error) = seen.error {
                out.line(
                    &format!("{label}_bystander_error"),
                    format!("\"error\":{error:?}"),
                );
            }
        }
    }
    /// Joins first, so an operation that was blocked for the whole window is counted.
    fn finish(mut self, out: &mut Out, label: &str) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.take(out, label);
    }
}

fn measure<C: Conn>(
    conn: &mut C,
    out: &mut Out,
    tables: u64,
    per_table: u64,
    seed: u64,
) -> Result<()> {
    let mut random = Random(seed);
    let mut op = op_base();
    // New rows get sequences above the loaded range, unique per run.
    let mut fresh = per_table as i64 + (op & 0x3FFF_FFFF_FFFF) * 1000;
    let bytes = [9u8; OP_BYTES];

    let mut micros = Vec::new();
    let mut found = 0u64;
    for _ in 0..2000 {
        let (table, seq) = (random.next(tables), random.next(per_table) as i64);
        found += timed(&mut micros, || conn.point(table, seq))?.is_some() as u64;
    }
    if found != 2000 {
        return Err(format!("point read found {found} of 2000").into());
    }
    out.latency("point_read", micros);

    let (mut micros, mut found) = (Vec::new(), 0u64);
    for _ in 0..1000 {
        let table = random.next(tables);
        let a = random.next(a_cardinality(per_table)) as i64;
        found += timed(&mut micros, || conn.by_a(table, a))? as u64;
    }
    out.latency("index_eq_read", micros);
    out.value("index_eq_rows_per_read_x100", found * 100 / 1000, "rows");

    let (mut micros, mut found) = (Vec::new(), 0u64);
    for _ in 0..1000 {
        let table = random.next(tables);
        let low = random.next(per_table.saturating_sub(100)) as i64;
        found += timed(&mut micros, || conn.range_b(table, low, low + 99, 100))? as u64;
    }
    out.latency("range_read_100", micros);
    out.value("range_rows_per_read", found / 1000, "rows");

    let mut micros = Vec::new();
    for _ in 0..500 {
        fresh += 1;
        let table = random.next(tables);
        let row = row(table, fresh, per_table);
        timed(&mut micros, || conn.insert(table, &row))?;
    }
    out.latency("insert", micros);

    let mut micros = Vec::new();
    for _ in 0..500 {
        let (table, seq) = (random.next(tables), random.next(per_table) as i64);
        let a = random.next(a_cardinality(per_table)) as i64;
        if !timed(&mut micros, || {
            conn.update(table, seq, a, &format!("edit {seq:>27}"))
        })? {
            return Err("update found no row".into());
        }
    }
    out.latency("update", micros);

    let mut micros = Vec::new();
    for _ in 0..500 {
        op += 1;
        let table = random.next(tables);
        let row = row(table, random.next(per_table) as i64, per_table);
        timed(&mut micros, || {
            conn.mutation(table % DOMAINS, table, op, &bytes, &row)
        })?;
    }
    out.latency("mutation", micros);
    out.value("retries", conn.retries(), "count");
    Ok(())
}

/// `writers` threads, one domain each, running mutations for `seconds`.
fn concurrent<E: Engine>(
    engine: &Arc<E>,
    out: &mut Out,
    tables: u64,
    per_table: u64,
    writers: u64,
    reader: bool,
    seconds: u64,
) -> Result<()> {
    // Threads connect first; the clock starts when all are ready.
    let ready = Arc::new(std::sync::Barrier::new(
        writers as usize + reader as usize + 1,
    ));
    let run = Duration::from_secs(seconds);
    let mut threads = Vec::new();
    for writer in 0..writers {
        let (engine, ready) = (engine.clone(), ready.clone());
        threads.push(std::thread::spawn(move || -> Result<(Vec<u64>, u64)> {
            let conn = engine.connect();
            ready.wait();
            let (mut conn, deadline) = (conn?, Instant::now() + run);
            let mut random = Random(writer + 1);
            let mut op = op_base() + (writer as i64) * (1 << 40);
            let bytes = [5u8; OP_BYTES];
            let owned = (tables / DOMAINS).max(1);
            let mut micros = Vec::new();
            while Instant::now() < deadline {
                op += 1;
                // Tables of this writer's domain; with fewer tables than domains, writers share.
                let table = (random.next(owned) * DOMAINS + writer) % tables;
                let row = row(table, random.next(per_table) as i64, per_table);
                timed(&mut micros, || {
                    conn.mutation(writer, table, op, &bytes, &row)
                })?;
            }
            Ok((micros, conn.retries()))
        }));
    }
    let reading = reader.then(|| {
        let (engine, ready) = (engine.clone(), ready.clone());
        std::thread::spawn(move || -> Result<Vec<u64>> {
            let conn = engine.connect();
            ready.wait();
            let (mut conn, deadline) = (conn?, Instant::now() + run);
            let mut random = Random(99);
            let mut micros = Vec::new();
            while Instant::now() < deadline {
                let (table, seq) = (random.next(tables), random.next(per_table) as i64);
                timed(&mut micros, || conn.point(table, seq))?;
                std::thread::sleep(Duration::from_micros(200));
            }
            Ok(micros)
        })
    });
    ready.wait();
    let started = Instant::now();
    let (mut all, mut retries) = (Vec::new(), 0);
    for thread in threads {
        let (micros, retried) = thread.join().map_err(|_| "writer panicked")??;
        all.extend(micros);
        retries += retried;
    }
    let elapsed = started.elapsed().as_millis().max(1) as u64;
    let label = format!(
        "writers_{writers}{}",
        if reader { "_with_reader" } else { "" }
    );
    out.value(
        &format!("{label}_tx_per_s"),
        all.len() as u64 * 1000 / elapsed,
        "tx/s",
    );
    out.value(&format!("{label}_retries"), retries, "count");
    out.latency(&format!("{label}_mutation"), all);
    if let Some(reading) = reading {
        out.latency(
            &format!("{label}_point_read"),
            reading.join().map_err(|_| "reader panicked")??,
        );
    }
    Ok(())
}

fn workloads<E: Engine>(
    engine: &Arc<E>,
    out: &mut Out,
    tables: u64,
    per_table: u64,
    seconds: u64,
) -> Result<()> {
    for writers in [1, 4, 8] {
        concurrent(engine, out, tables, per_table, writers, false, seconds)?;
    }
    concurrent(engine, out, tables, per_table, 4, true, seconds)
}

fn load<E: Engine>(config: Config, rows: u64, out: &mut Out) -> Result<()> {
    let (tables, per_table) = (config.tables, (rows / config.tables).max(1));
    let started = Instant::now();
    let engine = Arc::new(E::install(&config)?);
    let mut conn = engine.connect()?;
    out.value("install_ms", started.elapsed().as_millis() as u64, "ms");
    out.value("install_resident_mib", resident_mib(), "MiB");

    let bystander = Bystander::start(engine.clone(), tables, per_table);
    let started = Instant::now();
    let (mut peak, mut commits) = (0, Vec::new());
    let mut chunk = Vec::with_capacity(LOAD_CHUNK);
    for table in 0..tables {
        let mut seq = 0;
        while seq < per_table {
            chunk.clear();
            let end = (seq + LOAD_CHUNK as u64).min(per_table);
            chunk.extend((seq..end).map(|seq| row(table, seq as i64, per_table)));
            timed(&mut commits, || conn.load(table, &chunk))?;
            seq = end;
        }
        if table % (tables / 50).max(1) == 0 {
            peak = peak.max(resident_mib());
        }
    }
    let elapsed = started.elapsed().as_millis().max(1) as u64;
    out.value("load_ms", elapsed, "ms");
    out.value(
        "load_rows_per_s",
        tables * per_table * 1000 / elapsed,
        "rows/s",
    );
    out.value("load_peak_resident_mib", peak.max(resident_mib()), "MiB");
    out.latency("load_chunk_commit", commits);
    bystander.take(out, "load");
    let started = Instant::now();
    conn.checkpoint()?;
    out.value(
        "load_checkpoint_ms",
        started.elapsed().as_millis() as u64,
        "ms",
    );
    bystander.finish(out, "load_checkpoint");

    measure(&mut conn, out, tables, per_table, 0x51ED_270B)?;
    let bystander = Bystander::start(engine.clone(), tables, per_table);
    std::thread::sleep(Duration::from_millis(200));
    drop(std::mem::take(
        &mut *bystander.seen.lock().map_err(|_| "bystander lock")?,
    ));
    let started = Instant::now();
    conn.checkpoint()?;
    out.value("checkpoint_ms", started.elapsed().as_millis() as u64, "ms");
    bystander.finish(out, "checkpoint");
    out.value("files_mib", dir_bytes(&config.dir) / (1024 * 1024), "MiB");
    out.value("resident_mib", resident_mib(), "MiB");
    drop(conn);
    workloads(&engine, out, tables, per_table, 3)
}

fn reopen<E: Engine>(config: Config, rows: u64, out: &mut Out) -> Result<()> {
    let (tables, per_table) = (config.tables, (rows / config.tables).max(1));
    let started = Instant::now();
    let engine = Arc::new(E::open(&config)?);
    let mut conn = engine.connect()?;
    if conn.point(tables - 1, per_table as i64 - 1)?.is_none() {
        return Err("reopened store misses the last loaded row".into());
    }
    out.value(
        "open_first_read_us",
        started.elapsed().as_micros() as u64,
        "us",
    );
    out.value("open_resident_mib", resident_mib(), "MiB");
    measure(&mut conn, out, tables, per_table, 0x1F2C_33A7)?;
    out.value("resident_mib", resident_mib(), "MiB");
    drop(conn);
    workloads(&engine, out, tables, per_table, 3)?;
    out.value("final_resident_mib", resident_mib(), "MiB");
    Ok(())
}

const USAGE: &str =
    "usage: load|reopen|reopen-durable <dir> <tables> <rows>; results append to $ENGINE_SCALE_OUT";

pub fn run<E: Engine>() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or(USAGE)?;
    let dir = PathBuf::from(args.next().ok_or(USAGE)?);
    let tables = args.next().ok_or(USAGE)?.parse::<u64>()?.max(1);
    let rows = args.next().ok_or(USAGE)?.parse::<u64>()?;
    let durable = mode == "reopen-durable";
    let file = match std::env::var_os("ENGINE_SCALE_OUT") {
        Some(path) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
        ),
        None => None,
    };
    let mut out = Out {
        prefix: format!(
            "\"engine\":\"{}\",\"variant\":{:?},\"version\":{:?},\"tables\":{tables},\"rows\":{rows},\"mode\":\"{mode}\"",
            E::NAME,
            std::env::var("ENGINE_SCALE_VARIANT").unwrap_or_default(),
            E::version()
        ),
        file,
    };
    let config = Config {
        dir,
        tables,
        durable,
    };
    match mode.as_str() {
        "load" => {
            std::fs::create_dir_all(&config.dir)?;
            if std::fs::read_dir(&config.dir)?.next().is_some() {
                return Err("load needs an empty directory".into());
            }
            load::<E>(config, rows, &mut out)
        }
        "reopen" | "reopen-durable" => reopen::<E>(config, rows, &mut out),
        _ => Err(USAGE.into()),
    }
}
