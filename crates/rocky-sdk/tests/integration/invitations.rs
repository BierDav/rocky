use crate::common::{self, Device, Failure};
use rocky_sdk::{
    Configuration, DenyReason, DeviceId, ErrorKind, IdentityKey, IdentityProvider, Invite,
    InviteError, JoinError, Network, Opened, PersonId, Right, ServiceError, transport::memory::Hub,
};
use serde_json::json;
use std::{path::Path, sync::Arc};

fn writer() -> rocky_sdk::Access {
    rocky_sdk::Access::Full
}
async fn issuer(directory: &Path, hub: &Arc<Hub>) -> Result<Device, Failure> {
    Device::new(
        crate::common::configuration(
            directory,
            common::parsed_schema(),
            None,
            None,
            Some(Network::Owned(hub.connector("issuer"))),
        )
        .create()
        .await?,
    )
}
async fn reopened(directory: &Path, hub: &Arc<Hub>) -> Result<Device, Failure> {
    Device::new(
        crate::common::configuration(
            directory,
            common::parsed_schema(),
            None,
            None,
            Some(Network::Owned(hub.connector("issuer"))),
        )
        .open()
        .await?,
    )
}
fn joining(
    directory: &Path,
    hub: &Arc<Hub>,
    person: Option<&Arc<dyn IdentityProvider>>,
) -> Result<Configuration, rocky_sdk::Error> {
    let configuration = Configuration::new(directory, common::parsed_schema())?
        .network(Network::Owned(hub.connector(&directory.to_string_lossy())))
        .sync(rocky_sdk::SyncMode::Manual);
    Ok(match person {
        Some(person) => configuration.identity(person.clone()),
        None => configuration,
    })
}
async fn join(
    directory: &Path,
    hub: &Arc<Hub>,
    person: Option<&Arc<dyn IdentityProvider>>,
    code: &rocky_sdk::InvitationCode,
) -> Result<Opened, rocky_sdk::Error> {
    joining(directory, hub, person)?.join(code).await
}
fn refusal(outcome: Result<Opened, rocky_sdk::Error>) -> Result<ErrorKind, Failure> {
    match outcome {
        Ok(_) => Err("the invitation was accepted".into()),
        Err(error) => Ok(error.kind().clone()),
    }
}
fn generated_identity() -> Arc<dyn IdentityProvider> {
    Arc::new(IdentityKey::generate())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_admits_one_device_under_a_fresh_identity_and_no_other() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    owner.add(1, "Milk").await;
    let invitation = owner
        .database
        .invite(Invite::new(writer()).label("Alice"))
        .await?;
    assert!(invitation.code.to_string().starts_with("rocky://invite/"));
    assert!(
        !format!("{:?}", invitation.code).contains(&invitation.code.to_string()[15..]),
        "a code redacts itself in debug output"
    );
    let opened = join(&root.path().join("member"), &hub, None, &invitation.code).await?;
    let minted = opened
        .identity
        .ok_or("a joiner without an identity gets one")?;
    let member = Device::new(opened.database)?;
    let me = member.database.me().await?;
    assert_eq!(me.person.key(), minted.person_key());
    assert_eq!(me.access, writer());
    common::introduce(&owner, &member).await;
    member.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(member.titles().await, ["Milk"]);

    let pending = owner.database.invitations().await?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].label.as_deref(), Some("Alice"));
    assert_eq!(pending[0].redeemed_by, Some(PersonId::of(&minted)));
    let recorded = owner
        .database
        .people()
        .await?
        .into_iter()
        .find(|record| record.id == PersonId::of(&minted))
        .ok_or("the issuer recorded no admission")?;
    assert_eq!(recorded.invitation, Some(invitation.id));
    assert_eq!(recorded.label.as_deref(), Some("Alice"));

    for (name, person) in [("second", None), ("third", Some(generated_identity()))] {
        assert_eq!(
            refusal(
                join(
                    &root.path().join(name),
                    &hub,
                    person.as_ref(),
                    &invitation.code
                )
                .await
            )?,
            ErrorKind::Join(JoinError::Used),
            "a token admits a single device: {name}"
        );
    }
    assert_eq!(
        owner
            .database
            .cancel_invitation(invitation.id)
            .await
            .err()
            .map(|error| error.kind().clone()),
        Some(ErrorKind::Invite(InviteError::Redeemed))
    );
    member.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identity_code_admits_only_that_identity() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    let person = generated_identity();
    let identity = person.public_key()?;
    let invitation = owner
        .database
        .invite(Invite::new(writer()).for_identity(PersonId::new(identity)))
        .await?;
    assert_eq!(
        refusal(
            join(
                &root.path().join("stranger"),
                &hub,
                Some(&generated_identity()),
                &invitation.code
            )
            .await
        )?,
        ErrorKind::Join(JoinError::OtherIdentity),
        "a provider of another identity is refused before it asks the issuer"
    );
    assert_eq!(
        refusal(join(&root.path().join("nobody"), &hub, None, &invitation.code).await)?,
        ErrorKind::Denied {
            table: None,
            need: Right::Sign,
            reason: DenyReason::IdentityRequired,
        }
    );
    let opened = join(
        &root.path().join("member"),
        &hub,
        Some(&person),
        &invitation.code,
    )
    .await?;
    assert!(
        opened.identity.is_none(),
        "a configured identity is never returned"
    );
    assert_eq!(opened.database.me().await?.person.key(), identity);
    assert_eq!(
        owner.database.invitations().await?[0].redeemed_by,
        Some(PersonId::new(identity))
    );
    assert_eq!(
        owner
            .database
            .invite(Invite::new(rocky_sdk::Access::Read).for_identity(PersonId::new(identity)))
            .await
            .err()
            .map(|error| error.kind().clone()),
        Some(ErrorKind::Invite(InviteError::AlreadyMember)),
        "a member is not invited into other access"
    );
    opened.database.close().await?;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_invitation_admits_no_one_and_leaves_no_record() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    let invitation = owner.database.invite(Invite::new(writer())).await?;
    owner.database.cancel_invitation(invitation.id).await?;
    assert!(owner.database.invitations().await?.is_empty());
    assert_eq!(
        owner
            .database
            .cancel_invitation(invitation.id)
            .await
            .err()
            .map(|error| error.kind().clone()),
        Some(ErrorKind::Invite(InviteError::Unknown))
    );
    let before = owner.database.people().await?.len();
    assert_eq!(
        refusal(join(&root.path().join("member"), &hub, None, &invitation.code).await)?,
        ErrorKind::Join(JoinError::Cancelled)
    );
    assert_eq!(
        owner.database.people().await?.len(),
        before,
        "a refused redemption admits nothing"
    );
    let again = owner.database.invite(Invite::new(writer())).await?;
    let opened = join(&root.path().join("member"), &hub, None, &again.code).await?;
    assert_eq!(
        Some(opened.database.me().await?.person.key()),
        opened.identity.as_ref().map(IdentityKey::person_key),
        "the refused join resumes in the same directory with a newer invitation"
    );
    opened.database.close().await?;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_invitation_is_refused_by_its_issuer() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let directory = root.path().join("issuer");
    let owner = issuer(&directory, &hub).await?;
    let invitation = owner
        .database
        .invite(Invite::new(writer()).valid_for(std::time::Duration::from_secs(60)))
        .await?;
    assert_eq!(
        owner
            .database
            .invite(Invite::new(writer()).valid_for(std::time::Duration::from_secs(59)))
            .await
            .err()
            .map(|error| error.code().to_owned()),
        Some("invitation_validity".to_owned())
    );
    owner.close().await;
    let mut book = common::authority_json(&directory, "invitations")?;
    book["open"][invitation.id.to_string()]["expires"] = json!(1);
    common::write_authority(&directory, "invitations", &serde_json::to_vec(&book)?).await?;
    let owner = reopened(&directory, &hub).await?;
    assert_eq!(
        refusal(join(&root.path().join("member"), &hub, None, &invitation.code).await)?,
        ErrorKind::Join(JoinError::Expired)
    );
    assert!(owner.database.invitations().await?.is_empty());
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_the_identity_ends_its_unused_invitation() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    let person = generated_identity();
    let identity = person.public_key()?;
    let invitation = owner
        .database
        .invite(Invite::new(writer()).for_identity(PersonId::new(identity)))
        .await?;
    owner
        .database
        .remove(rocky_sdk::PersonId::new(identity))
        .await?;
    assert_eq!(
        refusal(
            join(
                &root.path().join("member"),
                &hub,
                Some(&person),
                &invitation.code
            )
            .await
        )?,
        ErrorKind::Removed
    );
    assert_eq!(
        owner
            .database
            .invite(Invite::new(writer()).for_identity(PersonId::new(identity)))
            .await
            .err()
            .map(|error| error.kind().clone()),
        Some(ErrorKind::Removed),
        "a removed identity is not invited again"
    );
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_travels_only_over_a_network() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let offline = Device::new(
        crate::common::configuration(
            &root.path().join("offline"),
            common::parsed_schema(),
            None,
            None,
            None,
        )
        .create()
        .await?,
    )?;
    let unroutable = offline
        .database
        .invite(Invite::new(writer()))
        .await
        .err()
        .ok_or("an unroutable invitation was issued")?;
    assert_eq!(
        unroutable.kind(),
        &ErrorKind::Invite(InviteError::NetworkRequired)
    );
    offline.close().await;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    let invitation = owner.database.invite(Invite::new(writer())).await?;
    let without_network = Configuration::new(root.path().join("member"), common::parsed_schema())?
        .join(&invitation.code)
        .await
        .err()
        .ok_or("an invitation was accepted without a network")?;
    assert_eq!(without_network.code(), "network_required");
    assert!(owner.database.invitations().await?[0].redeemed_by.is_none());
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_minted_phrase_links_a_second_device_and_an_admin_unlinks_a_lost_one()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    owner.add(1, "Milk").await;
    let invitation = owner.database.invite(Invite::new(writer())).await?;
    let opened = join(&root.path().join("phone"), &hub, None, &invitation.code).await?;
    let phrase = opened.identity.ok_or("minted identity")?.phrase();
    let person: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::from_phrase(&phrase)?);
    let phone = Device::new(opened.database)?;
    common::introduce(&owner, &phone).await;
    phone.sync().await.map_err(|error| error.to_string())?;

    let laptop_directory = root.path().join("laptop");
    let laptop = Device::new(
        common::link(
            joining(&laptop_directory, &hub, None)?,
            person.clone(),
            &phone.contact().await,
            &phone.database,
        )
        .await?,
    )?;
    assert_eq!(
        laptop.database.me().await?.person.key(),
        person.public_key()?,
        "the phrase links a second device of the same person"
    );
    common::introduce(&owner, &laptop).await;
    common::introduce(&phone, &laptop).await;
    for device in [&laptop, &phone, &owner, &laptop] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    assert_eq!(laptop.titles().await, ["Milk"]);

    let lost = phone.database.me().await?.device.key();
    owner.database.unlink(DeviceId::new(lost)).await?;
    assert!(
        laptop
            .database
            .unlink(DeviceId::new(owner.database.me().await?.device.key()))
            .await
            .is_err(),
        "a member unlinks no other person's device"
    );
    owner.add(2, "Eggs").await;
    owner.sync().await.map_err(|error| error.to_string())?;
    laptop.sync().await.map_err(|error| error.to_string())?;
    laptop.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        laptop.titles().await,
        ["Eggs", "Milk"],
        "the person keeps the devices the admin did not unlink"
    );
    let _ = phone.sync().await;
    assert!(
        phone.purged().await,
        "the unlinked device purges what it held and receives nothing after its unlink"
    );
    phone.close().await;
    laptop.close().await;
    owner.close().await;
    Ok(())
}

