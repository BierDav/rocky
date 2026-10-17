use super::*;
use rocky_credentials::{GENESIS_LINE_FORMAT, GenesisLineStatement, SignedGenesisLine};

fn minimum(graph: &mut TipGraph, owner: &PrivateKey, value: u32) {
    graph.genesis_lines.push(
        SignedGenesisLine::sign(
            GenesisLineStatement {
                format: GENESIS_LINE_FORMAT,
                scope: graph.scope,
                minimum_in_sync: value,
            },
            owner,
        )
        .unwrap(),
    );
}

fn migration(
    graph: &TipGraph,
    owner: &PrivateKey,
    signers: [&PrivateKey; 3],
    schema: [u8; 32],
    sequence: u64,
) -> (SignedSeal, SignedTipOpening, SignedBookmarkMove) {
    let mut statement = seal(graph, graph.genesis(), sequence, owner).statement;
    statement.action = SealAction::Close;
    let closing =
        SignedSeal::endorse(statement, signers[0], &issuer_chain(owner, signers[0])).unwrap();
    let opened = SignedTipOpening::endorse(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            scope: graph.scope,
            seal: closing.id().unwrap(),
            schema,
            minimum_in_sync: 2,
        },
        signers[1],
        &issuer_chain(owner, signers[1]),
    )
    .unwrap();
    let moved = SignedBookmarkMove::endorse(
        movement(
            graph,
            "main",
            Some(graph.genesis()),
            Some(opened.tip().unwrap()),
            owner,
        )
        .statement,
        signers[2],
        &issuer_chain(owner, signers[2]),
    )
    .unwrap();
    (closing, opened, moved)
}

fn append(graph: &mut TipGraph, records: &(SignedSeal, SignedTipOpening, SignedBookmarkMove)) {
    graph.seals.push(records.0.clone());
    graph.openings.push(records.1.clone());
    graph.moves.push(records.2.clone());
}

fn verify(graph: &TipGraph, owner: &PrivateKey) {
    for seal in &graph.seals {
        SignedSeal::from_bytes(&seal.to_bytes().unwrap())
            .unwrap()
            .verify(owner.public_key(), graph)
            .unwrap();
    }
    for opening in &graph.openings {
        SignedTipOpening::from_bytes(&opening.to_bytes().unwrap())
            .unwrap()
            .verify(owner.public_key(), graph)
            .unwrap();
    }
    for movement in &graph.moves {
        SignedBookmarkMove::from_bytes(&movement.to_bytes().unwrap())
            .unwrap()
            .verify(owner.public_key(), graph)
            .unwrap();
    }
}

fn main(graph: &mut TipGraph, owner: &PrivateKey) {
    minimum(graph, owner, 2);
    graph
        .moves
        .push(movement(graph, "main", None, Some(graph.genesis()), owner));
}

#[test]
fn concurrent_migration_history_remains_authenticated_in_every_dependency_order() {
    let owner = PrivateKey::new();
    let (a, b) = (PrivateKey::new(), PrivateKey::new());
    let mut base = graph();
    main(&mut base, &owner);
    let first = migration(&base, &owner, [&a; 3], [5; 32], 1);
    let second = migration(&base, &owner, [&b; 3], [6; 32], 2);
    let winner = if first.0.id().unwrap() < second.0.id().unwrap() {
        &first
    } else {
        &second
    };
    for reverse in [false, true] {
        let mut graph = base.clone();
        for records in if reverse {
            [&second, &first]
        } else {
            [&first, &second]
        } {
            append(&mut graph, records);
            verify(&graph, &owner);
        }
        assert_eq!(
            graph.bookmark("main").unwrap(),
            Some(winner.1.tip().unwrap())
        );
        assert_eq!(
            graph.schema(winner.1.tip().unwrap()).unwrap(),
            winner.1.statement.schema
        );
        assert_eq!(graph.losing_moves("main").unwrap().len(), 1);
        assert!(
            !graph
                .continues_any(graph.genesis(), winner.1.tip().unwrap())
                .unwrap()
        );
        assert!(
            graph
                .succeeds(graph.genesis(), winner.1.tip().unwrap())
                .unwrap()
        );
        for records in [&first, &second] {
            assert!(
                graph
                    .succeeds_any(graph.genesis(), records.1.tip().unwrap())
                    .unwrap()
            );
        }
    }
    // A graph union is independent of the order of seals, openings and moves.
    let mut together = base;
    together
        .openings
        .extend([second.1.clone(), first.1.clone()]);
    together.moves.extend([first.2.clone(), second.2.clone()]);
    together.seals.extend([second.0.clone(), first.0.clone()]);
    verify(&together, &owner);
    assert_eq!(
        together.bookmark("main").unwrap(),
        Some(winner.1.tip().unwrap())
    );
}

