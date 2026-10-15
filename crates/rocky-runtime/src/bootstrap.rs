//! Bounded, resumable DDL installation. The plan hash prevents resuming another build.
use crate::{
    Error,
    scoped_connection::ScopedConnection,
    sqlite::{self, Value},
    storage,
};

pub(crate) const STEP: usize = 64;
pub(crate) type Statement = (String, Vec<Value>);

/// Only compiled CREATE TABLE/INDEX/VIEW statements are accepted for schema creation.
pub(crate) fn idempotent(sql: &str) -> Result<String, Error> {
    let words: Vec<_> = sql.split_whitespace().take(6).collect();
    let unique = words
        .get(1)
        .is_some_and(|word| word.eq_ignore_ascii_case("UNIQUE"));
    let kind = if unique { 2 } else { 1 };
    if words
        .first()
        .is_none_or(|word| !word.eq_ignore_ascii_case("CREATE"))
        || words.get(kind).is_none_or(|word| {
            if unique {
                !word.eq_ignore_ascii_case("INDEX")
            } else {
                !word.eq_ignore_ascii_case("TABLE")
                    && !word.eq_ignore_ascii_case("INDEX")
                    && !word.eq_ignore_ascii_case("VIEW")
            }
        })
        || words.len() <= kind + 1
    {
        return Err(Error::Incompatible(
            "schema requires CREATE TABLE, CREATE INDEX or CREATE VIEW statements".into(),
        ));
    }
    let name = words[kind + 1];
    if name.eq_ignore_ascii_case("IF") {
        return Ok(sql.to_string());
    }
    let start = sql
        .find(name)
        .ok_or_else(|| Error::Incompatible("invalid schema statement".into()))?;
    Ok(format!("{}IF NOT EXISTS {}", &sql[..start], &sql[start..]))
}
pub(crate) async fn install(
    root: &ScopedConnection,
    binding: &[u8],
    statements: Vec<Statement>,
) -> Result<(), Error> {
    storage::rows(root, "PRAGMA journal_mode=wal").await?;
    root.execute("PRAGMA synchronous=FULL", ()).await?;
    root.execute("PRAGMA fullfsync=ON", ()).await?;
    let mut plan = Vec::new();
    for (sql, values) in &statements {
        plan.extend((sql.len() as u64).to_le_bytes());
        plan.extend(sql.as_bytes());
        for value in values {
            if let Value::Blob(bytes) = value {
                plan.extend((bytes.len() as u64).to_le_bytes());
                plan.extend(bytes);
            }
        }
    }
    let hash = rocky_core::commitment("rocky.schema-bootstrap.v1", &plan);
    root.execute("BEGIN IMMEDIATE", ()).await?;
    let result = async {
        root.execute("CREATE TABLE IF NOT EXISTS rocky_runtime(id INTEGER PRIMARY KEY CHECK(id=1),binding BLOB NOT NULL)", ()).await?;
        root.execute("CREATE TABLE IF NOT EXISTS rocky_bootstrap(id INTEGER PRIMARY KEY CHECK(id=1),binding BLOB NOT NULL,plan BLOB NOT NULL,position INTEGER NOT NULL)", ()).await?;
        root.execute("INSERT OR IGNORE INTO rocky_bootstrap VALUES(1,?,?,0)", sqlite::params![binding.to_vec(), hash.to_vec()]).await?;
        Ok(())
    }.await;
    storage::finish(root, result).await?;
    let recorded = storage::rows(
        root,
        "SELECT binding,plan,position FROM rocky_bootstrap WHERE id=1",
    )
    .await?;
    let mut position = match recorded.as_slice() {
        [row] => match row.as_slice() {
            [
                rocky_core::SqlValue::Blob(saved),
                rocky_core::SqlValue::Blob(plan),
                rocky_core::SqlValue::Integer(position),
            ] if saved == binding
                && plan == &hash
                && *position >= 0
                && (*position as usize) <= statements.len() =>
            {
                *position as usize
            }
            _ => {
                return Err(Error::Incompatible(
                    "interrupted schema installation belongs to another plan".into(),
                ));
            }
        },
        _ => return Err(Error::Corrupt("schema bootstrap marker".into())),
    };
    while position < statements.len() {
        let end = (position + STEP).min(statements.len());
        root.execute("BEGIN IMMEDIATE", ()).await?;
        let result = async {
            for (sql, values) in &statements[position..end] {
                root.execute(sql, values.clone()).await?;
            }
            root.execute(
                "UPDATE rocky_bootstrap SET position=? WHERE id=1",
                [end as i64],
            )
            .await?;
            Ok(())
        }
        .await;
        storage::finish(root, result).await?;
        position = end;
        #[cfg(test)]
        if ABORT_AFTER_STEP.load(std::sync::atomic::Ordering::Relaxed) {
            std::process::abort();
        }
    }
    root.execute("BEGIN IMMEDIATE", ()).await?;
    let result = async {
        root.execute(
            "INSERT OR REPLACE INTO rocky_runtime VALUES(1,?)",
            [binding.to_vec()],
        )
        .await?;
        root.execute("DROP TABLE rocky_bootstrap", ()).await?;
        Ok(())
    }
    .await;
    storage::finish(root, result).await
}
#[cfg(test)]
pub(crate) static ABORT_AFTER_STEP: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Base, shadow and protocol schema of a domain, with its identity last.
pub(crate) fn domain_statements<A: rocky_core::StoredApplication>(
    mapping: &ScopedConnection,
    identity: crate::Identity,
    app: &A,
    config: &crate::RuntimeConfig,
) -> Result<Vec<Statement>, Error> {
    let mut statements = Vec::new();
    let schema = app.installed_schema().statements;
    let protocol = |sql: &[&str]| -> Vec<Statement> {
        sql.iter()
            .map(|sql| (mapping.sql(sql), Vec::new()))
            .collect()
    };
    statements.extend(protocol(storage::SCHEMA));
    statements.extend(protocol(crate::worker::retention::SCHEMA));
    statements.extend(protocol(&[storage::UNAPPLIED_INDEX]));
    statements.push((
        mapping.sql("INSERT INTO rocky_checkpoint VALUES(1,?,?,?)"),
        crate::worker::retention::initial_checkpoint()?,
    ));
    statements.extend(schema.iter().map(|sql| (mapping.sql(sql), Vec::new())));
    statements.extend(protocol(&[storage::PEER_SCHEMA]));
    let namespace = mapping.names().namespace().map_or_else(
        || "rocky_ckpt".to_string(),
        |namespace| format!("{namespace}_ckpt"),
    );
    let shadow = mapping.rescoped(Some(&namespace), &schema);
    for sql in &schema {
        statements.push((shadow.sql(sql), Vec::new()));
    }
    statements.push((
        mapping.sql("INSERT INTO rocky_replica VALUES(1,?,0,0,0)"),
        vec![Value::Blob(crate::worker::identity_bytes(
            identity, app, config,
        ))],
    ));
    Ok(statements)
}
