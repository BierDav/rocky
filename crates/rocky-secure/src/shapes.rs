//! Test-only shapes of committed SQL transactions (features_13 phase 3).
//!
//! A SQL session collects its statements, bound values and captured rows;
//! each local commit classifies them into one [`Shape`]. Nothing reaches the
//! database. With `ROCKY_INTENT_SHAPES` naming a directory, each shape is
//! appended as one JSON line to `<dir>/<ROCKY_INTENT_SHAPES_SOURCE>.jsonl`
//! for `scripts/intent-shapes` to aggregate.
use rocky_core::{LogicalSchema, LogicalTable, SqlValue, TableClass};
use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    AssignmentTarget, BinaryOperator, Expr, FromTable, FunctionArg, FunctionArgExpr,
    FunctionArguments, ObjectName, SetExpr, SqliteOnConflict, Statement, TableFactor, TableObject,
    Value, visit_expressions,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

/// Partitions one transaction may reach (`MAX_BUNDLE_FRAMES`).
const MAX_PARTITIONS: usize = crate::MAX_BUNDLE_FRAMES;

/// Exactly one class per transaction, checked in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Refused,
    Coordinated,
    Constrained,
    Keyed,
    Predicate,
    Split,
}
/// The effect algebra of the prototype's symbolic engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Add,
    Assign,
    Append,
    KeyedDelete,
    FreshInsert,
}
/// How the transaction was committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Plain,
    Coordinated,
    Reauthoring,
    Move,
    /// A staged publication or a migration session: not an application write.
    Migration,
}
/// One committed transaction. `class` is absent when no replicated row changed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Shape {
    pub process: u32,
    pub at_ms: u64,
    pub origin: Origin,
    pub class: Option<Class>,
    /// What made the transaction `Refused`.
    pub refusal: Option<String>,
    pub statements: usize,
    /// Statements writing replicated tables.
    pub writes: usize,
    pub rows: usize,
    pub partitions: usize,
    /// Some cell is assigned, incremented or deleted twice.
    pub collapses: bool,
    pub algebra: BTreeSet<Op>,
    /// Every replicated write is one of the algebra's operations.
    pub algebra_only: bool,
    pub tables: BTreeSet<String>,
    /// Replicated write statements, without values.
    pub sql: Vec<String>,
}

