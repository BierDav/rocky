use super::*;
use rocky_credentials::{AuthorRange, Authority, MAX_MEMBERSHIP_NODES};

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn relayed_frames_cannot_hide_nodes_past_the_membership_bound() {
    let fixture = Fixture::new();
    let directory = Scratch::new().await;
    let receiver_key = PrivateKey::new();
    let receiver_identity = PrivateKey::new();
    let receiver_chain = CredentialChain(vec![
        Authority::owner(&fixture.publisher)
            .issue(
                receiver_identity.public_key(),
                fixture.full(),
                AuthorRange::new(1, 99).unwrap(),
                validity(),
            )
            .unwrap(),
    ])
    .certify(&receiver_identity, receiver_key.public_key(), 1)
    .unwrap();
    let mut config = fixture.config(&receiver_key, fixture.full(), &[fixture.public]);
    config.credentials = receiver_chain;
    let receiver =
        EncryptedReplica::open(directory.path().join("receiver.db"), fixture.app(), config)
            .await
            .unwrap();
    let identity = PrivateKey::new();
    let links = CredentialChain(vec![
        Authority::owner(&fixture.publisher)
            .issue(
                identity.public_key(),
                fixture.full(),
                AuthorRange::new(100, 199).unwrap(),
                validity(),
            )
            .unwrap(),
    ]);
    let mut previous = None;
    let mut frames = Vec::new();
    // Separate offline replicas each know only their own certificate. A carrier
    // can relay their frames without volunteering their membership set.
    for sequence in 0..=MAX_MEMBERSHIP_NODES {
        let node = PrivateKey::new();
        let chain = links
            .clone()
            .certify_member(
                &identity,
                node.public_key(),
                100 + sequence as u32,
                sequence as u64,
                previous,
            )
            .unwrap();
        previous = Some(chain.certificate().unwrap().id().unwrap());
        let mut config = fixture.config(&node, fixture.full(), &[fixture.public]);
        config.credentials = chain;
        let source = EncryptedReplica::open(
            directory.path().join(format!("source-{sequence}.db")),
            fixture.app(),
            config,
        )
        .await
        .unwrap();
        frames.push(
            source
                .author(
                    [sequence as u8 + 1; 32],
                    vec![change(
                        fixture.public,
                        sequence as u8 + 1,
                        None,
                        Some(fixture.app().name(&format!("member-{sequence}"))),
                    )],
                    vec![],
                )
                .await
                .unwrap(),
        );
    }
    let results = receiver.receive_all(frames).await;
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        MAX_MEMBERSHIP_NODES
    );
    assert!(results[MAX_MEMBERSHIP_NODES].is_err());
    let known = receiver.revocations().await;
    assert_eq!(
        known
            .membership()
            .unwrap()
            .next_sequence(identity.public_key())
            .unwrap(),
        33
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_existing_link_refreshes_membership_without_changing_the_row_revision() {
    let f = Fixture::new();
    let directory = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a_config = f.config(&alice, f.full(), &[f.public]);
    let b_config = f.config(&bob, f.full(), &[f.public]);
    let (a_chain, b_chain) = (a_config.credentials.clone(), b_config.credentials.clone());
    let a = EncryptedReplica::open(directory.path().join("a.db"), f.app(), a_config)
        .await
        .unwrap();
    let b = EncryptedReplica::open(directory.path().join("b.db"), f.app(), b_config)
        .await
        .unwrap();
    let (mut client, mut server) =
        crate::network::link::testing::sessions((&a, alice, a_chain), (&b, bob, b_chain))
            .await
            .unwrap();
    let identity = PrivateKey::new();
    let chain = CredentialChain(vec![
        Authority::owner(&f.publisher)
            .issue(
                identity.public_key(),
                f.full(),
                AuthorRange::new(100, 199).unwrap(),
                validity(),
            )
            .unwrap(),
    ])
    .certify(&identity, PrivateKey::new().public_key(), 100)
    .unwrap();
    let certificate = chain.certificate().unwrap().clone();
    let mut evidence = RevocationSet::new();
    evidence.link(certificate.clone()).unwrap();
    let a_revision = a.snapshot(f.public).await.unwrap().revision;
    let b_revision = b.snapshot(f.public).await.unwrap().revision;
    let digest = a.revocation_digest().await.unwrap();
    let events = a.observe_membership().await;
    a.learn_revocations(&evidence).await.unwrap();
    assert!(events.has_changed().unwrap());
    assert_eq!(a.revocation_digest().await.unwrap(), digest);
    let (served, refreshed) = tokio::join!(server.serve_to_end(&b), async {
        client.refresh_revocations(&a).await.unwrap();
        client.close().await.unwrap();
    });
    served.unwrap();
    let () = refreshed;
    assert!(
        b.revocations()
            .await
            .certificates()
            .any(|held| held == &certificate)
    );
    assert_eq!(a.snapshot(f.public).await.unwrap().revision, a_revision);
    assert_eq!(b.snapshot(f.public).await.unwrap().revision, b_revision);
}
