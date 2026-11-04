//! A member that is not one of the control's trusted identities claims
//! counter values through a claim right the owner grants, until the owner
//! revokes it.
use super::{fixture::*, *};

fn denied(error: &Failure) -> bool {
    error.downcast_ref::<CoordinationError>() == Some(&CoordinationError::Denied)
}
use std::sync::atomic::Ordering;

/// Whether `peer` holds revocations its coordination service has not
/// acknowledged.
async fn unpushed(peer: &Peer) -> Result<bool, Failure> {
    Ok(peer.session.status().await?.authority.revocations.unpushed)
}
/// An owner registered as the domain's only trusted identity on `bundle`.
async fn owner(root: &std::path::Path, bundle: &TrustBundle) -> Result<Peer, Failure> {
    let a = Peer::start(&root.join("a"), SCHEMA, Some(bundle), None, true).await?;
    let owner = a.call(json!({"type":"session"})).await?["principal"].clone();
    let tenant = PrivateKey::new();
    a.call(
        json!({"type":"coordination_register","tenant":tenant.to_hex(),"registration":vec![1u8;32],
        "trusted":[owner],"lifetime":3600,"root":a.root()}),
    )
    .await?;
    Ok(a)
}
async fn principal(peer: &Peer) -> Result<String, Failure> {
    Ok(peer.call(json!({"type":"session"})).await?["principal"]
        .as_str()
        .ok_or("principal")?
        .to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_claims_through_its_claim_right_until_it_is_revoked() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let a = Peer::start(&root.path().join("a"), SCHEMA, Some(&bundle), None, true).await?;
    let owner = a.call(json!({"type":"session"})).await?["principal"].clone();
    let tenant = PrivateKey::new();
    a.call(
        json!({"type":"coordination_register","tenant":tenant.to_hex(),"registration":vec![1u8;32],
        "trusted":[owner],"lifetime":3600,"root":a.root()}),
    )
    .await?;
    let m = Peer::start(
        &root.path().join("m"),
        SCHEMA,
        Some(&bundle),
        Some(a.invite("writer").await?),
        false,
    )
    .await?;
    introduce(&a, &m).await?;
    let member = m.call(json!({"type":"session"})).await?["principal"]
        .as_str()
        .ok_or("principal")?
        .to_owned();
    let (owner_db, member_db) = (&a.opened.database, &m.opened.database);
    let invoices = counters::Name::new("invoices")?;

    // Not trusted and holding no right: refused, though it asked its issuer.
    let first = counters::Claim::fresh()?;
    assert!(denied(
        &member_db.claim(invoices.clone(), first).await.unwrap_err()
    ));

    let grant = a
        .session
        .grant_claims(&member, Some(&[b"invoices"]), 1)
        .await?;
    assert_eq!(grant.identity, member);
    m.session.install_claims(&grant).await?;
    assert_eq!(member_db.claim(invoices.clone(), first).await?, 1);
    assert_eq!(member_db.claim(invoices.clone(), first).await?, 1);
    assert_eq!(
        owner_db
            .claim(invoices.clone(), counters::Claim::fresh()?)
            .await?,
        2
    );
    let reading = member_db.counter(invoices.clone(), Some(first)).await?;
    assert_eq!(
        (reading.value, reading.claim),
        (2, Some(counters::Lookup::Claimed(1)))
    );
    // The right names one counter.
    let credits = counters::Name::new("credits")?;
    assert!(denied(
        &member_db
            .claim(credits, counters::Claim::fresh()?)
            .await
            .unwrap_err()
    ));
    // Revoking it reaches the service at once; even a retry is refused.
    a.session.revoke_claims(&member).await?;
    assert!(denied(
        &member_db.claim(invoices.clone(), first).await.unwrap_err()
    ));
    assert!(denied(
        &member_db
            .claim(invoices.clone(), counters::Claim::fresh()?)
            .await
            .unwrap_err()
    ));
    assert_eq!(
        owner_db.claim(invoices, counters::Claim::fresh()?).await?,
        3
    );
    m.close().await?;
    a.close().await
}

/// Granting an identity a narrower right replaces its previous one: the
/// links of the previous grant, still unexpired, are refused at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn granting_again_revokes_the_previous_grant() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let a = owner(root.path(), &bundle).await?;
    let m = Peer::start(
        &root.path().join("m"),
        SCHEMA,
        Some(&bundle),
        Some(a.invite("writer").await?),
        false,
    )
    .await?;
    introduce(&a, &m).await?;
    let member = principal(&m).await?;
    let (invoices, credits) = (
        counters::Name::new("invoices")?,
        counters::Name::new("credits")?,
    );
    let wide = a.session.grant_claims(&member, None, 1).await?;
    m.session.install_claims(&wide).await?;
    assert_eq!(
        m.opened
            .database
            .claim(credits.clone(), counters::Claim::fresh()?)
            .await?,
        1
    );

    let narrow = a
        .session
        .grant_claims(&member, Some(&[b"invoices"]), 1)
        .await?;
    // The member still presents the wide link; its grant was revoked.
    assert!(denied(
        &m.opened
            .database
            .claim(credits.clone(), counters::Claim::fresh()?)
            .await
            .unwrap_err()
    ));
    m.session.install_claims(&narrow).await?;
    assert_eq!(
        m.opened
            .database
            .claim(invoices, counters::Claim::fresh()?)
            .await?,
        1
    );
    assert!(denied(
        &m.opened
            .database
            .claim(credits, counters::Claim::fresh()?)
            .await
            .unwrap_err()
    ));
    m.close().await?;
    a.close().await
}

