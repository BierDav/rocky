use crate::common::{self, Device, Failure};
use rocky_sdk::{
    Configuration, Database, ErrorKind, IdentityKey, IdentityProvider, InvitationRefusal,
    InviteTerms, Network, Open, Opened, transport::memory::Hub,
};
use serde_json::json;
use std::{path::Path, sync::Arc};

fn writer() -> rocky_sdk::Access {
    rocky_sdk::Access::Full
}
fn issuer(directory: &Path, hub: &Arc<Hub>) -> Result<Device, Failure> {
    Device::new(Database::create(
        Open {
            identity: None,
            directory,
            schema: common::parsed_schema(),
            secrets: None,
            network: Some(Network::Owned(hub.connector("issuer"))),
        },
        None,
    )?)
}
fn reopened(directory: &Path, hub: &Arc<Hub>) -> Result<Device, Failure> {
    Device::new(Database::open(Open {
        identity: None,
        directory,
        schema: common::parsed_schema(),
        secrets: None,
        network: Some(Network::Owned(hub.connector("issuer"))),
    })?)
}
async fn accept(
    directory: &Path,
    hub: &Arc<Hub>,
    person: &Arc<dyn IdentityProvider>,
    invitation: &str,
) -> Result<Opened, Failure> {
    Configuration::new(directory, common::parsed_schema())?
        .network(Network::Owned(hub.connector(&directory.to_string_lossy())))
        .identity(person.clone())
        .accept_invitation(invitation)
        .await
}
fn refusal(outcome: Result<Opened, Failure>) -> Result<ErrorKind, Failure> {
    match outcome {
        Ok(_) => Err("the invitation was accepted".into()),
        Err(error) => Ok(rocky_sdk::Error::from_failure(&error).kind().clone()),
    }
}
fn generated_identity() -> Arc<dyn IdentityProvider> {
    Arc::new(IdentityKey::generate())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_identity_joins_once_by_invitation_with_its_own_provider() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub)?;
    owner.add(1, "Milk").await;
    let person = generated_identity();
    let identity = person.public_key()?;
    let invitation = owner
        .session
        .invite(identity, writer(), InviteTerms::default())
        .await?;
    assert_eq!(invitation.identity, identity);
    assert!(invitation.uri.starts_with("rocky://invite/"));
    let stranger = refusal(
        accept(
            &root.path().join("stranger"),
            &hub,
            &generated_identity(),
            &invitation.uri,
        )
        .await,
    )?;
    assert_eq!(
        stranger,
        ErrorKind::Invitation(InvitationRefusal::OtherIdentity),
        "a provider of another identity is refused before it asks the issuer"
    );

    let opened = accept(&root.path().join("member"), &hub, &person, &invitation.uri).await?;
    assert!(
        opened.identity.is_none(),
        "the joiner keeps its own identity: no phrase is handed over"
    );
    let member = Device::new(opened)?.with_identity(person.clone());
    let info = member.session.info().await?;
    assert_eq!(
        info.principal, identity,
        "the invited identity holds the grants"
    );
    assert_eq!(info.access, writer());
    common::introduce(&owner, &member).await;
    member.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(member.titles().await, ["Milk"]);
    member.add(2, "Eggs").await;
    owner.sync().await.map_err(|error| error.to_string())?;
    let mut titles = owner.titles().await;
    titles.sort();
    assert_eq!(titles, ["Eggs", "Milk"]);

    let recorded = owner
        .session
        .identities()
        .await?
        .into_iter()
        .find(|record| record.identity == identity)
        .ok_or("the issuer recorded no admission")?;
    assert_eq!(recorded.invitation, Some(invitation.id));
    assert_eq!(recorded.access, Some(writer()));
    let open = owner.session.open_invitations().await?;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].redeemed_by, Some(info.node));

    let second =
        refusal(accept(&root.path().join("second"), &hub, &person, &invitation.uri).await)?;
    assert_eq!(
        second,
        ErrorKind::Invitation(InvitationRefusal::Used),
        "another node of the identity links instead of reusing the invitation"
    );
    let withdrawn = owner
        .session
        .withdraw_invitation(invitation.id)
        .await
        .err()
        .ok_or("a used invitation was withdrawn")?;
    assert_eq!(
        rocky_sdk::Error::from_failure(&withdrawn).code(),
        InvitationRefusal::Used.code(),
        "a used invitation ends only with its identity's revocation"
    );
    member.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_withdrawn_invitation_admits_no_one_and_leaves_no_record() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub)?;
    let person = generated_identity();
    let identity = person.public_key()?;
    let invitation = owner
        .session
        .invite(identity, writer(), InviteTerms::default())
        .await?;
    owner.session.withdraw_invitation(invitation.id).await?;
    assert!(owner.session.open_invitations().await?.is_empty());
    let refused =
        refusal(accept(&root.path().join("member"), &hub, &person, &invitation.uri).await)?;
    assert_eq!(refused, ErrorKind::Invitation(InvitationRefusal::Unknown));
    assert!(
        owner
            .session
            .identities()
            .await?
            .iter()
            .all(|record| record.identity != identity),
        "a refused redemption admits nothing"
    );
    let again = owner
        .session
        .invite(identity, writer(), InviteTerms::default())
        .await?;
    let opened = accept(&root.path().join("member"), &hub, &person, &again.uri).await?;
    assert_eq!(
        opened.database.session().info().await?.principal,
        identity,
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
    let owner = issuer(&directory, &hub)?;
    let person = generated_identity();
    let invitation = owner
        .session
        .invite(
            person.public_key()?,
            writer(),
            InviteTerms {
                valid_for: Some(std::time::Duration::from_secs(60)),
                label: Some(b"Alice".to_vec()),
                ..InviteTerms::default()
            },
        )
        .await?;
    owner.close().await;
    let mut book = common::authority_json(&directory, "invitations")?;
    book["open"][invitation.id.to_hex()]["expires"] = json!(1);
    common::write_authority(&directory, "invitations", &serde_json::to_vec(&book)?).await?;
    let owner = reopened(&directory, &hub)?;
    let refused =
        refusal(accept(&root.path().join("member"), &hub, &person, &invitation.uri).await)?;
    assert_eq!(refused, ErrorKind::Invitation(InvitationRefusal::Expired));
    assert!(owner.session.open_invitations().await?.is_empty());
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_the_identity_ends_its_unused_invitation() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub)?;
    let person = generated_identity();
    let identity = person.public_key()?;
    let invitation = owner
        .session
        .invite(identity, writer(), InviteTerms::default())
        .await?;
    owner.session.revoke_identity(identity).await?;
    let refused = accept(&root.path().join("member"), &hub, &person, &invitation.uri)
        .await
        .err()
        .ok_or("a revoked identity joined")?;
    assert!(
        refused.to_string().contains("revoked"),
        "the issuer names the revocation: {refused}"
    );
    assert!(
        owner
            .session
            .invite(identity, writer(), InviteTerms::default())
            .await
            .is_err(),
        "a revoked identity is not invited again"
    );
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_travels_only_over_a_network() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let offline = Device::new(Database::create(
        Open {
            identity: None,
            directory: &root.path().join("offline"),
            schema: common::parsed_schema(),
            secrets: None,
            network: None,
        },
        None,
    )?)?;
    let unroutable = offline
        .session
        .invite(
            generated_identity().public_key()?,
            writer(),
            InviteTerms::default(),
        )
        .await
        .err()
        .ok_or("an unroutable invitation was issued")?;
    assert_eq!(
        rocky_sdk::Error::from_failure(&unroutable).code(),
        "network_required"
    );
    offline.close().await;
    let hub = Arc::new(Hub::default());
    let owner = issuer(&root.path().join("issuer"), &hub)?;
    let person = generated_identity();
    let invitation = owner
        .session
        .invite(person.public_key()?, writer(), InviteTerms::default())
        .await?;
    let without_network = Configuration::new(root.path().join("member"), common::parsed_schema())?
        .identity(person.clone())
        .accept_invitation(&invitation.uri)
        .await
        .err()
        .ok_or("an invitation was accepted without a network")?;
    assert_eq!(
        rocky_sdk::Error::from_failure(&without_network).code(),
        "network_required"
    );
    let without_identity = Configuration::new(root.path().join("member"), common::parsed_schema())?
        .network(Network::Owned(hub.connector("member")))
        .accept_invitation(&invitation.uri)
        .await;
    assert!(
        without_identity.is_err(),
        "joining by invitation never mints an identity"
    );
    assert!(
        owner.session.open_invitations().await?[0]
            .redeemed_by
            .is_none()
    );
    owner.close().await;
    Ok(())
}
