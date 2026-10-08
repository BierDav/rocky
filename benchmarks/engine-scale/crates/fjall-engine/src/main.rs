//! fjall: LSM keyspaces with optimistic transactions, tables and indexes built by hand.
//! `KV_LAYOUT=prefixed` keeps all rows in one keyspace and all index entries in another.
use std::sync::Arc;
use std::time::Duration;

use fjall::{
    KeyspaceCreateOptions, OptimisticTxDatabase, OptimisticTxKeyspace, OptimisticWriteTx,
    PersistMode, Readable,
};
use harness::{Config, Conn, DOMAINS, Engine, NODE, Result, Row};

const SIGN: u64 = 1 << 63;
const PK: usize = 24;
const INDEX_A: u8 = 0;
const INDEX_B: u8 = 1;
/// Row value: node, seq, a, b, lamport, stamp_node, stamp_seq, c length, c.
const A_AT: usize = 24;
const B_AT: usize = 32;
const C_AT: usize = 76;
const REPLICA_KEY: [u8; 8] = [0; 8];

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(default)
}
fn prefixed() -> bool {
    std::env::var("KV_LAYOUT").is_ok_and(|layout| layout == "prefixed")
}
/// `FJALL_CHECKPOINT=persist` only syncs the journal instead of flushing memtables.
fn persist_only() -> bool {
    std::env::var("FJALL_CHECKPOINT").is_ok_and(|mode| mode == "persist")
}

fn ordered(value: i64) -> [u8; 8] {
    (value as u64 ^ SIGN).to_be_bytes()
}
fn int_at(bytes: &[u8], at: usize) -> Result<i64> {
    let raw = bytes.get(at..at + 8).ok_or("short value")?;
    Ok(i64::from_be_bytes(raw.try_into()?))
}
fn pk(seq: i64) -> [u8; PK] {
    let mut key = [0; PK];
    key[..16].copy_from_slice(&NODE);
    key[16..].copy_from_slice(&ordered(seq));
    key
}
fn encode_row(seq: i64, a: i64, b: i64, c: &str, lamport: i64, stamp_seq: i64) -> Vec<u8> {
    let mut value = Vec::with_capacity(C_AT + c.len());
    value.extend_from_slice(&NODE);
    for field in [seq, a, b, lamport] {
        value.extend_from_slice(&field.to_be_bytes());
    }
    value.extend_from_slice(&NODE);
    value.extend_from_slice(&stamp_seq.to_be_bytes());
    value.extend_from_slice(&(c.len() as u32).to_be_bytes());
    value.extend_from_slice(c.as_bytes());
    value
}
fn text(value: &[u8]) -> Result<String> {
    let length =
        u32::from_be_bytes(value.get(C_AT - 4..C_AT).ok_or("short row")?.try_into()?) as usize;
    Ok(std::str::from_utf8(value.get(C_AT..C_AT + length).ok_or("short row")?)?.to_owned())
}

struct Spaces {
    prefixed: bool,
    rows: Vec<OptimisticTxKeyspace>,
    index_a: Vec<OptimisticTxKeyspace>,
    index_b: Vec<OptimisticTxKeyspace>,
    ops: Vec<OptimisticTxKeyspace>,
    outbox: Vec<OptimisticTxKeyspace>,
    replica: Vec<OptimisticTxKeyspace>,
}

