use common::{Device, Failure};
use rocky_credentials::RevocationMode;
use rocky_sdk::{Network, status::SyncReport, transport::memory::Hub};
use serde_json::Value;
use std::{collections::BTreeSet, path::Path, sync::Arc};

use crate::common;

fn open(directory: &Path, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        common::parsed_schema(),
        None,
        common::prepared_identity(directory),
        Some(Network::Owned(hub.connector(name))),
    )
}
async fn create(root: &Path, hub: &Arc<Hub>, name: &str) -> Result<Device, Failure> {
    Device::new(open(&root.join(name), hub, name).create().await?)
}
async fn join(
    root: &Path,
    hub: &Arc<Hub>,
    name: &str,
    invitation: &rocky_sdk::InvitationCode,
) -> Result<Device, Failure> {
    Device::new(open(&root.join(name), hub, name).join(invitation).await?)
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    common::authority_json(&root.join(name), "identity")
}
fn peer_id(node: &str) -> Result<String, Failure> {
    Ok(rocky_credentials::PeerId::of(node.parse()?).to_hex())
}
fn assert_issuers(root: &Path, name: &str, nodes: &[&str]) -> Result<(), Failure> {
    let saved = identity(root, name)?;
    let actual: BTreeSet<_> = saved["contact_refs"]
        .as_object()
        .ok_or("contacts")?
        .iter()
        .filter(|(_, entry)| entry["issuer"] == true)
        .map(|(id, _)| id.clone())
        .collect();
    let expected = nodes
        .iter()
        .map(|node| peer_id(node))
        .collect::<Result<BTreeSet<_>, _>>()?;
    assert_eq!(actual, expected, "{name}");
    Ok(())
}
async fn synced(device: &Device) -> Result<SyncReport, rocky_sdk::Error> {
    device.sync_now().await
}
fn reconciled_with(report: &SyncReport, node: &str) -> bool {
    report
        .peers
        .iter()
        .any(|peer| peer.node.to_string() == node && peer.reconciled())
}
async fn synced_titles(device: &Device) -> Result<Vec<String>, Failure> {
    for _ in 0..4 {
        synced(device).await?;
        match device
            .database
            .query("SELECT title FROM items", Vec::new())
            .await
        {
            Ok(_) => return Ok(device.titles().await),
            Err(_) => continue,
        }
    }
    Err("the rows never became readable".into())
}
async fn device_info(device: &Device) -> rocky_sdk::status::AuthorityStatus {
    device.database.status().await.expect("status").authority
}
async fn destroy(root: &Path, name: &str, device: Device) -> Result<(), Failure> {
    device.close().await;
    drop(device);
    std::fs::remove_dir_all(root.join(name))?;
    Ok(())
}

#[tokio::test]
async fn a_second_issuer_carries_the_domain_after_the_creator_is_lost() -> Result<(), Failure> {
    creator_revocation(RevocationMode::Lost).await
}

#[tokio::test]
async fn a_compromised_creator_ends_its_early_enrollee_by_cascade() -> Result<(), Failure> {
    creator_revocation(RevocationMode::Compromised).await
}

