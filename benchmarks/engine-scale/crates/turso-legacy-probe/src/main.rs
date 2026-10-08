//! The pinned engine without Rocky, at target size; see benchmarks/engine-scale/RESULTS.md.
//! `cargo run --release -p turso-legacy-probe -- load <dir> <tables> <rows>`
//! then `... -- reopen <dir> <tables> <rows>` in a fresh process.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Failure = Box<dyn std::error::Error>;

const NODE: [u8; 16] = [3; 16];
/// Sequences above this are written by the measurements, not by the load.
const EXTRA: i64 = 1 << 40;

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
fn file_mib(dir: &std::path::Path) -> u64 {
    let bytes: u64 = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum();
    bytes / (1024 * 1024)
}
fn report(label: &str, mut micros: Vec<u128>) {
    micros.sort_unstable();
    let at = |q: usize| micros.get((micros.len() * q / 100).min(micros.len().saturating_sub(1)));
    println!(
        "{label}: n={} p50_us={} p99_us={} max_us={}",
        micros.len(),
        at(50).copied().unwrap_or(0),
        at(99).copied().unwrap_or(0),
        micros.last().copied().unwrap_or(0)
    );
}
struct Random(u64);
impl Random {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound.max(1)
    }
}
async fn drain(mut rows: turso::Rows) -> Result<usize, Failure> {
    let mut count = 0;
    while rows.next().await?.is_some() {
        count += 1;
    }
    Ok(count)
}
async fn pragma(conn: &turso::Connection, sql: &str) -> Result<(), Failure> {
    drain(conn.query(sql, ()).await?).await?;
    Ok(())
}
fn insert(table: u64) -> String {
    format!("INSERT INTO t{table} VALUES(?,?,?,?,?,?,?,?)")
}
fn values(id: i64) -> [turso::Value; 8] {
    [
        turso::Value::Blob(NODE.to_vec()),
        turso::Value::Integer(id),
        turso::Value::Integer(
            ((id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as i64 % 1000,
        ),
        turso::Value::Integer(id),
        turso::Value::Text(format!("row {id:>28}")),
        turso::Value::Integer(id),
        turso::Value::Blob(NODE.to_vec()),
        turso::Value::Integer(id),
    ]
}

/// What another thread sees while the loader commits and checkpoints.
#[derive(Default)]
struct Bystander {
    micros: Vec<u128>,
    errors: usize,
    last_error: Option<String>,
}
fn bystander(
    db: turso::Database,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Bystander>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            let Ok(conn) = db.connect() else { return };
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let result: Result<(), Failure> = async {
                    conn.execute("BEGIN CONCURRENT", ()).await?;
                    let read = conn
                        .query(
                            "SELECT a FROM t0 WHERE node=? AND sequence=?",
                            turso::params![NODE.to_vec(), 0i64],
                        )
                        .await;
                    let read = match read {
                        Ok(rows) => drain(rows).await.map(|_| ()),
                        Err(error) => Err(error.into()),
                    };
                    conn.execute(if read.is_ok() { "COMMIT" } else { "ROLLBACK" }, ())
                        .await?;
                    read
                }
                .await;
                if let Ok(mut seen) = seen.lock() {
                    match result {
                        Ok(()) => seen.micros.push(started.elapsed().as_micros()),
                        Err(error) => {
                            seen.errors += 1;
                            seen.last_error = Some(error.to_string());
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
    })
}

async fn measure(
    conn: &turso::Connection,
    label: &str,
    tables: u64,
    per_table: u64,
    random: &mut Random,
    extra: &mut i64,
) -> Result<(), Failure> {
    let rows = tables * per_table;
    let mut micros = Vec::new();
    for _ in 0..300 {
        *extra += 1;
        let started = Instant::now();
        conn.execute("BEGIN CONCURRENT", ()).await?;
        conn.execute(insert(random.next(tables)), values(*extra))
            .await?;
        conn.execute("COMMIT", ()).await?;
        micros.push(started.elapsed().as_micros());
    }
    report(&format!("{label} insert, sql text, any table"), micros);

    let mut statement = conn.prepare(insert(0)).await?;
    let mut micros = Vec::new();
    for _ in 0..300 {
        *extra += 1;
        let started = Instant::now();
        conn.execute("BEGIN CONCURRENT", ()).await?;
        statement.execute(values(*extra)).await?;
        conn.execute("COMMIT", ()).await?;
        micros.push(started.elapsed().as_micros());
    }
    report(&format!("{label} insert, prepared, one table"), micros);

    let mut micros = Vec::new();
    for _ in 0..300 {
        let id = random.next(rows);
        let started = Instant::now();
        conn.execute("BEGIN CONCURRENT", ()).await?;
        conn.execute(
            format!(
                "UPDATE t{} SET a=?,c=? WHERE node=? AND sequence=?",
                id % tables
            ),
            turso::params![
                random.next(1000) as i64,
                format!("edit {id:>27}"),
                NODE.to_vec(),
                id as i64
            ],
        )
        .await?;
        conn.execute("COMMIT", ()).await?;
        micros.push(started.elapsed().as_micros());
    }
    report(&format!("{label} update by key, sql text"), micros);

    let mut micros = Vec::new();
    let mut found = 0;
    for _ in 0..1000 {
        let id = random.next(rows);
        let started = Instant::now();
        found += drain(
            conn.query(
                format!(
                    "SELECT a,b,c FROM t{} WHERE node=? AND sequence=?",
                    id % tables
                ),
                turso::params![NODE.to_vec(), id as i64],
            )
            .await?,
        )
        .await?;
        micros.push(started.elapsed().as_micros());
    }
    report(
        &format!("{label} point read, sql text, found {found}"),
        micros,
    );

    let mut statement = conn
        .prepare("SELECT a,b,c FROM t0 WHERE node=? AND sequence=?")
        .await?;
    let mut micros = Vec::new();
    let mut found = 0;
    for _ in 0..1000 {
        let id = random.next(per_table) * tables;
        let started = Instant::now();
        found += drain(
            statement
                .query(turso::params![NODE.to_vec(), id as i64])
                .await?,
        )
        .await?;
        micros.push(started.elapsed().as_micros());
    }
    report(
        &format!("{label} point read, prepared, found {found}"),
        micros,
    );

    let mut micros = Vec::new();
    let mut found = 0;
    for _ in 0..500 {
        let started = Instant::now();
        found += drain(
            conn.query(
                format!("SELECT sequence,c FROM t{} WHERE a=?", random.next(tables)),
                turso::params![random.next(1000) as i64],
            )
            .await?,
        )
        .await?;
        micros.push(started.elapsed().as_micros());
    }
    report(
        &format!("{label} index read, {found} rows in total"),
        micros,
    );

    let mut micros = Vec::new();
    let mut found = 0;
    for _ in 0..500 {
        let low = random.next(rows) as i64;
        let started = Instant::now();
        found += drain(
            conn.query(
                format!(
                    "SELECT sequence,c FROM t{} WHERE b BETWEEN ? AND ? LIMIT 100",
                    random.next(tables)
                ),
                turso::params![low, low + 100 * tables as i64],
            )
            .await?,
        )
        .await?;
        micros.push(started.elapsed().as_micros());
    }
    report(
        &format!("{label} range read, {found} rows in total"),
        micros,
    );
    Ok(())
}

async fn load(dir: &std::path::Path, tables: u64, rows: u64) -> Result<(), Failure> {
    let db = turso::Builder::new_local(&dir.join("engine.db").to_string_lossy())
        .build()
        .await?;
    let conn = db.connect()?;
    // Same order as the runtime: schema in WAL mode, then MVCC.
    pragma(&conn, "PRAGMA journal_mode=wal").await?;
    pragma(&conn, "PRAGMA synchronous=0").await?;
    let started = Instant::now();
    for table in 0..tables {
        conn.execute(format!("CREATE TABLE t{table}(node BLOB,sequence INTEGER,a INTEGER,b INTEGER,c TEXT,lamport INTEGER,stamp_node BLOB,stamp_sequence INTEGER,PRIMARY KEY(node,sequence))"), ()).await?;
        for column in ["a", "b"] {
            conn.execute(
                format!("CREATE INDEX t{table}_{column} ON t{table}({column})"),
                (),
            )
            .await?;
        }
    }
    pragma(&conn, "PRAGMA journal_mode=mvcc").await?;
    println!(
        "engine tables={tables} install: {} ms, resident {} MiB",
        started.elapsed().as_millis(),
        resident_mib()
    );

    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Bystander::default()));
    let thread = bystander(db.clone(), stop.clone(), seen.clone());
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut extra = EXTRA;
    let target = (rows / tables).max(1);
    let mut loaded = 0u64;
    for share in [2u64, 10, 20, 50, 100] {
        let until = (target * share / 100).max(loaded + 1).min(target);
        if until <= loaded {
            continue;
        }
        let started = Instant::now();
        let mut commits = Vec::new();
        for table in 0..tables {
            let mut statement = conn.prepare(insert(table)).await?;
            conn.execute("BEGIN CONCURRENT", ()).await?;
            for row in loaded..until {
                statement
                    .execute(values((row * tables + table) as i64))
                    .await?;
            }
            let commit = Instant::now();
            conn.execute("COMMIT", ()).await?;
            commits.push(commit.elapsed().as_micros());
        }
        let added = (until - loaded) * tables;
        loaded = until;
        let label = format!("engine tables={tables} rows={}", loaded * tables);
        println!(
            "{label} load: {added} rows in {} ms ({}/s), resident {} MiB, files {} MiB",
            started.elapsed().as_millis(),
            added as u128 * 1000 / started.elapsed().as_millis().max(1),
            resident_mib(),
            file_mib(dir)
        );
        report(&format!("{label} batch commit"), commits);
        if let Ok(mut seen) = seen.lock() {
            let seen = std::mem::take(&mut *seen);
            report(&format!("{label} other thread during load"), seen.micros);
            println!(
                "{label} other thread errors: {} {}",
                seen.errors,
                seen.last_error.unwrap_or_default()
            );
        }
        measure(&conn, &label, tables, loaded, &mut random, &mut extra).await?;
        let started = Instant::now();
        let result = pragma(&conn, "PRAGMA wal_checkpoint(TRUNCATE)").await;
        println!(
            "{label} forced checkpoint: {} ms ({}), resident {} MiB, files {} MiB",
            started.elapsed().as_millis(),
            result.map_or_else(|error| error.to_string(), |()| "ok".into()),
            resident_mib(),
            file_mib(dir)
        );
        if let Ok(mut seen) = seen.lock() {
            let seen = std::mem::take(&mut *seen);
            report(
                &format!("{label} other thread during measures"),
                seen.micros,
            );
            println!(
                "{label} other thread errors: {} {}",
                seen.errors,
                seen.last_error.unwrap_or_default()
            );
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = thread.join();
    Ok(())
}

async fn reopen(dir: &std::path::Path, tables: u64, rows: u64) -> Result<(), Failure> {
    let label = format!("engine tables={tables} rows={rows} reopen");
    let started = Instant::now();
    let db = turso::Builder::new_local(&dir.join("engine.db").to_string_lossy())
        .build()
        .await?;
    let conn = db.connect()?;
    pragma(&conn, "PRAGMA journal_mode=mvcc").await?;
    pragma(&conn, "PRAGMA synchronous=0").await?;
    let found = drain(
        conn.query(
            "SELECT a FROM t0 WHERE node=? AND sequence=?",
            turso::params![NODE.to_vec(), 0i64],
        )
        .await?,
    )
    .await?;
    println!(
        "{label}: first read after {} ms (found {found}), resident {} MiB, files {} MiB",
        started.elapsed().as_millis(),
        resident_mib(),
        file_mib(dir)
    );
    let mut random = Random(0x51ED_270B_1F2C_33A7);
    let mut extra = EXTRA * 2;
    measure(
        &conn,
        &label,
        tables,
        (rows / tables).max(1),
        &mut random,
        &mut extra,
    )
    .await?;
    println!("{label}: resident after measures {} MiB", resident_mib());
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Failure> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    let dir = std::path::PathBuf::from(
        args.next()
            .ok_or("usage: load|reopen <dir> <tables> <rows>")?,
    );
    let tables = args
        .next()
        .map_or(Ok(2000), |text| text.parse::<u64>())?
        .max(1);
    let rows = args
        .next()
        .map_or(Ok(5_000_000), |text| text.parse::<u64>())?;
    match mode.as_str() {
        "load" => {
            std::fs::create_dir_all(&dir)?;
            load(&dir, tables, rows).await
        }
        "reopen" => reopen(&dir, tables, rows).await,
        _ => Err("usage: load|reopen <dir> <tables> <rows>".into()),
    }
}
