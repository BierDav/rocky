//! A private close is certified without admitting or exporting its candidate graph.
use super::*;
use crate::StagedCloseStatus;
use rocky_credentials::{SealAction, SignedSeal};

async fn trusted(fixture: &Fixture, name: &str) -> (PrivateKey, Replica) {
    let (identity, node) = (PrivateKey::new(), PrivateKey::new());
    let replica = fixture
        .lineage
        .open_trusted(
            &fixture.dir.path().join(format!("{name}.db")),
            (&identity, &node),
            3,
            None,
        )
        .await
        .unwrap();
    let node = (node, replica);
    seals::join_main(fixture, &node).await;
    node
}

async fn close(fixture: &Fixture) -> SignedSeal {
    let mut statement = seals::seal(
        &fixture.lineage,
        &fixture.alice.1,
        tip_log(&fixture.lineage).tip,
    )
    .await
    .statement;
    statement.action = SealAction::Close;
    SignedSeal::sign(statement, fixture.lineage.publisher()).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_relay_certifies_only_the_exact_private_close() {
    let fixture = Fixture::new(1).await;
    seals::follow_main(&fixture).await;
    let relay = trusted(&fixture, "relay").await;
    let close = close(&fixture).await;
    let bytes = close.to_bytes().unwrap();
    let graph_before = relay.1.tip_graph(fixture.lineage.domain).await.unwrap();
    let log = tip_log(&fixture.lineage);
    let proof = match relay.1.relay_staged_close(&bytes).await.unwrap() {
        StagedCloseStatus::Finality(proof) => proof,
        other => panic!("{other:?}"),
    };
    assert_eq!(proof.seal, close.id().unwrap());
    assert_eq!(
        relay.1.tip_graph(fixture.lineage.domain).await.unwrap(),
        graph_before
    );
    assert!(
        !relay
            .1
            .tip_statements(None, 32)
            .await
            .unwrap()
            .iter()
            .any(|statement| statement.id().unwrap() == close.id().unwrap())
    );
    assert!(relay.1.pending_tip_candidates().await.unwrap().is_empty());
    assert!(matches!(
        relay.1.staged_close_finality(&bytes).await.unwrap(),
        StagedCloseStatus::Finality(_)
    ));
    assert_eq!(
        fixture.mesh.log.head(log).unwrap().terminal.unwrap().digest,
        close.id().unwrap()
    );
}

/// Relays settled with their proof leave the journal's quota to new ones.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn settled_relays_never_exhaust_the_relay_quota() {
    let fixture = Fixture::new(1).await;
    seals::follow_main(&fixture).await;
    let relay = trusted(&fixture, "settled").await;
    let close = close(&fixture).await;
    let bytes = close.to_bytes().unwrap();
    let journal = |proof: bool| {
        let relay = &relay.1;
        async move {
            let inner = relay.inner.lock(crate::replica::Class::Background).await;
            for n in 0..64u8 {
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_close_relays VALUES(?,x'00',1,?)",
                        crate::storage::bindings(vec![
                            SqlValue::Blob(vec![n; 32]),
                            if proof {
                                SqlValue::Blob(vec![n])
                            } else {
                                SqlValue::Null
                            },
                        ]),
                    )
                    .await
                    .unwrap();
            }
        }
    };
    journal(false).await;
    assert!(matches!(
        relay.1.relay_staged_close(&bytes).await,
        Err(Error::Quota { .. })
    ));
    relay
        .1
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute("DELETE FROM secure_close_relays", ())
        .await
        .unwrap();
    journal(true).await;
    assert!(matches!(
        relay.1.relay_staged_close(&bytes).await.unwrap(),
        StagedCloseStatus::Finality(_)
    ));
}

