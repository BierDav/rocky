//! Node-signed authority: a node certificate never expires and holds no
//! grants; it lasts while its identity's capability does and until the
//! identity or the node is revoked. Issuers sign with node keys.
use rocky_core::{
    AppId, DomainId, MeshId, ResourceGrant, ResourceScope, Right, RowSelection, Scope,
};
use rocky_credentials::*;

const NOW: u64 = 1_800_000_000;
const APP: AppId = AppId([7; 16]);
const SCOPE: Scope = Scope {
    mesh: MeshId([1; 16]),
    app: APP,
    domain: DomainId([2; 16]),
};

fn grant() -> ResourceGrant {
    ResourceGrant {
        scope: ResourceScope {
            domain: SCOPE,
            table: [3; 32],
            encryption: [4; 32],
        },
        rows: RowSelection::All,
        rights: [Right::Read, Right::Insert].into(),
        fields: rocky_core::FieldSelection::All,
    }
}
fn revoking(root: &PrivateKey, revocation: impl FnOnce(&mut Revocation)) -> RevocationSet {
    let mut statement = Revocation {
        app: APP,
        issuer: root.public_key(),
        time: NOW,
        nodes: Default::default(),
        identities: Default::default(),
        capabilities: Default::default(),
        cut: Default::default(),
        recovery: None,
    };
    revocation(&mut statement);
    RevocationSet::try_from(vec![Authority::owner(root).revoke(statement).unwrap()]).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_certificate_outlives_three_renewals_and_ends_with_its_identity() {
    let (root, identity, node) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let none = RevocationSet::new();
    // A capability for a day, and the one certificate its identity signs.
    let mut capability = Authority::owner(&root)
        .issue(
            identity.public_key(),
            vec![grant()],
            AuthorRange::new(10, 19).unwrap(),
            (NOW, DAY, 0),
        )
        .unwrap();
    let certificate = CredentialChain(vec![capability.clone()])
        .certify(&identity, node.public_key(), 10)
        .unwrap()
        .certificate()
        .unwrap()
        .clone();
    assert_eq!(certificate.capability.expires, NEVER);
    assert!(certificate.capability.grants.is_empty());
    // Three renewals replace the capability only; the identity signs none
    // of them, and the same certificate verifies under each.
    for renewal in 1..=3 {
        let time = NOW + renewal * DAY - 60;
        let expired =
            CredentialChain(vec![capability.clone()]).with_certificate(certificate.clone());
        assert!(
            expired
                .verify(root.public_key(), node.public_key(), &none, time + 120)
                .is_err()
        );
        capability = Authority::owner(&root)
            .renew(&capability.capability, &none, (time, DAY, 0))
            .unwrap();
        let credentials = Credentials {
            publisher: root.public_key(),
            revocations: none.clone(),
            capabilities: CredentialChain(vec![capability.clone()]),
            certificate: Some(certificate.clone()),
        };
        let verified = credentials
            .verify(node.public_key(), APP, time + 120)
            .unwrap();
        assert_eq!(verified.principal, identity.public_key());
        assert_eq!(verified.grants, vec![grant()]);
    }
    // Revoking the identity refuses the certificate under any capability.
    let chain = CredentialChain(vec![capability]).with_certificate(certificate);
    let time = NOW + 3 * DAY;
    assert!(
        chain
            .verify(root.public_key(), node.public_key(), &none, time)
            .is_ok()
    );
    let revoked = revoking(&root, |statement| {
        statement.identities = [identity.public_key()].into();
    });
    assert!(
        chain
            .verify(root.public_key(), node.public_key(), &revoked, time)
            .is_err()
    );
    // Revoking only the node refuses it as well.
    let unlinked = revoking(&root, |statement| {
        statement.nodes = [PeerId::of(node.public_key())].into();
    });
    assert!(
        chain
            .verify(root.public_key(), node.public_key(), &unlinked, time)
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn issuers_sign_with_node_keys_and_a_revoked_node_signs_nothing_new() {
    let (root, issuer, member) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let trusted = CredentialChain(vec![
        Authority::owner(&root)
            .issue_trusted(
                issuer.public_key(),
                SCOPE,
                AuthorRange::new(1, 99).unwrap(),
                NOW,
            )
            .unwrap(),
    ]);
    let node = PrivateKey::new();
    let node_chain = trusted
        .clone()
        .certify(&issuer, node.public_key(), 1)
        .unwrap();
    // The node, not the identity, signs a capability; it names the
    // identity as its issuer and verifies as the identity's grant.
    let authority = Authority::issuer(&node, &node_chain).unwrap();
    assert_eq!(authority.identity().unwrap(), issuer.public_key());
    let link = authority
        .issue(
            member.public_key(),
            vec![grant()],
            AuthorRange::new(100, 109).unwrap(),
            (NOW, DAY, 0),
        )
        .unwrap();
    assert_eq!(link.issuer, issuer.public_key());
    assert_eq!(
        link.attestation().certificate().unwrap().node(),
        node.public_key()
    );
    let member_node = PrivateKey::new();
    let member_chain = authority
        .chain_of(link)
        .certify(&member, member_node.public_key(), 100)
        .unwrap();
    let none = RevocationSet::new();
    assert!(
        member_chain
            .verify(root.public_key(), member_node.public_key(), &none, NOW)
            .is_ok()
    );
    // The identity's own key signing an issuer statement is refused.
    assert!(Endorsement::sign(&issuer, &trusted, "rocky.test", &1u8).is_err());
    // Its grants come from a link its node signed for it; its node
    // certificate holds none.
    let held = authority
        .hold(vec![grant()], Vec::new(), (NOW, DAY, 0))
        .unwrap();
    assert_eq!(held.capability.expires, NEVER);
    let own = CredentialChain(vec![trusted.0[0].clone(), held])
        .with_certificate(node_chain.certificate().unwrap().clone());
    assert_eq!(
        own.verify(
            root.public_key(),
            node.public_key(),
            &none,
            NOW + 1000 * DAY
        )
        .unwrap(),
        vec![grant()]
    );
    // Once the root revokes the signing node, what it signed is refused
    // and it signs nothing that verifies; another node of the identity
    // still issues.
    let unlinked = revoking(&root, |statement| {
        statement.nodes = [PeerId::of(node.public_key())].into();
    });
    assert!(
        member_chain
            .verify(root.public_key(), member_node.public_key(), &unlinked, NOW)
            .is_err()
    );
    let other = PrivateKey::new();
    let other_chain = trusted
        .clone()
        .certify(&issuer, other.public_key(), 2)
        .unwrap();
    let renewed = Authority::issuer(&other, &other_chain)
        .unwrap()
        .renew(&member_chain.0[1].capability, &unlinked, (NOW, DAY, 0))
        .unwrap();
    let mut renewed_chain = trusted.clone();
    renewed_chain.0.push(renewed);
    let renewed_chain = renewed_chain.with_certificate(member_chain.certificate().unwrap().clone());
    assert!(
        renewed_chain
            .verify(root.public_key(), member_node.public_key(), &unlinked, NOW)
            .is_ok()
    );
}
