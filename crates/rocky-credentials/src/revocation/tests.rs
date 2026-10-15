use super::*;
use rocky_core::AppId;

const APP: AppId = AppId([7; 16]);
const NOW: u64 = 2_000_000_000;

fn statement(issuer: &PrivateKey, time: u64, node: PublicKey) -> Revocation {
    Revocation {
        app: APP,
        issuer: issuer.public_key(),
        time,
        nodes: [crate::PeerId::of(node)].into(),
        users: [PrivateKey::new().public_key()].into(),
        capabilities: [[time as u8; 32]].into(),
    }
}
fn single(signed: &SignedRevocation) -> RevocationSet {
    RevocationSet::try_from(vec![signed.clone()]).unwrap()
}

#[test]
fn every_interleaving_of_merges_yields_one_digest() {
    let publisher = PrivateKey::new();
    let issuers = [PrivateKey::new(), PrivateKey::new()];
    let statements: Vec<SignedRevocation> = (0..5)
        .map(|index| {
            let issuer = &issuers[index % 2];
            SignedRevocation::sign(
                statement(
                    issuer,
                    NOW - index as u64 * DAY,
                    PrivateKey::new().public_key(),
                ),
                &publisher,
                issuer,
            )
            .unwrap()
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
    let publisher_key = publisher.public_key();
    let mut each = |order: &[usize]| {
        let mut set = RevocationSet::new();
        for index in order {
            set.merge(&single(&statements[*index]), publisher_key, APP, NOW)
                .unwrap();
        }
        // Merging a set again, or a subset, adds nothing.
        let again = set.clone();
        assert!(
            set.merge(&again, publisher_key, APP, NOW)
                .unwrap()
                .is_empty()
        );
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
        a.merge(&b_copy, publisher_key, APP, NOW).unwrap();
        b.merge(&a_copy, publisher_key, APP, NOW).unwrap();
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
    let publisher = PrivateKey::new();
    let issuer = PrivateKey::new();
    let sign = |time| {
        SignedRevocation::sign(
            statement(&issuer, time, PrivateKey::new().public_key()),
            &publisher,
            &issuer,
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
    let issuer = PrivateKey::new();
    let node = PrivateKey::new().public_key();
    let good = SignedRevocation::sign(statement(&issuer, NOW, node), &publisher, &issuer).unwrap();
    let mut set = single(&good);
    let before = set.clone();
    let mut forged = SignedRevocation::sign(
        statement(&issuer, NOW, PrivateKey::new().public_key()),
        &publisher,
        &issuer,
    )
    .unwrap();
    forged
        .statement
        .nodes
        .insert(crate::PeerId::of(PrivateKey::new().public_key()));
    let other_app = SignedRevocation::sign(
        Revocation {
            app: AppId([8; 16]),
            ..statement(&issuer, NOW, PrivateKey::new().public_key())
        },
        &publisher,
        &issuer,
    )
    .unwrap();
    let other_publisher = SignedRevocation::sign(
        statement(&issuer, NOW, PrivateKey::new().public_key()),
        &PrivateKey::new(),
        &issuer,
    )
    .unwrap();
    let mut wrong_issuer = SignedRevocation::sign(
        statement(&issuer, NOW, PrivateKey::new().public_key()),
        &publisher,
        &issuer,
    )
    .unwrap();
    wrong_issuer.issuer = PrivateKey::new().sign(b"other");
    let fresh = SignedRevocation::sign(
        statement(&issuer, NOW, PrivateKey::new().public_key()),
        &publisher,
        &issuer,
    )
    .unwrap();
    for bad in [forged, other_app, other_publisher, wrong_issuer] {
        // The good statement beside it is not learned either.
        let incoming = RevocationSet::try_from(vec![fresh.clone(), bad]).unwrap();
        assert!(
            set.merge(&incoming, publisher.public_key(), APP, NOW)
                .is_err()
        );
        assert_eq!(set, before);
    }
    // Over the size limit: refused when signed and when decoded.
    let mut huge = statement(&issuer, NOW, node);
    huge.users = (0..160).map(|_| PrivateKey::new().public_key()).collect();
    assert!(SignedRevocation::sign(huge, &publisher, &issuer).is_err());
    // An issuer can only co-sign with its own key, and a statement names something.
    assert!(SignedRevocation::sign(statement(&issuer, NOW, node), &publisher, &publisher).is_err());
    let empty = Revocation {
        nodes: BTreeSet::new(),
        users: BTreeSet::new(),
        capabilities: BTreeSet::new(),
        ..statement(&issuer, NOW, node)
    };
    assert!(SignedRevocation::sign(empty, &publisher, &issuer).is_err());
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
    let (a, b) = (PrivateKey::new(), PrivateKey::new());
    let revoke = |by: &PrivateKey, target: &PrivateKey| {
        SignedRevocation::sign(statement(by, NOW, target.public_key()), &publisher, by).unwrap()
    };
    let mut set = single(&revoke(&a, &b));
    set.merge(&single(&revoke(&b, &a)), publisher.public_key(), APP, NOW)
        .unwrap();
    assert!(set.revokes(&a.public_key()) && set.revokes(&b.public_key()));
    assert_eq!(set.len(), 2);
}

#[test]
fn signing_adds_a_bounded_overhead() {
    let (publisher, issuer) = (PrivateKey::new(), PrivateKey::new());
    let body = statement(&issuer, NOW, PrivateKey::new().public_key());
    let signed = SignedRevocation::sign(body.clone(), &publisher, &issuer).unwrap();
    let overhead = encode(&signed).unwrap().len() - encode(&body).unwrap().len();
    assert!(overhead <= SIGNATURE_OVERHEAD, "{overhead}");
}
