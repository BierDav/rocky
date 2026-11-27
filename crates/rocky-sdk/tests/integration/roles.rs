use crate::common::{self, Device, Failure};
use rocky_sdk::{
    Access, Affinity, Column, DenyReason, ErrorKind, Network, PersonId, Right, Role, RoleName,
    Schema, SqlValue, Table, TableRights, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

const SCANNER: RoleName = RoleName::from_static("scanner");
const CLINICIAN: RoleName = RoleName::from_static("clinician");
const INSPECTOR: RoleName = RoleName::from_static("inspector");

fn venue() -> Result<Schema, Failure> {
    venue_at(
        1,
        Some(Role::new(SCANNER).read("tickets").append("check_ins")),
    )
}
fn venue_at(version: u32, scanner: Option<Role>) -> Result<Schema, Failure> {
    let builder = Schema::builder("venue", version)
        .table(
            Table::replayed("tickets")
                .column(Column::new("code", Affinity::Text).required())
                .column(Column::new("holder", Affinity::Text).required()),
        )
        .table(
            Table::replayed("check_ins")
                .column(Column::new("code", Affinity::Text).required())
                .column(Column::new("gate", Affinity::Text).required()),
        );
    Ok(match scanner {
        Some(role) => builder.role(role),
        None => builder,
    }
    .build()?)
}
async fn venue_with_scanner(root: &Path, hub: &Arc<Hub>) -> Result<(Device, Device), Failure> {
    let schema = venue()?;
    let admin = Device::new(
        open(&root.join("admin"), &schema, hub, "admin")
            .create()
            .await?,
    )?;
    let invitation = admin
        .database
        .invite(rocky_sdk::Invite::new(Access::Role(SCANNER)))
        .await?;
    let scanner = Device::new(
        open(&root.join("scanner"), &schema, hub, "scanner")
            .fixture_person()
            .join(&invitation.code)
            .await?,
    )?;
    synced(&scanner).await?;
    insert(&scanner, "check_ins", ["T1", "north"]).await?;
    synced(&scanner).await?;
    Ok((admin, scanner))
}
async fn check_in_rights(device: &Device) -> Result<Option<TableRights>, Failure> {
    Ok(device.database.me().await?.tables.get("check_ins").cloned())
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
        .role(Role::new(INSPECTOR).write("charts").per_partition("ward"))
        .build()?)
}
fn open<'a>(
    directory: &'a Path,
    schema: &'a Schema,
    hub: &Arc<Hub>,
    name: &str,
) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        schema,
        None,
        None,
        Some(Network::Owned(hub.connector(name))),
    )
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
    let columns = match table {
        "tickets" => "code,holder",
        "check_ins" => "code,gate",
        _ => "ward,note",
    };
    device
        .database
        .write(async |tx| {
            tx.execute(
                &format!("INSERT INTO {table}({columns}) VALUES(?,?)"),
                vec![
                    SqlValue::Text(values[0].into()),
                    SqlValue::Text(values[1].into()),
                ],
            )
            .await
        })
        .await?;
    Ok(())
}
async fn ward_name(device: &Device, name: &str) -> Result<(), Failure> {
    device
        .database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO wards(name) VALUES(?)",
                vec![SqlValue::Text(name.into())],
            )
            .await
        })
        .await?;
    Ok(())
}
async fn write(device: &Device, sql: &str) -> Result<(), Failure> {
    device
        .database
        .write(async |tx| tx.execute(sql, Vec::new()).await)
        .await?;
    Ok(())
}
fn holds_reader_key(directory: &Path, table: &str) -> Result<bool, Failure> {
    let identity = common::authority_json(directory, "identity")?;
    Ok(identity["keys"].get(table).is_some())
}
fn ward(name: &str) -> rocky_sdk::Partition {
    rocky_sdk::Partition::new("ward", vec![SqlValue::Text(name.into())])
}
async fn synced(device: &Device) -> Result<(), Failure> {
    crate::support::peerless_ok(device.sync_now().await)?;
    Ok(())
}

