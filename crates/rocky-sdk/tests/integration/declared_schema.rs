use crate::common::{self, Device, Failure, introduce};
use rocky_sdk::{
    Access, Affinity, Column, Configuration, Database, DeclaredSchema, ErrorKind, Network, Schema,
    Table, params, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

fn notes(version: u32) -> Schema {
    let mut notes = Table::replayed("notes").column(common::required("body", Affinity::Text));
    if version > 1 {
        notes = notes.column(Column::new("pinned", Affinity::Integer));
    }
    common::build(Schema::builder("notes", version).table(notes))
}
fn open(directory: &Path, hub: &Arc<Hub>, name: &str, version: u32) -> Configuration {
    common::configuration(
        directory,
        &notes(version),
        None,
        Some(common::external_identity(directory)),
        Some(Network::Owned(hub.connector(name))),
    )
}
async fn write(database: &Database) -> Result<(), rocky_sdk::Error> {
    database
        .write(async |tx| {
            tx.execute("INSERT INTO notes(body) VALUES('late')", params![])
                .await?;
            Ok(())
        })
        .await
}
async fn bodies(database: &Database) -> Result<usize, rocky_sdk::Error> {
    Ok(database
        .query("SELECT body FROM notes", params![])
        .await?
        .collect()
        .await?
        .len())
}
async fn sync(a: &Device, b: &Device) -> Result<(), Failure> {
    for device in [a, b, a] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_application_ahead_of_its_database_is_refused_sql_but_keeps_status_and_sync()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (owner_path, member_path) = (root.path().join("owner"), root.path().join("member"));
    let owner = Device::new(
        common::create_as(
            open(&owner_path, &hub, "owner", 1),
            common::external_key(&owner_path),
        )
        .await?,
    )?;
    let invitation = owner.invite_as(Access::Full).await;
    let member = Device::new(
        open(&member_path, &hub, "member", 1)
            .fixture_person()
            .join(&invitation)
            .await?,
    )?;
    member.sync().await.map_err(|error| error.to_string())?;
    introduce(&owner, &member).await;
    write(&owner.database).await?;
    sync(&owner, &member).await?;
    member.close().await;

    let member = Device::new(open(&member_path, &hub, "member", 2).open().await?)?;
    let ahead = ErrorKind::SchemaMismatch {
        installed: 1,
        declared: 2,
    };
    let refused = bodies(&member.database)
        .await
        .err()
        .ok_or("a query ran while the application was ahead")?;
    assert_eq!(refused.kind(), &ahead, "{refused}");
    assert!(
        refused.to_string().contains("admin device must publish"),
        "{refused}"
    );
    let refused = write(&member.database)
        .await
        .err()
        .ok_or("a write ran while the application was ahead")?;
    assert_eq!(refused.kind(), &ahead, "{refused}");
    assert_eq!(
        member
            .database
            .status()
            .await?
            .data()
            .generation
            .application,
        DeclaredSchema::Ahead,
        "status stays available"
    );
    sync(&owner, &member).await?;

    owner.close().await;
    let owner = Device::new(open(&owner_path, &hub, "owner", 2).open().await?)?;
    sync(&owner, &member).await?;
    assert_eq!(
        member
            .database
            .status()
            .await?
            .data()
            .generation
            .application,
        DeclaredSchema::Current
    );
    assert_eq!(
        bodies(&member.database).await?,
        1,
        "once an admin published the declared version the application reads again"
    );
    write(&member.database).await?;
    owner.close().await;
    member.close().await;
    Ok(())
}
