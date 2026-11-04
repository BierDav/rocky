//! The offline root's authority: trusted grants that never expire, issued
//! identities that remember the trusted identity that first enrolled them,
//! and revocations of trusted identities that only the root signs and that
//! cascade by that origin.
use rocky_core::{AppId, DomainId, MeshId, Scope};
use rocky_credentials::{
    AuthorRange, Authority, CredentialChain, DAY, MAX_CHAIN_LIFETIME, NEVER, PrivateKey,
    Revocation, RevocationSet, RootKey,
};

const APP: AppId = AppId([3; 16]);
const NOW: u64 = 2_000_000_000;
const SCOPE: Scope = Scope {
    mesh: MeshId([1; 16]),
    app: APP,
    domain: DomainId([2; 16]),
};

/// A trusted identity: its key and its root-signed grant.
fn trusted(root: &RootKey, authors: AuthorRange) -> (PrivateKey, CredentialChain) {
    let key = PrivateKey::new();
    let grant = Authority::owner(root)
        .issue_trusted(key.public_key(), SCOPE, authors, NOW)
        .unwrap();
    (key, CredentialChain(vec![grant]))
}
/// An issuer identity `parent` enrolls below it.
fn issuer(parent: &(PrivateKey, CredentialChain)) -> (PrivateKey, CredentialChain) {
    let key = PrivateKey::new();
    let link = Authority::issuer(&parent.0, &parent.1)
        .issue_issuer(key.public_key(), None, (NOW, 30 * DAY, 0))
        .unwrap();
    let mut chain = parent.1.clone();
    chain.0.push(link);
    (key, chain)
}
/// A member identity `parent` enrolls, and its node's chain.
fn member(parent: &(PrivateKey, CredentialChain)) -> (PrivateKey, PrivateKey, CredentialChain) {
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let link = Authority::issuer(&parent.0, &parent.1)
        .issue_terms(
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
            (1, Some(AuthorRange::new(100, 199).unwrap())),
            (NOW, 30 * DAY, 0),
        )
        .unwrap();
    let mut chain = parent.1.clone();
    chain.0.push(link);
    let chain = chain.certify(&user, node.public_key(), 100).unwrap();
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
    }
}
fn set(statement: rocky_credentials::SignedRevocation) -> RevocationSet {
    RevocationSet::try_from(vec![statement]).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_trusted_grant_never_expires_and_only_the_root_signs_one() {
    let root = RootKey::generate();
    let (key, grant) = trusted(&root, AuthorRange::ALL);
    assert_eq!(grant.0[0].capability.expires, NEVER);
    let node = PrivateKey::new();
    let chain = grant.clone().certify(&key, node.public_key(), 1).unwrap();
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
    // No trusted identity grants trust, and nobody renews a trusted grant.
    assert!(
        Authority::issuer(&key, &grant)
            .issue_trusted(PrivateKey::new().public_key(), SCOPE, AuthorRange::ALL, NOW)
            .is_err()
    );
    assert!(
        Authority::owner(&root)
            .renew(&grant.0[0].capability, &RevocationSet::new(), (NOW, DAY, 0))
            .is_err()
    );
    // A link below a trusted grant is bounded like any other.
    let (_, _, device) = member(&(key, grant));
    assert!(device.expires() <= NOW + 30 * DAY);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_trusted_identity_cannot_revoke_another_and_the_root_can() {
    let root = RootKey::generate();
    let a = trusted(&root, AuthorRange::new(1, 1000).unwrap());
    let b = trusted(&root, AuthorRange::new(1001, 2000).unwrap());
    let b_node = PrivateKey::new();
    let b_chain =
        b.1.clone()
            .certify(&b.0, b_node.public_key(), 1001)
            .unwrap();
    let by_a = Authority::issuer(&a.0, &a.1).revoke(Revocation {
        nodes: [rocky_credentials::PeerId::of(b_node.public_key())].into(),
        ..revocation(&a.0, &b.0)
    });
    // The statement verifies, as any issuer's does, yet ends nothing of b.
    let by_a = set(by_a.unwrap());
    by_a.verify(root.public_key(), APP).unwrap();
    assert!(
        b_chain
            .trusted_node(
                root.public_key(),
                SCOPE,
                b_node.public_key(),
                Some((&by_a, NOW))
            )
            .is_ok()
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
    // A trusted identity still revokes what it did not get from the root.
    let (_, device, chain) = member(&b);
    let by_a = set(Authority::issuer(&a.0, &a.1)
        .revoke(Revocation {
            nodes: [rocky_credentials::PeerId::of(device.public_key())].into(),
            identities: Default::default(),
            ..revocation(&a.0, &b.0)
        })
        .unwrap());
    assert!(
        chain
            .verify(root.public_key(), device.public_key(), &by_a, NOW)
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn revoking_a_trusted_identity_cascades_to_what_it_first_enrolled() {
    let root = RootKey::generate();
    let hijacked = trusted(&root, AuthorRange::new(1, 1000).unwrap());
    let honest = trusted(&root, AuthorRange::new(1001, 2000).unwrap());
    let (user, node, enrolled) = member(&hijacked);
    let origin = |chain: &CredentialChain| {
        chain.0[chain.0.len() - 2]
            .capability
            .extended()
            .unwrap()
            .origin
    };
    assert_eq!(origin(&enrolled), Some(hijacked.0.public_key()));
    // The honest trusted node renews it: the origin stays.
    let renewed = Authority::issuer(&honest.0, &honest.1)
        .renew(
            &enrolled.0[1].capability,
            &RevocationSet::new(),
            (NOW, 30 * DAY, 0),
        )
        .unwrap();
    let mut chain = honest.1.clone();
    chain.0.push(renewed.clone());
    let renewed_chain = chain.certify(&user, node.public_key(), 100).unwrap();
    assert_eq!(origin(&renewed_chain), Some(hijacked.0.public_key()));
    // An issuer the hijacked node enrolled hands its origin on.
    let below = issuer(&hijacked);
    let (_, below_node, below_member) = member(&below);
    assert_eq!(origin(&below_member), Some(hijacked.0.public_key()));
    let (_, own_node, own) = member(&honest);

    let revoked = set(Authority::owner(&root)
        .revoke(revocation(&root, &hijacked.0))
        .unwrap());
    let verify = |chain: &CredentialChain, node: &PrivateKey| {
        chain.verify(root.public_key(), node.public_key(), &revoked, NOW)
    };
    assert!(verify(&enrolled, &node).is_err());
    assert!(verify(&renewed_chain, &node).is_err());
    assert!(verify(&below_member, &below_node).is_err());
    assert!(verify(&own, &own_node).is_ok());
    // No trusted node renews it again; the device enrolls anew.
    assert!(
        Authority::issuer(&honest.0, &honest.1)
            .renew(&renewed.capability, &revoked, (NOW, 30 * DAY, 0))
            .is_err()
    );
    assert!(revoked.revokes_origin(&hijacked.0.public_key()));
    // A trusted identity's statement naming another cascades to nothing.
    let by_honest = set(Authority::issuer(&honest.0, &honest.1)
        .revoke(revocation(&honest.0, &hijacked.0))
        .unwrap());
    assert!(
        renewed_chain
            .verify(root.public_key(), node.public_key(), &by_honest, NOW)
            .is_ok()
    );
}
