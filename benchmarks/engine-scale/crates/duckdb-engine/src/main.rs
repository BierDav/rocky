//! DuckDB; one file, one database instance, one cloned connection per thread.
//! `DUCKDB_PROFILE=constrained` caps memory and threads for a phone-sized budget.
use std::fs::File;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use duckdb::{Connection, params};
use harness::{Config, Conn, DOMAINS, Engine, NODE, Result, Row};

/// Statement shapes per data table, plus slack for the per-domain statements.
const SHAPES_PER_TABLE: usize = 6;

struct Duck {
    root: Mutex<Connection>,
    tables: u64,
    /// Present when commits must reach stable storage; see `DuckConn::settle`.
    sentinel: Option<PathBuf>,
}

struct DuckConn {
    conn: Connection,
    sentinel: Option<File>,
    /// `DUCKDB_UPSERT=update-first`: UPDATE, then INSERT when no row matched.
    update_first: bool,
    retries: u64,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn constrained() -> bool {
    env("DUCKDB_PROFILE").as_deref() == Some("constrained")
}

/// Optimistic-concurrency failures; DuckDB has no error code for them.
fn conflict(error: &duckdb::Error) -> bool {
    let text = error.to_string();
    // The second is a plain CHECKPOINT meeting an open write transaction.
    text.contains("onflict") || text.contains("Cannot CHECKPOINT: there are other write")
}

fn pause(attempt: u32) {
    std::thread::sleep(Duration::from_micros(200 * u64::from(attempt.clamp(1, 5))));
}

impl Duck {
    fn start(config: &Config) -> Result<Self> {
        let mut options = duckdb::Config::default().enable_autoload_extension(false)?;
        if constrained() {
            options = options.max_memory("256MB")?.threads(2)?;
        }
        if let Some(memory) = env("DUCKDB_MEMORY") {
            options = options.max_memory(&memory)?;
        }
        if let Some(threads) = env("DUCKDB_THREADS") {
            options = options.threads(threads.parse()?)?;
        }
        if let Some(threshold) = env("DUCKDB_CHECKPOINT_THRESHOLD") {
            options = options.with("checkpoint_threshold", threshold)?;
        }
        // Only takes effect when the file is created; the default is 256 KiB.
        if let Some(bytes) = env("DUCKDB_BLOCK_SIZE") {
            options = options.with("default_block_size", bytes)?;
        }
        let root = Connection::open_with_flags(config.dir.join("db.duckdb"), options)?;
        // The bundled build syncs with fsync() only, so durable mode adds F_FULLFSYNC itself.
        let full = config.durable && env("DUCKDB_FULLFSYNC").as_deref() != Some("0");
        Ok(Self {
            root: Mutex::new(root),
            tables: config.tables,
            sentinel: full.then(|| config.dir.join("fullsync")),
        })
    }
}

impl Engine for Duck {
    const NAME: &'static str = "duckdb";
    type Conn = DuckConn;

    fn version() -> String {
        let mut text = format!(
            "1.5.6 {}",
            if constrained() {
                "constrained"
            } else {
                "default"
            }
        );
        for name in [
            "DUCKDB_MEMORY",
            "DUCKDB_THREADS",
            "DUCKDB_CHECKPOINT_THRESHOLD",
            "DUCKDB_FULLFSYNC",
            "DUCKDB_UPSERT",
        ] {
            if let Some(value) = env(name) {
                text.push_str(&format!(
                    " {}={value}",
                    name.trim_start_matches("DUCKDB_").to_lowercase()
                ));
            }
        }
        text
    }

    fn install(config: &Config) -> Result<Self> {
        let engine = Self::start(config)?;
        {
            let root = engine.root.lock().map_err(|_| "root connection lock")?;
            root.execute_batch("BEGIN")?;
            for table in 0..config.tables {
                root.execute_batch(&format!(
                    "CREATE TABLE t{table}(node BLOB NOT NULL,seq BIGINT NOT NULL,a BIGINT,b BIGINT,c VARCHAR,\
                     lamport BIGINT,stamp_node BLOB,stamp_seq BIGINT,PRIMARY KEY(node,seq));\
                     CREATE INDEX t{table}_a ON t{table}(a);\
                     CREATE INDEX t{table}_b ON t{table}(b);"
                ))?;
            }
            for domain in 0..DOMAINS {
                root.execute_batch(&format!(
                    "CREATE TABLE ops_{domain}(node BLOB NOT NULL,seq BIGINT NOT NULL,bytes BLOB,lamport BIGINT,\
                     status BIGINT,origin BIGINT,PRIMARY KEY(node,seq));\
                     CREATE TABLE outbox_{domain}(node BLOB NOT NULL,seq BIGINT NOT NULL,PRIMARY KEY(node,seq));\
                     CREATE TABLE replica_{domain}(id BIGINT PRIMARY KEY,seq BIGINT,lamport BIGINT,revision BIGINT);\
                     INSERT INTO replica_{domain} VALUES(0,0,0,0);"
                ))?;
            }
            root.execute_batch("COMMIT")?;
        }
        Ok(engine)
    }