/// Where a data table lives: keyspaces and the key prefixes inside them.
struct Place<'a> {
    rows: &'a OptimisticTxKeyspace,
    index_a: &'a OptimisticTxKeyspace,
    index_b: &'a OptimisticTxKeyspace,
    table: Option<u32>,
}
impl Spaces {
    fn place(&self, table: u64) -> Result<Place<'_>> {
        let at = if self.prefixed { 0 } else { table as usize };
        // Both indexes share one keyspace in the prefixed layout.
        let index_b = if self.prefixed {
            &self.index_a
        } else {
            &self.index_b
        };
        Ok(Place {
            rows: self.rows.get(at).ok_or("unknown table")?,
            index_a: self.index_a.get(at).ok_or("unknown table")?,
            index_b: index_b.get(at).ok_or("unknown table")?,
            table: self.prefixed.then_some(table as u32),
        })
    }
    fn all(&self) -> impl Iterator<Item = &OptimisticTxKeyspace> {
        [
            &self.rows,
            &self.index_a,
            &self.index_b,
            &self.ops,
            &self.outbox,
            &self.replica,
        ]
        .into_iter()
        .flatten()
    }
}
impl Place<'_> {
    fn row_key(&self, pk: &[u8]) -> Vec<u8> {
        let mut key = Vec::with_capacity(4 + PK);
        if let Some(table) = self.table {
            key.extend_from_slice(&table.to_be_bytes());
        }
        key.extend_from_slice(pk);
        key
    }
    fn index_prefix(&self, index: u8, value: i64) -> Vec<u8> {
        let mut key = Vec::with_capacity(5 + 8 + PK);
        if let Some(table) = self.table {
            key.extend_from_slice(&table.to_be_bytes());
            key.push(index);
        }
        key.extend_from_slice(&ordered(value));
        key
    }
    fn index_key(&self, index: u8, value: i64, pk: &[u8]) -> Vec<u8> {
        let mut key = self.index_prefix(index, value);
        key.extend_from_slice(pk);
        key
    }
    fn index(&self, index: u8) -> &OptimisticTxKeyspace {
        if index == INDEX_A {
            self.index_a
        } else {
            self.index_b
        }
    }
    fn reindex(
        &self,
        tx: &mut OptimisticWriteTx,
        index: u8,
        pk: &[u8],
        old: Option<i64>,
        new: i64,
    ) {
        if old == Some(new) {
            return;
        }
        if let Some(old) = old {
            tx.remove(self.index(index), self.index_key(index, old, pk));
        }
        tx.insert(self.index(index), self.index_key(index, new, pk), []);
    }
    /// Write a row and its index entries; `unique` rejects an existing primary key.
    fn put(
        &self,
        tx: &mut OptimisticWriteTx,
        row: &Row,
        stamp_seq: i64,
        unique: bool,
    ) -> Result<()> {
        let pk = pk(row.seq);
        let key = self.row_key(&pk);
        let old = match tx.get(self.rows, &key)? {
            Some(old) => Some((int_at(&old, A_AT)?, int_at(&old, B_AT)?)),
            None => None,
        };
        if unique && old.is_some() {
            return Err(format!("duplicate primary key {}", row.seq).into());
        }
        tx.insert(
            self.rows,
            key,
            encode_row(row.seq, row.a, row.b, &row.c, row.lamport, stamp_seq),
        );
        self.reindex(tx, INDEX_A, &pk, old.map(|old| old.0), row.a);
        self.reindex(tx, INDEX_B, &pk, old.map(|old| old.1), row.b);
        Ok(())
    }
}

struct Fjall {
    db: OptimisticTxDatabase,
    spaces: Arc<Spaces>,
    durable: bool,
}

struct FjallConn {
    db: OptimisticTxDatabase,
    spaces: Arc<Spaces>,
    durable: bool,
    retries: u64,
}

