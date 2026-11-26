use crate::{
    Access, Affinity, Column, Configuration, Database, ErrorKind, Invite, Network, Role, RoleName,
    Schema, SqlValue, Table, database::fault, enrollment::Settings, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};
type Failure = crate::Failure;

const SCANNER: RoleName = RoleName::from_static("scanner");

fn venue(version: u32, scanner: Role) -> Result<Schema, Failure> {
    Ok(Schema::builder("venue", version)
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
        .role(scanner)
        .build()?)
}
fn wide() -> Result<Schema, Failure> {
    venue(1, Role::new(SCANNER).read("tickets").append("check_ins"))
}
fn narrowed() -> Result<Schema, Failure> {
    venue(2, Role::new(SCANNER).read("tickets"))
}
fn configuration(
    directory: &Path,
    schema: &Schema,
    hub: &Arc<Hub>,
) -> Result<Configuration, Failure> {
    let name = directory.to_string_lossy().into_owned();
    Ok(Configuration::new(directory, schema)?
        .network(Network::Owned(hub.connector(&name)))
        .sync(crate::SyncMode::Manual))
}
async fn open_rolled_out(
    directory: &Path,
    schema: &Schema,
    hub: &Arc<Hub>,
) -> Result<Database, crate::Error> {
    let configuration = configuration(directory, schema, hub)
        .map_err(|error| crate::Error::from_failure(&error))?;
    match configuration.open().await {
        Err(error) => match error.kind() {
            ErrorKind::RolloutConfirmation(required) => {
                configuration.open_confirmed(&required.report.digest).await
            }
            _ => Err(error),
        },
        opened => opened,
    }
}
async fn check_in(database: &Database, code: &str) -> Result<(), Failure> {
    Ok(database
        .write(async |tx| {
            let id = tx.next_id("check_ins").await?;
            tx.execute(
                "INSERT INTO check_ins(code,gate,id) VALUES(?,?,?)",
                vec![
                    SqlValue::Text(code.into()),
                    SqlValue::Text("north".into()),
                    id.sql(),
                ],
            )
            .await
        })
        .await?)
}
fn settings(directory: &Path) -> Result<Settings, Failure> {
    Settings::load(&directory.join("rocky.sqlite"))
}
fn check_in_epochs(directory: &Path) -> Result<usize, Failure> {
    Ok(settings(directory)?
        .keys
        .get("check_ins")
        .map_or(0, Vec::len))
}
fn pending_narrowing(directory: &Path) -> Result<bool, Failure> {
    Ok(settings(directory)?
        .authority
        .is_some_and(|authority| authority.rights_change.is_some()))
}
async fn revocations(database: &Database) -> Result<u64, Failure> {
    Ok(database.status().await?.authority.revocations.statements)
}

async fn narrow_through_a_crash_at(stage: fault::Stage) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (admin_dir, scanner_dir) = (root.path().join("admin"), root.path().join("scanner"));
    let admin = configuration(&admin_dir, &wide()?, &hub)?
        .create()
        .await?
        .database;
    let invitation = admin.invite(Invite::new(Access::Role(SCANNER))).await?;
    let scanner = configuration(&scanner_dir, &wide()?, &hub)?
        .join(&invitation.code)
        .await?
        .database;
    scanner.sync_now().await?;
    check_in(&scanner, "T1").await?;
    scanner.sync_now().await?;
    let statements = revocations(&admin).await?;
    admin.close().await?;
    scanner.close().await?;
    let epochs = check_in_epochs(&admin_dir)?;

    fault::STOPS
        .lock()
        .map_err(|_| "poisoned fault")?
        .push((admin_dir.clone(), stage));
    assert!(
        open_rolled_out(&admin_dir, &narrowed()?, &hub)
            .await
            .is_err(),
        "the rollout stops at {stage:?}"
    );
    assert!(
        !fault::STOPS
            .lock()
            .map_err(|_| "poisoned fault")?
            .contains(&(admin_dir.clone(), stage)),
        "the rollout reached {stage:?}"
    );
    let admin = open_rolled_out(&admin_dir, &narrowed()?, &hub).await?;
    let _ = admin.sync_now().await;
    assert_eq!(
        revocations(&admin).await?,
        statements + 1,
        "one revocation ends the narrowed role's capabilities after {stage:?}"
    );
    let scanner = configuration(&scanner_dir, &narrowed()?, &hub)?
        .open()
        .await?;
    let _ = scanner.sync_now().await;
    let _ = scanner.sync_now().await;
    assert_eq!(scanner.me().await?.tables.get("check_ins"), None);
    assert!(
        check_in(&scanner, "T2").await.is_err(),
        "the narrowed scanner appends nothing after {stage:?}"
    );
    scanner.close().await?;
    admin.close().await?;
    assert_eq!(
        check_in_epochs(&admin_dir)?,
        epochs + 1,
        "the narrowed table's key rotates exactly once after {stage:?}"
    );
    assert!(!pending_narrowing(&admin_dir)?);
    let admin = open_rolled_out(&admin_dir, &narrowed()?, &hub).await?;
    let _ = admin.sync_now().await;
    admin.close().await?;
    assert_eq!(
        check_in_epochs(&admin_dir)?,
        epochs + 1,
        "a later open and pass rotate nothing again after {stage:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_crash_after_recording_the_narrowing_still_narrows_once() -> Result<(), Failure> {
    narrow_through_a_crash_at(fault::Stage::RightsChangeRecorded).await
}