#[test]
fn two_migration_openings_on_one_close_resolve_by_move_digest() {
    let owner = PrivateKey::new();
    let issuer = PrivateKey::new();
    let mut graph = graph();
    main(&mut graph, &owner);
    let first = migration(&graph, &owner, [&issuer; 3], [5; 32], 1);
    let mut second = migration(&graph, &owner, [&issuer; 3], [6; 32], 1);
    second.0 = first.0.clone();
    append(&mut graph, &first);
    graph.openings.push(second.1.clone());
    graph.moves.push(second.2.clone());
    verify(&graph, &owner);
    let winner = if first.2.id().unwrap() < second.2.id().unwrap() {
        &first
    } else {
        &second
    };
    assert_eq!(
        graph.bookmark("main").unwrap(),
        Some(winner.1.tip().unwrap())
    );
    assert_eq!(graph.losing_moves("main").unwrap().len(), 1);
}

#[test]
fn larger_genesis_minimum_changes_eligibility_without_invalidating_history() {
    let owner = PrivateKey::new();
    let issuer = PrivateKey::new();
    let mut graph = graph();
    main(&mut graph, &owner);
    let records = migration(&graph, &owner, [&issuer; 3], [5; 32], 1);
    append(&mut graph, &records);
    let tip = records.1.tip().unwrap();
    assert_eq!(graph.bookmark("main").unwrap(), Some(tip));
    minimum(&mut graph, &owner, 3);
    verify(&graph, &owner);
    assert_eq!(graph.schema_any(tip).unwrap(), [5; 32]);
    assert!(graph.schema(tip).is_err());
    assert!(graph.succeeds_any(graph.genesis(), tip).unwrap());
    assert_eq!(graph.bookmark("main").unwrap(), Some(graph.genesis()));
}

#[test]
fn revoking_each_independent_migration_signer_preserves_historical_authentication() {
    for role in 0..3 {
        let owner = PrivateKey::new();
        let first_signers = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
        let second_signers = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
        let mut graph = graph();
        main(&mut graph, &owner);
        let first = migration(
            &graph,
            &owner,
            [&first_signers[0], &first_signers[1], &first_signers[2]],
            [5; 32],
            1,
        );
        let second = migration(
            &graph,
            &owner,
            [&second_signers[0], &second_signers[1], &second_signers[2]],
            [6; 32],
            2,
        );
        let (winner, loser, signers) = if first.0.id().unwrap() < second.0.id().unwrap() {
            (&first, &second, &first_signers)
        } else {
            (&second, &first, &second_signers)
        };
        append(&mut graph, &first);
        append(&mut graph, &second);
        assert_eq!(
            graph.bookmark("main").unwrap(),
            Some(winner.1.tip().unwrap())
        );
        graph
            .exclude_revoked(&revoking(&owner, &[signers[role].public_key()]))
            .unwrap();
        verify(&graph, &owner);
        assert_eq!(
            graph.schema_any(winner.1.tip().unwrap()).unwrap(),
            winner.1.statement.schema
        );
        let expected = if role == 0 {
            loser.1.tip().unwrap()
        } else {
            graph.genesis()
        };
        assert_eq!(graph.bookmark("main").unwrap(), Some(expected));
        assert!(!graph.losing_moves("main").unwrap().is_empty());
    }
}

