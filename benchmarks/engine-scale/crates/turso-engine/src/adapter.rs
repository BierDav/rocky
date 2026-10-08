//! Shared by `turso-engine` and `turso-patched`; the including crate root
//! supplies `NAME` and `VERSION`.
use std::collections::HashMap;
use std::time::Duration;

use harness::{Config, DOMAINS, NODE, Result, Row};
use turso::{Connection, Statement, Value};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Insert,
    Upsert,
    Update,
    Point,
    ByA,
    RangeB,
    Op,
    Outbox,
    Replica,
}

pub struct Turso {
    db: turso::Database,
    durable: bool,
}

pub struct TursoConn {
    runtime: tokio::runtime::Runtime,
    inner: Inner,
}

struct Inner {
    conn: Connection,
    begin: Statement,
    commit: Statement,
    /// Prepared statements per (operation, table or domain).
    cache: HashMap<(Kind, u64), Statement>,
    retries: u64,
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?)
}

async fn values(conn: &Connection, sql: &str) -> turso::Result<Vec<Vec<Value>>> {
    let mut rows = conn.query(sql, ()).await?;
    let mut all = Vec::new();
    while let Some(row) = rows.next().await? {
        all.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<turso::Result<_>>()?,
        );
    }
    Ok(all)
}

/// Sets the flush policy on this connection and reads it back.
async fn durability(conn: &Connection, durable: bool) -> Result<()> {
    let (synchronous, fullfsync) = if durable {
        ("FULL", "ON")
    } else {
        ("0", "OFF")
    };
    values(conn, &format!("PRAGMA synchronous={synchronous}")).await?;
    values(conn, &format!("PRAGMA fullfsync={fullfsync}")).await?;
    let expected = i64::from(durable);
    for (sql, expected) in [
        ("PRAGMA synchronous", expected * 2),
        ("PRAGMA fullfsync", expected),
    ] {
        let found = values(conn, sql).await?;
        if !matches!(found.as_slice(), [row] if matches!(row.as_slice(), [Value::Integer(v)] if *v == expected))
        {
            return Err(format!("{sql} is {found:?}, expected {expected}").into());
        }
    }
    // Experiment knob; unset keeps the engine default of 2,000 pages, as Rocky runs it.
    if let Ok(kib) = std::env::var("TURSO_CACHE_KIB") {
        values(conn, &format!("PRAGMA cache_size=-{}", kib.parse::<u64>()?)).await?;
    }
    Ok(())
}

async fn journal_mode(conn: &Connection, mode: &str) -> Result<()> {
    values(conn, &format!("PRAGMA journal_mode={mode}")).await?;
    let found = values(conn, "PRAGMA journal_mode").await?;
    if !matches!(found.as_slice(), [row] if matches!(row.as_slice(), [Value::Text(v)] if v == mode))
    {
        return Err(format!("journal_mode is {found:?}, expected {mode}").into());
    }
    Ok(())
}

fn retryable(error: &turso::Error) -> bool {
    match error {
        turso::Error::Busy(_) | turso::Error::BusySnapshot(_) => true,
        other => {
            let text = other.to_string();
            text.contains("Write-write conflict") || text.contains("database is locked")
        }
    }
}

fn file(config: &Config) -> String {
    config.dir.join("engine.db").to_string_lossy().into_owned()
}

impl harness::Engine for Turso {
    const NAME: &'static str = super::NAME;
    type Conn = TursoConn;

    fn version() -> String {
        super::VERSION.into()
    }

    fn install(config: &Config) -> Result<Self> {
        runtime()?.block_on(async {
            let db = turso::Builder::new_local(&file(config)).build().await?;
            let conn = db.connect()?;
            // MVCC DDL is not rollback-safe: create the schema in WAL mode, then switch.
            journal_mode(&conn, "wal").await?;
            durability(&conn, config.durable).await?;
            for table in 0..config.tables {
                conn.execute(
                    format!(
                        "CREATE TABLE t{table}(node BLOB,seq INTEGER,a INTEGER,b INTEGER,c TEXT,\
                         lamport INTEGER,stamp_node BLOB,stamp_seq INTEGER,PRIMARY KEY(node,seq))"
                    ),
                    (),
                )
                .await?;
                for column in ["a", "b"] {
                    conn.execute(format!("CREATE INDEX t{table}_{column} ON t{table}({column})"), ()).await?;
                }
            }
            for domain in 0..DOMAINS {
                for sql in [
                    format!(
                        "CREATE TABLE ops_{domain}(node BLOB,seq INTEGER,bytes BLOB,lamport INTEGER,\
                         status INTEGER,origin INTEGER,PRIMARY KEY(node,seq))"
                    ),
                    format!("CREATE TABLE outbox_{domain}(node BLOB,seq INTEGER,PRIMARY KEY(node,seq))"),
                    format!(
                        "CREATE TABLE replica_{domain}(id INTEGER PRIMARY KEY,seq INTEGER,\
                         lamport INTEGER,revision INTEGER)"
                    ),
                    format!("INSERT INTO replica_{domain} VALUES(1,0,0,0)"),
                ] {
                    conn.execute(sql, ()).await?;
                }
            }
            journal_mode(&conn, "mvcc").await?;
            Ok(Self { db, durable: config.durable })
        })
    }

