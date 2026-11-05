//! The offline root's authority: trusted grants that never expire, issued
//! identities that remember the trusted identity that first enrolled them,
//! and revocations of trusted identities that only the root signs and that
//! cascade by that origin.
use rocky_core::{AppId, DomainId, MeshId, Scope};
use rocky_credentials::{
    AuthorRange, Authority, CredentialChain, DAY, MAX_CHAIN_LIFETIME, NEVER, Origin, PrivateKey,
    Revocation, RevocationSet, RootKey, RootRecovery, SignedCapability,
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
    trusted_at(root, authors, NOW)
}
/// A trusted identity the root granted at `time`.
fn trusted_at(root: &RootKey, authors: AuthorRange, time: u64) -> (PrivateKey, CredentialChain) {
    let key = PrivateKey::new();
    let grant = Authority::owner(root)
        .issue_trusted(key.public_key(), SCOPE, authors, time)
        .unwrap();
    (key, CredentialChain(vec![grant]))
}
/// A recovery's statement keeping the trusted identity `recovered`.
fn recovery(root: &RootKey, recovered: &(PrivateKey, CredentialChain), slot: u32) -> RevocationSet {
    let statement = Revocation {
        app: APP,
        issuer: root.public_key(),
        time: NOW,
        nodes: Default::default(),
        identities: Default::default(),
        capabilities: Default::default(),
        cut: Default::default(),
        recovery: Some(RootRecovery {
            since: recovered.1.0[0].capability.not_before,
            keep: recovered.0.public_key(),
            slot,
        }),
    };
    set(Authority::owner(root).revoke(statement).unwrap())
}
/// A trusted identity's node chain.
fn node_of(trusted: &(PrivateKey, CredentialChain), author: u32) -> (PrivateKey, CredentialChain) {
    let node = PrivateKey::new();
    let chain = trusted
        .1
        .clone()
        .certify(&trusted.0, node.public_key(), author)
        .unwrap();
    (node, chain)
}
/// An issuer identity `parent` enrolls below it.
fn issuer(parent: &(PrivateKey, CredentialChain)) -> (PrivateKey, CredentialChain) {
    let key = PrivateKey::new();
    let link = rocky_credentials::testing::authority(&parent.0, &parent.1)
        .issue_issuer(
            key.public_key(),
            Some(rocky_credentials::AuthorRange::ALL),
            (NOW, 30 * DAY, 0),
        )
        .unwrap();
    let mut chain = parent.1.clone();
    chain.0.push(link);
    (key, chain)
}
/// A member identity `parent` enrolls, and its node's chain.
fn member(parent: &(PrivateKey, CredentialChain)) -> (PrivateKey, PrivateKey, CredentialChain) {
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let link = rocky_credentials::testing::authority(&parent.0, &parent.1)
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
        cut: Default::default(),
        recovery: None,
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
        rocky_credentials::testing::authority(&key, &grant)
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
    let by_a = rocky_credentials::testing::authority(&a.0, &a.1).revoke(Revocation {
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
    let by_a = set(rocky_credentials::testing::authority(&a.0, &a.1)
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
            .map(|origin| origin.identity)
    };
    assert_eq!(origin(&enrolled), Some(hijacked.0.public_key()));
    // The honest trusted node renews it: the origin stays.
    let renewed = rocky_credentials::testing::authority(&honest.0, &honest.1)
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
        rocky_credentials::testing::authority(&honest.0, &honest.1)
            .renew(&renewed.capability, &revoked, (NOW, 30 * DAY, 0))
            .is_err()
    );
    assert!(revoked.revokes_origin(&hijacked.0.public_key()));
    // A trusted identity's statement naming another cascades to nothing.
    let by_honest = set(rocky_credentials::testing::authority(&honest.0, &honest.1)
        .revoke(revocation(&honest.0, &hijacked.0))
        .unwrap());
    assert!(
        renewed_chain
            .verify(root.public_key(), node.public_key(), &by_honest, NOW)
            .is_ok()
    );
}

/// A recovery's statement ends every trusted grant valid before the
/// recovered identity's, the ones its store never knew included, with
/// what they first enrolled, renewed elsewhere or not. The recovered
/// identity, and trust the root grants afterwards, stay.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_recovery_ends_every_earlier_trusted_grant_and_what_it_enrolled() {
    let root = RootKey::generate();
    // Promoted after the backup the recovery used: no store it reaches
    // names it.
    let unknown = trusted(&root, AuthorRange::new(1, 1000).unwrap());
    let (user, node, enrolled) = member(&unknown);
    let below = issuer(&unknown);
    let (_, below_node, below_member) = member(&below);
    let recovered = trusted_at(&root, AuthorRange::recovery(7).unwrap(), NOW + 2 * DAY);
    // A survivor that renewed the device before it learned the recovery.
    let renewed = rocky_credentials::testing::authority(&recovered.0, &recovered.1)
        .renew(
            &enrolled.0[1].capability,
            &RevocationSet::new(),
            (NOW + 2 * DAY, 30 * DAY, 0),
        )
        .unwrap();
    let mut chain = recovered.1.clone();
    chain.0.push(renewed.clone());
    let renewed_chain = chain.certify(&user, node.public_key(), 100).unwrap();
    let (_, own_node, own) = member(&recovered);
    let readded = trusted_at(&root, AuthorRange::new(1, 1000).unwrap(), NOW + 3 * DAY);

    let set = recovery(&root, &recovered, 7);
    let at = NOW + 4 * DAY;
    let verify = |chain: &CredentialChain, node: &PrivateKey| {
        chain.verify(root.public_key(), node.public_key(), &set, at)
    };
    let (unknown_node, unknown_chain) = node_of(&unknown, 1);
    for (chain, node) in [
        (&unknown_chain, &unknown_node),
        (&enrolled, &node),
        (&below_member, &below_node),
        (&renewed_chain, &node),
    ] {
        assert!(verify(chain, node).is_err());
        assert!(set.revoking(chain).unwrap().is_some());
    }
    let (recovered_node, recovered_chain) = node_of(
        &recovered,
        rocky_credentials::RECOVERY_FIRST + 7 * rocky_credentials::RECOVERY_SLOT,
    );
    let (readded_node, readded_chain) = node_of(&readded, 1);
    for (chain, node) in [
        (&recovered_chain, &recovered_node),
        (&own, &own_node),
        (&readded_chain, &readded_node),
    ] {
        verify(chain, node).unwrap();
    }
    assert!(set.ends_trusted(&unknown.1.0[0].capability));
    assert!(!set.ends_trusted(&readded.1.0[0].capability));
    // No survivor renews what the ended identity enrolled once it holds
    // the statement.
    assert!(
        rocky_credentials::testing::authority(&recovered.0, &recovered.1)
            .renew(&renewed.capability, &set, (at, 30 * DAY, 0))
            .is_err()
    );
}