#[tokio::test]
async fn a_scanner_reads_tickets_and_only_appends_check_ins() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = venue()?;
    let admin = Device::new(
        open(&root.path().join("admin"), &schema, &hub, "admin")
            .create()
            .await?,
    )?;
    insert(&admin, "tickets", ["T1", "Ada"]).await?;
    let invitation = admin
        .database
        .invite(rocky_sdk::Invite::new(Access::Role(SCANNER)))
        .await?;
    let scanner_dir = root.path().join("scanner");
    let scanner = Device::new(
        open(&scanner_dir, &schema, &hub, "scanner")
            .fixture_person()
            .join(&invitation.code)
            .await?,
    )?;
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
        scanner.database.me().await?.access,
        Access::Role(SCANNER),
        "the role survives into the installed settings"
    );
    let me = scanner.database.me().await?;
    assert_eq!(me.access, Access::Role(SCANNER));
    assert!(!me.admin);
    assert_eq!(
        me.tables.get("tickets"),
        Some(&TableRights {
            read: true,
            ..TableRights::default()
        })
    );
    assert_eq!(
        me.tables
            .get("check_ins")
            .map(|rights| (rights.insert, rights.update, rights.delete)),
        Some((true, false, false))
    );
    assert_eq!(
        scanner
            .database
            .people()
            .await
            .map_err(|error| error.kind().clone()),
        Err(ErrorKind::Denied {
            table: None,
            need: Right::Administer,
            reason: DenyReason::AdminRequired,
        }),
        "only an admin lists people"
    );
    assert!(admin.database.me().await?.admin);
    scanner.close().await;
    admin.close().await;
    Ok(())
}

async fn person(device: &Device) -> Result<PersonId, Failure> {
    Ok(device.database.me().await?.person)
}

