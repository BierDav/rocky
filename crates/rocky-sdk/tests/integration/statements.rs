use crate::common::{Device, Failure};
use rocky_sdk::{
    Affinity, Column, Database, ErrorKind, InvalidName, NameRule, Network, Schema, SchemaError,
    SqlValue, Table, Unsupported, params, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

fn schema() -> Result<Schema, Failure> {
    Ok(Schema::builder("statements", 1)
        .table(
            Table::replayed("notes")
                .column(Column::new("title", Affinity::Text).required())
                .column(Column::new("tag", Affinity::Text)),
        )
        .build()?)
}
fn open(root: &Path, schema: &Schema, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    crate::common::configuration(
        &root.join(name),
        schema,
        None,
        None,
        Some(Network::Owned(hub.connector(name))),
    )
}
async fn note(database: &Database, title: &str, tag: Option<&str>) -> Result<(), Failure> {
    let mut tx = database.transaction().await?;
    let id = tx.next_id("notes", Vec::new()).await?;
    tx.execute(
        "INSERT INTO notes(title,tag,id) VALUES(?,?,?)",
        params![title, tag, id],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
async fn write(
    database: &Database,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<(), rocky_sdk::Error> {
    let mut tx = database.transaction().await?;
    tx.execute(sql, params).await?;
    tx.commit().await?;
    Ok(())
}
async fn titles(
    database: &Database,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<Vec<String>, Failure> {
    database
        .query(sql, params)
        .await?
        .collect()
        .await?
        .iter()
        .map(|row| Ok(row.get_at(0)?))
        .collect()
}
async fn all(database: &Database) -> Result<Vec<String>, Failure> {
    titles(
        database,
        "SELECT title || ':' || coalesce(tag, '-') FROM notes ORDER BY title",
        Vec::new(),
    )
    .await
}

#[tokio::test]
async fn is_comparisons_read_and_replay_on_another_device() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = schema()?;
    let a = Device::new(open(root.path(), &schema, &hub, "a").create().await?)?;
    let invitation = a.invite_as(rocky_sdk::Access::Full).await;
    let b = Device::new(
        open(root.path(), &schema, &hub, "b")
            .join(&invitation)
            .await?,
    )?;
    note(&a.database, "plain", None).await?;
    note(&a.database, "tagged", Some("x")).await?;
    note(&a.database, "other", Some("y")).await?;

    assert_eq!(
        titles(
            &a.database,
            "SELECT title FROM notes WHERE tag IS ?",
            params![SqlValue::Null]
        )
        .await?,
        ["plain"]
    );
    assert_eq!(
        titles(
            &a.database,
            "SELECT title FROM notes WHERE tag IS NOT lower(?) ORDER BY title",
            params!["X"]
        )
        .await?,
        ["other", "plain"]
    );
    assert_eq!(
        titles(
            &a.database,
            "SELECT title FROM notes WHERE tag IS NOT DISTINCT FROM ?",
            params!["y"]
        )
        .await?,
        ["other"]
    );

    write(
        &a.database,
        "UPDATE notes SET tag = 'none' WHERE tag IS ?",
        params![SqlValue::Null],
    )
    .await?;
    write(
        &a.database,
        "UPDATE notes SET title = upper(title) WHERE tag IS NOT lower(?)",
        params!["X"],
    )
    .await?;
    write(
        &a.database,
        "DELETE FROM notes WHERE tag IS NOT DISTINCT FROM ?",
        params!["y"],
    )
    .await?;
    let expected = ["PLAIN:none", "tagged:x"];
    assert_eq!(all(&a.database).await?, expected);
    for device in [&b, &a, &b] {
        crate::support::peerless_ok(device.sync_now().await)?;
    }
    assert_eq!(
        all(&b.database).await?,
        expected,
        "b replays the same writes"
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test]
async fn a_clock_or_random_value_in_a_replayed_write_is_refused_with_a_hint_to_bind_it()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let schema = schema()?;
    let database = crate::common::configuration(&root.path().join("a"), &schema, None, None, None)
        .create()
        .await?
        .database;
    note(&database, "plain", None).await?;
    for (sql, function) in [
        ("UPDATE notes SET tag = datetime('now')", "datetime"),
        ("UPDATE notes SET tag = random()", "random"),
    ] {
        let refused = write(&database, sql, Vec::new())
            .await
            .err()
            .ok_or("a non-deterministic replayed write ran")?;
        let ErrorKind::Sql {
            unsupported: Some(Unsupported::NonDeterministic(named)),
            ..
        } = refused.kind()
        else {
            return Err(format!("{sql}: {refused:?}").into());
        };
        assert_eq!(named, function);
        assert!(
            refused.to_string().contains("bind it as a parameter"),
            "{refused}"
        );
        assert!(!refused.retryable());
    }
    write(&database, "UPDATE notes SET tag = ?", params!["2026-11-24"]).await?;
    assert_eq!(all(&database).await?, ["plain:2026-11-24"]);
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_with_clause_in_a_replayed_write_is_refused_as_with() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let schema = schema()?;
    let database = crate::common::configuration(&root.path().join("a"), &schema, None, None, None)
        .create()
        .await?
        .database;
    note(&database, "plain", None).await?;
    for sql in [
        "WITH picked AS (SELECT 'x' AS tag) UPDATE notes SET tag = (SELECT tag FROM picked)",
        "UPDATE notes SET tag = 'x' WHERE id IN (WITH ids AS (SELECT id FROM notes) SELECT id FROM ids)",
    ] {
        let refused = write(&database, sql, Vec::new())
            .await
            .err()
            .ok_or("a replayed write with WITH ran")?;
        assert!(
            matches!(
                refused.kind(),
                ErrorKind::Sql {
                    unsupported: Some(Unsupported::With),
                    ..
                }
            ),
            "{sql}: {refused:?}"
        );
    }
    assert_eq!(all(&database).await?, ["plain:-"]);
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_write_over_a_size_limit_names_the_limit_and_both_sizes() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let schema = schema()?;
    let database = crate::common::configuration(&root.path().join("a"), &schema, None, None, None)
        .create()
        .await?
        .database;
    let mut tx = database.transaction().await?;
    let id = tx.next_id("notes", Vec::new()).await?;
    let refused = tx
        .execute(
            "INSERT INTO notes(title,id) VALUES(?,?)",
            params!["x".repeat(200 * 1024), id],
        )
        .await
        .err()
        .ok_or("an oversized intent was recorded")?;
    let ErrorKind::TooLarge {
        limit: rocky_sdk::Limit::Intent,
        size,
        max,
    } = *refused.kind()
    else {
        return Err(format!("{refused:?}").into());
    };
    assert_eq!(max, 128 * 1024);
    assert!(size > 200 * 1024, "{refused}");
    assert!(!refused.retryable());
    tx.rollback().await?;
    note(&database, "after", None).await?;
    assert_eq!(all(&database).await?, ["after:-"]);
    database.close().await?;
    Ok(())
}

#[test]
fn an_invalid_name_in_a_schema_names_its_path_and_the_rule_it_breaks() {
    let text = || Column::new("body", Affinity::Text);
    for (schema, path, name, rule) in [
        (
            Schema::builder("1st", 1).table(Table::replayed("notes").column(text())),
            "application",
            "1st",
            NameRule::Identifier,
        ),
        (
            Schema::builder("notes", 1).table(Table::replayed("my notes").column(text())),
            "tables",
            "my notes",
            NameRule::Identifier,
        ),
        (
            Schema::builder("notes", 1).table(Table::replayed("sqlite_notes").column(text())),
            "tables",
            "sqlite_notes",
            NameRule::Reserved,
        ),
        (
            Schema::builder("notes", 1).table(
                Table::replayed("notes")
                    .column(text())
                    .column(Column::new("Body", Affinity::Text)),
            ),
            "tables.notes.columns",
            "Body",
            NameRule::Unique,
        ),
    ] {
        let refused = schema.build().err();
        let expected = InvalidName::new(path, name, rule);
        assert!(
            matches!(
                refused.as_ref().map(rocky_sdk::Error::kind),
                Some(ErrorKind::Schema(error)) if **error == SchemaError::InvalidName(expected.clone())
            ),
            "{expected}: {refused:?}"
        );
    }
}

#[tokio::test]
async fn only_a_transaction_without_a_write_statement_commits_no_writes() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database =
        crate::common::configuration(&root.path().join("a"), &schema()?, None, None, None)
            .create()
            .await?
            .database;
    let tx = database.transaction().await?;
    assert_eq!(tx.commit().await?, rocky_sdk::Commit::NoWrites);
    let mut tx = database.transaction().await?;
    tx.execute("SELECT count(*) FROM notes", Vec::new()).await?;
    assert_eq!(tx.commit().await?, rocky_sdk::Commit::NoWrites);
    let mut tx = database.transaction().await?;
    tx.execute(
        "UPDATE notes SET tag = 'none' WHERE title = 'absent'",
        Vec::new(),
    )
    .await?;
    assert!(
        matches!(tx.commit().await?, rocky_sdk::Commit::Recorded(_)),
        "a write that matches no row still records an intent, which every device replays"
    );
    database.close().await?;
    Ok(())
}
