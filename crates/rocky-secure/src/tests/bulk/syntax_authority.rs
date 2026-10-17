//! A warm syntax fact never supplies time, revocation or signature authority.
use super::syntax::*;
use super::*;

#[tokio::test]
async fn warm_issuer_syntax_still_requires_current_time_and_unrevoked_writer() {
    let ((l, _, _, replica, signed), _) = fixture::issuer_candidate().await;
    assert!(!signed.endorsement.chain.0.is_empty());
    release(&replica, &signed).await;
    let frame = frames(&replica).await.remove(0);
    let cap = &signed.endorsement.chain.0[0].capability;
    let valid = cap.not_before + 1;
    {
        let inner = replica.inner.lock().await;
        inner.migration_syntax_capacity(32768);
        let before = snapshot(&inner).await;
        let cold = reconstruct(&inner, &frame).await.unwrap();
        inner
            .check_stored_migration_for_test(&cold, valid)
            .await
            .unwrap();
        let warm = reconstruct(&inner, &frame).await.unwrap();
        assert_eq!(inner.migration_syntax_stats().0, 1);
        assert_eq!(inner.migration_syntax_stats().1, 1);
        same_error(
            inner
                .check_stored_migration_for_test(&warm, cap.expires)
                .await
                .unwrap_err(),
            crate::credential(
                signed
                    .verify_active(
                        l.publisher().public_key(),
                        inner.graph_for(l.domain).unwrap(),
                        &RevocationSet::new(),
                        cap.expires,
                    )
                    .unwrap_err(),
            ),
        );
        assert_eq!(snapshot(&inner).await, before);
    }
    // Supported set reconciliation; revoke only the writer node, keeping the opening reachable.
    let time = now().unwrap();
    let statement = SignedRevocation::sign(
        Revocation {
            app: l.domain.app,
            issuer: l.publisher().public_key(),
            time,
            nodes: [rocky_credentials::PeerId::of(
                PublicKey::from_bytes(&signed.statement.writer).unwrap(),
            )]
            .into(),
            identities: Default::default(),
            capabilities: Default::default(),
        },
        l.publisher(),
        &CredentialChain::default(),
    )
    .unwrap();
    let revoked = RevocationSet::try_from(vec![statement]).unwrap();
    replica.learn_revocations(&revoked).await.unwrap();
    let inner = replica.inner.lock().await;
    let before = snapshot(&inner).await;
    let warm = reconstruct(&inner, &frame).await.unwrap();
    assert!(inner.migration_syntax_stats().1 >= 2);
    same_error(
        inner
            .check_stored_migration_for_test(&warm, valid)
            .await
            .unwrap_err(),
        crate::credential(rocky_credentials::Error::Denied(
            "migration writer revoked".into(),
        )),
    );
    assert_eq!(snapshot(&inner).await, before);
    drop(inner);
    // Historical audit is deliberately independent of current acquisition expiry/revocation.
    replica.audit(l.scope("items")).await.unwrap();
}

#[tokio::test]
async fn warm_bad_signatures_parse_but_actual_acquisition_refuses_every_time() {
    let ((l, _, _, replica, signed), issuer) = fixture::issuer_candidate().await;
    release(&replica, &signed).await;
    let frame = frames(&replica).await.remove(0);
    let time = now().unwrap();
    let mut bad_endorsement = signed.clone();
    let mut other = signed.statement.clone();
    other.bytes += 1;
    bad_endorsement.endorsement = rocky_credentials::Endorsement::sign(
        &issuer,
        &signed.endorsement.chain,
        "rocky.migration-publication.v1",
        &other,
    )
    .unwrap();
    let mut binding = serde_json::to_value(&signed.writer).unwrap();
    binding["signature"] =
        serde_json::to_value(l.publisher().sign(b"different writer signature")).unwrap();
    let mut bad_writer = signed.clone();
    bad_writer.writer = serde_json::from_value(binding).unwrap();
    for (bad, reason) in [
        (bad_endorsement, "invalid tip statement signature"),
        (bad_writer, "migration writer assignment"),
    ] {
        let inner = replica.inner.lock().await;
        let before = snapshot(&inner).await;
        inner
            .check_stored_migration_for_test(&frame, time)
            .await
            .unwrap();
        inner.migration_syntax_capacity(32768);
        save(&inner).await;
        signed_bytes(&inner, bad.to_bytes().unwrap()).await;
        for _ in 0..2 {
            let reconstructed = reconstruct(&inner, &frame).await.unwrap();
            assert_eq!(descriptor(&reconstructed), &bad);
            same_error(
                inner
                    .check_stored_migration_for_test(&reconstructed, time)
                    .await
                    .unwrap_err(),
                crate::credential(rocky_credentials::Error::Denied(reason.into())),
            );
        }
        assert_eq!(inner.migration_syntax_stats().0, 1);
        assert_eq!(inner.migration_syntax_stats().1, 1);
        restore(&inner).await;
        assert_eq!(snapshot(&inner).await, before);
        drop(inner);
        let before = snapshot(&*replica.inner.lock().await).await;
        same_error(
            replica.admit_migration_publication(bad).await.unwrap_err(),
            crate::credential(rocky_credentials::Error::Denied(reason.into())),
        );
        assert_eq!(snapshot(&*replica.inner.lock().await).await, before);
    }
    replica.audit(l.scope("items")).await.unwrap();
}

#[tokio::test]
async fn warm_descriptor_uses_the_new_current_migration_minimum() {
    let (l, _, _, replica, signed) = candidate(1).await;
    release(&replica, &signed).await;
    let frame = frames(&replica).await.remove(0);
    {
        let inner = replica.inner.lock().await;
        inner.migration_syntax_capacity(32768);
        let cold = reconstruct(&inner, &frame).await.unwrap();
        inner
            .check_stored_migration_for_test(&cold, now().unwrap())
            .await
            .unwrap();
    }
    replica
        .publish_tip_statements(&[TipStatement::GenesisLine(
            SignedGenesisLine::sign(
                GenesisLineStatement {
                    format: 1,
                    scope: l.domain,
                    minimum_in_sync: 3,
                },
                l.publisher(),
            )
            .unwrap(),
        )])
        .await
        .unwrap();
    let inner = replica.inner.lock().await;
    let before = snapshot(&inner).await;
    let warm = reconstruct(&inner, &frame).await.unwrap();
    assert_eq!(inner.migration_syntax_stats().0, 1);
    assert!(inner.migration_syntax_stats().1 > 0);
    let error = inner
        .check_stored_migration_for_test(&warm, now().unwrap())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("migration minimum"), "{error}");
    assert_eq!(snapshot(&inner).await, before);
    drop(inner);
    replica.audit(l.scope("items")).await.unwrap();
}