struct Row {
    table: String,
    partition: Vec<SqlValue>,
    group: String,
}
/// One SQL session's open transaction.
#[derive(Default)]
pub(crate) struct Pending {
    current: Option<(String, Vec<SqlValue>)>,
    statements: Vec<(String, Vec<SqlValue>)>,
    marks: Vec<usize>,
    rows: Vec<Row>,
    schema: Option<LogicalSchema>,
}
impl Pending {
    /// A new transaction; an autocommitting statement already named itself.
    pub(crate) fn begin(&mut self) {
        *self = Self {
            current: self.current.take(),
            ..Self::default()
        };
    }
    /// The statement about to run; kept only if it succeeds.
    pub(crate) fn statement(&mut self, sql: &str, params: &[SqlValue]) {
        self.current = Some((sql.into(), params.to_vec()));
    }
    pub(crate) fn succeeded(&mut self) {
        self.statements.extend(self.current.take());
    }
    pub(crate) fn savepoint(&mut self) {
        self.marks.push(self.statements.len());
    }
    pub(crate) fn release(&mut self, index: usize) {
        self.marks.truncate(index);
    }
    pub(crate) fn rollback_to(&mut self, index: usize) {
        if let Some(mark) = self.marks.get(index) {
            self.statements.truncate(*mark);
        }
        self.marks.truncate(index + 1);
    }
    /// A captured replicated row, as the commit publishes it.
    pub(crate) fn row(&mut self, schema: &LogicalSchema, table: &str, image: &[SqlValue]) {
        let Some(logical) = find(schema, table) else {
            return;
        };
        let partition = logical
            .partition_by
            .iter()
            .filter_map(|column| position(logical, column).and_then(|index| image.get(index)))
            .cloned()
            .collect();
        let group = logical
            .partition_group
            .clone()
            .unwrap_or_else(|| logical.sql_name.clone());
        if self.schema.is_none() {
            self.schema = Some(schema.clone());
        }
        self.rows.push(Row {
            table: logical.sql_name.clone(),
            partition,
            group,
        });
    }
    /// Classifies the committed transaction and hands it to the runner.
    pub(crate) fn committed(&mut self, origin: Origin) {
        let pending = std::mem::take(self);
        let shape = pending.classify(origin);
        emit(&shape);
    }
    fn classify(self, origin: Origin) -> Shape {
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let partitions: BTreeSet<_> = self
            .rows
            .iter()
            .map(|row| (row.group.clone(), format!("{:?}", row.partition)))
            .collect();
        let mut shape = Shape {
            process: std::process::id(),
            at_ms,
            origin,
            class: None,
            refusal: None,
            statements: self.statements.len(),
            writes: 0,
            rows: self.rows.len(),
            partitions: partitions.len(),
            collapses: false,
            algebra: BTreeSet::new(),
            algebra_only: true,
            tables: self.rows.iter().map(|row| row.table.clone()).collect(),
            sql: Vec::new(),
        };
        // A move addresses its row by key and reaches two partitions.
        let moved = origin == Origin::Move;
        if moved {
            shape.writes += 1;
            shape.partitions = shape.partitions.max(2);
            shape.algebra_only = false;
        }
        let Some(schema) = self.schema.as_ref() else {
            shape.class = moved.then_some(Class::Keyed);
            return shape;
        };
        let mut constrained = false;
        let mut keyed = true;
        let mut cells = Cells::default();
        for (sql, params) in &self.statements {
            let parsed = match rocky_storage::parsed::parse(sql) {
                Ok(parsed) => parsed,
                Err(error) => {
                    shape.refusal.get_or_insert(format!("unparsed: {error}"));
                    continue;
                }
            };
            for statement in parsed.iter() {
                let Some(write) = Write::of(statement, schema) else {
                    if let Some(refusal) = schema_statement(statement) {
                        shape.refusal.get_or_insert(refusal);
                    }
                    continue;
                };
                shape.writes += 1;
                shape.sql.push(sql.clone());
                let values = Placeholders::bind(statement, params);
                if let Some(refusal) = write.refusal(statement) {
                    shape.refusal.get_or_insert(refusal);
                }
                constrained |= write.constrained(schema);
                let effect = write.effect(&values);
                keyed &= effect.keyed;
                shape.algebra_only &= effect.keyed && !effect.other;
                shape.algebra.extend(effect.ops.iter().copied());
                shape.collapses |= cells.assign(&write.table.sql_name, effect.key, &effect.cells);
            }
        }
        if partitions.len() > MAX_PARTITIONS {
            shape.refusal.get_or_insert(format!(
                "{} partitions exceed {MAX_PARTITIONS}",
                partitions.len()
            ));
        }
        shape.algebra_only &= shape.writes > 0;
        let split = shape.partitions > 1;
        shape.class = Some(if shape.refusal.is_some() {
            Class::Refused
        } else if origin == Origin::Coordinated {
            Class::Coordinated
        } else if constrained {
            Class::Constrained
        } else if keyed && shape.writes > 0 {
            Class::Keyed
        } else if !split {
            Class::Predicate
        } else {
            Class::Split
        });
        shape
    }
}

fn emit(shape: &Shape) {
    use std::io::Write as _;
    use std::sync::{Mutex, OnceLock};
    static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let file = FILE.get_or_init(|| {
        let directory = std::env::var_os("ROCKY_INTENT_SHAPES")?;
        let source =
            std::env::var("ROCKY_INTENT_SHAPES_SOURCE").unwrap_or_else(|_| "unnamed".into());
        std::fs::create_dir_all(&directory).ok()?;
        let path =
            std::path::Path::new(&directory).join(format!("{}.jsonl", source.replace('/', "-")));
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    });
    if let (Some(file), Ok(mut line)) = (file, serde_json::to_vec(shape)) {
        line.push(b'\n');
        if let Ok(mut file) = file.lock() {
            let _ = file.write_all(&line);
        }
    }
}