impl Fjall {
    /// Opens the store; keyspaces are created on first use and found again afterwards.
    fn new(config: &Config) -> Result<Self> {
        let db = OptimisticTxDatabase::builder(config.dir.join("fjall"))
            .cache_size(env_u64("FJALL_CACHE_MIB", 512) << 20)
            .max_cached_files(Some(env_u64("FJALL_MAX_FILES", 1024) as usize))
            .max_journaling_size(env_u64("FJALL_JOURNAL_MIB", 512) << 20)
            .open()?;
        let prefixed = prefixed();
        let space = |name: String| db.keyspace(&name, KeyspaceCreateOptions::default);
        let many = |count: u64, name: &dyn Fn(u64) -> String| {
            (0..count)
                .map(|at| space(name(at)))
                .collect::<fjall::Result<Vec<_>>>()
        };
        let spaces = if prefixed {
            Spaces {
                prefixed,
                rows: vec![space("rows".into())?],
                index_a: vec![space("idx".into())?],
                index_b: Vec::new(),
                ops: many(DOMAINS, &|at| format!("ops_{at}"))?,
                outbox: many(DOMAINS, &|at| format!("outbox_{at}"))?,
                replica: many(DOMAINS, &|at| format!("replica_{at}"))?,
            }
        } else {
            Spaces {
                prefixed,
                rows: many(config.tables, &|at| format!("t{at}"))?,
                index_a: many(config.tables, &|at| format!("t{at}_a"))?,
                index_b: many(config.tables, &|at| format!("t{at}_b"))?,
                ops: many(DOMAINS, &|at| format!("ops_{at}"))?,
                outbox: many(DOMAINS, &|at| format!("outbox_{at}"))?,
                replica: many(DOMAINS, &|at| format!("replica_{at}"))?,
            }
        };
        Ok(Self {
            db,
            spaces: Arc::new(spaces),
            durable: config.durable,
        })
    }
}

