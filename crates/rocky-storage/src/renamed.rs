use rocky_core::LogicalSchema;
use sqlparser::ast::*;
use std::{collections::BTreeMap, ops::ControlFlow};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Renames {
    tables: BTreeMap<String, String>,
    columns: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameRefusal {
    Unparsed(String),
    Ambiguous(String),
}
impl std::fmt::Display for RenameRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unparsed(reason) => write!(f, "the statement does not parse: {reason}"),
            Self::Ambiguous(name) => {
                write!(f, "{name} names renamed columns of more than one table")
            }
        }
    }
}
impl std::error::Error for RenameRefusal {}

impl Renames {
    pub fn of(schema: &LogicalSchema) -> Self {
        let mut renames = Self::default();
        for table in &schema.tables {
            for earlier in &table.earlier_names {
                renames
                    .tables
                    .insert(earlier.to_ascii_lowercase(), table.sql_name.clone());
            }
            let columns: BTreeMap<_, _> = table
                .columns
                .iter()
                .flat_map(|column| {
                    column
                        .earlier_names
                        .iter()
                        .map(|earlier| (earlier.to_ascii_lowercase(), column.sql_name.clone()))
                })
                .collect();
            if !columns.is_empty() {
                renames
                    .columns
                    .insert(table.sql_name.to_ascii_lowercase(), columns);
            }
        }
        renames
    }
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.columns.is_empty()
    }
    pub fn apply(&self, sql: &str) -> Result<String, RenameRefusal> {
        if self.is_empty() {
            return Ok(sql.to_owned());
        }
        let mut statement = match &*crate::parsed::parse(sql).map_err(RenameRefusal::Unparsed)? {
            [statement] => statement.clone(),
            _ => return Err(RenameRefusal::Unparsed("one statement required".into())),
        };
        let mut scope = Scope {
            renames: self,
            names: BTreeMap::new(),
            target: None,
        };
        let _ = Visit::visit(&statement, &mut scope);
        scope.target = target(&statement).map(|name| self.current(&name));
        if let Some(target) = &scope.target {
            scope
                .names
                .insert(target.to_ascii_lowercase(), target.clone());
            scope.names.insert("excluded".into(), target.clone());
        }
        rename_targets(&mut statement, &scope);
        if let ControlFlow::Break(refusal) = VisitMut::visit(&mut statement, &mut scope) {
            return Err(refusal);
        }
        Ok(statement.to_string())
    }
    fn current(&self, table: &str) -> String {
        self.tables
            .get(&table.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| table.to_owned())
    }
    fn column(&self, table: &str, column: &str) -> Option<&String> {
        self.columns
            .get(&table.to_ascii_lowercase())?
            .get(&column.to_ascii_lowercase())
    }
}

