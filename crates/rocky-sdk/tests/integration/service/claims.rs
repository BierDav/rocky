use super::{fixture::*, *};
use crate::support::snapshot as snap;

fn denied(error: &Failure) -> bool {
    error.downcast_ref::<CoordinationError>() == Some(&CoordinationError::Denied)
}
use std::sync::atomic::Ordering;

async fn unpushed(peer: &Peer) -> Result<bool, Failure> {
    Ok(peer.session.status().await?.authority.revocations.unpushed)
}
async fn owner(root: &std::path::Path, bundle: &TrustBundle) -> Result<Peer, Failure> {
    let a = Peer::start(&root.join("a"), SCHEMA, Some(bundle), None, true).await?;
    let owner = snap::value(a.session.info()).await?["principal"].clone();
    let tenant = PrivateKey::new();
    snap::value(
        a.publish_coordination(
            rocky_coordinator::control::ControlRequest {
                tenant: rocky_sdk::credentials::PrivateKey::from_bytes(&snap::digest(
                    &(tenant.to_hex()),
                )),
                trusted: serde_json::from_value::<Vec<rocky_sdk::credentials::PublicKey>>(json!([
                    owner
                ]))
                .expect("typed fixture"),
                lifetime: 3600,
                id: serde_json::from_value::<[u8; 32]>(json!(vec![1u8; 32]))
                    .expect("typed fixture"),
                revision: rocky_coordinator::control::ControlRevision::Register {
                    grant: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedCoordinationGrant>>,
                    >(json!(null))
                    .expect("typed fixture"),
                    admission: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedTenantAdmission>>,
                    >(json!(null))
                    .expect("typed fixture"),
                },
            },
            Some((a.root()).clone()),
        ),
    )
    .await?;
    Ok(a)
}
async fn principal(peer: &Peer) -> Result<String, Failure> {
    Ok(snap::value(peer.session.info()).await?["principal"]
        .as_str()
        .ok_or("principal")?
        .to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_claims_through_its_claim_right_until_it_is_revoked() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let a = Peer::start(&root.path().join("a"), SCHEMA, Some(&bundle), None, true).await?;
    let owner = snap::value(a.session.info()).await?["principal"].clone();
    let tenant = PrivateKey::new();
    snap::value(
        a.publish_coordination(
            rocky_coordinator::control::ControlRequest {
                tenant: rocky_sdk::credentials::PrivateKey::from_bytes(&snap::digest(
                    &(tenant.to_hex()),
                )),
                trusted: serde_json::from_value::<Vec<rocky_sdk::credentials::PublicKey>>(json!([
                    owner
                ]))
                .expect("typed fixture"),
                lifetime: 3600,
                id: serde_json::from_value::<[u8; 32]>(json!(vec![1u8; 32]))
                    .expect("typed fixture"),
                revision: rocky_coordinator::control::ControlRevision::Register {
                    grant: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedCoordinationGrant>>,
                    >(json!(null))
                    .expect("typed fixture"),
                    admission: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedTenantAdmission>>,
                    >(json!(null))
                    .expect("typed fixture"),
                },
            },
            Some((a.root()).clone()),
        ),
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
    let member = snap::value(m.session.info()).await?["principal"]
        .as_str()
        .ok_or("principal")?
        .to_owned();
    let (owner_db, member_db) = (&a.opened.database, &m.opened.database);
    let invoices = counters::Name::new("invoices")?;

    let first = counters::Claim::fresh()?;
    assert!(
        denied(&member_db.claim(invoices.clone(), first).await.unwrap_err()),
        "a member without trust or a right is refused, though it asked its issuer"
    );

    let grant = a
        .session
        .grant_claims(&member, rocky_sdk::ClaimCounters::Named(&[b"invoices"]), 1)
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
    let credits = counters::Name::new("credits")?;
    assert!(
        denied(
            &member_db
                .claim(credits, counters::Claim::fresh()?)
                .await
                .unwrap_err()
        ),
        "a right names one counter"
    );
    a.session.revoke_claims(&member).await?;
    assert!(
        denied(&member_db.claim(invoices.clone(), first).await.unwrap_err()),
        "revoking the right reaches the service at once and even a retry is refused"
    );
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
    let wide = a
        .session
        .grant_claims(&member, rocky_sdk::ClaimCounters::Every, 1)
        .await?;
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
        .grant_claims(&member, rocky_sdk::ClaimCounters::Named(&[b"invoices"]), 1)
        .await?;
    assert!(
        denied(
            &m.opened
                .database
                .claim(credits.clone(), counters::Claim::fresh()?)
                .await
                .unwrap_err()
        ),
        "the still-presented wide link was revoked"
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_issuer_renews_a_right_after_its_granting_issuer_is_gone() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let a = Peer::start(&root.path().join("a"), SCHEMA, Some(&bundle), None, true).await?;
    let owner = snap::value(a.session.info()).await?["principal"].clone();
    let tenant = PrivateKey::new();
    snap::value(
        a.publish_coordination(
            rocky_coordinator::control::ControlRequest {
                tenant: rocky_sdk::credentials::PrivateKey::from_bytes(&snap::digest(
                    &(tenant.to_hex()),
                )),
                trusted: serde_json::from_value::<Vec<rocky_sdk::credentials::PublicKey>>(json!([
                    owner
                ]))
                .expect("typed fixture"),
                lifetime: 3600,
                id: serde_json::from_value::<[u8; 32]>(json!(vec![1u8; 32]))
                    .expect("typed fixture"),
                revision: rocky_coordinator::control::ControlRevision::Register {
                    grant: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedCoordinationGrant>>,
                    >(json!(null))
                    .expect("typed fixture"),
                    admission: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedTenantAdmission>>,
                    >(json!(null))
                    .expect("typed fixture"),
                },
            },
            Some((a.root()).clone()),
        ),
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
    let member = snap::value(m.session.info()).await?["principal"]
        .as_str()
        .ok_or("principal")?
        .to_owned();
    let b_node = snap::value(b.session.info()).await?["node"].clone();
    let invoices = counters::Name::new("invoices")?;

    let grant = a
        .session
        .grant_claims(&member, rocky_sdk::ClaimCounters::Named(&[b"invoices"]), 1)
        .await?;
    m.session.install_claims(&grant).await?;
    assert_eq!(
        m.opened
            .database
            .claim(invoices.clone(), counters::Claim::fresh()?)
            .await?,
        1
    );
    b.sync().await?;
    snap::value(async {
        m.session.renew().await?;
        m.session.info().await
    })
    .await?;
    a.close().await?;

    snap::value(async {
        m.session.renew().await?;
        m.session.info().await
    })
    .await?;
    let held = m.session.held_claims().await?.ok_or("a held right")?;
    assert_eq!(
        Some(json!(held.issuer)),
        Some(b_node),
        "the member renews through B once B received the grant's registry row"
    );
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
        .grant_claims(&member, rocky_sdk::ClaimCounters::Named(&[b"invoices"]), 1)
        .await?;
    m.session.install_claims(&grant).await?;
    assert_eq!(
        m.opened
            .database
            .claim(invoices.clone(), counters::Claim::fresh()?)
            .await?,
        1
    );

    a.session.set_networking_enabled(false).await?;
    gate.store(false, Ordering::SeqCst);
    snap::value(a.session.revoke_identity(&(snap::text(&(admin))))).await?;
    assert!(unpushed(&a).await?);
    let fresh_from_dismissed_issuer = b
        .session
        .grant_claims(&member, rocky_sdk::ClaimCounters::Named(&[b"invoices"]), 1)
        .await?;
    m.session
        .install_claims(&fresh_from_dismissed_issuer)
        .await?;

    gate.store(true, Ordering::SeqCst);
    a.session.set_networking_enabled(true).await?;
    m.sync().await?;
    assert!(
        !unpushed(&m).await?,
        "a member syncing first learns the owner's statement and pushes it in the same pass"
    );
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
        .grant_claims(&member, rocky_sdk::ClaimCounters::Named(&[b"invoices"]), 1)
        .await?;
    m.session.install_claims(&grant).await?;
    let first = counters::Claim::fresh()?;
    assert_eq!(m.opened.database.claim(invoices.clone(), first).await?, 1);
    m.session.set_networking_enabled(false).await?;
    a.session.set_networking_enabled(false).await?;

    gate.store(false, Ordering::SeqCst);
    a.session.revoke_claims(&member).await?;
    assert!(unpushed(&a).await?);
    a.close().await?;

    gate.store(true, Ordering::SeqCst);
    assert_eq!(
        m.opened.database.claim(invoices.clone(), first).await?,
        1,
        "until the revocation is pushed, the member's link is live"
    );
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
