//! Application statements as a SQLite-dialect syntax tree: the supported
//! surface is checked and logical table names are resolved to physical ones
//! by position, never by textual replacement. The engine's authorizer still
//! decides every table access. See `docs/runtime/queries-and-observation.md`.
use sqlparser::ast::{
    Expr, FromTable, ObjectName, ObjectNamePart, Query, SelectItem,
    SelectItemQualifiedWildcardKind, Statement, TableFactor, TableObject, Visit, Visitor,
};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Span};
use std::collections::BTreeSet;
use std::ops::ControlFlow;

/// What a statement does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Read,
    Insert,
    Update,
    Delete,
}
/// A statement inside the supported surface, with physical table names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analyzed {
    pub sql: String,
    pub kind: Kind,
    /// Logical tables the statement names, lowercase and without common
    /// table expressions. Unknown names are included as written.
    pub tables: BTreeSet<String>,
}
/// Why a statement never reaches the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// It reaches beyond what a session may name: schema changes, pragmas,
    /// attachments, protocol or engine objects.
    Denied(String),
    /// It is outside the supported surface or not parsable as SQLite.
    Unsupported(String),
}
impl From<Rejection> for crate::sqlite::Error {
    fn from(rejection: Rejection) -> Self {
        match rejection {
            Rejection::Denied(_) => Self::Denied,
            Rejection::Unsupported(reason) => Self::Unsupported(reason),
        }
    }
}

/// Table-valued functions a statement may read from.
const TABLE_FUNCTIONS: &[&str] = &["json_each", "json_tree"];
/// Functions that reach the file, the engine or its extensions.
const DENIED_FUNCTIONS: &[&str] = &[
    "load_extension",
    "readfile",
    "writefile",
    "edit",
    "fts3_tokenizer",
    "zipfile",
];
/// Prefixes of names that are never an application's.
const RESERVED: &[&str] = &["sqlite_", "rocky_", "secure_", "pragma_"];
/// Longest statement text analyzed.
pub const MAX_STATEMENT_BYTES: usize = 1024 * 1024;

/// Leading keywords of statements that change the schema, the engine or
/// its attachments.
const FORBIDDEN: &[&str] = &["PRAGMA", "ATTACH", "DETACH", "CREATE", "DROP", "ALTER"];
fn forbidden(sql: &str) -> bool {
    crate::scoped_connection::tokens(sql)
        .iter()
        .find(|token| !token.trim().is_empty())
        .is_some_and(|word| {
            FORBIDDEN
                .iter()
                .any(|keyword| word.eq_ignore_ascii_case(keyword))
        })
}
fn reserved(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    RESERVED.iter().any(|prefix| name.starts_with(prefix))
}
fn denied<T>(reason: impl Into<String>) -> Result<T, Rejection> {
    Err(Rejection::Denied(reason.into()))
}
fn unsupported<T>(reason: impl Into<String>) -> Result<T, Rejection> {
    Err(Rejection::Unsupported(reason.into()))
}

/// Byte offsets of the parser's line and column positions.
struct Offsets<'a> {
    sql: &'a str,
    lines: Vec<usize>,
}
impl<'a> Offsets<'a> {
    fn new(sql: &'a str) -> Self {
        let mut lines = vec![0];
        lines.extend(sql.match_indices('\n').map(|(index, _)| index + 1));
        Self { sql, lines }
    }
    fn at(&self, location: Location) -> Option<usize> {
        let start = *self
            .lines
            .get(usize::try_from(location.line).ok()?.checked_sub(1)?)?;
        let column = usize::try_from(location.column).ok()?.checked_sub(1)?;
        let line = &self.sql[start..];
        match line.char_indices().nth(column) {
            Some((offset, _)) => Some(start + offset),
            None if line.chars().count() == column => Some(self.sql.len()),
            None => None,
        }
    }
    fn range(&self, span: Span) -> Option<std::ops::Range<usize>> {
        Some(self.at(span.start)?..self.at(span.end)?)
    }
}

/// A table name as one plain identifier, or None if it is qualified.
fn single(name: &ObjectName) -> Option<&sqlparser::ast::Ident> {
    match name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => Some(ident),
        _ => None,
    }
}

