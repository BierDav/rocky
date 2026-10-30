//! Deterministic SQL of replayed tables: the function allowlist, the
//! functions a table's expressions call and the tables an assertion reads.
//! These are declaration checks; `rocky-storage` parses each assertion
//! again before it runs and refuses what these scans miss.
use crate::logical::{Token, keyword, tokenize};

/// SQLite functions whose result depends only on their arguments and the
/// engine build. Part of the engine identity a tip records.
pub const DETERMINISTIC_FUNCTIONS: &[&str] = &[
    // Scalar.
    "abs",
    "char",
    "coalesce",
    "concat",
    "concat_ws",
    "format",
    "glob",
    "hex",
    "ifnull",
    "iif",
    "instr",
    "length",
    "like",
    "likelihood",
    "likely",
    "lower",
    "ltrim",
    "max",
    "min",
    "nullif",
    "octet_length",
    "printf",
    "quote",
    "replace",
    "round",
    "rtrim",
    "sign",
    "substr",
    "substring",
    "trim",
    "typeof",
    "unhex",
    "unicode",
    "unlikely",
    "upper",
    "zeroblob",
    // Exact math; transcendental functions use the platform's libm.
    "ceil",
    "ceiling",
    "floor",
    "mod",
    "trunc",
    // Aggregate.
    "avg",
    "count",
    "group_concat",
    "string_agg",
    "sum",
    "total",
    // Window.
    "cume_dist",
    "dense_rank",
    "first_value",
    "lag",
    "last_value",
    "lead",
    "nth_value",
    "ntile",
    "percent_rank",
    "rank",
    "row_number",
    // JSON.
    "json",
    "json_array",
    "json_array_length",
    "json_error_position",
    "json_extract",
    "json_group_array",
    "json_group_object",
    "json_insert",
    "json_object",
    "json_patch",
    "json_pretty",
    "json_quote",
    "json_remove",
    "json_replace",
    "json_set",
    "json_type",
    "json_valid",
    "jsonb",
    "jsonb_array",
    "jsonb_extract",
    "jsonb_group_array",
    "jsonb_group_object",
    "jsonb_insert",
    "jsonb_object",
    "jsonb_patch",
    "jsonb_remove",
    "jsonb_replace",
    "jsonb_set",
];

/// Keywords SQLite accepts directly before `(` that are not calls.
const KEYWORDS: &[&str] = &[
    "all",
    "and",
    "as",
    "between",
    "by",
    "case",
    "cast",
    "check",
    "distinct",
    "else",
    "escape",
    "except",
    "exists",
    "filter",
    "from",
    "having",
    "in",
    "intersect",
    "is",
    "join",
    "limit",
    "not",
    "offset",
    "on",
    "or",
    "over",
    "select",
    "then",
    "union",
    "using",
    "values",
    "when",
    "where",
];
/// Keywords that read the clock.
const CLOCK_KEYWORDS: &[&str] = &["current_date", "current_time", "current_timestamp"];

/// The first function `tokens` call outside the allowlist, or a clock
/// keyword they name.
fn refused_call(tokens: &[Token]) -> Option<String> {
    for (index, token) in tokens.iter().enumerate() {
        let name = match token {
            Token::Word(word) if CLOCK_KEYWORDS.contains(&word.to_ascii_lowercase().as_str()) => {
                return Some(word.to_ascii_lowercase());
            }
            Token::Word(name) | Token::Quoted(name) => name.to_ascii_lowercase(),
            _ => continue,
        };
        let called = tokens.get(index + 1) == Some(&Token::Symbol('('));
        // `CAST(x AS DECIMAL(10,2))` names a type, not a function.
        let typed = index > 0 && keyword(&tokens[index - 1], "AS");
        let word = matches!(token, Token::Word(_));
        if !called || typed || (word && KEYWORDS.contains(&name.as_str())) {
            continue;
        }
        if !DETERMINISTIC_FUNCTIONS.contains(&name.as_str()) {
            return Some(name);
        }
    }
    None
}

/// The parenthesized body that opens at `tokens[open]`, without its parens.
fn body(tokens: &[Token], open: usize) -> &[Token] {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token {
            Token::Symbol('(') => depth += 1,
            Token::Symbol(')') => {
                depth -= 1;
                if depth == 0 {
                    return &tokens[open + 1..index];
                }
            }
            _ => {}
        }
    }
    &tokens[open + 1..]
}

