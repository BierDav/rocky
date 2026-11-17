use super::*;
use crate::{Authority, PrivateKey};
use rocky_core::AppId;

const APP: AppId = AppId([7; 16]);
const NOW: u64 = 2_000_000_000;

thread_local! {
    static DERIVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// Counts a value derived from a whole set on this thread.
pub(super) fn count_derivation() {
    DERIVATIONS.with(|count| count.set(count.get() + 1));
}
fn derivations() -> usize {
    DERIVATIONS.with(std::cell::Cell::get)
}

/// An issuer identity of `publisher`: its key and issuer chain, valid for
/// the statement times these tests use.
fn issuer(publisher: &PrivateKey) -> (PrivateKey, CredentialChain) {
    issuer_at(publisher, NOW)
}
/// An issuer whose chain is valid for 300 days around `time`.
fn issuer_at(publisher: &PrivateKey, time: u64) -> (PrivateKey, CredentialChain) {
    let key = PrivateKey::new();
    let link = Authority::owner_in(
        publisher,
        crate::DatabaseContext {
            publisher: publisher.public_key(),
            scope: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: APP,
                domain: rocky_core::DomainId([3; 16]),
            },
        },
    )
    .unwrap()
    .issue_issuer(
        key.public_key(),
        crate::Validity {
            issued_at: time - 5 * DAY,
            lifetime: std::time::Duration::from_secs(300 * DAY),
            clock_skew: std::time::Duration::from_secs(DAY),
        },
    )
    .unwrap();
    (key, CredentialChain(vec![link]))
}
fn signed(statement: Revocation, (key, chain): &(PrivateKey, CredentialChain)) -> SignedRevocation {
    SignedRevocation::sign(statement, crate::testing::signer(key, chain), chain).unwrap()
}
fn statement(issuer: &PrivateKey, time: u64, node: PublicKey) -> Revocation {
    Revocation {
        app: APP,
        issuer: issuer.public_key(),
        time,
        nodes: BTreeSet::new(),
        identities: [node, PrivateKey::new().public_key()].into(),
        capabilities: [[time as u8; 32]].into(),
        cut: Default::default(),
        mode: crate::RevocationMode::Lost,
        recovery: None,
    }
}
fn single(signed: &SignedRevocation) -> RevocationSet {
    RevocationSet::try_from(vec![signed.clone()]).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn every_interleaving_of_merges_yields_one_digest() {
    let publisher = PrivateKey::new();
    let issuers = [issuer(&publisher), issuer(&publisher)];
    let mut statements: Vec<SignedRevocation> = (0..3)
        .map(|index| {
            let issuer = &issuers[index % 2];
            signed(
                statement(
                    &issuer.0,
                    NOW - index as u64 * DAY,
                    PrivateKey::new().public_key(),
                ),
                issuer,
            )
        })
        .collect();
    let target = PrivateKey::new().public_key();
    let conflicting_root_decisions = [
        (RevocationMode::Lost, NOW),
        (RevocationMode::Compromised, NOW - DAY),
    ];
    for (mode, time) in conflicting_root_decisions {
        let body = Revocation {
            identities: [target].into(),
            mode,
            ..statement(&publisher, time, target)
        };
        statements
            .push(SignedRevocation::sign(body, &publisher, &CredentialChain::default()).unwrap());
    }
    let mut digests = BTreeSet::new();
    let mut order: Vec<usize> = (0..statements.len()).collect();
    /// Heap's algorithm: every order of single-statement merges.
    fn permutations(k: usize, order: &mut Vec<usize>, each: &mut dyn FnMut(&[usize])) {
        if k <= 1 {
            each(order);
            return;
        }
        for i in 0..k {
            permutations(k - 1, order, each);
            let swap = if k.is_multiple_of(2) { i } else { 0 };
            order.swap(swap, k - 1);
        }
    }
    let anchor = publisher.public_key();
    let mut each = |order: &[usize]| {
        let mut set = RevocationSet::new();
        for index in order {
            set.merge(&single(&statements[*index]), anchor, APP, NOW)
                .unwrap();
        }
        let again = set.clone();
        assert!(
            set.merge(&again, anchor, APP, NOW).unwrap().is_empty(),
            "merging a set again adds nothing"
        );
        assert!(set.revokes(&target));
        assert!(set.revokes_origin(&target));
        let decoded = RevocationSet::from_bytes(&set.to_bytes().unwrap()).unwrap();
        assert_eq!(decoded, set);
        assert!(decoded.revokes_origin(&target));
        digests.insert(set.digest(NOW).unwrap());
    };
    let count = order.len();
    permutations(count, &mut order, &mut each);
    for split in 1..statements.len() {
        let (left, right) = statements.split_at(split);
        let mut a = RevocationSet::try_from(left.to_vec()).unwrap();
        let mut b = RevocationSet::try_from(right.to_vec()).unwrap();
        let (a_copy, b_copy) = (a.clone(), b.clone());
        a.merge(&b_copy, anchor, APP, NOW).unwrap();
        b.merge(&a_copy, anchor, APP, NOW).unwrap();
        assert_eq!(
            a, b,
            "partial sets merge into each other in either direction"
        );
        digests.insert(a.digest(NOW).unwrap());
    }
    assert_eq!(digests.len(), 1);
    assert_ne!(
        digests.first().copied(),
        Some(RevocationSet::new().digest(NOW).unwrap())
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn the_mode_is_required_signed_and_only_the_root_may_cascade() {
    let publisher = PrivateKey::new();
    let target = PrivateKey::new().public_key();
    let body = statement(&publisher, NOW, target);
    let lost =
        SignedRevocation::sign(body.clone(), &publisher, &CredentialChain::default()).unwrap();
    let mut tampered = lost.clone();
    tampered.statement.mode = RevocationMode::Compromised;
    assert_ne!(lost.id().unwrap(), tampered.id().unwrap());
    assert!(tampered.verify(publisher.public_key(), APP).is_err());
    let compromised =
        SignedRevocation::sign(tampered.statement, &publisher, &CredentialChain::default())
            .unwrap();
    compromised.verify(publisher.public_key(), APP).unwrap();

    let recovery = Revocation {
        recovery: Some(RootDecision::Grants(RootRecovery {
            since: NOW,
            keep: target,
        })),
        ..body
    };
    assert!(
        SignedRevocation::sign(recovery.clone(), &publisher, &CredentialChain::default()).is_err()
    );
    SignedRevocation::sign(
        Revocation {
            mode: RevocationMode::Compromised,
            ..recovery
        },
        &publisher,
        &CredentialChain::default(),
    )
    .unwrap()
    .verify(publisher.public_key(), APP)
    .unwrap();

    let issuing = issuer(&publisher);
    let mut body = statement(&issuing.0, NOW, target);
    body.mode = RevocationMode::Compromised;
    assert!(
        SignedRevocation::sign(
            body,
            crate::testing::signer(&issuing.0, &issuing.1),
            &issuing.1
        )
        .is_err()
    );
    let mut forged = signed(statement(&issuing.0, NOW, target), &issuing);
    forged.statement.mode = RevocationMode::Compromised;
    assert!(forged.verify(publisher.public_key(), APP).is_err());

    let mut old = serde_json::to_value(&lost).unwrap();
    old["statement"].as_object_mut().unwrap().remove("mode");
    assert!(serde_json::from_value::<SignedRevocation>(old).is_err());
    let current = single(&lost);
    let mut old = ReplicatedAuthority::from(current.clone());
    old.format = 3;
    assert!(RevocationSet::from_bytes(&encode(&old).unwrap()).is_err());
    assert_eq!(
        RevocationSet::from_bytes(&current.to_bytes().unwrap()).unwrap(),
        current
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn statements_past_the_cutoff_leave_the_digest_and_the_set() {
    let publisher = PrivateKey::new();
    let sign = |time| {
        let issuing = issuer_at(&publisher, time);
        SignedRevocation::sign(
            statement(&issuing.0, time, PrivateKey::new().public_key()),
            crate::testing::signer(&issuing.0, &issuing.1),
            &issuing.1,
        )
        .unwrap()
    };
    let fresh = sign(NOW);
    let old = sign(NOW - MAX_CHAIN_LIFETIME - LIFETIME_SKEW - DAY);
    assert!(!old.live(NOW));
    let expiry = fresh.expiry_day() * DAY;
    assert!(
        fresh.live(expiry - 1),
        "the same day-rounded cutoff applies on either side of the expiry instant"
    );
    assert!(!fresh.live(expiry));
    assert!(expiry >= fresh.statement.time + MAX_CHAIN_LIFETIME + LIFETIME_SKEW);

    let mut with_old = single(&fresh);
    let held = with_old.clone();
    with_old.statements.insert(old.id().unwrap(), old.clone());
    assert_eq!(with_old.digest(NOW).unwrap(), held.digest(NOW).unwrap());
    assert_eq!(with_old.live(NOW), 1);
    let mut set = single(&fresh);
    assert!(
        set.merge(&single(&old), publisher.public_key(), APP, NOW)
            .unwrap()
            .is_empty(),
        "an expired statement is not learned"
    );
    assert_eq!(set, held);
    let mut later = with_old.clone();
    later
        .merge(&RevocationSet::new(), publisher.public_key(), APP, NOW)
        .unwrap();
    assert_eq!(later.len(), 1);
    assert_eq!(
        held.digest(expiry).unwrap(),
        RevocationSet::new().digest(expiry).unwrap(),
        "past its own cutoff the fresh statement leaves too"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_bad_statement_is_refused_and_changes_nothing() {
    let publisher = PrivateKey::new();
    let issuing = issuer(&publisher);
    let issuer = &issuing.0;
    let node = PrivateKey::new().public_key();
    let good = signed(statement(issuer, NOW, node), &issuing);
    let mut set = single(&good);
    let before = set.clone();
    let mut forged = signed(
        statement(issuer, NOW, PrivateKey::new().public_key()),
        &issuing,
    );
    forged
        .statement
        .nodes
        .insert(crate::PeerId::of(PrivateKey::new().public_key()));
    let mut other_app = good.clone();
    other_app.statement.app = AppId([8; 16]);
    let other_publisher = signed(
        statement(issuer, NOW, PrivateKey::new().public_key()),
        &(issuer.clone(), self::issuer(&PrivateKey::new()).1),
    );
    let member_without_issuer_authority = {
        let key = PrivateKey::new();
        let link = Authority::owner(&publisher)
            .issue_terms(
                key.public_key(),
                vec![rocky_core::ResourceGrant {
                    scope: rocky_core::ResourceScope {
                        domain: rocky_core::Scope {
                            mesh: rocky_core::MeshId([0; 16]),
                            app: APP,
                            domain: rocky_core::DomainId([0; 16]),
                        },
                        table: [1; 32],
                        encryption: [2; 32],
                    },
                    rows: rocky_core::RowSelection::All,
                    rights: [rocky_core::Right::Read].into(),
                    fields: rocky_core::FieldSelection::Only(BTreeSet::new()),
                }],
                crate::DelegationTerms { delegations: 2 },
                crate::Validity {
                    issued_at: NOW,
                    lifetime: std::time::Duration::from_secs(DAY),
                    clock_skew: std::time::Duration::from_secs(0),
                },
            )
            .unwrap();
        signed(
            statement(&key, NOW, PrivateKey::new().public_key()),
            &(key, CredentialChain(vec![link])),
        )
    };
    let expired = signed(
        statement(issuer, NOW + 400 * DAY, PrivateKey::new().public_key()),
        &issuing,
    );
    let mut owner_revoked = statement(issuer, NOW, node);
    owner_revoked.identities.insert(publisher.public_key());
    let owner_revoked = signed(owner_revoked, &issuing);
    let fresh = signed(
        statement(issuer, NOW, PrivateKey::new().public_key()),
        &issuing,
    );
    for bad in [
        forged,
        other_app,
        other_publisher,
        member_without_issuer_authority,
        expired,
        owner_revoked,
    ] {
        if let Ok(incoming) = RevocationSet::try_from(vec![fresh.clone(), bad]) {
            assert!(
                set.merge(&incoming, publisher.public_key(), APP, NOW)
                    .is_err(),
                "a bad statement keeps the good one beside it from being learned"
            );
        }
        assert_eq!(set, before);
    }
    let mut huge = statement(issuer, NOW, node);
    huge.identities = (0..300).map(|_| PrivateKey::new().public_key()).collect();
    assert!(
        SignedRevocation::sign(huge, crate::testing::signer(issuer, &issuing.1), &issuing.1)
            .is_err(),
        "a statement over the size limit is refused when signed"
    );
    assert!(
        SignedRevocation::sign(statement(issuer, NOW, node), &publisher, &issuing.1).is_err(),
        "an identity signs only statements naming it"
    );
    let empty = Revocation {
        nodes: BTreeSet::new(),
        identities: BTreeSet::new(),
        capabilities: BTreeSet::new(),
        ..statement(issuer, NOW, node)
    };
    assert!(
        SignedRevocation::sign(
            empty,
            crate::testing::signer(issuer, &issuing.1),
            &issuing.1
        )
        .is_err()
    );
    let pair = RevocationSet::try_from(vec![good.clone(), fresh.clone()]).unwrap();
    let mut reversed: Vec<SignedRevocation> = pair.clone().into();
    reversed.reverse();
    assert!(
        RevocationSet::from_bytes(&encode(&reversed).unwrap()).is_err(),
        "the encoding is canonical; a set in another order is refused"
    );
    assert_eq!(
        RevocationSet::from_bytes(&pair.to_bytes().unwrap()).unwrap(),
        pair
    );
}

/// Two identities the root signed for cannot revoke each other: only a
/// root statement ends a root-signed link. Two issuers below one do.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn only_the_root_ends_its_links_and_issuers_below_revoke_each_other() {
    let publisher = PrivateKey::new();
    let revoke = |by: &(PrivateKey, CredentialChain), target: &PrivateKey| {
        let mut body = statement(&by.0, NOW, PrivateKey::new().public_key());
        body.identities = [target.public_key()].into();
        signed(body, by)
    };
    let (a, b) = (issuer(&publisher), issuer(&publisher));
    let mut set = single(&revoke(&a, &b.0));
    set.merge(&single(&revoke(&b, &a.0)), publisher.public_key(), APP, NOW)
        .unwrap();
    assert!(set.revokes(&a.0.public_key()) && set.revokes(&b.0.public_key()));
    assert_eq!(set.len(), 2);
    assert!(
        a.1.issuer(publisher.public_key(), Some((&set, NOW)))
            .is_ok()
    );
    assert!(
        b.1.issuer(publisher.public_key(), Some((&set, NOW)))
            .is_ok()
    );
    let below = |parent: &(PrivateKey, CredentialChain)| {
        let key = PrivateKey::new();
        let link = crate::testing::authority(&parent.0, &parent.1)
            .issue_issuer(
                key.public_key(),
                crate::Validity {
                    issued_at: NOW - 5 * DAY,
                    lifetime: std::time::Duration::from_secs(300 * DAY),
                    clock_skew: std::time::Duration::from_secs(DAY),
                },
            )
            .unwrap();
        let mut chain = parent.1.clone();
        chain.0.push(link);
        (key, chain)
    };
    let (c, d) = (below(&a), below(&a));
    let mut set = single(&revoke(&c, &d.0));
    set.merge(&single(&revoke(&d, &c.0)), publisher.public_key(), APP, NOW)
        .unwrap();
    assert!(
        c.1.issuer(publisher.public_key(), Some((&set, NOW)))
            .is_err(),
        "below a, two issuers end each other's chains"
    );
    assert!(
        d.1.issuer(publisher.public_key(), Some((&set, NOW)))
            .is_err()
    );
    let root = SignedRevocation::sign(
        Revocation {
            identities: [a.0.public_key()].into(),
            ..statement(&publisher, NOW, PrivateKey::new().public_key())
        },
        &publisher,
        &CredentialChain::default(),
    )
    .unwrap();
    let set = single(&root);
    assert!(
        a.1.issuer(publisher.public_key(), Some((&set, NOW)))
            .is_err(),
        "the root ends a, and with it every chain below"
    );
    assert!(
        c.1.issuer(publisher.public_key(), Some((&set, NOW)))
            .is_err()
    );
    assert!(
        b.1.issuer(publisher.public_key(), Some((&set, NOW)))
            .is_ok()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn signing_adds_a_bounded_overhead() {
    let publisher = PrivateKey::new();
    let issuing = issuer(&publisher);
    let body = statement(&issuing.0, NOW, PrivateKey::new().public_key());
    let signed = signed(body.clone(), &issuing);
    let overhead = encode(&signed).unwrap().len()
        - encode(&body).unwrap().len()
        - encode(&issuing.1).unwrap().len();
    assert!(overhead <= SIGNATURE_OVERHEAD, "{overhead}");
}

/// A root statement never leaves the set: it may end a trusted grant,
/// which never expires.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_root_statement_is_permanent() {
    let publisher = PrivateKey::new();
    let old = NOW - 10 * MAX_CHAIN_LIFETIME;
    let root = SignedRevocation::sign(
        statement(&publisher, old, PrivateKey::new().public_key()),
        &publisher,
        &CredentialChain::default(),
    )
    .unwrap();
    assert!(root.root() && root.live(NOW) && root.live(u64::MAX - 1));
    assert!(!root.statement.live(NOW));
    let mut set = RevocationSet::new();
    assert_eq!(
        set.merge(&single(&root), publisher.public_key(), APP, NOW)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(set.pruned(u64::MAX - 1).len(), 1);
}

/// An identity's membership certificates of five nodes, then a sixth that
/// forks its links at sequence 1, with the root that trusts the identity.
fn forked_links() -> (PrivateKey, Vec<SignedCapability>) {
    let root = PrivateKey::new();
    let identity = PrivateKey::new();
    let scope = rocky_core::Scope {
        mesh: rocky_core::MeshId([1; 16]),
        app: APP,
        domain: rocky_core::DomainId([3; 16]),
    };
    let grant = CredentialChain(vec![
        Authority::owner(&root)
            .issue_trusted(identity.public_key(), scope, NOW)
            .unwrap(),
    ]);
    let certify = |sequence: u32, previous: Option<Commitment>| {
        grant
            .clone()
            .certify_member(
                &identity,
                PrivateKey::new().public_key(),
                u64::from(sequence),
                previous,
            )
            .unwrap()
            .certificate()
            .unwrap()
            .clone()
    };
    let mut certificates = vec![certify(0, None)];
    for sequence in 1..5 {
        let previous = certificates.last().unwrap().id().unwrap();
        certificates.push(certify(sequence, Some(previous)));
    }
    let fork_at_sequence_one = certify(1, Some(certificates[0].id().unwrap()));
    certificates.push(fork_at_sequence_one);
    (root, certificates)
}

/// Certificates linked one by one, merged as one batch or decoded hold the
/// same evidence; a fork among them ends nodes in each.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn batched_links_hold_what_single_links_hold() {
    let (root, certificates) = forked_links();
    let mut single = RevocationSet::new();
    for certificate in &certificates {
        assert!(single.link(certificate.clone()).unwrap());
    }
    assert!(!single.link(certificates[0].clone()).unwrap());
    assert!(single.has_revocations());
    let mut merged = RevocationSet::new();
    merged.merge(&single, root.public_key(), APP, NOW).unwrap();
    let decoded = RevocationSet::from_bytes(&single.to_bytes().unwrap()).unwrap();
    for set in [&merged, &decoded] {
        assert_eq!(set, &single);
        assert!(set.has_revocations());
        assert_eq!(set.digest(NOW).unwrap(), single.digest(NOW).unwrap());
    }
    assert_ne!(
        single.digest(NOW).unwrap(),
        RevocationSet::new().digest(NOW).unwrap()
    );
}

/// Judging every certificate's node, or keying a cache by the set, derives
/// the fork and the fingerprint once; a change derives them again.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn whole_set_derivations_are_computed_once_per_change() {
    let (_, certificates) = forked_links();
    let mut set = RevocationSet::new();
    for certificate in &certificates[..5] {
        set.link(certificate.clone()).unwrap();
    }
    let judge = |set: &RevocationSet| {
        certificates
            .iter()
            .filter(|certificate| {
                set.ends_node_of(certificate.issuer, certificate.capability.subject, false)
                    || set.ends_at(certificate, false).unwrap()
                    || set.revokes(&certificate.capability.subject)
            })
            .count()
    };
    let before = derivations();
    assert_eq!(judge(&set), 0);
    let fingerprint = set.fingerprint().unwrap();
    for _ in 0..10 {
        assert_eq!(set.fingerprint().unwrap(), fingerprint);
    }
    assert_eq!(set.clone().fingerprint().unwrap(), fingerprint);
    assert_eq!(derivations() - before, 2);

    set.link(certificates[5].clone()).unwrap();
    let before = derivations();
    assert!(
        judge(&set) > 0,
        "the fork ends the nodes linked past its place"
    );
    assert_ne!(set.fingerprint().unwrap(), fingerprint);
    assert_eq!(derivations() - before, 2);
}
