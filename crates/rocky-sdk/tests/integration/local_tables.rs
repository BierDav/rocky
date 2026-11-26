use crate::common::{Failure, configuration};
use rocky_sdk::{
    Access, Affinity, Column, Database, Invite, Network, Schema, Table, params,
    transport::memory::Hub,
};
use std::sync::Arc;

fn schema() -> Result<Schema, Failure> {
    Ok(Schema::builder("till", 1)
        .table(Table::replayed("sales").column(Column::new("total", Affinity::Integer).required()))
        .table(
            Table::local("parked")
                .column(Column::new("cart", Affinity::Text).required())
                .primary_key(["cart"]),
        )
        .build()?)
}
async fn write(database: &Database, sql: &str) -> Result<(), Failure> {
    Ok(database
        .write(async |tx| tx.execute(sql, params![]).await)
        .await?)
}
async fn count(database: &Database, table: &str) -> Result<usize, Failure> {
    let rows = database
        .query(&format!("SELECT 1 FROM {table}"), params![])
        .await?;
    Ok(rows.collect().await?.len())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_of_a_local_table_never_leave_the_device_and_survive_reopening() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = schema()?;
    let open = |name: &str| {
        configuration(
            &root.path().join(name),
            &schema,
            None,
            None,
            Some(Network::Owned(hub.connector(name))),
        )
    };
    let till = open("till").create().await?.database;
    let invitation = till.invite(Invite::new(Access::Full)).await?;
    let office = open("office").join(&invitation.code).await?.database;

    write(&till, "INSERT INTO sales(total) VALUES(1200)").await?;
    write(&till, "INSERT INTO parked(cart) VALUES('cart-1')").await?;
    write(&office, "INSERT INTO parked(cart) VALUES('cart-9')").await?;
    for database in [&till, &office, &till] {
        crate::support::peerless_ok(database.sync_now().await)?;
    }
    assert_eq!(count(&office, "sales").await?, 1, "replayed rows sync");
    let carts = |database: Database| async move {
        let rows = database.query("SELECT cart FROM parked", params![]).await?;
        let carts: Vec<String> = rows
            .collect()
            .await?
            .iter()
            .map(|row| row.get("cart"))
            .collect::<Result<_, _>>()?;
        Ok::<_, Failure>(carts)
    };
    assert_eq!(
        carts(office.clone()).await?,
        ["cart-9"],
        "the office holds only its own parked cart"
    );
    assert_eq!(carts(till.clone()).await?, ["cart-1"]);

    till.close().await?;
    let reopened = open("till").open().await?;
    assert_eq!(
        carts(reopened.clone()).await?,
        ["cart-1"],
        "a local row is kept across reopening"
    );
    reopened.close().await?;
    office.close().await?;
    Ok(())
}

#[test]
fn keyless_local_table_is_rejected_at_schema_build() {
    let error = Schema::builder("till", 1)
        .table(Table::local("outbox").column(Column::new("body", Affinity::Text).required()))
        .build()
        .expect_err("a local table without a declared primary key must be rejected");
    assert!(
        error.to_string().contains("primary key"),
        "unexpected error: {error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roles_do_not_govern_local_tables_or_next_id() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = Schema::builder("till", 1)
        .table(Table::replayed("sales").column(Column::new("total", Affinity::Integer).required()))
        .table(
            Table::local("parked")
                .column(Column::new("cart", Affinity::Text).required())
                .primary_key(["cart"]),
        )
        .role(rocky_sdk::Role::new(rocky_sdk::RoleName::new("viewer")?).read("sales"))
        .build()?;
    let open = |name: &str| {
        configuration(
            &root.path().join(name),
            &schema,
            None,
            None,
            Some(Network::Owned(hub.connector(name))),
        )
    };
    let till = open("till").create().await?.database;
    let invitation = till
        .invite(Invite::new(Access::Role(rocky_sdk::RoleName::new(
            "viewer",
        )?)))
        .await?;
    let viewer = open("viewer").join(&invitation.code).await?.database;
    write(&viewer, "INSERT INTO parked(cart) VALUES('c1')").await?;
    write(&viewer, "DELETE FROM parked WHERE cart = 'c1'").await?;
    let error = viewer
        .write(async |tx| tx.next_id("parked").await)
        .await
        .expect_err("local tables have no generated id");
    assert!(
        !matches!(error.kind(), rocky_sdk::ErrorKind::Denied { .. }),
        "roles must not govern local tables, got {error}"
    );
    viewer.close().await?;
    till.close().await?;
    Ok(())
}
