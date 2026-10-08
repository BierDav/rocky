//! Historical SQL scenarios with explicit format-2 incarnation allocation.
#![allow(dead_code)]
use rocky_core::{LogicalSchema, NodeId, SqlValue, TableClass};
use rocky_runtime::{Error, SqlRows, SqlSession, SqlState};
use rocky_sdk::TypedApp;
use sqlparser::{
    ast::{Expr, Ident, ObjectName, SelectItem, SetExpr, SqliteOnConflict, Statement},
    dialect::SQLiteDialect,
    parser::Parser,
};

pub fn app(schema: &LogicalSchema, domains: &[rocky_core::DomainId], nodes: &[NodeId]) -> TypedApp {
    let mut app = TypedApp::new(schema.clone(), domains).unwrap();
    for node in nodes {
        for table in schema.mergeable() {
            app = app
                .with_row_author(*node, &table.sql_name, u32::from(node.0[0]))
                .unwrap();
        }
    }
    app
}
fn expression(text: &str) -> Expr {
    let Statement::Query(query) = Parser::parse_sql(&SQLiteDialect {}, &format!("SELECT {text}"))
        .unwrap()
        .remove(0)
    else {
        panic!("fixture expression")
    };
    let SetExpr::Select(select) = *query.body else {
        panic!("fixture select")
    };
    let SelectItem::UnnamedExpr(expr) = select.projection.into_iter().next().unwrap() else {
        panic!("fixture expression")
    };
    expr
}
async fn allocate(session: &SqlSession, table: &str, local: bool) -> Result<i64, Error> {
    if !local && !table.starts_with("a_") && !table.starts_with("b_") {
        return Ok(session.next_id(table).await?.integer());
    }
    let floor = if local {
        425201762304i64
    } else {
        4294967296i64
    };
    let row = session
        .run(
            &format!("SELECT COALESCE(MAX(id),{floor})+1 FROM \"{table}\""),
            vec![],
        )
        .await?;
    let SqlValue::Integer(id) = row.rows[0][0] else {
        panic!("fixture identity")
    };
    Ok(id)
}
/// Supplies IDs once. Keep the returned statement when retrying a durable token.
pub async fn rewrite(session: &SqlSession, sql: &str) -> Result<String, Error> {
    let Ok(mut statements) = Parser::parse_sql(&SQLiteDialect {}, sql) else {
        return Ok(sql.into());
    };
    if statements.len() != 1 {
        return Ok(sql.into());
    }
    let Statement::Insert(insert) = &mut statements[0] else {
        return Ok(sql.into());
    };
    let table = insert.table.to_string().trim_matches('"').to_string();
    let catalog = session.catalog().await?;
    let Some(descriptor) = catalog.iter().find(|entry| entry.name == table) else {
        return Ok(sql.into());
    };
    let local = descriptor.class == TableClass::LocalOnly;
    if insert
        .columns
        .iter()
        .any(|column| column.to_string().trim_matches('"') == "id")
    {
        return Ok(sql.into());
    }
    if insert.columns.is_empty() {
        insert.columns = descriptor
            .columns
            .iter()
            .filter(|column| !["id", "row_kind"].contains(&column.name.as_str()))
            .map(|column| ObjectName::from(Ident::new(&column.name)))
            .collect();
    }
    let key_columns: Vec<_> = descriptor
        .columns
        .iter()
        .filter(|column| column.key.is_some())
        .collect();
    let key_positions: Vec<_> = key_columns
        .iter()
        .filter_map(|key| {
            insert
                .columns
                .iter()
                .position(|column| column.to_string().trim_matches('"') == key.name)
        })
        .collect();
    let reuse = insert.or != Some(SqliteOnConflict::Replace);
    insert.columns.push(Ident::new("id").into());
    let source = insert.source.as_mut().unwrap();
    match source.body.as_mut() {
        SetExpr::Values(values) => {
            values.rows.sort_by_key(|row| {
                key_positions
                    .iter()
                    .map(|position| row[*position].to_string())
                    .collect::<Vec<_>>()
            });
            let mut local_offset = 0;
            for row in &mut values.rows {
                let mut held = None;
                if reuse
                    && key_positions.len() == key_columns.len()
                    && key_positions
                        .iter()
                        .all(|position| !row[*position].to_string().contains('?'))
                {
                    let predicates: Vec<_> = key_columns
                        .iter()
                        .zip(&key_positions)
                        .map(|(key, position)| format!("\"{}\"={}", key.name, row[*position]))
                        .collect();
                    let result = session
                        .run(
                            &format!(
                                "SELECT id FROM \"{table}\" WHERE {}",
                                predicates.join(" AND ")
                            ),
                            vec![],
                        )
                        .await?;
                    if let Some(first) = result.rows.first()
                        && let SqlValue::Integer(id) = first[0]
                    {
                        held = Some(
                            if insert.or == Some(SqliteOnConflict::Ignore) || insert.on.is_some() {
                                id
                            } else {
                                i64::MAX
                            },
                        );
                    }
                }
                let id = match held {
                    Some(id) => id,
                    None => allocate(session, &table, local).await?,
                };
                let id = id
                    + if local {
                        let offset = local_offset;
                        local_offset += 1;
                        offset
                    } else {
                        0
                    };
                row.push(expression(&id.to_string()));
            }
        }
        SetExpr::Select(_) => {
            let count_sql = format!("SELECT count(*) FROM ({source})");
            let rows = session.run(&count_sql, vec![]).await?;
            let SqlValue::Integer(count) = rows.rows[0][0] else {
                panic!("fixture count")
            };
            let first = allocate(session, &table, local).await?;
            if !local {
                for _ in 1..count {
                    allocate(session, &table, false).await?;
                }
            }
            let SetExpr::Select(select) = source.body.as_mut() else {
                unreachable!()
            };
            select
                .projection
                .push(SelectItem::UnnamedExpr(expression(&format!(
                    "{first} + row_number() OVER () - 1"
                ))));
        }
        _ => panic!("fixture INSERT source"),
    }
    Ok(statements[0].to_string())
}
pub async fn run(session: &SqlSession, sql: &str, values: Vec<SqlValue>) -> Result<SqlRows, Error> {
    if !sql.trim_start().to_uppercase().starts_with("INSERT") {
        return session.run(sql, values).await;
    }
    let implicit = session.run("SELECT 1", vec![]).await?.state == SqlState::Idle;
    if implicit {
        session.run("BEGIN", vec![]).await?;
    }
    session.run("SAVEPOINT fixture_identity", vec![]).await?;
    let result = async { session.run(&rewrite(session, sql).await?, values).await }.await;
    if result.is_err() && !sql.to_uppercase().contains("OR FAIL") {
        let _ = session.run("ROLLBACK TO fixture_identity", vec![]).await;
    }
    let _ = session.run("RELEASE fixture_identity", vec![]).await;
    if !implicit {
        return result;
    }
    match result {
        Ok(mut result) => {
            let committed = session.run("COMMIT", vec![]).await?;
            result.state = committed.state;
            result.committed = committed.committed;
            Ok(result)
        }
        Err(error) => {
            let _ = session.run("ROLLBACK", vec![]).await;
            Err(error)
        }
    }
}