#[tokio::test]
async fn a_person_downgraded_to_read_can_no_longer_write() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = venue()?;
    let admin = Device::new(
        open(&root.path().join("admin"), &schema, &hub, "admin")
            .create()
            .await?,
    )?;
    let invitation = admin
        .database
        .invite(rocky_sdk::Invite::new(Access::Full))
        .await?;
    let member = Device::new(
        open(&root.path().join("member"), &schema, &hub, "member")
            .fixture_person()
            .join(&invitation.code)
            .await?,
    )?;
    synced(&member).await?;
    insert(&member, "tickets", ["T1", "Ada"]).await?;
    synced(&member).await?;
    admin
        .database
        .set_access(person(&member).await?, Access::Read)
        .await?;
    let _refused_until_the_member_renews = admin.sync_now().await;
    let _ = member.sync().await;
    synced(&member).await?;
    assert_eq!(member.database.me().await?.access, Access::Read);
    assert!(
        insert(&member, "tickets", ["T2", "Eve"]).await.is_err(),
        "a person downgraded to Read writes nothing"
    );
    assert_eq!(rows(&member, "SELECT code FROM tickets").await?, ["T1"]);
    insert(&admin, "tickets", ["T3", "Bob"]).await?;
    synced(&admin).await?;
    synced(&member).await?;
    assert_eq!(
        rows(&member, "SELECT code FROM tickets ORDER BY code").await?,
        ["T1", "T3"],
        "a downgraded person still reads"
    );
    member.close().await;
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn a_removed_clinician_reads_no_chart_written_after_removal() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = clinic()?;
    let admin = Device::new(
        open(&root.path().join("admin"), &schema, &hub, "admin")
            .create()
            .await?,
    )?;
    admin.database.create_partition(&ward("A")).await?;
    insert(&admin, "charts", ["A", "before removal"]).await?;
    let invitation = admin
        .database
        .invite(rocky_sdk::Invite::new(Access::Role(CLINICIAN)).partition(ward("A")))
        .await?;
    let clinician = Device::new(
        open(&root.path().join("clinician"), &schema, &hub, "clinician")
            .fixture_person()
            .join(&invitation.code)
            .await?,
    )?;
    synced(&clinician).await?;
    synced(&clinician).await?;
    assert_eq!(
        rows(&clinician, "SELECT note FROM charts").await?,
        ["before removal"]
    );
    admin.database.remove(person(&clinician).await?).await?;
    insert(&admin, "charts", ["A", "after removal"]).await?;
    synced(&admin).await?;
    let _ = clinician.sync().await;
    let _ = clinician.sync().await;
    let read = rows(&clinician, "SELECT note FROM charts").await;
    assert!(
        !read
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|note| note == "after removal"),
        "{read:?}"
    );
    let removed = person(&clinician).await?;
    assert!(
        admin
            .database
            .people()
            .await?
            .iter()
            .all(|listed| listed.id != removed),
        "a removed person is no longer listed"
    );
    clinician.close().await;
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn a_granted_chart_reaches_the_phone_and_the_laptop() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = clinic()?;
    let admin = Device::new(
        open(&root.path().join("admin"), &schema, &hub, "admin")
            .create()
            .await?,
    )?;
    admin.database.create_partition(&ward("A")).await?;
    insert(&admin, "charts", ["A", "chart in A"]).await?;
    let invitation = admin
        .database
        .invite(rocky_sdk::Invite::new(Access::Role(CLINICIAN)))
        .await?;
    let identity: Arc<dyn rocky_sdk::IdentityProvider> =
        Arc::new(rocky_sdk::IdentityKey::generate());
    let opened = open(&root.path().join("phone"), &schema, &hub, "phone")
        .identity(identity.clone())
        .fixture_person()
        .join(&invitation.code)
        .await?;
    let phone = Device::new(opened)?;
    synced(&phone).await?;
    let laptop = Device::new(
        common::link(
            open(&root.path().join("laptop"), &schema, &hub, "laptop"),
            identity,
            &phone.contact().await,
            &phone.database,
        )
        .await?,
    )?;
    synced(&laptop).await?;
    synced(&phone).await?;
    synced(&admin).await?;
    let clinician = person(&phone).await?;
    assert_eq!(person(&laptop).await?, clinician);
    let listed = admin.database.people().await?;
    let devices = listed
        .iter()
        .find(|listed| listed.id == clinician)
        .map(|listed| listed.devices.len());
    assert_eq!(devices, Some(2), "{listed:?}");
    admin
        .database
        .grant_partition(clinician, &ward("A"))
        .await?;
    synced(&admin).await?;
    for device in [&phone, &laptop] {
        synced(device).await?;
        synced(device).await?;
        assert_eq!(
            rows(device, "SELECT note FROM charts").await?,
            ["chart in A"]
        );
    }
    for device in [phone, laptop, admin] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_clinician_reads_only_the_wards_an_admin_granted() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = clinic()?;
    let admin = Device::new(
        open(&root.path().join("admin"), &schema, &hub, "admin")
            .create()
            .await?,
    )?;
    for name in ["A", "B"] {
        admin.database.create_partition(&ward(name)).await?;
        insert(&admin, "charts", [name, &format!("chart in {name}")]).await?;
    }
    ward_name(&admin, "A").await?;
    let invitation = admin
        .database
        .invite(rocky_sdk::Invite::new(Access::Role(CLINICIAN)))
        .await?;
    let clinician = Device::new(
        open(&root.path().join("clinician"), &schema, &hub, "clinician")
            .fixture_person()
            .join(&invitation.code)
            .await?,
    )?;
    synced(&clinician).await?;
    let clinician_identity = clinician.database.me().await?.person.key();
    assert_eq!(rows(&clinician, "SELECT name FROM wards").await?, ["A"]);
    assert!(
        rows(&clinician, "SELECT note FROM charts").await.is_err(),
        "a per-partition role installs no partitioned table until an admin grants a partition"
    );
    admin
        .database
        .grant_partition(rocky_sdk::PersonId::new(clinician_identity), &ward("A"))
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
            .database
            .grant_partition(rocky_sdk::PersonId::new(clinician_identity), &ward("B"))
            .await
            .is_err(),
        "only an admin grants a partition"
    );
    assert!(
        admin
            .database
            .grant_partition(
                rocky_sdk::PersonId::new(admin.database.me().await?.person.key()),
                &ward("B")
            )
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
    let admin = Device::new(
        open(&root.path().join("admin"), &schema, &hub, "admin")
            .create()
            .await?,
    )?;
    assert!(
        admin
            .database
            .invite(rocky_sdk::Invite::new(Access::Role(RoleName::new(
                "usher"
            )?)))
            .await
            .is_err()
    );
    for (name, rule) in [
        ("admin", rocky_sdk::NameRule::Reserved),
        ("Full", rocky_sdk::NameRule::Reserved),
        ("read", rocky_sdk::NameRule::Reserved),
        ("store", rocky_sdk::NameRule::Reserved),
        ("not a name", rocky_sdk::NameRule::Identifier),
    ] {
        assert_eq!(
            RoleName::new(name).err(),
            Some(rocky_sdk::InvalidName::new("roles", name, rule))
        );
    }
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn a_rollout_that_narrows_a_role_needs_confirmation_and_ends_its_writes()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (admin, scanner) = venue_with_scanner(root.path(), &hub).await?;
    admin.close().await;
    scanner.close().await;
    let narrowed = venue_at(2, Some(Role::new(SCANNER).read("tickets")))?;
    let refused = open(&root.path().join("admin"), &narrowed, &hub, "admin")
        .open()
        .await
        .err()
        .ok_or("a narrowing rollout opens only once confirmed")?;
    let ErrorKind::RolloutConfirmation(required) = refused.kind() else {
        return Err(format!("expected a rollout confirmation, found {refused}").into());
    };
    assert_eq!(required.report.narrowed_roles, [SCANNER]);
    let admin = Device::new(
        open(&root.path().join("admin"), &narrowed, &hub, "admin")
            .open_confirmed(&required.report.digest)
            .await?,
    )?;
    synced(&admin).await?;
    let scanner = Device::new(
        open(&root.path().join("scanner"), &narrowed, &hub, "scanner")
            .open()
            .await?,
    )?;
    let _ = scanner.sync().await;
    synced(&scanner).await?;
    assert!(
        !holds_reader_key(&root.path().join("scanner"), "check_ins")?,
        "a narrowed scanner keeps no reader key for check_ins"
    );
    assert_eq!(
        check_in_rights(&scanner).await?,
        None,
        "the narrowed role no longer reaches check_ins"
    );
    assert!(
        insert(&scanner, "check_ins", ["T2", "south"])
            .await
            .is_err(),
        "a narrowed scanner appends nothing"
    );
    scanner.close().await;
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn a_rollout_that_removes_a_held_role_fails_with_role_in_use() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (admin, scanner) = venue_with_scanner(root.path(), &hub).await?;
    let holder = person(&scanner).await?;
    admin.close().await;
    let refused = open(
        &root.path().join("admin"),
        &venue_at(2, None)?,
        &hub,
        "admin",
    )
    .open()
    .await
    .err()
    .ok_or("a rollout keeps every held role")?;
    assert_eq!(
        refused.kind(),
        &ErrorKind::RoleInUse(rocky_sdk::RoleInUse {
            role: SCANNER,
            people: vec![holder],
        })
    );
    scanner.close().await;
    Ok(())
}

