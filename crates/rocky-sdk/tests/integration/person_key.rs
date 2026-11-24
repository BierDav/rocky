use crate::common::{self, Device, Failure};
use crate::support::snapshot as snap;
use rocky_sdk::{
    Access, Configuration, DeclaredSchema, ErrorKind, IdentityKey, IdentityProvider, Invite,
    Network, transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn newer_schema() -> String {
    let mut schema: Value = serde_json::from_str(common::SCHEMA).expect("fixture schema");
    schema["version"] = json!(2);
    if let Some(columns) = schema["tables"][0]["columns"].as_array_mut() {
        columns.push(json!({"name": "note", "type": "text"}));
    }
    schema.to_string()
}

fn configured(
    directory: &Path,
    schema: &str,
    hub: &Arc<Hub>,
    person: Option<&IdentityKey>,
) -> Configuration {
    let name = directory.to_string_lossy().into_owned();
    common::configuration(
        directory,
        snap::schema(schema),
        None,
        person.map(|key| -> Arc<dyn IdentityProvider> {
            Arc::new(IdentityKey::from_secret(key.secret()))
        }),
        Some(Network::Owned(hub.connector(&name))),
    )
}

struct Owned {
    _root: tempfile::TempDir,
    hub: Arc<Hub>,
    person: IdentityKey,
    owner: Device,
}

async fn reopened_with_the_person_key_alone(schema: &str) -> Result<Owned, Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let directory = root.path().join("owner");
    let created = configured(&directory, common::SCHEMA, &hub, None)
        .create()
        .await?;
    let phrase = created.owner.phrase();
    created.database.close().await?;
    drop(created.root);
    let person = IdentityKey::from_phrase(&phrase)?;
    let owner = Device::new(
        configured(&directory, schema, &hub, Some(&person))
            .open()
            .await?,
    )?;
    Ok(Owned {
        _root: root,
        hub,
        person,
        owner,
    })
}

async fn second_device(owned: &Owned) -> Result<Device, Failure> {
    let directory = owned._root.path().join("laptop");
    Device::new(
        common::link(
            configured(&directory, common::SCHEMA, &owned.hub, Some(&owned.person)),
            Arc::new(IdentityKey::from_secret(owned.person.secret())),
            &owned.owner.contact().await,
            &owned.owner.database,
        )
        .await?,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_person_key_publishes_a_new_schema_version_without_the_root() -> Result<(), Failure> {
    let declared = newer_schema();
    let owned = reopened_with_the_person_key_alone(&declared).await?;
    let generation = owned.owner.database.status().await?.data.generation;
    assert_eq!(generation.application, DeclaredSchema::Current);
    assert_eq!(generation.current.schema, 2);
    owned.owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_person_key_approves_a_link_of_the_owners_second_device_without_the_root()
-> Result<(), Failure> {
    let owned = reopened_with_the_person_key_alone(common::SCHEMA).await?;
    let laptop = second_device(&owned).await?;
    let me = laptop.database.me().await?;
    assert_eq!(me.person.key(), owned.person.public_key());
    assert!(me.admin);
    laptop.close().await;
    owned.owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_freshly_linked_owner_device_is_the_owner_before_it_syncs() -> Result<(), Failure> {
    let owned = reopened_with_the_person_key_alone(common::SCHEMA).await?;
    let laptop = second_device(&owned).await?;
    assert!(
        laptop.database.me().await?.owner,
        "ownership comes from the root-signed owner grant, not from the registry's arrival"
    );
    assert!(owned.owner.database.me().await?.owner);
    laptop.close().await;
    owned.owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_person_key_unlinks_one_of_the_owners_devices_without_the_root() -> Result<(), Failure>
{
    let owned = reopened_with_the_person_key_alone(common::SCHEMA).await?;
    let laptop = second_device(&owned).await?;
    common::introduce(&owned.owner, &laptop).await;
    let device = laptop.database.me().await?.device;
    owned.owner.database.unlink(device).await?;
    let refused = laptop
        .sync()
        .await
        .err()
        .ok_or("an unlinked device synced")?;
    assert_eq!(refused["code"], "removed", "{refused}");
    laptop.close().await;
    owned.owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_person_key_invites_without_the_root() -> Result<(), Failure> {
    let owned = reopened_with_the_person_key_alone(common::SCHEMA).await?;
    owned.owner.add(1, "Milk").await;
    let invitation = owned
        .owner
        .database
        .invite(Invite::new(Access::Full))
        .await?;
    let member = Device::new(
        configured(
            &owned._root.path().join("member"),
            common::SCHEMA,
            &owned.hub,
            None,
        )
        .join(&invitation.code)
        .await?,
    )?;
    common::introduce(&owned.owner, &member).await;
    member.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(member.titles().await, ["Milk"]);
    member.close().await;
    owned.owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_person_key_an_unlink_is_refused_as_identity_required() -> Result<(), Failure> {
    let owned = reopened_with_the_person_key_alone(common::SCHEMA).await?;
    let laptop = second_device(&owned).await?;
    let device = laptop.database.me().await?.device;
    let directory = owned._root.path().join("owner");
    owned.owner.close().await;
    let keyless = configured(&directory, common::SCHEMA, &owned.hub, None)
        .open()
        .await?;
    let refused = keyless
        .unlink(device)
        .await
        .err()
        .ok_or("unlinked without the person key")?;
    assert_eq!(
        *refused.kind(),
        ErrorKind::Denied {
            table: None,
            need: rocky_sdk::Right::Sign,
            reason: rocky_sdk::DenyReason::IdentityRequired,
        }
    );
    keyless.close().await?;
    laptop.close().await;
    Ok(())
}
