use crate::common::{self, Device, Failure, introduce};
use rocky_sdk::{
    Affinity, Database, DropReason, ErrorKind, ForeignKey, Network, RowIdentity, SqlValue, Table,
    TransactionOutcome, TransactionToken, Unsupported, params, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

fn schema() -> rocky_sdk::Schema {
    common::build(
        rocky_sdk::Schema::builder("events", 1)
            .table(
                Table::replayed("tickets")
                    .column(common::required("code", Affinity::Text))
                    .column(rocky_sdk::Column::new("holder", Affinity::Text))
                    .primary_key(["code"]),
            )
            .table(
                Table::replayed("check_ins")
                    .column(common::required("gate", Affinity::Text))
                    .column(common::required("ticket", Affinity::Text))
                    .foreign_key(
                        ForeignKey::new(["ticket"], "tickets")
                            .on_delete(rocky_sdk::DeleteAction::Cascade),
                    ),
            ),
    )
}

fn open(directory: &Path, hub: Option<&Arc<Hub>>, name: &str) -> rocky_sdk::Configuration {
    common::configuration(
        directory,
        &schema(),
        None,
        None,
        hub.map(|hub| Network::Owned(hub.connector(name))),
    )
}

async fn attempt(
    database: &Database,
    sql: &str,
    values: Vec<SqlValue>,
) -> Result<TransactionToken, rocky_sdk::Error> {
    database
        .write(async |transaction| {
            transaction.execute(sql, values).await?;
            Ok(transaction.token())
        })
        .await
}

async fn write(
    database: &Database,
    sql: &str,
    values: Vec<SqlValue>,
) -> Result<TransactionToken, Failure> {
    Ok(attempt(database, sql, values).await?)
}

async fn refused(database: &Database, sql: &str) -> Result<rocky_sdk::Error, Failure> {
    attempt(database, sql, params![])
        .await
        .err()
        .ok_or_else(|| format!("{sql} ran").into())
}

async fn tickets(database: &Database) -> Result<Vec<(String, Option<String>)>, Failure> {
    let rows = database
        .query("SELECT code, holder FROM tickets ORDER BY code", params![])
        .await?
        .collect()
        .await?;
    rows.iter()
        .map(|row| Ok((row.get_at(0)?, row.get_at(1)?)))
        .collect()
}

async fn pair(root: &Path) -> Result<(Device, Device), Failure> {
    let hub = Arc::new(Hub::default());
    let a = Device::new(open(&root.join("a"), Some(&hub), "a").create().await?)?;
    let invitation = a.invite_as(rocky_sdk::Access::Full).await;
    let b = Device::new(
        open(&root.join("b"), Some(&hub), "b")
            .join(&invitation)
            .await?,
    )?;
    b.sync().await.map_err(|error| error.to_string())?;
    introduce(&a, &b).await;
    Ok((a, b))
}

async fn sync(a: &Device, b: &Device) -> Result<(), Failure> {
    for device in [a, b, a] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tokio::test]
async fn a_plain_insert_into_a_declared_key_table_needs_no_id() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = open(&root.path().join("a"), None, "a")
        .create()
        .await?
        .database;
    write(
        &database,
        "INSERT INTO tickets(code, holder) VALUES(?, ?)",
        params!["A1", "Ada"],
    )
    .await?;
    assert_eq!(
        tickets(&database).await?,
        [("A1".into(), Some("Ada".into()))]
    );
    let rows = database.query("SELECT * FROM tickets", params![]).await?;
    assert_eq!(
        rows.columns(),
        ["code", "holder"],
        "a table with a declared key has no id column"
    );
    let refusal = database
        .write(async |transaction| transaction.next_id("tickets").await)
        .await
        .err()
        .ok_or("a declared-key table allocated a generated id")?;
    assert!(refusal.to_string().contains("no generated id"), "{refusal}");
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn an_insert_without_id_into_a_keyless_table_gets_a_generated_id() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = open(&root.path().join("a"), None, "a")
        .create()
        .await?
        .database;
    let reserved = database
        .write(async |transaction| {
            transaction
                .execute("INSERT INTO tickets(code) VALUES('A1')", params![])
                .await?;
            transaction
                .execute(
                    "INSERT INTO check_ins(gate, ticket) VALUES('north', ?), ('south', ?)",
                    params!["A1", "A1"],
                )
                .await?;
            let reserved = transaction.next_id("check_ins").await?;
            transaction
                .execute(
                    "INSERT INTO check_ins(id, gate, ticket) VALUES(?, 'east', 'A1')",
                    params![reserved],
                )
                .await?;
            Ok::<_, Failure>(reserved)
        })
        .await?;
    let rows = database
        .query("SELECT id, gate FROM check_ins ORDER BY id", params![])
        .await?
        .collect()
        .await?;
    let ids = rows
        .iter()
        .map(|row| row.get_at::<RowIdentity>(0))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        ids.iter().map(|id| id.sequence()).collect::<Vec<_>>(),
        [1, 2, 3],
        "each omitted id continues the device's own sequence, and next_id takes the next one"
    );
    assert!(ids.iter().all(|id| id.writer() == ids[0].writer()));
    assert_eq!(ids[2], reserved);
    let selected = refused(
        &database,
        "INSERT INTO check_ins(gate, ticket) SELECT gate, ticket FROM check_ins",
    )
    .await?;
    assert!(
        matches!(
            selected.kind(),
            ErrorKind::Sql { unsupported: Some(Unsupported::SelectWithoutId(table)), .. }
                if table == "check_ins"
        ),
        "{selected:?}"
    );
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_foreign_key_references_a_declared_key() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = open(&root.path().join("a"), None, "a")
        .create()
        .await?
        .database;
    let orphan = refused(
        &database,
        "INSERT INTO check_ins(gate, ticket) VALUES('north', 'missing')",
    )
    .await?;
    assert!(matches!(orphan.kind(), ErrorKind::Sql { .. }), "{orphan:?}");
    write(
        &database,
        "INSERT INTO tickets(code) VALUES('A1')",
        params![],
    )
    .await?;
    write(
        &database,
        "INSERT INTO check_ins(gate, ticket) VALUES('north', 'A1')",
        params![],
    )
    .await?;
    write(
        &database,
        "DELETE FROM tickets WHERE code = 'A1'",
        params![],
    )
    .await?;
    let left = database
        .query("SELECT count(*) FROM check_ins", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(
        left[0].get_at::<i64>(0)?,
        0,
        "deleting the ticket cascades through its declared key"
    );
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn updating_a_key_column_is_refused() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = open(&root.path().join("a"), None, "a")
        .create()
        .await?
        .database;
    write(
        &database,
        "INSERT INTO tickets(code) VALUES('A1')",
        params![],
    )
    .await?;
    write(
        &database,
        "INSERT INTO check_ins(gate, ticket) VALUES('north', 'A1')",
        params![],
    )
    .await?;
    for (sql, table, column) in [
        (
            "UPDATE tickets SET code = 'B2' WHERE code = 'A1'",
            "tickets",
            "code",
        ),
        (
            "INSERT INTO tickets(code) VALUES('A1') ON CONFLICT(code) DO UPDATE SET code = 'B2'",
            "tickets",
            "code",
        ),
        ("UPDATE check_ins SET id = NULL", "check_ins", "id"),
    ] {
        let refusal = refused(&database, sql).await?;
        match refusal.kind() {
            ErrorKind::Sql {
                unsupported:
                    Some(Unsupported::KeyUpdate {
                        table: named,
                        column: written,
                    }),
                ..
            } => {
                assert_eq!((named.as_str(), written.as_str()), (table, column), "{sql}");
            }
            other => return Err(format!("{sql}: {other:?}").into()),
        }
    }
    write(
        &database,
        "UPDATE tickets SET holder = 'Ada' WHERE code = 'A1'",
        params![],
    )
    .await?;
    assert_eq!(
        tickets(&database).await?,
        [("A1".into(), Some("Ada".into()))]
    );
    database.close().await?;
    Ok(())
}