#[tokio::test]
async fn a_rollout_that_widens_a_role_reaches_its_holders_at_their_next_sync() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (admin, scanner) = venue_with_scanner(root.path(), &hub).await?;
    admin.close().await;
    scanner.close().await;
    let widened = venue_at(
        2,
        Some(Role::new(SCANNER).read("tickets").write("check_ins")),
    )?;
    let admin = Device::new(
        open(&root.path().join("admin"), &widened, &hub, "admin")
            .open()
            .await?,
    )?;
    synced(&admin).await?;
    let scanner = Device::new(
        open(&root.path().join("scanner"), &widened, &hub, "scanner")
            .open()
            .await?,
    )?;
    synced(&scanner).await?;
    assert_eq!(
        check_in_rights(&scanner).await?.map(|rights| (
            rights.insert,
            rights.update,
            rights.delete
        )),
        Some((true, true, true))
    );
    write(&scanner, "UPDATE check_ins SET gate='south'").await?;
    scanner.close().await;
    admin.close().await;
    Ok(())
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
        let schema = clinic()?;
        let admin = Device::new(
            open(&root.path().join("admin"), &schema, &hub, "admin")
                .create()
                .await?,
        )?;
        admin.database.create_partition(&ward("A")).await?;
        insert(&admin, "charts", ["A", "chart in A"]).await?;
        Ok(Self {
            root,
            hub,
            schema,
            admin,
        })
    }
    fn open(&self, name: &str) -> rocky_sdk::Configuration {
        open(&self.root.path().join(name), &self.schema, &self.hub, name)
    }
    async fn clinician(
        &self,
        name: &str,
    ) -> Result<(Device, Arc<dyn rocky_sdk::IdentityProvider>), Failure> {
        let invitation = self
            .admin
            .database
            .invite(rocky_sdk::Invite::new(Access::Role(CLINICIAN)).label(name))
            .await?;
        let identity: Arc<dyn rocky_sdk::IdentityProvider> =
            Arc::new(rocky_sdk::IdentityKey::generate());
        let opened = self
            .open(name)
            .identity(identity.clone())
            .fixture_person()
            .join(&invitation.code)
            .await?;
        let device = Device::new(opened)?;
        synced(&device).await?;
        Ok((device, identity))
    }
    async fn link(
        &self,
        name: &str,
        identity: Arc<dyn rocky_sdk::IdentityProvider>,
        approver: &Device,
    ) -> Result<Device, Failure> {
        Device::new(
            common::link(
                self.open(name),
                identity,
                &approver.contact().await,
                &approver.database,
            )
            .await?,
        )
    }
    async fn reinvited(
        &self,
        name: &str,
        identity: Arc<dyn rocky_sdk::IdentityProvider>,
        label: &str,
    ) -> Result<Device, Failure> {
        let invitation = self
            .admin
            .database
            .invite(
                rocky_sdk::Invite::new(Access::Role(CLINICIAN))
                    .for_identity(PersonId::new(identity.public_key()?))
                    .label(label),
            )
            .await?;
        Device::new(
            self.open(name)
                .identity(identity)
                .fixture_person()
                .join(&invitation.code)
                .await?,
        )
    }
    async fn close(self, devices: impl IntoIterator<Item = Device>) {
        for device in devices {
            device.close().await;
        }
        self.admin.close().await;
    }
}
async fn reads_ward_a(device: &Device) -> Result<(), Failure> {
    for _ in 0..3 {
        synced(device).await?;
    }
    assert_eq!(
        rows(device, "SELECT note FROM charts").await?,
        ["chart in A"]
    );
    assert!(
        device
            .database
            .me()
            .await?
            .tables
            .get("charts")
            .is_some_and(|rights| rights.read),
        "the device reports read on the granted table"
    );
    Ok(())
}