async fn creator_revocation(mode: RevocationMode) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a").await?;
    let early = join(root, &hub, "early", &a.invite().await).await?;
    a.add(1, "Milk").await;
    synced(&early).await?;

    let b = join(
        root,
        &hub,
        "b",
        &a.database
            .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
            .await?
            .code,
    )
    .await?;
    let principal = device_info(&b).await.principal;
    a.database
        .make_admin(
            principal,
            a.root_signer()?,
        )
        .await?;
    b.database.renew().await?;
    a.database.add_peer(&b.contact().await).await?;
    let root_key = a.root_signer()?;
    let (a_node, b_node) = (a.node().await, b.node().await);
    let (a_identity, b_identity) = (a.identity().await, b.identity().await);
    assert_issuers(root, "a", &[&b_node])?;
    assert_issuers(root, "b", &[&a_node])?;
    assert_eq!(
        identity(root, "a")?["contact_refs"][peer_id(&b_node)?]["contact"]["routes"][0],
        "memory:b"
    );
    assert_ne!(
        device_info(&a).await.principal,
        device_info(&b).await.principal,
        "paired issuers share no identity key"
    );
    assert_ne!(
        device_info(&a).await.node,
        device_info(&b).await.node,
        "paired issuers share no node key"
    );
    synced(&b).await?;
    assert_eq!(b.titles().await, ["Milk"]);

    let late = join(root, &hub, "late", &a.invite().await).await?;
    assert_issuers(root, "late", &[&a_node, &b_node])?;
    assert_issuers(root, "early", &[&a_node])?;
    early.database.renew().await?;
    assert_issuers(root, "early", &[&a_node, &b_node])?;
    let report = synced(&late).await?;
    assert!(
        reconciled_with(&report, &b_node),
        "the second issuer serves a device it never saw by its chain: {report:?}"
    );
    let late_node = late.node().await;
    assert!(
        b.database
            .status()
            .await?
            .network
            .peers
            .iter()
            .any(|peer| peer.node.to_string() == late_node
                && peer.last_authenticated_exchange_at_epoch_seconds.is_some())
    );
    assert_eq!(
        identity(root, "late")?["contact_refs"]
            .as_object()
            .map(|peers| peers.len()),
        Some(2)
    );

    destroy(root, "a", a).await?;

    let error = b
        .database
        .remove(rocky_sdk::PersonId::new(b_identity.parse()?))
        .await
        .err()
        .ok_or("revoked itself")?;
    assert_eq!(
        error.code(),
        "owner_required",
        "a trusted node does not revoke itself without the root: {error}"
    );
    let error = b
        .database
        .remove(rocky_sdk::PersonId::new(a_identity.parse()?))
        .await
        .err()
        .ok_or("revoked a trusted node without the root")?;
    assert_eq!(
        error.code(),
        "owner_required",
        "an admin node revokes no admin node without the root: {error}"
    );
    b.database
        .revoke_admin(
            rocky_sdk::PersonId::new(a_identity.parse()?),
            match mode {
                RevocationMode::Lost => rocky_sdk::Removal::Departed,
                RevocationMode::Compromised => rocky_sdk::Removal::Compromised,
            },
            root_key,
        )
        .await?;
    assert_eq!(
        b.database.status().await?.authority.revocations.statements,
        1,
        "with the root, a trusted node revokes the lost one"
    );
    assert_issuers(root, "b", &[])?;
    assert!(
        identity(root, "b")?["contact_refs"]
            .get(peer_id(&a_node)?)
            .is_none()
    );

    if mode == RevocationMode::Compromised {
        let error = early
            .database
            .renew()
            .await
            .err()
            .ok_or("renewed a cascaded identity")?;
        assert_eq!(error.kind(), &rocky_sdk::ErrorKind::Removed, "{error}");
        let status = early.database.status().await?;
        assert_eq!(
            status
                .authority
                .verified_revocation
                .as_ref()
                .map(|notice| &notice.reason),
            Some(&rocky_sdk::status::RevocationReason::Cascade {
                admin: rocky_sdk::PersonId::new(a_identity.parse()?)
            }),
            "{status:?}"
        );
        assert!(early.database.renew().await.is_err());
        for device in [&b, &early, &late] {
            device.close().await;
        }
        drop(early);
        let early = Device::new(open(&root.join("early"), &hub, "early").open().await?)?;
        let status = early.database.status().await?;
        assert_eq!(
            status
                .authority
                .verified_revocation
                .as_ref()
                .map(|notice| &notice.reason),
            Some(&rocky_sdk::status::RevocationReason::Cascade {
                admin: rocky_sdk::PersonId::new(a_identity.parse()?)
            }),
            "{status:?}"
        );
        assert!(early.database.renew().await.is_err());
        early.close().await;
        return Ok(());
    }

    b.seal_and_compact().await;
    let c = join(root, &hub, "c", &b.invite().await).await?;
    assert_eq!(device_info(&c).await.revocations.statements, 1);
    let report = synced(&c).await?;
    assert!(reconciled_with(&report, &b_node), "{report:?}");
    assert_eq!(
        c.titles().await,
        ["Milk"],
        "the second issuer's invitee gets pre-rotation rows through a base at a seal"
    );

    let before = device_info(&early).await;
    assert_eq!(before.revocations.statements, 0);
    early.database.renew().await?;
    let after = device_info(&early).await;
    assert_eq!(
        after.revocations.statements, 1,
        "a device the creator enrolled renews through the second issuer"
    );
    assert_eq!(
        device_info(&b).await.expires_at_epoch_seconds,
        u64::MAX,
        "a trusted grant never expires"
    );
    assert!(
        before.expires_at_epoch_seconds <= after.expires_at_epoch_seconds
            && after.expires_at_epoch_seconds < u64::MAX,
        "a device chain below a trusted grant keeps its own lifetime"
    );
    assert_issuers(root, "early", &[&b_node])?;
    assert!(
        identity(root, "early")?["contact_refs"]
            .get(peer_id(&a_node)?)
            .is_none(),
        "a renewed device forgets the revoked creator"
    );
    early.add(2, "Eggs").await;
    assert_eq!(early.titles().await, ["Eggs", "Milk"]);
    let report = synced(&early).await?;
    assert!(
        reconciled_with(&report, &b_node),
        "each sync completes with the second issuer, not only returns: {report:?}"
    );
    assert_eq!(b.titles().await, ["Eggs", "Milk"]);
    let report = synced(&c).await?;
    assert!(reconciled_with(&report, &b_node), "{report:?}");
    assert_eq!(c.titles().await, ["Eggs", "Milk"]);

    synced(&late).await?;
    assert_eq!(
        device_info(&late).await.revocations.statements,
        1,
        "a refused sync renews from the second issuer and learns the revocation"
    );
    assert_issuers(root, "late", &[&b_node])?;
    late.add(3, "Bread").await;
    synced(&late).await?;
    let report = synced(&c).await?;
    assert!(reconciled_with(&report, &b_node), "{report:?}");
    assert_eq!(c.titles().await, ["Bread", "Eggs", "Milk"]);
    assert_eq!(device_info(&c).await.revocations.statements, 1);
    assert_issuers(root, "early", &[&b_node])?;
    for device in [&b, &c, &early, &late] {
        device.close().await;
    }
    drop((c, early));
    for (name, titles) in [
        ("c", &["Bread", "Eggs", "Milk"][..]),
        ("early", &["Eggs", "Milk"][..]),
    ] {
        let device = Device::new(open(&root.join(name), &hub, name).open().await?)?;
        assert_eq!(
            device.titles().await,
            titles,
            "{name}: rows survive reopening"
        );
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_credential_renews_through_the_second_issuer_once_the_first_is_gone()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a").await?;
    let b = join(
        root,
        &hub,
        "b",
        &a.database
            .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
            .await?
            .code,
    )
    .await?;
    a.admit(&b).await;
    a.database.add_peer(&b.contact().await).await?;
    synced(&b).await.map_err(|error| {
        format!("the second issuer bootstraps its keys before their source is lost: {error}")
    })?;
    let g = join(
        root,
        &hub,
        "g",
        &a.database
            .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
            .await?
            .code,
    )
    .await?;
    let granted = device_info(&g).await.expires_at_epoch_seconds;
    destroy(root, "a", a).await?;

    g.database.renew().await?;
    let renewed = device_info(&g).await.expires_at_epoch_seconds;
    assert!(
        renewed >= granted,
        "the second issuer renews for the domain's lifetime: {granted} {renewed}"
    );
    g.add(1, "Milk").await;
    let report = synced(&g).await?;
    assert_eq!(b.titles().await, ["Milk"], "{report:?}");
    for device in [&b, &g] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn revoking_a_trusted_identity_cascades_to_what_it_enrolled() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a").await?;
    let t = join(
        root,
        &hub,
        "t",
        &a.database
            .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
            .await?
            .code,
    )
    .await?;
    let t_identity = device_info(&t).await.principal;
    a.database
        .make_admin(
            t_identity,
            a.root_signer()?,
        )
        .await?;
    t.database.renew().await?;
    let (a_node, t_node) = (a.node().await, t.node().await);
    a.add(1, "Milk").await;
    synced(&t).await?;
    let enrolled_by_t = join(root, &hub, "d", &t.invite().await).await?;
    assert_eq!(synced_titles(&enrolled_by_t).await?, ["Milk"]);
    let enrolled_by_a = join(root, &hub, "e", &a.invite().await).await?;
    assert_eq!(synced_titles(&enrolled_by_a).await?, ["Milk"]);
    assert_issuers(root, "d", &[&a_node, &t_node])?;

    t.close().await;
    drop(t);
    enrolled_by_t
        .database
        .renew()
        .await
        .map_err(|error| format!("renews through the creator, which never enrolled it: {error}"))?;

    let error = a
        .database
        .remove(t_identity)
        .await
        .err()
        .ok_or("revoked a trusted identity without the root")?;
    assert_eq!(
        error.code(),
        "owner_required",
        "only the root revokes a trusted identity: {error}"
    );
    a.database
        .revoke_admin(
            t_identity,
            rocky_sdk::Removal::Compromised,
            a.root_signer()?,
        )
        .await?;

    let _ = enrolled_by_t.sync().await;
    let _ = enrolled_by_t.database.renew().await;
    let status = enrolled_by_t.database.status().await?;
    assert_eq!(
        status
            .authority
            .verified_revocation
            .as_ref()
            .map(|revoked| &revoked.reason),
        Some(&rocky_sdk::status::RevocationReason::Cascade { admin: t_identity }),
        "the cut-off device learns the revoked trusted node is why: {status:?}"
    );
    assert!(enrolled_by_t.database.renew().await.is_err());

    enrolled_by_a.add(2, "Eggs").await;
    let report = synced(&enrolled_by_a).await?;
    assert!(
        reconciled_with(&report, &a_node),
        "the creator's enrollee keeps working: {report:?}"
    );
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);

    a.seal_and_compact().await;
    let again = join(root, &hub, "d2", &a.invite().await).await?;
    assert_eq!(
        synced_titles(&again).await?,
        ["Eggs", "Milk"],
        "the cut-off user enrolls again through the honest node and gets pre-rotation rows from a base"
    );
    for device in [&a, &enrolled_by_t, &enrolled_by_a, &again] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_identity_revocation_reports_identity_rather_than_node_or_cascade()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = create(directory.path(), &hub, "owner").await?;
    let member = join(directory.path(), &hub, "member", &owner.invite().await).await?;
    let identity = member.database.me().await?.person.key();
    owner
        .database
        .remove(rocky_sdk::PersonId::new(identity))
        .await?;
    let _ = member.sync().await;
    let _ = member.database.renew().await;
    assert_eq!(
        member
            .database
            .status()
            .await?
            .authority
            .verified_revocation
            .map(|notice| notice.reason),
        Some(rocky_sdk::status::RevocationReason::Identity)
    );
    member.close().await;
    owner.close().await;
    Ok(())
}