#[tokio::test]
async fn two_devices_creating_the_same_key_offline_converge_with_one_dropped() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let (a, b) = pair(root.path()).await?;
    let insert = "INSERT INTO tickets(code, holder) VALUES('X', ?)";
    let by_a = write(&a.database, insert, params!["a"]).await?;
    let by_b = write(&b.database, insert, params!["b"]).await?;
    sync(&a, &b).await?;
    let outcome = |device: &Device, token| {
        let database = device.database.clone();
        async move { database.transaction_outcome(token).await }
    };
    let dropped = Some(TransactionOutcome::Dropped {
        reason: DropReason::Constraint,
        sealed: false,
    });
    let applied = Some(TransactionOutcome::Applied { sealed: false });
    let (kept, holder) = match outcome(&a, by_a).await? {
        outcome if outcome == applied => (by_a, "a"),
        _ => (by_b, "b"),
    };
    let lost = if kept == by_a { by_b } else { by_a };
    for device in [&a, &b] {
        assert_eq!(outcome(device, kept).await?, applied);
        assert_eq!(
            outcome(device, lost).await?,
            dropped,
            "the later insert in global order fails its key and drops on every device"
        );
        assert_eq!(
            tickets(&device.database).await?,
            [("X".into(), Some(holder.into()))]
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_upsert_on_a_declared_key_replays_onto_the_same_row() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (a, b) = pair(root.path()).await?;
    let upsert = "INSERT INTO tickets(code, holder) VALUES('X', ?) \
                  ON CONFLICT(code) DO UPDATE SET holder = excluded.holder";
    let created = write(
        &a.database,
        "INSERT INTO tickets(code, holder) VALUES('X', 'a')",
        params![],
    )
    .await?;
    let upserted = write(&b.database, upsert, params!["b"]).await?;
    sync(&a, &b).await?;
    let applied = Some(TransactionOutcome::Applied { sealed: false });
    for device in [&a, &b] {
        assert_eq!(
            device.database.transaction_outcome(upserted).await?,
            applied,
            "an upsert applies whether its key exists yet or not"
        );
        assert_eq!(tickets(&device.database).await?.len(), 1);
    }
    assert_eq!(tickets(&a.database).await?, tickets(&b.database).await?);
    let second = write(&a.database, upsert, params!["c"]).await?;
    sync(&a, &b).await?;
    for device in [&a, &b] {
        assert_eq!(device.database.transaction_outcome(second).await?, applied);
        assert_eq!(
            tickets(&device.database).await?,
            [("X".into(), Some("c".into()))],
            "the upsert replays onto the same row"
        );
    }
    let _ = created;
    Ok(())
}

#[tokio::test]
async fn a_deleted_natural_key_can_be_created_again() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (a, b) = pair(root.path()).await?;
    write(
        &a.database,
        "INSERT INTO tickets(code, holder) VALUES('X', 'first')",
        params![],
    )
    .await?;
    write(
        &a.database,
        "INSERT INTO check_ins(gate, ticket) VALUES('north', 'X')",
        params![],
    )
    .await?;
    sync(&a, &b).await?;
    write(
        &b.database,
        "DELETE FROM tickets WHERE code = 'X'",
        params![],
    )
    .await?;
    sync(&a, &b).await?;
    let again = write(
        &a.database,
        "INSERT INTO tickets(code, holder) VALUES('X', 'second')",
        params![],
    )
    .await?;
    sync(&a, &b).await?;
    for device in [&a, &b] {
        assert_eq!(
            device.database.transaction_outcome(again).await?,
            Some(TransactionOutcome::Applied { sealed: false }),
            "a natural key is free again once its row is deleted, as in SQL"
        );
        assert_eq!(
            tickets(&device.database).await?,
            [("X".into(), Some("second".into()))]
        );
        let check_ins = device
            .database
            .query("SELECT count(*) FROM check_ins", params![])
            .await?
            .collect()
            .await?;
        assert_eq!(
            check_ins[0].get_at::<i64>(0)?,
            0,
            "the re-created ticket does not inherit the deleted one's check-ins"
        );
    }
    Ok(())
}
