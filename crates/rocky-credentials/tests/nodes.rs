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
        .certify_member(
            &issuer,
            other.public_key(),
            2,
            1,
            Some(node_chain.certificate().unwrap().id().unwrap()),
        )
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_identity_unlinks_only_its_certified_node_and_the_unlink_does_not_expire() {
    let (root, identity, first, second) = (
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    );
    let grant = Authority::owner(&root)
        .issue_trusted(
            identity.public_key(),
            SCOPE,
            AuthorRange::new(1, 32).unwrap(),
            NOW,
        )
        .unwrap();
    let links = CredentialChain(vec![grant]);
    let first_chain = links
        .clone()
        .certify_member(&identity, first.public_key(), 1, 0, None)
        .unwrap();
    let head = first_chain.certificate().unwrap().id().unwrap();
    let second_chain = links
        .certify_member(&identity, second.public_key(), 2, 1, Some(head))
        .unwrap();
    assert!(
        first_chain
            .unlink(&identity, APP, NOW, second.public_key())
            .is_err()
    );
    assert!(
        first_chain
            .unlink(&PrivateKey::new(), APP, NOW, first.public_key())
            .is_err()
    );
    let unlink = first_chain
        .unlink(&identity, APP, NOW, first.public_key())
        .unwrap();
    unlink.verify(root.public_key(), APP).unwrap();
    assert!(unlink.live(NOW + 10 * MAX_CHAIN_LIFETIME));
    let revoked = RevocationSet::try_from(vec![unlink]).unwrap();
    assert!(
        first_chain
            .verify(root.public_key(), first.public_key(), &revoked, NOW)
            .is_err()
    );
    second_chain
        .verify(root.public_key(), second.public_key(), &revoked, NOW)
        .unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_node_delegates_its_identitys_rights_without_the_identity_key() {
    let (root, identity, node, recipient, recipient_node) = (
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    );
    let grant = Authority::owner(&root)
        .issue_delegable(
            identity.public_key(),
            vec![grant()],
            2,
            AuthorRange::new(1, 128).unwrap(),
            (NOW, DAY, 0),
        )
        .unwrap();
    let held = CredentialChain(vec![grant])
        .certify(&identity, node.public_key(), 1)
        .unwrap();
    drop(identity);
    let bindings = held
        .0
        .iter()
        .map(|link| link.author_binding.clone())
        .collect::<Vec<_>>();
    AuthorBinding::verify_chain(
        &bindings,
        root.public_key(),
        self::grant().scope.table,
        1,
        node.public_key(),
    )
    .unwrap();
    assert!(
        AuthorBinding::verify_chain(&bindings, root.public_key(), [99; 32], 1, node.public_key())
            .is_err()
    );
    let delegated = held
        .capabilities()
        .delegate(
            Signer::node(&node, &held).unwrap(),
            recipient.public_key(),
            rocky_core::PrincipalKind::User,
            vec![self::grant()],
            1,
            Some(AuthorRange::new(64, 96).unwrap()),
            (NOW, DAY, 0),
        )
        .unwrap()
        .certify(&recipient, recipient_node.public_key(), 64)
        .unwrap();
    delegated
        .verify(
            root.public_key(),
            recipient_node.public_key(),
            &RevocationSet::new(),
            NOW,
        )
        .unwrap();
    let ended = revoking(&root, |statement| {
        statement.nodes.insert(PeerId::of(node.public_key()));
    });
    assert!(
        delegated
            .verify(root.public_key(), recipient_node.public_key(), &ended, NOW)
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn replicated_membership_voids_both_forks_and_relinks_without_reusing_authors() {
    let root = PrivateKey::new();
    let identity = PrivateKey::new();
    let links = CredentialChain(vec![
        Authority::owner(&root)
            .issue_trusted(
                identity.public_key(),
                SCOPE,
                AuthorRange::new(1, 100).unwrap(),
                NOW,
            )
            .unwrap(),
    ]);
    let first = PrivateKey::new();
    let first_chain = links
        .clone()
        .certify_member(&identity, first.public_key(), 1, 0, None)
        .unwrap();
    let predecessor = first_chain.certificate().unwrap().id().unwrap();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let a_chain = links
        .clone()
        .certify_member(&identity, a.public_key(), 2, 1, Some(predecessor))
        .unwrap();
    let b_chain = links
        .clone()
        .certify_member(&identity, b.public_key(), 2, 1, Some(predecessor))
        .unwrap();
    let mut left = RevocationSet::new();
    left.link(first_chain.certificate().unwrap().clone())
        .unwrap();
    left.link(a_chain.certificate().unwrap().clone()).unwrap();
    let mut right = RevocationSet::new();
    right.link(b_chain.certificate().unwrap().clone()).unwrap();
    a_chain
        .verify(root.public_key(), a.public_key(), &left, NOW)
        .unwrap();
    assert!(!left.has_revocations());
    left.merge(&right, root.public_key(), APP, NOW).unwrap();
    assert!(left.has_revocations());
    assert!(left.membership().unwrap().is_forked(identity.public_key()));
    for (chain, key) in [(&a_chain, &a), (&b_chain, &b)] {
        assert!(
            chain
                .verify(root.public_key(), key.public_key(), &left, NOW)
                .is_err()
        );
    }
    first_chain
        .verify(root.public_key(), first.public_key(), &left, NOW)
        .unwrap();
    let view = left.membership().unwrap();
    assert_eq!(view.next_sequence(identity.public_key()).unwrap(), 2);
    let recovered = PrivateKey::new();
    let recovered_chain = links
        .certify_member(
            &identity,
            recovered.public_key(),
            3,
            2,
            view.head(identity.public_key())
                .map(|head| head.certificate),
        )
        .unwrap();
    left.link(recovered_chain.certificate().unwrap().clone())
        .unwrap();
    recovered_chain
        .verify(root.public_key(), recovered.public_key(), &left, NOW)
        .unwrap();
    let restored = RevocationSet::from_bytes(&left.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, left);
    right.merge(&left, root.public_key(), APP, NOW).unwrap();
    assert_eq!(right.digest(NOW).unwrap(), left.digest(NOW).unwrap());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn unlink_reuses_a_membership_slot_but_never_its_author_number() {
    let root = PrivateKey::new();
    let identity = PrivateKey::new();
    let links = CredentialChain(vec![
        Authority::owner(&root)
            .issue_trusted(
                identity.public_key(),
                SCOPE,
                AuthorRange::new(1, 100).unwrap(),
                NOW,
            )
            .unwrap(),
    ]);
    let mut known = RevocationSet::new();
    let mut chains = Vec::new();
    for sequence in 0..MAX_MEMBERSHIP_NODES as u64 {
        let key = PrivateKey::new();
        let chain = links
            .clone()
            .certify_member(
                &identity,
                key.public_key(),
                1 + sequence as u32,
                sequence,
                known
                    .membership()
                    .unwrap()
                    .head(identity.public_key())
                    .map(|head| head.certificate),
            )
            .unwrap();
        known.link(chain.certificate().unwrap().clone()).unwrap();
        chains.push(chain);
    }
    assert!(
        !known
            .membership()
            .unwrap()
            .can_link(identity.public_key(), |node| known.revokes_reference(&node))
    );
    let unlink = chains[0]
        .unlink(
            &identity,
            APP,
            NOW,
            chains[0].certificate().unwrap().capability.subject,
        )
        .unwrap();
    known
        .merge(
            &RevocationSet::try_from(vec![unlink]).unwrap(),
            root.public_key(),
            APP,
            NOW,
        )
        .unwrap();
    assert!(
        known
            .membership()
            .unwrap()
            .can_link(identity.public_key(), |node| known.revokes_reference(&node))
    );
    let replacement = PrivateKey::new();
    let membership = known.membership().unwrap();
    let sequence = membership.next_sequence(identity.public_key()).unwrap();
    assert_eq!(sequence, 32);
    let chain = links
        .clone()
        .certify_member(
            &identity,
            replacement.public_key(),
            33,
            sequence,
            membership
                .head(identity.public_key())
                .map(|head| head.certificate),
        )
        .unwrap();
    assert!(
        links
            .certify_member(
                &identity,
                replacement.public_key(),
                1,
                sequence,
                membership
                    .head(identity.public_key())
                    .map(|head| head.certificate)
            )
            .is_err()
    );
    known.link(chain.certificate().unwrap().clone()).unwrap();
    chain
        .verify(root.public_key(), replacement.public_key(), &known, NOW)
        .unwrap();
    assert_eq!(chain.author_number().unwrap(), 33);
    assert!(
        !known
            .membership()
            .unwrap()
            .can_link(identity.public_key(), |node| known.revokes_reference(&node))
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_certificate_verifies_before_its_predecessor_history_arrives() {
    let root = PrivateKey::new();
    let identity = PrivateKey::new();
    let links = CredentialChain(vec![
        Authority::owner(&root)
            .issue_trusted(
                identity.public_key(),
                SCOPE,
                AuthorRange::new(1, 100).unwrap(),
                NOW,
            )
            .unwrap(),
    ]);
    let first = links
        .clone()
        .certify(&identity, PrivateKey::new().public_key(), 1)
        .unwrap();
    let node = PrivateKey::new();
    let second = links
        .certify_member(
            &identity,
            node.public_key(),
            2,
            1,
            Some(first.certificate().unwrap().id().unwrap()),
        )
        .unwrap();
    let mut known = RevocationSet::new();
    known.link(second.certificate().unwrap().clone()).unwrap();
    assert!(
        known
            .membership()
            .unwrap()
            .head(identity.public_key())
            .is_none()
    );
    second
        .verify(root.public_key(), node.public_key(), &known, NOW)
        .unwrap();
    known.link(first.certificate().unwrap().clone()).unwrap();
    assert_eq!(
        known
            .membership()
            .unwrap()
            .head(identity.public_key())
            .unwrap()
            .sequence,
        1
    );
    second
        .verify(root.public_key(), node.public_key(), &known, NOW)
        .unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_unlink_cannot_end_another_identitys_authority_for_the_same_node_key() {
    let root = PrivateKey::new();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let node = PrivateKey::new();
    let certify = |identity: &PrivateKey, first| {
        CredentialChain(vec![
            Authority::owner(&root)
                .issue_trusted(
                    identity.public_key(),
                    SCOPE,
                    AuthorRange::new(first, first + 10).unwrap(),
                    NOW,
                )
                .unwrap(),
        ])
        .certify(identity, node.public_key(), first)
        .unwrap()
    };
    let a_chain = certify(&a, 1);
    let b_chain = certify(&b, 101);
    let statement = a_chain.unlink(&a, APP, NOW, node.public_key()).unwrap();
    let mut known = RevocationSet::try_from(vec![statement]).unwrap();
    known.link(b_chain.certificate().unwrap().clone()).unwrap();
    assert!(
        a_chain
            .verify(root.public_key(), node.public_key(), &known, NOW)
            .is_err()
    );
    b_chain
        .verify(root.public_key(), node.public_key(), &known, NOW)
        .unwrap();
    assert!(known.revoking(&b_chain).unwrap().is_none());
    assert!(!known.revokes(&node.public_key()));
    assert!(known.ends_node_of(a.public_key(), node.public_key(), true));
    assert!(!known.ends_node_of(b.public_key(), node.public_key(), true));
    let signed = Endorsement::sign(
        Signer::node(&node, &b_chain).unwrap(),
        &b_chain.capabilities(),
        "rocky.test.scoped-unlink",
        &1u8,
    )
    .unwrap();
    signed
        .verify_current(
            root.public_key(),
            "rocky.test.scoped-unlink",
            &1u8,
            &known,
            NOW,
        )
        .unwrap();
    assert_eq!(
        RevocationSet::from_bytes(&known.to_bytes().unwrap()).unwrap(),
        known
    );
}