/// The first function outside the allowlist that a `CREATE TABLE`'s
/// `CHECK`s, generated columns or expression defaults call, or that a
/// `CREATE INDEX`'s terms and `WHERE` call: SQLite evaluates them on every
/// replica.
pub(crate) fn refused_function(statement: &str) -> Option<String> {
    let Some(tokens) = tokenize(statement) else {
        return Some("unterminated quote".into());
    };
    if tokens.iter().any(|token| keyword(token, "INDEX")) {
        // Everything after the indexed table's name.
        let open = tokens
            .iter()
            .position(|token| *token == Token::Symbol('('))?;
        return refused_call(&tokens[open..]);
    }
    tokens.windows(2).enumerate().find_map(|(index, pair)| {
        let opens = ["CHECK", "AS", "DEFAULT"]
            .iter()
            .any(|word| keyword(&pair[0], word));
        if opens && pair[1] == Token::Symbol('(') {
            return refused_call(body(&tokens, index + 1));
        }
        // A bare `DEFAULT CURRENT_TIMESTAMP`.
        refused_call(std::slice::from_ref(&pair[1]))
            .filter(|name| CLOCK_KEYWORDS.contains(&name.as_str()))
    })
}

/// Why an assertion is not one plain `SELECT` over named tables.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AssertionShape {
    /// The tables it reads, lowercase, in order of appearance.
    Reads(Vec<String>),
    /// A function outside the allowlist, or a clock keyword.
    Function(String),
    Refused(String),
}

