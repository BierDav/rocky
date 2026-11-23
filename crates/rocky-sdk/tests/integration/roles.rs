use crate::common::{self, Device, Failure};
use rocky_sdk::{
    Access, Affinity, Column, Database, Network, Open, PartitionTarget, Role, RoleName, Schema,
    SqlValue, Table, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

const SCANNER: RoleName = RoleName::from_static("scanner");
const CLINICIAN: RoleName = RoleName::from_static("clinician");

fn venue() -> Result<Schema, Failure> {
    Ok(Schema::builder("venue", 1)
        .table(
            Table::replayed("tickets")
                .column(Column::new("code", Affinity::Text).required())
                .column(Column::new("holder", Affinity::Text).required()),
        )
        .table(
            Table::replayed("check_ins")
                .column(Column::new("code", Affinity::Text).required())
                .column(Column::new("gate", Affinity::Text).required()),
        )
        .role(Role::new(SCANNER).read("tickets").append("check_ins"))
        .build()?)
}
fn clinic() -> Result<Schema, Failure> {
    Ok(Schema::builder("clinic", 1)
        .table(Table::replayed("wards").column(Column::new("name", Affinity::Text).required()))
        .table(
            Table::replayed("charts")
                .column(Column::new("ward", Affinity::Text).required())
                .column(Column::new("note", Affinity::Text).required())
                .primary_key(["ward", "id"])
                .partition_by(["ward"])
                .partition_group("ward"),
        )
        .role(
            Role::new(CLINICIAN)
                .read("wards")
                .write("charts")
                .per_partition("ward"),
        )
        .build()?)
}
fn open<'a>(directory: &'a Path, schema: &'a Schema, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        identity: None,
        directory,
        schema,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
    }
}
async fn rows(device: &Device, sql: &str) -> Result<Vec<String>, Failure> {
    device
        .database
        .query(sql, Vec::new())
        .await?
        .collect()
        .await?
        .iter()
        .map(|row| Ok(row.get_at::<String>(0)?))
        .collect()
}
async fn insert(device: &Device, table: &str, values: [&str; 2]) -> Result<(), Failure> {
    let mut tx = device.database.transaction().await?;
    let (columns, partition) = match table {
        "tickets" => ("code,holder", Vec::new()),
        "check_ins" => ("code,gate", Vec::new()),
        _ => ("ward,note", vec![SqlValue::Text(values[0].into())]),
    };
    let id = tx.next_id(table, partition).await?;
    tx.execute(
        &format!("INSERT INTO {table}({columns},id) VALUES(?,?,?)"),
        vec![
            SqlValue::Text(values[0].into()),
            SqlValue::Text(values[1].into()),
            id.sql(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
async fn ward_name(device: &Device, name: &str) -> Result<(), Failure> {
    let mut tx = device.database.transaction().await?;
    let id = tx.next_id("wards", Vec::new()).await?;
    tx.execute(
        "INSERT INTO wards(name,id) VALUES(?,?)",
        vec![SqlValue::Text(name.into()), id.sql()],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
async fn write(device: &Device, sql: &str) -> Result<(), Failure> {
    let mut tx = device.database.transaction().await?;
    tx.execute(sql, Vec::new()).await?;
    tx.commit().await?;
    Ok(())
}
fn holds_reader_key(directory: &Path, table: &str) -> Result<bool, Failure> {
    let identity = common::authority_json(directory, "identity")?;
    Ok(identity["keys"].get(table).is_some())
}
fn ward(name: &str) -> PartitionTarget {
    PartitionTarget::Group {
        group: "ward".into(),
        values: vec![SqlValue::Text(name.into())],
    }
}
async fn synced(device: &Device) -> Result<(), Failure> {
    device.sync().await.map_err(|error| error.to_string())?;
    Ok(())
}

#[tokio::test]
async fn a_scanner_reads_tickets_and_only_appends_check_ins() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = venue()?;
    let admin = Device::new(Database::create(
        open(&root.path().join("admin"), &schema, &hub, "admin"),
        None,
    )?)?;
    insert(&admin, "tickets", ["T1", "Ada"]).await?;
    let provisioned = admin.session.provision(Access::Role(SCANNER), None).await?;
    assert_eq!(provisioned.access, Access::Role(SCANNER));
    let scanner_dir = root.path().join("scanner");
    let scanner = Device::new(Database::join(
        open(&scanner_dir, &schema, &hub, "scanner"),
        &provisioned.uri,
    )?)?;
    synced(&scanner).await?;
    assert_eq!(rows(&scanner, "SELECT code FROM tickets").await?, ["T1"]);
    assert!(
        insert(&scanner, "tickets", ["T2", "Eve"]).await.is_err(),
        "a scanner writes no ticket"
    );
    assert!(
        write(&scanner, "UPDATE tickets SET holder='Eve'")
            .await
            .is_err(),
        "a scanner changes no ticket"
    );
    insert(&scanner, "check_ins", ["T1", "north"]).await?;
    assert!(
        write(&scanner, "UPDATE check_ins SET gate='south'")
            .await
            .is_err(),
        "appending allows no update"
    );
    assert!(
        write(&scanner, "DELETE FROM check_ins").await.is_err(),
        "appending allows no delete"
    );
    synced(&scanner).await?;
    synced(&admin).await?;
    assert_eq!(rows(&admin, "SELECT gate FROM check_ins").await?, ["north"]);
    assert!(holds_reader_key(&scanner_dir, "tickets")?);
    assert_eq!(
        scanner.session.info().await?.access,
        Access::Role(SCANNER),
        "the role survives into the installed settings"
    );
    scanner.close().await;
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn a_clinician_reads_only_the_wards_an_admin_granted() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = clinic()?;
    let admin = Device::new(Database::create(
        open(&root.path().join("admin"), &schema, &hub, "admin"),
        None,
    )?)?;
    for name in ["A", "B"] {
        admin.session.create_partition(ward(name)).await?;
        insert(&admin, "charts", [name, &format!("chart in {name}")]).await?;
    }
    ward_name(&admin, "A").await?;
    let provisioned = admin
        .session
        .provision(Access::Role(CLINICIAN), None)
        .await?;
    let clinician = Device::new(Database::join(
        open(&root.path().join("clinician"), &schema, &hub, "clinician"),
        &provisioned.uri,
    )?)?;
    synced(&clinician).await?;
    assert_eq!(rows(&clinician, "SELECT name FROM wards").await?, ["A"]);
    assert!(
        rows(&clinician, "SELECT note FROM charts").await.is_err(),
        "a per-partition role installs no partitioned table until an admin grants a partition"
    );
    admin
        .session
        .grant_partition(provisioned.identity, ward("A"))
        .await?;
    synced(&admin).await?;
    synced(&clinician).await?;
    synced(&clinician).await?;
    assert_eq!(
        rows(&clinician, "SELECT note FROM charts").await?,
        ["chart in A"]
    );
    insert(&clinician, "charts", ["A", "seen"]).await?;
    assert!(
        insert(&clinician, "charts", ["B", "forged"]).await.is_err(),
        "a clinician writes no ward it was not granted"
    );
    assert!(
        clinician
            .session
            .grant_partition(provisioned.identity, ward("B"))
            .await
            .is_err(),
        "only an admin grants a partition"
    );
    assert!(
        admin
            .session
            .grant_partition(admin.session.info().await?.principal, ward("B"))
            .await
            .is_err(),
        "whole access already holds every partition"
    );
    clinician.close().await;
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn an_unknown_or_reserved_role_is_refused() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = venue()?;
    let admin = Device::new(Database::create(
        open(&root.path().join("admin"), &schema, &hub, "admin"),
        None,
    )?)?;
    assert!(
        admin
            .session
            .provision(Access::Role(RoleName::new("usher")?), None)
            .await
            .is_err()
    );
    for reserved in ["admin", "Full", "read", "store", "not a name"] {
        assert!(RoleName::new(reserved).is_err(), "{reserved}");
    }
    admin.close().await;
    Ok(())
}
