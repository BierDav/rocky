use super::declared::{coordinated_by, follow_main_uncomposed, receive_each};
use super::seals::{bookmark, seal, share};
use super::*;
use crate::{FinalityStall, TipStatement};
use rocky_coordinator::coordinated::ENDED_LOGS;
use rocky_core::{LogTerminal, TipId};
use rocky_credentials::{AuthorMark, SignedSeal};

async fn end_other_lines(fixture: &Fixture) {
    let client = fixture.mesh.client(&fixture.alice.0);
    for line in 0..ENDED_LOGS {
        let tip = TipId(rocky_core::commitment("other-line", &line.to_le_bytes()));
        client
            .seal(
                TipLog {
                    scope: fixture.lineage.domain,
                    tip,
                },
                LogTerminal {
                    digest: rocky_core::commitment("other-seal", &line.to_le_bytes()),
                    continuation: None,
                    confirms: false,
                },
            )
            .await
            .unwrap();
    }
}
pub(super) async fn seal_marked(
    fixture: &Fixture,
    node: &Replica,
    tip: TipId,
    mark: u64,
) -> SignedSeal {
    let mut statement = seal(&fixture.lineage, node, tip).await.statement;
    if mark > 0 {
        statement.frontier.push(AuthorMark {
            author: rocky_core::WriterId::from_bytes([0xee; 32]),
            sequence: mark,
        });
        statement.frontier.sort_by_key(|mark| mark.author);
    }
    SignedSeal::sign(statement, fixture.lineage.publisher()).unwrap()
}
pub(super) async fn publish_moving_main(
    fixture: &Fixture,
    node: &Replica,
    sealed: &SignedSeal,
) -> TipId {
    let next = sealed.successor().unwrap().unwrap();
    node.publish_tip_statements(&[
        TipStatement::Seal(sealed.clone()),
        bookmark(
            &fixture.lineage,
            "main",
            Some(sealed.statement.tip),
            Some(next),
        ),
    ])
    .await
    .unwrap();
    next
}
fn log(fixture: &Fixture, tip: TipId) -> TipLog {
    TipLog {
        scope: fixture.lineage.domain,
        tip,
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_seal_becomes_final_after_the_coordinator_dropped_its_lines_earlier_logs() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let alice = &fixture.alice.1;
    let domain = fixture.lineage.domain;
    super::seals::compose(&fixture, &fixture.alice);
    receive_each(
        alice,
        &[coordinated_by(&fixture.lineage, 1, &fixture.mesh.log)],
    )
    .await;
    let genesis = tip_log(&fixture.lineage).tip;
    let first = seal_marked(&fixture, alice, genesis, 0).await;
    let t1 = publish_moving_main(&fixture, alice, &first).await;
    let second = seal_marked(&fixture, alice, t1, 0).await;
    let t2 = publish_moving_main(&fixture, alice, &second).await;
    let finalized = alice.finalize_seals(domain).await.unwrap();
    assert_eq!((finalized.admitted, finalized.stalled), (2, None));

    end_other_lines(&fixture).await;
    let dropped = fixture.mesh.log.head(log(&fixture, genesis)).unwrap();
    assert!(
        dropped.terminal.is_none() && dropped.ended_dropped,
        "the coordinator reads the dropped log of genesis as never ended"
    );
    let third = seal_marked(&fixture, alice, t2, 0).await;
    publish_moving_main(&fixture, alice, &third).await;
    let finalized = alice.finalize_seals(domain).await.unwrap();
    assert_eq!(
        (finalized.admitted, finalized.stalled),
        (1, None),
        "the pass starts after the newest final seal, past the dropped logs"
    );
    let graph = alice.tip_graph(domain).await.unwrap();
    assert!(graph.is_final(third.id().unwrap()).unwrap());
    assert!(
        fixture
            .mesh
            .log
            .head(log(&fixture, genesis))
            .unwrap()
            .terminal
            .is_none(),
        "the dropped log of genesis is never ended again"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_lagging_node_never_ends_a_dropped_log_again_with_a_losing_seal() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let (alice, bob) = (&fixture.alice.1, &fixture.bob.1);
    let domain = fixture.lineage.domain;
    let genesis = tip_log(&fixture.lineage).tip;
    let losing = seal_marked(&fixture, bob, genesis, 0).await;
    let mut winning = seal_marked(&fixture, alice, genesis, 1).await;
    let mut mark = 1;
    while winning.id().unwrap() < losing.id().unwrap() {
        mark += 1;
        winning = seal_marked(&fixture, alice, genesis, mark).await;
    }
    publish_moving_main(&fixture, bob, &losing).await;

    let settings = coordinated_by(&fixture.lineage, 1, &fixture.mesh.log);
    super::seals::compose(&fixture, &fixture.alice);
    receive_each(alice, std::slice::from_ref(&settings)).await;
    let t1 = publish_moving_main(&fixture, alice, &winning).await;
    assert_eq!(alice.finalize_seals(domain).await.unwrap().admitted, 1);
    receive_each(bob, &[settings, TipStatement::Seal(winning.clone())]).await;
    assert_eq!(
        bob.tip_graph(domain).await.unwrap().seal(genesis).unwrap(),
        Some(&losing),
        "without the finality record bob prefers his seal by digest"
    );

    end_other_lines(&fixture).await;
    super::seals::compose(&fixture, &fixture.bob);
    let finalized = bob.finalize_seals(domain).await.unwrap();
    assert_eq!(
        (finalized.admitted, finalized.stalled),
        (0, Some(FinalityStall::LogDropped { tip: genesis })),
        "nothing proves genesis's log was not dropped"
    );
    assert!(
        fixture
            .mesh
            .log
            .head(log(&fixture, genesis))
            .unwrap()
            .terminal
            .is_none(),
        "the dropped log is never ended a second time"
    );
    assert!(
        !bob.tip_graph(domain)
            .await
            .unwrap()
            .is_final(losing.id().unwrap())
            .unwrap()
    );

    share(alice, bob).await;
    let graph = bob.tip_graph(domain).await.unwrap();
    assert_eq!(graph.seal(genesis).unwrap(), Some(&winning));
    assert_eq!(bob.followed_tip().await, t1);
    assert_eq!(
        bob.finalize_seals(domain).await.unwrap().stalled,
        None,
        "the winner's finality clears the stall"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_received_seal_no_node_of_its_line_can_confirm_raises_a_stall() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let (alice, bob) = (&fixture.alice.1, &fixture.bob.1);
    let domain = fixture.lineage.domain;
    let genesis = tip_log(&fixture.lineage).tip;
    let mut seals = Vec::new();
    let mut tip = genesis;
    for _ in 0..4 {
        let sealed = seal_marked(&fixture, bob, tip, 0).await;
        tip = publish_moving_main(&fixture, bob, &sealed).await;
        seals.push(sealed);
    }
    let held: Vec<_> = seals[..3]
        .iter()
        .map(|sealed| TipStatement::Seal(sealed.clone()))
        .collect();
    receive_each(alice, &held).await;
    assert_eq!(alice.followed_tip().await, genesis);

    receive_each(
        alice,
        &[coordinated_by(&fixture.lineage, 1, &fixture.mesh.log)],
    )
    .await;
    super::seals::compose(&fixture, &fixture.alice);
    alice
        .receive_tip_statements(&[TipStatement::Seal(seals[3].clone())])
        .await
        .unwrap();
    let finalized = alice.finalize_seals(domain).await.unwrap();
    let first = seals[0].successor().unwrap().unwrap();
    assert_eq!(
        finalized.stalled,
        Some(FinalityStall::EntryPending { tip: first }),
        "the stall names the first unended log of bob's line"
    );
    assert!(
        alice
            .tip_graph(domain)
            .await
            .unwrap()
            .is_final(seals[0].id().unwrap())
            .unwrap(),
        "alice ended the log of genesis, on her path, with bob's first seal"
    );
}
