use rocky_core::{
    AppId, DomainId, FieldSelection, MeshId, ResourceGrant, ResourceScope, Right, RowSelection,
    Scope,
};
use rocky_credentials::*;

const NOW: u64 = 1_800_000_000;
fn context(root: &PrivateKey, namespace: u8) -> DatabaseContext {
    DatabaseContext {
        publisher: root.public_key(),
        scope: Scope {
            mesh: MeshId([namespace; 16]),
            app: AppId([2; 16]),
            domain: DomainId([namespace; 16]),
        },
    }
}
fn chain(
    root: &PrivateKey,
    identity: &PrivateKey,
    database: DatabaseContext,
    first: u32,
) -> CredentialChain {
    let grants = vec![ResourceGrant {
        scope: ResourceScope {
            domain: database.scope,
            table: [3; 32],
            encryption: [4; 32],
        },
        rows: RowSelection::All,
        fields: FieldSelection::All,
        rights: [Right::Read, Right::Insert].into(),
    }];
    CredentialChain(vec![
        Authority::owner_in(root, database)
            .unwrap()
            .issue(
                identity.public_key(),
                grants,
                AuthorRange::new(first, first + 99).unwrap(),
                (NOW, DAY, 0),
            )
            .unwrap(),
    ])
    .certify(identity, PrivateKey::new().public_key(), first)
    .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn one_identity_has_independent_membership_under_distinct_roots_and_namespaces() {
    let roots = [PrivateKey::new(), PrivateKey::new()];
    let identity = PrivateKey::new();
    let contexts = [
        context(&roots[0], 1),
        context(&roots[1], 1),
        context(&roots[0], 9),
    ];
    let chains: Vec<_> = contexts
        .iter()
        .enumerate()
        .map(|(i, database)| chain(&roots[usize::from(i == 1)], &identity, *database, 100))
        .collect();
    let mut sets = Vec::new();
    for (database, chain) in contexts.iter().zip(&chains) {
        let mut set = RevocationSet::new();
        set.link(chain.certificate().unwrap().clone()).unwrap();
        set.verify_in(*database).unwrap();
        assert!(!set.is_empty());
        assert!(!set.has_revocations());
        let membership = set.membership().unwrap();
        assert_eq!(membership.next_sequence(identity.public_key()).unwrap(), 1);
        assert!(!membership.is_forked(identity.public_key()));
        sets.push(set);
    }
    for foreign in 1..contexts.len() {
        let before = sets[0].clone();
        let mut empty = RevocationSet::new();
        assert!(empty.merge_in(&sets[foreign], contexts[0], NOW).is_err());
        assert_eq!(empty, RevocationSet::new());
        assert!(
            sets[0]
                .link(chains[foreign].certificate().unwrap().clone())
                .is_err()
        );
        let foreign_set = sets[foreign].clone();
        assert!(
            sets[0]
                .merge(
                    &foreign_set,
                    contexts[0].publisher,
                    contexts[0].scope.app,
                    NOW
                )
                .is_err()
        );
        assert_eq!(sets[0], before);
        let mut view = sets[0].membership().unwrap();
        let original = view.clone();
        assert!(view.insert(chains[foreign].certificate().unwrap()).is_err());
        assert_eq!(view, original);
        let forged = chains[0]
            .capabilities()
            .with_certificate(chains[foreign].certificate().unwrap().clone());
        assert!(forged.verify_database(contexts[0]).is_err());
        let unlink = chains[foreign]
            .unlink(
                &identity,
                contexts[foreign].scope.app,
                NOW,
                chains[foreign].certificate().unwrap().capability.subject,
            )
            .unwrap();
        let foreign_unlink = RevocationSet::try_from(vec![unlink]).unwrap();
        assert!(foreign_unlink.verify_in(contexts[0]).is_err());
        assert!(
            sets[0]
                .merge(
                    &foreign_unlink,
                    contexts[0].publisher,
                    contexts[0].scope.app,
                    NOW
                )
                .is_err()
        );
        assert_eq!(sets[0], before);
    }
    // Filling one database never consumes another database's author or slot.
    for sequence in 1..MAX_MEMBERSHIP_NODES as u64 {
        let membership = sets[0].membership().unwrap();
        let linked = chains[0]
            .capabilities()
            .certify_member(
                &identity,
                PrivateKey::new().public_key(),
                100 + sequence as u32,
                sequence,
                membership
                    .head(identity.public_key())
                    .map(|head| head.certificate),
            )
            .unwrap();
        sets[0].link(linked.certificate().unwrap().clone()).unwrap();
    }
    assert!(
        !sets[0]
            .membership()
            .unwrap()
            .can_link(identity.public_key(), |_| false)
    );
    for set in &sets[1..] {
        assert!(
            set.membership()
                .unwrap()
                .can_link(identity.public_key(), |_| false)
        );
        assert_eq!(
            set.membership()
                .unwrap()
                .next_sequence(identity.public_key())
                .unwrap(),
            1
        );
    }
}
