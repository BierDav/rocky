use rocky_core::{AppId, DomainId, MeshId, PrincipalKind, Scope, TipId};
use rocky_credentials::{
    AuthorMark, AuthorRange, Authority, BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement,
    CredentialChain, Extended, Extension, PrivateKey, RevocationSet, SEAL_FORMAT, SealAction,
    SealStatement, SignedBookmarkMove, SignedCapability, SignedSeal, SignedTipOpening,
    TIP_OPENING_FORMAT, TipGraph, TipOpeningStatement, encode, genesis_tip,
};

fn graph() -> TipGraph {
    TipGraph::new(
        Scope {
            mesh: MeshId([1; 16]),
            app: AppId([2; 16]),
            domain: DomainId([3; 16]),
        },
        [4; 32],
    )
}
fn seal(graph: &TipGraph, tip: TipId, sequence: u64, owner: &PrivateKey) -> SignedSeal {
    SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: graph.scope,
            tip,
            frontier: vec![AuthorMark {
                author: 1,
                sequence,
            }],
            action: SealAction::Continue {
                schema: graph.genesis_schema,
            },
        },
        owner,
    )
    .unwrap()
}
fn movement(
    graph: &TipGraph,
    name: &str,
    from: Option<TipId>,
    to: Option<TipId>,
    owner: &PrivateKey,
) -> SignedBookmarkMove {
    SignedBookmarkMove::sign(
        BookmarkMoveStatement {
            format: BOOKMARK_MOVE_FORMAT,
            scope: graph.scope,
            name: name.into(),
            from,
            to,
        },
        owner,
    )
    .unwrap()
}
fn opening(graph: &TipGraph, seal: &SignedSeal, owner: &PrivateKey) -> SignedTipOpening {
    SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            scope: graph.scope,
            seal: seal.id().unwrap(),
            minimum_in_sync: 2,
        },
        owner,
    )
    .unwrap()
}

#[test]
fn genesis_and_statement_ids_roundtrip_and_refuse_other_formats() {
    let owner = PrivateKey::new();
    let mut graph = graph();
    assert_eq!(genesis_tip(graph.genesis_schema), graph.genesis());
    assert_ne!(genesis_tip([5; 32]), graph.genesis());
    let signed = seal(&graph, graph.genesis(), 3, &owner);
    let decoded = SignedSeal::from_bytes(&signed.to_bytes().unwrap()).unwrap();
    decoded.verify(owner.public_key(), &graph).unwrap();
    assert_eq!(decoded, signed);
    assert_eq!(
        signed.successor().unwrap(),
        Some(TipId(signed.id().unwrap()))
    );
    graph.seals.push(signed.clone());
    let opened = opening(&graph, &signed, &owner);
    SignedTipOpening::from_bytes(&opened.to_bytes().unwrap())
        .unwrap()
        .verify(owner.public_key(), &graph)
        .unwrap();
    assert_eq!(opened.tip().unwrap(), TipId(opened.id().unwrap()));
    let moved = movement(&graph, "main", None, Some(graph.genesis()), &owner);
    SignedBookmarkMove::from_bytes(&moved.to_bytes().unwrap())
        .unwrap()
        .verify(owner.public_key(), &graph)
        .unwrap();
    for format in [0, 2, u32::MAX] {
        let mut other = signed.clone();
        other.statement.format = format;
        assert!(other.verify(owner.public_key(), &graph).is_err());
        assert!(SignedSeal::from_bytes(&other.to_bytes().unwrap()).is_err());
        let mut other = opened.clone();
        other.statement.format = format;
        assert!(other.verify(owner.public_key(), &graph).is_err());
        assert!(SignedTipOpening::from_bytes(&other.to_bytes().unwrap()).is_err());
        let mut other = moved.clone();
        other.statement.format = format;
        assert!(other.verify(owner.public_key(), &graph).is_err());
        assert!(SignedBookmarkMove::from_bytes(&other.to_bytes().unwrap()).is_err());
        #[derive(serde::Serialize)]
        struct Format {
            format: u32,
        }
        #[derive(serde::Serialize)]
        struct Envelope {
            statement: Format,
        }
        let bytes = encode(&Envelope {
            statement: Format { format },
        })
        .unwrap();
        assert!(SignedSeal::from_bytes(&bytes).is_err());
        assert!(SignedTipOpening::from_bytes(&bytes).is_err());
        assert!(SignedBookmarkMove::from_bytes(&bytes).is_err());
    }
    let mut tampered = signed.clone();
    tampered.statement.frontier[0].sequence += 1;
    assert!(tampered.verify(owner.public_key(), &graph).is_err());
    assert!(
        signed
            .verify(PrivateKey::new().public_key(), &graph)
            .is_err()
    );
}