/// Names every common table expression of a statement defines.
#[derive(Default)]
struct Ctes(BTreeSet<String>);
impl Visitor for Ctes {
    type Break = ();
    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.0.insert(cte.alias.name.value.to_ascii_lowercase());
            }
        }
        ControlFlow::Continue(())
    }
}

/// Qualified column references of a `RETURNING` clause to one table.
struct Qualifiers<'n> {
    table: &'n str,
    found: Vec<Span>,
}
impl Visitor for Qualifiers<'_> {
    type Break = ();
    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        if let Expr::CompoundIdentifier(parts) = expr
            && let [qualifier, _] = parts.as_slice()
            && qualifier.value.eq_ignore_ascii_case(self.table)
        {
            self.found.push(qualifier.span);
        }
        ControlFlow::Continue(())
    }
}

struct Resolver<'r, F> {
    offsets: Offsets<'r>,
    physical: F,
    ctes: BTreeSet<String>,
    tables: BTreeSet<String>,
    edits: Vec<(std::ops::Range<usize>, String)>,
    failure: Option<Rejection>,
    migration: bool,
}
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
impl<F: Fn(&str) -> Option<String>> Resolver<'_, F> {
    fn fail(&mut self, rejection: Rejection) -> ControlFlow<()> {
        self.failure = Some(rejection);
        ControlFlow::Break(())
    }
    /// Resolves one table reference; `aliased` keeps the logical name
    /// visible to qualified columns when the statement gave no alias.
    fn table(&mut self, name: &ObjectName, aliased: bool) -> Result<(), Rejection> {
        if let [
            ObjectNamePart::Identifier(parent),
            ObjectNamePart::Identifier(table),
        ] = name.0.as_slice()
            && self.migration
            && parent.value.eq_ignore_ascii_case("parent")
        {
            let logical = table.value.to_ascii_lowercase();
            if reserved(&logical) {
                return denied(format!("parent table {logical}"));
            }
            let key = format!("parent.{logical}");
            self.tables.insert(key.clone());
            let physical = (self.physical)(&key)
                .ok_or_else(|| Rejection::Denied(format!("unbound parent table {logical}")))?;
            let start = self
                .offsets
                .range(parent.span)
                .ok_or_else(|| Rejection::Unsupported("statement positions".into()))?
                .start;
            let end = self
                .offsets
                .range(table.span)
                .ok_or_else(|| Rejection::Unsupported("statement positions".into()))?
                .end;
            let replacement = if aliased {
                quoted(&physical)
            } else {
                format!("{} AS {}", quoted(&physical), quoted(&logical))
            };
            self.edits.push((start..end, replacement));
            return Ok(());
        }
        let Some(ident) = single(name) else {
            return denied(format!("qualified table name {name}"));
        };
        let logical = ident.value.to_ascii_lowercase();
        if reserved(&logical) {
            return denied(format!("table {logical}"));
        }
        let physical = (self.physical)(&logical);
        if self.ctes.contains(&logical) {
            if physical.is_some() {
                return unsupported(format!("common table expression {logical} shadows a table"));
            }
            return Ok(());
        }
        self.tables.insert(logical.clone());
        let Some(physical) = physical.filter(|physical| !physical.eq_ignore_ascii_case(&logical))
        else {
            return Ok(());
        };
        let Some(range) = self.offsets.range(ident.span) else {
            return unsupported("statement positions");
        };
        let written = self.offsets.sql[range.clone()].to_string();
        let replacement = if aliased {
            quoted(&physical)
        } else {
            format!("{} AS {written}", quoted(&physical))
        };
        self.edits.push((range, replacement));
        Ok(())
    }
    /// A DML target: qualified columns of its `RETURNING` clause name the
    /// physical table, since the engine does not resolve aliases there.
    fn target(
        &mut self,
        name: &ObjectName,
        aliased: bool,
        returning: Option<&Vec<SelectItem>>,
    ) -> Result<(), Rejection> {
        if single(name).is_none() {
            return denied("a migration parent is read-only");
        }
        self.table(name, aliased)?;
        let (Some(ident), Some(returning), false) = (single(name), returning, aliased) else {
            return Ok(());
        };
        let logical = ident.value.to_ascii_lowercase();
        let Some(physical) = (self.physical)(&logical) else {
            return Ok(());
        };
        let mut qualifiers = Qualifiers {
            table: &logical,
            found: Vec::new(),
        };
        for item in returning {
            if let SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(name),
                _,
            ) = item
                && let Some(qualifier) = single(name)
                && qualifier.value.eq_ignore_ascii_case(&logical)
            {
                qualifiers.found.push(qualifier.span);
            }
            let _ = item.visit(&mut qualifiers);
        }
        for span in qualifiers.found {
            let range = self
                .offsets
                .range(span)
                .ok_or_else(|| Rejection::Unsupported("statement positions".into()))?;
            self.edits.push((range, quoted(&physical)));
        }
        Ok(())
    }
}
impl<F: Fn(&str) -> Option<String>> Visitor for Resolver<'_, F> {
    type Break = ();
    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
        let result = match statement {
            Statement::Insert(insert) => match &insert.table {
                TableObject::TableName(name) => self.target(
                    name,
                    insert.table_alias.is_some(),
                    insert.returning.as_ref(),
                ),
                _ => unsupported("INSERT into a table function"),
            },
            Statement::Update(update) => match &update.table.relation {
                TableFactor::Table { name, alias, .. } if update.table.joins.is_empty() => {
                    self.target(name, alias.is_some(), update.returning.as_ref())
                }
                _ => unsupported("UPDATE target"),
            },
            Statement::Delete(delete) => match &delete.from {
                FromTable::WithFromKeyword(from) if delete.tables.is_empty() => {
                    match from.as_slice() {
                        [target] if target.joins.is_empty() => match &target.relation {
                            TableFactor::Table { name, alias, .. } => {
                                self.target(name, alias.is_some(), delete.returning.as_ref())
                            }
                            _ => unsupported("DELETE target"),
                        },
                        _ => unsupported("DELETE from several tables"),
                    }
                }
                _ => unsupported("DELETE form"),
            },
            _ => Ok(()),
        };
        match result {
            Ok(()) => ControlFlow::Continue(()),
            Err(rejection) => self.fail(rejection),
        }
    }
    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        let result = match factor {
            TableFactor::Table {
                name, alias, args, ..
            } => match args {
                Some(_) => {
                    let function = single(name).map(|ident| ident.value.to_ascii_lowercase());
                    match function {
                        Some(function) if TABLE_FUNCTIONS.contains(&function.as_str()) => Ok(()),
                        _ => denied(format!("table-valued function {name}")),
                    }
                }
                None => self.table(name, alias.is_some()),
            },
            TableFactor::Derived { .. } | TableFactor::NestedJoin { .. } => Ok(()),
            other => unsupported(format!("table source {other}")),
        };
        match result {
            Ok(()) => ControlFlow::Continue(()),
            Err(rejection) => self.fail(rejection),
        }
    }
    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        let result = match expr {
            Expr::Function(function) => match single(&function.name) {
                Some(name) => {
                    let name = name.value.to_ascii_lowercase();
                    if DENIED_FUNCTIONS.contains(&name.as_str()) || reserved(&name) {
                        denied(format!("function {name}"))
                    } else {
                        Ok(())
                    }
                }
                None => denied(format!("qualified function {}", function.name)),
            },
            Expr::CompoundIdentifier(parts)
                if self.migration
                    && parts.len() == 3
                    && parts[0].value.eq_ignore_ascii_case("parent") =>
            {
                let start = self.offsets.range(parts[0].span).map(|r| r.start);
                let end = self.offsets.range(parts[1].span).map(|r| r.end);
                match (start, end) {
                    (Some(start), Some(end)) => {
                        self.edits.push((start..end, quoted(&parts[1].value)));
                        Ok(())
                    }
                    _ => unsupported("statement positions"),
                }
            }
            Expr::CompoundIdentifier(parts) if parts.len() > 2 => denied("schema-qualified column"),
            _ => Ok(()),
        };
        match result {
            Ok(()) => ControlFlow::Continue(()),
            Err(rejection) => self.fail(rejection),
        }
    }
}

