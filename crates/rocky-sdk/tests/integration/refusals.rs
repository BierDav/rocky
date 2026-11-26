use crate::common::{Device, Failure};
use rocky_sdk::{
    Access, Affinity, Column, Configuration, DenyReason, ErrorKind, IdentityKey, Invite, Network,
    Partition, Right, Role, RoleName, Schema, SqlValue, Table, Unsupported, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

const SCANNER: RoleName = RoleName::from_static("scanner");
const CLINICIAN: RoleName = RoleName::from_static("clinician");
const AUDITOR: RoleName = RoleName::from_static("auditor");

fn schema() -> Result<Schema, Failure> {
    Ok(Schema::builder("refusals", 1)
        .table(Table::replayed("tickets").column(Column::new("code", Affinity::Text).required()))
        .table(Table::replayed("check_ins").column(Column::new("gate", Affinity::Text).required()))
        .table(
            Table::replayed("charts")
                .column(Column::new("ward", Affinity::Text).required())
                .column(Column::new("note", Affinity::Text))
                .primary_key(["ward", "id"])
                .partition_by(["ward"])
                .partition_group("ward"),
        )
        .role(Role::new(SCANNER).read("tickets").append("check_ins"))
        .role(Role::new(CLINICIAN).write("charts").per_partition("ward"))
        .role(Role::new(AUDITOR).read("charts").per_partition("ward"))
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
    async fn holding(&self, role: RoleName, wards: &[&str]) -> Result<Device, Failure> {
        let device = self.invited(role).await?;
        let person = device.database.me().await?.person;
        for name in wards {
            self.admin
                .database
                .grant_partition(person, &ward(name))
                .await?;
        }
        for device in [&self.admin, &device, &device] {
            crate::support::peerless_ok(device.sync_now().await)?;
        }
        Ok(device)
    }
    async fn chart(&self, name: &str, note: &str) -> Result<(), Failure> {
        self.admin
            .database
            .write(async |tx| {
                let id = tx
                    .next_id("charts", vec![SqlValue::Text(name.into())])
                    .await?;
                tx.execute(
                    "INSERT INTO charts(ward,id,note) VALUES(?,?,?)",
                    vec![
                        SqlValue::Text(name.into()),
                        id.sql(),
                        SqlValue::Text(note.into()),
                    ],
                )
                .await
            })
            .await?;
        Ok(())
    }
    async fn close(self, devices: impl IntoIterator<Item = Device>) {
        for device in devices {
            device.close().await;
        }
        self.admin.close().await;
    }
}
async fn write(device: &Device, sql: &str, params: Vec<SqlValue>) -> Result<(), rocky_sdk::Error> {
    device
        .database
        .write(async |tx| tx.execute(sql, params).await)
        .await
}
async fn insert(device: &Device, table: &str, value: &str) -> Result<(), rocky_sdk::Error> {
    let column = if table == "check_ins" { "gate" } else { "code" };
    device
        .database
        .write(async |tx| {
            tx.execute(
                &format!("INSERT INTO {table}({column}) VALUES(?)"),
                vec![SqlValue::Text(value.into())],
            )
            .await
        })
        .await
}
async fn allocate(
    device: &Device,
    table: &str,
    partition: Vec<SqlValue>,
) -> Result<(), rocky_sdk::Error> {
    device
        .database
        .write(async |tx| tx.next_id(table, partition).await.map(drop))
        .await
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
async fn notes(device: &Device, sql: &str) -> Result<Vec<String>, Failure> {
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
async fn a_missing_table_right_is_not_reported_as_a_missing_partition() -> Result<(), Failure> {
    let venue = Venue::new().await?;
    venue.admin.database.create_partition(&ward("A")).await?;
    venue.chart("A", "admitted").await?;
    let auditor = venue.holding(AUDITOR, &["A"]).await?;
    assert_eq!(
        notes(&auditor, "SELECT note FROM charts WHERE ward = 'A'").await?,
        ["admitted"]
    );
    let updated = write(
        &auditor,
        "UPDATE charts SET note='discharged' WHERE ward='A'",
        Vec::new(),
    )
    .await;
    assert_eq!(
        refused(updated)?,
        denied(
            Some("charts"),
            Right::Update("note".into()),
            DenyReason::NotGranted
        )
    );
    assert_eq!(
        refused(allocate(&auditor, "charts", vec![SqlValue::Text("A".into())]).await)?,
        denied(Some("charts"), Right::Allocate, DenyReason::NotGranted)
    );
    let deleted = write(&auditor, "DELETE FROM charts WHERE ward='A'", Vec::new()).await;
    assert_eq!(
        refused(deleted)?,
        denied(Some("charts"), Right::Write, DenyReason::NotGranted)
    );
    venue.close([auditor]).await;
    Ok(())
}

#[tokio::test]
async fn a_write_into_a_partition_not_held_is_a_final_partition_refusal() -> Result<(), Failure> {
    let venue = Venue::new().await?;
    for name in ["A", "B", "C"] {
        venue.admin.database.create_partition(&ward(name)).await?;
        venue.chart(name, "admitted").await?;
    }
    let clinician = venue.holding(CLINICIAN, &["A", "C"]).await?;
    let person = clinician.database.me().await?.person;
    venue
        .admin
        .database
        .revoke_partition(person, &ward("C"))
        .await?;
    let _refused_until_the_clinician_renews = venue.admin.database.sync_now().await;
    for device in [&clinician, &venue.admin, &clinician] {
        crate::support::peerless_ok(device.sync_now().await)?;
    }
    let partition_not_granted = |need: Right| {
        denied(
            Some("charts"),
            need,
            DenyReason::PartitionNotGranted {
                group: "ward".into(),
            },
        )
    };
    for never_or_no_longer_held in ["B", "C"] {
        assert_eq!(
            refused(
                allocate(
                    &clinician,
                    "charts",
                    vec![SqlValue::Text(never_or_no_longer_held.into())]
                )
                .await
            )?,
            partition_not_granted(Right::Allocate),
            "ward {never_or_no_longer_held}"
        );
    }
    let inserted = clinician
        .database
        .write(async |tx| {
            let id = tx
                .next_id("charts", vec![SqlValue::Text("A".into())])
                .await?;
            tx.execute(
                "INSERT INTO charts(ward,id,note) VALUES('B',?,'moved')",
                vec![id.sql()],
            )
            .await
        })
        .await;
    assert_eq!(refused(inserted)?, partition_not_granted(Right::Append));
    let updated = write(
        &clinician,
        "UPDATE charts SET note='discharged' WHERE ward='B'",
        Vec::new(),
    )
    .await;
    assert_eq!(
        refused(updated)?,
        partition_not_granted(Right::Update("note".into()))
    );
    write(
        &clinician,
        "UPDATE charts SET note='discharged' WHERE ward='A'",
        Vec::new(),
    )
    .await?;
    venue.close([clinician]).await;
    Ok(())
}

#[tokio::test]
async fn a_read_holds_only_the_rows_of_partitions_held_whether_filtered_or_not()
-> Result<(), Failure> {
    let venue = Venue::new().await?;
    for name in ["A", "B"] {
        venue.admin.database.create_partition(&ward(name)).await?;
        venue.chart(name, &format!("chart in {name}")).await?;
    }
    let clinician = venue.holding(CLINICIAN, &["A"]).await?;
    assert_eq!(
        notes(&clinician, "SELECT note FROM charts").await?,
        ["chart in A"]
    );
    assert_eq!(
        notes(&clinician, "SELECT note FROM charts WHERE ward = 'A'").await?,
        ["chart in A"]
    );
    assert_eq!(
        notes(&clinician, "SELECT note FROM charts WHERE ward = 'B'").await?,
        Vec::<String>::new()
    );
    assert_eq!(
        notes(
            &clinician,
            "SELECT note FROM charts WHERE ward IN ('A', 'B') ORDER BY ward"
        )
        .await?,
        ["chart in A"]
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

#[tokio::test(flavor = "multi_thread")]
async fn a_statement_over_the_intent_limit_fails_at_execute_and_a_blob_counts_its_own_size()
-> Result<(), Failure> {
    let schema = Schema::builder("files", 1)
        .table(Table::replayed("files").column(Column::new("data", Affinity::Blob).required()))
        .build()?;
    let root = tempfile::tempdir()?;
    let database = Configuration::new(root.path().join("files"), &schema)?
        .create()
        .await?
        .database;
    let insert = "INSERT INTO files(id, data) VALUES(?, ?)";
    database
        .write(async |tx| {
            let id = tx.next_id("files", vec![]).await?;
            tx.execute(insert, vec![id.sql(), SqlValue::Blob(vec![7; 100 * 1024])])
                .await
        })
        .await?;
    let refused = database
        .write(async |tx| {
            let id = tx.next_id("files", vec![]).await?;
            tx.execute(insert, vec![id.sql(), SqlValue::Blob(vec![7; 200 * 1024])])
                .await
        })
        .await
        .err()
        .ok_or("a 200 KiB blob fit in one intent")?;
    assert!(
        matches!(
            refused.kind(),
            ErrorKind::TooLarge {
                limit: rocky_sdk::Limit::Intent,
                max: 131_072,
                size,
            } if *size > 200 * 1024 && *size < 201 * 1024
        ),
        "{refused:?}"
    );
    database.close().await?;
    Ok(())
}
