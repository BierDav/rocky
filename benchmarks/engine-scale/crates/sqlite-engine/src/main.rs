//! SQLite in WAL mode; one file, one connection per thread.
use std::path::PathBuf;
use std::time::Duration;

use harness::{Config, Conn, DOMAINS, Engine, NODE, Result, Row};
use rusqlite::{Connection, ErrorCode, OpenFlags, params};

/// Statement shapes per data table, plus slack for the per-domain statements.
const SHAPES_PER_TABLE: usize = 6;

struct Sqlite {
    path: PathBuf,
    tables: u64,
    durable: bool,
}

struct SqliteConn {
    conn: Connection,
    retries: u64,
}

fn env_i64(name: &str) -> Option<i64> {
    std::env::var(name).ok()?.parse().ok()
}

fn busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn pause(attempt: u32) {
    std::thread::sleep(Duration::from_micros(200 * u64::from(attempt.clamp(1, 5))));
}

impl Sqlite {
    fn new(config: &Config) -> Self {
        Self {
            path: config.dir.join("db.sqlite"),
            tables: config.tables,
            durable: config.durable,
        }
    }

    /// Opening can meet `SQLITE_BUSY_RECOVERY` while another thread recovers the WAL.
    fn raw(&self, create: bool) -> Result<Connection> {
        let mut attempt = 0;
        let (conn, actual) = loop {
            match self.configure(create) {
                Ok(opened) => break opened,
                Err(error) if busy(&error) => {}
                Err(error) => return Err(error.into()),
            }
            attempt += 1;
            pause(attempt);
        };
        let expected = if self.durable { (2, 1, 1) } else { (0, 0, 0) };
        if actual != expected {
            return Err(format!("durability pragmas are {actual:?}, expected {expected:?}").into());
        }
        Ok(conn)
    }

    fn configure(&self, create: bool) -> rusqlite::Result<(Connection, (i64, i64, i64))> {
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
        let conn = Connection::open_with_flags(&self.path, flags)?;
        // Busy errors are retried and counted here, not inside SQLite.
        conn.busy_timeout(Duration::ZERO)?;
        if let Some(kib) = env_i64("SQLITE_CACHE_KIB") {
            conn.pragma_update(None, "cache_size", -kib)?;
        }
        if let Some(bytes) = env_i64("SQLITE_MMAP_BYTES") {
            conn.pragma_update(None, "mmap_size", bytes)?;
        }
        if self.durable {
            conn.pragma_update(None, "synchronous", "FULL")?;
            conn.pragma_update(None, "fullfsync", "ON")?;
            conn.pragma_update(None, "checkpoint_fullfsync", "ON")?;
        } else {
            conn.pragma_update(None, "synchronous", "OFF")?;
        }
        let read = |name: &str| conn.pragma_query_value(None, name, |row| row.get::<_, i64>(0));
        let actual = (
            read("synchronous")?,
            read("fullfsync")?,
            read("checkpoint_fullfsync")?,
        );
        // Load the schema now so later statements do not pay for or fail on it.
        conn.query_row("SELECT count(*) FROM sqlite_schema WHERE 0", [], |_| Ok(()))?;
        Ok((conn, actual))
    }
}

impl Engine for Sqlite {
    const NAME: &'static str = "sqlite";
    type Conn = SqliteConn;

    fn version() -> String {
        let suffix = if env_i64("SQLITE_WITHOUT_ROWID") == Some(1) {
            " without-rowid"
        } else {
            ""
        };
        format!("{} wal{suffix}", rusqlite::version())
    }

    fn install(config: &Config) -> Result<Self> {
        let engine = Self::new(config);
        let mut conn = engine.raw(true)?;
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if mode != "wal" {
            return Err(format!("journal mode is {mode}").into());
        }
        let tail = if env_i64("SQLITE_WITHOUT_ROWID") == Some(1) {
            " WITHOUT ROWID"
        } else {
            ""
        };
        let tx = conn.transaction()?;
        for table in 0..config.tables {
            tx.execute_batch(&format!(
                "CREATE TABLE t{table}(node BLOB NOT NULL,seq INTEGER NOT NULL,a INTEGER,b INTEGER,c TEXT,\
                 lamport INTEGER,stamp_node BLOB,stamp_seq INTEGER,PRIMARY KEY(node,seq)){tail};\
                 CREATE INDEX t{table}_a ON t{table}(a);\
                 CREATE INDEX t{table}_b ON t{table}(b);"
            ))?;
        }
        for domain in 0..DOMAINS {
            tx.execute_batch(&format!(
                "CREATE TABLE ops_{domain}(node BLOB NOT NULL,seq INTEGER NOT NULL,bytes BLOB,lamport INTEGER,\
                 status INTEGER,origin INTEGER,PRIMARY KEY(node,seq)){tail};\
                 CREATE TABLE outbox_{domain}(node BLOB NOT NULL,seq INTEGER NOT NULL,PRIMARY KEY(node,seq)){tail};\
                 CREATE TABLE replica_{domain}(id INTEGER PRIMARY KEY,seq INTEGER,lamport INTEGER,revision INTEGER);\
                 INSERT INTO replica_{domain} VALUES(0,0,0,0);"
            ))?;
        }
        tx.commit()?;
        Ok(engine)
    }

    fn open(config: &Config) -> Result<Self> {
        let engine = Self::new(config);
        let mode: String = engine
            .raw(false)?
            .pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if mode != "wal" {
            return Err(format!("journal mode is {mode}").into());
        }
        Ok(engine)
    }

