//! A member that is not one of the control's trusted identities claims
//! counter values through a claim right the owner grants, until the owner
//! revokes it.
use super::{fixture::*, *};

fn denied(error: &Failure) -> bool {
    error.downcast_ref::<CoordinationError>() == Some(&CoordinationError::Denied)
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
        "trusted":[owner],"lifetime":3600}),
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
        "trusted":[owner],"lifetime":3600}),
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
