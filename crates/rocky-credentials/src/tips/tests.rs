use super::*;
use crate::{AuthorRange, Authority, PrivateKey, Revocation};
use rocky_core::{AppId, DomainId, MeshId};
use std::cell::Cell;

thread_local! {
    static IDS: Cell<usize> = const { Cell::new(0) };
}
/// Counts a statement ID computed on this thread.
pub(crate) fn count_id() {
    IDS.with(|ids| ids.set(ids.get() + 1));
}
fn ids_computed() -> usize {
    IDS.with(Cell::get)
}

/// `count` chained moves of `signer`'s lane, each after the last.
fn chain(
    graph: &mut TipGraph,
    owner: &PrivateKey,
    signer: &PrivateKey,
    count: usize,
) -> Vec<SignedBookmarkMove> {
    let credentials = CredentialChain(vec![
        Authority::owner(owner)
            .issue_trusted(
                signer.public_key(),
                graph.scope,
                AuthorRange::new(5, 5).unwrap(),
                100,
            )
            .unwrap(),
    ]);
    let node = crate::testing::signer(signer, &credentials);
    let mut link = ChainLink::first(node.signing_node().unwrap());
    let mut moves = Vec::new();
    for n in 0..count {
        let signed = SignedBookmarkMove::endorse_linked(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope: graph.scope,
                name: format!("b{n}"),
                from: None,
                to: Some(graph.genesis()),
            },
            node.clone(),
            &credentials,
            Some(link),
        )
        .unwrap();
        link = link.next(signed.id().unwrap());
        graph.moves.push(signed.clone());
        moves.push(signed);
    }
    moves
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn pruning_judges_only_the_lanes_a_cut_names_and_what_arrived_since() {
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let mut graph = TipGraph::new(scope, [4; 32]);
    let (owner, revoked, other) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let cut = chain(&mut graph, &owner, &revoked, 3);
    chain(&mut graph, &owner, &other, 50);
    let statement = Revocation {
        app: scope.app,
        issuer: owner.public_key(),
        time: 200,
        nodes: Default::default(),
        identities: [revoked.public_key()].into(),
        capabilities: Default::default(),
        cut: vec![Cut {
            identity: revoked.public_key(),
            lane: cut[0].endorsement().node().unwrap(),
            statements: vec![Head {
                sequence: 0,
                id: cut[0].id().unwrap(),
            }],
            acknowledgments: None,
        }],
        recovery: None,
    };
    let revocations =
        RevocationSet::try_from(vec![Authority::owner(&owner).revoke(statement).unwrap()]).unwrap();
    let past: BTreeSet<Commitment> = cut[1..].iter().map(|m| m.id().unwrap()).collect();
    assert_eq!(
        graph.prunable(&revocations, &BTreeSet::new()).unwrap(),
        past
    );

    // Warm, only the lists' last items are hashed to see they still hold.
    let before = ids_computed();
    assert_eq!(
        graph.prunable(&revocations, &BTreeSet::new()).unwrap(),
        past
    );
    let warm = ids_computed() - before;
    assert!(warm <= 6, "{warm} IDs computed");

    // A statement admitted since is indexed once.
    let late = chain(&mut graph, &owner, &PrivateKey::new(), 1);
    let before = ids_computed();
    assert_eq!(
        graph.prunable(&revocations, &BTreeSet::new()).unwrap(),
        past
    );
    let admitted = ids_computed() - before;
    assert!(admitted <= 8, "{admitted} IDs computed");
    assert!(!past.contains(&late[0].id().unwrap()));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_trusted_identity_keeps_both_lanes_through_unlink_and_root_cuts() {
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let root = PrivateKey::new();
    let identity = PrivateKey::new();
    let nodes = [PrivateKey::new(), PrivateKey::new()];
    let grant = CredentialChain(vec![
        Authority::owner(&root)
            .issue_trusted(
                identity.public_key(),
                scope,
                AuthorRange::new(10, 41).unwrap(),
                100,
            )
            .unwrap(),
    ]);
    let first = grant
        .clone()
        .certify(&identity, nodes[0].public_key(), 10)
        .unwrap();
    let second = grant
        .certify_member(
            &identity,
            nodes[1].public_key(),
            11,
            1,
            Some(first.certificate().unwrap().id().unwrap()),
        )
        .unwrap();
    let chains = [first, second];
    let mut graph = TipGraph::new(scope, [4; 32]);
    let append = |graph: &mut TipGraph, lane: usize, n: usize| {
        let signed = SignedBookmarkMove::endorse_linked(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope,
                name: format!("lane{lane}-{n}"),
                from: None,
                to: Some(graph.genesis()),
            },
            crate::Signer::node(&nodes[lane], &chains[lane]).unwrap(),
            &chains[lane].capabilities(),
            Some(
                graph
                    .next_link(&identity.public_key(), &nodes[lane].public_key())
                    .unwrap(),
            ),
        )
        .unwrap();
        graph.moves.push(signed.clone());
        signed.id().unwrap()
    };
    let kept: BTreeSet<_> = (0..6).map(|n| append(&mut graph, n % 2, n)).collect();
    let unlink_statement = Revocation {
        app: scope.app,
        issuer: identity.public_key(),
        time: 200,
        nodes: [crate::PeerId::of(nodes[0].public_key())].into(),
        identities: Default::default(),
        capabilities: Default::default(),
        cut: vec![
            graph
                .heads(&identity.public_key(), &nodes[0].public_key())
                .unwrap(),
        ],
        recovery: None,
    };
    let unlink =
        crate::SignedRevocation::sign(unlink_statement.clone(), &identity, &chains[0]).unwrap();
    unlink.verify(root.public_key(), scope.app).unwrap();
    // An identity never cuts a sibling it did not unlink, or a different identity's lane.
    for foreign_identity in [false, true] {
        let mut forged = unlink_statement.clone();
        if foreign_identity {
            forged.cut[0].identity = root.public_key();
        } else {
            forged.cut[0].lane = nodes[1].public_key();
        }
        assert!(crate::SignedRevocation::sign(forged, &identity, &chains[0]).is_err());
    }
    let mut revocations = RevocationSet::try_from(vec![unlink]).unwrap();
    let after_unlink = append(&mut graph, 0, 7);
    let repeated_unlink = crate::SignedRevocation::sign(
        Revocation {
            time: 201,
            cut: vec![
                graph
                    .heads(&identity.public_key(), &nodes[0].public_key())
                    .unwrap(),
            ],
            ..unlink_statement
        },
        &identity,
        &chains[0],
    )
    .unwrap();
    let repeated = RevocationSet::try_from(vec![repeated_unlink]).unwrap();
    let before_retry = revocations.clone();
    revocations
        .merge(&repeated, root.public_key(), scope.app, 201)
        .unwrap();
    let mut reverse_retry = repeated;
    reverse_retry
        .merge(&before_retry, root.public_key(), scope.app, 201)
        .unwrap();
    assert_eq!(
        reverse_retry.cut(&identity.public_key(), &nodes[0].public_key()),
        revocations.cut(&identity.public_key(), &nodes[0].public_key())
    );
    let survivor = append(&mut graph, 1, 8);
    let root_cut = graph
        .proposed_cut(&identity.public_key(), &[nodes[1].public_key()].into())
        .unwrap()
        .0;
    let root_revoke = Authority::owner(&root)
        .revoke(Revocation {
            app: scope.app,
            issuer: root.public_key(),
            time: 300,
            nodes: Default::default(),
            identities: [identity.public_key()].into(),
            capabilities: Default::default(),
            cut: root_cut,
            recovery: None,
        })
        .unwrap();
    revocations
        .merge(
            &RevocationSet::try_from(vec![root_revoke]).unwrap(),
            root.public_key(),
            scope.app,
            300,
        )
        .unwrap();
    let after_root = append(&mut graph, 1, 9);
    let later_unlink = crate::SignedRevocation::sign(
        Revocation {
            app: scope.app,
            issuer: identity.public_key(),
            time: 400,
            nodes: [crate::PeerId::of(nodes[1].public_key())].into(),
            identities: Default::default(),
            capabilities: Default::default(),
            cut: vec![
                graph
                    .heads(&identity.public_key(), &nodes[1].public_key())
                    .unwrap(),
            ],
            recovery: None,
        },
        &identity,
        &chains[1],
    )
    .unwrap();
    let later_unlink = RevocationSet::try_from(vec![later_unlink]).unwrap();
    let before = revocations.clone();
    revocations
        .merge(&later_unlink, root.public_key(), scope.app, 400)
        .unwrap();
    let mut reverse = later_unlink;
    reverse
        .merge(&before, root.public_key(), scope.app, 400)
        .unwrap();
    assert_eq!(
        reverse.cut(&identity.public_key(), &nodes[1].public_key()),
        revocations.cut(&identity.public_key(), &nodes[1].public_key())
    );
    assert_eq!(
        graph.prunable(&revocations, &BTreeSet::new()).unwrap(),
        [after_unlink, after_root].into()
    );
    graph.exclude_revoked(&revocations).unwrap();
    assert!(kept.iter().all(|id| !graph.revoked.contains(id)));
    assert!(!graph.revoked.contains(&survivor));
    assert_eq!(revocations.pruned(u64::MAX).cut_lanes().count(), 2);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn implicit_empty_unlink_cut_matches_an_explicit_empty_lane() {
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let root = PrivateKey::new();
    let identity = PrivateKey::new();
    let node = PrivateKey::new();
    let sibling = PrivateKey::new();
    let chain = CredentialChain(vec![
        Authority::owner(&root)
            .issue_trusted(
                identity.public_key(),
                scope,
                AuthorRange::new(10, 41).unwrap(),
                100,
            )
            .unwrap(),
    ])
    .certify(&identity, node.public_key(), 10)
    .unwrap();
    let graph = TipGraph::new(scope, [4; 32]);
    let statement = Revocation {
        app: scope.app,
        issuer: identity.public_key(),
        time: 200,
        nodes: [crate::PeerId::of(node.public_key())].into(),
        identities: Default::default(),
        capabilities: Default::default(),
        cut: vec![],
        recovery: None,
    };
    let implicit = crate::SignedRevocation::sign(statement.clone(), &identity, &chain).unwrap();
    let explicit = crate::SignedRevocation::sign(
        Revocation {
            cut: vec![
                graph
                    .heads(&identity.public_key(), &node.public_key())
                    .unwrap(),
            ],
            ..statement
        },
        &identity,
        &chain,
    )
    .unwrap();
    implicit.verify(root.public_key(), scope.app).unwrap();
    explicit.verify(root.public_key(), scope.app).unwrap();
    assert!(implicit.to_bytes().unwrap().len() < explicit.to_bytes().unwrap().len());
    let implicit = RevocationSet::try_from(vec![implicit]).unwrap();
    let explicit = RevocationSet::try_from(vec![explicit]).unwrap();
    let cut = implicit.cut(&identity.public_key(), &node.public_key());
    assert!(cut.is_some());
    assert_eq!(
        cut,
        explicit.cut(&identity.public_key(), &node.public_key())
    );
    for set in [&implicit, &explicit] {
        assert!(set.identity_unlinks(identity.public_key(), crate::PeerId::of(node.public_key())));
        assert!(!set.identity_unlinks(
            identity.public_key(),
            crate::PeerId::of(sibling.public_key())
        ));
    }
}