    fn connect(&self) -> Result<SqliteConn> {
        let conn = self.raw(false)?;
        let capacity = env_i64("SQLITE_STMT_CACHE")
            .map_or(self.tables as usize * SHAPES_PER_TABLE + 64, |n| n as usize);
        conn.set_prepared_statement_cache_capacity(capacity);
        Ok(SqliteConn { conn, retries: 0 })
    }
}

impl SqliteConn {
    /// One `BEGIN IMMEDIATE` transaction, rerun from the start while the file is busy.
    fn write<T>(&mut self, mut body: impl FnMut(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut attempt = 0;
        loop {
            let result = self
                .conn
                .prepare_cached("BEGIN IMMEDIATE")
                .and_then(|mut begin| begin.execute([]))
                .and_then(|_| body(&self.conn))
                .and_then(|value| {
                    self.conn
                        .prepare_cached("COMMIT")?
                        .execute([])
                        .map(|_| value)
                });
            match result {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if !self.conn.is_autocommit() {
                        self.conn.execute_batch("ROLLBACK")?;
                    }
                    if !busy(&error) {
                        return Err(error.into());
                    }
                }
            }
            attempt += 1;
            self.retries += 1;
            pause(attempt);
        }
    }

    /// A read, rerun while busy (only possible during WAL recovery or a truncate).
    fn read<T>(&mut self, mut body: impl FnMut(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut attempt = 0;
        loop {
            match body(&self.conn) {
                Ok(value) => return Ok(value),
                Err(error) if busy(&error) => {}
                Err(error) => return Err(error.into()),
            }
            attempt += 1;
            self.retries += 1;
            pause(attempt);
        }
    }
}

fn insert_row(conn: &Connection, table: u64, row: &Row) -> rusqlite::Result<usize> {
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
}

impl Conn for SqliteConn {
    fn load(&mut self, table: u64, rows: &[Row]) -> Result<()> {
        self.write(|conn| {
            let mut statement =
                conn.prepare_cached(&format!("INSERT INTO t{table} VALUES(?,?,?,?,?,?,?,?)"))?;
            for row in rows {
                statement.execute(params![
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
            Ok(())
        })
    }

    fn insert(&mut self, table: u64, row: &Row) -> Result<()> {
        self.write(|conn| insert_row(conn, table, row).map(|_| ()))
    }

    fn update(&mut self, table: u64, seq: i64, a: i64, c: &str) -> Result<bool> {
        self.write(|conn| {
            conn.prepare_cached(&format!(
                "UPDATE t{table} SET a=?,c=? WHERE node=? AND seq=?"
            ))?
            .execute(params![a, c, &NODE[..], seq])
            .map(|changed| changed > 0)
        })
    }

    fn point(&mut self, table: u64, seq: i64) -> Result<Option<(i64, i64, String)>> {
        self.read(|conn| {
            let mut statement = conn.prepare_cached(&format!(
                "SELECT a,b,c FROM t{table} WHERE node=? AND seq=?"
            ))?;
            let mut rows = statement.query(params![&NODE[..], seq])?;
            match rows.next()? {
                Some(row) => Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?))),
                None => Ok(None),
            }
        })
    }

    fn by_a(&mut self, table: u64, a: i64) -> Result<usize> {
        self.read(|conn| {
            let mut statement =
                conn.prepare_cached(&format!("SELECT seq,c FROM t{table} WHERE a=?"))?;
            let mut rows = statement.query(params![a])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                let _: (i64, String) = (row.get(0)?, row.get(1)?);
                count += 1;
            }
            Ok(count)
        })
    }

    fn range_b(&mut self, table: u64, low: i64, high: i64, limit: usize) -> Result<usize> {
        self.read(|conn| {
            let mut statement = conn.prepare_cached(&format!(
                "SELECT seq,c FROM t{table} WHERE b BETWEEN ? AND ? ORDER BY b LIMIT ?"
            ))?;
            let mut rows = statement.query(params![low, high, limit as i64])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                let _: (i64, String) = (row.get(0)?, row.get(1)?);
                count += 1;
            }
            Ok(count)
        })
    }

    fn mutation(
        &mut self,
        domain: u64,
        table: u64,
        op: i64,
        bytes: &[u8],
        row: &Row,
    ) -> Result<()> {
        self.write(|conn| {
            conn.prepare_cached(&format!(
                "INSERT INTO t{table} VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(node,seq) DO UPDATE SET \
                 a=excluded.a,b=excluded.b,c=excluded.c,lamport=excluded.lamport,\
                 stamp_node=excluded.stamp_node,stamp_seq=excluded.stamp_seq"
            ))?
            .execute(params![&NODE[..], row.seq, row.a, row.b, row.c, row.lamport, &NODE[..], op])?;
            conn.prepare_cached(&format!("INSERT INTO ops_{domain} VALUES(?,?,?,?,0,0)"))?
                .execute(params![&NODE[..], op, bytes, row.lamport])?;
            conn.prepare_cached(&format!("INSERT INTO outbox_{domain} VALUES(?,?)"))?
                .execute(params![&NODE[..], op])?;
            let changed = conn
                .prepare_cached(&format!("UPDATE replica_{domain} SET seq=?,lamport=?,revision=revision+1 WHERE id=0"))?
                .execute(params![op, row.lamport])?;
            if changed != 1 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            Ok(())
        })
    }

    fn checkpoint(&mut self) -> Result<()> {
        // The pragma reports a blocked checkpoint in its first column rather than as an error.
        self.read(|conn| {
            let blocked: i64 =
                conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
            if blocked != 0 {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    None,
                ));
            }
            Ok(())
        })
    }

    fn retries(&self) -> u64 {
        self.retries
    }
}

fn main() -> Result<()> {
    harness::run::<Sqlite>()
}