#[tokio::test]
async fn a_crash_after_publishing_the_narrowed_schema_still_narrows_once() -> Result<(), Failure> {
    narrow_through_a_crash_at(fault::Stage::RightsChangeCommitted).await
}

#[tokio::test]
async fn a_crash_after_the_narrowing_revocation_begins_still_narrows_once() -> Result<(), Failure> {
    narrow_through_a_crash_at(fault::Stage::RevocationBegun).await
}

#[tokio::test]
async fn a_crash_between_narrowing_access_and_its_revocation_still_revokes_once()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (admin_dir, member_dir) = (root.path().join("admin"), root.path().join("member"));
    let admin = configuration(&admin_dir, &wide()?, &hub)?
        .create()
        .await?
        .database;
    let invitation = admin.invite(Invite::new(Access::Full)).await?;
    let member = configuration(&member_dir, &wide()?, &hub)?
        .join(&invitation.code)
        .await?
        .database;
    member.sync_now().await?;
    let person = member.me().await?.person;
    let statements = revocations(&admin).await?;
    fault::STOPS
        .lock()
        .map_err(|_| "poisoned fault")?
        .push((admin_dir.clone(), fault::Stage::RightsChangeCommitted));
    assert!(
        admin
            .set_access(person, Access::Role(SCANNER))
            .await
            .is_err(),
        "the change stops after the registry commit"
    );
    assert_eq!(revocations(&admin).await?, statements);
    let _ = admin.sync_now().await;
    assert_eq!(
        revocations(&admin).await?,
        statements + 1,
        "the next pass revokes the narrowed person's capabilities"
    );
    let _ = admin.sync_now().await;
    assert_eq!(revocations(&admin).await?, statements + 1);
    let _ = member.sync_now().await;
    let _ = member.sync_now().await;
    assert_eq!(member.me().await?.access, Access::Role(SCANNER));
    assert!(
        check_in(&member, "T1").await.is_ok(),
        "the scanner role still appends check-ins"
    );
    let deleted = member
        .write(async |tx| tx.execute("DELETE FROM check_ins", Vec::new()).await)
        .await;
    assert!(
        matches!(
            deleted.as_ref().map_err(crate::Error::kind),
            Err(crate::ErrorKind::Denied { .. })
        ),
        "the narrowed person deletes nothing, refused at the statement: {deleted:?}"
    );
    member.close().await?;
    admin.close().await?;
    assert!(!pending_narrowing(&admin_dir)?);
    Ok(())
}

