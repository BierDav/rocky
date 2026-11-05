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
    let mut link = ChainLink::first(signer.public_key());
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
            signer,
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
            lane: revoked.public_key(),
            statements: vec![Head {
                sequence: 0,
                id: cut[0].id().unwrap(),
            }],
            acknowledgments: None,
        }],
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
