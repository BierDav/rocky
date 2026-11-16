//! Issuer recovery: a second issuer keeps a domain enrolling, renewing and
//! revoking after its creator is lost; compromise ends its first enrollees.
// The SDK's blocking constructors, native transports and paused clocks.
#![cfg(not(target_family = "wasm"))]
use common::{Device, Failure};
use rocky_sdk::{
    CallFailure, Database, GrantProfile, Network, Open, ReconciliationReport, RevocationMode,
    transport::memory::Hub,
};
use serde_json::Value;
use std::{collections::BTreeSet, path::Path, sync::Arc};

mod common;

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
/// Persisted renewal contacts are indexed by opaque peer references.
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
/// Syncs until `device` reads its rows: a fresh member first renews with
/// the keys, then fetches the rows. Bounded; never a sleep.
async fn synced_titles(device: &Device) -> Result<Vec<String>, Failure> {
    let read = rocky_sdk::ReadQuery::new("SELECT title FROM items", 1);
    for _ in 0..4 {
        synced(device).await?;
        match device.session().read(read.clone()).await {
            Ok(_) => return Ok(device.titles().await),
            Err(_) => continue,
        }
    }
    Err("the rows never became readable".into())
}
async fn session(device: &Device) -> rocky_sdk::SessionInfo {
    device.session().info().await.expect("session")
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
    // Enrolled before a second issuer exists: it knows only the creator.
    let early = join(root, &hub, "early", &a.invite().await)?;
    a.add(1, "Milk").await;
    synced(&early).await?;

    let b = join(
        root,
        &hub,
        "b",
        &a.session().invitation_with(GrantProfile::Issuer).await?.uri,
    )?;
    // The root trusts the second issuer; only it may later revoke the creator.
    let principal = session(&b).await.principal.to_hex();
    let grant = a.session().trust(&principal, a.root_signer()?).await?;
    b.session().install_trust(&grant.grant).await?;
    let root_key = a.root_signer()?;
    let (a_node, b_node) = (a.node().await, b.node().await);
    let (a_identity, b_identity) = (a.identity().await, b.identity().await);
    // Each issuer knows the other's routes; no keys are shared beyond the
    // publisher key.
    assert_issuers(root, "a", &[&b_node])?;
    assert_issuers(root, "b", &[&a_node])?;
    assert_eq!(
        identity(root, "a")?["contact_refs"][peer_id(&b_node)?]["contact"]["routes"][0]["address"],
        "b"
    );
    assert_ne!(session(&a).await.principal, session(&b).await.principal);
    assert_ne!(session(&a).await.writer, session(&b).await.writer);
    synced(&b).await?;
    assert_eq!(b.titles().await, ["Milk"]);

    // Devices enrolled or renewed after the pairing learn the second issuer.
    let late = join(root, &hub, "late", &a.invite().await)?;
    assert_issuers(root, "late", &[&a_node, &b_node])?;
    assert_issuers(root, "early", &[&a_node])?;
    early.session().renew().await?;
    assert_issuers(root, "early", &[&a_node, &b_node])?;
    // The second issuer serves a device it never saw: its chain admits it.
    let report = synced(&late).await?;
    assert!(reconciled_with(&report, &b_node), "{report:?}");
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

    // The creator device is destroyed.
    a.close().await;
    drop(a);
    std::fs::remove_dir_all(root.join("a"))?;

    // A trusted node revokes no trusted node, itself included, without
    // the root; with it, it revokes the lost one.
    let error = b
        .session()
        .revoke_identity(&b_identity)
        .await
        .err()
        .ok_or("revoked itself")?;
    assert_eq!(
        CallFailure::from_failure(&error).code(),
        "publisher_required",
        "{error}"
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
        "{error}"
    );
    let revoked = b
        .session()
        .revoke_trusted(&a_identity, mode, root_key)
        .await?;
    assert_eq!(revoked.revocations.statements, 1);
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

    // The second issuer invites a third device, which syncs with it: the
    // rows before the rotation reach it through a base at a seal.
    b.seal_and_compact().await;
    let c = join(root, &hub, "c", &b.invite().await)?;
    assert_eq!(session(&c).await.revocations.statements, 1);
    let report = synced(&c).await?;
    assert!(reconciled_with(&report, &b_node), "{report:?}");
    assert_eq!(c.titles().await, ["Milk"]);

    // A device the creator enrolled renews through the second issuer and
    // forgets the revoked creator.
    let before = session(&early).await;
    assert_eq!(before.revocations.statements, 0);
    early.session().renew().await?;
    let after = session(&early).await;
    assert_eq!(after.revocations.statements, 1);
    // A device chain keeps its own lifetime; the trusted grant above it
    // never expires.
    assert_eq!(session(&b).await.expires, u64::MAX);
    assert!(before.expires <= after.expires && after.expires < u64::MAX);
    assert_issuers(root, "early", &[&b_node])?;
    assert!(
        identity(root, "early")?["contact_refs"]
            .get(peer_id(&a_node)?)
            .is_none()
    );
    early.add(2, "Eggs").await;
    assert_eq!(early.titles().await, ["Eggs", "Milk"]);
    // Each sync completes with the second issuer, not only returns.
    let report = synced(&early).await?;
    assert!(reconciled_with(&report, &b_node), "{report:?}");
    assert_eq!(b.titles().await, ["Eggs", "Milk"]);
    let report = synced(&c).await?;
    assert!(reconciled_with(&report, &b_node), "{report:?}");
    assert_eq!(c.titles().await, ["Eggs", "Milk"]);

    // A refused sync renews from the second issuer, learning the
    // revocation; its earlier chain stays current.
    synced(&late).await?;
    assert_eq!(session(&late).await.revocations.statements, 1);
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
    // The rows survive reopening.
    for (name, titles) in [
        ("c", &["Bread", "Eggs", "Milk"][..]),
        ("early", &["Eggs", "Milk"][..]),
    ] {
        let device = Device::new(Database::open(open(&root.join(name), &hub, name))?)?;
        assert_eq!(device.titles().await, titles);
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
    // Finish the second issuer's key bootstrap before destroying its source.
    synced(&b).await?;
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
    a.close().await;
    drop(a);
    std::fs::remove_dir_all(root.join("a"))?;

    g.session().renew().await?;
    let renewed = session(&g).await.expires;
    // The second issuer renews it for the domain's lifetime, not 60 seconds.
    assert!(renewed > granted + 24 * 60 * 60, "{granted} {renewed}");
    g.add(1, "Milk").await;
    let report = synced(&g).await?;
    assert_eq!(b.titles().await, ["Milk"], "{report:?}");
    for device in [&b, &g] {
        device.close().await;
    }
    Ok(())
}

/// The root marks a second trusted identity compromised: every identity it
/// first enrolled loses access, even one the creator renewed since, and its
/// status says why; a device the creator enrolled keeps working, and the
/// cut-off user enrolls again through the honest trusted node. The second
/// node signed no tip statement, which its revocation would drop.
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
    // `d` is the second trusted node's enrollment, `e` the creator's.
    let d = join(root, &hub, "d", &t.invite().await)?;
    assert_eq!(synced_titles(&d).await?, ["Milk"]);
    let e = join(root, &hub, "e", &a.invite().await)?;
    assert_eq!(synced_titles(&e).await?, ["Milk"]);
    // `d` learned both trusted nodes when it synced.
    assert_issuers(root, "d", &[&a_node, &t_node])?;

    // The second trusted node goes away; `d` renews through the creator,
    // which never enrolled it.
    t.close().await;
    drop(t);
    d.session().renew().await?;

    // Only the root revokes a trusted identity.
    let error = a
        .session()
        .revoke_identity(&t_identity)
        .await
        .err()
        .ok_or("revoked a trusted identity without the root")?;
    assert_eq!(
        CallFailure::from_failure(&error).code(),
        "publisher_required",
        "{error}"
    );
    a.session()
        .revoke_trusted(&t_identity, RevocationMode::Compromised, a.root_signer()?)
        .await?;

    // `d` learns it is cut off, and that the revoked trusted node is why.
    let _ = d.sync().await;
    let _ = d.session().renew().await;
    let status = d.session().status().await?;
    assert_eq!(
        status
            .authority
            .verified_revocation
            .as_ref()
            .map(|revoked| &revoked.reason),
        Some(&rocky_sdk::RevocationReason::Cascade {
            trusted: t_identity
        }),
        "{status:?}"
    );
    assert!(d.session().renew().await.is_err());

    // `e` keeps working with the creator.
    e.add(2, "Eggs").await;
    let report = synced(&e).await?;
    assert!(reconciled_with(&report, &a_node), "{report:?}");
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);

    // The cut-off user enrolls again through the honest trusted node; the
    // rows before the rotation reach it through a base at a seal.
    a.seal_and_compact().await;
    let again = join(root, &hub, "d2", &a.invite().await)?;
    assert_eq!(synced_titles(&again).await?, ["Eggs", "Milk"]);
    for device in [&a, &d, &e, &again] {
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