    fn open(config: &Config) -> Result<Self> {
        runtime()?.block_on(async {
            let db = turso::Builder::new_local(&file(config)).build().await?;
            journal_mode(&db.connect()?, "mvcc").await?;
            Ok(Self {
                db,
                durable: config.durable,
            })
        })
    }

    fn connect(&self) -> Result<TursoConn> {
        let runtime = runtime()?;
        let inner = runtime.block_on(async {
            let conn = self.db.connect()?;
            durability(&conn, self.durable).await?;
            let begin = conn.prepare("BEGIN CONCURRENT").await?;
            let commit = conn.prepare("COMMIT").await?;
            Ok::<_, harness::Failure>(Inner {
                conn,
                begin,
                commit,
                cache: HashMap::new(),
                retries: 0,
            })
        })?;
        Ok(TursoConn { runtime, inner })
    }
}

fn sql(kind: Kind, id: u64) -> String {
    match kind {
        Kind::Insert => format!("INSERT INTO t{id} VALUES(?,?,?,?,?,?,?,?)"),
        Kind::Upsert => format!(
            "INSERT INTO t{id} VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(node,seq) DO UPDATE SET \
             a=excluded.a,b=excluded.b,c=excluded.c,lamport=excluded.lamport,\
             stamp_node=excluded.stamp_node,stamp_seq=excluded.stamp_seq"
        ),
        Kind::Update => format!("UPDATE t{id} SET a=?,c=? WHERE node=? AND seq=?"),
        Kind::Point => format!("SELECT a,b,c FROM t{id} WHERE node=? AND seq=?"),
        Kind::ByA => format!("SELECT seq,c FROM t{id} WHERE a=?"),
        Kind::RangeB => {
            format!("SELECT seq,c FROM t{id} WHERE b BETWEEN ? AND ? ORDER BY b LIMIT ?")
        }
        Kind::Op => format!("INSERT INTO ops_{id} VALUES(?,?,?,?,0,0)"),
        Kind::Outbox => format!("INSERT INTO outbox_{id} VALUES(?,?)"),
        Kind::Replica => {
            format!("UPDATE replica_{id} SET seq=?,lamport=?,revision=revision+1 WHERE id=1")
        }
    }
}

fn row_values(row: &Row) -> [Value; 8] {
    [
        Value::Blob(NODE.to_vec()),
        Value::Integer(row.seq),
        Value::Integer(row.a),
        Value::Integer(row.b),
        Value::Text(row.c.clone()),
        Value::Integer(row.lamport),
        Value::Blob(NODE.to_vec()),
        Value::Integer(row.seq),
    ]
}