/// Checks one statement against the supported surface and resolves its
/// tables through `physical`, which maps a lowercase logical name to the
/// physical table, or None for a name the caller does not bind.
pub fn analyze(
    sql: &str,
    physical: impl Fn(&str) -> Option<String>,
) -> Result<Analyzed, Rejection> {
    analyze_with(sql, physical, false)
}

/// Migration-only resolver: `parent.<table>` can be read, never written.
/// The caller binds each parent name to its frozen read-only table.
pub fn analyze_migration(
    sql: &str,
    physical: impl Fn(&str) -> Option<String>,
) -> Result<Analyzed, Rejection> {
    analyze_with(sql, physical, true)
}
fn analyze_with(
    sql: &str,
    physical: impl Fn(&str) -> Option<String>,
    migration: bool,
) -> Result<Analyzed, Rejection> {
    if sql.len() > MAX_STATEMENT_BYTES {
        return unsupported("statement length");
    }
    let statements = match Parser::parse_sql(&SQLiteDialect {}, sql) {
        Ok(statements) => statements,
        // A schema, pragma or attachment statement is denied in any spelling.
        Err(_) if forbidden(sql) => return denied("schema, pragma or attachment statement"),
        Err(error) => return unsupported(error.to_string()),
    };
    let statement = match statements.as_slice() {
        [statement] => statement,
        [] => return unsupported("empty statement"),
        _ => return unsupported("one statement per call"),
    };
    let kind = match statement {
        Statement::Query(_) => Kind::Read,
        Statement::Insert(_) => Kind::Insert,
        Statement::Update(_) => Kind::Update,
        Statement::Delete(_) => Kind::Delete,
        Statement::Pragma { .. }
        | Statement::AttachDatabase { .. }
        | Statement::CreateTable(_)
        | Statement::CreateView(_)
        | Statement::CreateIndex(_)
        | Statement::CreateTrigger(_)
        | Statement::CreateVirtualTable { .. }
        | Statement::Drop { .. }
        | Statement::DropTrigger(_)
        | Statement::AlterTable(_) => return denied("schema, pragma or attachment statement"),
        other => {
            let keyword = other.to_string();
            let keyword = keyword.split_whitespace().next().unwrap_or_default();
            return unsupported(format!("{keyword} statements"));
        }
    };
    let mut ctes = Ctes::default();
    let _ = statement.visit(&mut ctes);
    let mut resolver = Resolver {
        offsets: Offsets::new(sql),
        physical,
        ctes: ctes.0,
        tables: BTreeSet::new(),
        edits: Vec::new(),
        failure: None,
        migration,
    };
    let _ = statement.visit(&mut resolver);
    if let Some(failure) = resolver.failure {
        return Err(failure);
    }
    let mut edits = resolver.edits;
    edits.sort_by_key(|(range, _)| range.start);
    edits.dedup_by(|a, b| a.0 == b.0);
    let mut rewritten = String::with_capacity(sql.len() + 32 * edits.len());
    let mut copied = 0;
    for (range, replacement) in edits {
        if range.start < copied {
            return unsupported("overlapping table references");
        }
        rewritten.push_str(&sql[copied..range.start]);
        rewritten.push_str(&replacement);
        copied = range.end;
    }
    rewritten.push_str(&sql[copied..]);
    Ok(Analyzed {
        sql: rewritten,
        kind,
        tables: resolver.tables,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_parents_are_bound_by_position_and_cannot_be_written() {
        let bind = |name: &str| (name == "parent.items").then(|| "secure_frozen_items".into());
        let sql = "INSERT INTO items(title) SELECT parent.items.title FROM parent.items";
        assert!(analyze(sql, bind).is_err());
        let analyzed = analyze_migration(sql, bind).unwrap();
        assert_eq!(
            analyzed.tables,
            BTreeSet::from(["items".into(), "parent.items".into()])
        );
        assert_eq!(
            analyzed.sql,
            "INSERT INTO items(title) SELECT \"items\".title FROM \"secure_frozen_items\" AS \"items\""
        );
        for sql in [
            "DELETE FROM parent.items",
            "UPDATE parent.items SET title='x'",
            "INSERT INTO parent.items(title) VALUES('x')",
            "SELECT * FROM main.items",
        ] {
            assert!(analyze_migration(sql, bind).is_err(), "{sql}");
        }
        assert!(analyze_migration("SELECT * FROM parent.missing", bind).is_err());
    }

    fn names(name: &str) -> Option<String> {
        ["items", "tags"]
            .contains(&name)
            .then(|| format!("ns_{name}"))
    }
    fn rewrite(sql: &str) -> String {
        analyze(sql, names)
            .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
            .sql
    }

    #[test]
    fn names_resolve_by_position_and_keep_aliases_literals_and_columns() {
        for (sql, expected) in [
            (
                "SELECT items.items, 'items' FROM items WHERE items = 1",
                "SELECT items.items, 'items' FROM \"ns_items\" AS items WHERE items = 1",
            ),
            (
                "SELECT i.id FROM items i JOIN \"tags\" t ON t.id = i.id",
                "SELECT i.id FROM \"ns_items\" i JOIN \"ns_tags\" t ON t.id = i.id",
            ),
            (
                "WITH recent AS (SELECT * FROM items) SELECT * FROM recent, tags",
                "WITH recent AS (SELECT * FROM \"ns_items\" AS items) SELECT * FROM recent, \"ns_tags\" AS tags",
            ),
            (
                "SELECT * FROM items WHERE id IN (SELECT id FROM tags WHERE tags.id = items.id)",
                "SELECT * FROM \"ns_items\" AS items WHERE id IN (SELECT id FROM \"ns_tags\" AS tags WHERE tags.id = items.id)",
            ),
            (
                "SELECT id, row_number() OVER (PARTITION BY title ORDER BY id) FROM `items`",
                "SELECT id, row_number() OVER (PARTITION BY title ORDER BY id) FROM \"ns_items\" AS `items`",
            ),
            (
                "INSERT INTO items(id) VALUES('items') ON CONFLICT(id) DO UPDATE SET id=excluded.id RETURNING items.id, items.*",
                "INSERT INTO \"ns_items\" AS items(id) VALUES('items') ON CONFLICT(id) DO UPDATE SET id=excluded.id RETURNING \"ns_items\".id, \"ns_items\".*",
            ),
            (
                "UPDATE OR IGNORE items SET title = ? WHERE items.id IN (SELECT id FROM tags)",
                "UPDATE OR IGNORE \"ns_items\" AS items SET title = ? WHERE items.id IN (SELECT id FROM \"ns_tags\" AS tags)",
            ),
            (
                "DELETE FROM items\n  WHERE id = :id",
                "DELETE FROM \"ns_items\" AS items\n  WHERE id = :id",
            ),
            (
                "SELECT 'é', x FROM\n  items, json_each('[1]')",
                "SELECT 'é', x FROM\n  \"ns_items\" AS items, json_each('[1]')",
            ),
        ] {
            assert_eq!(rewrite(sql), expected, "{sql}");
        }
        let analyzed = analyze("SELECT * FROM items JOIN unknown", names).unwrap();
        assert_eq!(analyzed.kind, Kind::Read);
        assert_eq!(
            analyzed.tables,
            ["items", "unknown"].map(String::from).into()
        );
    }

    #[test]
    fn statements_outside_the_surface_are_refused_before_the_engine() {
        for sql in [
            "PRAGMA user_version = 3",
            "PRAGMA journal_mode=DELETE",
            "DETACH DATABASE y",
            "ATTACH DATABASE 'x' AS y",
            "CREATE TRIGGER t AFTER INSERT ON items BEGIN SELECT 1; END",
            "CREATE VIEW v AS SELECT * FROM items",
            "CREATE VIRTUAL TABLE v USING fts5(a)",
            "DROP TABLE items",
            "SELECT * FROM main.items",
            "SELECT * FROM rocky_operations",
            "SELECT * FROM pragma_table_info('items')",
            "SELECT * FROM sqlite_schema",
            "SELECT load_extension('x')",
            "SELECT main.items.id FROM items",
        ] {
            assert!(
                matches!(analyze(sql, names), Err(Rejection::Denied(_))),
                "{sql}"
            );
        }
        for sql in [
            "SELECT 1; SELECT 2",
            "",
            "VACUUM",
            "SELECT * FROM items INDEXED BY by_title",
            "WITH items AS (SELECT 1) SELECT * FROM items",
            "EXPLAIN QUERY PLAN SELECT * FROM items",
            "SELECT * FROM",
        ] {
            assert!(
                matches!(analyze(sql, names), Err(Rejection::Unsupported(_))),
                "{sql}"
            );
        }
    }
}
