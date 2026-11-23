use rocky_core::{AppId, DomainId, MeshId, PrincipalKind, Scope, TipId};
use rocky_credentials::{
    AuthorMark, Authority, BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, CredentialChain, Extended,
    Extension, PrivateKey, RevocationSet, SEAL_FORMAT, SealAction, SealStatement,
    SignedBookmarkMove, SignedCapability, SignedSeal, SignedTipOpening, TIP_OPENING_FORMAT,
    TipGraph, TipOpeningStatement, encode, genesis_tip,
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
                author: rocky_core::WriterId::from_bytes([1; 32]),
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
            schema: graph.schema_any(seal.statement.tip).unwrap(),
            scope: graph.scope,
            seal: seal.id().unwrap(),
        },
        owner,
    )
    .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn genesis_and_statement_ids_roundtrip_and_refuse_other_formats() {
    let owner = PrivateKey::new();
    let mut graph = graph();
    assert_eq!(genesis_tip(graph.genesis_schema), graph.genesis());
    assert_ne!(genesis_tip([5; 32]), graph.genesis());
    let signed = seal(&graph, graph.genesis(), 3, &owner);
    let decoded = SignedSeal::from_bytes(&signed.to_bytes().unwrap()).unwrap();
    decoded.verify_historic(owner.public_key(), &graph).unwrap();
    assert_eq!(decoded, signed);
    assert_eq!(
        signed.successor().unwrap(),
        Some(TipId(signed.id().unwrap()))
    );
    graph.seals.push(signed.clone());
    let opened = opening(&graph, &signed, &owner);
    SignedTipOpening::from_bytes(&opened.to_bytes().unwrap())
        .unwrap()
        .verify_historic(owner.public_key(), &graph)
        .unwrap();
    assert_eq!(opened.tip().unwrap(), TipId(opened.id().unwrap()));
    let moved = movement(&graph, "main", None, Some(graph.genesis()), &owner);
    SignedBookmarkMove::from_bytes(&moved.to_bytes().unwrap())
        .unwrap()
        .verify_historic(owner.public_key(), &graph)
        .unwrap();
    for format in [0, 1, 9, u32::MAX] {
        let mut other = signed.clone();
        other.statement.format = format;
        assert!(other.verify_historic(owner.public_key(), &graph).is_err());
        assert!(SignedSeal::from_bytes(&other.to_bytes().unwrap()).is_err());
        let mut other = opened.clone();
        other.statement.format = format;
        assert!(other.verify_historic(owner.public_key(), &graph).is_err());
        assert!(SignedTipOpening::from_bytes(&other.to_bytes().unwrap()).is_err());
        let mut other = moved.clone();
        other.statement.format = format;
        assert!(other.verify_historic(owner.public_key(), &graph).is_err());
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
    assert!(
        tampered
            .verify_historic(owner.public_key(), &graph)
            .is_err()
    );
    assert!(
        signed
            .verify_historic(PrivateKey::new().public_key(), &graph)
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn two_issuers_seal_concurrently_and_lower_digest_wins_in_every_order() {
    let owner = PrivateKey::new();
    let issuers = [PrivateKey::new(), PrivateKey::new()];
    let mut graph = graph();
    let mut seals = Vec::new();
    for (index, issuer) in issuers.iter().enumerate() {
        let chain = CredentialChain(vec![
            Authority::owner_in(
                &owner,
                rocky_credentials::DatabaseContext {
                    publisher: owner.public_key(),
                    scope: Scope {
                        mesh: MeshId([1; 16]),
                        app: AppId([2; 16]),
                        domain: DomainId([3; 16]),
                    },
                },
            )
            .unwrap()
            .issue_issuer(
                issuer.public_key(),
                rocky_credentials::Validity {
                    issued_at: 100,
                    lifetime: std::time::Duration::from_secs(1000),
                    clock_skew: std::time::Duration::from_secs(0),
                },
            )
            .unwrap(),
        ]);
        let signed = SignedSeal::endorse_linked(
            seal(&graph, graph.genesis(), index as u64, &owner).statement,
            rocky_credentials::testing::lane_for(issuer, &chain),
            &chain,
            link(&graph, issuer, &chain),
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
    retained
        .verify_historic(owner.public_key(), &graph)
        .unwrap();
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
    moved.verify_historic(owner.public_key(), &graph).unwrap();
    graph.moves = vec![created, moved];
    assert_eq!(graph.bookmark("main").unwrap(), winner.successor().unwrap());
    graph.seals.reverse();
    graph.moves.reverse();
    assert_eq!(graph.bookmark("main").unwrap(), winner.successor().unwrap());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn issuers_move_only_forward_and_owner_controlled_trust_opens_and_moves_sideways() {
    let owner = PrivateKey::new();
    let issuer = PrivateKey::new();
    let trusted = PrivateKey::new();
    let mut graph = graph();
    let issuer_chain = CredentialChain(vec![
        Authority::owner_in(
            &owner,
            rocky_credentials::DatabaseContext {
                publisher: owner.public_key(),
                scope: Scope {
                    mesh: MeshId([1; 16]),
                    app: AppId([2; 16]),
                    domain: DomainId([3; 16]),
                },
            },
        )
        .unwrap()
        .issue_issuer(
            issuer.public_key(),
            rocky_credentials::Validity {
                issued_at: 100,
                lifetime: std::time::Duration::from_secs(1000),
                clock_skew: std::time::Duration::from_secs(0),
            },
        )
        .unwrap(),
    ]);
    let trusted_chain = CredentialChain(vec![
        Authority::owner_in(
            &owner,
            rocky_credentials::DatabaseContext {
                publisher: owner.public_key(),
                scope: Scope {
                    mesh: MeshId([1; 16]),
                    app: AppId([2; 16]),
                    domain: DomainId([3; 16]),
                },
            },
        )
        .unwrap()
        .issue_trusted(trusted.public_key(), graph.scope, 100)
        .unwrap(),
    ]);
    let first = seal(&graph, graph.genesis(), 1, &owner);
    let next = first.successor().unwrap().unwrap();
    graph.seals.push(first.clone());
    let opened = SignedTipOpening::endorse_linked(
        opening(&graph, &first, &owner).statement,
        rocky_credentials::testing::lane_for(&trusted, &trusted_chain),
        &trusted_chain,
        link(&graph, &trusted, &trusted_chain),
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
        SignedTipOpening::endorse_linked(
            opened.statement,
            rocky_credentials::testing::lane_for(&issuer, &issuer_chain),
            &issuer_chain,
            link(&graph, &issuer, &issuer_chain)
        )
        .unwrap()
        .verify_historic(owner.public_key(), &graph)
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
        let signed = SignedBookmarkMove::endorse_linked(
            statement.clone(),
            rocky_credentials::testing::lane_for(&issuer, &issuer_chain),
            &issuer_chain,
            link(&graph, &issuer, &issuer_chain),
        )
        .unwrap();
        assert_eq!(
            signed
                .verify_current(owner.public_key(), &graph, &RevocationSet::new(), 200)
                .is_ok(),
            allowed
        );
        SignedBookmarkMove::endorse_linked(
            statement,
            rocky_credentials::testing::lane_for(&trusted, &trusted_chain),
            &trusted_chain,
            link(&graph, &trusted, &trusted_chain),
        )
        .unwrap()
        .verify_current(owner.public_key(), &graph, &RevocationSet::new(), 200)
        .unwrap();
    }
    assert!(
        rocky_credentials::testing::authority(&issuer, &issuer_chain)
            .issue_trusted(trusted.public_key(), graph.scope, 100)
            .is_err()
    );
    let forged = SignedCapability::sign(trusted_chain.0[0].capability.clone(), &issuer).unwrap();
    let forged_chain = CredentialChain(vec![issuer_chain.0[0].clone(), forged]);
    assert!(
        SignedTipOpening::endorse_linked(
            opening(&graph, &first, &owner).statement,
            rocky_credentials::testing::lane_for(&trusted, &forged_chain),
            &forged_chain,
            link(&graph, &trusted, &forged_chain)
        )
        .unwrap()
        .verify_historic(owner.public_key(), &graph)
        .is_err()
    );
    let opened = SignedTipOpening::endorse_linked(
        opening(&graph, &first, &owner).statement,
        rocky_credentials::testing::lane_for(&trusted, &trusted_chain),
        &trusted_chain,
        link(&graph, &trusted, &trusted_chain),
    )
    .unwrap();
    assert!(
        opened
            .verify_current(
                owner.public_key(),
                &graph,
                &RevocationSet::new(),
                u64::MAX / 2
            )
            .is_ok(),
        "a trusted grant never expires; only the root's revocation ends it"
    );
    assert!(
        opened
            .verify_current(
                owner.public_key(),
                &graph,
                &revoking(&owner, &[trusted.public_key()]),
                1200
            )
            .is_err()
    );
    let mut other = graph.clone();
    other.scope.domain = DomainId([7; 16]);
    let wrong = SignedSeal::endorse_linked(
        seal(&other, other.genesis(), 0, &owner).statement,
        rocky_credentials::testing::lane_for(&trusted, &trusted_chain),
        &trusted_chain,
        link(&graph, &trusted, &trusted_chain),
    )
    .unwrap();
    assert!(wrong.verify_historic(owner.public_key(), &other).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn schema_changes_are_allowed_but_partial_and_node_authority_and_invalid_frontiers_are_refused() {
    let owner = PrivateKey::new();
    let user = PrivateKey::new();
    let graph = graph();
    let mut statement = seal(&graph, graph.genesis(), 1, &owner).statement;
    statement.action = SealAction::Continue { schema: [8; 32] };
    SignedSeal::sign(statement.clone(), &owner)
        .unwrap()
        .verify_historic(owner.public_key(), &graph)
        .unwrap();
    statement.action = SealAction::Close;
    SignedSeal::sign(statement.clone(), &owner)
        .unwrap()
        .verify_historic(owner.public_key(), &graph)
        .unwrap();
    for frontier in [
        vec![AuthorMark {
            author: rocky_core::WriterId::from_bytes([1; 32]),
            sequence: u64::MAX,
        }],
        vec![
            AuthorMark {
                author: rocky_core::WriterId::from_bytes([2; 32]),
                sequence: 1,
            },
            AuthorMark {
                author: rocky_core::WriterId::from_bytes([1; 32]),
                sequence: 1,
            },
        ],
        vec![
            AuthorMark {
                author: rocky_core::WriterId::from_bytes([1; 32]),
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
            ..Default::default()
        })
        .unwrap(),
    };
    let chain = CredentialChain(vec![SignedCapability::sign(capability, &owner).unwrap()]);
    assert!(
        SignedSeal::endorse_linked(
            seal(&graph, graph.genesis(), 1, &owner).statement,
            rocky_credentials::testing::lane_for(&user, &chain),
            &chain,
            link(&graph, &user, &chain)
        )
        .unwrap()
        .verify_historic(owner.public_key(), &graph)
        .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn node_signatures_never_inherit_trusted_identity_control() {
    let owner = PrivateKey::new();
    let identity = PrivateKey::new();
    let node = PrivateKey::new();
    let graph = graph();
    let root = Authority::owner_in(
        &owner,
        rocky_credentials::DatabaseContext {
            publisher: owner.public_key(),
            scope: Scope {
                mesh: MeshId([1; 16]),
                app: AppId([2; 16]),
                domain: DomainId([3; 16]),
            },
        },
    )
    .unwrap()
    .issue_trusted(identity.public_key(), graph.scope, 100)
    .unwrap();
    let chain = rocky_credentials::device_chain(root, &identity, &node).unwrap();
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
        SignedSeal::endorse_linked(
            seal(&graph, graph.genesis(), 1, &owner).statement,
            rocky_credentials::testing::lane_for(&node, &chain),
            &chain,
            link(&graph, &node, &chain)
        )
        .unwrap()
        .verify_historic(owner.public_key(), &graph)
        .is_err()
    );
}

fn issuer_chain(owner: &PrivateKey, issuer: &PrivateKey) -> CredentialChain {
    CredentialChain(vec![
        Authority::owner_in(
            owner,
            rocky_credentials::DatabaseContext {
                publisher: owner.public_key(),
                scope: Scope {
                    mesh: MeshId([1; 16]),
                    app: AppId([2; 16]),
                    domain: DomainId([3; 16]),
                },
            },
        )
        .unwrap()
        .issue_issuer(
            issuer.public_key(),
            rocky_credentials::Validity {
                issued_at: 100,
                lifetime: std::time::Duration::from_secs(1000),
                clock_skew: std::time::Duration::from_secs(0),
            },
        )
        .unwrap(),
    ])
}
fn trusted_chain(graph: &TipGraph, owner: &PrivateKey, trusted: &PrivateKey) -> CredentialChain {
    CredentialChain(vec![
        Authority::owner_in(
            owner,
            rocky_credentials::DatabaseContext {
                publisher: owner.public_key(),
                scope: Scope {
                    mesh: MeshId([1; 16]),
                    app: AppId([2; 16]),
                    domain: DomainId([3; 16]),
                },
            },
        )
        .unwrap()
        .issue_trusted(trusted.public_key(), graph.scope, 100)
        .unwrap(),
    ])
}
fn endorsed(
    graph: &TipGraph,
    owner: &PrivateKey,
    tip: TipId,
    sequence: u64,
    signer: &PrivateKey,
    chain: &CredentialChain,
) -> SignedSeal {
    let signed = SignedSeal::endorse_linked(
        seal(graph, tip, sequence, owner).statement,
        rocky_credentials::testing::lane_for(signer, chain),
        chain,
        link(graph, signer, chain),
    )
    .unwrap();
    signed
        .verify_current(owner.public_key(), graph, &RevocationSet::new(), 200)
        .unwrap();
    signed
}
fn revoking(owner: &PrivateKey, identities: &[rocky_credentials::PublicKey]) -> RevocationSet {
    let statement = rocky_credentials::Revocation {
        app: graph().scope.app,
        issuer: owner.public_key(),
        time: 200,
        nodes: Default::default(),
        identities: identities.iter().copied().collect(),
        capabilities: Default::default(),
        cut: Default::default(),
        mode: rocky_credentials::RevocationMode::Lost,
    };
    let signed = Authority::owner_in(
        owner,
        rocky_credentials::DatabaseContext {
            publisher: owner.public_key(),
            scope: Scope {
                mesh: MeshId([1; 16]),
                app: AppId([2; 16]),
                domain: DomainId([3; 16]),
            },
        },
    )
    .unwrap()
    .revoke(statement)
    .unwrap();
    RevocationSet::try_from(vec![signed]).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn seals_resolve_by_authority_before_digest_and_revocation_resolves_again() {
    let owner = PrivateKey::new();
    let (issuer, other, trusted) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let mut graph = graph();
    let tip = graph.genesis();
    let issuers: Vec<_> = (0..8)
        .map(|sequence| {
            let signer = if sequence % 2 == 0 { &issuer } else { &other };
            endorsed(
                &graph,
                &owner,
                tip,
                sequence,
                signer,
                &issuer_chain(&owner, signer),
            )
        })
        .collect();
    let trusted_seal = endorsed(
        &graph,
        &owner,
        tip,
        20,
        &trusted,
        &trusted_chain(&graph, &owner, &trusted),
    );
    let owner_seal = seal(&graph, tip, 30, &owner);
    let authority = |seal: &SignedSeal| graph.authority(seal.endorsement());
    assert_eq!(
        authority(&issuers[0]),
        rocky_credentials::TipAuthority::Issuer
    );
    assert_eq!(
        authority(&trusted_seal),
        rocky_credentials::TipAuthority::Trusted
    );
    assert_eq!(
        authority(&owner_seal),
        rocky_credentials::TipAuthority::Owner
    );

    graph.seals = issuers.clone();
    let lowest = issuers
        .iter()
        .min_by_key(|seal| seal.id().unwrap())
        .unwrap()
        .clone();
    assert_eq!(
        graph.seal(tip).unwrap(),
        Some(&lowest),
        "among issuers the lower digest wins"
    );
    graph.seals.push(trusted_seal.clone());
    assert_eq!(
        graph.seal(tip).unwrap(),
        Some(&trusted_seal),
        "a trusted seal beats every issuer's, whatever the digests"
    );
    graph.seals.insert(0, owner_seal.clone());
    assert_eq!(
        graph.seal(tip).unwrap(),
        Some(&owner_seal),
        "the owner's seal beats all"
    );
    graph.seals.reverse();
    assert_eq!(
        graph.seal(tip).unwrap(),
        Some(&owner_seal),
        "the owner's seal beats all in every order"
    );
    assert!(
        graph
            .schema(trusted_seal.successor().unwrap().unwrap())
            .is_err()
    );

    graph.seals = issuers.clone();
    let winner_signer = if issuers.iter().position(|seal| *seal == lowest).unwrap() % 2 == 0 {
        &issuer
    } else {
        &other
    };
    let revocations = revoking(&owner, &[winner_signer.public_key()]);
    assert!(
        graph.exclude_revoked(&revocations).unwrap(),
        "revoking the issuer whose seal won among issuers changes the race"
    );
    assert!(!graph.exclude_revoked(&revocations).unwrap());
    let next = issuers
        .iter()
        .filter(|seal| !graph.revoked.contains(&seal.id().unwrap()))
        .min_by_key(|seal| seal.id().unwrap())
        .unwrap();
    assert_ne!(next, &lowest);
    assert_eq!(graph.seal(tip).unwrap(), Some(next));
    assert_eq!(graph.revoked.len(), 4);
    graph.seals.push(owner_seal.clone());
    assert!(!graph.exclude_revoked(&revocations).unwrap());
    assert!(!graph.revoked.contains(&owner_seal.id().unwrap()));
    assert_eq!(graph.seal(tip).unwrap(), Some(&owner_seal));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_owners_backward_move_beats_an_issuers_forward_move_and_the_loser_is_listed() {
    let owner = PrivateKey::new();
    let issuer = PrivateKey::new();
    let chain = issuer_chain(&owner, &issuer);
    let mut graph = graph();
    let first = seal(&graph, graph.genesis(), 1, &owner);
    let main_tip = first.successor().unwrap().unwrap();
    graph.seals.push(first);
    graph.moves = vec![
        movement(&graph, "main", None, Some(graph.genesis()), &owner),
        movement(
            &graph,
            "main",
            Some(graph.genesis()),
            Some(main_tip),
            &owner,
        ),
    ];
    let forward_seal = endorsed(&graph, &owner, main_tip, 2, &issuer, &chain);
    let forward_tip = forward_seal.successor().unwrap().unwrap();
    let mut close = seal(&graph, main_tip, 3, &owner);
    close.statement.action = SealAction::Close;
    let close = SignedSeal::sign(close.statement, &owner).unwrap();
    graph.seals.push(forward_seal.clone());
    let forward = SignedBookmarkMove::endorse_linked(
        movement(&graph, "main", Some(main_tip), Some(forward_tip), &owner).statement,
        rocky_credentials::testing::lane_for(&issuer, &chain),
        &chain,
        link(&graph, &issuer, &chain),
    )
    .unwrap();
    graph.seals.push(close.clone());
    let back = movement(
        &graph,
        "main",
        Some(main_tip),
        Some(graph.genesis()),
        &owner,
    );
    for order in [false, true] {
        graph.moves.truncate(2);
        if order {
            graph.moves.extend([forward.clone(), back.clone()]);
        } else {
            graph.moves.extend([back.clone(), forward.clone()]);
        }
        assert_eq!(
            graph.seal(main_tip).unwrap(),
            Some(&close),
            "the owner's close wins over an issuer's concurrent seal"
        );
        assert_eq!(graph.bookmark("main").unwrap(), Some(graph.genesis()));
        assert_eq!(graph.losing_moves("main").unwrap(), vec![&forward]);
    }
    assert!(
        graph
            .exclude_revoked(&revoking(&owner, &[issuer.public_key()]))
            .unwrap()
    );
    assert_eq!(
        graph.bookmark("main").unwrap(),
        Some(graph.genesis()),
        "revoking the owner's rival changes nothing here"
    );
    assert!(
        graph.losing_moves("main").unwrap().is_empty(),
        "a revoked loser is no longer listed"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn checkpoint_records_name_a_held_seal_and_revocation_unlists_them() {
    use rocky_credentials::{
        CHECKPOINT_RECORD_FORMAT, CheckpointRecordStatement, SignedCheckpointRecord,
    };
    let owner = PrivateKey::new();
    let issuer = PrivateKey::new();
    let mut graph = graph();
    let sealed = seal(&graph, graph.genesis(), 3, &owner);
    let statement = |seal: &SignedSeal, rows: u64| CheckpointRecordStatement {
        format: CHECKPOINT_RECORD_FORMAT,
        scope: graph.scope,
        seal: seal.id().unwrap(),
        table: rocky_core::ResourceScope {
            domain: graph.scope,
            encryption: [6; 32],
            table: [7; 32],
        },
        checkpoint: [8; 32],
        signer: PrivateKey::new().public_key(),
        rows,
    };
    let record = SignedCheckpointRecord::endorse_linked(
        statement(&sealed, 4),
        rocky_credentials::testing::lane_for(&issuer, &issuer_chain(&owner, &issuer)),
        &issuer_chain(&owner, &issuer),
        link(&graph, &issuer, &issuer_chain(&owner, &issuer)),
    )
    .unwrap();
    let current = |record: &SignedCheckpointRecord, graph: &TipGraph| {
        record.verify_current(owner.public_key(), graph, &RevocationSet::new(), 200)
    };
    assert!(
        current(&record, &graph).is_err(),
        "a record of a seal the graph does not hold is refused"
    );
    graph.seals.push(sealed.clone());
    current(&record, &graph).unwrap();
    let mut other = statement(&sealed, 4);
    other.table.domain.domain = DomainId([9; 16]);
    assert!(SignedCheckpointRecord::sign(other, &owner).is_err());
    let decoded = SignedCheckpointRecord::from_bytes(&record.to_bytes().unwrap()).unwrap();
    assert_eq!(decoded, record);

    let owners = SignedCheckpointRecord::sign(statement(&sealed, 5), &owner).unwrap();
    graph.checkpoints = vec![record.clone(), owners.clone()];
    assert_eq!(
        graph.checkpoints_at(sealed.id().unwrap()).unwrap(),
        vec![&record, &owners],
        "inspection lists a checkpoint at its seal"
    );
    assert!(graph.checkpoints_at([0; 32]).unwrap().is_empty());
    assert!(
        graph
            .exclude_revoked(&revoking(&owner, &[issuer.public_key()]))
            .unwrap()
    );
    assert_eq!(
        graph.checkpoints_at(sealed.id().unwrap()).unwrap(),
        vec![&owners],
        "revoking the issuer unlists its record; the owner's stays"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_seal_in_a_parent_format_is_refused() {
    #[derive(serde::Serialize)]
    struct ParentStatement {
        format: u32,
        scope: Scope,
        tip: TipId,
        frontier: Vec<AuthorMark>,
        action: SealAction,
    }
    #[derive(serde::Serialize)]
    struct ParentSeal {
        statement: ParentStatement,
        endorsement: rocky_credentials::Endorsement,
    }
    let owner = PrivateKey::new();
    let graph = graph();
    let statement = ParentStatement {
        format: 1,
        scope: graph.scope,
        tip: graph.genesis(),
        frontier: vec![],
        action: SealAction::Continue {
            schema: graph.genesis_schema,
        },
    };
    let endorsement = rocky_credentials::Endorsement::sign(
        &owner,
        &CredentialChain::default(),
        "rocky.seal.v1",
        &statement,
    )
    .unwrap();
    let bytes = encode(&ParentSeal {
        statement,
        endorsement,
    })
    .unwrap();
    assert!(SignedSeal::from_bytes(&bytes).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_control_only_trusted_identity_certifies_nodes_without_row_authority() {
    let graph = graph();
    let owner = PrivateKey::new();
    let identity = PrivateKey::new();
    let node = PrivateKey::new();
    let grant = Authority::owner_in(
        &owner,
        rocky_credentials::DatabaseContext {
            publisher: owner.public_key(),
            scope: Scope {
                mesh: MeshId([1; 16]),
                app: AppId([2; 16]),
                domain: DomainId([3; 16]),
            },
        },
    )
    .unwrap()
    .issue_trusted(identity.public_key(), graph.scope, 100)
    .unwrap();
    let chain = CredentialChain(vec![grant])
        .certify(&identity, &node)
        .unwrap();
    assert_eq!(
        chain
            .trusted_node(owner.public_key(), graph.scope, node.public_key(), None)
            .unwrap(),
        identity.public_key()
    );
    let (grants, partitions) = chain
        .verify_selectors(
            owner.public_key(),
            node.public_key(),
            &RevocationSet::new(),
            200,
        )
        .unwrap();
    assert!(grants.is_empty());
    assert!(partitions.is_empty());
}
fn link(
    graph: &TipGraph,
    signer: &PrivateKey,
    chain: &CredentialChain,
) -> Option<rocky_credentials::ChainLink> {
    let trusted = matches!(chain.0.as_slice(), [only] if only.capability.extended().is_ok_and(|e| e.trusted.is_some()));
    trusted.then(|| {
        graph
            .next_link(
                &signer.public_key(),
                &rocky_credentials::testing::lane_of(signer),
            )
            .unwrap()
    })
}

fn linked_move(
    graph: &TipGraph,
    name: &str,
    signer: &PrivateKey,
    chain: &CredentialChain,
    link: rocky_credentials::ChainLink,
) -> SignedBookmarkMove {
    SignedBookmarkMove::endorse_linked(
        BookmarkMoveStatement {
            format: BOOKMARK_MOVE_FORMAT,
            scope: graph.scope,
            name: name.into(),
            from: None,
            to: Some(graph.genesis()),
        },
        rocky_credentials::testing::lane_for(signer, chain),
        chain,
        Some(link),
    )
    .unwrap()
}
fn cutting(
    owner: &PrivateKey,
    identity: rocky_credentials::PublicKey,
    head: Option<&SignedBookmarkMove>,
) -> RevocationSet {
    let cut = head.map(|head| rocky_credentials::Cut {
        identity,
        lane: rocky_credentials::testing::lane_key(identity),
        statements: vec![rocky_credentials::Head {
            sequence: head.link.unwrap().sequence,
            id: head.id().unwrap(),
        }],
    });
    let statement = rocky_credentials::Revocation {
        app: graph().scope.app,
        issuer: owner.public_key(),
        time: 200,
        nodes: Default::default(),
        identities: [identity].into(),
        capabilities: Default::default(),
        cut: cut.into_iter().collect(),
        mode: rocky_credentials::RevocationMode::Lost,
    };
    RevocationSet::try_from(vec![
        Authority::owner_in(
            owner,
            rocky_credentials::DatabaseContext {
                publisher: owner.public_key(),
                scope: Scope {
                    mesh: MeshId([1; 16]),
                    app: AppId([2; 16]),
                    domain: DomainId([3; 16]),
                },
            },
        )
        .unwrap()
        .revoke(statement)
        .unwrap(),
    ])
    .unwrap()
}
fn ids(moves: &[&SignedBookmarkMove]) -> std::collections::BTreeSet<[u8; 32]> {
    moves
        .iter()
        .map(|statement| statement.id().unwrap())
        .collect()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_forked_chain_is_flagged_and_a_cut_keeps_only_the_named_branch() {
    let owner = PrivateKey::new();
    let trusted = PrivateKey::new();
    let chain = trusted_chain(&graph(), &owner, &trusted);
    let mut graph = graph();
    let first = rocky_credentials::ChainLink::first(rocky_credentials::testing::lane_of(&trusted));
    let base = linked_move(&graph, "base", &trusted, &chain, first);
    graph.moves.push(base.clone());
    let fork = first.next(base.id().unwrap());
    let kept = linked_move(&graph, "kept", &trusted, &chain, fork);
    let other = linked_move(&graph, "other", &trusted, &chain, fork);
    for statement in [&kept, &other] {
        statement
            .verify_historic(owner.public_key(), &graph)
            .unwrap();
    }
    graph.moves.extend([kept.clone(), other.clone()]);
    let after = linked_move(
        &graph,
        "after",
        &trusted,
        &chain,
        fork.next(other.id().unwrap()),
    );
    after.verify_historic(owner.public_key(), &graph).unwrap();
    graph.moves.push(after.clone());
    graph.exclude_revoked(&RevocationSet::new()).unwrap();
    assert!(
        graph.revoked.is_empty(),
        "two statements naming the same predecessor are a fork, not a revocation"
    );
    assert_eq!(
        graph.equivocations.iter().copied().collect::<Vec<_>>(),
        [rocky_credentials::Equivocation {
            identity: trusted.public_key(),
            lane: rocky_credentials::testing::lane_of(&trusted),
            sequence: 1,
        }]
    );

    let revocations = cutting(&owner, trusted.public_key(), Some(&kept));
    graph.exclude_revoked(&revocations).unwrap();
    assert_eq!(
        graph.revoked,
        ids(&[&other, &after]),
        "a cut at kept leaves the other branch and what follows it past the cut"
    );
    assert_eq!(
        graph
            .past_cut
            .iter()
            .map(|item| item.id)
            .collect::<std::collections::BTreeSet<_>>(),
        ids(&[&other, &after])
    );
    assert_eq!(graph.bookmark("kept").unwrap(), Some(graph.genesis()));
    assert_eq!(graph.bookmark("other").unwrap(), None);
    assert_eq!(
        graph.prunable(&revocations, &Default::default()).unwrap(),
        ids(&[&other, &after])
    );

    graph
        .exclude_revoked(&cutting(&owner, trusted.public_key(), None))
        .unwrap();
    assert_eq!(
        graph.revoked,
        ids(&[&base, &kept, &other, &after]),
        "without a cut, the revocation leaves out the whole chain"
    );
    assert!(graph.past_cut.is_empty());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_cut_refuses_what_follows_its_head_and_prunes_only_what_nothing_kept_needs() {
    let owner = PrivateKey::new();
    let trusted = PrivateKey::new();
    let chain = trusted_chain(&graph(), &owner, &trusted);
    let mut graph = graph();
    let first = rocky_credentials::ChainLink::first(rocky_credentials::testing::lane_of(&trusted));
    let base = linked_move(&graph, "base", &trusted, &chain, first);
    graph.moves.push(base.clone());
    let late = linked_move(
        &graph,
        "late",
        &trusted,
        &chain,
        first.next(base.id().unwrap()),
    );
    let revocations = cutting(&owner, trusted.public_key(), Some(&base));
    let place = |statement: &SignedBookmarkMove| {
        graph
            .beyond_cut(
                &revocations,
                &trusted.public_key(),
                &statement.link.unwrap(),
                statement.id().unwrap(),
            )
            .unwrap()
    };
    assert!(!place(&base), "the cut's head stays");
    assert!(place(&late));
    late.verify_current(owner.public_key(), &graph, &revocations, 300)
        .unwrap();
    graph.moves.push(late.clone());
    graph.exclude_revoked(&revocations).unwrap();
    assert_eq!(
        graph.prunable(&revocations, &Default::default()).unwrap(),
        ids(&[&late]),
        "the cut, not the revocation, judges a revoked identity's statement past the cut"
    );
    let next = late.link.unwrap().next(late.id().unwrap());
    let sealed = SignedSeal::endorse_linked(
        seal(&graph, graph.genesis(), 1, &owner).statement,
        rocky_credentials::testing::lane_for(&trusted, &chain),
        &chain,
        Some(next),
    )
    .unwrap();
    graph.seals.push(sealed.clone());
    let after = linked_move(
        &graph,
        "after",
        &trusted,
        &chain,
        next.next(sealed.id().unwrap()),
    );
    graph.moves.push(after.clone());
    graph.openings.push(opening(&graph, &sealed, &owner));
    assert_eq!(
        graph.prunable(&revocations, &Default::default()).unwrap(),
        ids(&[&after]),
        "a late seal the owner opened a tip on stays held, and so does its predecessor"
    );
    graph.remove_statements(&ids(&[&after])).unwrap();
    assert_eq!(graph.moves, [base, late]);
    assert_eq!(graph.seals, [sealed]);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_cut_binds_its_chain_to_the_identity_it_names() {
    let owner = PrivateKey::new();
    let (cut_identity, impostor) = (PrivateKey::new(), PrivateKey::new());
    let cut_chain = trusted_chain(&graph(), &owner, &cut_identity);
    let impostor_chain = trusted_chain(&graph(), &owner, &impostor);
    let mut graph = graph();
    let lane =
        rocky_credentials::ChainLink::first(rocky_credentials::testing::lane_of(&cut_identity));
    let head = linked_move(&graph, "head", &cut_identity, &cut_chain, lane);
    graph.moves.push(head.clone());
    let keep_first_chain_end_impostor = rocky_credentials::Revocation {
        app: graph.scope.app,
        issuer: owner.public_key(),
        time: 200,
        nodes: Default::default(),
        identities: [cut_identity.public_key(), impostor.public_key()].into(),
        capabilities: Default::default(),
        cut: vec![rocky_credentials::Cut {
            identity: cut_identity.public_key(),
            lane: rocky_credentials::testing::lane_of(&cut_identity),
            statements: vec![rocky_credentials::Head {
                sequence: 0,
                id: head.id().unwrap(),
            }],
        }],
        mode: rocky_credentials::RevocationMode::Lost,
    };
    let revocations = RevocationSet::try_from(vec![
        Authority::owner_in(
            &owner,
            rocky_credentials::DatabaseContext {
                publisher: owner.public_key(),
                scope: Scope {
                    mesh: MeshId([1; 16]),
                    app: AppId([2; 16]),
                    domain: DomainId([3; 16]),
                },
            },
        )
        .unwrap()
        .revoke(keep_first_chain_end_impostor)
        .unwrap(),
    ])
    .unwrap();
    assert!(
        SignedBookmarkMove::endorse_linked(
            head.statement.clone(),
            rocky_credentials::testing::lane_for(&impostor, &impostor_chain),
            &impostor_chain,
            Some(lane),
        )
        .is_err(),
        "the impostor cannot sign on the cut identity's lane; a lane is its signing node"
    );
    let forged = linked_move(
        &graph,
        "forged",
        &impostor,
        &impostor_chain,
        rocky_credentials::ChainLink::first(rocky_credentials::testing::lane_of(&impostor)),
    );
    assert!(
        forged
            .verify_current(owner.public_key(), &graph, &revocations, 300)
            .is_err(),
        "on its own lane, the impostor's revocation refuses it"
    );
    graph.moves.push(forged.clone());
    graph.exclude_revoked(&revocations).unwrap();
    assert!(
        graph.revoked.contains(&forged.id().unwrap()),
        "were it held, it is the impostor's revoked chain, not past the cut"
    );
    assert!(graph.past_cut.is_empty());
    assert!(graph.equivocations.is_empty());
    assert!(!graph.revoked.contains(&head.id().unwrap()));
    let unrevoked = rocky_credentials::Revocation {
        app: graph.scope.app,
        issuer: owner.public_key(),
        time: 200,
        nodes: Default::default(),
        identities: [impostor.public_key()].into(),
        capabilities: Default::default(),
        cut: vec![rocky_credentials::Cut {
            identity: cut_identity.public_key(),
            lane: rocky_credentials::testing::lane_of(&cut_identity),
            statements: Vec::new(),
        }],
        mode: rocky_credentials::RevocationMode::Lost,
    };
    assert!(
        Authority::owner_in(
            &owner,
            rocky_credentials::DatabaseContext {
                publisher: owner.public_key(),
                scope: Scope {
                    mesh: MeshId([1; 16]),
                    app: AppId([2; 16]),
                    domain: DomainId([3; 16])
                }
            }
        )
        .unwrap()
        .revoke(unrevoked)
        .is_err(),
        "a cut may only name identities or nodes its statement revokes"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_chained_statement_needs_its_predecessor_and_the_trusted_link() {
    let owner = PrivateKey::new();
    let trusted = PrivateKey::new();
    let chain = trusted_chain(&graph(), &owner, &trusted);
    let mut graph = graph();
    let first = rocky_credentials::ChainLink::first(rocky_credentials::testing::lane_of(&trusted));
    let base = linked_move(&graph, "base", &trusted, &chain, first);
    let ahead = linked_move(
        &graph,
        "ahead",
        &trusted,
        &chain,
        first.next(base.id().unwrap()),
    );
    let missing = ahead
        .verify_historic(owner.public_key(), &graph)
        .unwrap_err();
    assert!(
        missing.to_string().contains("predecessor"),
        "a later link whose predecessor the graph does not hold is refused: {missing}"
    );
    graph.moves.push(base.clone());
    ahead.verify_historic(owner.public_key(), &graph).unwrap();
    let wrong = linked_move(
        &graph,
        "wrong",
        &trusted,
        &chain,
        rocky_credentials::ChainLink {
            previous: Some([7; 32]),
            stage: None,
            ..first.next(base.id().unwrap())
        },
    );
    assert!(
        wrong.verify_historic(owner.public_key(), &graph).is_err(),
        "a link naming another predecessor at that place is refused"
    );
    let unlinked = SignedBookmarkMove::endorse_linked(
        base.statement.clone(),
        rocky_credentials::testing::lane_for(&trusted, &chain),
        &chain,
        None,
    )
    .unwrap();
    assert!(
        unlinked
            .verify_historic(owner.public_key(), &graph)
            .is_err(),
        "a trusted statement without a link is refused"
    );
    let issuer = PrivateKey::new();
    let linked_issuer = SignedBookmarkMove::endorse_linked(
        base.statement.clone(),
        rocky_credentials::testing::lane_for(&issuer, &issuer_chain(&owner, &issuer)),
        &issuer_chain(&owner, &issuer),
        Some(rocky_credentials::ChainLink::first(
            rocky_credentials::testing::lane_of(&issuer),
        )),
    )
    .unwrap();
    assert!(
        linked_issuer
            .verify_historic(owner.public_key(), &graph)
            .is_err(),
        "an issuer's statement with a link is refused"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_stage_branches_off_the_live_chain_and_a_cut_keeps_the_branches_it_names() {
    let owner = PrivateKey::new();
    let trusted = PrivateKey::new();
    let chain = trusted_chain(&graph(), &owner, &trusted);
    let mut graph = graph();
    let first = rocky_credentials::ChainLink::first(rocky_credentials::testing::lane_of(&trusted));
    let base = linked_move(&graph, "base", &trusted, &chain, first);
    graph.moves.push(base.clone());
    let after = first.next(base.id().unwrap());
    let (stage_seal, staged) = stage(&graph, &trusted, &chain, after);
    let live = linked_move(&graph, "live", &trusted, &chain, after);
    stage_seal
        .verify_historic(owner.public_key(), &graph)
        .unwrap();
    graph.seals.push(stage_seal.clone());
    for statement in [&staged, &live] {
        statement
            .verify_historic(owner.public_key(), &graph)
            .unwrap();
        graph.moves.push(statement.clone());
    }
    graph.exclude_revoked(&RevocationSet::new()).unwrap();
    assert!(
        graph.equivocations.is_empty(),
        "a stage and the live chain at the same sequence are two branches, no fork"
    );
    assert_eq!(
        graph
            .next_link(
                &trusted.public_key(),
                &rocky_credentials::testing::lane_of(&trusted)
            )
            .unwrap()
            .sequence,
        3,
        "the live chain goes on past every branch, so no place repeats"
    );
    let heads = graph
        .heads(
            &trusted.public_key(),
            &rocky_credentials::testing::lane_of(&trusted),
        )
        .unwrap();
    assert_eq!(
        heads.statements.len(),
        2,
        "the proposer's cut names both heads and keeps both"
    );
    let app = graph.scope.app;
    let keep = |cut: rocky_credentials::Cut| {
        let statement = rocky_credentials::Revocation {
            app,
            issuer: owner.public_key(),
            time: 200,
            nodes: Default::default(),
            identities: [trusted.public_key()].into(),
            capabilities: Default::default(),
            cut: vec![cut],
            mode: rocky_credentials::RevocationMode::Lost,
        };
        RevocationSet::try_from(vec![
            Authority::owner_in(
                &owner,
                rocky_credentials::DatabaseContext {
                    publisher: owner.public_key(),
                    scope: Scope {
                        mesh: MeshId([1; 16]),
                        app: AppId([2; 16]),
                        domain: DomainId([3; 16]),
                    },
                },
            )
            .unwrap()
            .revoke(statement)
            .unwrap(),
        ])
        .unwrap()
    };
    graph.exclude_revoked(&keep(heads.clone())).unwrap();
    assert!(graph.revoked.is_empty());
    let live_only = rocky_credentials::Cut {
        statements: vec![rocky_credentials::Head {
            sequence: 1,
            id: live.id().unwrap(),
        }],
        ..heads
    };
    graph.exclude_revoked(&keep(live_only)).unwrap();
    let mut left_out = ids(&[&staged]);
    left_out.insert(stage_seal.id().unwrap());
    assert_eq!(
        graph.revoked, left_out,
        "naming only the live head leaves the stage branch out"
    );
}

fn stage(
    graph: &TipGraph,
    trusted: &PrivateKey,
    chain: &CredentialChain,
    link: rocky_credentials::ChainLink,
) -> (SignedSeal, SignedBookmarkMove) {
    let mut statement = seal(graph, graph.genesis(), 1, &PrivateKey::new()).statement;
    statement.action = rocky_credentials::SealAction::Continue { schema: [8; 32] };
    let opened = link.branch(&statement).unwrap();
    let sealed = SignedSeal::endorse_linked(
        statement,
        rocky_credentials::testing::lane_for(trusted, chain),
        chain,
        Some(opened),
    )
    .unwrap();
    let staged = linked_move(
        graph,
        "staged",
        trusted,
        chain,
        opened.next(sealed.id().unwrap()),
    );
    (sealed, staged)
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_stage_branch_opens_only_with_the_schema_seal_its_tag_names() {
    let owner = PrivateKey::new();
    let trusted = PrivateKey::new();
    let chain = trusted_chain(&graph(), &owner, &trusted);
    let mut graph = graph();
    let first = rocky_credentials::ChainLink::first(rocky_credentials::testing::lane_of(&trusted));
    let base = linked_move(&graph, "base", &trusted, &chain, first);
    graph.moves.push(base.clone());
    let after = first.next(base.id().unwrap());
    let (stage_seal, staged) = stage(&graph, &trusted, &chain, after);
    let forged = rocky_credentials::ChainLink {
        stage: Some([5; 32]),
        ..after
    };
    let fork = linked_move(&graph, "fork", &trusted, &chain, forged);
    assert!(
        fork.verify_historic(owner.public_key(), &graph).is_err(),
        "a move the fresh tag names no seal of is a fork"
    );
    let mut other = stage_seal.statement.clone();
    other.action = rocky_credentials::SealAction::Continue { schema: [7; 32] };
    let borrowed = SignedSeal::endorse_linked(
        other,
        rocky_credentials::testing::lane_for(&trusted, &chain),
        &chain,
        stage_seal.link,
    )
    .unwrap();
    assert!(
        borrowed
            .verify_historic(owner.public_key(), &graph)
            .is_err(),
        "a seal cannot carry another seal's tag"
    );
    let opening_move = linked_move(&graph, "open", &trusted, &chain, stage_seal.link.unwrap());
    assert!(
        opening_move
            .verify_historic(owner.public_key(), &graph)
            .is_err(),
        "a move cannot open a stage branch"
    );
    stage_seal
        .verify_historic(owner.public_key(), &graph)
        .unwrap();
    graph.seals.push(stage_seal.clone());
    staged.verify_historic(owner.public_key(), &graph).unwrap();
    graph.moves.push(staged.clone());
    let twin = linked_move(&graph, "twin", &trusted, &chain, staged.link.unwrap());
    twin.verify_historic(owner.public_key(), &graph).unwrap();
    graph.moves.push(twin);
    graph.exclude_revoked(&RevocationSet::new()).unwrap();
    assert_eq!(
        graph.equivocations.len(),
        1,
        "two statements at one place of the branch are a fork"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_full_cut_leaves_out_the_oldest_stage_branches_and_refuses_too_many_forks() {
    let owner = PrivateKey::new();
    let trusted = PrivateKey::new();
    let chain = trusted_chain(&graph(), &owner, &trusted);
    let lanes = [rocky_credentials::testing::lane_of(&trusted)].into();
    let mut graph = graph();
    let mut link =
        rocky_credentials::ChainLink::first(rocky_credentials::testing::lane_of(&trusted));
    let mut stages = Vec::new();
    for n in 0..70u64 {
        let live = linked_move(&graph, &format!("live{n}"), &trusted, &chain, link);
        graph.moves.push(live.clone());
        link = link.next(live.id().unwrap());
        if n < 69 {
            let mut statement = seal(&graph, graph.genesis(), n, &owner).statement;
            statement.action = rocky_credentials::SealAction::Continue { schema: [8; 32] };
            let branch = link.branch(&statement).unwrap();
            let sealed = SignedSeal::endorse_linked(
                statement,
                rocky_credentials::testing::lane_for(&trusted, &chain),
                &chain,
                Some(branch),
            )
            .unwrap();
            graph.seals.push(sealed.clone());
            stages.push(rocky_credentials::Head {
                sequence: branch.sequence,
                id: sealed.id().unwrap(),
            });
        }
    }
    let (cut, dropped) = graph.proposed_cut(&trusted.public_key(), &lanes).unwrap();
    let [cut] = cut.as_slice() else {
        panic!("one chain")
    };
    assert_eq!(
        cut.statements.len(),
        rocky_credentials::MAX_CUT_HEADS,
        "69 stage branches off a live chain of 70 moves fill the cut's heads"
    );
    assert_eq!(
        dropped,
        stages[..6]
            .iter()
            .map(|head| (rocky_credentials::testing::lane_of(&trusted), *head))
            .collect::<Vec<_>>(),
        "the six oldest stages go; the live head and the newest stages stay"
    );
    assert!(stages[6..].iter().all(|head| cut.statements.contains(head)));
    assert!(cut.statements.iter().any(|head| head.sequence == 69));

    let mut forked = self::graph();
    let first = rocky_credentials::ChainLink::first(rocky_credentials::testing::lane_of(&trusted));
    let base = linked_move(&forked, "base", &trusted, &chain, first);
    forked.moves.push(base.clone());
    for n in 0..65 {
        let fork = linked_move(
            &forked,
            &format!("fork{n}"),
            &trusted,
            &chain,
            first.next(base.id().unwrap()),
        );
        forked.moves.push(fork);
    }
    assert!(
        forked.proposed_cut(&trusted.public_key(), &lanes).is_err(),
        "live forks are never left out: 65 of them refuse the cut"
    );
}