#[tokio::test]
async fn a_promotion_made_while_the_person_joins_reaches_their_device() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = wide()?;
    let created = configuration(&root.path().join("owner"), &schema, &hub)?
        .create()
        .await?;
    let owner = created.database;
    let root_key: Arc<dyn crate::IdentityProvider> = Arc::new(created.root);
    let invitation = owner.invite(Invite::new(Access::Full)).await?;
    let server = root.path().join("server");
    let (bootstrapping, finish_join) = crate::pause::arm(&server, crate::pause::Point::Bootstrap);
    let joining_configuration = configuration(&server, &schema, &hub)?;
    let mut joining = std::pin::pin!(joining_configuration.join(&invitation.code));
    tokio::select! {
        biased;
        _ = &mut joining => return Err("join returned before its first pull began".into()),
        reached = bootstrapping => reached.map_err(|_| -> Failure { "the join never reached its first pull".into() })?,
    }
    let me = owner.me().await?.person;
    let joined = owner
        .people()
        .await?
        .into_iter()
        .find(|person| person.id != me)
        .ok_or("the redeemed invitation lists no person")?;
    owner.make_admin(joined.id, root_key).await?;
    let _ = finish_join.send(());
    let server = joining.await?.database;
    server.sync_now().await?;
    assert!(
        server.me().await?.admin,
        "a promotion made while the device joined reaches it at its next sync"
    );
    server.close().await?;
    owner.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_live_promotion_reaches_the_promoted_device_without_a_sync_call() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = wide()?;
    let live = |name: &str| -> Result<Configuration, Failure> {
        Ok(configuration(&root.path().join(name), &schema, &hub)?.sync(crate::SyncMode::Live))
    };
    let created = live("owner")?.create().await?;
    let owner = created.database;
    let invitation = owner.invite(Invite::new(Access::Full)).await?;
    let member = live("member")?.join(&invitation.code).await?.database;
    member.sync_now().await?;
    owner.sync_now().await?;
    let mut changes = member.status_changes();
    let before = changes.next().await?.authority;
    assert!(!before.is_issuer);
    owner
        .make_admin(before.principal, Arc::new(created.root))
        .await?;
    while !changes.next().await?.authority.is_issuer {}
    assert!(
        member.me().await?.admin,
        "a device that reports itself an issuer reads the registry"
    );
    member.close().await?;
    owner.close().await?;
    Ok(())
}

#[tokio::test]
async fn live_partition_grant_revocation_and_removal_arrive_via_status_changes()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = Schema::builder("clinic", 1)
        .table(
            Table::replayed("notes")
                .column(Column::new("patient", Affinity::Text).required())
                .column(Column::new("body", Affinity::Text).required())
                .partition_by(["patient"])
                .partition_group("chart"),
        )
        .role(
            Role::new(RoleName::new("clinician")?)
                .write("notes")
                .per_partition("chart"),
        )
        .build()?;
    let live = |name: &str| -> Result<Configuration, Failure> {
        Ok(configuration(&root.path().join(name), &schema, &hub)?.sync(crate::SyncMode::Live))
    };
    let owner = live("owner")?.create().await?.database;
    let ada = crate::Partition::new("chart", vec![crate::SqlValue::Text("ada".into())]);
    owner.create_partition(&ada).await?;
    let invitation = owner
        .invite(Invite::new(Access::Role(RoleName::new("clinician")?)))
        .await?;
    let clinician = live("clinician")?.join(&invitation.code).await?.database;
    clinician.sync_now().await?;
    owner.sync_now().await?;
    let mut changes = clinician.status_changes();
    let person = changes.next().await?.authority().principal;

    owner.grant_partition(person, &ada).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = changes.next().await?;
            if status
                .data()
                .partitions
                .iter()
                .any(|entry| entry.partition == ada && entry.held && entry.complete)
            {
                return Ok::<_, Failure>(());
            }
        }
    })
    .await
    .map_err(|_| "partition grant did not arrive promptly in Live mode")??;
    clinician
        .write(async |tx| {
            tx.execute(
                "INSERT INTO notes(patient, body) VALUES('ada', 'live note')",
                crate::params![],
            )
            .await
        })
        .await?;

    owner.revoke_partition(person, &ada).await?;
    let waited = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = changes.next().await?;
            if !status
                .data()
                .partitions
                .iter()
                .any(|entry| entry.partition == ada && entry.held)
            {
                return Ok::<_, Failure>(());
            }
        }
    })
    .await;
    if waited.is_err() {
        let current = clinician.status().await?;
        return Err(format!(
            "partition revocation did not arrive promptly in Live mode; partitions={:?}",
            current.data().partitions
        )
        .into());
    }
    waited.unwrap()?;

    owner.remove(person).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = changes.next().await?;
            if status.authority().verified_revocation.is_some() {
                return Ok::<_, Failure>(());
            }
        }
    })
    .await
    .map_err(|_| "removal did not arrive promptly in Live mode")??;

    clinician.close().await?;
    owner.close().await?;
    Ok(())
}