/// Flush every non-empty memtable to a table file, then sync the journal.
fn flush(db: &OptimisticTxDatabase, spaces: &Spaces) -> Result<()> {
    if !persist_only() {
        for space in spaces.all() {
            space.inner().rotate_memtable()?;
        }
        while spaces
            .all()
            .any(|space| space.inner().sealed_memtable_count() > 0)
        {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(db.persist(PersistMode::SyncAll)?)
}

/// A clean shutdown leaves no journal to replay on the next open.
impl Drop for Fjall {
    fn drop(&mut self) {
        let _ = flush(&self.db, &self.spaces);
    }
}

impl Engine for Fjall {
    const NAME: &'static str = "fjall";
    type Conn = FjallConn;

    fn version() -> String {
        let layout = if prefixed() { "prefixed" } else { "per-table" };
        let checkpoint = if persist_only() { "persist" } else { "flush" };
        format!(
            "3.1.12 occ {layout} cache={}MiB journal={}MiB checkpoint={checkpoint}",
            env_u64("FJALL_CACHE_MIB", 512),
            env_u64("FJALL_JOURNAL_MIB", 512)
        )
    }

    fn install(config: &Config) -> Result<Self> {
        let engine = Self::new(config)?;
        for replica in &engine.spaces.replica {
            replica.insert(REPLICA_KEY, [0u8; 24])?;
        }
        engine.db.persist(PersistMode::SyncAll)?;
        Ok(engine)
    }

    fn open(config: &Config) -> Result<Self> {
        Self::new(config)
    }

    fn connect(&self) -> Result<FjallConn> {
        Ok(FjallConn {
            db: self.db.clone(),
            spaces: self.spaces.clone(),
            durable: self.durable,
            retries: 0,
        })
    }
}

impl FjallConn {
    /// One optimistic transaction, rerun from the start when it conflicts.
    fn write<T>(
        &mut self,
        body: impl Fn(&Spaces, &mut OptimisticWriteTx) -> Result<T>,
    ) -> Result<T> {
        loop {
            let mut tx = self.db.write_tx()?;
            if self.durable {
                tx = tx.durability(Some(PersistMode::SyncAll));
            }
            let value = body(&self.spaces, &mut tx)?;
            if tx.commit()?.is_ok() {
                return Ok(value);
            }
            self.retries += 1;
        }
    }

    /// Scan index keys in `low..=high`, fetch each row by primary key, decode `(seq, c)`.
    fn through_index(
        &mut self,
        table: u64,
        index: u8,
        low: i64,
        high: i64,
        limit: usize,
    ) -> Result<usize> {
        let place = self.spaces.place(table)?;
        let snapshot = self.db.read_tx();
        let first = place.index_prefix(index, low);
        let last = place.index_key(index, high, &[0xFF; PK]);
        let mut count = 0;
        for entry in snapshot.range(place.index(index), first..=last).take(limit) {
            let key = entry.key()?;
            let pk = key
                .get(key.len().saturating_sub(PK)..)
                .ok_or("short index key")?;
            let row = snapshot
                .get(place.rows, place.row_key(pk))?
                .ok_or("index entry without row")?;
            let _: (i64, String) = (int_at(&row, 16)?, text(&row)?);
            count += 1;
        }
        Ok(count)
    }
}

impl Conn for FjallConn {
    fn load(&mut self, table: u64, rows: &[Row]) -> Result<()> {
        self.write(|spaces, tx| {
            let place = spaces.place(table)?;
            rows.iter()
                .try_for_each(|row| place.put(tx, row, row.seq, true))
        })
    }

    fn insert(&mut self, table: u64, row: &Row) -> Result<()> {
        self.write(|spaces, tx| spaces.place(table)?.put(tx, row, row.seq, true))
    }

    fn update(&mut self, table: u64, seq: i64, a: i64, c: &str) -> Result<bool> {
        self.write(|spaces, tx| {
            let place = spaces.place(table)?;
            let pk = pk(seq);
            let key = place.row_key(&pk);
            let Some(old) = tx.get(place.rows, &key)? else {
                return Ok(false);
            };
            let (old_a, b) = (int_at(&old, A_AT)?, int_at(&old, B_AT)?);
            tx.insert(
                place.rows,
                key,
                encode_row(seq, a, b, c, int_at(&old, 40)?, int_at(&old, 64)?),
            );
            place.reindex(tx, INDEX_A, &pk, Some(old_a), a);
            Ok(true)
        })
    }

    fn point(&mut self, table: u64, seq: i64) -> Result<Option<(i64, i64, String)>> {
        let place = self.spaces.place(table)?;
        let Some(row) = place.rows.get(place.row_key(&pk(seq)))? else {
            return Ok(None);
        };
        Ok(Some((
            int_at(&row, A_AT)?,
            int_at(&row, B_AT)?,
            text(&row)?,
        )))
    }

    fn by_a(&mut self, table: u64, a: i64) -> Result<usize> {
        self.through_index(table, INDEX_A, a, a, usize::MAX)
    }

    fn range_b(&mut self, table: u64, low: i64, high: i64, limit: usize) -> Result<usize> {
        self.through_index(table, INDEX_B, low, high, limit)
    }

    fn mutation(
        &mut self,
        domain: u64,
        table: u64,
        op: i64,
        bytes: &[u8],
        row: &Row,
    ) -> Result<()> {
        self.write(|spaces, tx| {
            spaces.place(table)?.put(tx, row, op, false)?;
            let domain = domain as usize;
            let key = pk(op);
            let mut operation = Vec::with_capacity(24 + bytes.len());
            for field in [row.lamport, 0, 0] {
                operation.extend_from_slice(&field.to_be_bytes());
            }
            operation.extend_from_slice(bytes);
            tx.insert(
                spaces.ops.get(domain).ok_or("unknown domain")?,
                key,
                operation,
            );
            tx.insert(spaces.outbox.get(domain).ok_or("unknown domain")?, key, []);
            let replica = spaces.replica.get(domain).ok_or("unknown domain")?;
            let revision = match tx.get(replica, REPLICA_KEY)? {
                Some(old) => int_at(&old, 16)?,
                None => return Err("missing replica row".into()),
            };
            let mut next = [0u8; 24];
            next[..8].copy_from_slice(&op.to_be_bytes());
            next[8..16].copy_from_slice(&row.lamport.to_be_bytes());
            next[16..].copy_from_slice(&(revision + 1).to_be_bytes());
            tx.insert(replica, REPLICA_KEY, next);
            Ok(())
        })
    }

    /// The journal is only truncated once the memtables it covers are flushed.
    fn checkpoint(&mut self) -> Result<()> {
        flush(&self.db, &self.spaces)
    }

    fn retries(&self) -> u64 {
        self.retries
    }
}

fn main() -> Result<()> {
    harness::run::<Fjall>()
}
