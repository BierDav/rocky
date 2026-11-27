use crate::common::{self, Device, Failure};
use rocky_sdk::{
    Access, Affinity, Database, DeleteAction, ErrorKind, ForeignKey, IdentityKey, Network, Right,
    Schema, Table, TransactionToken, params, transport::memory::Hub,
};
use std::sync::Arc;

fn schema() -> Schema {
    common::build(
        Schema::builder("counts", 1)
            .table(
                Table::replayed("lists")
                    .column(common::required("code", Affinity::Text))
                    .primary_key(["code"]),
            )
            .table(
                Table::replayed("items")
                    .column(common::required("code", Affinity::Text))
                    .column(common::required("list", Affinity::Text))
                    .column(common::required("title", Affinity::Text))
                    .primary_key(["code"])
                    .foreign_key(
                        ForeignKey::new(["list"], "lists").on_delete(DeleteAction::Cascade),
                    ),
            ),
    )
}

async fn created(root: &std::path::Path, hub: &Arc<Hub>) -> Result<Database, Failure> {
    let database = common::configuration(
        &root.join("owner"),
        &schema(),
        None,
        None,
        Some(Network::Owned(hub.connector("owner"))),
    )
    .create()
    .await?
    .database;
    database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO lists(code) VALUES('home'), ('work')",
                params![],
            )
            .await?;
            tx.execute(
                "INSERT INTO items(code, list, title) VALUES \
                 ('a', 'home', 'milk'), ('b', 'home', 'eggs'), ('c', 'work', 'memo')",
                params![],
            )
            .await
        })
        .await?;
    Ok(database)
}

async fn counted(database: &Database, sql: &str) -> Result<(u64, TransactionToken), Failure> {
    Ok(database
        .write(async |tx| {
            let changed = tx.execute(sql, params![]).await?;
            Ok::<_, rocky_sdk::Error>((changed, tx.token()))
        })
        .await?)
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_counts_the_local_rows_each_statement_changed() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let database = created(root.path(), &hub).await?;
    assert_eq!(
        counted(
            &database,
            "UPDATE items SET title = 'bread' WHERE code = 'none'"
        )
        .await?
        .0,
        0,
        "a guarded update that matches no row changes none"
    );
    assert_eq!(
        counted(
            &database,
            "UPDATE items SET title = 'oat milk' WHERE code = 'a'"
        )
        .await?
        .0,
        1
    );
    let (rewritten, token) = counted(
        &database,
        "UPDATE items SET title = 'oat milk' WHERE code = 'a'",
    )
    .await?;
    assert_eq!(rewritten, 1, "SQLite counts an identical rewrite");
    assert_eq!(
        database.transaction_outcome(token).await?,
        None,
        "an identical rewrite syncs nothing, so it has no outcome"
    );
    assert_eq!(
        counted(&database, "DELETE FROM items WHERE list = 'home'")
            .await?
            .0,
        2
    );
    assert_eq!(
        counted(&database, "DELETE FROM lists WHERE code = 'work'")
            .await?
            .0,
        1,
        "the item the delete cascades to is not counted"
    );
    assert!(
        database
            .query("SELECT code FROM items", params![])
            .await?
            .collect()
            .await?
            .is_empty(),
        "the cascade removed the work item"
    );
    database.close().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_after_a_write_in_one_transaction_counts_no_rows() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let database = created(root.path(), &hub).await?;
    let (written, read) = database
        .write(async |tx| {
            let written = tx
                .execute(
                    "UPDATE items SET title = 'tea' WHERE list = 'home'",
                    params![],
                )
                .await?;
            let read = tx.execute("SELECT code FROM items", params![]).await?;
            Ok::<_, rocky_sdk::Error>((written, read))
        })
        .await?;
    assert_eq!((written, read), (2, 0));
    database.close().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_statement_fails_instead_of_counting_zero_rows() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = Device::new(created(root.path(), &hub).await?)?;
    let reader = Device::new(
        common::configuration(
            &root.path().join("reader"),
            &schema(),
            None,
            Some(Arc::new(IdentityKey::generate())),
            Some(Network::Owned(hub.connector("reader"))),
        )
        .join(&owner.invite_as(Access::Read).await)
        .await?,
    )?;
    let refused = reader
        .database
        .write(async |tx| {
            tx.execute(
                "UPDATE items SET title = 'x' WHERE code = 'none'",
                params![],
            )
            .await
        })
        .await
        .err()
        .ok_or("a reader's update counted rows")?;
    assert!(
        matches!(
            refused.kind(),
            ErrorKind::Denied {
                need: Right::Update(column),
                ..
            } if column == "title"
        ),
        "{refused:?}"
    );
    reader.close().await;
    owner.close().await;
    Ok(())
}
