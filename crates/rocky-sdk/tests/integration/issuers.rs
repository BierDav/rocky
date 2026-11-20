use common::{Device, Failure};
use rocky_sdk::{
    CallFailure, Database, GrantProfile, Network, Open, ReconciliationReport, RevocationMode,
    transport::memory::Hub,
};
use serde_json::Value;
use std::{collections::BTreeSet, path::Path, sync::Arc};

use crate::common;

fn open<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        identity: common::prepared_identity(directory),
        directory,
        schema: common::SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
fn create(root: &Path, hub: &Arc<Hub>, name: &str) -> Result<Device, Failure> {
    Device::new(Database::create(open(&root.join(name), hub, name), None)?)
}
fn join(root: &Path, hub: &Arc<Hub>, name: &str, invitation: &str) -> Result<Device, Failure> {
    Device::new(Database::join(
        open(&root.join(name), hub, name),
        invitation,
    )?)
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    Ok(serde_json::from_slice(&std::fs::read(
        root.join(name).join("identity.json"),
    )?)?)
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
async fn synced(device: &Device) -> Result<ReconciliationReport, Failure> {
    device.session().sync_once().await
}
fn reconciled_with(report: &ReconciliationReport, node: &str) -> bool {
    report.peers.iter().any(|peer| {
        peer.node == node
            && peer.attempts.iter().any(|attempt| {
                attempt.operation == rocky_sdk::AttemptOperation::Reconcile
                    && attempt.outcome == Some(rocky_sdk::AttemptOutcome::Completed)
            })
    })
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
async fn session(device: &Device) -> rocky_sdk::SessionInfo {
    device.session().info().await.expect("session")
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
    let a = create(root, &hub, "a")?;
    a.follow_main().await;
    let early = join(root, &hub, "early", &a.invite().await)?;
    a.add(1, "Milk").await;
    synced(&early).await?;

    let b = join(
        root,
        &hub,
        "b",
        &a.session().invitation_with(GrantProfile::Issuer).await?.uri,
    )?;
    let principal = session(&b).await.principal.to_hex();
    let grant = a.session().trust(&principal, a.root_signer()?).await?;
    b.session().install_trust(&grant.grant).await?;
    let root_key = a.root_signer()?;
    let (a_node, b_node) = (a.node().await, b.node().await);
    let (a_identity, b_identity) = (a.identity().await, b.identity().await);
    assert_issuers(root, "a", &[&b_node])?;
    assert_issuers(root, "b", &[&a_node])?;
    assert_eq!(
        identity(root, "a")?["contact_refs"][peer_id(&b_node)?]["contact"]["routes"][0]["address"],
        "b"
    );
    assert_ne!(
        session(&a).await.principal,
        session(&b).await.principal,
        "paired issuers share no identity key"
    );
    assert_ne!(
        session(&a).await.writer,
        session(&b).await.writer,
        "paired issuers share no writer key"
    );
    synced(&b).await?;
    assert_eq!(b.titles().await, ["Milk"]);

    let late = join(root, &hub, "late", &a.invite().await)?;
    assert_issuers(root, "late", &[&a_node, &b_node])?;
    assert_issuers(root, "early", &[&a_node])?;
    early.session().renew().await?;
    assert_issuers(root, "early", &[&a_node, &b_node])?;
    let report = synced(&late).await?;
    assert!(
        reconciled_with(&report, &b_node),
        "the second issuer serves a device it never saw by its chain: {report:?}"
    );
    let late_node = late.node().await;
    assert!(
        b.session()
            .peers()
            .await?
            .iter()
            .any(|peer| peer.node == late_node
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
        .session()
        .revoke_identity(&b_identity)
        .await
        .err()
        .ok_or("revoked itself")?;
    assert_eq!(
        CallFailure::from_failure(&error).code(),
        "publisher_required",
        "a trusted node does not revoke itself without the root: {error}"
    );
    let error = b
        .session()
        .revoke_identity(&a_identity)
        .await
        .err()
        .ok_or("revoked a trusted node without the root")?;
    assert_eq!(
        CallFailure::from_failure(&error).code(),
        "publisher_required",
        "a trusted node revokes no trusted node without the root: {error}"
    );
    let revoked = b
        .session()
        .revoke_trusted(&a_identity, mode, root_key)
        .await?;
    assert_eq!(
        revoked.revocations.statements, 1,
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
            .session()
            .renew()
            .await
            .err()
            .ok_or("renewed a cascaded identity")?;
        assert_eq!(
            CallFailure::from_failure(&error).code(),
            "revoked",
            "{error}"
        );
        let status = early.session().status().await?;
        assert_eq!(
            status
                .authority
                .verified_revocation
                .as_ref()
                .map(|notice| &notice.reason),
            Some(&rocky_sdk::RevocationReason::Cascade {
                trusted: a_identity.clone()
            }),
            "{status:?}"
        );
        assert!(early.session().renew().await.is_err());
        for device in [&b, &early, &late] {
            device.close().await;
        }
        drop(early);
        let early = Device::new(Database::open(open(&root.join("early"), &hub, "early"))?)?;
        let status = early.session().status().await?;
        assert_eq!(
            status
                .authority
                .verified_revocation
                .as_ref()
                .map(|notice| &notice.reason),
            Some(&rocky_sdk::RevocationReason::Cascade {
                trusted: a_identity
            }),
            "{status:?}"
        );
        assert!(early.session().renew().await.is_err());
        early.close().await;
        return Ok(());
    }

    b.seal_and_compact().await;
    let c = join(root, &hub, "c", &b.invite().await)?;
    assert_eq!(session(&c).await.revocations.statements, 1);
    let report = synced(&c).await?;
    assert!(reconciled_with(&report, &b_node), "{report:?}");
    assert_eq!(
        c.titles().await,
        ["Milk"],
        "the second issuer's invitee gets pre-rotation rows through a base at a seal"
    );

    let before = session(&early).await;
    assert_eq!(before.revocations.statements, 0);
    early.session().renew().await?;
    let after = session(&early).await;
    assert_eq!(
        after.revocations.statements, 1,
        "a device the creator enrolled renews through the second issuer"
    );
    assert_eq!(
        session(&b).await.expires,
        u64::MAX,
        "a trusted grant never expires"
    );
    assert!(
        before.expires <= after.expires && after.expires < u64::MAX,
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
        session(&late).await.revocations.statements,
        1,
        "a refused sync renews from the second issuer and learns the revocation"
    );
    assert_issuers(root, "late", &[&b_node])?;
    late.add(3, "Bread").await;
    synced(&late).await?;
    let report = synced(&c).await?;
    assert!(reconciled_with(&report, &b_node), "{report:?}");
    assert_eq!(c.titles().await, ["Bread", "Eggs", "Milk"]);
    assert_eq!(session(&c).await.revocations.statements, 1);
    assert_issuers(root, "early", &[&b_node])?;
    for device in [&b, &c, &early, &late] {
        device.close().await;
    }
    drop((c, early));
    for (name, titles) in [
        ("c", &["Bread", "Eggs", "Milk"][..]),
        ("early", &["Eggs", "Milk"][..]),
    ] {
        let device = Device::new(Database::open(open(&root.join(name), &hub, name))?)?;
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
async fn a_granted_credential_close_to_expiry_renews_through_the_second_issuer()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let b = join(
        root,
        &hub,
        "b",
        &a.session().invitation_with(GrantProfile::Issuer).await?.uri,
    )?;
    synced(&b).await.map_err(|error| {
        format!("the second issuer bootstraps its keys before their source is lost: {error}")
    })?;
    let keys = common::node_keys(&root.join("g"), None)?;
    let supplied = a
        .session()
        .grant(
            keys,
            GrantProfile::Writer {
                scopes: ["items".into()].into(),
            },
            rocky_sdk::GrantTerms {
                ttl: Some(60),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(supplied.issuers.len(), 2);
    let g = Device::new(Database::install(
        open(&root.join("g"), &hub, "g"),
        supplied,
    )?)?;
    let granted = session(&g).await.expires;
    destroy(root, "a", a).await?;

    g.session().renew().await?;
    let renewed = session(&g).await.expires;
    let one_day = 24 * 60 * 60;
    assert!(
        renewed > granted + one_day,
        "the second issuer renews for the domain's lifetime, not the 60 s grant: {granted} {renewed}"
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
    let a = create(root, &hub, "a")?;
    a.follow_main().await;
    let t = join(
        root,
        &hub,
        "t",
        &a.session().invitation_with(GrantProfile::Issuer).await?.uri,
    )?;
    let t_identity = session(&t).await.principal.to_hex();
    let grant = a.session().trust(&t_identity, a.root_signer()?).await?;
    t.session().install_trust(&grant.grant).await?;
    let (a_node, t_node) = (a.node().await, t.node().await);
    a.add(1, "Milk").await;
    synced(&t).await?;
    let enrolled_by_t = join(root, &hub, "d", &t.invite().await)?;
    assert_eq!(synced_titles(&enrolled_by_t).await?, ["Milk"]);
    let enrolled_by_a = join(root, &hub, "e", &a.invite().await)?;
    assert_eq!(synced_titles(&enrolled_by_a).await?, ["Milk"]);
    assert_issuers(root, "d", &[&a_node, &t_node])?;

    t.close().await;
    drop(t);
    enrolled_by_t
        .session()
        .renew()
        .await
        .map_err(|error| format!("renews through the creator, which never enrolled it: {error}"))?;

    let error = a
        .session()
        .revoke_identity(&t_identity)
        .await
        .err()
        .ok_or("revoked a trusted identity without the root")?;
    assert_eq!(
        CallFailure::from_failure(&error).code(),
        "publisher_required",
        "only the root revokes a trusted identity: {error}"
    );
    a.session()
        .revoke_trusted(&t_identity, RevocationMode::Compromised, a.root_signer()?)
        .await?;

    let _ = enrolled_by_t.sync().await;
    let _ = enrolled_by_t.session().renew().await;
    let status = enrolled_by_t.session().status().await?;
    assert_eq!(
        status
            .authority
            .verified_revocation
            .as_ref()
            .map(|revoked| &revoked.reason),
        Some(&rocky_sdk::RevocationReason::Cascade {
            trusted: t_identity
        }),
        "the cut-off device learns the revoked trusted node is why: {status:?}"
    );
    assert!(enrolled_by_t.session().renew().await.is_err());

    enrolled_by_a.add(2, "Eggs").await;
    let report = synced(&enrolled_by_a).await?;
    assert!(
        reconciled_with(&report, &a_node),
        "the creator's enrollee keeps working: {report:?}"
    );
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);

    a.seal_and_compact().await;
    let again = join(root, &hub, "d2", &a.invite().await)?;
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
    let owner = create(directory.path(), &hub, "owner")?;
    let member = join(directory.path(), &hub, "member", &owner.invite().await)?;
    let identity = member.session().info().await?.principal;
    owner.session().revoke_identity(&identity.to_hex()).await?;
    let _ = member.sync().await;
    let _ = member.session().renew().await;
    assert_eq!(
        member
            .session()
            .status()
            .await?
            .authority
            .verified_revocation
            .map(|notice| notice.reason),
        Some(rocky_sdk::RevocationReason::Identity)
    );
    member.close().await;
    owner.close().await;
    Ok(())
}
