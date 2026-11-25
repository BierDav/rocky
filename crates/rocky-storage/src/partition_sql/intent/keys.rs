use super::{Bindings, Checked, IntentRefusal, SplitError, prepare, target, value};
use rocky_core::SqlValue;
use sqlparser::ast::*;
use std::{collections::BTreeMap, ops::ControlFlow};

#[derive(Clone, Debug)]
pub struct GeneratedTable {
    pub partition_by: Vec<String>,
    pub insertable: Vec<String>,
}

fn named(name: &ObjectName) -> String {
    match name.0.last() {
        Some(ObjectNamePart::Identifier(ident)) => ident.value.to_ascii_lowercase(),
        _ => name.to_string().to_ascii_lowercase(),
    }
}

pub fn check_key_writes(
    sql: &str,
    views: &[rocky_core::LogicalView],
    key_of: impl Fn(&str) -> Option<Vec<String>>,
) -> Result<(), IntentRefusal> {
    let prepared = prepare(sql, views)?;
    let Ok((table, _)) = target(&prepared.statement) else {
        return Ok(());
    };
    let Some(key) = key_of(&table) else {
        return Ok(());
    };
    let assignments: &[Assignment] = match &prepared.statement {
        Statement::Update(update) => &update.assignments,
        Statement::Insert(Insert {
            on:
                Some(OnInsert::OnConflict(OnConflict {
                    action: OnConflictAction::DoUpdate(update),
                    ..
                })),
            ..
        }) => &update.assignments,
        _ => &[],
    };
    for assignment in assignments {
        let targets = match &assignment.target {
            AssignmentTarget::ColumnName(name) => std::slice::from_ref(name),
            AssignmentTarget::Tuple(names) => names.as_slice(),
        };
        if let Some(column) = targets
            .iter()
            .map(named)
            .find(|column| key.iter().any(|key| key.eq_ignore_ascii_case(column)))
        {
            return Err(IntentRefusal::KeyUpdate { table, column });
        }
    }
    Ok(())
}

pub fn fill_generated_ids<E>(
    sql: &str,
    params: &[SqlValue],
    views: &[rocky_core::LogicalView],
    generated: impl Fn(&str) -> Option<GeneratedTable>,
    mut allocate: impl FnMut(&[SqlValue]) -> Result<Vec<u8>, E>,
) -> Result<Option<Checked>, SplitError<E>> {
    let prepared = prepare(sql, views)?;
    let mut statement = prepared.statement.clone();
    let Statement::Insert(insert) = &mut statement else {
        return Ok(None);
    };
    let Ok((table, _)) = target(&Statement::Insert(insert.clone())) else {
        return Ok(None);
    };
    let Some(shape) = generated(&table) else {
        return Ok(None);
    };
    let id = rocky_core::GENERATED_ID;
    if insert
        .columns
        .iter()
        .any(|column| named(column).eq_ignore_ascii_case(id))
    {
        return Ok(None);
    }
    if shape
        .partition_by
        .iter()
        .any(|column| column.eq_ignore_ascii_case(id))
    {
        return Err(IntentRefusal::Unsupported(format!(
            "{table} is partitioned by its own id, so an INSERT names the id that next_id allocated"
        ))
        .into());
    }
    let mut bindings = Bindings {
        params,
        placeholders: BTreeMap::new(),
        values: BTreeMap::new(),
        numbers: BTreeMap::new(),
    };
    if let ControlFlow::Break(error) = statement.visit(&mut bindings) {
        return Err(IntentRefusal::Unsupported(error).into());
    }
    bindings.bind().map_err(IntentRefusal::Unsupported)?;
    let Statement::Insert(insert) = &mut statement else {
        return Ok(None);
    };
    let source = insert.source.get_or_insert_with(|| {
        Box::new(Query {
            with: None,
            body: Box::new(SetExpr::Values(Values {
                value_keyword: false,
                explicit_row: false,
                rows: vec![Parens::with_empty_span(Vec::new())],
            })),
            order_by: None,
            limit_clause: None,
            fetch: None,
            locks: Vec::new(),
            for_clause: None,
            settings: None,
            format_clause: None,
            pipe_operators: Vec::new(),
        })
    });
    let SetExpr::Values(values) = &mut *source.body else {
        return Err(IntentRefusal::GeneratedIdFromSelect { table }.into());
    };
    if insert.columns.is_empty() {
        if values
            .rows
            .iter()
            .any(|row| !row.is_empty() && row.len() != shape.insertable.len())
        {
            return Ok(None);
        }
        if values.rows.iter().any(|row| !row.is_empty()) {
            insert.columns = shape
                .insertable
                .iter()
                .map(|name| ObjectName::from(vec![Ident::with_quote('"', name.clone())]))
                .collect();
        }
    }
    let positions = shape
        .partition_by
        .iter()
        .map(|column| {
            insert
                .columns
                .iter()
                .position(|name| named(name).eq_ignore_ascii_case(column))
                .ok_or_else(|| {
                    IntentRefusal::Unsupported(format!(
                        "INSERT into {table} must name its partition column {column}"
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut ids = Vec::with_capacity(values.rows.len());
    for row in &values.rows {
        let partition = positions
            .iter()
            .map(|index| row.get(*index).and_then(|expr| value(expr, &bindings)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                IntentRefusal::Unsupported(
                    "a partition value must be a literal or a bound parameter".into(),
                )
            })?;
        ids.push(allocate(&partition).map_err(SplitError::Evaluate)?);
    }
    insert
        .columns
        .push(ObjectName::from(vec![Ident::with_quote('"', id)]));
    for (row, id) in values.rows.iter_mut().zip(ids) {
        let hex = id.iter().map(|byte| format!("{byte:02X}")).collect();
        row.push(Expr::value(Value::HexStringLiteral(hex)));
    }
    Ok(Some(Checked {
        sql: statement.to_string(),
        params: params.to_vec(),
    }))
}
