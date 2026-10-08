//! The smallest in-place schema change: append-only non-unique indexes.
use crate::{
    Error,
    scoped_connection::{Names, ScopedConnection, tokens},
    storage,
};
use rocky_core::{SqlValue, StoredApplication};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) async fn apply<A: StoredApplication>(
    conn: &ScopedConnection,
    app: &A,
) -> Result<(), Error> {
    let incompatible = || {
        Error::Incompatible(
            "additive indexes must be immutable non-unique indexes on application tables".into(),
        )
    };
    let definition = app.installed_schema();
    if definition.additive_indexes.is_empty() {
        let physical = conn
            .names()
            .physical("rocky_additive_indexes")
            .unwrap_or("rocky_additive_indexes");
        let existing = storage::collect_rows(
            conn.query(
                "SELECT name FROM sqlite_schema WHERE type='table' AND name=?",
                [physical],
            )
            .await?,
        )
        .await?;
        if existing.is_empty() {
            return Ok(());
        }
    }
    let mut declared = BTreeMap::new();
    let tables = crate::scoped_connection::tables(&definition.statements);
    let mut schema = definition.statements();
    for sql in &definition.additive_indexes {
        let words: Vec<_> = tokens(sql)
            .into_iter()
            .filter(|word| !word.trim().is_empty())
            .collect();
        let mut position = 2;
        if words
            .get(position)
            .is_some_and(|word| word.eq_ignore_ascii_case("IF"))
        {
            if words
                .get(3)
                .is_none_or(|word| !word.eq_ignore_ascii_case("NOT"))
                || words
                    .get(4)
                    .is_none_or(|word| !word.eq_ignore_ascii_case("EXISTS"))
            {
                return Err(incompatible());
            }
            position = 5;
        }
        if words
            .first()
            .is_none_or(|word| !word.eq_ignore_ascii_case("CREATE"))
            || words
                .get(1)
                .is_none_or(|word| !word.eq_ignore_ascii_case("INDEX"))
            || words
                .get(position + 1)
                .is_none_or(|word| !word.eq_ignore_ascii_case("ON"))
            || words.iter().any(|word| word == ";")
        {
            return Err(incompatible());
        }
        let name = words
            .get(position)
            .ok_or_else(incompatible)?
            .trim_matches(['"', '`', '[', ']'])
            .to_ascii_lowercase();
        let table = words
            .get(position + 2)
            .ok_or_else(incompatible)?
            .trim_matches(['"', '`', '[', ']']);
        if name.starts_with("rocky_")
            || name.starts_with("sqlite_")
            || conn.names().physical(&name).is_some()
            || !tables
                .iter()
                .any(|(known, _)| known.eq_ignore_ascii_case(table))
        {
            return Err(incompatible());
        }
        let hash = rocky_core::commitment("rocky.additive-index.v1", sql.as_bytes());
        if declared.insert(name, hash.to_vec()).is_some() {
            return Err(incompatible());
        }
        schema.push(sql);
    }
    conn.execute("CREATE TABLE IF NOT EXISTS rocky_additive_indexes(name TEXT PRIMARY KEY,definition BLOB NOT NULL)", ()).await?;
    let mut installed = BTreeSet::new();
    for row in storage::rows(conn, "SELECT name,definition FROM rocky_additive_indexes").await? {
        match row.as_slice() {
            [SqlValue::Text(name), SqlValue::Blob(hash)] if declared.get(name) == Some(hash) => {
                installed.insert(name.clone());
            }
            _ => return Err(incompatible()),
        }
    }
    let names = Names::new(conn.names().namespace(), &schema);
    for name in declared.keys().filter(|name| !installed.contains(*name)) {
        let physical = names.physical(name).ok_or_else(incompatible)?;
        let existing = storage::collect_rows(
            conn.query(
                "SELECT name FROM sqlite_schema WHERE name=? COLLATE NOCASE",
                [physical],
            )
            .await?,
        )
        .await?;
        if !existing.is_empty() {
            return Err(incompatible());
        }
    }
    for sql in &definition.additive_indexes {
        conn.connection()
            .execute(names.sql(&crate::bootstrap::idempotent(sql)?), ())
            .await?;
    }
    for (name, hash) in declared {
        conn.execute(
            "INSERT OR IGNORE INTO rocky_additive_indexes VALUES(?,?)",
            crate::sqlite::params![name, hash],
        )
        .await?;
    }
    Ok(())
}