#[test]
fn two_issuers_seal_concurrently_and_lower_digest_wins_in_every_order() {
    let owner = PrivateKey::new();
    let issuers = [PrivateKey::new(), PrivateKey::new()];
    let mut graph = graph();
    let mut seals = Vec::new();
    for (index, issuer) in issuers.iter().enumerate() {
        let chain = CredentialChain(vec![
            Authority::owner(&owner)
                .issue_issuer(issuer.public_key(), None, (100, 1000, 0))
                .unwrap(),
        ]);
        let signed = SignedSeal::endorse(
            seal(&graph, graph.genesis(), index as u64, &owner).statement,
            issuer,
            &chain,
        )
        .unwrap();
        signed
            .verify_current(owner.public_key(), &graph, &RevocationSet::new(), 200)
            .unwrap();
        seals.push(signed);
    }
    graph.seals = seals.clone();
    let winner = seals.iter().min_by_key(|seal| seal.id().unwrap()).unwrap();
    assert_eq!(graph.seal(graph.genesis()).unwrap(), Some(winner));
    assert_eq!(
        graph.schema(winner.successor().unwrap().unwrap()).unwrap(),
        graph.genesis_schema
    );
    let losing = seals.iter().max_by_key(|seal| seal.id().unwrap()).unwrap();
    assert!(graph.schema(losing.successor().unwrap().unwrap()).is_err());
    graph.seals.reverse();
    assert_eq!(graph.seal(graph.genesis()).unwrap(), Some(winner));
    assert_eq!(
        graph
            .line_root(winner.successor().unwrap().unwrap())
            .unwrap(),
        graph.genesis()
    );
    let losing_tip = losing.successor().unwrap().unwrap();
    let retained = seal(&graph, losing_tip, 8, &owner);
    retained.verify(owner.public_key(), &graph).unwrap();
    graph.seals.push(retained.clone());
    assert_eq!(
        graph
            .schema_any(retained.successor().unwrap().unwrap())
            .unwrap(),
        graph.genesis_schema
    );
    assert!(
        graph
            .schema(retained.successor().unwrap().unwrap())
            .is_err()
    );
    let created = movement(&graph, "main", None, Some(graph.genesis()), &owner);
    let moved = movement(
        &graph,
        "main",
        Some(graph.genesis()),
        Some(losing_tip),
        &owner,
    );
    moved.verify(owner.public_key(), &graph).unwrap();
    graph.moves = vec![created, moved];
    assert_eq!(graph.bookmark("main").unwrap(), winner.successor().unwrap());
    graph.seals.reverse();
    graph.moves.reverse();
    assert_eq!(graph.bookmark("main").unwrap(), winner.successor().unwrap());
}

#[test]
fn concurrent_bookmark_moves_and_backward_move_resolve_without_arrival_order() {
    let owner = PrivateKey::new();
    let mut graph = graph();
    let first = seal(&graph, graph.genesis(), 1, &owner);
    let second_tip = first.successor().unwrap().unwrap();
    graph.seals.push(first);
    let second = seal(&graph, second_tip, 2, &owner);
    let third_tip = second.successor().unwrap().unwrap();
    graph.seals.push(second);
    let created = movement(&graph, "main", None, Some(graph.genesis()), &owner);
    let a = movement(
        &graph,
        "main",
        Some(graph.genesis()),
        Some(second_tip),
        &owner,
    );
    let b = movement(
        &graph,
        "main",
        Some(graph.genesis()),
        Some(third_tip),
        &owner,
    );
    let expected = if a.id().unwrap() < b.id().unwrap() {
        second_tip
    } else {
        third_tip
    };
    graph.moves = vec![created, a, b];
    assert_eq!(graph.bookmark("main").unwrap(), Some(expected));
    graph.moves.reverse();
    assert_eq!(graph.bookmark("main").unwrap(), Some(expected));
    graph.moves.push(movement(
        &graph,
        "main",
        Some(expected),
        Some(graph.genesis()),
        &owner,
    ));
    assert_eq!(graph.bookmark("main").unwrap(), Some(graph.genesis()));
}