/// The issuer that granted a right goes away for good; another issuer
/// renews the holder's link from the replicated registry and it claims on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_issuer_renews_a_right_after_its_granting_issuer_is_gone() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let a = Peer::start(&root.path().join("a"), SCHEMA, Some(&bundle), None, true).await?;
    let owner = a.call(json!({"type":"session"})).await?["principal"].clone();
    let tenant = PrivateKey::new();
    a.call(
        json!({"type":"coordination_register","tenant":tenant.to_hex(),"registration":vec![1u8;32],
        "trusted":[owner],"lifetime":3600,"root":a.root()}),
    )
    .await?;
    let b = Peer::start(
        &root.path().join("b"),
        SCHEMA,
        Some(&bundle),
        Some(a.invite("issuer").await?),
        false,
    )
    .await?;
    introduce(&a, &b).await?;
    let m = Peer::start(
        &root.path().join("m"),
        SCHEMA,
        Some(&bundle),
        Some(a.invite("writer").await?),
        false,
    )
    .await?;
    introduce(&a, &m).await?;
    introduce(&b, &m).await?;
    let member = m.call(json!({"type":"session"})).await?["principal"]
        .as_str()
        .ok_or("principal")?
        .to_owned();
    let b_node = b.call(json!({"type":"session"})).await?["node"].clone();
    let invoices = counters::Name::new("invoices")?;

    let grant = a
        .session
        .grant_claims(&member, Some(&[b"invoices"]), 1)
        .await?;
    m.session.install_claims(&grant).await?;
    assert_eq!(
        m.opened
            .database
            .claim(invoices.clone(), counters::Claim::fresh()?)
            .await?,
        1
    );
    // B receives the grant's registry row; the member learns B as an issuer.
    b.sync().await?;
    m.call(json!({"type":"renew"})).await?;
    a.close().await?;

    m.call(json!({"type":"renew"})).await?;
    let held = m.session.held_claims().await?.ok_or("a held right")?;
    assert_eq!(Some(json!(held.issuer)), Some(b_node));
    assert!(held.expires >= grant.expires);
    assert_eq!(held.counters, grant.counters);
    assert_eq!(
        m.opened
            .database
            .claim(invoices, counters::Claim::fresh()?)
            .await?,
        2
    );
    m.close().await?;
    b.close().await
}

