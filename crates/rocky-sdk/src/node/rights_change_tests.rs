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
        Err(crate::Error {
            kind: ErrorKind::RolloutConfirmation(required),
            ..
        }) => configuration.open_confirmed(&required.report.digest).await,
        opened => opened,
    }
}
async fn check_in(database: &Database, code: &str) -> Result<(), Failure> {
    let mut tx = database.transaction().await?;
    let id = tx.next_id("check_ins", Vec::new()).await?;
    tx.execute(
        "INSERT INTO check_ins(code,gate,id) VALUES(?,?,?)",
        vec![
            SqlValue::Text(code.into()),
            SqlValue::Text("north".into()),
            id.sql(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
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
    let mut tx = member.transaction().await?;
    assert!(
        tx.execute("DELETE FROM check_ins", Vec::new())
            .await
            .is_err()
            || tx.commit().await.is_err(),
        "the narrowed person deletes nothing"
    );
    member.close().await?;
    admin.close().await?;
    assert!(!pending_narrowing(&admin_dir)?);
    Ok(())
}
