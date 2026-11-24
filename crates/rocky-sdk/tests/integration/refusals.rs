use crate::common::{Device, Failure};
use rocky_sdk::{
    Access, Affinity, Column, Configuration, DenyReason, ErrorKind, IdentityKey, Invite, Network,
    Partition, Right, Role, RoleName, Schema, SqlValue, Table, Unsupported, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

const SCANNER: RoleName = RoleName::from_static("scanner");
const CLINICIAN: RoleName = RoleName::from_static("clinician");

fn schema() -> Result<Schema, Failure> {
    Ok(Schema::builder("refusals", 1)
        .table(Table::replayed("tickets").column(Column::new("code", Affinity::Text).required()))
        .table(Table::replayed("check_ins").column(Column::new("gate", Affinity::Text).required()))
        .table(
            Table::replayed("charts")
                .column(Column::new("ward", Affinity::Text).required())
                .primary_key(["ward", "id"])
                .partition_by(["ward"])
                .partition_group("ward"),
        )
        .role(Role::new(SCANNER).read("tickets").append("check_ins"))
        .role(Role::new(CLINICIAN).write("charts").per_partition("ward"))
        .build()?)
}
fn ward(name: &str) -> Partition {
    Partition::new("ward", vec![SqlValue::Text(name.into())])
}
fn denied(table: Option<&str>, need: Right, reason: DenyReason) -> ErrorKind {
    ErrorKind::Denied {
        table: table.map(Into::into),
        need,
        reason,
    }
}

struct Venue {
    root: tempfile::TempDir,
    hub: Arc<Hub>,
    schema: Schema,
    admin: Device,
}
impl Venue {
    async fn new() -> Result<Self, Failure> {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let schema = schema()?;
        let admin = Device::new(
            Self::open(root.path(), &schema, &hub, "admin")
                .create()
                .await?,
        )?;
        Ok(Self {
            root,
            hub,
            schema,
            admin,
        })
    }
    fn open(root: &Path, schema: &Schema, hub: &Arc<Hub>, name: &str) -> Configuration {
        crate::common::configuration(
            &root.join(name),
            schema,
            None,
            None,
            Some(Network::Owned(hub.connector(name))),
        )
    }
    async fn invited(&self, role: RoleName) -> Result<Device, Failure> {
        let invitation = self
            .admin
            .database
            .invite(Invite::new(Access::Role(role.clone())))
            .await?;
        let device = Device::new(
            Self::open(self.root.path(), &self.schema, &self.hub, role.as_str())
                .join(&invitation.code)
                .await?,
        )?;
        crate::support::peerless_ok(device.sync_now().await)?;
        Ok(device)
    }
    async fn close(self, devices: impl IntoIterator<Item = Device>) {
        for device in devices {
            device.close().await;
        }
        self.admin.close().await;
    }
}
async fn write(device: &Device, sql: &str, params: Vec<SqlValue>) -> Result<(), rocky_sdk::Error> {
    let mut tx = device.database.transaction().await?;
    tx.execute(sql, params).await?;
    tx.commit().await?;
    Ok(())
}
async fn insert(device: &Device, table: &str, value: &str) -> Result<(), rocky_sdk::Error> {
    let column = if table == "check_ins" { "gate" } else { "code" };
    let mut tx = device.database.transaction().await?;
    let id = tx.next_id(table, Vec::new()).await?;
    tx.execute(
        &format!("INSERT INTO {table}({column},id) VALUES(?,?)"),
        vec![SqlValue::Text(value.into()), id.sql()],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
async fn allocate(
    device: &Device,
    table: &str,
    partition: Vec<SqlValue>,
) -> Result<(), rocky_sdk::Error> {
    let mut tx = device.database.transaction().await?;
    tx.next_id(table, partition).await?;
    tx.rollback().await
}
async fn read(device: &Device, sql: &str) -> Result<(), rocky_sdk::Error> {
    device
        .database
        .query(sql, Vec::new())
        .await?
        .collect()
        .await?;
    Ok(())
}
fn refused(result: Result<(), rocky_sdk::Error>) -> Result<ErrorKind, Failure> {
    let error = result.err().ok_or("the call was refused")?;
    assert!(!error.retryable(), "a refusal is final: {error}");
    Ok(error.kind().clone())
}

#[tokio::test]
async fn a_write_without_the_right_names_the_table_and_the_right_it_needs() -> Result<(), Failure> {
    let venue = Venue::new().await?;
    let scanner = venue.invited(SCANNER).await?;
    let ticket = write(
        &scanner,
        "INSERT INTO tickets(code,id) VALUES('T1',?)",
        vec![SqlValue::Blob(vec![0; 40])],
    )
    .await;
    assert_eq!(
        refused(ticket)?,
        denied(Some("tickets"), Right::Append, DenyReason::NotGranted)
    );
    insert(&scanner, "check_ins", "north").await?;
    let moved = write(&scanner, "UPDATE check_ins SET gate='south'", Vec::new()).await;
    assert_eq!(
        refused(moved)?,
        denied(
            Some("check_ins"),
            Right::Update("gate".into()),
            DenyReason::NotGranted
        )
    );
    let deleted = write(&scanner, "DELETE FROM check_ins", Vec::new()).await;
    assert_eq!(
        refused(deleted)?,
        denied(Some("check_ins"), Right::Write, DenyReason::NotGranted)
    );
    venue.close([scanner]).await;
    Ok(())
}

#[tokio::test]
async fn a_read_without_the_right_names_the_table() -> Result<(), Failure> {
    let venue = Venue::new().await?;
    let scanner = venue.invited(SCANNER).await?;
    assert_eq!(
        refused(read(&scanner, "SELECT ward FROM charts").await)?,
        denied(Some("charts"), Right::Read, DenyReason::NotGranted)
    );
    venue.close([scanner]).await;
    Ok(())
}

#[tokio::test]
async fn a_store_only_device_reading_is_refused_as_store_only() -> Result<(), Failure> {
    let venue = Venue::new().await?;
    let invitation = venue
        .admin
        .database
        .invite(Invite::new(Access::Store))
        .await?;
    let store = Device::new(
        Configuration::store_only(venue.root.path().join("store"))
            .network(Network::Owned(venue.hub.connector("store")))
            .identity(Arc::new(IdentityKey::generate()))
            .join(&invitation.code)
            .await?,
    )?;
    assert_eq!(
        refused(read(&store, "SELECT code FROM tickets").await)?,
        denied(Some("tickets"), Right::Read, DenyReason::StoreOnly)
    );
    venue.close([store]).await;
    Ok(())
}

#[tokio::test]
async fn allocating_a_row_id_names_the_right_or_the_partition_it_needs() -> Result<(), Failure> {
    let venue = Venue::new().await?;
    let scanner = venue.invited(SCANNER).await?;
    assert_eq!(
        refused(allocate(&scanner, "tickets", Vec::new()).await)?,
        denied(Some("tickets"), Right::Allocate, DenyReason::NotGranted)
    );
    let clinician = venue.invited(CLINICIAN).await?;
    assert_eq!(
        refused(allocate(&clinician, "charts", vec![SqlValue::Text("B".into())]).await)?,
        denied(
            Some("charts"),
            Right::Allocate,
            DenyReason::PartitionNotGranted {
                group: "ward".into()
            }
        )
    );
    assert_eq!(
        refused(allocate(&venue.admin, "charts", vec![SqlValue::Text("Z".into())]).await)?,
        denied(
            Some("charts"),
            Right::Allocate,
            DenyReason::PartitionNotCreated {
                group: "ward".into()
            }
        )
    );
    venue.admin.database.create_partition(&ward("Z")).await?;
    allocate(&venue.admin, "charts", vec![SqlValue::Text("Z".into())]).await?;
    venue.close([scanner, clinician]).await;
    Ok(())
}

#[tokio::test]
async fn reading_a_partition_not_held_names_the_group() -> Result<(), Failure> {
    let venue = Venue::new().await?;
    for name in ["A", "B"] {
        venue.admin.database.create_partition(&ward(name)).await?;
    }
    let clinician = venue.invited(CLINICIAN).await?;
    let person = clinician.database.me().await?.person;
    venue
        .admin
        .database
        .grant_partition(person, &ward("A"))
        .await?;
    for device in [&venue.admin, &clinician, &clinician] {
        crate::support::peerless_ok(device.sync_now().await)?;
    }
    read(&clinician, "SELECT ward FROM charts WHERE ward = 'A'").await?;
    assert_eq!(
        refused(read(&clinician, "SELECT ward FROM charts WHERE ward = 'B'").await)?,
        denied(
            Some("charts"),
            Right::Read,
            DenyReason::PartitionNotGranted {
                group: "ward".into()
            }
        )
    );
    venue.close([clinician]).await;
    Ok(())
}

#[tokio::test]
async fn an_authorizer_refusal_of_a_statement_rocky_never_runs_is_an_unsupported_statement()
-> Result<(), Failure> {
    let venue = Venue::new().await?;
    for sql in [
        "SELECT * FROM sqlite_master",
        "ATTACH DATABASE ':memory:' AS other",
        "PRAGMA writable_schema=1",
        "CREATE TABLE extra(a)",
        "SELECT load_extension('x')",
    ] {
        let kind = refused(write(&venue.admin, sql, Vec::new()).await)?;
        assert!(
            matches!(
                kind,
                ErrorKind::Sql {
                    code: 23,
                    unsupported: Some(Unsupported::Statement),
                    ..
                }
            ),
            "{sql}: {kind:?}"
        );
    }
    venue.close([]).await;
    Ok(())
}

#[tokio::test]
async fn a_call_that_signs_for_the_person_without_their_identity_is_denied() -> Result<(), Failure>
{
    let venue = Venue::new().await?;
    let linking = Configuration::new(venue.root.path().join("laptop"), &venue.schema)?
        .network(Network::Owned(venue.hub.connector("laptop")))
        .link("unused")
        .await
        .map(drop);
    assert_eq!(
        refused(linking)?,
        denied(None, Right::Sign, DenyReason::IdentityRequired)
    );
    venue.close([]).await;
    Ok(())
}