/// An origin carries its trusted grant's time, checked wherever the chain
/// shows that grant, so a lost node cannot date what it enrolls past a
/// recovery.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_origin_states_its_trusted_grants_time() {
    let root = RootKey::generate();
    let lost = trusted(&root, AuthorRange::new(1, 1000).unwrap());
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

/// Of two recoveries the later keeps its identity and ends the earlier,
/// whichever statement a store learns first; the earlier's statement ends
/// nothing the later holds.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn the_later_of_two_recoveries_ends_the_earlier() {
    let root = RootKey::generate();
    let first = trusted_at(&root, AuthorRange::recovery(3).unwrap(), NOW);
    let second = trusted_at(&root, AuthorRange::recovery(3).unwrap(), NOW + DAY);
    let (a, b) = (recovery(&root, &first, 3), recovery(&root, &second, 3));
    let ends = |set: &RevocationSet, identity: &(PrivateKey, CredentialChain)| {
        set.ends_trusted(&identity.1.0[0].capability)
    };
    assert!(!ends(&a, &first) && !ends(&a, &second));
    assert!(ends(&b, &first) && !ends(&b, &second));
    for (one, other) in [(&a, &b), (&b, &a)] {
        let mut merged = one.clone();
        merged
            .merge(other, root.public_key(), APP, NOW + DAY)
            .unwrap();
        assert!(ends(&merged, &first) && !ends(&merged, &second));
        // Both took slot 3: only the later's nodes hold its numbers.
        let author = rocky_credentials::RECOVERY_FIRST + 3 * rocky_credentials::RECOVERY_SLOT;
        for (recovered, kept) in [(&first, false), (&second, true)] {
            let (node, chain) = node_of(recovered, author);
            let verified = chain.verify(root.public_key(), node.public_key(), &merged, NOW + DAY);
            assert_eq!(verified.is_ok(), kept);
        }
        assert_eq!(
            merged.recovery().map(|r| r.keep),
            Some(second.0.public_key())
        );
        assert_eq!(merged.recoveries().len(), 2);
    }
    // Granted in the same second, the larger key is the later.
    let tie = trusted_at(&root, AuthorRange::recovery(4).unwrap(), NOW);
    let (low, high) = if tie.0.public_key() < first.0.public_key() {
        (&tie, &first)
    } else {
        (&first, &tie)
    };
    let set = recovery(&root, high, 4);
    assert!(ends(&set, low) && !ends(&set, high));
    // Only the root signs one.
    let mut statement = Revocation {
        app: APP,
        issuer: first.0.public_key(),
        time: NOW,
        nodes: Default::default(),
        identities: Default::default(),
        capabilities: Default::default(),
        cut: Default::default(),
        recovery: Some(RootRecovery {
            since: NOW + 10 * DAY,
            keep: first.0.public_key(),
            slot: 3,
        }),
    };
    assert!(
        rocky_credentials::testing::authority(&first.0, &first.1)
            .revoke(statement.clone())
            .is_err()
    );
    statement.issuer = root.public_key();
    // Its cut keeps chains of any identity it ends, never of the one it
    // keeps.
    let cut = |identity: &PrivateKey| rocky_credentials::Cut {
        identity: identity.public_key(),
        lane: PrivateKey::new().public_key(),
        statements: vec![rocky_credentials::Head {
            sequence: 1,
            id: [1; 32],
        }],
        acknowledgments: None,
    };
    for (cut, valid) in [(cut(&second.0), true), (cut(&first.0), false)] {
        let mut cutting = statement.clone();
        cutting.cut = vec![cut];
        assert_eq!(Authority::owner(&root).revoke(cutting).is_ok(), valid);
    }
    statement.recovery = statement.recovery.map(|r| RootRecovery {
        slot: rocky_credentials::RECOVERY_SLOTS,
        ..r
    });
    assert!(Authority::owner(&root).revoke(statement).is_err());
}