#[test]
fn a_redirected_continuation_noop_never_blocks_a_migration_move() {
    let owner = PrivateKey::new();
    let issuer = PrivateKey::new();
    let mut graph = graph();
    main(&mut graph, &owner);
    // The owner's close beats the issuer's continue. Its continuation move
    // redirects to genesis, and therefore must be listed as a loser.
    let mut records = migration(&graph, &owner, [&issuer; 3], [5; 32], 1);
    records.0 = SignedSeal::sign(records.0.statement.clone(), &owner).unwrap();
    records.1 = SignedTipOpening::endorse(
        TipOpeningStatement {
            seal: records.0.id().unwrap(),
            ..records.1.statement
        },
        &issuer,
        &issuer_chain(&owner, &issuer),
    )
    .unwrap();
    records.2 = SignedBookmarkMove::endorse(
        movement(
            &graph,
            "main",
            Some(graph.genesis()),
            Some(records.1.tip().unwrap()),
            &owner,
        )
        .statement,
        &issuer,
        &issuer_chain(&owner, &issuer),
    )
    .unwrap();
    let continued = endorsed(
        &graph,
        &owner,
        graph.genesis(),
        3,
        &issuer,
        &issuer_chain(&owner, &issuer),
    );
    let moved = movement(
        &graph,
        "main",
        Some(graph.genesis()),
        continued.successor().unwrap(),
        &owner,
    );
    graph.seals.push(continued.clone());
    graph.moves.push(moved.clone());
    append(&mut graph, &records);
    verify(&graph, &owner);
    assert_eq!(
        graph
            .canonical_tip(continued.successor().unwrap().unwrap())
            .unwrap(),
        graph.genesis()
    );
    assert_eq!(
        graph.bookmark("main").unwrap(),
        Some(records.1.tip().unwrap())
    );
    assert!(graph.losing_moves("main").unwrap().contains(&&moved));
    assert_eq!(
        graph.bookmark_path("main").unwrap(),
        vec![(graph.genesis(), records.1.tip().unwrap())]
    );
}

#[test]
fn issuer_migration_openings_refuse_other_edges_and_unsupported_minima() {
    let owner = PrivateKey::new();
    let issuer = PrivateKey::new();
    let mut graph = graph();
    main(&mut graph, &owner);
    let records = migration(&graph, &owner, [&issuer; 3], [5; 32], 1);
    graph.seals.push(records.0.clone());
    let continued = seal(&graph, graph.genesis(), 2, &owner);
    graph.seals.push(continued.clone());
    for statement in [
        TipOpeningStatement {
            schema: graph.genesis_schema,
            ..records.1.statement.clone()
        },
        TipOpeningStatement {
            minimum_in_sync: 1,
            ..records.1.statement.clone()
        },
        TipOpeningStatement {
            seal: continued.id().unwrap(),
            ..records.1.statement.clone()
        },
        TipOpeningStatement {
            seal: [9; 32],
            ..records.1.statement.clone()
        },
        TipOpeningStatement {
            scope: Scope {
                domain: DomainId([9; 16]),
                ..graph.scope
            },
            ..records.1.statement.clone()
        },
    ] {
        assert!(
            SignedTipOpening::endorse(statement, &issuer, &issuer_chain(&owner, &issuer))
                .unwrap()
                .verify(owner.public_key(), &graph)
                .is_err()
        );
    }
    records.1.verify(owner.public_key(), &graph).unwrap();
    let mut old = records.1.clone();
    old.statement.format = 1;
    assert!(SignedTipOpening::from_bytes(&old.to_bytes().unwrap()).is_err());
}

#[test]
fn schemas_follow_the_requested_tip_and_validate_all_its_ancestry() {
    let owner = PrivateKey::new();
    let mut graph = graph();
    let mut first = seal(&graph, graph.genesis(), 1, &owner).statement;
    first.action = SealAction::Continue { schema: [5; 32] };
    let first = SignedSeal::sign(first, &owner).unwrap();
    let tip = first.successor().unwrap().unwrap();
    graph.seals.push(first.clone());
    assert_eq!(graph.schema(tip).unwrap(), [5; 32]);
    let opened = SignedTipOpening::sign(
        TipOpeningStatement {
            schema: [6; 32],
            ..opening(&graph, &first, &owner).statement
        },
        &owner,
    )
    .unwrap();
    let branch = opened.tip().unwrap();
    graph.openings.push(opened);
    let next = seal(&graph, branch, 1, &owner);
    graph.seals.push(next.clone());
    assert_eq!(graph.schema(branch).unwrap(), [6; 32]);
    assert_eq!(
        graph.schema(next.successor().unwrap().unwrap()).unwrap(),
        graph.genesis_schema
    );
    let losing = PrivateKey::new();
    let rival = SignedSeal::endorse(
        first.statement.clone(),
        &losing,
        &issuer_chain(&owner, &losing),
    )
    .unwrap();
    // Replacing the owner seal leaves all descendants structurally readable.
    graph.seals = vec![rival, next.clone()];
    graph.seals.push(first.clone());
    let mut revoked = std::collections::BTreeSet::new();
    revoked.insert(first.id().unwrap());
    graph.revoked = revoked;
    assert!(graph.schema(next.successor().unwrap().unwrap()).is_err());
    assert_eq!(
        graph
            .schema_any(next.successor().unwrap().unwrap())
            .unwrap(),
        graph.genesis_schema
    );
}