#[tokio::test]
async fn a_replacement_device_invited_for_the_person_holds_their_granted_partitions()
-> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    let (phone, identity) = clinic.clinician("phone").await?;
    let clinician = person(&phone).await?;
    clinic
        .admin
        .database
        .grant_partition(clinician, &ward("A"))
        .await?;
    synced(&clinic.admin).await?;
    reads_ward_a(&phone).await?;
    let lost = phone.database.me().await?.device;
    phone.close().await;
    clinic.admin.database.unlink(lost).await?;
    let tablet = clinic
        .reinvited("tablet", identity, "replacement tablet")
        .await?;
    synced(&clinic.admin).await?;
    reads_ward_a(&tablet).await?;
    let listed = clinic.admin.database.people().await?;
    let listed = listed
        .iter()
        .find(|listed| listed.id == clinician)
        .ok_or("the clinician is listed")?;
    assert_eq!(listed.partitions, [ward("A")]);
    assert_eq!(
        listed.label.as_deref(),
        Some("phone"),
        "the first invitation labels the person once"
    );
    assert_eq!(
        listed
            .devices
            .iter()
            .map(|device| (device.id, device.label.as_deref()))
            .collect::<Vec<_>>(),
        [(
            tablet.database.me().await?.device,
            Some("replacement tablet")
        )],
        "each invitation labels the device it admits"
    );
    clinic.close([tablet]).await;
    Ok(())
}

#[tokio::test]
async fn a_device_linked_before_its_approver_learns_of_a_grant_still_receives_it()
-> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    let (phone, identity) = clinic.clinician("phone").await?;
    let clinician = person(&phone).await?;
    clinic
        .admin
        .database
        .grant_partition(clinician, &ward("A"))
        .await?;
    synced(&clinic.admin).await?;
    let laptop = clinic.link("laptop", identity, &phone).await?;
    synced(&laptop).await?;
    synced(&clinic.admin).await?;
    synced(&clinic.admin).await?;
    reads_ward_a(&laptop).await?;
    reads_ward_a(&phone).await?;
    clinic.close([phone, laptop]).await;
    Ok(())
}

#[tokio::test]
async fn a_device_linked_after_a_grant_holds_it() -> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    let (phone, identity) = clinic.clinician("phone").await?;
    let clinician = person(&phone).await?;
    clinic
        .admin
        .database
        .grant_partition(clinician, &ward("A"))
        .await?;
    synced(&clinic.admin).await?;
    reads_ward_a(&phone).await?;
    let laptop = clinic.link("laptop", identity, &phone).await?;
    reads_ward_a(&laptop).await?;
    clinic.close([phone, laptop]).await;
    Ok(())
}

