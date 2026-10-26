//! Parsed statements by SQL text: a statement that several checks read, or
//! that runs again, is parsed once.
use sqlparser::{ast::Statement, dialect::SQLiteDialect, parser::Parser};
use std::{
    borrow::Borrow,
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex},
};

/// Entries per generation. Two generations are kept, so an entry used
/// since the last turnover stays while a stream of distinct ones passes.
const GENERATION: usize = 256;
/// Longer texts, such as bulk inserts, are parsed each time instead of
/// displacing the statements an application repeats.
pub(crate) const CACHED_BYTES: usize = 16 * 1024;

/// A bounded map of recently used entries.
pub(crate) struct Recent<K, V>(Mutex<Option<[HashMap<K, V>; 2]>>);
impl<K: Eq + Hash, V: Clone> Recent<K, V> {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }
    pub(crate) fn get<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let [current, previous] = guard.as_mut()?;
        if let Some(hit) = current.get(key) {
            return Some(hit.clone());
        }
        let (key, hit) = previous.remove_entry(key)?;
        Self::insert(guard.as_mut()?, key, hit.clone());
        Some(hit)
    }
    pub(crate) fn put(&self, key: K, value: V) {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::insert(guard.get_or_insert_with(Default::default), key, value);
    }
    fn insert([current, previous]: &mut [HashMap<K, V>; 2], key: K, value: V) {
        if current.len() >= GENERATION {
            *previous = std::mem::take(current);
        }
        current.insert(key, value);
    }
}

type Parsed = Arc<[Statement]>;
static PARSED: Recent<Arc<str>, Parsed> = Recent::new();

/// The statements of `sql`, parsed in the SQLite dialect.
pub fn parse(sql: &str) -> Result<Parsed, String> {
    let cached = sql.len() <= CACHED_BYTES;
    if cached && let Some(hit) = PARSED.get(sql) {
        return Ok(hit);
    }
    #[cfg(any(test, feature = "test-support"))]
    PARSES.with(|parses| parses.set(parses.get() + 1));
    let parsed: Parsed = Parser::parse_sql(&SQLiteDialect {}, sql)
        .map_err(|error| error.to_string())?
        .into();
    if cached {
        PARSED.put(sql.into(), parsed.clone());
    }
    Ok(parsed)
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static PARSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
/// Texts the parser read on this thread, for tests that count parses.
#[cfg(any(test, feature = "test-support"))]
pub fn parses() -> u64 {
    PARSES.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_is_parsed_once_while_it_stays_in_use() {
        let before = parses();
        let sql = "SELECT 1 AS parsed_once_marker";
        let first = parse(sql).unwrap();
        for _ in 0..3 {
            assert!(Arc::ptr_eq(&first, &parse(sql).unwrap()));
        }
        assert_eq!(parses() - before, 1);
        // A stream of distinct texts keeps a statement that is still used.
        for index in 0..GENERATION * 3 {
            parse(&format!("SELECT {index} AS parsed_stream_marker")).unwrap();
            parse(sql).unwrap();
        }
        assert_eq!(parses() - before, 1 + 3 * GENERATION as u64);
        assert!(parse("SELECT FROM WHERE").is_err());
    }
}