struct Scope<'a> {
    renames: &'a Renames,
    names: BTreeMap<String, String>,
    target: Option<String>,
}
impl Scope<'_> {
    fn table_of(&self, qualifier: &Ident) -> Option<&String> {
        self.names.get(&qualifier.value.to_ascii_lowercase())
    }
    fn rename_in(&self, table: Option<&String>, column: &mut Ident) {
        if let Some(current) = table.and_then(|table| self.renames.column(table, &column.value)) {
            column.value.clone_from(current);
        }
    }
    fn rename_unqualified(&self, column: &mut Ident) -> ControlFlow<RenameRefusal> {
        let mut found: Vec<&String> = self
            .names
            .values()
            .filter_map(|table| self.renames.column(table, &column.value))
            .collect();
        found.sort();
        found.dedup();
        match found.as_slice() {
            [] => ControlFlow::Continue(()),
            [current] => {
                column.value.clone_from(current);
                ControlFlow::Continue(())
            }
            _ => ControlFlow::Break(RenameRefusal::Ambiguous(column.value.clone())),
        }
    }
}
fn last(name: &mut ObjectName) -> Option<&mut Ident> {
    match name.0.last_mut() {
        Some(ObjectNamePart::Identifier(ident)) => Some(ident),
        _ => None,
    }
}
fn target(statement: &Statement) -> Option<String> {
    let name = match statement {
        Statement::Insert(insert) => match &insert.table {
            TableObject::TableName(name) => name,
            _ => return None,
        },
        Statement::Update(update) => match &update.table.relation {
            TableFactor::Table { name, .. } => name,
            _ => return None,
        },
        _ => return None,
    };
    match name.0.last() {
        Some(ObjectNamePart::Identifier(ident)) => Some(ident.value.clone()),
        _ => None,
    }
}
fn rename_assignments(assignments: &mut [Assignment], scope: &Scope<'_>) {
    for assignment in assignments {
        let columns = match &mut assignment.target {
            AssignmentTarget::ColumnName(name) => std::slice::from_mut(name),
            AssignmentTarget::Tuple(names) => names.as_mut_slice(),
        };
        for name in columns {
            if let Some(column) = last(name) {
                scope.rename_in(scope.target.as_ref(), column);
            }
        }
    }
}
fn rename_targets(statement: &mut Statement, scope: &Scope<'_>) {
    match statement {
        Statement::Insert(insert) => {
            for name in &mut insert.columns {
                if let Some(column) = last(name) {
                    scope.rename_in(scope.target.as_ref(), column);
                }
            }
            if let Some(OnInsert::OnConflict(conflict)) = &mut insert.on {
                if let Some(ConflictTarget::Columns(columns)) = &mut conflict.conflict_target {
                    for column in columns {
                        scope.rename_in(scope.target.as_ref(), column);
                    }
                }
                if let OnConflictAction::DoUpdate(update) = &mut conflict.action {
                    rename_assignments(&mut update.assignments, scope);
                }
            }
        }
        Statement::Update(update) => rename_assignments(&mut update.assignments, scope),
        _ => {}
    }
}
impl Visitor for Scope<'_> {
    type Break = ();
    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table { name, alias, .. } = factor
            && let Some(ObjectNamePart::Identifier(ident)) = name.0.last()
        {
            let current = self.renames.current(&ident.value);
            self.names
                .insert(ident.value.to_ascii_lowercase(), current.clone());
            self.names
                .insert(current.to_ascii_lowercase(), current.clone());
            if let Some(alias) = alias {
                self.names
                    .insert(alias.name.value.to_ascii_lowercase(), current);
            }
        }
        ControlFlow::Continue(())
    }
}
impl VisitorMut for Scope<'_> {
    type Break = RenameRefusal;
    fn pre_visit_relation(&mut self, relation: &mut ObjectName) -> ControlFlow<RenameRefusal> {
        if let Some(table) = last(relation) {
            table.value = self.renames.current(&table.value);
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<RenameRefusal> {
        match expr {
            Expr::Identifier(column) => self.rename_unqualified(column),
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                let split = parts.len() - 2;
                let (qualifier, column) = parts[split..].split_at_mut(1);
                let table = self.table_of(&qualifier[0]).cloned();
                if self
                    .renames
                    .tables
                    .contains_key(&qualifier[0].value.to_ascii_lowercase())
                    && let Some(current) = &table
                {
                    qualifier[0].value.clone_from(current);
                }
                self.rename_in(table.as_ref(), &mut column[0]);
                ControlFlow::Continue(())
            }
            _ => ControlFlow::Continue(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renames() -> Renames {
        Renames {
            tables: BTreeMap::from([("items".into(), "entries".into())]),
            columns: BTreeMap::from([
                (
                    "entries".into(),
                    BTreeMap::from([
                        ("title".into(), "name".into()),
                        ("label".into(), "name".into()),
                    ]),
                ),
                (
                    "lists".into(),
                    BTreeMap::from([("caption".into(), "heading".into())]),
                ),
            ]),
        }
    }
    fn renamed(sql: &str) -> String {
        renames().apply(sql).unwrap()
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn an_old_statement_names_the_current_table_and_columns() {
        assert_eq!(
            renamed("INSERT INTO items(id, title) VALUES(?, ?)"),
            "INSERT INTO entries (id, name) VALUES (?, ?)"
        );
        assert_eq!(
            renamed("UPDATE items SET label = upper(title) WHERE items.title = 'a'"),
            "UPDATE entries SET name = upper(name) WHERE entries.name = 'a'"
        );
        assert_eq!(
            renamed(
                "DELETE FROM items WHERE title IN (SELECT heading FROM lists l WHERE l.caption = items.title)"
            ),
            "DELETE FROM entries WHERE name IN (SELECT heading FROM lists l WHERE l.heading = entries.name)"
        );
        assert_eq!(
            renamed(
                "INSERT INTO items(id, title) VALUES(?, ?) ON CONFLICT(title) DO UPDATE SET title = excluded.title"
            ),
            "INSERT INTO entries (id, name) VALUES (?, ?) ON CONFLICT(name) DO UPDATE SET name = excluded.name"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn current_names_and_other_tables_are_left_alone() {
        assert_eq!(
            renamed("UPDATE notes SET title = 'x' WHERE body = ?"),
            "UPDATE notes SET title = 'x' WHERE body = ?"
        );
        assert_eq!(
            Renames::default()
                .apply("UPDATE  items SET title=1")
                .unwrap(),
            "UPDATE  items SET title=1",
            "a schema without renames leaves the statement as written"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_name_two_tables_renamed_differently_is_refused() {
        let mut renames = renames();
        renames.columns.insert(
            "lists".into(),
            BTreeMap::from([("title".into(), "heading".into())]),
        );
        assert_eq!(
            renames.apply("DELETE FROM items WHERE EXISTS (SELECT 1 FROM lists WHERE title = 'a')"),
            Err(RenameRefusal::Ambiguous("title".into()))
        );
    }
}