/// A close published with its finality leaves the relay journal; a retry
/// still answers with a verified proof.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_published_final_close_leaves_the_relay_journal() {
    let fixture = Fixture::new(1).await;
    seals::follow_main(&fixture).await;
    let relay = trusted(&fixture, "published").await;
    let close = close(&fixture).await;
    let bytes = close.to_bytes().unwrap();
    let proof = match relay.1.relay_staged_close(&bytes).await.unwrap() {
        StagedCloseStatus::Finality(proof) => proof,
        other => panic!("{other:?}"),
    };
    let journaled = || async {
        crate::storage::rows(
            &relay
                .1
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .conn,
            "SELECT count(*) FROM secure_close_relays",
        )
        .await
        .unwrap()
    };
    assert_eq!(journaled().await, vec![vec![SqlValue::Integer(1)]]);
    relay
        .1
        .receive_tip_statements(&[
            crate::TipStatement::Seal(close.clone()),
            crate::TipStatement::Finality(proof),
        ])
        .await
        .unwrap();
    assert_eq!(journaled().await, vec![vec![SqlValue::Integer(0)]]);
    for retried in [
        relay.1.staged_close_finality(&bytes).await.unwrap(),
        relay.1.relay_staged_close(&bytes).await.unwrap(),
    ] {
        assert!(
            matches!(retried, StagedCloseStatus::Finality(proof) if proof.seal == close.id().unwrap())
        );
    }
    assert_eq!(journaled().await, vec![vec![SqlValue::Integer(0)]]);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_unknown_rival_terminal_never_certifies_the_private_candidate() {
    let fixture = Fixture::new(1).await;
    seals::follow_main(&fixture).await;
    let relay = trusted(&fixture, "relay").await;
    let candidate = close(&fixture).await;
    let mut statement = candidate.statement.clone();
    statement.frontier.push(rocky_credentials::AuthorMark {
        author: rocky_core::WriterId::from_bytes([0xff; 32]),
        sequence: 1,
    });
    let rival = SignedSeal::sign(statement, fixture.lineage.publisher()).unwrap();
    assert_ne!(rival.id().unwrap(), candidate.id().unwrap());
    let log = tip_log(&fixture.lineage);
    fixture
        .mesh
        .client(&fixture.alice.0)
        .seal(
            log,
            rocky_core::LogTerminal {
                digest: rival.id().unwrap(),
                continuation: None,
                confirms: false,
            },
        )
        .await
        .unwrap();
    let bytes = candidate.to_bytes().unwrap();
    assert!(matches!(
        relay.1.relay_staged_close(&bytes).await.unwrap(),
        StagedCloseStatus::Pending
    ));
    assert!(matches!(
        relay.1.staged_close_finality(&bytes).await.unwrap(),
        StagedCloseStatus::Pending
    ));
    assert!(relay.1.tip_graph(log.scope).await.unwrap().seals.is_empty());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn relays_refuse_continuations_and_recovery_candidates() {
    let fixture = Fixture::new(1).await;
    seals::follow_main(&fixture).await;
    let relay = trusted(&fixture, "relay").await;
    let continuation = seals::seal(
        &fixture.lineage,
        &fixture.alice.1,
        tip_log(&fixture.lineage).tip,
    )
    .await;
    assert!(
        relay
            .1
            .relay_staged_close(&continuation.to_bytes().unwrap())
            .await
            .is_err()
    );
    assert!(relay.1.staged_close_finality(&[1, 2, 3]).await.is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_committed_relay_resumes_exact_bytes_after_reopen_without_admitting_the_close() {
    let fixture = Fixture::new(2).await;
    seals::follow_main(&fixture).await;
    let close = close(&fixture).await;
    let bytes = close.to_bytes().unwrap();
    assert!(matches!(
        fixture.alice.1.relay_staged_close(&bytes).await.unwrap(),
        StagedCloseStatus::Pending
    ));
    let head = fixture.mesh.log.head(tip_log(&fixture.lineage)).unwrap();
    assert_eq!(head.terminal.unwrap().digest, close.id().unwrap());
    let records = crate::storage::rows(
        &fixture
            .alice
            .1
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT bytes,dispatched FROM secure_close_relays",
    )
    .await
    .unwrap();
    assert_eq!(
        records,
        vec![vec![SqlValue::Blob(bytes.clone()), SqlValue::Integer(1)]]
    );
    let fixture = fixture.restart_alice().await;
    seals::compose(&fixture, &fixture.alice);
    assert!(matches!(
        fixture.alice.1.staged_close_finality(&bytes).await.unwrap(),
        StagedCloseStatus::Pending
    ));
    assert!(matches!(
        fixture.bob.1.staged_close_finality(&bytes).await.unwrap(),
        StagedCloseStatus::Finality(_)
    ));
    let proof = match fixture.alice.1.relay_staged_close(&bytes).await.unwrap() {
        StagedCloseStatus::Finality(proof) => proof,
        other => panic!("{other:?}"),
    };
    assert_eq!(proof.seal, close.id().unwrap());
    assert_eq!(
        fixture
            .mesh
            .log
            .head(tip_log(&fixture.lineage))
            .unwrap()
            .commit,
        head.commit
    );
    assert!(
        fixture
            .alice
            .1
            .tip_graph(fixture.lineage.domain)
            .await
            .unwrap()
            .seals
            .is_empty()
    );
    assert!(
        fixture
            .alice
            .1
            .pending_tip_candidates()
            .await
            .unwrap()
            .is_empty()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_pending_relay_uses_the_raised_signed_minimum_and_distinct_acknowledgers() {
    let fixture = Fixture::new(2).await;
    seals::follow_main(&fixture).await;
    let relay = trusted(&fixture, "minimum-relay").await;
    let close = close(&fixture).await;
    let bytes = close.to_bytes().unwrap();
    assert!(matches!(
        relay.1.relay_staged_close(&bytes).await.unwrap(),
        StagedCloseStatus::Pending
    ));
    let raised = TipStatement::GenesisLine(
        rocky_credentials::SignedGenesisLine::sign(
            rocky_credentials::GenesisLineStatement {
                format: rocky_credentials::GENESIS_LINE_FORMAT,
                scope: fixture.lineage.domain,
                minimum_in_sync: 3,
            },
            fixture.lineage.publisher(),
        )
        .unwrap(),
    );
    relay.1.receive_tip_statements(&[raised]).await.unwrap();
    fixture.alice.1.staged_close_finality(&bytes).await.unwrap();
    for _ in 0..2 {
        assert!(matches!(
            relay.1.staged_close_finality(&bytes).await.unwrap(),
            StagedCloseStatus::Pending
        ));
    }
    fixture.bob.1.staged_close_finality(&bytes).await.unwrap();
    assert!(matches!(
        relay.1.staged_close_finality(&bytes).await.unwrap(),
        StagedCloseStatus::Finality(_)
    ));
    let head = fixture.mesh.log.head(tip_log(&fixture.lineage)).unwrap();
    assert_eq!(head.acknowledgments.len(), 3);
    assert_eq!(head.terminal.unwrap().digest, close.id().unwrap());
    assert!(
        relay
            .1
            .tip_graph(fixture.lineage.domain)
            .await
            .unwrap()
            .seals
            .is_empty()
    );
}

/// A relay holding the client of a coordinator the settings do not name
/// ends no log with it, and a proof it took from that log before the
/// settings arrived no longer answers.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_relay_answers_a_staged_close_only_with_finality_at_the_named_coordinator() {
    let fixture = Fixture::new(1).await;
    seals::follow_main(&fixture).await;
    let relay = trusted(&fixture, "named").await;
    let close = close(&fixture).await;
    let bytes = close.to_bytes().unwrap();
    let elsewhere = super::declared::coordinated_by(&fixture.lineage, 1, &MemoryLog::new());
    let fresh = trusted(&fixture, "fresh").await;
    super::declared::receive_each(&fresh.1, std::slice::from_ref(&elsewhere)).await;
    assert!(matches!(
        fresh.1.relay_staged_close(&bytes).await.unwrap(),
        StagedCloseStatus::Pending
    ));
    assert_eq!(
        fixture
            .mesh
            .log
            .head(tip_log(&fixture.lineage))
            .unwrap()
            .terminal,
        None,
        "the relay ends no log of a coordinator the settings do not name"
    );

    let proof = match relay.1.relay_staged_close(&bytes).await.unwrap() {
        StagedCloseStatus::Finality(proof) => proof,
        other => panic!("{other:?}"),
    };
    super::declared::receive_each(&relay.1, &[elsewhere]).await;
    let graph = relay.1.tip_graph(fixture.lineage.domain).await.unwrap();
    assert!(!graph.counts_finality(&proof).unwrap());
    for status in [
        relay.1.staged_close_finality(&bytes).await.unwrap(),
        relay.1.relay_staged_close(&bytes).await.unwrap(),
    ] {
        assert!(
            matches!(status, StagedCloseStatus::Pending),
            "the stored proof of the other log no longer answers: {status:?}"
        );
    }
}

/// A relay that admitted a seal before it composed a log follows a tip
/// whose parent's log no node ended: it ends that tip's log for a close
/// only once the seal leading into the tip ended its parent's.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_relay_never_ends_a_tips_log_before_the_seal_leading_into_it_ended_its_parent() {
    let fixture = Fixture::new(1).await;
    seals::follow_main(&fixture).await;
    let main = tip_log(&fixture.lineage);
    let (identity, node) = (PrivateKey::new(), PrivateKey::new());
    let lagging = fixture
        .lineage
        .open_trusted(
            &fixture.dir.path().join("lagging.db"),
            (&identity, &node),
            3,
            None,
        )
        .await
        .unwrap();
    let lagging = (node, lagging);
    seals::share(&fixture.alice.1, &lagging.1).await;
    lagging
        .1
        .follow_bookmark(fixture.lineage.domain, "main")
        .await
        .unwrap();
    let entering = seals::seal(&fixture.lineage, &fixture.alice.1, main.tip).await;
    let next = entering.successor().unwrap().unwrap();
    lagging
        .1
        .receive_tip_statements(&[
            TipStatement::Seal(entering),
            seals::bookmark(&fixture.lineage, "main", Some(main.tip), Some(next)),
        ])
        .await
        .unwrap();
    assert_eq!(
        lagging.1.followed_log(fixture.lineage.domain).await.tip,
        next
    );
    assert_eq!(
        fixture.mesh.log.head(main).unwrap().terminal,
        None,
        "no node ended the parent's log"
    );
    seals::compose(&fixture, &lagging);
    let mut statement = seals::seal(&fixture.lineage, &lagging.1, next)
        .await
        .statement;
    statement.action = SealAction::Close;
    let close = SignedSeal::sign(statement, fixture.lineage.publisher()).unwrap();
    assert!(matches!(
        lagging
            .1
            .relay_staged_close(&close.to_bytes().unwrap())
            .await
            .unwrap(),
        StagedCloseStatus::Pending
    ));
    let child = TipLog {
        scope: fixture.lineage.domain,
        tip: next,
    };
    assert_eq!(
        fixture.mesh.log.head(child).unwrap().terminal,
        None,
        "the child's log stays open while its parent's is"
    );
}