async fn writes_an_item(device: &Device, title: &str) -> Result<(), Failure> {
    let mut tx = device.database.transaction().await?;
    tx.execute(
        "INSERT INTO items(key,title,completed) VALUES(?,?,0)",
        vec![
            rocky_sdk::SqlValue::Text(title.to_owned()),
            rocky_sdk::SqlValue::Text(title.to_owned()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn wider_access_and_admin_rights_reach_each_device_at_its_next_sync() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    owner.add(1, "Milk").await;
    let invitation = owner
        .database
        .invite(Invite::new(rocky_sdk::Access::Read))
        .await?;
    let opened = join(&root.path().join("phone"), &hub, None, &invitation.code).await?;
    let phrase = opened.identity.ok_or("minted identity")?.phrase();
    let person: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::from_phrase(&phrase)?);
    let phone = Device::new(opened.database)?;
    common::introduce(&owner, &phone).await;
    phone.sync().await.map_err(|error| error.to_string())?;
    let laptop = Device::new(
        common::link(
            joining(&root.path().join("laptop"), &hub, None)?,
            person.clone(),
            &phone.contact().await,
            &phone.database,
        )
        .await?,
    )?;
    common::introduce(&owner, &laptop).await;
    common::introduce(&phone, &laptop).await;
    for device in [&laptop, &phone, &owner, &laptop] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    let devices = [&phone, &laptop];
    for device in devices {
        assert_eq!(device.database.me().await?.access, rocky_sdk::Access::Read);
        assert!(writes_an_item(device, "refused").await.is_err());
    }
    let id = phone.database.me().await?.person;

    owner
        .database
        .set_access(id, rocky_sdk::Access::Full)
        .await?;
    for (device, title) in devices
        .into_iter()
        .zip(["from the phone", "from the laptop"])
    {
        device.sync().await.map_err(|error| error.to_string())?;
        assert_eq!(
            device.database.me().await?.access,
            rocky_sdk::Access::Full,
            "one sync with the issuer brings the wider access"
        );
        writes_an_item(device, title).await?;
    }

    owner.database.make_admin(id, owner.root()).await?;
    for device in devices {
        assert!(!device.database.me().await?.admin);
        device.sync().await.map_err(|error| error.to_string())?;
        assert!(
            device.database.me().await?.admin,
            "one sync with the issuer makes the device an admin's"
        );
    }
    assert!(laptop.database.people().await.is_ok());
    laptop.close().await;
    phone.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn open_tells_a_directory_without_a_database_from_one_with_a_database() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let empty = root.path().join("empty");
    let missing = joining(&empty, &hub, None)?
        .open()
        .await
        .err()
        .ok_or("an empty directory opened")?;
    assert_eq!(missing.kind(), &ErrorKind::NoDatabase);
    assert!(!missing.retryable());
    assert!(!empty.exists(), "a refused open creates nothing");
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    let invitation = owner.database.invite(Invite::new(writer())).await?;
    owner.close().await;
    let existing = Configuration::new(root.path().join("issuer"), common::parsed_schema())?
        .create()
        .await
        .err()
        .ok_or("a second database was created over the first")?;
    assert_eq!(existing.kind(), &ErrorKind::DatabaseExists);
    let joined_over = join(&root.path().join("issuer"), &hub, None, &invitation.code)
        .await
        .err()
        .ok_or("a join replaced an existing database")?;
    assert_eq!(joined_over.kind(), &ErrorKind::DatabaseExists);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_join_that_fails_is_unfinished_until_the_same_invitation_resumes_it()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    let invitation = owner.database.invite(Invite::new(writer())).await?;
    owner.close().await;
    let member = root.path().join("member");
    let failed = join(&member, &hub, None, &invitation.code)
        .await
        .err()
        .ok_or("a join reached an issuer that is offline")?;
    assert!(
        matches!(
            failed.kind(),
            ErrorKind::Service(ServiceError::Unreachable(_))
        ),
        "{failed:?}"
    );
    assert!(failed.retryable());
    let unfinished = joining(&member, &hub, None)?
        .open()
        .await
        .err()
        .ok_or("an unfinished join opened")?;
    assert_eq!(unfinished.kind(), &ErrorKind::JoinUnfinished);
    let owner = reopened(&root.path().join("issuer"), &hub).await?;
    owner.add(1, "Milk").await;
    let joined = join(&member, &hub, None, &invitation.code).await?;
    assert!(
        joined.identity.is_some(),
        "the resumed join keeps its minted key"
    );
    let member = Device::new(joined)?;
    assert_eq!(member.titles().await, ["Milk"]);
    member.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_code_no_attached_transport_routes_is_refused_as_no_route() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    let invitation = owner.database.invite(Invite::new(writer())).await?;
    let refused = Configuration::new(root.path().join("member"), common::parsed_schema())?
        .network(rocky_tcp::Options::dialer().into())
        .join(&invitation.code)
        .await
        .err()
        .ok_or("a TCP transport redeemed a code that names only a memory route")?;
    assert!(
        matches!(
            refused.kind(),
            ErrorKind::Service(ServiceError::NoRoute { schemes, .. })
                if schemes.iter().any(|scheme| scheme == "tcp")
        ),
        "{refused:?}"
    );
    assert!(
        !refused.retryable(),
        "a missing transport is a configuration error"
    );
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_code_for_another_application_is_refused_once_the_issuer_answers_and_resumes_with_its_own()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub).await?;
    let invitation = owner.database.invite(Invite::new(writer())).await?;
    let notes = rocky_sdk::Schema::builder("notes", 1)
        .table(
            rocky_sdk::Table::replayed("notes")
                .column(rocky_sdk::Column::new("body", rocky_sdk::Affinity::Text)),
        )
        .build()?;
    let refused = Configuration::new(root.path().join("member"), &notes)?
        .network(Network::Owned(hub.connector("member")))
        .join(&invitation.code)
        .await
        .err()
        .ok_or("a notes app joined a shopping database")?;
    assert_eq!(
        refused.kind(),
        &ErrorKind::Join(JoinError::OtherApplication)
    );
    assert!(!refused.retryable());
    assert!(
        !root.path().join("member").join("rocky.sqlite").exists(),
        "a refused join installs no database"
    );
    let reopened = Configuration::new(root.path().join("member"), &notes)?
        .open()
        .await
        .err()
        .ok_or("a refused join opened")?;
    assert_eq!(reopened.kind(), &ErrorKind::JoinUnfinished);
    let resumed = joining(&root.path().join("member"), &hub, None)?
        .join(&invitation.code)
        .await?;
    assert!(
        resumed.identity.is_some(),
        "the issuer recognizes the refused device and the join keeps its minted key"
    );
    resumed.database.close().await?;
    owner.close().await;
    Ok(())
}