async fn shared_recipients(device: &Device) -> Result<Vec<usize>, Failure> {
    let mut recipients = Vec::new();
    let mut after = 0;
    loop {
        let page = device.database.activity(after).await?;
        if page.events.is_empty() {
            return Ok(recipients);
        }
        after = page.token;
        recipients.extend(
            page.events
                .into_iter()
                .filter_map(|event| match event.detail {
                    rocky_sdk::ActivityKind::PartitionShared {
                        partition,
                        recipients,
                    } if partition == ward("A") => Some(recipients),
                    _ => None,
                }),
        );
    }
}

#[tokio::test]
async fn granting_a_held_partition_again_delivers_it_to_every_device_of_the_person()
-> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    let (phone, identity) = clinic.clinician("phone").await?;
    let clinician = person(&phone).await?;
    let laptop = clinic.link("laptop", identity, &phone).await?;
    for device in [&laptop, &phone, &clinic.admin] {
        synced(device).await?;
    }
    clinic
        .admin
        .database
        .grant_partition(clinician, &ward("A"))
        .await?;
    synced(&clinic.admin).await?;
    reads_ward_a(&phone).await?;
    reads_ward_a(&laptop).await?;
    let before = shared_recipients(&clinic.admin).await?;
    clinic
        .admin
        .database
        .grant_partition(clinician, &ward("A"))
        .await?;
    let after = shared_recipients(&clinic.admin).await?;
    assert_eq!(
        after.get(before.len()..),
        Some([2].as_slice()),
        "granting a held partition again delivers it to both devices"
    );
    synced(&clinic.admin).await?;
    reads_ward_a(&phone).await?;
    reads_ward_a(&laptop).await?;
    let listed = clinic.admin.database.people().await?;
    assert_eq!(
        listed
            .iter()
            .find(|listed| listed.id == clinician)
            .map(|listed| listed.partitions.clone()),
        Some(vec![ward("A")]),
        "granting twice records the partition once"
    );
    clinic.close([phone, laptop]).await;
    Ok(())
}

#[tokio::test]
async fn a_partitioned_table_without_a_granted_partition_says_which_grant_clears_it()
-> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    let (phone, _) = clinic.clinician("phone").await?;
    let refused = phone
        .database
        .query("SELECT note FROM charts", Vec::new())
        .await
        .err()
        .ok_or("a per-partition role reads no partition before a grant")?;
    assert_eq!(
        refused.kind(),
        &ErrorKind::Denied {
            table: Some("charts".into()),
            need: Right::Read,
            reason: DenyReason::PartitionNotGranted {
                group: "ward".into()
            },
        },
        "{refused}"
    );
    assert!(
        phone
            .database
            .status()
            .await?
            .blockers()
            .iter()
            .any(|blocker| matches!(
                &blocker.kind,
                rocky_core::BlockerKind::PartitionNotGranted { group } if group == "ward"
            )),
        "status lists the missing grant"
    );
    clinic.close([phone]).await;
    Ok(())
}