impl Inner {
    async fn statement(&mut self, kind: Kind, id: u64) -> turso::Result<&mut Statement> {
        use std::collections::hash_map::Entry;
        Ok(match self.cache.entry((kind, id)) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(self.conn.prepare(sql(kind, id)).await?),
        })
    }

    async fn execute(
        &mut self,
        kind: Kind,
        id: u64,
        params: impl turso::IntoParams,
    ) -> turso::Result<u64> {
        let statement = self.statement(kind, id).await?;
        let changed = statement.execute(params).await;
        // A finished statement keeps its cursors, and their pages, until reset.
        statement.reset()?;
        changed
    }

    /// Runs the statement to completion; `each` sees every row.
    async fn query(
        &mut self,
        kind: Kind,
        id: u64,
        params: impl turso::IntoParams,
        mut each: impl FnMut(&turso::Row) -> turso::Result<()>,
    ) -> turso::Result<usize> {
        let statement = self.statement(kind, id).await?;
        let mut rows = statement.query(params).await?;
        let mut count = 0;
        let result = loop {
            match rows.next().await {
                Ok(Some(row)) => match each(&row) {
                    Ok(()) => count += 1,
                    Err(error) => break Err(error),
                },
                Ok(None) => break Ok(count),
                Err(error) => break Err(error),
            }
        };
        drop(rows);
        // A finished statement keeps its cursors, and their pages, until reset.
        statement.reset()?;
        result
    }

    /// One `BEGIN CONCURRENT` transaction, rerun as a whole after a conflict.
    async fn transaction<T>(
        &mut self,
        mut body: impl AsyncFnMut(&mut Self) -> turso::Result<T>,
    ) -> Result<T> {
        loop {
            let result = match self.begin.execute(()).await {
                Ok(_) => match body(self).await {
                    Ok(value) => self.commit.execute(()).await.map(|_| value),
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            let error = match result {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            // A conflict can already have ended the transaction inside the engine.
            if !self.conn.is_autocommit()? {
                self.conn.execute("ROLLBACK", ()).await?;
            }
            if !retryable(&error) {
                return Err(error.into());
            }
            self.retries += 1;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// An autocommit read, retried like a transaction.
    async fn read<T>(
        &mut self,
        mut body: impl AsyncFnMut(&mut Self) -> turso::Result<T>,
    ) -> Result<T> {
        loop {
            match body(self).await {
                Ok(value) => return Ok(value),
                Err(error) if retryable(&error) => {
                    self.retries += 1;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl harness::Conn for TursoConn {
    fn load(&mut self, table: u64, rows: &[Row]) -> Result<()> {
        self.runtime.block_on(self.inner.transaction(async |inner| {
            for row in rows {
                inner.execute(Kind::Insert, table, row_values(row)).await?;
            }
            Ok(())
        }))
    }

    fn insert(&mut self, table: u64, row: &Row) -> Result<()> {
        self.runtime.block_on(self.inner.transaction(async |inner| {
            inner
                .execute(Kind::Insert, table, row_values(row))
                .await
                .map(|_| ())
        }))
    }

    fn update(&mut self, table: u64, seq: i64, a: i64, c: &str) -> Result<bool> {
        self.runtime.block_on(self.inner.transaction(async |inner| {
            let changed = inner
                .execute(
                    Kind::Update,
                    table,
                    turso::params![a, c, NODE.to_vec(), seq],
                )
                .await?;
            Ok(changed > 0)
        }))
    }

    fn point(&mut self, table: u64, seq: i64) -> Result<Option<(i64, i64, String)>> {
        self.runtime.block_on(self.inner.read(async |inner| {
            let mut found = None;
            inner
                .query(
                    Kind::Point,
                    table,
                    turso::params![NODE.to_vec(), seq],
                    |row| {
                        found = Some((row.get(0)?, row.get(1)?, row.get(2)?));
                        Ok(())
                    },
                )
                .await?;
            Ok(found)
        }))
    }

    fn by_a(&mut self, table: u64, a: i64) -> Result<usize> {
        self.runtime.block_on(self.inner.read(async |inner| {
            inner
                .query(Kind::ByA, table, turso::params![a], |row| {
                    row.get::<i64>(0)?;
                    row.get::<String>(1).map(|_| ())
                })
                .await
        }))
    }

    fn range_b(&mut self, table: u64, low: i64, high: i64, limit: usize) -> Result<usize> {
        self.runtime.block_on(self.inner.read(async |inner| {
            inner
                .query(
                    Kind::RangeB,
                    table,
                    turso::params![low, high, limit as i64],
                    |row| {
                        row.get::<i64>(0)?;
                        row.get::<String>(1).map(|_| ())
                    },
                )
                .await
        }))
    }

    fn mutation(
        &mut self,
        domain: u64,
        table: u64,
        op: i64,
        bytes: &[u8],
        row: &Row,
    ) -> Result<()> {
        self.runtime.block_on(self.inner.transaction(async |inner| {
            inner.execute(Kind::Upsert, table, row_values(row)).await?;
            inner
                .execute(
                    Kind::Op,
                    domain,
                    turso::params![NODE.to_vec(), op, bytes.to_vec(), row.lamport],
                )
                .await?;
            inner
                .execute(Kind::Outbox, domain, turso::params![NODE.to_vec(), op])
                .await?;
            inner
                .execute(Kind::Replica, domain, turso::params![op, row.lamport])
                .await?;
            Ok(())
        }))
    }

    fn checkpoint(&mut self) -> Result<()> {
        let before = self.inner.retries;
        // Refused as busy while any other transaction is open, so this can starve.
        let result =
            self.runtime.block_on(self.inner.read(async |inner| {
                values(&inner.conn, "PRAGMA wal_checkpoint(TRUNCATE)").await
            }))?;
        eprintln!(
            "checkpoint: {} busy retries, result {result:?}",
            self.inner.retries - before
        );
        Ok(())
    }

    fn retries(&self) -> u64 {
        self.inner.retries
    }
}
