//! Deterministic SQL of replayed tables: the function allowlist, the
//! functions a table's expressions call and the tables an assertion reads.
//! These are declaration checks; `rocky-storage` parses each assertion
//! again before it runs and refuses what these scans miss.
use crate::logical::{Token, keyword, tokenize};

/// SQLite functions whose result depends only on their arguments and the
/// engine build. Part of the engine identity a tip records. Functions that
/// turn INTEGER values into REAL ones (`avg`, `total`, `round`, `mod`) are
/// left out, so exact columns never pass through binary floating point.
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
    "trunc",
    // Arithmetic that fails on `i64` overflow; Rocky registers these.
    "checked_add",
    "checked_div",
    "checked_mul",
    "checked_sub",
    // Aggregate.
    "count",
    "group_concat",
    "string_agg",
    "sum",
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

/// The expressions of a `CREATE TABLE`'s `CHECK`s, generated columns and
/// expression defaults, or of a `CREATE INDEX`'s terms and `WHERE`: SQLite
/// evaluates them on every replica.
fn expressions(tokens: &[Token]) -> Vec<&[Token]> {
    if tokens.iter().any(|token| keyword(token, "INDEX")) {
        // Everything after the indexed table's name.
        return tokens
            .iter()
            .position(|token| *token == Token::Symbol('('))
            .map(|open| vec![&tokens[open..]])
            .unwrap_or_default();
    }
    tokens
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| {
            ["CHECK", "AS", "DEFAULT"]
                .iter()
                .any(|word| keyword(&pair[0], word))
                && pair[1] == Token::Symbol('(')
        })
        .map(|(index, _)| body(tokens, index + 1))
        .collect()
}

/// The first function outside the allowlist that a table's or index's
/// expressions call, or a bare `DEFAULT CURRENT_TIMESTAMP`.
pub(crate) fn refused_function(statement: &str) -> Option<String> {
    let Some(tokens) = tokenize(statement) else {
        return Some("unterminated quote".into());
    };
    expressions(&tokens)
        .into_iter()
        .find_map(refused_call)
        .or_else(|| {
            tokens.iter().find_map(|token| {
                refused_call(std::slice::from_ref(token))
                    .filter(|name| CLOCK_KEYWORDS.contains(&name.as_str()))
            })
        })
}

/// Keywords after which `-` or `+` signs a number.
const SIGN_KEYWORDS: &[&str] = &[
    "AND", "BETWEEN", "CASE", "ELSE", "ESCAPE", "GLOB", "IN", "IS", "LIKE", "NOT", "OR", "THEN",
    "WHEN", "WHERE",
];

/// The first `+`, `-`, `*` or `/` a table's or index's expressions apply.
/// SQLite evaluates them as written and turns an INTEGER result outside
/// `i64` into a REAL, so they must call the `checked_*` functions instead.
/// A signed number, `count(*)` and the `->` JSON operators are not refused.
pub(crate) fn refused_operator(statement: &str) -> Option<char> {
    let tokens = tokenize(statement)?;
    expressions(&tokens).into_iter().find_map(|tokens| {
        tokens.iter().enumerate().find_map(|(index, token)| {
            let Token::Symbol(operator @ ('+' | '-' | '*' | '/')) = *token else {
                return None;
            };
            let before = index.checked_sub(1).map(|before| &tokens[before]);
            let next = tokens.get(index + 1);
            let operand = before.is_some_and(|before| match before {
                Token::Literal | Token::Quoted(_) | Token::Symbol(')') => true,
                Token::Word(word) => !SIGN_KEYWORDS.contains(&word.to_ascii_uppercase().as_str()),
                Token::Symbol(_) => false,
            });
            let allowed = match operator {
                '-' if next == Some(&Token::Symbol('>')) => true,
                '+' | '-' => !operand && next == Some(&Token::Literal),
                '*' => before == Some(&Token::Symbol('(')) && next == Some(&Token::Symbol(')')),
                _ => false,
            };
            (!allowed).then_some(operator)
        })
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
    fn table_expressions_apply_only_checked_arithmetic() {
        for accepted in [
            "CREATE TABLE t (a INTEGER CHECK (a BETWEEN -5 AND +5), b TEXT DEFAULT (-1), \
             CHECK (a IN (-1, 1) OR a > -2 OR a % 2 = 0 OR CASE WHEN a THEN -1 ELSE -2 END < 0), \
             CHECK (checked_add(a, checked_mul(a, -3)) <= checked_div(a, 2)), \
             CHECK (b->>'$.k' IS NOT NULL OR b->'$' IS NULL), CHECK (b || '-' <> 'x-'))",
            "CREATE INDEX i ON t (checked_sub(0, a)) WHERE a > -1",
            "CREATE TABLE t (a INTEGER, b INTEGER DEFAULT -1, c INTEGER)",
        ] {
            assert_eq!(refused_operator(accepted), None, "{accepted}");
        }
        for (refused, operator) in [
            (
                "CREATE TABLE t (a INTEGER, b INTEGER, CHECK (a + b > 0))",
                '+',
            ),
            (
                "CREATE TABLE t (a INTEGER, b INTEGER, CHECK (a - 1 > b))",
                '-',
            ),
            ("CREATE TABLE t (a INTEGER, CHECK (a > -a))", '-'),
            ("CREATE TABLE t (a INTEGER, CHECK ((a) * 2 > 0))", '*'),
            ("CREATE TABLE t (a INTEGER, b INTEGER AS (a / 2))", '/'),
            ("CREATE TABLE t (a INTEGER, b INTEGER DEFAULT (1 + 1))", '+'),
            ("CREATE INDEX i ON t (a * 2)", '*'),
            ("CREATE INDEX i ON t (a) WHERE a - 1 > 0", '-'),
        ] {
            assert_eq!(refused_operator(refused), Some(operator), "{refused}");
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
            "CREATE TABLE t (a INTEGER, b INTEGER AS (checked_mul(a, 2)), c TEXT DEFAULT ('x'))",
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
            ("CREATE TABLE t (a INTEGER, CHECK (round(a) > 0))", "round"),
            ("CREATE TABLE t (a INTEGER, CHECK (mod(a, 2) = 0))", "mod"),
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