#[tokio::test]
async fn a_role_holding_only_partitioned_tables_opens_and_names_the_missing_grant()
-> Result<(), Failure> {
    let clinic = Clinic::new().await?;
    let invitation = clinic
        .admin
        .database
        .invite(rocky_sdk::Invite::new(Access::Role(INSPECTOR)))
        .await?;
    let tablet = Device::new(
        clinic
            .open("tablet")
            .fixture_person()
            .join(&invitation.code)
            .await?,
    )?;
    synced(&tablet).await?;
    let refused = rows(&tablet, "SELECT note FROM charts")
        .await
        .err()
        .ok_or("an inspector reads no partition before a grant")?;
    let refused = refused
        .downcast_ref::<rocky_sdk::Error>()
        .ok_or("a typed SDK error")?;
    assert_eq!(
        refused.kind(),
        &ErrorKind::Denied {
            table: Some("charts".into()),
            need: Right::Read,
            reason: DenyReason::PartitionNotGranted {
                group: "ward".into()
            },
        },
        "{refused}"
    );
    let inspector = person(&tablet).await?;
    clinic
        .admin
        .database
        .grant_partition(inspector, &ward("A"))
        .await?;
    synced(&clinic.admin).await?;
    reads_ward_a(&tablet).await?;
    assert!(
        tablet
            .database
            .status()
            .await?
            .blockers()
            .iter()
            .all(|blocker| !matches!(
                blocker.kind,
                rocky_core::BlockerKind::PartitionNotGranted { .. }
            )),
        "a grant clears the blocker"
    );
    clinic.close([tablet]).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_append_and_an_update_of_named_columns_compose_on_one_table() -> Result<(), Failure> {
    const DISPATCHER: RoleName = RoleName::from_static("dispatcher");
    let schema = Schema::builder("alerts", 1)
        .table(
            Table::replayed("alerts")
                .column(Column::new("title", Affinity::Text).required())
                .column(Column::new("state", Affinity::Text).required()),
        )
        .role(
            Role::new(DISPATCHER)
                .append("alerts")
                .update_columns("alerts", ["state"]),
        )
        .build()?;
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let admin = Device::new(
        open(&root.path().join("admin"), &schema, &hub, "admin")
            .create()
            .await?,
    )?;
    let invitation = admin
        .database
        .invite(rocky_sdk::Invite::new(Access::Role(DISPATCHER)))
        .await?;
    let dispatcher = Device::new(
        open(&root.path().join("dispatcher"), &schema, &hub, "dispatcher")
            .fixture_person()
            .join(&invitation.code)
            .await?,
    )?;
    assert_eq!(
        dispatcher.database.me().await?.tables.get("alerts"),
        Some(&TableRights {
            read: true,
            insert: true,
            update: true,
            delete: false,
        })
    );
    let write = |sql: &'static str, params: Vec<SqlValue>| {
        let database = dispatcher.database.clone();
        async move {
            database
                .write(async |tx| tx.execute(sql, params).await)
                .await
        }
    };
    let id = dispatcher
        .database
        .write(async |tx| {
            let id = tx.next_id("alerts").await?;
            tx.execute(
                "INSERT INTO alerts(id,title,state) VALUES(?,?,?)",
                vec![
                    id.sql(),
                    SqlValue::Text("Fire".into()),
                    SqlValue::Text("open".into()),
                ],
            )
            .await?;
            Ok::<_, Failure>(id)
        })
        .await?;
    write(
        "UPDATE alerts SET state = ? WHERE id = ?",
        vec![SqlValue::Text("closed".into()), id.sql()],
    )
    .await?;
    let renamed = write(
        "UPDATE alerts SET title = ? WHERE id = ?",
        vec![SqlValue::Text("Flood".into()), id.sql()],
    )
    .await
    .err()
    .ok_or("the role updated a column it was not granted")?;
    assert!(
        matches!(
            renamed.kind(),
            ErrorKind::Denied {
                need: Right::Update(column),
                ..
            } if column == "title"
        ),
        "{renamed:?}"
    );
    let deleted = write("DELETE FROM alerts", Vec::new())
        .await
        .err()
        .ok_or("the role deleted a row")?;
    assert!(
        matches!(
            deleted.kind(),
            ErrorKind::Denied {
                need: Right::Write,
                ..
            }
        ),
        "{deleted:?}"
    );
    assert_eq!(
        rows(&dispatcher, "SELECT title || ':' || state FROM alerts").await?,
        ["Fire:closed"]
    );
    dispatcher.close().await;
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn role_reading_child_without_referenced_parent_is_rejected() -> Result<(), Failure> {
    let reader = RoleName::from_static("reader");
    let refused = Schema::builder("ledger", 1)
        .table(Table::replayed("accounts").column(Column::new("name", Affinity::Text).required()))
        .table(
            Table::replayed("transactions").column(
                Column::new("account", Affinity::Blob)
                    .required()
                    .references("accounts"),
            ),
        )
        .role(Role::new(reader.clone()).read("transactions"))
        .build()
        .err()
        .ok_or("a role reading a child without its FK parent was accepted")?;
    assert!(
        matches!(
            refused.kind(),
            ErrorKind::Schema(error)
                if matches!(
                    error.as_ref(),
                    rocky_sdk::SchemaError::Role(
                        rocky_sdk::RoleError::MissingReferencedTable {
                            role,
                            table,
                            parent,
                        }
                    ) if role == "reader" && table == "transactions" && parent == "accounts"
                )
        ),
        "{refused:?}"
    );
    let accepted = Schema::builder("ledger", 1)
        .table(Table::replayed("accounts").column(Column::new("name", Affinity::Text).required()))
        .table(
            Table::replayed("transactions").column(
                Column::new("account", Affinity::Blob)
                    .required()
                    .references("accounts"),
            ),
        )
        .role(Role::new(reader).read("accounts").read("transactions"))
        .build();
    assert!(accepted.is_ok(), "{accepted:?}");
    Ok(())
}
