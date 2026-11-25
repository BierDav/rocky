use crate::common::{self, Device, Failure};
use rocky_sdk::{
    Access, Configuration, ErrorKind, IdentityKey, IdentityProvider, Invite, Network,
    status::DeclaredSchema, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

fn newer_schema() -> rocky_sdk::Schema {
    common::build(
        common::shopping(2).table(
            common::items().column(rocky_sdk::Column::new("note", rocky_sdk::Affinity::Text)),
        ),
    )
}

fn configured(
    directory: &Path,
    schema: &rocky_sdk::Schema,
    hub: &Arc<Hub>,
    person: Option<&IdentityKey>,
) -> Configuration {
    let name = directory.to_string_lossy().into_owned();
    common::configuration(
        directory,
        schema,
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

async fn reopened_with_the_person_key_alone(schema: &rocky_sdk::Schema) -> Result<Owned, Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let directory = root.path().join("owner");
    let created = configured(&directory, &common::schema(), &hub, None)
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
            configured(
                &directory,
                &common::schema(),
                &owned.hub,
                Some(&owned.person),
            ),
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
    let owned = reopened_with_the_person_key_alone(&common::schema()).await?;
    let laptop = second_device(&owned).await?;
    let me = laptop.database.me().await?;
    assert_eq!(me.person.key(), owned.person.person_key());
    assert!(me.admin);
    laptop.close().await;
    owned.owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_freshly_linked_owner_device_is_the_owner_before_it_syncs() -> Result<(), Failure> {
    let owned = reopened_with_the_person_key_alone(&common::schema()).await?;
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
    let owned = reopened_with_the_person_key_alone(&common::schema()).await?;
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
    let owned = reopened_with_the_person_key_alone(&common::schema()).await?;
    owned.owner.add(1, "Milk").await;
    let invitation = owned
        .owner
        .database
        .invite(Invite::new(Access::Full))
        .await?;
    let member = Device::new(
        configured(
            &owned._root.path().join("member"),
            &common::schema(),
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
    let owned = reopened_with_the_person_key_alone(&common::schema()).await?;
    let laptop = second_device(&owned).await?;
    let device = laptop.database.me().await?.device;
    let directory = owned._root.path().join("owner");
    owned.owner.close().await;
    let keyless = configured(&directory, &common::schema(), &owned.hub, None)
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

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_is_the_person_the_root_derives_and_an_arc_shares_its_key() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let created = Configuration::new(root.path().join("owner"), common::parsed_schema())?
        .create()
        .await?;
    let owner = rocky_sdk::PersonId::of(&created.owner);
    assert_eq!(owner, rocky_sdk::PersonId::of(&created.root.identity()));
    assert_eq!(created.database.me().await?.person, owner);
    created.database.close().await?;
    let shared = Arc::new(created.owner);
    let reopened = Configuration::new(root.path().join("owner"), common::parsed_schema())?
        .identity(shared.clone())
        .open()
        .await?;
    assert_eq!(
        reopened.me().await?.person,
        rocky_sdk::PersonId::of(&shared)
    );
    reopened.close().await?;
    Ok(())
}