fn find<'a>(schema: &'a LogicalSchema, name: &str) -> Option<&'a LogicalTable> {
    schema
        .tables
        .iter()
        .find(|table| table.sql_name.eq_ignore_ascii_case(name))
}
fn position(table: &LogicalTable, column: &str) -> Option<usize> {
    table
        .columns
        .iter()
        .position(|candidate| candidate.sql_name.eq_ignore_ascii_case(column))
}
fn name_of(name: &ObjectName) -> String {
    name.0
        .last()
        .and_then(|part| part.as_ident())
        .map_or_else(String::new, |ident| ident.value.clone())
}
fn column_of(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Identifier(ident) => Some(&ident.value),
        Expr::CompoundIdentifier(idents) => idents.last().map(|ident| ident.value.as_str()),
        Expr::Nested(inner) => column_of(inner),
        _ => None,
    }
}
fn schema_statement(statement: &Statement) -> Option<String> {
    match statement {
        Statement::CreateTable(_)
        | Statement::CreateIndex(_)
        | Statement::CreateView { .. }
        | Statement::CreateTrigger(_)
        | Statement::AlterTable(_)
        | Statement::Drop { .. } => Some("schema statement".into()),
        _ => None,
    }
}

/// Bound values by placeholder occurrence, numbered as SQLite numbers them.
struct Placeholders(BTreeMap<usize, SqlValue>);
impl Placeholders {
    fn bind(statement: &Statement, params: &[SqlValue]) -> Self {
        let mut bound = BTreeMap::new();
        let (mut next, mut named) = (0usize, BTreeMap::<String, usize>::new());
        let _ = visit_expressions(statement, |expr| {
            if let Expr::Value(value) = expr
                && let Value::Placeholder(text) = &value.value
            {
                let index = if text == "?" {
                    next += 1;
                    next
                } else if let Some(number) = text.strip_prefix('?').and_then(|n| n.parse().ok()) {
                    next = next.max(number);
                    number
                } else {
                    *named.entry(text.clone()).or_insert_with(|| {
                        next += 1;
                        next
                    })
                };
                if let Some(value) = params.get(index - 1) {
                    bound.insert(expr as *const Expr as usize, value.clone());
                }
            }
            ControlFlow::<()>::Continue(())
        });
        Self(bound)
    }
    /// A value the statement fixes without reading a row.
    fn constant(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Value(value) => match &value.value {
                Value::Placeholder(_) => self
                    .0
                    .get(&(expr as *const Expr as usize))
                    .map(|value| format!("{value:?}")),
                value => Some(value.to_string()),
            },
            Expr::Nested(inner) => self.constant(inner),
            Expr::UnaryOp { expr: inner, op } => {
                self.constant(inner).map(|value| format!("{op}{value}"))
            }
            _ => None,
        }
    }
}

/// Cells assigned so far, by table, key and column; `None` marks the whole row.
#[derive(Default)]
struct Cells(BTreeSet<(String, String, Option<String>)>);
impl Cells {
    /// Records the cells; true if one was already written.
    fn assign(&mut self, table: &str, key: Option<String>, cells: &[Option<String>]) -> bool {
        let Some(key) = key else {
            return false;
        };
        let mut repeated = false;
        for cell in cells {
            let whole = (table.to_owned(), key.clone(), None);
            repeated |= self.0.contains(&whole)
                || match cell {
                    Some(_) => self.0.contains(&(table.into(), key.clone(), cell.clone())),
                    None => self.0.iter().any(|(t, k, _)| t == table && *k == key),
                };
            self.0.insert((table.into(), key.clone(), cell.clone()));
        }
        repeated
    }
}

