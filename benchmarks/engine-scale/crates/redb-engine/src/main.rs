//! redb: one file, copy-on-write B-trees, tables and indexes built by hand.
//! `KV_LAYOUT=prefixed` keeps all rows in one tree and all index entries in another.
use std::sync::Arc;

use harness::{Config, Conn, DOMAINS, Engine, NODE, Result, Row};
use redb::{
    Database, Durability, ReadableDatabase, ReadableTable, Table, TableDefinition, WriteTransaction,
};

type Def<'a> = TableDefinition<'a, &'static [u8], &'static [u8]>;
type Tree<'txn> = Table<'txn, &'static [u8], &'static [u8]>;

const SIGN: u64 = 1 << 63;
const PK: usize = 24;
const INDEX_A: u8 = 0;
const INDEX_B: u8 = 1;
/// Row value: node, seq, a, b, lamport, stamp_node, stamp_seq, c length, c.
const A_AT: usize = 24;
const B_AT: usize = 32;
const C_AT: usize = 76;
const REPLICA_KEY: [u8; 8] = [0; 8];

fn prefixed() -> bool {
    std::env::var("KV_LAYOUT").is_ok_and(|layout| layout == "prefixed")
}
fn cache_mib() -> usize {
    std::env::var("REDB_CACHE_MIB")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(512)
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

/// Where a data table lives: tree names and the key prefixes inside them.
struct Place {
    rows: String,
    index_a: String,
    index_b: String,
    table: Option<u32>,
}
impl Place {
    fn new(table: u64, prefixed: bool) -> Self {
        if prefixed {
            Self {
                rows: "rows".into(),
                index_a: "idx".into(),
                index_b: "idx".into(),
                table: Some(table as u32),
            }
        } else {
            Self {
                rows: format!("t{table}"),
                index_a: format!("t{table}_a"),
                index_b: format!("t{table}_b"),
                table: None,
            }
        }
    }
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
}

/// The trees of one data table inside a write transaction.
struct Open<'txn> {
    place: Place,
    rows: Tree<'txn>,
    index_a: Tree<'txn>,
    /// `None` when both indexes share one tree.
    index_b: Option<Tree<'txn>>,
}
impl<'txn> Open<'txn> {
    fn new(tx: &'txn WriteTransaction, table: u64, prefixed: bool) -> Result<Self> {
        let place = Place::new(table, prefixed);
        let rows = tx.open_table(Def::new(&place.rows))?;
        let index_a = tx.open_table(Def::new(&place.index_a))?;
        let index_b = if prefixed {
            None
        } else {
            Some(tx.open_table(Def::new(&place.index_b))?)
        };
        Ok(Self {
            place,
            rows,
            index_a,
            index_b,
        })
    }
    fn index(&mut self, index: u8) -> &mut Tree<'txn> {
        match &mut self.index_b {
            Some(tree) if index == INDEX_B => tree,
            _ => &mut self.index_a,
        }
    }
    fn reindex(&mut self, index: u8, pk: &[u8], old: Option<i64>, new: i64) -> Result<()> {
        if old == Some(new) {
            return Ok(());
        }
        if let Some(old) = old {
            let stale = self.place.index_key(index, old, pk);
            self.index(index).remove(stale.as_slice())?;
        }
        let key = self.place.index_key(index, new, pk);
        self.index(index).insert(key.as_slice(), [].as_slice())?;
        Ok(())
    }
    /// Write a row and its index entries; `unique` rejects an existing primary key.
    fn put(&mut self, row: &Row, stamp_seq: i64, unique: bool) -> Result<()> {
        let pk = pk(row.seq);
        let key = self.place.row_key(&pk);
        let value = encode_row(row.seq, row.a, row.b, &row.c, row.lamport, stamp_seq);
        let old = match self.rows.insert(key.as_slice(), value.as_slice())? {
            Some(old) => Some((int_at(old.value(), A_AT)?, int_at(old.value(), B_AT)?)),
            None => None,
        };
        if unique && old.is_some() {
            return Err(format!("duplicate primary key {}", row.seq).into());
        }
        self.reindex(INDEX_A, &pk, old.map(|old| old.0), row.a)?;
        self.reindex(INDEX_B, &pk, old.map(|old| old.1), row.b)
    }
}

struct Redb {
    db: Arc<Database>,
    durable: bool,
    prefixed: bool,
}

struct RedbConn {
    db: Arc<Database>,
    durability: Durability,
    prefixed: bool,
}

impl Redb {
    fn new(config: &Config, create: bool) -> Result<Self> {
        let path = config.dir.join("db.redb");
        let mut builder = Database::builder();
        builder.set_cache_size(cache_mib() << 20);
        let db = if create {
            builder.create(path)?
        } else {
            builder.open(path)?
        };
        Ok(Self {
            db: Arc::new(db),
            durable: config.durable,
            prefixed: prefixed(),
        })
    }
}

/// Commit an empty durable transaction, which also persists earlier non-durable commits.
fn sync(db: &Database) -> Result<()> {
    let mut tx = db.begin_write()?;
    tx.set_durability(Durability::Immediate)?;
    Ok(tx.commit()?)
}

impl Drop for Redb {
    fn drop(&mut self) {
        let _ = sync(&self.db);
    }
}

impl Engine for Redb {
    const NAME: &'static str = "redb";
    type Conn = RedbConn;

    fn version() -> String {
        let layout = if prefixed() { "prefixed" } else { "per-table" };
        format!("4.3.0 {layout} cache={}MiB", cache_mib())
    }