#[test]
fn issuers_move_only_forward_and_owner_controlled_trust_opens_and_moves_sideways() {
    let owner = PrivateKey::new();
    let issuer = PrivateKey::new();
    let trusted = PrivateKey::new();
    let mut graph = graph();
    let issuer_chain = CredentialChain(vec![
        Authority::owner(&owner)
            .issue_issuer(issuer.public_key(), None, (100, 1000, 0))
            .unwrap(),
    ]);
    let trusted_chain = CredentialChain(vec![
        Authority::owner(&owner)
            .issue_trusted(
                trusted.public_key(),
                graph.scope,
                Vec::new(),
                AuthorRange::ALL,
                (100, 1000, 0),
            )
            .unwrap(),
    ]);
    let first = seal(&graph, graph.genesis(), 1, &owner);
    let next = first.successor().unwrap().unwrap();
    graph.seals.push(first.clone());
    let opened = SignedTipOpening::endorse(
        opening(&graph, &first, &owner).statement,
        &trusted,
        &trusted_chain,
    )
    .unwrap();
    opened
        .verify_current(owner.public_key(), &graph, &RevocationSet::new(), 200)
        .unwrap();
    let branch = opened.tip().unwrap();
    graph.openings.push(opened.clone());
    assert_eq!(graph.line_root(branch).unwrap(), branch);
    assert!(!graph.continues(next, branch).unwrap());
    assert!(
        SignedTipOpening::endorse(opened.statement, &issuer, &issuer_chain)
            .unwrap()
            .verify(owner.public_key(), &graph)
            .is_err()
    );
    for (from, to, allowed) in [
        (Some(graph.genesis()), Some(next), true),
        (None, Some(next), false),
        (Some(next), None, false),
        (Some(next), Some(graph.genesis()), false),
        (Some(next), Some(branch), false),
    ] {
        let statement = movement(&graph, "main", from, to, &owner).statement;
        let signed =
            SignedBookmarkMove::endorse(statement.clone(), &issuer, &issuer_chain).unwrap();
        assert_eq!(
            signed
                .verify_current(owner.public_key(), &graph, &RevocationSet::new(), 200)
                .is_ok(),
            allowed
        );
        SignedBookmarkMove::endorse(statement, &trusted, &trusted_chain)
            .unwrap()
            .verify_current(owner.public_key(), &graph, &RevocationSet::new(), 200)
            .unwrap();
    }
    assert!(
        Authority::issuer(&issuer, &issuer_chain)
            .issue_trusted(
                trusted.public_key(),
                graph.scope,
                Vec::new(),
                AuthorRange::ALL,
                (100, 1000, 0)
            )
            .is_err()
    );
    let forged = SignedCapability::sign(trusted_chain.0[0].capability.clone(), &issuer).unwrap();
    let forged_chain = CredentialChain(vec![issuer_chain.0[0].clone(), forged]);
    assert!(
        SignedTipOpening::endorse(
            opening(&graph, &first, &owner).statement,
            &trusted,
            &forged_chain
        )
        .unwrap()
        .verify(owner.public_key(), &graph)
        .is_err()
    );
    assert!(
        SignedTipOpening::endorse(
            opening(&graph, &first, &owner).statement,
            &trusted,
            &trusted_chain
        )
        .unwrap()
        .verify_current(owner.public_key(), &graph, &RevocationSet::new(), 1200)
        .is_err()
    );
    let mut other = graph.clone();
    other.scope.domain = DomainId([7; 16]);
    let wrong = SignedSeal::endorse(
        seal(&other, other.genesis(), 0, &owner).statement,
        &trusted,
        &trusted_chain,
    )
    .unwrap();
    assert!(wrong.verify(owner.public_key(), &other).is_err());
}

