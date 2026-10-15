use super::*;
use crate::{Authority, PrivateKey};
use rocky_core::AppId;

const APP: AppId = AppId([7; 16]);
const NOW: u64 = 2_000_000_000;

/// An issuer identity of `publisher`: its key and issuer chain, valid for
/// the statement times these tests use.
fn issuer(publisher: &PrivateKey) -> (PrivateKey, CredentialChain) {
    let key = PrivateKey::new();
    let link = Authority::owner(publisher)
        .issue_issuer(key.public_key(), None, (NOW - 5 * DAY, 300 * DAY, DAY))
        .unwrap();
    (key, CredentialChain(vec![link]))
}
fn signed(statement: Revocation, (key, chain): &(PrivateKey, CredentialChain)) -> SignedRevocation {
    SignedRevocation::sign(statement, key, chain).unwrap()
}
fn statement(issuer: &PrivateKey, time: u64, node: PublicKey) -> Revocation {
    Revocation {
        app: APP,
        issuer: issuer.public_key(),
        time,
        nodes: [crate::PeerId::of(node)].into(),
        identities: [PrivateKey::new().public_key()].into(),
        capabilities: [[time as u8; 32]].into(),
    }
}
fn single(signed: &SignedRevocation) -> RevocationSet {
    RevocationSet::try_from(vec![signed.clone()]).unwrap()
}

#[test]
fn every_interleaving_of_merges_yields_one_digest() {
    let publisher = PrivateKey::new();
    let issuers = [issuer(&publisher), issuer(&publisher)];
    let statements: Vec<SignedRevocation> = (0..5)
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
    let mut digests = BTreeSet::new();
    let mut order: Vec<usize> = (0..statements.len()).collect();
    // Heap's algorithm: every order of single-statement merges.
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
        // Merging a set again, or a subset, adds nothing.
        let again = set.clone();
        assert!(set.merge(&again, anchor, APP, NOW).unwrap().is_empty());
        digests.insert(set.digest(NOW).unwrap());
    };
    let count = order.len();
    permutations(count, &mut order, &mut each);
    // Partial sets merged into each other in either direction.
    for split in 1..statements.len() {
        let (left, right) = statements.split_at(split);
        let mut a = RevocationSet::try_from(left.to_vec()).unwrap();
        let mut b = RevocationSet::try_from(right.to_vec()).unwrap();
        let (a_copy, b_copy) = (a.clone(), b.clone());
        a.merge(&b_copy, anchor, APP, NOW).unwrap();
        b.merge(&a_copy, anchor, APP, NOW).unwrap();
        assert_eq!(a, b);
        digests.insert(a.digest(NOW).unwrap());
    }
    assert_eq!(digests.len(), 1);
    assert_ne!(
        digests.first().copied(),
        Some(RevocationSet::new().digest(NOW).unwrap())
    );
}

#[test]
fn statements_past_the_cutoff_leave_the_digest_and_the_set() {
    // The owner signs: its statements need no chain valid at their time.
    let publisher = PrivateKey::new();
    let sign = |time| {
        SignedRevocation::sign(
            statement(&publisher, time, PrivateKey::new().public_key()),
            &publisher,
            &CredentialChain::default(),
        )
        .unwrap()
    };
    let fresh = sign(NOW);
    let old = sign(NOW - MAX_CHAIN_LIFETIME - LIFETIME_SKEW - DAY);
    assert!(!old.statement.live(NOW));
    // The same day-rounded cutoff on either side of the expiry instant.
    let expiry = fresh.statement.expiry_day() * DAY;
    assert!(fresh.statement.live(expiry - 1));
    assert!(!fresh.statement.live(expiry));
    assert!(expiry >= fresh.statement.time + MAX_CHAIN_LIFETIME + LIFETIME_SKEW);

    let mut with_old = single(&fresh);
    let held = with_old.clone();
    with_old.statements.insert(old.id().unwrap(), old.clone());
    assert_eq!(with_old.digest(NOW).unwrap(), held.digest(NOW).unwrap());
    assert_eq!(with_old.live(NOW), 1);
    // An expired statement is not learned, and a merge drops held ones.
    let mut set = single(&fresh);
    assert!(
        set.merge(&single(&old), publisher.public_key(), APP, NOW)
            .unwrap()
            .is_empty()
    );
    assert_eq!(set, held);
    let mut later = with_old.clone();
    later
        .merge(&RevocationSet::new(), publisher.public_key(), APP, NOW)
        .unwrap();
    assert_eq!(later.len(), 1);
    // Past its own cutoff the fresh one leaves too.
    assert_eq!(
        held.digest(expiry).unwrap(),
        RevocationSet::new().digest(expiry).unwrap()
    );
}

