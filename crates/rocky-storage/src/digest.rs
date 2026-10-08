//! Order-independent digests of stored rows and operations, and the row
//! tracker that keeps them current. See `docs/runtime/checkpoints.md`.
use crate::sqlite::Value;
use rocky_core::Commitment;
use std::collections::HashMap;

/// The sum, modulo 2^256, of the hashes of a multiset of items, and its size.
/// Adding and removing items commute, so a digest can follow single changes
/// and two replicas holding the same items have the same digest. It detects
/// corruption and divergence; it is not collision resistant against a party
/// that chooses the items.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetDigest {
    pub sum: Commitment,
    pub count: u64,
}
impl SetDigest {
    pub fn add(&mut self, item: &Commitment) {
        let mut carry = 0u16;
        for (byte, other) in self.sum.iter_mut().zip(item) {
            let total = u16::from(*byte) + u16::from(*other) + carry;
            *byte = total as u8;
            carry = total >> 8;
        }
        self.count = self.count.wrapping_add(1);
    }
    pub fn remove(&mut self, item: &Commitment) {
        let mut borrow = 0i16;
        for (byte, other) in self.sum.iter_mut().zip(item) {
            let total = i16::from(*byte) - i16::from(*other) - borrow;
            *byte = total.rem_euclid(256) as u8;
            borrow = i16::from(total < 0);
        }
        self.count = self.count.wrapping_sub(1);
    }
    /// Adds a difference: `other` may hold removed items as wrapped values.
    pub fn merge(&mut self, other: &Self) {
        let count = self.count.wrapping_add(other.count);
        self.add(&other.sum);
        self.count = count;
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.sum.to_vec();
        bytes.extend(self.count.to_le_bytes());
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (sum, count) = bytes.split_at_checked(32)?;
        Some(Self {
            sum: sum.try_into().ok()?,
            count: u64::from_le_bytes(count.try_into().ok()?),
        })
    }
}

/// The hash of one stored row: its logical table and every column in order.
pub fn row_hash<'a>(table: &str, values: impl Iterator<Item = &'a Value>) -> Commitment {
    let mut bytes = (table.len() as u32).to_le_bytes().to_vec();
    bytes.extend(table.as_bytes());
    for value in values {
        match value {
            Value::Null => bytes.push(0),
            Value::Integer(value) => {
                bytes.push(1);
                bytes.extend(value.to_le_bytes());
            }
            // The engine stores an integral REAL as an integer, and its row
            // hook then reports the integer where a read reports the REAL.
            Value::Real(value) if value.fract() == 0.0 && value.abs() < -(i64::MIN as f64) => {
                bytes.push(1);
                bytes.extend((*value as i64).to_le_bytes());
            }
            Value::Real(value) => {
                bytes.push(2);
                bytes.extend(value.to_bits().to_le_bytes());
            }
            Value::Text(value) => {
                bytes.push(3);
                bytes.extend((value.len() as u64).to_le_bytes());
                bytes.extend(value.as_bytes());
            }
            Value::Blob(value) => {
                bytes.push(4);
                bytes.extend((value.len() as u64).to_le_bytes());
                bytes.extend(value);
            }
        }
    }
    rocky_core::commitment("rocky.checkpoint.row.v1", &bytes)
}
pub fn operation_hash(bytes: &[u8]) -> Commitment {
    rocky_core::commitment("rocky.checkpoint.operation.v1", bytes)
}