#[test]
fn migration_descriptors_require_the_exact_opener_and_certified_initial_writer() {
    use rocky_core::MigrationPublicationStatement;
    use rocky_credentials::{SignedMigrationPublication, device_chain};
    let (owner, identity, node) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let mut g = graph();
    main(&mut g, &owner);
    let chain = trusted_chain(&g, &owner, &identity);
    let mut close = seal(&g, g.genesis(), 0, &owner).statement;
    close.action = SealAction::Close;
    let close = SignedSeal::sign(close, &owner).unwrap();
    g.seals.push(close.clone());
    let opening = SignedTipOpening::endorse(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            scope: g.scope,
            seal: close.id().unwrap(),
            schema: [9; 32],
            minimum_in_sync: 2,
        },
        &identity,
        &chain,
    )
    .unwrap();
    let tip = opening.tip().unwrap();
    g.openings.push(opening.clone());
    let writer = device_chain(chain.0[0].clone(), &identity, node.public_key(), 5).unwrap();
    let mut statement = MigrationPublicationStatement {
        format: 1,
        scope: g.scope,
        tip,
        opening: opening.id().unwrap(),
        parent: close.id().unwrap(),
        schema: [9; 32],
        publication: rocky_core::commitment("rocky.sdk.migration.v3", &tip.0),
        writer: *node.public_key().as_bytes(),
        author: 5,
        frames: 0,
        bytes: 0,
        root: [0; 32],
    };
    statement.root = statement.empty().hash;
    let sign =
        |s| SignedMigrationPublication::endorse(s, &identity, &chain, writer.clone()).unwrap();
    let signed = sign(statement.clone());
    signed
        .verify_active(owner.public_key(), &g, &RevocationSet::new(), 200)
        .unwrap();
    assert_eq!(
        SignedMigrationPublication::from_bytes(&signed.to_bytes().unwrap()).unwrap(),
        signed
    );
    for field in 0..7 {
        let mut altered = statement.clone();
        match field {
            0 => altered.opening = [88; 32],
            1 => altered.parent = [88; 32],
            2 => altered.schema = [88; 32],
            3 => altered.writer = *PrivateKey::new().public_key().as_bytes(),
            4 => altered.author = 6,
            5 => altered.scope.domain = DomainId([88; 16]),
            _ => {
                altered.tip = TipId([88; 32]);
                altered.publication =
                    rocky_core::commitment("rocky.sdk.migration.v3", &altered.tip.0);
            }
        }
        altered.root = altered.empty().hash;
        assert!(
            sign(altered).verify(owner.public_key(), &g, None).is_err(),
            "field {field}"
        );
    }
    let certificate = serde_json::to_value(&signed.writer).unwrap();
    assert!(certificate.get("grants").is_none() && certificate.get("partitions").is_none());
    for format in [0, 2] {
        let mut unsupported = signed.clone();
        unsupported.statement.format = format;
        assert!(SignedMigrationPublication::from_bytes(&unsupported.to_bytes().unwrap()).is_err());
    }
    let other = PrivateKey::new();
    let other_chain = issuer_chain(&owner, &other);
    let wrong = SignedMigrationPublication::endorse(
        statement.clone(),
        &other,
        &other_chain,
        writer.clone(),
    )
    .unwrap();
    assert!(wrong.verify(owner.public_key(), &g, None).is_err());
    let mut forged = signed.clone();
    forged.statement.schema = [88; 32];
    forged.statement.root = forged.statement.empty().hash;
    assert!(forged.verify(owner.public_key(), &g, None).is_err());
    // Current revocation stops acquisition, historical provenance remains auditable.
    let revoked = revoking(&owner, &[identity.public_key()]);
    assert!(
        signed
            .verify_active(owner.public_key(), &g, &revoked, 200)
            .is_err()
    );
    signed.verify(owner.public_key(), &g, None).unwrap();
    // An ordinary opening on a continuation cannot acquire a bulk account.
    g.seals[0].statement.action = SealAction::Continue {
        schema: g.genesis_schema,
    };
    assert!(signed.verify(owner.public_key(), &g, None).is_err());
}
