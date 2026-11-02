//! Schema restrictions: transition checks on each row a transaction
//! updated or deleted, judged on the row as it stood before the
//! transaction. Author commit and every replay run them on the same
//! images, so replicas decide alike. Rows Rocky writes itself (undo,
//! checkpoints, rebuilds) never pass through here.
use super::*;
use rocky_core::{Affinity, Collation, LogicalSchema, LogicalTable, Right};

/// The first restriction of `logical` that refuses `right` on a row whose
/// image before the transaction, in column order, is `before`.
pub(in crate::replica) fn restricted(
    conn: &sqlite::Connection,
    schema: &LogicalSchema,
    logical: &LogicalTable,
    before: Option<&[SqlValue]>,
    right: Option<Right>,
) -> Result<Option<String>, Error> {
    let Some(before) = before else {
        return Ok(None);
    };
    let (verb, refuses): (&str, fn(&rocky_core::LogicalRestriction) -> bool) = match right {
        Some(Right::Update) => ("update", |restriction| restriction.refuse_update),
        Some(Right::Delete) => ("delete", |restriction| restriction.refuse_delete),
        _ => return Ok(None),
    };
    for restriction in schema.restrictions_of(logical).filter(|r| refuses(r)) {
        if matches(conn, logical, restriction, before)? {
            return Ok(Some(format!(
                "restriction {} refuses this {verb} of {}",
                restriction.name, logical.sql_name
            )));
        }
    }
    Ok(None)
}

/// Whether `before` satisfies the restriction's `when`. The row is bound as
/// a one-row subquery whose columns carry the table's affinities and
/// collations, so the rule compares as it would against the table.
fn matches(
    conn: &sqlite::Connection,
    logical: &LogicalTable,
    restriction: &rocky_core::LogicalRestriction,
    before: &[SqlValue],
) -> Result<bool, Error> {
    if restriction.when.is_none() {
        return Ok(true);
    }
    let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    let rule = restriction
        .bind_when(logical, |column| quote(&column.sql_name))
        .ok_or_else(|| Error::Corrupt(format!("restriction {}", restriction.name)))?;
    if before.len() != logical.columns.len() {
        return Err(Error::Corrupt("restricted row image".into()));
    }
    let columns: Vec<String> = logical
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let value = match column.affinity {
                Affinity::Integer => format!("CAST(?{} AS INTEGER)", index + 1),
                Affinity::Real => format!("CAST(?{} AS REAL)", index + 1),
                Affinity::Text => format!("CAST(?{} AS TEXT)", index + 1),
                Affinity::Blob | Affinity::Numeric => format!("?{}", index + 1),
            };
            let collation = match column.collation {
                Collation::Binary => "BINARY",
                Collation::Nocase => "NOCASE",
                Collation::Rtrim => "RTRIM",
            };
            format!("{value} COLLATE {collation} AS {}", quote(&column.sql_name))
        })
        .collect();
    let sql = format!(
        "SELECT 1 FROM (SELECT {}) WHERE ({rule})",
        columns.join(",")
    );
    Ok(!conn
        .rows(&sql, storage::bindings(before.to_vec()))?
        .is_empty())
}