/// The tables an assertion's `SELECT` reads. Refuses `WITH`, qualified
/// names and table-valued functions rather than guessing at them.
pub(crate) fn assertion_tables(select_sql: &str) -> AssertionShape {
    let refused = |reason: &str| AssertionShape::Refused(reason.into());
    let Some(tokens) = tokenize(select_sql) else {
        return refused("unterminated quote");
    };
    if !tokens.first().is_some_and(|token| keyword(token, "SELECT")) {
        return refused("must be one SELECT");
    }
    // Depths whose FROM list is open; whether a table source comes next.
    let (mut depth, mut lists, mut expect) = (0usize, Vec::<usize>::new(), false);
    let mut tables = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let next = tokens.get(index + 1);
        match token {
            // A subquery or nested join: its own FROM lists name its tables.
            Token::Symbol('(') => {
                expect = false;
                depth += 1;
            }
            Token::Symbol(')') => {
                lists.retain(|open| *open < depth);
                let Some(outer) = depth.checked_sub(1) else {
                    return refused("unbalanced parentheses");
                };
                depth = outer;
            }
            Token::Symbol(';') => return refused("must be one SELECT"),
            Token::Symbol(',') if lists.last() == Some(&depth) => expect = true,
            Token::Word(word) if !expect || keyword(token, "WITH") => {
                let word = word.to_ascii_lowercase();
                match word.as_str() {
                    "with" => return refused("WITH is not supported"),
                    "from" => {
                        if lists.last() != Some(&depth) {
                            lists.push(depth);
                        }
                        expect = true;
                    }
                    "join" => expect = true,
                    "where" | "group" | "having" | "order" | "limit" | "window" | "union"
                    | "except" | "intersect" => lists.retain(|open| *open < depth),
                    _ => {}
                }
            }
            Token::Word(name) | Token::Quoted(name) if expect => {
                if next == Some(&Token::Symbol('.')) {
                    return refused("qualified table names are not supported");
                }
                if next == Some(&Token::Symbol('(')) {
                    return refused("table-valued functions are not supported");
                }
                tables.push(name.to_ascii_lowercase());
                expect = false;
            }
            _ if expect => return refused("unsupported table source"),
            _ => {}
        }
    }
    if depth != 0 || expect {
        return refused("unbalanced parentheses");
    }
    if let Some(function) = refused_call(&tokens) {
        return AssertionShape::Function(function);
    }
    AssertionShape::Reads(tables)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reads(sql: &str) -> Vec<String> {
        match assertion_tables(sql) {
            AssertionShape::Reads(tables) => tables,
            other => panic!("{sql}: {other:?}"),
        }
    }

    #[test]
    fn assertion_tables_follow_joins_commas_aliases_and_subqueries() {
        assert_eq!(
            reads("SELECT team FROM members GROUP BY team HAVING COUNT(*) > 10"),
            ["members"]
        );
        assert_eq!(
            reads(
                "SELECT h.id FROM headers AS h JOIN lines l ON l.header = h.id, \"Totals\" t \
                 WHERE h.total <> (SELECT SUM(amount) FROM lines x WHERE x.header = h.id)"
            ),
            ["headers", "lines", "totals", "lines"]
        );
        assert_eq!(
            reads("SELECT * FROM (SELECT a, b FROM one) s, two WHERE s.a IN (1, 2)"),
            ["one", "two"]
        );
        assert_eq!(
            reads("SELECT (SELECT COUNT(*) FROM b), c FROM a LEFT JOIN d USING (x)"),
            ["b", "a", "d"]
        );
    }

    #[test]
    fn unsupported_assertion_shapes_are_refused() {
        for (sql, reason) in [
            ("WITH t AS (SELECT 1) SELECT * FROM t", "must be one SELECT"),
            (
                "SELECT * FROM (WITH t AS (SELECT 1) SELECT * FROM t)",
                "WITH is not supported",
            ),
            (
                "SELECT * FROM main.items",
                "qualified table names are not supported",
            ),
            (
                "SELECT * FROM json_each('[1]')",
                "table-valued functions are not supported",
            ),
            ("SELECT * FROM items; SELECT 1", "must be one SELECT"),
            ("SELECT * FROM (SELECT 1", "unbalanced parentheses"),
            ("SELECT * FROM", "unbalanced parentheses"),
            ("DELETE FROM items", "must be one SELECT"),
        ] {
            assert_eq!(
                assertion_tables(sql),
                AssertionShape::Refused(reason.into()),
                "{sql}"
            );
        }
        for (sql, function) in [
            ("SELECT random() FROM items", "random"),
            (
                "SELECT * FROM items WHERE at < CURRENT_TIMESTAMP",
                "current_timestamp",
            ),
        ] {
            assert_eq!(
                assertion_tables(sql),
                AssertionShape::Function(function.into()),
                "{sql}"
            );
        }
    }

    #[test]
    fn table_expressions_call_only_allowlisted_functions() {
        for accepted in [
            "CREATE TABLE t (a INTEGER CHECK (a BETWEEN 0 AND 10), b TEXT, PRIMARY KEY(a), \
             CHECK (CASE WHEN a > 5 THEN length(b) > 0 ELSE b IS NULL END), \
             CHECK (b IN ('x', 'y') OR b LIKE 'z%' OR b GLOB 'q*'), \
             CHECK (coalesce(nullif(b, ''), 'none') <> 'bad'), \
             CHECK (CAST(a AS DECIMAL(10, 2)) >= 0 AND NOT (a < 0)), \
             CHECK (json_valid(b) OR json_extract(b, '$.k') IS NOT NULL), \
             FOREIGN KEY(a) REFERENCES p(id))",
            "CREATE TABLE t (a INTEGER, b INTEGER AS (a * 2), c TEXT DEFAULT ('x'))",
            "CREATE UNIQUE INDEX i ON t (lower(b)) WHERE a IS NOT NULL",
        ] {
            assert_eq!(refused_function(accepted), None, "{accepted}");
        }
        for (refused, function) in [
            ("CREATE TABLE t (a INTEGER CHECK (a < random()))", "random"),
            ("CREATE TABLE t (a TEXT, CHECK (a REGEXP ('x')))", "regexp"),
            (
                "CREATE TABLE t (a TEXT, CHECK (\"sqlite_version\"() <> a))",
                "sqlite_version",
            ),
            ("CREATE TABLE t (a REAL, CHECK (sin(a) < 1))", "sin"),
            (
                "CREATE TABLE t (a TEXT DEFAULT CURRENT_TIMESTAMP)",
                "current_timestamp",
            ),
            ("CREATE TABLE t (a TEXT DEFAULT (date('now')))", "date"),
            (
                "CREATE TABLE t (a INTEGER, b INTEGER AS (a + changes()))",
                "changes",
            ),
            ("CREATE INDEX i ON t (a) WHERE a > unixepoch()", "unixepoch"),
        ] {
            assert_eq!(
                refused_function(refused).as_deref(),
                Some(function),
                "{refused}"
            );
        }
    }
}
