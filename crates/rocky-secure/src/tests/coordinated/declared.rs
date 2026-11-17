use super::seals::{bookmark, seal, share};
use super::*;
use crate::TipStatement;
use rocky_credentials::{
    AuthorMark, Coordination, DATABASE_SETTINGS_FORMAT, DatabaseSettingsStatement,
    GENESIS_LINE_FORMAT, GenesisLineStatement, SignedDatabaseSettings, SignedGenesisLine,
};

pub(super) fn refusal_code(error: &Error) -> Option<&'static str> {
    match error {
        Error::Application(inner) => inner.downcast_ref::<crate::Refusal>().map(|r| r.code),
        _ => None,
    }
}
pub(super) fn settings(
    lineage: &Lineage,
    revision: u64,
    coordination: Coordination,
) -> TipStatement {
    TipStatement::Settings(
        SignedDatabaseSettings::sign(
            DatabaseSettingsStatement {
                format: DATABASE_SETTINGS_FORMAT,
                scope: lineage.domain,
                revision,
                coordination,
            },
            lineage.publisher(),
        )
        .unwrap(),
    )
}
pub(super) fn coordinated_by(lineage: &Lineage, revision: u64, log: &MemoryLog) -> TipStatement {
    settings(
        lineage,
        revision,
        Coordination::Coordinated {
            bundle: log.bundle(),
        },
    )
}
pub(super) async fn follow_main_uncomposed(fixture: &Fixture) {
    let (alice, bob) = (&fixture.alice.1, &fixture.bob.1);
    let line = TipStatement::GenesisLine(
        SignedGenesisLine::sign(
            GenesisLineStatement {
                format: GENESIS_LINE_FORMAT,
                scope: fixture.lineage.domain,
                minimum_in_sync: 1,
            },
            fixture.lineage.publisher(),
        )
        .unwrap(),
    );
    let main = bookmark(
        &fixture.lineage,
        "main",
        None,
        Some(tip_log(&fixture.lineage).tip),
    );
    alice.publish_tip_statements(&[main, line]).await.unwrap();
    share(alice, bob).await;
    for node in [alice, bob] {
        node.follow_bookmark(fixture.lineage.domain, "main")
            .await
            .unwrap();
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn without_settings_naming_a_coordinator_a_node_without_a_log_seals_a_line_with_a_minimum() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let bob = &fixture.bob.1;
    let tip = tip_log(&fixture.lineage).tip;
    let sealed = seal(&fixture.lineage, bob, tip).await;
    bob.publish_tip_statements(&[TipStatement::Seal(sealed.clone())])
        .await
        .unwrap();
    assert_eq!(
        bob.tip_graph(fixture.lineage.domain)
            .await
            .unwrap()
            .seal(tip)
            .unwrap(),
        Some(&sealed),
        "an uncoordinated domain keeps sealing and migrating without a coordinator"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_node_without_the_named_coordinators_log_refuses_to_seal() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let (alice, bob) = (&fixture.alice.1, &fixture.bob.1);
    alice
        .publish_tip_statements(&[coordinated_by(&fixture.lineage, 1, &fixture.mesh.log)])
        .await
        .unwrap();
    share(alice, bob).await;
    assert_eq!(
        bob.tip_graph(fixture.lineage.domain)
            .await
            .unwrap()
            .coordinator()
            .unwrap()
            .map(|bundle| bundle.identity),
        Some(fixture.mesh.log.bundle().identity),
        "the owner's settings reach every device by sync"
    );
    let tip = tip_log(&fixture.lineage).tip;
    let next = seal(&fixture.lineage, bob, tip).await;
    let successor = next.successor().unwrap();
    let refused = bob
        .publish_tip_statements(&[
            TipStatement::Seal(next),
            bookmark(&fixture.lineage, "main", Some(tip), successor),
        ])
        .await
        .unwrap_err();
    assert_eq!(refusal_code(&refused), Some("coordinator_required"));
    let graph = bob.tip_graph(fixture.lineage.domain).await.unwrap();
    assert_eq!(graph.seal(tip).unwrap(), None, "nothing was sealed");
    assert_eq!(graph.bookmark("main").unwrap(), Some(tip), "main stays");
    assert_eq!(bob.followed_tip().await, tip, "bob still follows genesis");

    let other = MemoryLog::new();
    other.configure(fixture.lineage.domain, 1).unwrap();
    bob.set_domain_log(Some(Arc::new(other.client(fixture.bob.0.public_key()))));
    for attempt in 0..2 {
        let mismatched = bob
            .publish_tip_statements(&[
                TipStatement::Seal(seal(&fixture.lineage, bob, tip).await),
                bookmark(&fixture.lineage, "main", Some(tip), successor),
            ])
            .await
            .unwrap_err();
        assert_eq!(
            refusal_code(&mismatched),
            Some("coordinator_mismatch"),
            "attempt {attempt} names the real cause, never a retryable wait"
        );
    }
    let journaled = crate::storage::rows(
        &bob.inner.lock(crate::replica::Class::Background).await.conn,
        "SELECT count(*) FROM secure_pending_tips WHERE own=1",
    )
    .await
    .unwrap();
    assert_eq!(
        journaled,
        vec![vec![SqlValue::Integer(0)]],
        "a refused batch is never left journaled"
    );
    assert!(
        other
            .head(tip_log(&fixture.lineage))
            .unwrap()
            .terminal
            .is_none(),
        "another coordinator's log is never ended"
    );

    super::seals::compose(&fixture, &fixture.bob);
    let sealed = seal(&fixture.lineage, bob, tip).await;
    bob.publish_tip_statements(&[TipStatement::Seal(sealed.clone())])
        .await
        .unwrap();
    assert_eq!(
        fixture
            .mesh
            .log
            .head(tip_log(&fixture.lineage))
            .unwrap()
            .terminal
            .map(|terminal| terminal.digest),
        Some(sealed.id().unwrap()),
        "with its log composed, bob seals through it"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn seals_made_without_a_log_end_it_once_composed_and_their_finality_carries_them() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    let tip = tip_log(&fixture.lineage).tip;
    super::seals::compose(&fixture, alice);
    fixture.mesh.log.set_available(false);
    let mut rival = seal(&fixture.lineage, &alice.1, tip).await.statement;
    rival.frontier.push(AuthorMark {
        author: rocky_core::WriterId::from_bytes([0xee; 32]),
        sequence: 1,
    });
    let rival = rocky_credentials::SignedSeal::sign(rival, fixture.lineage.publisher()).unwrap();
    assert_eq!(
        refusal_code(
            &alice
                .1
                .publish_tip_statements(&[TipStatement::Seal(rival.clone())])
                .await
                .unwrap_err()
        ),
        Some("coordinator_unavailable"),
        "alice's own seal waits in her journal"
    );
    let early = seal(&fixture.lineage, &bob.1, tip).await;
    let successor = early.successor().unwrap();
    bob.1
        .publish_tip_statements(&[
            TipStatement::Seal(early.clone()),
            bookmark(&fixture.lineage, "main", Some(tip), successor),
        ])
        .await
        .unwrap();
    assert_eq!(bob.1.followed_tip().await, successor.unwrap());
    alice
        .1
        .receive_tip_statements(&bob.1.tip_statements(None, 32).await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        alice.1.followed_tip().await,
        tip,
        "a coordinated node whose coordinator is down defers the unconfirmed seal: the split"
    );

    fixture.mesh.log.set_available(true);
    super::seals::compose(&fixture, bob);
    assert_eq!(
        bob.1
            .finalize_seals(fixture.lineage.domain)
            .await
            .unwrap()
            .admitted,
        1
    );
    assert_eq!(
        fixture
            .mesh
            .log
            .head(tip_log(&fixture.lineage))
            .unwrap()
            .terminal
            .map(|terminal| terminal.digest),
        Some(early.id().unwrap()),
        "bob ends the log with the seal he made without it"
    );

    fixture.mesh.log.set_available(false);
    alice
        .1
        .receive_tip_statements(&bob.1.tip_statements(None, 32).await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        alice.1.followed_tip().await,
        successor.unwrap(),
        "the finality record proves the seal without alice's coordinator"
    );
    alice.1.settle_tip_statements().await.unwrap();
    let journaled = crate::storage::rows(
        &alice
            .1
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT count(*) FROM secure_pending_tips",
    )
    .await
    .unwrap();
    assert_eq!(
        journaled,
        vec![vec![SqlValue::Integer(0)]],
        "alice drops her rival seal once another seal of the tip is final"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_node_keeps_one_seal_of_a_tip_waiting_for_its_log() {
    let fixture = Fixture::new(1).await;
    super::seals::follow_main(&fixture).await;
    let alice = &fixture.alice.1;
    let tip = tip_log(&fixture.lineage).tip;
    fixture.mesh.log.set_available(false);
    let first = seal(&fixture.lineage, alice, tip).await;
    assert_eq!(
        refusal_code(
            &alice
                .publish_tip_statements(&[TipStatement::Seal(first.clone())])
                .await
                .unwrap_err()
        ),
        Some("coordinator_unavailable")
    );
    let mut again = first.statement.clone();
    again.frontier.push(AuthorMark {
        author: rocky_core::WriterId::from_bytes([0xee; 32]),
        sequence: 1,
    });
    let again = rocky_credentials::SignedSeal::sign(again, fixture.lineage.publisher()).unwrap();
    assert_eq!(
        refusal_code(
            &alice
                .publish_tip_statements(&[TipStatement::Seal(again)])
                .await
                .unwrap_err()
        ),
        Some("seal_pending")
    );
    let journaled = crate::storage::rows(
        &alice
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT count(*) FROM secure_pending_tips WHERE own=1",
    )
    .await
    .unwrap();
    assert_eq!(
        journaled,
        vec![vec![SqlValue::Integer(1)]],
        "a second seal of a waiting tip is not journaled"
    );
    fixture.mesh.log.set_available(true);
    alice.settle_tip_statements().await.unwrap();
    assert_eq!(
        alice
            .tip_graph(fixture.lineage.domain)
            .await
            .unwrap()
            .seal(tip)
            .unwrap(),
        Some(&first),
        "the waiting seal ends the log once the coordinator answers"
    );
}

pub(super) async fn receive_each(node: &Replica, statements: &[TipStatement]) {
    for statement in statements {
        node.receive_tip_statements(std::slice::from_ref(statement))
            .await
            .expect("every valid owner-signed revision is admitted");
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn conflicting_settings_are_admitted_in_every_order_and_resolve_alike() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let (alice, bob) = (&fixture.alice.1, &fixture.bob.1);
    let lineage = &fixture.lineage;
    let (x, y) = (&fixture.mesh.log, &MemoryLog::new());
    let mut refreshed = x.bundle();
    refreshed.url = Some("https://coordinator.example/bundle.json".into());
    let revisions = [
        coordinated_by(lineage, 1, x),
        coordinated_by(lineage, 1, y),
        settings(lineage, 5, Coordination::Uncoordinated),
        coordinated_by(lineage, 6, y),
        settings(
            lineage,
            7,
            Coordination::Coordinated {
                bundle: refreshed.clone(),
            },
        ),
    ];
    let mut reversed = revisions.clone();
    reversed.reverse();
    receive_each(alice, &revisions).await;
    receive_each(bob, &reversed).await;
    let id = |statement: &TipStatement| statement.id().unwrap();
    let founder = match id(&revisions[0]) < id(&revisions[1]) {
        true => x.bundle().identity,
        false => y.bundle().identity,
    };
    let expected = match founder == x.bundle().identity {
        true => (refreshed, 7),
        false => (y.bundle(), 6),
    };
    for node in [alice, bob] {
        let graph = node.tip_graph(lineage.domain).await.unwrap();
        assert_eq!(graph.settings.len(), revisions.len());
        assert_eq!(
            graph.coordinator().unwrap(),
            Some(&expected.0),
            "the earliest coordinated revision, the lower digest of one revision, names the coordinator for good"
        );
        assert_eq!(
            graph
                .settings()
                .unwrap()
                .map(|s| (s.revision, s.coordinator())),
            Some((expected.1, Some(&expected.0))),
            "the governing settings and the coordinator agree in every order"
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn the_owners_devices_refuse_to_sign_a_downgrade_or_another_coordinator() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let alice = &fixture.alice.1;
    let lineage = &fixture.lineage;
    let log = &fixture.mesh.log;
    let graph = alice.tip_graph(lineage.domain).await.unwrap();
    assert_eq!(
        graph
            .next_settings(Coordination::Coordinated {
                bundle: log.bundle()
            })
            .unwrap()
            .map(|next| next.revision),
        Some(1)
    );
    receive_each(alice, &[coordinated_by(lineage, 1, log)]).await;
    let graph = alice.tip_graph(lineage.domain).await.unwrap();
    assert!(
        graph.next_settings(Coordination::Uncoordinated).is_err(),
        "never uncoordinated"
    );
    assert!(
        graph
            .next_settings(Coordination::Coordinated {
                bundle: MemoryLog::new().bundle()
            })
            .is_err(),
        "never another coordinator"
    );
    assert_eq!(
        graph
            .next_settings(Coordination::Coordinated {
                bundle: log.bundle()
            })
            .unwrap(),
        None,
        "nothing to sign when the settings already say so"
    );
    let mut refreshed = log.bundle();
    refreshed.url = Some("https://coordinator.example/bundle.json".into());
    assert_eq!(
        graph
            .next_settings(Coordination::Coordinated { bundle: refreshed })
            .unwrap()
            .map(|next| next.revision),
        Some(2),
        "the same coordinator's bundle may be refreshed"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn received_settings_not_the_owners_for_this_database_are_refused_and_a_replay_changes_nothing()
 {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let bob = &fixture.bob.1;
    let lineage = &fixture.lineage;
    let statement = |scope| DatabaseSettingsStatement {
        format: DATABASE_SETTINGS_FORMAT,
        scope,
        revision: 1,
        coordination: Coordination::Coordinated {
            bundle: fixture.mesh.log.bundle(),
        },
    };
    let stranger = Lineage::new();
    let foreign =
        SignedDatabaseSettings::sign(statement(lineage.domain), stranger.publisher()).unwrap();
    let elsewhere = rocky_core::Scope {
        domain: rocky_core::DomainId([9; 16]),
        ..lineage.domain
    };
    let other_database =
        SignedDatabaseSettings::sign(statement(elsewhere), lineage.publisher()).unwrap();
    let TipStatement::Settings(mut forged) = coordinated_by(lineage, 1, &fixture.mesh.log) else {
        unreachable!()
    };
    forged.statement.revision = 2;
    for (refused, why) in [
        (foreign, "signed by a key other than the owner's"),
        (other_database, "for another database"),
        (forged, "altered after signing"),
    ] {
        assert!(
            bob.receive_tip_statements(&[TipStatement::Settings(refused)])
                .await
                .is_err(),
            "settings {why} are refused"
        );
    }
    let genuine = coordinated_by(lineage, 1, &fixture.mesh.log);
    assert_eq!(
        bob.receive_tip_statements(std::slice::from_ref(&genuine))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        bob.receive_tip_statements(&[genuine]).await.unwrap(),
        0,
        "a replay admits nothing"
    );
    assert_eq!(
        bob.tip_graph(lineage.domain).await.unwrap().settings.len(),
        1
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_conflicting_settings_revision_never_fails_the_batch_it_arrives_in() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let bob = &fixture.bob.1;
    let lineage = &fixture.lineage;
    receive_each(bob, &[coordinated_by(lineage, 1, &fixture.mesh.log)]).await;
    let tip = tip_log(lineage).tip;
    let batch = [
        settings(lineage, 2, Coordination::Uncoordinated),
        coordinated_by(lineage, 3, &MemoryLog::new()),
        bookmark(lineage, "other", None, Some(tip)),
    ];
    assert_eq!(bob.receive_tip_statements(&batch).await.unwrap(), 3);
    let graph = bob.tip_graph(lineage.domain).await.unwrap();
    assert_eq!(graph.bookmark("other").unwrap(), Some(tip));
    assert_eq!(
        graph.coordinator().unwrap().map(|bundle| bundle.identity),
        Some(fixture.mesh.log.bundle().identity),
        "held, the conflicting revisions change nothing"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn finality_acknowledged_at_another_coordinator_never_makes_a_seal_final() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    let lineage = &fixture.lineage;
    let tip = tip_log(lineage).tip;
    let other = MemoryLog::new();
    other.configure(lineage.domain, 1).unwrap();
    alice
        .1
        .set_domain_log(Some(Arc::new(other.client(alice.0.public_key()))));
    let elsewhere = seal(lineage, &alice.1, tip).await;
    alice
        .1
        .publish_tip_statements(&[TipStatement::Seal(elsewhere.clone())])
        .await
        .unwrap();
    assert_eq!(
        alice
            .1
            .finalize_seals(lineage.domain)
            .await
            .unwrap()
            .admitted,
        1
    );
    let graph = alice.1.tip_graph(lineage.domain).await.unwrap();
    assert!(graph.is_final(elsewhere.id().unwrap()).unwrap());
    assert_eq!(
        graph.finalities[0].coordinator(),
        Some(other.bundle().identity),
        "an acknowledgment names the coordinator whose log ended"
    );
    assert!(
        graph
            .next_settings(Coordination::Coordinated {
                bundle: fixture.mesh.log.bundle()
            })
            .is_err(),
        "alice's owner device never names another coordinator than her finality records"
    );
    assert!(
        graph
            .next_settings(Coordination::Coordinated {
                bundle: other.bundle()
            })
            .unwrap()
            .is_some()
    );

    super::seals::compose(&fixture, bob);
    let mut named = seal(lineage, &bob.1, tip).await.statement;
    named.frontier.push(AuthorMark {
        author: rocky_core::WriterId::from_bytes([0xee; 32]),
        sequence: 1,
    });
    let named = rocky_credentials::SignedSeal::sign(named, lineage.publisher()).unwrap();
    bob.1
        .publish_tip_statements(&[
            TipStatement::Seal(named.clone()),
            bookmark(lineage, "main", Some(tip), named.successor().unwrap()),
        ])
        .await
        .unwrap();
    assert_eq!(
        bob.1.finalize_seals(lineage.domain).await.unwrap().admitted,
        1
    );
    receive_each(&alice.1, &[coordinated_by(lineage, 1, &fixture.mesh.log)]).await;
    share(&alice.1, &bob.1).await;
    share(&bob.1, &alice.1).await;
    for node in [&alice.1, &bob.1] {
        let graph = node.tip_graph(lineage.domain).await.unwrap();
        assert!(
            !graph.is_final(elsewhere.id().unwrap()).unwrap(),
            "another coordinator's record never makes a seal final, whenever it arrived"
        );
        assert!(graph.is_final(named.id().unwrap()).unwrap());
        assert_eq!(
            graph.seal(tip).unwrap(),
            Some(&named),
            "one final seal per tip"
        );
        assert_eq!(
            node.followed_tip().await,
            named.successor().unwrap().unwrap()
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_node_never_ends_a_log_beyond_a_seal_that_lost_its_tip() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    let lineage = &fixture.lineage;
    let genesis = tip_log(lineage).tip;
    let first = seal(lineage, &bob.1, genesis).await;
    let t1 = first.successor().unwrap().unwrap();
    bob.1
        .publish_tip_statements(&[
            TipStatement::Seal(first.clone()),
            bookmark(lineage, "main", Some(genesis), Some(t1)),
        ])
        .await
        .unwrap();
    let second = seal(lineage, &bob.1, t1).await;
    let t2 = second.successor().unwrap().unwrap();
    bob.1
        .publish_tip_statements(&[
            TipStatement::Seal(second),
            bookmark(lineage, "main", Some(t1), Some(t2)),
        ])
        .await
        .unwrap();
    assert_eq!(bob.1.followed_tip().await, t2);

    super::seals::compose(&fixture, alice);
    receive_each(&alice.1, &[coordinated_by(lineage, 1, &fixture.mesh.log)]).await;
    let mut winner = seal(lineage, &alice.1, genesis).await.statement;
    winner.frontier.push(AuthorMark {
        author: rocky_core::WriterId::from_bytes([0xee; 32]),
        sequence: 1,
    });
    let winner = rocky_credentials::SignedSeal::sign(winner, lineage.publisher()).unwrap();
    alice
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(winner.clone()),
            bookmark(lineage, "main", Some(genesis), winner.successor().unwrap()),
        ])
        .await
        .unwrap();
    share(&alice.1, &bob.1).await;
    super::seals::compose(&fixture, bob);
    bob.1.finalize_seals(lineage.domain).await.unwrap();
    let log = |tip| rocky_core::TipLog {
        scope: lineage.domain,
        tip,
    };
    assert_eq!(
        fixture
            .mesh
            .log
            .head(log(genesis))
            .unwrap()
            .terminal
            .map(|terminal| terminal.digest),
        Some(winner.id().unwrap())
    );
    assert!(
        fixture.mesh.log.head(log(t1)).unwrap().terminal.is_none(),
        "the log of a tip on the losing branch is never ended"
    );
    share(&bob.1, &alice.1).await;
    for node in [&alice.1, &bob.1] {
        assert_eq!(
            node.followed_tip().await,
            winner.successor().unwrap().unwrap(),
            "both follow the seal that ended genesis"
        );
    }
}