    fn open(config: &Config) -> Result<Self> {
        Self::start(config)
    }

    fn connect(&self) -> Result<DuckConn> {
        let conn = self
            .root
            .lock()
            .map_err(|_| "root connection lock")?
            .try_clone()?;
        conn.set_prepared_statement_cache_capacity(self.tables as usize * SHAPES_PER_TABLE + 64);
        let sentinel = match &self.sentinel {
            Some(path) => Some(
                File::options()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(path)?,
            ),
            None => None,
        };
        let update_first = env("DUCKDB_UPSERT").as_deref() == Some("update-first");
        Ok(DuckConn {
            conn,
            sentinel,
            update_first,
            retries: 0,
        })
    }
}

impl DuckConn {
    /// DuckDB has already fsynced its log; this asks the drive to flush its cache
    /// (Rust's `sync_all` is F_FULLFSYNC on Apple platforms), which covers that log.
    fn settle(&self) -> Result<()> {
        if let Some(file) = &self.sentinel {
            file.sync_all()?;
        }
        Ok(())
    }

    /// Rerun `body` from the start while it fails with a transaction conflict.
    fn retrying<T>(&mut self, mut body: impl FnMut(&Connection) -> duckdb::Result<T>) -> Result<T> {
        let mut attempt = 0;
        loop {
            match body(&self.conn) {
                Ok(value) => {
                    self.settle()?;
                    return Ok(value);
                }
                Err(error) if conflict(&error) => {}
                Err(error) => return Err(error.into()),
            }
            attempt += 1;
            self.retries += 1;
            pause(attempt);
        }
    }

    /// One explicit transaction around `body`.
    fn write<T>(&mut self, mut body: impl FnMut(&Connection) -> duckdb::Result<T>) -> Result<T> {
        self.retrying(|conn| {
            conn.prepare_cached("BEGIN")?.execute([])?;
            let result = body(conn)
                .and_then(|value| conn.prepare_cached("COMMIT")?.execute([]).map(|_| value));
            if result.is_err() {
                // Fails harmlessly when the failed commit already ended the transaction.
                let _ = conn.execute_batch("ROLLBACK");
            }
            result
        })
    }
}

impl Conn for DuckConn {
    fn load(&mut self, table: u64, rows: &[Row]) -> Result<()> {
        let name = format!("t{table}");
        self.write(|conn| {
            let mut appender = conn.appender(&name)?;
            for row in rows {
                appender.append_row(params![
                    &NODE[..],
                    row.seq,
                    row.a,
                    row.b,
                    row.c,
                    row.lamport,
                    &NODE[..],
                    row.seq
                ])?;
            }
            appender.flush()
        })
    }

    // A single statement in autocommit mode is one committed transaction.
    fn insert(&mut self, table: u64, row: &Row) -> Result<()> {
        self.retrying(|conn| {
            conn.prepare_cached(&format!("INSERT INTO t{table} VALUES(?,?,?,?,?,?,?,?)"))?
                .execute(params![
                    &NODE[..],
                    row.seq,
                    row.a,
                    row.b,
                    row.c,
                    row.lamport,
                    &NODE[..],
                    row.seq
                ])
                .map(|_| ())
        })
    }

    fn update(&mut self, table: u64, seq: i64, a: i64, c: &str) -> Result<bool> {
        self.retrying(|conn| {
            conn.prepare_cached(&format!(
                "UPDATE t{table} SET a=?,c=? WHERE node=? AND seq=?"
            ))?
            .execute(params![a, c, &NODE[..], seq])
            .map(|changed| changed > 0)
        })
    }