/// Digest changes of the tracked tables of one write connection, fed by the
/// engine's row hook. Each registration gets a slot that sums the rows
/// inserted into, minus the rows removed from, its tables.
#[derive(Default)]
pub struct Tracker {
    /// Physical table name to its slot and logical name.
    tables: HashMap<String, (usize, String)>,
    slots: Vec<SetDigest>,
    /// A changed row could not be read; the step must not commit.
    failed: bool,
}
impl Tracker {
    /// Tracks `tables`, given as (physical, logical) names, in a new slot.
    pub fn register(&mut self, tables: impl Iterator<Item = (String, String)>) -> usize {
        let slot = self.slots.len();
        self.slots.push(SetDigest::default());
        for (physical, logical) in tables {
            self.tables.insert(physical, (slot, logical));
        }
        slot
    }
    pub fn table(&self, physical: &str) -> Option<(usize, &str)> {
        self.tables
            .get(physical)
            .map(|(slot, logical)| (*slot, logical.as_str()))
    }
    pub fn record(&mut self, slot: usize, removed: Option<Commitment>, added: Option<Commitment>) {
        let Some(digest) = self.slots.get_mut(slot) else {
            return;
        };
        if let Some(row) = removed {
            digest.remove(&row);
        }
        if let Some(row) = added {
            digest.add(&row);
        }
    }
    pub fn fail(&mut self) {
        self.failed = true;
    }
    /// The change since the last `take` of `slot`, or None after a failed read.
    pub fn take(&mut self, slot: usize) -> Option<SetDigest> {
        let failed = std::mem::take(&mut self.failed);
        let delta = self.slots.get_mut(slot).map(std::mem::take);
        if failed { None } else { delta }
    }
    pub fn peek(&self, slot: usize) -> SetDigest {
        self.slots.get(slot).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(conn: &crate::sqlite::Connection, table: &str) -> SetDigest {
        let mut digest = SetDigest::default();
        conn.scan(&format!("SELECT * FROM {table}"), Vec::new(), |row| {
            digest.add(&row_hash(table, row.iter()));
            Ok(())
        })
        .unwrap();
        digest
    }

    #[test]
    fn digests_commute_and_cancel() {
        let (a, b) = ([0xff; 32], [7; 32]);
        let mut one = SetDigest::default();
        one.add(&a);
        one.add(&b);
        let mut other = SetDigest::default();
        other.add(&b);
        other.add(&a);
        assert_eq!(one, other);
        assert_eq!(SetDigest::decode(&one.encode()), Some(one));
        let mut delta = SetDigest::default();
        delta.remove(&a);
        one.merge(&delta);
        one.remove(&b);
        assert!(one.is_zero());
    }

    const TRACKED: [&str; 4] = ["plain", "keyed", "loose", "typed"];
    /// The tracked digest must equal a scan after every kind of row change
    /// the engine supports, or a checkpoint would not verify.
    #[tokio::test]
    async fn the_row_hook_follows_every_kind_of_change() {
        let conn = crate::sqlite::Connection::open_in_memory().unwrap();
        for sql in [
            "CREATE TABLE plain(a INTEGER PRIMARY KEY,b TEXT,c BLOB)",
            "CREATE TABLE keyed(a BLOB,b INTEGER,c TEXT,PRIMARY KEY(a,b)) WITHOUT ROWID",
            "CREATE TABLE loose(a,b)",
            "CREATE TABLE other(a)",
            "CREATE TABLE typed(a TEXT PRIMARY KEY COLLATE NOCASE,r REAL,i INTEGER)",
        ] {
            conn.execute(sql, ()).await.unwrap();
        }
        let slot = conn.tracker().unwrap().lock().unwrap().register(
            TRACKED
                .into_iter()
                .map(|table| (table.to_string(), table.to_string())),
        );
        let mut tracked = SetDigest::default();
        for sql in [
            "INSERT INTO plain VALUES(1,'one',x'01'),(2,NULL,NULL),(7,'seven',x'')",
            "INSERT INTO keyed VALUES(x'aa',1,'k'),(x'aa',2,NULL)",
            "INSERT INTO loose VALUES(1,1),(1,1),(2.5,'x')",
            "INSERT INTO other VALUES(1)",
            // An integral REAL is stored, and reported by the hook, as an integer.
            "INSERT INTO typed VALUES('a',2.0,2.0),('b',0.5,1),('c',1e300,NULL),('d',9e999,-1)",
            "INSERT INTO typed VALUES('A',-3.0,7) ON CONFLICT(a) DO UPDATE SET a=excluded.a,r=excluded.r",
            "UPDATE typed SET r=4 WHERE a='b'",
            "DELETE FROM typed WHERE a='A'",
            "UPDATE plain SET b='uno' WHERE a=1",
            "UPDATE plain SET a=3 WHERE a=2",
            "INSERT INTO plain VALUES(1,'eins',NULL) ON CONFLICT(a) DO UPDATE SET b=excluded.b",
            "INSERT INTO plain VALUES(1,'x',NULL) ON CONFLICT(a) DO NOTHING",
            "INSERT OR REPLACE INTO plain VALUES(7,'replaced',NULL)",
            "INSERT OR REPLACE INTO keyed VALUES(x'aa',1,'replaced')",
            "UPDATE keyed SET c='c' WHERE b=2",
            "DELETE FROM keyed WHERE b=1",
            "DELETE FROM loose WHERE a=1",
            "INSERT INTO loose SELECT a,b FROM plain",
            "DELETE FROM loose",
            "DELETE FROM plain",
        ] {
            conn.execute(sql, ()).await.unwrap();
            let delta = conn.tracker().unwrap().lock().unwrap().take(slot).unwrap();
            tracked.merge(&delta);
            let mut expected = SetDigest::default();
            for table in TRACKED {
                expected.merge(&scan(&conn, table));
            }
            assert_eq!(tracked, expected, "{sql}");
        }
        // A rolled back change is reported; the caller discards it.
        conn.execute("BEGIN", ()).await.unwrap();
        conn.execute("INSERT INTO plain VALUES(9,'gone',NULL)", ())
            .await
            .unwrap();
        conn.execute("ROLLBACK", ()).await.unwrap();
        assert!(!conn.tracker().unwrap().lock().unwrap().peek(slot).is_zero());
    }
}
