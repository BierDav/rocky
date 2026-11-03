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