enum Verb<'a> {
    Insert(&'a sqlparser::ast::Insert),
    Update(&'a sqlparser::ast::Update),
    Delete(&'a sqlparser::ast::Delete),
}
/// A statement writing one replicated table.
struct Write<'a> {
    verb: Verb<'a>,
    table: &'a LogicalTable,
}
#[derive(Default)]
struct Effect {
    keyed: bool,
    /// An assignment outside the algebra, or an insert that replaces.
    other: bool,
    ops: BTreeSet<Op>,
    key: Option<String>,
    cells: Vec<Option<String>>,
}
impl<'a> Write<'a> {
    fn of(statement: &'a Statement, schema: &'a LogicalSchema) -> Option<Self> {
        let (verb, name) = match statement {
            Statement::Insert(insert) => match &insert.table {
                TableObject::TableName(name) => (Verb::Insert(insert), name_of(name)),
                _ => return None,
            },
            Statement::Update(update) => match &update.table.relation {
                TableFactor::Table { name, .. } => (Verb::Update(update), name_of(name)),
                _ => return None,
            },
            Statement::Delete(delete) => {
                let tables = match &delete.from {
                    FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => {
                        tables
                    }
                };
                match tables.first().map(|table| &table.relation) {
                    Some(TableFactor::Table { name, .. }) => (Verb::Delete(delete), name_of(name)),
                    _ => return None,
                }
            }
            _ => return None,
        };
        let table = find(schema, &name).filter(|table| table.class == TableClass::Mergeable)?;
        Some(Self { verb, table })
    }
    /// What intent replay refuses at commit: clock, random and unknown
    /// functions, row ids and engine-assigned keys.
    fn refusal(&self, statement: &Statement) -> Option<String> {
        let mut refusal = None;
        let _ = visit_expressions(statement, |expr| {
            match expr {
                Expr::Function(function) => {
                    let name = name_of(&function.name).to_ascii_lowercase();
                    if !deterministic(&name, &function.args) {
                        refusal = Some(format!("function {name}"));
                    }
                }
                Expr::Identifier(ident)
                    if matches!(
                        ident.value.to_ascii_lowercase().as_str(),
                        "rowid" | "_rowid_" | "oid"
                    ) && position(self.table, &ident.value).is_none() =>
                {
                    refusal = Some(format!("row id {}", ident.value));
                }
                _ => {}
            }
            if refusal.is_some() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        if refusal.is_none()
            && let Verb::Insert(insert) = &self.verb
            && !insert.columns.is_empty()
            && !self.table.primary_key.iter().all(|key| {
                insert
                    .columns
                    .iter()
                    .any(|column| name_of(column).eq_ignore_ascii_case(key))
            })
        {
            refusal = Some(format!("engine-assigned key of {}", self.table.sql_name));
        }
        refusal
    }
    /// Columns the statement writes; `None` when it writes whole rows.
    fn assigned(&self) -> Option<BTreeSet<String>> {
        match &self.verb {
            Verb::Update(update) => Some(
                update
                    .assignments
                    .iter()
                    .flat_map(|assignment| match &assignment.target {
                        AssignmentTarget::ColumnName(name) => vec![name_of(name)],
                        AssignmentTarget::Tuple(names) => names.iter().map(name_of).collect(),
                    })
                    .map(|name| name.to_ascii_lowercase())
                    .collect(),
            ),
            _ => None,
        }
    }
    /// Touches a unique index, foreign key or CHECK constraint.
    fn constrained(&self, schema: &LogicalSchema) -> bool {
        let assigned = self.assigned();
        let writes = |columns: Vec<&String>| match &assigned {
            None => true,
            Some(assigned) => columns
                .iter()
                .any(|column| assigned.contains(&column.to_ascii_lowercase())),
        };
        let inserts_or_updates = !matches!(self.verb, Verb::Delete(_));
        let unique = self.table.indexes.iter().any(|index| {
            index.unique && writes(index.columns.iter().map(|column| &column.name).collect())
        });
        let outgoing = self
            .table
            .foreign_keys
            .iter()
            .any(|key| writes(key.columns.iter().collect()));
        let check = self.table.create_sql.to_ascii_uppercase().contains("CHECK");
        let referenced = schema.tables.iter().any(|table| {
            table.foreign_keys.iter().any(|key| {
                key.table.eq_ignore_ascii_case(&self.table.sql_name)
                    && match self.verb {
                        Verb::Delete(_) => true,
                        Verb::Update(_) => writes(key.references.iter().collect()),
                        Verb::Insert(_) => false,
                    }
            })
        });
        (inserts_or_updates && (unique || outgoing || check)) || referenced
    }
    fn effect(&self, values: &Placeholders) -> Effect {
        let mut effect = Effect::default();
        let key = &self.table.primary_key;
        match &self.verb {
            Verb::Insert(insert) => {
                let Some(source) = &insert.source else {
                    return effect;
                };
                let SetExpr::Values(rows) = source.body.as_ref() else {
                    return effect;
                };
                let columns: Vec<String> = if insert.columns.is_empty() {
                    self.table
                        .columns
                        .iter()
                        .map(|c| c.sql_name.clone())
                        .collect()
                } else {
                    insert.columns.iter().map(name_of).collect()
                };
                effect.keyed = key.iter().all(|key| {
                    columns
                        .iter()
                        .any(|column| column.eq_ignore_ascii_case(key))
                });
                // `OR ABORT`, `FAIL` and `ROLLBACK` only end the statement differently.
                let replaces = matches!(
                    insert.or,
                    Some(SqliteOnConflict::Replace | SqliteOnConflict::Ignore)
                ) || insert.on.is_some()
                    || insert.replace_into;
                effect.other = replaces;
                if effect.keyed && !replaces {
                    effect.ops.insert(Op::FreshInsert);
                }
                // One row's key; several rows are each fresh.
                if let [row] = rows.rows.as_slice() {
                    effect.key = key
                        .iter()
                        .map(|key| {
                            columns
                                .iter()
                                .position(|column| column.eq_ignore_ascii_case(key))
                                .and_then(|index| row.get(index))
                                .and_then(|expr| values.constant(expr))
                        })
                        .collect::<Option<Vec<_>>>()
                        .map(|key| key.join(","));
                    effect.cells = vec![None];
                }
            }
            Verb::Update(update) => {
                effect.key = self.key(update.selection.as_ref(), values);
                effect.keyed = effect.key.is_some()
                    && update.from.is_none()
                    && update.limit.is_none()
                    && update.or.is_none();
                for assignment in &update.assignments {
                    let AssignmentTarget::ColumnName(name) = &assignment.target else {
                        effect.other = true;
                        continue;
                    };
                    let column = name_of(name).to_ascii_lowercase();
                    match operation(&column, &assignment.value, values) {
                        Some(op) => {
                            effect.ops.insert(op);
                        }
                        None => effect.other = true,
                    }
                    effect.cells.push(Some(column));
                }
                if !effect.keyed {
                    effect.ops.clear();
                }
            }
            Verb::Delete(delete) => {
                effect.key = self.key(delete.selection.as_ref(), values);
                effect.keyed = effect.key.is_some()
                    && delete.using.is_none()
                    && delete.limit.is_none()
                    && delete.order_by.is_empty();
                if effect.keyed {
                    effect.ops.insert(Op::KeyedDelete);
                }
                effect.cells = vec![None];
            }
        }
        effect
    }
    /// The full primary key a filter pins by equality, if it does.
    fn key(&self, selection: Option<&Expr>, values: &Placeholders) -> Option<String> {
        let mut pinned = BTreeMap::new();
        conjuncts(selection?, &mut |expr| {
            if let Expr::BinaryOp {
                left,
                op: BinaryOperator::Eq,
                right,
            } = expr
            {
                for (column, value) in [(left, right), (right, left)] {
                    if let (Some(column), Some(value)) = (column_of(column), values.constant(value))
                    {
                        pinned.insert(column.to_ascii_lowercase(), value);
                    }
                }
            }
        });
        self.table
            .primary_key
            .iter()
            .map(|key| pinned.get(&key.to_ascii_lowercase()).cloned())
            .collect::<Option<Vec<_>>>()
            .map(|key| key.join(","))
    }
}
fn conjuncts<'e>(expr: &'e Expr, visit: &mut impl FnMut(&'e Expr)) {
    match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => {
            conjuncts(left, visit);
            conjuncts(right, visit);
        }
        Expr::Nested(inner) => conjuncts(inner, visit),
        expr => visit(expr),
    }
}
/// `c = v`, `c = c + v`, `c = c - v` or `c = c || v` with `v` reading no row.
fn operation(column: &str, value: &Expr, values: &Placeholders) -> Option<Op> {
    let constant = |expr: &Expr| values.constant(expr).is_some() || reads_nothing(expr);
    if constant(value) {
        return Some(Op::Assign);
    }
    let Expr::BinaryOp { left, op, right } = value else {
        return None;
    };
    let is_column =
        |expr: &Expr| column_of(expr).is_some_and(|name| name.eq_ignore_ascii_case(column));
    match op {
        BinaryOperator::Plus
            if (is_column(left) && constant(right)) || (is_column(right) && constant(left)) =>
        {
            Some(Op::Add)
        }
        BinaryOperator::Minus if is_column(left) && constant(right) => Some(Op::Add),
        BinaryOperator::StringConcat if is_column(left) && constant(right) => Some(Op::Append),
        _ => None,
    }
}
/// An expression of literals, bound values and deterministic functions.
fn reads_nothing(expr: &Expr) -> bool {
    visit_expressions(expr, |expr| match expr {
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => ControlFlow::Break(()),
        _ => ControlFlow::Continue(()),
    })
    .is_continue()
}
/// SQLite functions whose result depends only on their arguments.
fn deterministic(name: &str, args: &FunctionArguments) -> bool {
    const ALLOWED: &[&str] = &[
        "abs",
        "avg",
        "char",
        "coalesce",
        "concat",
        "concat_ws",
        "count",
        "format",
        "glob",
        "group_concat",
        "hex",
        "ifnull",
        "iif",
        "instr",
        "json",
        "json_array",
        "json_array_length",
        "json_extract",
        "json_group_array",
        "json_group_object",
        "json_insert",
        "json_object",
        "json_patch",
        "json_quote",
        "json_remove",
        "json_replace",
        "json_set",
        "json_type",
        "json_valid",
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
        "string_agg",
        "substr",
        "substring",
        "sum",
        "total",
        "trim",
        "typeof",
        "unhex",
        "unicode",
        "unlikely",
        "upper",
        "zeroblob",
    ];
    const TIME: &[&str] = &[
        "date",
        "time",
        "datetime",
        "julianday",
        "strftime",
        "unixepoch",
        "timediff",
    ];
    if ALLOWED.contains(&name) {
        return true;
    }
    if !TIME.contains(&name) {
        return false;
    }
    // Deterministic only for an explicit time value, never 'now'.
    let FunctionArguments::List(list) = args else {
        return false;
    };
    let times = if name == "strftime" { 1 } else { 0 };
    list.args.get(times).is_some_and(|arg| {
        !matches!(
            arg,
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value)))
                if value.value.to_string().trim_matches('\'').eq_ignore_ascii_case("now")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_core::{
        Affinity, Collation, ColumnClass, LogicalColumn, LogicalForeignKey, SchemaSource,
        UpdatePolicy,
    };

    fn table(name: &str, columns: &[&str], key: &[&str], partition: &[&str]) -> LogicalTable {
        LogicalTable {
            sql_name: name.into(),
            granted_like: None,
            class: TableClass::Mergeable,
            coordinated: false,
            create_sql: format!("CREATE TABLE {name}(...)"),
            auto_generate: false,
            columns: columns
                .iter()
                .map(|column| LogicalColumn {
                    sql_name: (*column).into(),
                    affinity: Affinity::Integer,
                    not_null: false,
                    default: None,
                    collation: Collation::Binary,
                    class: ColumnClass::Mergeable,
                })
                .collect(),
            primary_key: key.iter().map(|key| (*key).into()).collect(),
            partition_by: partition.iter().map(|column| (*column).into()).collect(),
            partition_group: None,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            existence_log: false,
        }
    }
    fn schema() -> LogicalSchema {
        let mut links = table("links", &["list", "id", "item"], &["list", "id"], &["list"]);
        links.foreign_keys.push(LogicalForeignKey {
            table: "items".into(),
            columns: vec!["list".into(), "item".into()],
            references: vec!["list".into(), "id".into()],
            on_delete: "CASCADE".into(),
            on_update: "NO ACTION".into(),
        });
        LogicalSchema {
            format: 0,
            publisher: [0; 32],
            application: "shapes".into(),
            source: SchemaSource {
                kind: "test".into(),
                version: 1,
                identity: "shapes".into(),
            },
            update_policy: UpdatePolicy::Assigned,
            tables: vec![
                table("counters", &["id", "n", "log"], &["id"], &[]),
                table(
                    "items",
                    &["list", "id", "title", "done"],
                    &["list", "id"],
                    &["list"],
                ),
                links,
            ],
            views: Vec::new(),
        }
    }
    fn shape(statements: &[(&str, Vec<SqlValue>)], rows: &[(&str, Vec<SqlValue>)]) -> Shape {
        shape_as(Origin::Plain, statements, rows)
    }
    fn shape_as(
        origin: Origin,
        statements: &[(&str, Vec<SqlValue>)],
        rows: &[(&str, Vec<SqlValue>)],
    ) -> Shape {
        let schema = schema();
        let mut pending = Pending::default();
        for (sql, params) in statements {
            pending.statement(sql, params);
            pending.succeeded();
        }
        for (table, image) in rows {
            pending.row(&schema, table, image);
        }
        pending.classify(origin)
    }
    use SqlValue::Integer as I;

    #[test]
    fn savepoints_discard_rolled_back_and_failed_statements() {
        let mut pending = Pending::default();
        pending.begin();
        pending.statement("UPDATE counters SET n = 1 WHERE id = 7", &[]);
        pending.succeeded();
        pending.savepoint();
        pending.statement("UPDATE counters SET n = random() WHERE id = 7", &[]);
        pending.succeeded();
        pending.savepoint();
        pending.statement("DELETE FROM counters WHERE id = 7", &[]);
        pending.succeeded();
        pending.rollback_to(0);
        pending.statement("UPDATE counters SET n = missing() WHERE id = 7", &[]);
        // A failed statement never calls succeeded().
        pending.release(0);
        pending.row(&schema(), "counters", &[I(7), I(1), SqlValue::Null]);
        let shape = pending.classify(Origin::Plain);
        assert_eq!(shape.class, Some(Class::Keyed));
        assert_eq!(shape.algebra, [Op::Assign].into());
        assert_eq!((shape.statements, shape.writes), (1, 1));
        assert!(!shape.collapses);
    }

    #[test]
    fn keyed_increments_collapse_inside_the_algebra() {
        let shape = shape(
            &[
                (
                    "UPDATE counters SET n = n + ? WHERE id = ?",
                    vec![I(1), I(7)],
                ),
                (
                    "UPDATE counters SET n = n + 1, log = log || 'x' WHERE id = ?",
                    vec![I(7)],
                ),
            ],
            &[("counters", vec![I(7), I(2), SqlValue::Null])],
        );
        assert_eq!(shape.class, Some(Class::Keyed));
        assert_eq!(shape.algebra, [Op::Add, Op::Append].into());
        assert!(shape.algebra_only && shape.collapses);
        assert_eq!(
            (shape.statements, shape.writes, shape.partitions),
            (2, 2, 1)
        );
    }

    #[test]
    fn filters_are_predicates_in_one_partition_and_splits_across_several() {
        let update = (
            "UPDATE items SET done = 1 WHERE list = ? AND done = 0",
            vec![I(1)],
        );
        let one = shape(
            std::slice::from_ref(&update),
            &[("items", vec![I(1), I(1), I(0), I(1)])],
        );
        assert_eq!(one.class, Some(Class::Predicate));
        assert!(!one.algebra_only && one.algebra.is_empty());
        let two = shape(
            &[("UPDATE items SET done = 1 WHERE done = 0", vec![])],
            &[
                ("items", vec![I(1), I(1), I(0), I(1)]),
                ("items", vec![I(2), I(1), I(0), I(1)]),
            ],
        );
        assert_eq!((two.class, two.partitions), (Some(Class::Split), 2));
    }

    #[test]
    fn refusals_come_first_and_constraints_before_keys() {
        let clock = shape_as(
            Origin::Coordinated,
            &[(
                "UPDATE items SET title = datetime('now') WHERE list = ? AND id = ?",
                vec![I(1), I(2)],
            )],
            &[("items", vec![I(1), I(2), I(0), I(0)])],
        );
        assert_eq!(clock.class, Some(Class::Refused));
        assert_eq!(clock.refusal.as_deref(), Some("function datetime"));
        let fixed = shape(
            &[(
                "UPDATE items SET title = date('2026-10-28') WHERE list = ? AND id = ?",
                vec![I(1), I(2)],
            )],
            &[("items", vec![I(1), I(2), I(0), I(0)])],
        );
        assert_eq!(fixed.class, Some(Class::Keyed));
        let engine_key = shape(
            &[("INSERT INTO items(list, title) VALUES(?, 'a')", vec![I(1)])],
            &[("items", vec![I(1), I(9), I(0), I(0)])],
        );
        assert_eq!(engine_key.class, Some(Class::Refused));
        let reference = shape(
            &[(
                "INSERT INTO links(list, id, item) VALUES(?, ?, ?)",
                vec![I(1), I(1), I(2)],
            )],
            &[("links", vec![I(1), I(1), I(2)])],
        );
        assert_eq!(reference.class, Some(Class::Constrained));
        // Deleting a referenced parent depends on the key; renaming it does not.
        let parent = |sql: &str| {
            shape(
                &[(sql, vec![I(1), I(2)])],
                &[("items", vec![I(1), I(2), I(0), I(0)])],
            )
            .class
        };
        assert_eq!(
            parent("DELETE FROM items WHERE list = ? AND id = ?"),
            Some(Class::Constrained)
        );
        assert_eq!(
            parent("UPDATE items SET title = 'b' WHERE list = ? AND id = ?"),
            Some(Class::Keyed)
        );
    }

    #[test]
    fn coordinated_writes_and_local_only_transactions() {
        let row = [("items", vec![I(1), I(2), I(0), I(0)])];
        let insert = [(
            "INSERT INTO items(list, id, title, done) VALUES(?, ?, 'a', 0)",
            vec![I(1), I(2)],
        )];
        let coordinated = shape_as(Origin::Coordinated, &insert, &row);
        assert_eq!(coordinated.class, Some(Class::Coordinated));
        let aborting = [(
            "INSERT OR ABORT INTO items(list, id, title, done) VALUES(?, ?, 'a', 0)",
            vec![I(1), I(2)],
        )];
        assert_eq!(shape(&aborting, &row).algebra, [Op::FreshInsert].into());
        let replacing = [(
            "INSERT OR REPLACE INTO items(list, id, title, done) VALUES(?, ?, 'a', 0)",
            vec![I(1), I(2)],
        )];
        let replaced = shape(&replacing, &row);
        assert_eq!(
            (replaced.class, replaced.algebra_only),
            (Some(Class::Keyed), false)
        );
        let fresh = shape(&insert, &row);
        assert_eq!(
            (fresh.class, fresh.algebra_only),
            (Some(Class::Keyed), true)
        );
        assert_eq!(fresh.algebra, [Op::FreshInsert].into());
        assert_eq!(shape(&insert, &[]).class, None);
        let moved = shape_as(Origin::Move, &[], &[]);
        assert_eq!(
            (moved.class, moved.partitions, moved.algebra_only),
            (Some(Class::Keyed), 2, false)
        );
    }
}
