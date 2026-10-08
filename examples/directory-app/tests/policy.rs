use directory_app::*;
use rocky_core::*;
use std::collections::BTreeSet;
fn app() -> DirectoryApp {
    DirectoryApp {
        publisher: [1; 32],
        domain: DomainId([2; 16]),
        bootstrap_registrar: principal([3; 32]),
        initial_ownership: vec![],
    }
}
fn populated() -> Directory {
    let app = app();
    let mut d = Directory::default();
    for key in [[4; 32], [5; 32], [6; 32]] {
        d = app
            .transition(d, app.bootstrap_registrar, Transition::RegisterUser { key })
            .unwrap();
    }
    d
}
#[test]
fn ownership_bounded_delegation_and_revocation_cascade() {
    let app = app();
    let alice = principal([4; 32]);
    let bob = principal([5; 32]);
    let carol = principal([6; 32]);
    let d = populated();
    assert!(
        app.transition(
            d.clone(),
            bob,
            Transition::RegisterNode {
                key: [7; 32],
                owner: alice,
                deployment: None,
                time: 10
            }
        )
        .is_err()
    );
    let grant = Delegation {
        id: [11; 32],
        owner: alice,
        subject: bob,
        rights: BTreeSet::from([DirectoryRight::RegisterNode, DirectoryRight::Delegate]),
        depth: 2,
        not_before: 10,
        expires: 20,
        parent: None,
    };
    let d = app
        .transition(
            d,
            alice,
            Transition::Delegate {
                grant: grant.clone(),
                time: 10,
            },
        )
        .unwrap();
    let d = app
        .transition(
            d,
            bob,
            Transition::RegisterNode {
                key: [7; 32],
                owner: alice,
                deployment: None,
                time: 10,
            },
        )
        .unwrap();
    assert!(d.registered_node([7; 32]));
    assert!(
        app.transition(
            d.clone(),
            bob,
            Transition::RegisterNode {
                key: [8; 32],
                owner: alice,
                deployment: None,
                time: 20
            }
        )
        .is_err()
    );
    let mut child = grant.clone();
    child.id = [12; 32];
    child.subject = carol;
    child.parent = Some(grant.id);
    child.depth = 1;
    let d = app
        .transition(
            d,
            bob,
            Transition::Delegate {
                grant: child.clone(),
                time: 11,
            },
        )
        .unwrap();
    let mut expanded = child.clone();
    expanded.id = [13; 32];
    expanded.rights.insert(DirectoryRight::RegisterDeployment);
    assert!(
        app.transition(
            d.clone(),
            bob,
            Transition::Delegate {
                grant: expanded,
                time: 11
            }
        )
        .is_err()
    );
    let d = app
        .transition(d, alice, Transition::RevokeDelegation { id: grant.id })
        .unwrap();
    assert!(d.delegations.is_empty());
    assert!(
        app.transition(
            d.clone(),
            carol,
            Transition::RegisterNode {
                key: [8; 32],
                owner: alice,
                deployment: None,
                time: 11
            }
        )
        .is_err()
    );
    assert!(
        app.transition(d, alice, Transition::Delegate { grant, time: 11 })
            .is_err()
    );
}
#[test]
fn deletion_requires_disposition_and_retains_revocation_key_and_pending_work() {
    let app = app();
    let alice = principal([4; 32]);
    let bob = principal([5; 32]);
    let mut d = populated();
    let scope = Scope {
        mesh: MeshId([9; 16]),
        app: AppId([8; 16]),
        domain: DomainId([7; 16]),
    };
    d.ownership.push(Ownership {
        scope,
        owner: alice,
    });
    d = app
        .transition(
            d,
            alice,
            Transition::RegisterDeployment {
                key: [7; 32],
                owner: alice,
                time: 10,
            },
        )
        .unwrap();
    d = app
        .transition(
            d,
            alice,
            Transition::RegisterNode {
                key: [8; 32],
                owner: alice,
                deployment: Some([7; 32]),
                time: 10,
            },
        )
        .unwrap();
    let resource = ResourceScope {
        domain: scope,
        encryption: [1; 32],
        table: [2; 32],
    };
    let pending = (
        scope,
        RequestId {
            node: NodeId([8; 16]),
            sequence: AuthorSequence::new(1).unwrap(),
        },
    );
    d = app
        .transition(
            d,
            alice,
            Transition::TrackArtifacts(Artifacts {
                owner: alice,
                app_grants: vec![[22; 32]],
                keys: vec![resource],
                pending: vec![pending],
            }),
        )
        .unwrap();
    assert!(
        app.transition(
            d.clone(),
            alice,
            Transition::DeleteUser {
                disposition: vec![]
            }
        )
        .is_err()
    );
    let d = app
        .transition(
            d,
            alice,
            Transition::DeleteUser {
                disposition: vec![(scope, Disposition::Transfer(bob))],
            },
        )
        .unwrap();
    assert_eq!(d.ownership[0].owner, bob);
    assert!(!d.registered_node([8; 32]));
    assert!(d.deployments.is_empty());
    assert_eq!(d.deletions[0].rotate_keys, vec![resource]);
    assert_eq!(d.deletions[0].resolve_pending, vec![pending]);
    assert_eq!(d.deletions[0].revoked_grants, vec![[22; 32]]);
    assert!(
        app.transition(
            d.clone(),
            app.bootstrap_registrar,
            Transition::RegisterUser { key: [4; 32] }
        )
        .is_err()
    );
    assert!(
        app.transition(
            d,
            bob,
            Transition::RegisterNode {
                key: [8; 32],
                owner: bob,
                deployment: None,
                time: 10
            }
        )
        .is_err()
    );
}