    fn install(config: &Config) -> Result<Self> {
        let engine = Self::new(config, true)?;
        let tx = engine.db.begin_write()?;
        if engine.prefixed {
            tx.open_table(Def::new("rows"))?;
            tx.open_table(Def::new("idx"))?;
        } else {
            for table in 0..config.tables {
                Open::new(&tx, table, false)?;
            }
        }
        for domain in 0..DOMAINS {
            tx.open_table(Def::new(&format!("ops_{domain}")))?;
            tx.open_table(Def::new(&format!("outbox_{domain}")))?;
            tx.open_table(Def::new(&format!("replica_{domain}")))?
                .insert(REPLICA_KEY.as_slice(), [0u8; 24].as_slice())?;
        }
        tx.commit()?;
        Ok(engine)
    }

    fn open(config: &Config) -> Result<Self> {
        Self::new(config, false)
    }

    fn connect(&self) -> Result<RedbConn> {
        let durability = if self.durable {
            Durability::Immediate
        } else {
            Durability::None
        };
        Ok(RedbConn {
            db: self.db.clone(),
            durability,
            prefixed: self.prefixed,
        })
    }
}

impl RedbConn {
    /// One write transaction; redb blocks here until the previous writer has finished.
    fn write<T>(&mut self, body: impl FnOnce(&WriteTransaction) -> Result<T>) -> Result<T> {
        let mut tx = self.db.begin_write()?;
        tx.set_durability(self.durability)?;
        let value = body(&tx)?;
        tx.commit()?;
        Ok(value)
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
        let place = Place::new(table, self.prefixed);
        let tx = self.db.begin_read()?;
        let name = if index == INDEX_A {
            &place.index_a
        } else {
            &place.index_b
        };
        let entries = tx.open_table(Def::new(name))?;
        let rows = tx.open_table(Def::new(&place.rows))?;
        let first = place.index_prefix(index, low);
        let last = place.index_key(index, high, &[0xFF; PK]);
        let mut count = 0;
        for entry in entries
            .range::<&[u8]>(first.as_slice()..=last.as_slice())?
            .take(limit)
        {
            let (key, _) = entry?;
            let key = key.value();
            let pk = key
                .get(key.len().saturating_sub(PK)..)
                .ok_or("short index key")?;
            let row = rows
                .get(place.row_key(pk).as_slice())?
                .ok_or("index entry without row")?;
            let _: (i64, String) = (int_at(row.value(), 16)?, text(row.value())?);
            count += 1;
        }
        Ok(count)
    }
}

impl Conn for RedbConn {
    fn load(&mut self, table: u64, rows: &[Row]) -> Result<()> {
        let prefixed = self.prefixed;
        self.write(|tx| {
            let mut open = Open::new(tx, table, prefixed)?;
            rows.iter().try_for_each(|row| open.put(row, row.seq, true))
        })
    }

    fn insert(&mut self, table: u64, row: &Row) -> Result<()> {
        let prefixed = self.prefixed;
        self.write(|tx| Open::new(tx, table, prefixed)?.put(row, row.seq, true))
    }

    fn update(&mut self, table: u64, seq: i64, a: i64, c: &str) -> Result<bool> {
        let prefixed = self.prefixed;
        self.write(|tx| {
            let mut open = Open::new(tx, table, prefixed)?;
            let pk = pk(seq);
            let key = open.place.row_key(&pk);
            let old = match open.rows.get(key.as_slice())? {
                Some(old) => old.value().to_vec(),
                None => return Ok(false),
            };
            let (old_a, b) = (int_at(&old, A_AT)?, int_at(&old, B_AT)?);
            let value = encode_row(seq, a, b, c, int_at(&old, 40)?, int_at(&old, 64)?);
            open.rows.insert(key.as_slice(), value.as_slice())?;
            open.reindex(INDEX_A, &pk, Some(old_a), a)?;
            Ok(true)
        })
    }

    fn point(&mut self, table: u64, seq: i64) -> Result<Option<(i64, i64, String)>> {
        let place = Place::new(table, self.prefixed);
        let tx = self.db.begin_read()?;
        let rows = tx.open_table(Def::new(&place.rows))?;
        let Some(row) = rows.get(place.row_key(&pk(seq)).as_slice())? else {
            return Ok(None);
        };
        let row = row.value();
        Ok(Some((int_at(row, A_AT)?, int_at(row, B_AT)?, text(row)?)))
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
        let prefixed = self.prefixed;
        self.write(|tx| {
            Open::new(tx, table, prefixed)?.put(row, op, false)?;
            let key = pk(op);
            let mut operation = Vec::with_capacity(24 + bytes.len());
            for field in [row.lamport, 0, 0] {
                operation.extend_from_slice(&field.to_be_bytes());
            }
            operation.extend_from_slice(bytes);
            tx.open_table(Def::new(&format!("ops_{domain}")))?
                .insert(key.as_slice(), operation.as_slice())?;
            tx.open_table(Def::new(&format!("outbox_{domain}")))?
                .insert(key.as_slice(), [].as_slice())?;
            let mut replica = tx.open_table(Def::new(&format!("replica_{domain}")))?;
            let revision = match replica.get(REPLICA_KEY.as_slice())? {
                Some(old) => int_at(old.value(), 16)?,
                None => return Err("missing replica row".into()),
            };
            let mut next = [0u8; 24];
            next[..8].copy_from_slice(&op.to_be_bytes());
            next[8..16].copy_from_slice(&row.lamport.to_be_bytes());
            next[16..].copy_from_slice(&(revision + 1).to_be_bytes());
            replica.insert(REPLICA_KEY.as_slice(), next.as_slice())?;
            Ok(())
        })
    }

    /// redb has no journal; a durable commit is the only step that reaches stable storage.
    fn checkpoint(&mut self) -> Result<()> {
        sync(&self.db)
    }

    fn retries(&self) -> u64 {
        0
    }
}

fn main() -> Result<()> {
    harness::run::<Redb>()
}