/// The owner dismisses an issuer while the coordination service is down.
/// The issuer, not yet told, signs its holder a fresh link. Once the service
/// is back, the next sync of a node holding the revocation pushes it, and
/// the fresh link is refused: no one retries by hand.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_pushed_while_the_service_was_down_reaches_it_at_a_later_sync()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin, gate) = gated_service(root.path()).await?;
    let a = owner(root.path(), &bundle).await?;
    let b = Peer::start(
        &root.path().join("b"),
        SCHEMA,
        Some(&bundle),
        Some(a.invite("issuer").await?),
        false,
    )
    .await?;
    let m = Peer::start(
        &root.path().join("m"),
        SCHEMA,
        Some(&bundle),
        Some(a.invite("writer").await?),
        false,
    )
    .await?;
    introduce(&a, &m).await?;
    let (admin, member) = (principal(&b).await?, principal(&m).await?);
    let invoices = counters::Name::new("invoices")?;
    let grant = b
        .session
        .grant_claims(&member, Some(&[b"invoices"]), 1)
        .await?;
    m.session.install_claims(&grant).await?;
    assert_eq!(
        m.opened
            .database
            .claim(invoices.clone(), counters::Claim::fresh()?)
            .await?,
        1
    );

    gate.store(false, Ordering::SeqCst);
    a.call(json!({"type":"revoke","identity":admin})).await?;
    assert!(unpushed(&a).await?);
    // The dismissed issuer keeps signing; its holder installs the link.
    let fresh = b
        .session
        .grant_claims(&member, Some(&[b"invoices"]), 1)
        .await?;
    m.session.install_claims(&fresh).await?;

    gate.store(true, Ordering::SeqCst);
    // The member, not the revoking owner, syncs first: it learns the
    // statement from the owner and pushes it in the same pass.
    m.sync().await?;
    assert!(!unpushed(&m).await?);
    assert!(denied(
        &m.opened
            .database
            .claim(invoices.clone(), counters::Claim::fresh()?)
            .await
            .unwrap_err()
    ));
    assert_eq!(
        a.opened
            .database
            .claim(invoices, counters::Claim::fresh()?)
            .await?,
        2
    );
    m.close().await?;
    b.close().await?;
    a.close().await
}

/// A revocation the service never acknowledged stays pending across a
/// restart and is pushed by the reopened node's first sync.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unpushed_revocation_survives_a_restart() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin, gate) = gated_service(root.path()).await?;
    let a = owner(root.path(), &bundle).await?;
    let m = Peer::start(
        &root.path().join("m"),
        SCHEMA,
        Some(&bundle),
        Some(a.invite("writer").await?),
        false,
    )
    .await?;
    introduce(&a, &m).await?;
    let member = principal(&m).await?;
    let invoices = counters::Name::new("invoices")?;
    let grant = a
        .session
        .grant_claims(&member, Some(&[b"invoices"]), 1)
        .await?;
    m.session.install_claims(&grant).await?;
    let first = counters::Claim::fresh()?;
    assert_eq!(m.opened.database.claim(invoices.clone(), first).await?, 1);
    // The member stops syncing with peers, so it never learns the statement
    // and never pushes it itself; its claims still reach the service.
    m.session.set_networking_enabled(false).await?;

    gate.store(false, Ordering::SeqCst);
    a.session.revoke_claims(&member).await?;
    assert!(unpushed(&a).await?);
    a.close().await?;

    gate.store(true, Ordering::SeqCst);
    // The member's link is still live: until the push, the service answers.
    assert_eq!(m.opened.database.claim(invoices.clone(), first).await?, 1);
    let a = Peer::start(&root.path().join("a"), SCHEMA, Some(&bundle), None, false).await?;
    assert!(unpushed(&a).await?);
    a.sync().await?;
    assert!(!unpushed(&a).await?);
    assert!(denied(
        &m.opened
            .database
            .claim(invoices, counters::Claim::fresh()?)
            .await
            .unwrap_err()
    ));
    m.close().await?;
    a.close().await
}
