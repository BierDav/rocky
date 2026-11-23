use rocky_core::{AppId, DomainId, MeshId, Scope};
use rocky_credentials::{
    Authority, CredentialChain, DAY, IdentityKey, MAX_CHAIN_LIFETIME, NEVER, Origin, PrivateKey,
    Revocation, RevocationMode, RevocationSet, RootKey, SignedCapability,
};

const APP: AppId = AppId([3; 16]);
const NOW: u64 = 2_000_000_000;
const SCOPE: Scope = Scope {
    mesh: MeshId([1; 16]),
    app: APP,
    domain: DomainId([2; 16]),
};

fn trusted(root: &RootKey) -> (PrivateKey, CredentialChain) {
    let key = PrivateKey::new();
    let grant = Authority::owner(root)
        .issue_trusted(key.public_key(), SCOPE, NOW)
        .unwrap();
    (key, CredentialChain(vec![grant]))
}
fn member(parent: &(PrivateKey, CredentialChain)) -> (PrivateKey, PrivateKey, CredentialChain) {
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let link = rocky_credentials::testing::authority(&parent.0, &parent.1)
        .issue(
            user.public_key(),
            vec![rocky_core::ResourceGrant {
                scope: rocky_core::ResourceScope {
                    domain: SCOPE,
                    table: [9; 32],
                    encryption: [8; 32],
                },
                rows: rocky_core::RowSelection::All,
                fields: rocky_core::FieldSelection::All,
                rights: [rocky_core::Right::Read].into(),
            }],
            rocky_credentials::Validity {
                issued_at: NOW,
                lifetime: std::time::Duration::from_secs(30 * DAY),
                clock_skew: std::time::Duration::from_secs(0),
            },
        )
        .unwrap();
    let mut chain = parent.1.clone();
    chain.0.push(link);
    let chain = chain.certify(&user, &node).unwrap();
    (user, node, chain)
}
fn revocation(by: &dyn rocky_credentials::IdentityProvider, identity: &PrivateKey) -> Revocation {
    Revocation {
        app: APP,
        issuer: by.public_key().unwrap(),
        time: NOW,
        nodes: Default::default(),
        identities: [identity.public_key()].into(),
        capabilities: Default::default(),
        cut: Default::default(),
        mode: rocky_credentials::RevocationMode::Lost,
    }
}
fn set(statement: rocky_credentials::SignedRevocation) -> RevocationSet {
    RevocationSet::try_from(vec![statement]).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_trusted_grant_never_expires_and_only_the_root_signs_one() {
    let root = RootKey::generate();
    let (key, grant) = trusted(&root);
    assert_eq!(grant.0[0].capability.expires, NEVER);
    let node = PrivateKey::new();
    let chain = grant.clone().certify(&key, &node).unwrap();
    let far = NOW + 100 * MAX_CHAIN_LIFETIME;
    assert_eq!(
        chain
            .trusted_node(
                root.public_key(),
                SCOPE,
                node.public_key(),
                Some((&RevocationSet::new(), far))
            )
            .unwrap(),
        key.public_key()
    );
    assert!(
        rocky_credentials::testing::authority(&key, &grant)
            .issue_trusted(PrivateKey::new().public_key(), SCOPE, NOW)
            .is_err(),
        "no trusted identity grants trust"
    );
    assert!(
        Authority::owner(&root)
            .renew(
                &grant.0[0].capability,
                &RevocationSet::new(),
                rocky_credentials::Validity {
                    issued_at: NOW,
                    lifetime: std::time::Duration::from_secs(DAY),
                    clock_skew: std::time::Duration::from_secs(0)
                }
            )
            .is_err()
    );
    let (_, _, device) = member(&(key, grant));
    assert!(
        device.expires() <= NOW + 30 * DAY,
        "a link below a trusted grant is bounded like any other"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_trusted_identity_cannot_revoke_another_and_the_root_can() {
    let root = RootKey::generate();
    let a = trusted(&root);
    let b = trusted(&root);
    let b_node = PrivateKey::new();
    let b_chain = b.1.clone().certify(&b.0, &b_node).unwrap();
    let by_a = rocky_credentials::testing::authority(&a.0, &a.1).revoke(Revocation {
        nodes: [rocky_credentials::PeerId::of(b_node.public_key())].into(),
        ..revocation(&a.0, &b.0)
    });
    assert!(by_a.is_err(), "issuers cannot target nodes");
    let by_a = set(rocky_credentials::testing::authority(&a.0, &a.1)
        .revoke(revocation(&a.0, &b.0))
        .unwrap());
    by_a.verify(root.public_key(), APP).unwrap();
    assert!(
        b_chain
            .trusted_node(
                root.public_key(),
                SCOPE,
                b_node.public_key(),
                Some((&by_a, NOW))
            )
            .is_ok(),
        "an identity target verifies, but another issuer cannot end protected trust"
    );
    let by_root = set(Authority::owner(&root)
        .revoke(revocation(&root, &b.0))
        .unwrap());
    assert!(
        b_chain
            .trusted_node(
                root.public_key(),
                SCOPE,
                b_node.public_key(),
                Some((&by_root, NOW))
            )
            .is_err()
    );
    let (_, device, chain) = member(&b);
    let by_a = set(rocky_credentials::testing::authority(&a.0, &a.1)
        .revoke(Revocation {
            nodes: Default::default(),
            identities: [chain.principal().unwrap()].into(),
            ..revocation(&a.0, &b.0)
        })
        .unwrap());
    assert!(
        chain
            .verify(root.public_key(), device.public_key(), &by_a, NOW)
            .is_err(),
        "a trusted identity still revokes what it did not get from the root"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn revoking_a_trusted_identity_cascades_to_what_it_first_enrolled() {
    let root = RootKey::generate();
    let hijacked = trusted(&root);
    let honest = trusted(&root);
    let (user, node, enrolled) = member(&hijacked);
    let origin = |chain: &CredentialChain| {
        chain.0[chain.0.len() - 2]
            .capability
            .extended()
            .unwrap()
            .origin
            .map(|origin| origin.identity)
    };
    assert_eq!(origin(&enrolled), Some(hijacked.0.public_key()));
    let renewed = rocky_credentials::testing::authority(&honest.0, &honest.1)
        .renew(
            &enrolled.0[1].capability,
            &RevocationSet::new(),
            rocky_credentials::Validity {
                issued_at: NOW,
                lifetime: std::time::Duration::from_secs(30 * DAY),
                clock_skew: std::time::Duration::from_secs(0),
            },
        )
        .unwrap();
    let mut chain = honest.1.clone();
    chain.0.push(renewed.clone());
    let renewed_chain = chain.certify(&user, &node).unwrap();
    assert_eq!(
        origin(&renewed_chain),
        Some(hijacked.0.public_key()),
        "the honest trusted node's renewal keeps the origin"
    );
    let (_, own_node, own) = member(&honest);

    let lost_statement = Authority::owner(&root)
        .revoke(revocation(&root, &hijacked.0))
        .unwrap();
    let lost = set(lost_statement.clone());
    assert!(lost.revokes_trusted(&hijacked.0.public_key()));
    assert!(!lost.revokes_origin(&hijacked.0.public_key()));
    assert!(lost.revoking_holder(&enrolled).unwrap().is_none());
    assert!(!lost_statement.ends_at(&renewed, false).unwrap());
    assert!(
        renewed_chain
            .verify(root.public_key(), node.public_key(), &lost, NOW)
            .is_ok()
    );
    let after_loss = rocky_credentials::testing::authority(&honest.0, &honest.1)
        .renew(
            &enrolled.0[1].capability,
            &lost,
            rocky_credentials::Validity {
                issued_at: NOW,
                lifetime: std::time::Duration::from_secs(30 * DAY),
                clock_skew: std::time::Duration::ZERO,
            },
        )
        .unwrap();
    assert_eq!(
        after_loss.capability.extended().unwrap().origin,
        renewed.capability.extended().unwrap().origin,
        "a surviving issuer renews even the chain that still names the lost issuer"
    );

    let revoked = set(Authority::owner(&root)
        .revoke(Revocation {
            mode: RevocationMode::Compromised,
            ..revocation(&root, &hijacked.0)
        })
        .unwrap());
    let verify = |chain: &CredentialChain, node: &PrivateKey| {
        chain.verify(root.public_key(), node.public_key(), &revoked, NOW)
    };
    assert!(verify(&enrolled, &node).is_err());
    assert!(verify(&renewed_chain, &node).is_err());
    assert!(verify(&own, &own_node).is_ok());
    assert!(
        rocky_credentials::testing::authority(&honest.0, &honest.1)
            .renew(
                &renewed.capability,
                &revoked,
                rocky_credentials::Validity {
                    issued_at: NOW,
                    lifetime: std::time::Duration::from_secs(30 * DAY),
                    clock_skew: std::time::Duration::from_secs(0)
                }
            )
            .is_err(),
        "no trusted node renews it again; the device enrolls anew"
    );
    assert!(revoked.revokes_origin(&hijacked.0.public_key()));
    let by_honest = set(rocky_credentials::testing::authority(&honest.0, &honest.1)
        .revoke(revocation(&honest.0, &hijacked.0))
        .unwrap());
    assert!(
        renewed_chain
            .verify(root.public_key(), node.public_key(), &by_honest, NOW)
            .is_ok(),
        "a trusted identity's statement naming another cascades to nothing"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_origin_states_its_trusted_grants_time() {
    let root = RootKey::generate();
    let lost = trusted(&root);
    let (_, node, honest) = member(&lost);
    let link = &honest.0[1].capability;
    assert_eq!(
        link.extended().unwrap().origin,
        Some(Origin {
            identity: lost.0.public_key(),
            since: lost.1.0[0].capability.not_before,
        })
    );
    let mut extended = link.extended().unwrap();
    extended.origin = extended.origin.map(|origin| Origin {
        since: NOW + 100 * DAY,
        ..origin
    });
    let forged = SignedCapability::sign(
        rocky_credentials::Capability {
            extension: rocky_credentials::Extension::new(extended).unwrap(),
            ..link.clone()
        },
        &lost.0,
    )
    .unwrap();
    let forged = CredentialChain(vec![lost.1.0[0].clone(), forged]);
    let error = forged
        .issuer(root.public_key(), None)
        .err()
        .map(|error| error.to_string());
    assert_eq!(
        error.as_deref(),
        Some("an origin misstates its trusted grant")
    );
    honest
        .verify(
            root.public_key(),
            node.public_key(),
            &RevocationSet::new(),
            NOW,
        )
        .unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_identity_key_never_signs_as_the_root() {
    let root = RootKey::generate();
    let restored = RootKey::from_phrase(&root.phrase()).unwrap();
    for identity in [root.identity(), IdentityKey::generate()] {
        let grant = Authority::owner(&identity)
            .issue_trusted(PrivateKey::new().public_key(), SCOPE, NOW)
            .unwrap();
        let chain = CredentialChain(vec![grant]);
        assert!(
            chain
                .trusted_identity(root.public_key(), SCOPE, &RevocationSet::new(), NOW)
                .is_err(),
            "a trusted grant an identity key signs names no trusted identity under the root, even derived from the root's secret"
        );
    }
    let (key, chain) = trusted(&restored);
    assert_eq!(
        chain
            .trusted_identity(root.public_key(), SCOPE, &RevocationSet::new(), NOW)
            .unwrap(),
        key.public_key(),
        "the root's own grant names its trusted identity, so the refusal above is the signer's"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_identity_phrase_and_secret_restore_the_same_signer() {
    let identity = IdentityKey::generate();
    let by_phrase = IdentityKey::from_phrase(&identity.phrase()).unwrap();
    let by_secret = IdentityKey::from_secret(identity.secret());
    let node = PrivateKey::new();
    let grant = CredentialChain(vec![
        Authority::owner(&RootKey::generate())
            .issue_trusted(identity.public_key(), SCOPE, NOW)
            .unwrap(),
    ]);
    let certify = |signer: &IdentityKey| grant.clone().certify(signer, &node).unwrap();
    assert_eq!(
        certify(&by_phrase),
        certify(&identity),
        "Ed25519 is deterministic: the same key signs the same bytes"
    );
    assert_eq!(certify(&by_secret), certify(&identity));
    assert_eq!(identity.phrase().split(' ').count(), 24);
}