#[test]
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
    let other_app = signed(
        Revocation {
            app: AppId([8; 16]),
            ..statement(issuer, NOW, PrivateKey::new().public_key())
        },
        &issuing,
    );
    // Another domain's issuer, a member identity that holds no issuer
    // authority and an issuer chain not valid at the statement's time.
    let other_publisher = signed(
        statement(issuer, NOW, PrivateKey::new().public_key()),
        &(issuer.clone(), self::issuer(&PrivateKey::new()).1),
    );
    let member = {
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
                (2, None),
                (NOW, DAY, 0),
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
        member,
        expired,
        owner_revoked,
    ] {
        // The good statement beside it is not learned either.
        let incoming = RevocationSet::try_from(vec![fresh.clone(), bad]).unwrap();
        assert!(
            set.merge(&incoming, publisher.public_key(), APP, NOW)
                .is_err()
        );
        assert_eq!(set, before);
    }
    // Over the size limit: refused when signed and when decoded.
    let mut huge = statement(issuer, NOW, node);
    huge.identities = (0..300).map(|_| PrivateKey::new().public_key()).collect();
    assert!(SignedRevocation::sign(huge, issuer, &issuing.1).is_err());
    // An identity signs only statements naming it, which name something.
    assert!(SignedRevocation::sign(statement(issuer, NOW, node), &publisher, &issuing.1).is_err());
    let empty = Revocation {
        nodes: BTreeSet::new(),
        identities: BTreeSet::new(),
        capabilities: BTreeSet::new(),
        ..statement(issuer, NOW, node)
    };
    assert!(SignedRevocation::sign(empty, issuer, &issuing.1).is_err());
    // The encoding is canonical: a set in another order is refused.
    let pair = RevocationSet::try_from(vec![good.clone(), fresh.clone()]).unwrap();
    let mut reversed: Vec<SignedRevocation> = pair.clone().into();
    reversed.reverse();
    assert!(RevocationSet::from_bytes(&encode(&reversed).unwrap()).is_err());
    assert_eq!(
        RevocationSet::from_bytes(&pair.to_bytes().unwrap()).unwrap(),
        pair
    );
}

#[test]
fn a_revoked_issuers_statement_counts_and_two_issuers_revoke_each_other() {
    let publisher = PrivateKey::new();
    let (a, b) = (issuer(&publisher), issuer(&publisher));
    let revoke = |by: &(PrivateKey, CredentialChain), target: &PrivateKey| {
        let mut body = statement(&by.0, NOW, PrivateKey::new().public_key());
        body.identities = [target.public_key()].into();
        signed(body, by)
    };
    let mut set = single(&revoke(&a, &b.0));
    set.merge(&single(&revoke(&b, &a.0)), publisher.public_key(), APP, NOW)
        .unwrap();
    assert!(set.revokes(&a.0.public_key()) && set.revokes(&b.0.public_key()));
    assert_eq!(set.len(), 2);
    // Each issuer's chain is ended: neither signs a current statement.
    assert!(
        a.1.issuer(publisher.public_key(), Some((&set, NOW)))
            .is_err()
    );
    assert!(
        b.1.issuer(publisher.public_key(), Some((&set, NOW)))
            .is_err()
    );
}

#[test]
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
