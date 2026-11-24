use sqlparser::{ast::Statement, dialect::SQLiteDialect, parser::Parser};
use std::{
    borrow::Borrow,
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex},
};

const GENERATION: usize = 256;
pub(crate) const CACHED_BYTES: usize = 16 * 1024;

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

pub fn parse(sql: &str) -> Result<Parsed, String> {
    parse_through(&PARSED, sql)
}

pub fn normalized(sql: &str) -> std::borrow::Cow<'_, str> {
    use sqlparser::{
        keywords::Keyword,
        tokenizer::{Token, Tokenizer},
    };
    let Ok(tokens) = Tokenizer::new(&SQLiteDialect {}, sql).tokenize() else {
        return std::borrow::Cow::Borrowed(sql);
    };
    let significant =
        |from: usize| (from..tokens.len()).find(|&at| !matches!(tokens[at], Token::Whitespace(_)));
    let keyword = |at: Option<usize>| match at.map(|at| &tokens[at]) {
        Some(Token::Word(word)) => Some(word.keyword),
        _ => None,
    };
    let tests_a_truth_value = |at: Option<usize>| {
        matches!(
            keyword(at),
            Some(
                Keyword::NULL
                    | Keyword::TRUE
                    | Keyword::FALSE
                    | Keyword::UNKNOWN
                    | Keyword::DISTINCT
                    | Keyword::JSON
                    | Keyword::NORMALIZED
            )
        )
    };
    let mut rewrites = Vec::new();
    for at in 0..tokens.len() {
        if keyword(Some(at)) != Some(Keyword::IS) {
            continue;
        }
        let next = significant(at + 1);
        if keyword(next) == Some(Keyword::NOT) {
            let Some(not) = next else { continue };
            if !tests_a_truth_value(significant(not + 1)) {
                rewrites.push((at, not, "IS DISTINCT FROM"));
            }
        } else if !tests_a_truth_value(next) {
            rewrites.push((at, at, "IS NOT DISTINCT FROM"));
        }
    }
    if rewrites.is_empty() {
        return std::borrow::Cow::Borrowed(sql);
    }
    let mut rewritten = String::with_capacity(sql.len() + 16 * rewrites.len());
    let mut at = 0;
    for (start, end, replacement) in rewrites {
        tokens[at..start]
            .iter()
            .for_each(|token| rewritten.push_str(&token.to_string()));
        rewritten.push_str(replacement);
        at = end + 1;
    }
    tokens[at..]
        .iter()
        .for_each(|token| rewritten.push_str(&token.to_string()));
    std::borrow::Cow::Owned(rewritten)
}

fn parse_through(cache: &Recent<Arc<str>, Parsed>, sql: &str) -> Result<Parsed, String> {
    let cached = sql.len() <= CACHED_BYTES;
    if cached && let Some(hit) = cache.get(sql) {
        return Ok(hit);
    }
    #[cfg(any(test, feature = "test-support"))]
    PARSES.with(|parses| parses.set(parses.get() + 1));
    let parsed: Parsed = Parser::parse_sql(&SQLiteDialect {}, sql)
        .map_err(|error| error.to_string())?
        .into();
    if cached {
        cache.put(sql.into(), parsed.clone());
    }
    Ok(parsed)
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static PARSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
#[cfg(any(test, feature = "test-support"))]
pub fn parses() -> u64 {
    PARSES.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn sqlite_is_comparisons_become_distinct_from_and_parse() {
        for (sql, expected) in [
            (
                "SELECT 1 WHERE a IS ?",
                "SELECT 1 WHERE a IS NOT DISTINCT FROM ?",
            ),
            (
                "SELECT 1 WHERE a IS NOT lower(?)",
                "SELECT 1 WHERE a IS DISTINCT FROM lower(?)",
            ),
            (
                "SELECT 'a IS b' WHERE a is 7",
                "SELECT 'a IS b' WHERE a IS NOT DISTINCT FROM 7",
            ),
        ] {
            assert_eq!(normalized(sql), expected);
            assert!(parse(&normalized(sql)).is_ok(), "{sql}");
        }
        for sql in [
            "SELECT 1 WHERE a IS NULL",
            "SELECT 1 WHERE a IS NOT NULL",
            "SELECT 1 WHERE a IS TRUE",
            "SELECT 1 WHERE a IS NOT DISTINCT FROM ?",
            "SELECT 1 WHERE a IS DISTINCT FROM ?",
        ] {
            assert!(
                matches!(normalized(sql), std::borrow::Cow::Borrowed(_)),
                "{sql}"
            );
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_text_is_parsed_once_while_it_stays_in_use() {
        let cache = Recent::new();
        let before = parses();
        let sql = "SELECT 1 AS parsed_once";
        let first = parse_through(&cache, sql).unwrap();
        for _ in 0..3 {
            assert!(Arc::ptr_eq(&first, &parse_through(&cache, sql).unwrap()));
        }
        assert_eq!(parses() - before, 1);
        for index in 0..GENERATION * 3 {
            parse_through(&cache, &format!("SELECT {index} AS streamed")).unwrap();
            parse_through(&cache, sql).unwrap();
        }
        assert_eq!(
            parses() - before,
            1 + 3 * GENERATION as u64,
            "a stream of distinct texts keeps a statement that is still used"
        );
        assert!(parse_through(&cache, "SELECT FROM WHERE").is_err());
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn texts_longer_than_the_cache_bound_are_never_cached_and_displace_nothing() {
        let cache = Recent::new();
        let kept = "SELECT 1 AS kept";
        let first = parse_through(&cache, kept).unwrap();
        let padding = " ".repeat(CACHED_BYTES);
        let before = parses();
        for index in 0..2 * GENERATION {
            let oversized = format!("SELECT {index} AS bulk{padding}");
            parse_through(&cache, &oversized).unwrap();
            assert!(
                cache.get(oversized.as_str()).is_none(),
                "an oversized text is parsed each time, not cached"
            );
        }
        assert_eq!(parses() - before, 2 * GENERATION as u64);
        assert!(
            cache.get(kept).is_some_and(|hit| Arc::ptr_eq(&hit, &first)),
            "two generations of oversized texts leave the cached statement in place"
        );
    }
}
