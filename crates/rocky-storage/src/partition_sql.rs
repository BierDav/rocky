//! Completeness proofs for each resolved SQL table reference.
use rocky_core::{KeyColumn, PartitionValue, SqlValue};
use sqlparser::{ast::*, dialect::SQLiteDialect, parser::Parser};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::ControlFlow,
};

#[derive(Clone)]
pub struct PartitionColumn {
    pub name: String,
    pub key: KeyColumn,
}
#[derive(Clone)]
pub struct TablePartitions {
    pub columns: Vec<PartitionColumn>,
    pub complete: bool,
    pub unavailable: bool,
    /// Values in partition declaration order; only readable, held scopes.
    pub held: Vec<Vec<SqlValue>>,
    /// The table may hold puppets or a virtual row (`docs/runtime/existence-logs.md`): every
    /// read is filtered to real rows, complete or not.
    pub support: bool,
}
impl TablePartitions {
    /// Whether reads of the table are rewritten to a filtered subquery.
    fn filtered(&self) -> bool {
        self.support || (!self.complete && !self.columns.is_empty())
    }
}
/// Why a statement is refused: outside the supported surface, or a table
/// it reads is not available on this device.
#[derive(Debug)]
pub enum Refusal {
    Unsupported(String),
    Unavailable {
        table: String,
        reason: crate::sql::Unavailable,
    },
}
impl From<String> for Refusal {
    fn from(reason: String) -> Self {
        Self::Unsupported(reason)
    }
}
impl From<&str> for Refusal {
    fn from(reason: &str) -> Self {
        Self::Unsupported(reason.into())
    }
}
impl From<Refusal> for crate::sql::SqlError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Unsupported(reason) => Self::Unsupported(reason),
            Refusal::Unavailable { table, reason } => Self::Unavailable { table, reason },
        }
    }
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(reason) => f.write_str(reason),
            Self::Unavailable { table, reason } => {
                write!(f, "{table} is not available: {reason:?}")
            }
        }
    }
}
fn parse(sql: &str) -> Result<Statement, String> {
    let mut statements =
        Parser::parse_sql(&SQLiteDialect {}, sql).map_err(|error| error.to_string())?;
    if statements.len() != 1 {
        return Err("one statement required".into());
    }
    Ok(statements.remove(0))
}
fn table(factor: &TableFactor) -> Option<(String, String)> {
    let TableFactor::Table { name, alias, .. } = factor else {
        return None;
    };
    let name = match name.0.as_slice() {
        [ObjectNamePart::Identifier(name)] => name,
        [
            ObjectNamePart::Identifier(database),
            ObjectNamePart::Identifier(name),
        ] if database.value.eq_ignore_ascii_case("main") => name,
        _ => return None,
    };
    Some((
        name.value.to_ascii_lowercase(),
        alias.as_ref().map_or_else(
            || name.value.to_ascii_lowercase(),
            |alias| alias.name.value.to_ascii_lowercase(),
        ),
    ))
}
fn unnest(expr: &Expr) -> &Expr {
    if let Expr::Nested(expr) = expr {
        unnest(expr)
    } else {
        expr
    }
}
type Column = (String, String);
fn column(expr: &Expr, aliases: &[(String, String)]) -> Option<Column> {
    match unnest(expr) {
        Expr::Identifier(name) if aliases.len() == 1 => {
            Some((aliases[0].1.clone(), name.value.to_ascii_lowercase()))
        }
        Expr::CompoundIdentifier(parts) if parts.len() == 2 => Some((
            parts[0].value.to_ascii_lowercase(),
            parts[1].value.to_ascii_lowercase(),
        )),
        _ => None,
    }
}
struct Bindings<'a> {
    params: &'a [SqlValue],
    placeholders: BTreeMap<(u64, u64), String>,
    values: BTreeMap<(u64, u64), SqlValue>,
}
impl Bindings<'_> {
    fn bind(&mut self) -> Result<(), String> {
        let mut next = 0usize;
        for (span, name) in &self.placeholders {
            let index = if name == "?" {
                next + 1
            } else {
                name.strip_prefix('?')
                    .and_then(|name| name.parse::<usize>().ok())
                    .ok_or("partition proof needs positional parameters")?
            };
            next = next.max(index);
            if let Some(param) = index
                .checked_sub(1)
                .and_then(|index| self.params.get(index))
            {
                self.values.insert(*span, param.clone());
            }
        }
        Ok(())
    }
}
impl Visitor for Bindings<'_> {
    type Break = String;
    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
        if let Expr::Value(value) = expr
            && let Value::Placeholder(name) = &value.value
        {
            self.placeholders.insert(
                (value.span.start.line, value.span.start.column),
                name.clone(),
            );
        }
        ControlFlow::Continue(())
    }
}
fn value(expr: &Expr, bindings: &Bindings<'_>) -> Option<SqlValue> {
    match unnest(expr) {
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(text) => Some(SqlValue::Text(text.clone())),
            Value::Number(number, _) => number
                .parse::<i64>()
                .ok()
                .map(SqlValue::Integer)
                .or_else(|| number.parse::<f64>().ok().and_then(SqlValue::real)),
            Value::Placeholder(_) => bindings
                .values
                .get(&(value.span.start.line, value.span.start.column))
                .cloned(),
            Value::Boolean(value) => Some(SqlValue::Integer(i64::from(*value))),
            _ => None,
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match value(expr, bindings)? {
            SqlValue::Integer(number) => number.checked_neg().map(SqlValue::Integer),
            SqlValue::Real(bits) => SqlValue::real(-bits.get()),
            _ => None,
        },
        _ => None,
    }
}
#[derive(Default)]
struct Proof {
    values: BTreeMap<Column, Vec<SqlValue>>,
    equal: Vec<(Column, Column)>,
}
fn proof(expr: &Expr, aliases: &[(String, String)], bindings: &Bindings<'_>, to: &mut Proof) {
    match unnest(expr) {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => {
            proof(left, aliases, bindings, to);
            proof(right, aliases, bindings, to);
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } => {
            match (
                column(left, aliases),
                column(right, aliases),
                value(left, bindings),
                value(right, bindings),
            ) {
                (Some(left), Some(right), _, _) => to.equal.push((left, right)),
                (Some(column), None, _, Some(value)) | (None, Some(column), Some(value), _) => {
                    to.values.insert(column, vec![value]);
                }
                _ => {}
            }
        }
        Expr::InList {
            expr,
            list,
            negated: false,
        } => {
            if let Some(column) = column(expr, aliases)
                && let Some(values) = list
                    .iter()
                    .map(|expr| value(expr, bindings))
                    .collect::<Option<Vec<_>>>()
            {
                to.values.insert(column, values);
            }
        }
        _ => {}
    }
}
fn propagate(proof: &mut Proof) {
    for _ in 0..proof.equal.len() + 1 {
        let mut changed = false;
        for (left, right) in &proof.equal {
            if !proof.values.contains_key(right)
                && let Some(values) = proof.values.get(left).cloned()
            {
                proof.values.insert(right.clone(), values);
                changed = true;
            }
            if !proof.values.contains_key(left)
                && let Some(values) = proof.values.get(right).cloned()
            {
                proof.values.insert(left.clone(), values);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}
fn normalize(column: &PartitionColumn, value: &SqlValue) -> Result<PartitionValue, String> {
    PartitionValue::new(column.key, value.clone()).map_err(|error| error.to_string())
}
fn mentions_kind(expr: &Expr) -> bool {
    struct Kind;
    impl Visitor for Kind {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            let kind = match expr {
                Expr::Identifier(name) => name.value.eq_ignore_ascii_case("row_kind"),
                Expr::CompoundIdentifier(names) => names
                    .last()
                    .is_some_and(|name| name.value.eq_ignore_ascii_case("row_kind")),
                _ => false,
            };
            if kind {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    matches!(expr.visit(&mut Kind), ControlFlow::Break(()))
}
struct Checker<'a> {
    tables: &'a BTreeMap<String, TablePartitions>,
    bindings: &'a Bindings<'a>,
}
impl Checker<'_> {
    fn body(&self, body: &SetExpr) -> Result<(), Refusal> {
        match body {
            SetExpr::Select(select) => self.references(&select.from, select.selection.as_ref()),
            SetExpr::SetOperation { left, right, .. } => {
                self.body(left)?;
                self.body(right)
            }
            _ => Ok(()),
        }
    }
    fn references(&self, from: &[TableWithJoins], selection: Option<&Expr>) -> Result<(), Refusal> {
        let mut references = Vec::new();
        for item in from {
            if let Some(reference) = table(&item.relation) {
                references.push(reference);
            }
            for join in &item.joins {
                if let Some(reference) = table(&join.relation) {
                    references.push(reference);
                }
            }
        }
        let mut where_proof = Proof::default();
        if let Some(selection) = selection {
            proof(selection, &references, self.bindings, &mut where_proof);
        }
        // Inner join conditions constrain all participants. Outer ON clauses only constrain the non-preserved input.
        for item in from {
            for join in &item.joins {
                if let JoinOperator::Join(JoinConstraint::On(on))
                | JoinOperator::Inner(JoinConstraint::On(on)) = &join.join_operator
                {
                    proof(on, &references, self.bindings, &mut where_proof);
                }
            }
        }
        propagate(&mut where_proof);
        for (name, alias) in &references {
            let Some(rule) = self.tables.get(name) else {
                continue;
            };
            if rule.unavailable {
                return Err(Refusal::Unavailable {
                    table: name.clone(),
                    reason: crate::sql::Unavailable::Incomplete,
                });
            }
            if rule.complete || (rule.columns.is_empty() && !rule.support) {
                continue;
            }
            let mut effective = Proof {
                values: where_proof.values.clone(),
                equal: where_proof.equal.clone(),
            };
            for item in from {
                for join in &item.joins {
                    if table(&join.relation).is_some_and(|(_, target)| &target == alias)
                        && let JoinOperator::Left(JoinConstraint::On(on))
                        | JoinOperator::LeftOuter(JoinConstraint::On(on)) = &join.join_operator
                    {
                        proof(on, &references, self.bindings, &mut effective);
                    }
                }
            }
            propagate(&mut effective);
            if effective
                .values
                .get(&(alias.clone(), "row_kind".into()))
                .is_some_and(|values| values == &[SqlValue::Integer(0)])
            {
                continue;
            }
            if rule.columns.is_empty() {
                return Err(format!(
                    "{name}: a table this device does not hold needs row_kind = 0"
                )
                .into());
            }
            let mut tuples = vec![Vec::new()];
            for column in &rule.columns {
                let values=effective.values.get(&(alias.clone(),column.name.to_ascii_lowercase())).ok_or_else(|| format!("{name}: a partial table needs held partition equality/IN or row_kind = 0"))?;
                if tuples.len().saturating_mul(values.len()) > 4096 {
                    return Err("partition proof quota".into());
                }
                let mut next = Vec::new();
                for tuple in tuples {
                    for value in values {
                        let mut tuple = tuple.clone();
                        tuple.push(normalize(column, value)?);
                        next.push(tuple);
                    }
                }
                tuples = next;
            }
            let held = rule
                .held
                .iter()
                .map(|tuple| {
                    rule.columns
                        .iter()
                        .zip(tuple)
                        .map(|(column, value)| normalize(column, value))
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<Vec<_>, _>>()?;
            if tuples.iter().any(|tuple| !held.contains(tuple)) {
                return Err(Refusal::Unavailable {
                    table: name.clone(),
                    reason: crate::sql::Unavailable::NotHeld,
                });
            }
        }
        Ok(())
    }
}
impl Visitor for Checker<'_> {
    type Break = Refusal;
    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Refusal> {
        if let TableFactor::NestedJoin {
            table_with_joins, ..
        } = factor
            && let Err(error) = self.references(std::slice::from_ref(table_with_joins), None)
        {
            return ControlFlow::Break(error);
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Refusal> {
        if let Err(error) = self.body(&query.body) {
            return ControlFlow::Break(error);
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Refusal> {
        let result = match statement {
            Statement::Update(update) => {
                let mut sources = vec![update.table.clone()];
                if let Some(
                    UpdateTableFromKind::BeforeSet(from) | UpdateTableFromKind::AfterSet(from),
                ) = &update.from
                {
                    sources.extend(from.iter().cloned());
                }
                self.references(&sources, update.selection.as_ref())
            }
            Statement::Delete(delete) => {
                let (FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from)) =
                    &delete.from;
                self.references(from, delete.selection.as_ref())
            }
            _ => Ok(()),
        };
        match result {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    }
    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Refusal> {
        let is_kind = |expr: &Expr| match unnest(expr) {
            Expr::Identifier(name) => name.value.eq_ignore_ascii_case("row_kind"),
            Expr::CompoundIdentifier(names) => names
                .last()
                .is_some_and(|name| name.value.eq_ignore_ascii_case("row_kind")),
            _ => false,
        };
        let refused = match unnest(expr) {
            Expr::BinaryOp { left, op, right } if is_kind(left) || is_kind(right) => {
                let other = if is_kind(left) { right } else { left };
                *op != BinaryOperator::Eq
                    || value(other, self.bindings) != Some(SqlValue::Integer(0))
            }
            Expr::InList {
                expr,
                list,
                negated,
            } if is_kind(expr) => {
                *negated
                    || list
                        .iter()
                        .any(|expr| value(expr, self.bindings) != Some(SqlValue::Integer(0)))
            }
            // The subquery is checked as its own query block.
            Expr::InSubquery { expr, .. } => mentions_kind(expr),
            Expr::Identifier(_)
            | Expr::CompoundIdentifier(_)
            | Expr::Value(_)
            | Expr::Nested(_)
            | Expr::BinaryOp { .. }
            | Expr::Subquery(_)
            | Expr::Exists { .. } => false,
            other => mentions_kind(other),
        };
        if refused {
            return ControlFlow::Break("reserved row_kind cannot be selected".into());
        }
        ControlFlow::Continue(())
    }
}
/// Checks every query block separately. The returned statement filters partial reads to held scopes.
pub fn check(
    sql: &str,
    params: &[SqlValue],
    tables: &BTreeMap<String, TablePartitions>,
) -> Result<String, Refusal> {
    let mut statement = parse(sql)?;
    let mut bindings = Bindings {
        params,
        placeholders: BTreeMap::new(),
        values: BTreeMap::new(),
    };
    if let ControlFlow::Break(error) = statement.visit(&mut bindings) {
        return Err(error.into());
    }
    bindings.bind()?;
    let mut checker = Checker {
        tables,
        bindings: &bindings,
    };
    if let ControlFlow::Break(error) = statement.visit(&mut checker) {
        return Err(error);
    }
    let mut filter = Filter {
        tables,
        reads: Vec::new(),
    };
    if let ControlFlow::Break(error) = VisitMut::visit(&mut statement, &mut filter) {
        return Err(error.into());
    }
    Ok(statement.to_string())
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn literal(value: &SqlValue) -> String {
    match value {
        SqlValue::Null => "NULL".into(),
        SqlValue::Integer(value) => value.to_string(),
        SqlValue::Real(value) => value.get().to_string(),
        SqlValue::Text(value) => format!("'{}'", value.replace('\'', "''")),
        SqlValue::Blob(value) => format!(
            "X'{}'",
            value
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
    }
}
fn held_filter(rule: &TablePartitions, alias: Option<&str>) -> String {
    let partitions = held_partitions(rule, alias);
    if !rule.support {
        return partitions;
    }
    let kind = match alias {
        Some(alias) => format!("{}.\"row_kind\"", quoted(alias)),
        None => "\"row_kind\"".into(),
    };
    if rule.complete {
        format!("{kind} = 0")
    } else {
        format!("{kind} = 0 AND {partitions}")
    }
}
fn held_partitions(rule: &TablePartitions, alias: Option<&str>) -> String {
    if rule.held.is_empty() || rule.columns.is_empty() {
        return "0".into();
    }
    let names = rule
        .columns
        .iter()
        .map(|column| match alias {
            Some(alias) => format!("{}.{}", quoted(alias), quoted(&column.name)),
            None => quoted(&column.name),
        })
        .collect::<Vec<_>>();
    let values = rule
        .held
        .iter()
        .map(|tuple| {
            let values = tuple.iter().map(literal).collect::<Vec<_>>().join(",");
            if names.len() > 1 {
                format!("({values})")
            } else {
                values
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let names = if names.len() > 1 {
        format!("({})", names.join(","))
    } else {
        names[0].clone()
    };
    format!("{names} IN ({values})")
}
fn filter_expr(rule: &TablePartitions, alias: Option<&str>) -> Result<Expr, String> {
    let Statement::Query(query) = parse(&format!("SELECT 1 WHERE {}", held_filter(rule, alias)))?
    else {
        return Err("partition filter".into());
    };
    let SetExpr::Select(select) = *query.body else {
        return Err("partition filter".into());
    };
    select.selection.ok_or_else(|| "partition filter".into())
}
fn append(selection: &mut Option<Expr>, filter: Expr) {
    *selection = Some(match selection.take() {
        Some(old) => Expr::BinaryOp {
            left: Box::new(Expr::Nested(Box::new(old))),
            op: BinaryOperator::And,
            right: Box::new(Expr::Nested(Box::new(filter))),
        },
        None => filter,
    });
}
struct Filter<'a> {
    tables: &'a BTreeMap<String, TablePartitions>,
    reads: Vec<(u64, u64)>,
}
impl Filter<'_> {
    fn read_factor(&mut self, factor: &TableFactor) {
        match factor {
            TableFactor::Table { name, .. } => {
                if let Some(ObjectNamePart::Identifier(name)) = name.0.last() {
                    self.reads
                        .push((name.span.start.line, name.span.start.column));
                }
            }
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => self.sources(std::slice::from_ref(table_with_joins)),
            _ => {}
        }
    }
    fn sources(&mut self, from: &[TableWithJoins]) {
        for item in from {
            self.read_factor(&item.relation);
            for join in &item.joins {
                self.read_factor(&join.relation);
            }
        }
    }
    fn body(&mut self, body: &SetExpr) {
        match body {
            SetExpr::Select(select) => self.sources(&select.from),
            SetExpr::SetOperation { left, right, .. } => {
                self.body(left);
                self.body(right);
            }
            _ => {}
        }
    }
}
impl VisitorMut for Filter<'_> {
    type Break = String;
    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<String> {
        self.body(&query.body);
        ControlFlow::Continue(())
    }
    fn post_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<String> {
        let Some((name, alias_name)) = table(factor) else {
            return ControlFlow::Continue(());
        };
        let Some(rule) = self.tables.get(&name).filter(|rule| rule.filtered()) else {
            return ControlFlow::Continue(());
        };
        let TableFactor::Table {
            name: object,
            alias,
            ..
        } = factor
        else {
            return ControlFlow::Continue(());
        };
        let Some(ObjectNamePart::Identifier(ident)) = object.0.last() else {
            return ControlFlow::Continue(());
        };
        if !self
            .reads
            .contains(&(ident.span.start.line, ident.span.start.column))
        {
            return ControlFlow::Continue(());
        }
        let query = match parse(&format!(
            "SELECT * FROM {} WHERE {}",
            quoted(&name),
            held_filter(rule, None)
        )) {
            Ok(Statement::Query(query)) => query,
            _ => return ControlFlow::Break("partition filter".into()),
        };
        let alias = alias.clone().or_else(|| {
            Some(TableAlias {
                explicit: true,
                name: Ident::new(alias_name),
                columns: Vec::new(),
                at: None,
            })
        });
        *factor = TableFactor::Derived {
            lateral: false,
            sample: None,
            subquery: query,
            alias,
        };
        ControlFlow::Continue(())
    }
    fn pre_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<String> {
        if let Statement::Update(update) = statement
            && let Some(UpdateTableFromKind::BeforeSet(from) | UpdateTableFromKind::AfterSet(from)) =
                &update.from
        {
            self.sources(from);
        }
        let target = match statement {
            Statement::Update(update) => {
                table(&update.table.relation).map(|reference| (reference, &mut update.selection))
            }
            Statement::Delete(delete) => {
                let (FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from)) =
                    &delete.from;
                from.first()
                    .and_then(|item| table(&item.relation))
                    .map(|reference| (reference, &mut delete.selection))
            }
            _ => None,
        };
        if let Some(((name, alias), selection)) = target
            && let Some(rule) = self.tables.get(&name).filter(|rule| rule.filtered())
        {
            match filter_expr(rule, Some(&alias)) {
                Ok(filter) => append(selection, filter),
                Err(error) => return ControlFlow::Break(error),
            }
        }
        ControlFlow::Continue(())
    }
}

pub fn check_with_views(
    sql: &str,
    params: &[SqlValue],
    tables: &BTreeMap<String, TablePartitions>,
    views: &[rocky_core::LogicalView],
) -> Result<String, Refusal> {
    check(&expand_views(sql, views)?.to_string(), params, tables)
}

/// Conservative table references, after expanding views, for loading only the
/// readiness proofs a statement can use. CTE names may also be returned.
pub fn referenced_tables(
    sql: &str,
    views: &[rocky_core::LogicalView],
) -> Result<BTreeSet<String>, Refusal> {
    struct References(BTreeSet<String>);
    impl Visitor for References {
        type Break = ();
        fn pre_visit_relation(&mut self, name: &ObjectName) -> ControlFlow<()> {
            if let Some(ObjectNamePart::Identifier(name)) = name.0.last() {
                self.0.insert(name.value.to_ascii_lowercase());
            }
            ControlFlow::Continue(())
        }
    }
    let mut references = References(BTreeSet::new());
    let _ = expand_views(sql, views)?.visit(&mut references);
    Ok(references.0)
}

fn expand_views(sql: &str, views: &[rocky_core::LogicalView]) -> Result<Statement, Refusal> {
    struct Expand<'a> {
        views: &'a [rocky_core::LogicalView],
        changed: bool,
    }
    impl VisitorMut for Expand<'_> {
        type Break = String;
        fn post_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<String> {
            let Some((name, alias_name)) = table(factor) else {
                return ControlFlow::Continue(());
            };
            let Some(view) = self
                .views
                .iter()
                .find(|view| view.name.eq_ignore_ascii_case(&name))
            else {
                return ControlFlow::Continue(());
            };
            let query = match parse(&view.create_sql) {
                Ok(Statement::CreateView(view)) if view.columns.is_empty() => view.query,
                _ => return ControlFlow::Break(format!("unsupported view {name}")),
            };
            let TableFactor::Table { alias, .. } = factor else {
                return ControlFlow::Continue(());
            };
            let alias = alias.clone().or_else(|| {
                Some(TableAlias {
                    explicit: true,
                    name: Ident::new(alias_name),
                    columns: Vec::new(),
                    at: None,
                })
            });
            *factor = TableFactor::Derived {
                lateral: false,
                subquery: query,
                alias,
                sample: None,
            };
            self.changed = true;
            ControlFlow::Continue(())
        }
    }
    let mut statement = parse(sql)?;
    for _ in 0..16 {
        let mut expand = Expand {
            views,
            changed: false,
        };
        if let ControlFlow::Break(error) = VisitMut::visit(&mut statement, &mut expand) {
            return Err(error.into());
        }
        if !expand.changed {
            return Ok(statement);
        }
    }
    Err("view expansion cycle or depth quota".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_references_include_nested_views_subqueries_and_write_targets() {
        let views = vec![
            rocky_core::LogicalView {
                name: "inner_items".into(),
                create_sql: "CREATE VIEW inner_items AS SELECT * FROM main.items".into(),
            },
            rocky_core::LogicalView {
                name: "outer_items".into(),
                create_sql: "CREATE VIEW outer_items AS SELECT * FROM inner_items".into(),
            },
        ];
        for sql in [
            "SELECT * FROM metadata WHERE EXISTS(SELECT 1 FROM outer_items)",
            "UPDATE metadata SET value=(SELECT count(*) FROM outer_items)",
            "INSERT INTO metadata SELECT * FROM outer_items",
            "DELETE FROM metadata WHERE EXISTS(SELECT 1 FROM outer_items)",
        ] {
            assert_eq!(
                referenced_tables(sql, &views).unwrap(),
                ["items".into(), "metadata".into()].into(),
                "{sql}"
            );
        }
        assert!(referenced_tables("SELECT 1", &views).unwrap().is_empty());
        assert_eq!(
            referenced_tables("SELECT * FROM metadata", &views).unwrap(),
            ["metadata".into()].into()
        );
    }

    fn rules() -> BTreeMap<String, TablePartitions> {
        [(
            "items".into(),
            TablePartitions {
                columns: vec![PartitionColumn {
                    name: "list".into(),
                    key: KeyColumn {
                        affinity: rocky_core::Affinity::Text,
                        collation: rocky_core::Collation::Nocase,
                    },
                }],
                complete: false,
                unavailable: false,
                held: vec![vec![SqlValue::Text("A".into())]],
                support: false,
            },
        )]
        .into()
    }
    #[test]
    fn aliases_in_bindings_or_and_outer_join_proofs_are_checked() {
        let rules = rules();
        for sql in [
            "SELECT * FROM items WHERE list='a'",
            "SELECT * FROM items WHERE list IN ('a','A')",
            "UPDATE items SET amount=1 WHERE list='A'",
            "DELETE FROM items WHERE row_kind=0",
            "SELECT SUM(amount) FROM main.items WHERE row_kind=0",
            "SELECT amount FROM items WHERE list='A' UNION ALL SELECT amount FROM items WHERE row_kind=0",
            "SELECT a.amount,b.amount FROM items a JOIN items b ON a.list=b.list WHERE a.list='A'",
            "SELECT a.amount,b.amount FROM items a LEFT JOIN items b ON b.row_kind=0 WHERE a.list='A'",
        ] {
            check(sql, &[], &rules).unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        for sql in [
            "SELECT SUM(amount) FROM items",
            "SELECT * FROM items WHERE list IN ('A','B')",
            "SELECT * FROM items WHERE list='A' OR amount=1",
            "SELECT * FROM items WHERE row_kind=0 OR amount=1",
            "SELECT a.amount,b.amount FROM items a LEFT JOIN items b ON a.row_kind=0 AND b.row_kind=0",
            "UPDATE items SET amount=1",
            "DELETE FROM items WHERE list='B'",
            "SELECT * FROM items WHERE row_kind=2",
            "SELECT SUM(amount) FROM main.items",
            "SELECT amount FROM items WHERE list='A' UNION ALL SELECT amount FROM items",
            "UPDATE items AS a SET amount=b.amount FROM items AS b WHERE a.list='A'",
            "SELECT SUM(amount) FROM (items)",
            "SELECT * FROM items WHERE row_kind IN (1,2)",
            "SELECT * FROM items WHERE list='A' AND coalesce(row_kind,0)=1",
            "SELECT * FROM items WHERE list='A' AND NOT(row_kind=0)",
            "SELECT * FROM items WHERE row_kind != 0",
        ] {
            assert!(check(sql, &[], &rules).is_err(), "{sql}");
        }
        check(
            "SELECT ?2 FROM items WHERE list=?1",
            &[SqlValue::Text("A".into()), SqlValue::Integer(4)],
            &rules,
        )
        .unwrap();
        assert!(
            check(
                "SELECT ? FROM items WHERE list=?",
                &[SqlValue::Text("A".into()), SqlValue::Text("B".into())],
                &rules
            )
            .is_err()
        );
    }
    #[test]
    fn partial_reads_filter_cached_unheld_rows_without_changing_left_join_semantics() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE items(list TEXT,amount INTEGER,row_kind INTEGER); INSERT INTO items VALUES('A',10,0),('B',20,0);").unwrap();
        let sql = check(
            "SELECT SUM(amount) FROM items WHERE row_kind=0",
            &[],
            &rules(),
        )
        .unwrap();
        assert_eq!(
            conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            10
        );
        let qualified = check(
            "SELECT SUM(amount) FROM main.items WHERE row_kind=0",
            &[],
            &rules(),
        )
        .unwrap();
        assert_eq!(
            conn.query_row(&qualified, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            10
        );
        let union = check("SELECT amount FROM items WHERE row_kind=0 UNION ALL SELECT amount FROM items WHERE row_kind=0", &[], &rules()).unwrap();
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM ({union})"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
        let sql=check("SELECT a.amount,b.amount FROM items a LEFT JOIN items b ON b.list='B' AND b.row_kind=0 WHERE a.list='A'",&[],&rules()).unwrap();
        assert_eq!(
            conn.query_row(&sql, [], |row| Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?
            )))
            .unwrap(),
            (10, None)
        );
    }
    /// `customers` may hold support rows: unheld by `partial`, fully held
    /// otherwise.
    fn support(complete: bool) -> BTreeMap<String, TablePartitions> {
        [(
            "customers".into(),
            TablePartitions {
                columns: Vec::new(),
                complete,
                unavailable: false,
                held: Vec::new(),
                support: true,
            },
        )]
        .into()
    }
    #[test]
    fn support_rows_are_unreadable_through_every_statement_form() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE customers(id INTEGER PRIMARY KEY,name TEXT NOT NULL,row_kind INTEGER NOT NULL);
             CREATE TABLE orders(id INTEGER PRIMARY KEY,customer INTEGER NOT NULL,row_kind INTEGER NOT NULL);
             INSERT INTO customers VALUES(0,'',2),(7,'',1);
             INSERT INTO orders VALUES(1,7,0);",
        )
        .unwrap();
        let views = vec![
            rocky_core::LogicalView {
                name: "partial_customers".into(),
                create_sql:
                    "CREATE VIEW partial_customers AS SELECT * FROM customers WHERE row_kind = 0"
                        .into(),
            },
            rocky_core::LogicalView {
                name: "nested_customers".into(),
                create_sql: "CREATE VIEW nested_customers AS SELECT * FROM partial_customers"
                    .into(),
            },
            rocky_core::LogicalView {
                name: "all_customers".into(),
                create_sql: "CREATE VIEW all_customers AS SELECT * FROM customers".into(),
            },
        ];
        let unheld = support(false);
        for sql in [
            "SELECT * FROM customers",
            "SELECT count(*) FROM customers",
            "SELECT * FROM customers WHERE id=7",
            "SELECT * FROM customers WHERE row_kind=1",
            "SELECT * FROM customers WHERE row_kind IN (0,1)",
            "SELECT * FROM customers WHERE row_kind=0 OR id=7",
            "SELECT o.id,c.name FROM orders o LEFT JOIN customers c ON c.id=o.customer",
            "SELECT o.id,c.name FROM orders o JOIN customers c ON c.id=o.customer",
            "SELECT * FROM all_customers",
            "SELECT id FROM orders WHERE customer IN (SELECT id FROM customers)",
            "SELECT id FROM orders UNION SELECT id FROM customers",
            "UPDATE customers SET name='x' WHERE id=7",
            "DELETE FROM customers WHERE id=7",
        ] {
            assert!(
                check_with_views(sql, &[], &unheld, &views).is_err(),
                "{sql}"
            );
        }
        let read = |sql: &str, rules: &BTreeMap<String, TablePartitions>| {
            let sql = check_with_views(sql, &[], rules, &views)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            let mut statement = conn.prepare(&sql).unwrap();
            let width = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..width)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        use rusqlite::types::Value::{Integer, Null};
        // orders LEFT JOIN partial_customers: NULL for the puppet's columns.
        for sql in [
            "SELECT o.id,c.name FROM orders o LEFT JOIN partial_customers c ON c.id=o.customer",
            "SELECT o.id,c.name FROM orders o LEFT JOIN nested_customers c ON c.id=o.customer",
            "SELECT o.id,c.name FROM orders o LEFT JOIN customers c ON c.id=o.customer AND c.row_kind=0",
        ] {
            assert_eq!(read(sql, &unheld), vec![vec![Integer(1), Null]], "{sql}");
        }
        for sql in [
            "SELECT * FROM customers WHERE row_kind=0",
            "SELECT * FROM partial_customers",
            "SELECT id FROM orders WHERE customer IN (SELECT id FROM partial_customers)",
        ] {
            assert!(read(sql, &unheld).is_empty(), "{sql}");
        }
        // A fully held table reads its real rows only, also without a filter.
        let held = support(true);
        for sql in [
            "SELECT * FROM customers",
            "SELECT * FROM all_customers",
            "SELECT * FROM customers WHERE id IN (0,7)",
            "SELECT c.* FROM orders o JOIN customers c ON c.id=o.customer",
        ] {
            assert!(read(sql, &held).is_empty(), "{sql}");
        }
        for sql in [
            "SELECT * FROM customers WHERE row_kind=2",
            "SELECT * FROM customers WHERE row_kind<>0",
            "SELECT * FROM customers WHERE row_kind IN (SELECT 1)",
        ] {
            assert!(check_with_views(sql, &[], &held, &views).is_err(), "{sql}");
        }
        for sql in ["UPDATE customers SET name='x'", "DELETE FROM customers"] {
            let sql = check_with_views(sql, &[], &held, &views).unwrap();
            assert!(sql.contains("row_kind"), "{sql}");
            conn.execute(&sql, []).unwrap();
        }
        let left: i64 = conn
            .query_row("SELECT count(*) FROM customers WHERE name=''", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(left, 2);
    }
    #[test]
    fn nested_views_are_expanded_and_cycles_refused() {
        let views = vec![
            rocky_core::LogicalView {
                name: "partial_items".into(),
                create_sql: "CREATE VIEW partial_items AS SELECT * FROM items WHERE row_kind=0"
                    .into(),
            },
            rocky_core::LogicalView {
                name: "nested".into(),
                create_sql: "CREATE VIEW nested AS SELECT * FROM partial_items".into(),
            },
        ];
        check_with_views("SELECT SUM(amount) FROM nested", &[], &rules(), &views).unwrap();
        let views = vec![rocky_core::LogicalView {
            name: "cycle".into(),
            create_sql: "CREATE VIEW cycle AS SELECT * FROM cycle".into(),
        }];
        assert!(check_with_views("SELECT * FROM cycle", &[], &rules(), &views).is_err());
    }
}
