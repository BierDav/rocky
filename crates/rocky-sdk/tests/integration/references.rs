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
            let id = tx.next_id("accounts").await?;
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
            .fixture_person()
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
            let shelf = tx.next_id_in("shelves", params!["north"]).await?;
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
            let book = tx.next_id_in("books", params!["north"]).await?;
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

#[tokio::test]
async fn a_five_table_fk_chain_syncs_without_channel_deadlock() -> Result<(), Failure> {
    let schema = common::build(
        Schema::builder("fk_chain_5", 1)
            .table(Table::replayed("t1").column(common::required("label", Affinity::Text)))
            .table(
                Table::replayed("t2")
                    .column(
                        Column::new("parent", Affinity::Blob)
                            .required()
                            .references("t1"),
                    )
                    .column(common::required("label", Affinity::Text)),
            )
            .table(
                Table::replayed("t3")
                    .column(
                        Column::new("parent", Affinity::Blob)
                            .required()
                            .references("t2"),
                    )
                    .column(common::required("label", Affinity::Text)),
            )
            .table(
                Table::replayed("t4")
                    .column(
                        Column::new("parent", Affinity::Blob)
                            .required()
                            .references("t3"),
                    )
                    .column(common::required("label", Affinity::Text)),
            )
            .table(
                Table::replayed("t5")
                    .column(
                        Column::new("parent", Affinity::Blob)
                            .required()
                            .references("t4"),
                    )
                    .column(common::required("label", Affinity::Text)),
            ),
    );
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::new(
        common::configuration(
            &root.path().join("a"),
            &schema,
            None,
            None,
            Some(Network::Owned(hub.connector("a"))),
        )
        .create()
        .await?,
    )?;
    let invitation = a.invite_as(rocky_sdk::Access::Full).await;
    let b = Device::new(
        common::configuration(
            &root.path().join("b"),
            &schema,
            None,
            None,
            Some(Network::Owned(hub.connector("b"))),
        )
        .fixture_person()
        .join(&invitation)
        .await?,
    )?;
    b.sync().await.map_err(|error| error.to_string())?;
    introduce(&a, &b).await;

    a.database
        .write(async |tx| {
            let id1 = tx.next_id("t1").await?;
            tx.execute("INSERT INTO t1(id, label) VALUES(?, 'n1')", params![id1])
                .await?;
            let id2 = tx.next_id("t2").await?;
            tx.execute(
                "INSERT INTO t2(id, parent, label) VALUES(?, ?, 'n2')",
                params![id2, id1],
            )
            .await?;
            let id3 = tx.next_id("t3").await?;
            tx.execute(
                "INSERT INTO t3(id, parent, label) VALUES(?, ?, 'n3')",
                params![id3, id2],
            )
            .await?;
            let id4 = tx.next_id("t4").await?;
            tx.execute(
                "INSERT INTO t4(id, parent, label) VALUES(?, ?, 'n4')",
                params![id4, id3],
            )
            .await?;
            let id5 = tx.next_id("t5").await?;
            tx.execute(
                "INSERT INTO t5(id, parent, label) VALUES(?, ?, 'n5')",
                params![id5, id4],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(())
        })
        .await?;

    sync(&a, &b).await?;
    for table in ["t1", "t2", "t3", "t4", "t5"] {
        assert_eq!(count(&b.database, table).await?, 1, "{table}");
    }
    Ok(())
}

#[tokio::test]
async fn a_one_parent_four_children_star_syncs_without_channel_deadlock() -> Result<(), Failure> {
    let schema = common::build(
        Schema::builder("fk_star_5", 1)
            .table(Table::replayed("hub").column(common::required("label", Affinity::Text)))
            .table(
                Table::replayed("c1")
                    .column(
                        Column::new("hub", Affinity::Blob)
                            .required()
                            .references("hub"),
                    )
                    .column(common::required("label", Affinity::Text)),
            )
            .table(
                Table::replayed("c2")
                    .column(
                        Column::new("hub", Affinity::Blob)
                            .required()
                            .references("hub"),
                    )
                    .column(common::required("label", Affinity::Text)),
            )
            .table(
                Table::replayed("c3")
                    .column(
                        Column::new("hub", Affinity::Blob)
                            .required()
                            .references("hub"),
                    )
                    .column(common::required("label", Affinity::Text)),
            )
            .table(
                Table::replayed("c4")
                    .column(
                        Column::new("hub", Affinity::Blob)
                            .required()
                            .references("hub"),
                    )
                    .column(common::required("label", Affinity::Text)),
            ),
    );
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::new(
        common::configuration(
            &root.path().join("a"),
            &schema,
            None,
            None,
            Some(Network::Owned(hub.connector("a"))),
        )
        .create()
        .await?,
    )?;
    let invitation = a.invite_as(rocky_sdk::Access::Full).await;
    let b = Device::new(
        common::configuration(
            &root.path().join("b"),
            &schema,
            None,
            None,
            Some(Network::Owned(hub.connector("b"))),
        )
        .fixture_person()
        .join(&invitation)
        .await?,
    )?;
    b.sync().await.map_err(|error| error.to_string())?;
    introduce(&a, &b).await;

    a.database
        .write(async |tx| {
            let parent = tx.next_id("hub").await?;
            tx.execute(
                "INSERT INTO hub(id, label) VALUES(?, 'root')",
                params![parent],
            )
            .await?;
            for table in ["c1", "c2", "c3", "c4"] {
                let child = tx.next_id(table).await?;
                tx.execute(
                    &format!("INSERT INTO {table}(id, hub, label) VALUES(?, ?, '{table}')"),
                    params![child, parent],
                )
                .await?;
            }
            Ok::<_, rocky_sdk::Error>(())
        })
        .await?;

    sync(&a, &b).await?;
    for table in ["hub", "c1", "c2", "c3", "c4"] {
        assert_eq!(count(&b.database, table).await?, 1, "{table}");
    }
    Ok(())
}