#[test]
fn schema_changes_partial_and_node_authority_and_invalid_frontiers_are_refused() {
    let owner = PrivateKey::new();
    let user = PrivateKey::new();
    let graph = graph();
    let mut statement = seal(&graph, graph.genesis(), 1, &owner).statement;
    statement.action = SealAction::Continue { schema: [8; 32] };
    assert!(
        SignedSeal::sign(statement.clone(), &owner)
            .unwrap()
            .verify(owner.public_key(), &graph)
            .is_err()
    );
    statement.action = SealAction::Close;
    SignedSeal::sign(statement.clone(), &owner)
        .unwrap()
        .verify(owner.public_key(), &graph)
        .unwrap();
    for frontier in [
        vec![AuthorMark {
            author: 0,
            sequence: 1,
        }],
        vec![AuthorMark {
            author: 1,
            sequence: u64::MAX,
        }],
        vec![
            AuthorMark {
                author: 2,
                sequence: 1,
            },
            AuthorMark {
                author: 1,
                sequence: 1,
            },
        ],
        vec![
            AuthorMark {
                author: 1,
                sequence: 1
            };
            2
        ],
    ] {
        statement.frontier = frontier;
        assert!(SignedSeal::sign(statement.clone(), &owner).is_err());
    }
    let capability = rocky_credentials::Capability {
        subject: user.public_key(),
        kind: PrincipalKind::User,
        grants: vec![rocky_core::ResourceGrant {
            scope: rocky_core::ResourceScope {
                domain: graph.scope,
                table: [9; 32],
                encryption: [8; 32],
            },
            rows: rocky_core::RowSelection::All,
            fields: rocky_core::FieldSelection::All,
            rights: [rocky_core::Right::Read].into(),
        }],
        not_before: 100,
        expires: 1000,
        delegations: 1,
        extension: Extension::new(Extended {
            authors: Some(AuthorRange::ALL),
            ..Default::default()
        })
        .unwrap(),
    };
    let chain = CredentialChain(vec![SignedCapability::sign(capability, &owner).unwrap()]);
    assert!(
        SignedSeal::endorse(
            seal(&graph, graph.genesis(), 1, &owner).statement,
            &user,
            &chain
        )
        .unwrap()
        .verify(owner.public_key(), &graph)
        .is_err()
    );
}

#[test]
fn node_signatures_never_inherit_trusted_identity_control() {
    let owner = PrivateKey::new();
    let identity = PrivateKey::new();
    let node = PrivateKey::new();
    let graph = graph();
    let root = Authority::owner(&owner)
        .issue_trusted(
            identity.public_key(),
            graph.scope,
            vec![rocky_core::ResourceGrant {
                scope: rocky_core::ResourceScope {
                    domain: graph.scope,
                    table: [9; 32],
                    encryption: [8; 32],
                },
                rows: rocky_core::RowSelection::All,
                fields: rocky_core::FieldSelection::All,
                rights: [rocky_core::Right::Read].into(),
            }],
            AuthorRange::ALL,
            (100, 1000, 0),
        )
        .unwrap();
    let chain = rocky_credentials::device_chain(root, &identity, node.public_key(), 1).unwrap();
    chain
        .verify(
            owner.public_key(),
            node.public_key(),
            &RevocationSet::new(),
            200,
        )
        .unwrap();
    assert_eq!(
        chain
            .0
            .last()
            .unwrap()
            .capability
            .extended()
            .unwrap()
            .trusted,
        None
    );
    assert!(
        SignedSeal::endorse(
            seal(&graph, graph.genesis(), 1, &owner).statement,
            &node,
            &chain
        )
        .unwrap()
        .verify(owner.public_key(), &graph)
        .is_err()
    );
}
