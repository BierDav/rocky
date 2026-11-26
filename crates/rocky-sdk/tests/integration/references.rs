use crate::common::{self, Device, Failure, introduce};
use rocky_sdk::{
    Affinity, Column, Database, DropReason, ErrorKind, ForeignKey, Index, Network, Partition,
    RowIdentity, Schema, SchemaError, SqlValue, Table, TransactionOutcome, TransactionToken,
    params, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

fn accounts() -> Table {
    Table::replayed("accounts").column(common::required("name", Affinity::Text))
}
fn shelves() -> Table {
    Table::replayed("shelves")
        .column(common::required("site", Affinity::Text))
        .column(common::required("label", Affinity::Text))
        .primary_key(["site", "id"])
        .partition_by(["site"])
        .partition_group("site")
}
fn books() -> Table {
    Table::replayed("books")
        .column(common::required("site", Affinity::Text))
        .column(common::required("shelf", Affinity::Blob))
        .column(common::required("title", Affinity::Text))
        .primary_key(["site", "id"])
        .partition_by(["site"])
        .partition_group("site")
        .foreign_key(ForeignKey::new(["site", "shelf"], "shelves"))
        .index(Index::new("books_by_title", ["site", "title"]).unique())
}
fn ledger() -> Schema {
    common::build(
        Schema::builder("ledger", 1)
            .table(accounts())
            .table(
                Table::replayed("entries")
                    .column(
                        Column::new("account", Affinity::Blob)
                            .required()
                            .references("accounts"),
                    )
                    .column(common::required("cents", Affinity::Integer)),
            )
            .table(shelves())
            .table(books()),
    )
}

fn open(directory: &Path, hub: Option<&Arc<Hub>>, name: &str) -> rocky_sdk::Configuration {
    common::configuration(
        directory,
        &ledger(),
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
        .write(async |tx| {
            tx.execute(sql, values).await?;
            Ok(tx.token())
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
async fn count(database: &Database, table: &str) -> Result<i64, Failure> {
    let rows = database
        .query(&format!("SELECT count(*) FROM {table}"), params![])
        .await?
        .collect()
        .await?;
    Ok(rows.first().ok_or("no count")?.get_at(0)?)
}
async fn add_account(database: &Database) -> Result<RowIdentity, Failure> {
    Ok(database
        .write(async |tx| {
            let id = tx.next_id("accounts", Vec::new()).await?;
            tx.execute(
                "INSERT INTO accounts(id, name) VALUES(?, 'Ada')",
                params![id],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(id)
        })
        .await?)
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
async fn restrict_refuses_deleting_a_parent_that_rows_still_reference() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = open(&root.path().join("a"), None, "a")
        .create()
        .await?
        .database;
    let account = add_account(&database).await?;
    write(
        &database,
        "INSERT INTO entries(account, cents) VALUES(?, 500)",
        params![account],
    )
    .await?;
    let refused = attempt(
        &database,
        "DELETE FROM accounts WHERE id = ?",
        params![account],
    )
    .await
    .err()
    .ok_or("a referenced account was deleted")?;
    assert!(
        matches!(
            refused.kind(),
            ErrorKind::Sql {
                unsupported: None,
                ..
            }
        ),
        "{refused:?}"
    );
    assert_eq!(count(&database, "accounts").await?, 1);
    assert_eq!(
        count(&database, "entries").await?,
        1,
        "restrict keeps the history a cascade would have deleted"
    );
    write(
        &database,
        "DELETE FROM entries WHERE account = ?",
        params![account],
    )
    .await?;
    write(
        &database,
        "DELETE FROM accounts WHERE id = ?",
        params![account],
    )
    .await?;
    assert_eq!(count(&database, "accounts").await?, 0);
    database.close().await?;
    Ok(())
}

async fn outcome(device: &Device, token: TransactionToken) -> Result<TransactionOutcome, Failure> {
    device
        .database
        .transaction_outcome(token)
        .await?
        .ok_or_else(|| "an unknown transaction".into())
}

async fn one_of_two_drops_on_every_device(
    a: &Device,
    b: &Device,
    parent_delete: TransactionToken,
    child_insert: TransactionToken,
) -> Result<TransactionToken, Failure> {
    let applied = TransactionOutcome::Applied { sealed: false };
    let dropped = TransactionOutcome::Dropped {
        reason: DropReason::Constraint,
        sealed: false,
    };
    let kept = if outcome(a, parent_delete).await? == applied {
        parent_delete
    } else {
        child_insert
    };
    let lost = if kept == parent_delete {
        child_insert
    } else {
        parent_delete
    };
    for device in [a, b] {
        assert_eq!(outcome(device, kept).await?, applied);
        assert_eq!(
            outcome(device, lost).await?,
            dropped,
            "the later transaction in global order violates the reference and drops whole on every device"
        );
    }
    Ok(kept)
}

#[tokio::test]
async fn an_offline_parent_delete_and_child_insert_drop_the_later_one_on_every_device()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (a, b) = pair(root.path()).await?;
    let account = add_account(&a.database).await?;
    sync(&a, &b).await?;
    let deleted = write(
        &a.database,
        "DELETE FROM accounts WHERE id = ?",
        params![account],
    )
    .await?;
    let inserted = write(
        &b.database,
        "INSERT INTO entries(account, cents) VALUES(?, 700)",
        params![account],
    )
    .await?;
    sync(&a, &b).await?;
    let kept = one_of_two_drops_on_every_device(&a, &b, deleted, inserted).await?;
    let expected = if kept == deleted { (0, 0) } else { (1, 1) };
    for device in [&a, &b] {
        assert_eq!(
            (
                count(&device.database, "accounts").await?,
                count(&device.database, "entries").await?
            ),
            expected
        );
    }
    Ok(())
}

#[tokio::test]
async fn restrict_drops_the_later_of_an_offline_delete_and_insert_in_a_partition()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (a, b) = pair(root.path()).await?;
    let north = Partition::new("site", vec![SqlValue::Text("north".into())]);
    a.database.create_partition(&north).await?;
    let shelf = a
        .database
        .write(async |tx| {
            let shelf = tx.next_id("shelves", params!["north"]).await?;
            tx.execute(
                "INSERT INTO shelves(site, id, label) VALUES('north', ?, 'Poetry')",
                params![shelf],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(shelf)
        })
        .await?;
    sync(&a, &b).await?;
    let deleted = attempt(
        &a.database,
        "DELETE FROM shelves WHERE site = 'north' AND id = ?",
        params![shelf],
    )
    .await?;
    let inserted = b
        .database
        .write(async |tx| {
            let book = tx.next_id("books", params!["north"]).await?;
            tx.execute(
                "INSERT INTO books(site, id, shelf, title) VALUES('north', ?, ?, 'Odes')",
                params![book, shelf],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(tx.token())
        })
        .await?;
    sync(&a, &b).await?;
    let kept = one_of_two_drops_on_every_device(&a, &b, deleted, inserted).await?;
    let expected = if kept == deleted { (0, 0) } else { (1, 1) };
    for device in [&a, &b] {
        assert_eq!(
            (
                count(&device.database, "shelves").await?,
                count(&device.database, "books").await?
            ),
            expected
        );
    }
    Ok(())
}

#[test]
fn a_text_column_referencing_a_generated_id_is_refused_at_build() -> Result<(), Failure> {
    let refused = Schema::builder("ledger", 1)
        .table(accounts())
        .table(
            Table::replayed("entries")
                .column(Column::new("account", Affinity::Text).references("accounts")),
        )
        .build()
        .err()
        .ok_or("a text reference to a generated id was accepted")?;
    let ErrorKind::Schema(error) = refused.kind() else {
        return Err(format!("{refused:?}").into());
    };
    match error.as_ref() {
        SchemaError::ReferenceType {
            column,
            affinity: Affinity::Text,
            parent_column,
            parent_affinity: Affinity::Blob,
            ..
        } => {
            assert_eq!((column.as_str(), parent_column.as_str()), ("account", "id"));
        }
        other => return Err(format!("{other:?}").into()),
    }
    assert!(
        refused.to_string().contains("Blob column"),
        "the refusal says what a reference to a generated id is: {refused}"
    );
    Ok(())
}