    fn point(&mut self, table: u64, seq: i64) -> Result<Option<(i64, i64, String)>> {
        let mut statement = self.conn.prepare_cached(&format!(
            "SELECT a,b,c FROM t{table} WHERE node=? AND seq=?"
        ))?;
        let mut rows = statement.query(params![&NODE[..], seq])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?))),
            None => Ok(None),
        }
    }

    fn by_a(&mut self, table: u64, a: i64) -> Result<usize> {
        let mut statement = self
            .conn
            .prepare_cached(&format!("SELECT seq,c FROM t{table} WHERE a=?"))?;
        let mut rows = statement.query(params![a])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let _: (i64, String) = (row.get(0)?, row.get(1)?);
            count += 1;
        }
        Ok(count)
    }

    fn range_b(&mut self, table: u64, low: i64, high: i64, limit: usize) -> Result<usize> {
        let mut statement = self.conn.prepare_cached(&format!(
            "SELECT seq,c FROM t{table} WHERE b BETWEEN ? AND ? ORDER BY b LIMIT ?"
        ))?;
        let mut rows = statement.query(params![low, high, limit as i64])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let _: (i64, String) = (row.get(0)?, row.get(1)?);
            count += 1;
        }
        Ok(count)
    }

    fn mutation(
        &mut self,
        domain: u64,
        table: u64,
        op: i64,
        bytes: &[u8],
        row: &Row,
    ) -> Result<()> {
        let update_first = self.update_first;
        self.write(|conn| {
            if update_first {
                let changed = conn
                    .prepare_cached(&format!(
                        "UPDATE t{table} SET a=?,b=?,c=?,lamport=?,stamp_node=?,stamp_seq=? WHERE node=? AND seq=?"
                    ))?
                    .execute(params![
                        row.a,
                        row.b,
                        row.c,
                        row.lamport,
                        &NODE[..],
                        op,
                        &NODE[..],
                        row.seq
                    ])?;
                if changed == 0 {
                    conn.prepare_cached(&format!("INSERT INTO t{table} VALUES(?,?,?,?,?,?,?,?)"))?
                        .execute(params![
                            &NODE[..],
                            row.seq,
                            row.a,
                            row.b,
                            row.c,
                            row.lamport,
                            &NODE[..],
                            op
                        ])?;
                }
            } else {
                conn.prepare_cached(&format!(
                    "INSERT INTO t{table} VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(node,seq) DO UPDATE SET \
                     a=excluded.a,b=excluded.b,c=excluded.c,lamport=excluded.lamport,\
                     stamp_node=excluded.stamp_node,stamp_seq=excluded.stamp_seq"
                ))?
                .execute(params![
                    &NODE[..],
                    row.seq,
                    row.a,
                    row.b,
                    row.c,
                    row.lamport,
                    &NODE[..],
                    op
                ])?;
            }
            conn.prepare_cached(&format!("INSERT INTO ops_{domain} VALUES(?,?,?,?,0,0)"))?
                .execute(params![&NODE[..], op, bytes, row.lamport])?;
            conn.prepare_cached(&format!("INSERT INTO outbox_{domain} VALUES(?,?)"))?
                .execute(params![&NODE[..], op])?;
            let changed = conn
                .prepare_cached(&format!(
                    "UPDATE replica_{domain} SET seq=?,lamport=?,revision=revision+1 WHERE id=0"
                ))?
                .execute(params![op, row.lamport])?;
            if changed != 1 {
                return Err(duckdb::Error::QueryReturnedNoRows);
            }
            Ok(())
        })
    }

    fn checkpoint(&mut self) -> Result<()> {
        self.retrying(|conn| conn.execute_batch("CHECKPOINT"))
    }

    fn retries(&self) -> u64 {
        self.retries
    }
}

/// `sql <dir> <statement>...` prints each result as text; used to check plans and counts.
fn sql(mut args: impl Iterator<Item = String>) -> Result<()> {
    let config = Config {
        dir: args.next().ok_or("sql <dir> <statement>...")?.into(),
        tables: 1,
        durable: false,
    };
    let conn = Duck::start(&config)?.connect()?.conn;
    for text in args {
        let mut statement = conn.prepare(&text)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let mut line = Vec::new();
            while let Ok(value) = row.get_ref(line.len()) {
                line.push(match value {
                    duckdb::types::ValueRef::Text(text) => {
                        String::from_utf8_lossy(text).into_owned()
                    }
                    other => format!("{other:?}"),
                });
            }
            println!("{}", line.join(" | "));
        }
    }
    Ok(())
}

fn main() -> harness::Result<()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("sql") {
        return sql(args);
    }
    harness::run::<Duck>()
}
