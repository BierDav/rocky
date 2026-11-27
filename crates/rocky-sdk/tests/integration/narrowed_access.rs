use crate::common::{Device, Failure};
use rocky_sdk::{
    Access, Affinity, Column, Configuration, DenyReason, ErrorKind, Invite, Logout, Network,
    Partition, PersonId, Right, Role, RoleName, Schema, SqlValue, Table, transport::memory::Hub,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

const CLINICIAN: RoleName = RoleName::from_static("clinician");
const DOCTOR: RoleName = RoleName::from_static("doctor");
const NURSE: RoleName = RoleName::from_static("nurse");

fn charted(name: &str, column: &str) -> Table {
    Table::replayed(name)
        .column(Column::new("patient", Affinity::Text).required())
        .column(Column::new(column, Affinity::Text).required())
        .primary_key(["patient", "id"])
        .partition_by(["patient"])
        .partition_group("chart")
}
fn schema() -> Result<Schema, Failure> {
    Ok(Schema::builder("clinic", 1)
        .table(charted("notes", "body"))
        .table(charted("prescriptions", "drug"))
        .table(Table::replayed("wards").column(Column::new("name", Affinity::Text).required()))
        .table(Table::replayed("rotas").column(Column::new("shift", Affinity::Text).required()))
        .role(
            Role::new(CLINICIAN)
                .write("notes")
                .write("prescriptions")
                .per_partition("chart"),
        )
        .role(Role::new(DOCTOR).read("wards").read("rotas"))
        .role(Role::new(NURSE).read("wards"))
        .build()?)
}
fn chart(patient: &str) -> Partition {
    Partition::new("chart", vec![SqlValue::Text(patient.into())])
}
fn chart_not_granted() -> ErrorKind {
    ErrorKind::Denied {
        table: Some("notes".into()),
        need: Right::Read,
        reason: DenyReason::PartitionNotGranted {
            group: "chart".into(),
        },
    }
}

struct Clinic {
    root: tempfile::TempDir,
    hub: Arc<Hub>,
    schema: Schema,
    admin: Device,
}
impl Clinic {
    async fn new() -> Result<Self, Failure> {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let schema = schema()?;
        let admin = Device::new(
            Self::configuration(root.path(), &schema, &hub, "admin")
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
    fn configuration(root: &Path, schema: &Schema, hub: &Arc<Hub>, name: &str) -> Configuration {
        crate::common::configuration(
            &root.join(name),
            schema,
            None,
            None,
            Some(Network::Owned(hub.connector(name))),
        )
    }
    fn directory(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    async fn invited(&self, name: &str, access: Access) -> Result<Device, Failure> {
        let invitation = self.admin.database.invite(Invite::new(access)).await?;
        let device = Device::new(
            Self::configuration(self.root.path(), &self.schema, &self.hub, name)
                .fixture_person()
                .join(&invitation.code)
                .await?,
        )?;
        crate::support::peerless_ok(device.sync_now().await)?;
        Ok(device)
    }
    async fn reopen(&self, name: &str) -> Result<Device, Failure> {
        Device::new(
            Self::configuration(self.root.path(), &self.schema, &self.hub, name)
                .open()
                .await?,
        )
    }
    async fn clinician(&self, patients: &[&str]) -> Result<(Device, PersonId), Failure> {
        let device = self.invited("clinician", Access::Role(CLINICIAN)).await?;
        let person = device.database.me().await?.person;
        for patient in patients {
            self.admin
                .database
                .grant_partition(person, &chart(patient))
                .await?;
        }
        self.exchange(&device).await?;
        Ok((device, person))
    }
    async fn exchange(&self, device: &Device) -> Result<(), Failure> {
        let _refused_until_the_device_renews = self.admin.database.sync_now().await;
        for device in [device, &self.admin, device] {
            crate::support::peerless_ok(device.sync_now().await)?;
        }
        Ok(())
    }
    async fn admit(&self, patient: &str, note: &str, drug: &str) -> Result<(), Failure> {
        self.admin
            .database
            .create_partition(&chart(patient))
            .await?;
        self.admin
            .database
            .write(async |tx| {
                for (table, column, value) in
                    [("notes", "body", note), ("prescriptions", "drug", drug)]
                {
                    let id = tx
                        .next_id_in(table, vec![SqlValue::Text(patient.into())])
                        .await?;
                    tx.execute(
                        &format!("INSERT INTO {table}(patient,id,{column}) VALUES(?,?,?)"),
                        vec![
                            SqlValue::Text(patient.into()),
                            id.sql(),
                            SqlValue::Text(value.into()),
                        ],
                    )
                    .await?;
                }
                Ok::<_, Failure>(())
            })
            .await
    }
    async fn close(self, devices: impl IntoIterator<Item = Device>) {
        for device in devices {
            device.close().await;
        }
        self.admin.close().await;
    }
}

async fn texts(device: &Device, sql: &str) -> Result<Vec<String>, rocky_sdk::Error> {
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
fn refusal(result: Result<Vec<String>, rocky_sdk::Error>) -> Result<ErrorKind, Failure> {
    Ok(result.err().ok_or("the read was refused")?.kind().clone())
}

fn stored_text(directory: &Path) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    for name in ["rocky.sqlite", "rocky.sqlite-wal"] {
        match std::fs::read(directory.join(name)) {
            Ok(content) => bytes.extend(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(bytes)
}
fn file_holds(directory: &Path, needle: &str) -> Result<bool, Failure> {
    let bytes = stored_text(directory)?;
    Ok(bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes()))
}
fn rows_holding(directory: &Path, needle: &str) -> Result<Vec<String>, Failure> {
    let file = rusqlite::Connection::open_with_flags(
        directory.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let tables: Vec<String> = file
        .prepare("SELECT name FROM sqlite_schema WHERE type='table'")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let mut found = Vec::new();
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let mut statement = file.prepare(&format!("SELECT * FROM \"{quoted}\""))?;
        let width = statement.column_count();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            for index in 0..width {
                let bytes = match row.get_ref(index)? {
                    rusqlite::types::ValueRef::Text(bytes)
                    | rusqlite::types::ValueRef::Blob(bytes) => bytes,
                    _ => continue,
                };
                if bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
                {
                    found.push(table.clone());
                }
            }
        }
    }
    Ok(found)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_loses_its_last_partition_of_a_table_reopens_and_reads_none()
-> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    clinic.admit("ada", "ada note", "ada drug").await?;
    let (clinician, person) = clinic.clinician(&["ada"]).await?;
    assert_eq!(
        texts(&clinician, "SELECT body FROM notes").await?,
        ["ada note"]
    );
    clinic
        .admin
        .database
        .revoke_partition(person, &chart("ada"))
        .await?;
    clinic.exchange(&clinician).await?;
    assert_eq!(
        refusal(texts(&clinician, "SELECT body FROM notes").await)?,
        chart_not_granted()
    );
    clinician.close().await;

    let reopened = clinic.reopen("clinician").await?;
    assert_eq!(
        refusal(texts(&reopened, "SELECT body FROM notes").await)?,
        chart_not_granted(),
        "the reopened device reads no charts"
    );
    clinic
        .admin
        .database
        .grant_partition(person, &chart("ada"))
        .await?;
    clinic.exchange(&reopened).await?;
    assert_eq!(
        texts(&reopened, "SELECT body FROM notes").await?,
        ["ada note"],
        "a partition granted again is read again"
    );
    clinic.close([reopened]).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partition_revoked_while_the_device_is_offline_is_purged_when_it_learns_it()
-> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    clinic.admit("ada", "ada note", "ada drug").await?;
    let (clinician, person) = clinic.clinician(&["ada"]).await?;
    clinician.close().await;
    clinic
        .admin
        .database
        .revoke_partition(person, &chart("ada"))
        .await?;
    let offline = clinic.reopen("clinician").await?;
    assert_eq!(
        texts(&offline, "SELECT body FROM notes").await?,
        ["ada note"],
        "a device that has not synced cannot know of the revocation yet"
    );
    clinic.exchange(&offline).await?;
    offline.close().await;
    assert!(rows_holding(&clinic.directory("clinician"), "ada note")?.is_empty());
    let reopened = clinic.reopen("clinician").await?;
    assert_eq!(
        refusal(texts(&reopened, "SELECT body FROM notes").await)?,
        chart_not_granted()
    );
    clinic.close([reopened]).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_whose_role_narrows_reopens() -> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    clinic
        .admin
        .database
        .write(async |tx| {
            tx.execute("INSERT INTO wards(name) VALUES('north ward')", Vec::new())
                .await?;
            tx.execute("INSERT INTO rotas(shift) VALUES('night rota')", Vec::new())
                .await
        })
        .await?;
    let doctor = clinic.invited("doctor", Access::Role(DOCTOR)).await?;
    assert_eq!(
        texts(&doctor, "SELECT shift FROM rotas").await?,
        ["night rota"]
    );
    let person = doctor.database.me().await?.person;
    clinic
        .admin
        .database
        .set_access(person, Access::Role(NURSE))
        .await?;
    clinic.exchange(&doctor).await?;
    doctor.close().await;
    assert!(rows_holding(&clinic.directory("doctor"), "night rota")?.is_empty());

    let reopened = clinic.reopen("doctor").await?;
    assert_eq!(
        texts(&reopened, "SELECT name FROM wards").await?,
        ["north ward"]
    );
    assert_eq!(
        refusal(texts(&reopened, "SELECT shift FROM rotas").await)?,
        ErrorKind::Denied {
            table: Some("rotas".into()),
            need: Right::Read,
            reason: DenyReason::NotGranted,
        }
    );
    clinic.close([reopened]).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_partition_deletes_its_rows_from_the_device() -> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    clinic.admit("ada", "ada note", "ada drug").await?;
    clinic.admit("bob", "bob note", "bob drug").await?;
    let (clinician, person) = clinic.clinician(&["ada", "bob"]).await?;
    let directory = clinic.directory("clinician");
    assert_eq!(
        texts(&clinician, "SELECT body FROM notes ORDER BY body").await?,
        ["ada note", "bob note"]
    );
    assert!(
        !rows_holding(&directory, "ada drug")?.is_empty(),
        "the device held the partition's rows before the revocation"
    );
    clinic
        .admin
        .database
        .revoke_partition(person, &chart("ada"))
        .await?;
    clinic.exchange(&clinician).await?;
    for needle in ["ada note", "ada drug"] {
        assert_eq!(
            rows_holding(&directory, needle)?,
            Vec::<String>::new(),
            "{needle}"
        );
    }
    assert_eq!(
        texts(&clinician, "SELECT body FROM notes").await?,
        ["bob note"]
    );
    clinician.close().await;
    for needle in ["ada note", "ada drug"] {
        assert!(
            !file_holds(&directory, needle)?,
            "{needle} stays in the file"
        );
    }
    assert!(file_holds(&directory, "bob drug")?);
    clinic.close([]).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_device_keeps_no_replicated_rows_on_disk() -> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    clinic
        .admin
        .database
        .write(async |tx| {
            tx.execute("INSERT INTO wards(name) VALUES('north ward')", Vec::new())
                .await
        })
        .await?;
    let member = clinic.invited("member", Access::Full).await?;
    assert_eq!(
        texts(&member, "SELECT name FROM wards").await?,
        ["north ward"]
    );
    let person = member.database.me().await?.person;
    member.database.detach().await?;
    member
        .database
        .write(async |tx| {
            tx.execute("INSERT INTO wards(name) VALUES('unsent ward')", Vec::new())
                .await
        })
        .await?;
    clinic.admin.database.remove(person).await?;
    member
        .database
        .attach(Network::Owned(clinic.hub.connector("member")))
        .await?;
    let removed = member
        .sync_now()
        .await
        .err()
        .ok_or("a removed device synced")?;
    assert_eq!(removed.kind(), &ErrorKind::Removed, "{removed}");
    member.close().await;
    let directory = clinic.directory("member");
    for needle in ["north ward", "unsent ward"] {
        assert_eq!(rows_holding(&directory, needle)?, Vec::<String>::new());
        assert!(
            !file_holds(&directory, needle)?,
            "{needle} stays in the file"
        );
    }
    let reopened = clinic.reopen("member").await?;
    let refused = texts(&reopened, "SELECT name FROM wards")
        .await
        .err()
        .ok_or("a removed device read")?;
    assert_eq!(refused.kind(), &ErrorKind::Removed, "{refused}");
    clinic.close([reopened]).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn purge_on_close_removes_the_directory() -> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    let member = clinic.invited("member", Access::Read).await?;
    member.database.logout(Logout::PurgeOnClose).await?;
    member.close().await;
    let directory = clinic.directory("member");
    assert!(
        !directory.exists(),
        "left behind: {:?}",
        std::fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?
    );
    clinic.close([]).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_device_holds_no_rows_once_it_reports_removed() -> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    clinic.admit("ada", "ada note", "ada drug").await?;
    let (clinician, person) = clinic.clinician(&["ada"]).await?;
    let directory = clinic.directory("clinician");
    assert_eq!(
        texts(&clinician, "SELECT body FROM notes").await?,
        ["ada note"]
    );
    clinic.admin.database.remove(person).await?;
    let mut removed = None;
    for _ in 0..4 {
        if let Err(error) = clinician.sync_now().await
            && error.kind() == &ErrorKind::Removed
        {
            removed = Some(error);
            break;
        }
    }
    let removed = removed.ok_or("the device never reported removed")?;
    assert_eq!(removed.kind(), &ErrorKind::Removed, "{removed}");
    for needle in ["ada note", "ada drug"] {
        assert!(
            !file_holds(&directory, needle)?,
            "{needle} stays in the file of an open device that reports removed"
        );
    }
    assert_eq!(
        refusal(texts(&clinician, "SELECT body FROM notes").await)?,
        ErrorKind::Removed
    );
    clinic.close([clinician]).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_unlinked_device_holds_no_rows_once_it_reports_removed() -> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    clinic.admit("ada", "ada note", "ada drug").await?;
    let (clinician, _) = clinic.clinician(&["ada"]).await?;
    let directory = clinic.directory("clinician");
    assert_eq!(
        texts(&clinician, "SELECT body FROM notes").await?,
        ["ada note"]
    );
    let own = clinician.database.status().await?.authority().node;
    clinician.database.unlink(own).await?;
    assert_eq!(
        refusal(texts(&clinician, "SELECT body FROM notes").await)?,
        ErrorKind::Removed
    );
    for needle in ["ada note", "ada drug"] {
        assert!(
            !file_holds(&directory, needle)?,
            "{needle} stays in the file of a self-unlinked device that reports removed"
        );
    }
    clinic.close([clinician]).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn unsynced_write_dropped_on_revocation_reports_outcome_and_alert_while_synced_outcome_survives()
-> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    clinic.admit("ada", "ada note", "ada drug").await?;
    let (clinician, person) = clinic.clinician(&["ada"]).await?;
    let directory = clinic.directory("clinician");
    let synced_token = clinician
        .database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO notes(patient, body) VALUES('ada', 'synced note')",
                rocky_sdk::params![],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(tx.token())
        })
        .await?;
    clinic.exchange(&clinician).await?;
    clinic
        .admin
        .database
        .revoke_partition(person, &chart("ada"))
        .await?;
    let offline_token = clinician
        .database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO notes(patient, body) VALUES('ada', 'offline note')",
                rocky_sdk::params![],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(tx.token())
        })
        .await?;
    clinician.database.sync_now().await?;
    clinic.exchange(&clinician).await?;
    assert!(
        !texts(&clinic.admin, "SELECT body FROM notes")
            .await?
            .contains(&"offline note".to_owned()),
        "admin should not have the offline note"
    );
    assert_eq!(
        clinician
            .database
            .transaction_outcome(offline_token)
            .await?,
        Some(rocky_sdk::TransactionOutcome::Dropped {
            reason: rocky_sdk::DropReason::Admission,
            sealed: true,
        })
    );
    let synced_outcome = clinician.database.transaction_outcome(synced_token).await?;
    assert!(
        matches!(
            synced_outcome,
            Some(rocky_sdk::TransactionOutcome::Applied { .. })
        ),
        "synced write outcome should survive partition revocation, got {synced_outcome:?}"
    );
    let alerts = clinician.database.status().await?.alerts().to_vec();
    assert!(
        alerts.iter().any(|alert| matches!(
            alert.kind,
            rocky_sdk::status::AlertKind::WriteDropped {
                token,
                reason: rocky_sdk::DropReason::Admission,
                ..
            } if token == offline_token
        )),
        "expected WriteDropped alert for offline write, got {alerts:?}"
    );
    clinician.database.acknowledge_drop(offline_token).await?;
    let after_ack = clinician.database.status().await?.alerts().to_vec();
    assert!(
        !after_ack.iter().any(|alert| matches!(
            alert.kind,
            rocky_sdk::status::AlertKind::WriteDropped { token, .. } if token == offline_token
        )),
        "alert should clear after acknowledge"
    );
    for needle in ["ada note", "synced note", "offline note"] {
        assert!(
            !file_holds(&directory, needle)?,
            "{needle} stays in the file after partition revocation"
        );
    }
    clinic.close([clinician]).await;
    Ok(())
}
