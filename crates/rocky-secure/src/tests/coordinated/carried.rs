//! Which losing continuation's rows carry forward as late writes once the
//! owner's settings name a coordinator: only those of a seal that lost its
//! tip at that coordinator's log, where the log decided between seals no
//! graph held yet. A loss graphs decided before the settings, which the log
//! only confirmed afterwards, keeps its rows held on every replica, so a
//! re-authored row is never duplicated.
use super::declared::{coordinated_by, follow_main_uncomposed, receive_each};
use super::dropped::{publish_moving_main, seal_marked};
use super::*;
use crate::tests::compaction::sync;
use crate::tests::generations::{identity_insert, select, session};
use rocky_core::TipId;

/// Inserts the item `key` titled `title` on `node`'s followed tip.
async fn add(node: &Replica, key: &str, title: &str) {
    identity_insert(
        &session(node).await,
        "INSERT INTO items(key,title) VALUES(?,?)",
        vec![SqlValue::Text(key.into()), SqlValue::Text(title.into())],
    )
    .await
    .unwrap();
}
/// Frames `node` holds out of its view, and the held frames it keeps for
/// them: the application sees neither as rows.
async fn held(node: &Replica) -> (Vec<Vec<SqlValue>>, Vec<Vec<SqlValue>>) {
    let inner = node.inner.lock(crate::replica::Class::Background).await;
    (
        crate::storage::rows(
            &inner.conn,
            "SELECT count(*) FROM secure_operations WHERE status='held'",
        )
        .await
        .unwrap(),
        crate::storage::rows(&inner.conn, "SELECT count(*) FROM secure_held_frames")
            .await
            .unwrap(),
    )
}
fn counts(operations: i64, frames: i64) -> (Vec<Vec<SqlValue>>, Vec<Vec<SqlValue>>) {
    (
        vec![vec![SqlValue::Integer(operations)]],
        vec![vec![SqlValue::Integer(frames)]],
    )
}
/// Every key of `items` on `node`, in order.
async fn keys(node: &Replica) -> Vec<Vec<SqlValue>> {
    select(node, "SELECT key FROM items ORDER BY key").await
}
fn text(keys: &[&str]) -> Vec<Vec<SqlValue>> {
    keys.iter()
        .map(|key| vec![SqlValue::Text((*key).into())])
        .collect()
}
/// Shares tip statements and `items` frames both ways, and hands each
/// device every frame the other authored, those it holds too.
async fn exchange(fixture: &Fixture) {
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    sync(&fixture.lineage, &alice.1, &bob.1, &bob.0).await;
    sync(&fixture.lineage, &bob.1, &alice.1, &alice.0).await;
    for (from, to) in [(alice, bob), (bob, alice)] {
        for frame in
            crate::tests::stored_frames(&from.1, "SELECT header,body FROM secure_operations").await
        {
            if frame.operation().unwrap().header.public_key == from.0.public_key() {
                to.1.receive(frame).await.unwrap();
            }
        }
    }
}

/// A device that joins now: alice's statements and frames, then every
/// frame bob authored.
async fn joined_after(fixture: &Fixture, name: &str) -> Replica {
    let key = PrivateKey::new();
    let node = super::open(&fixture.lineage, &fixture.dir, name, &key).await;
    node.receive_tip_statements(&fixture.alice.1.tip_statements(None, 32).await.unwrap())
        .await
        .unwrap();
    node.follow_bookmark(fixture.lineage.domain, "main")
        .await
        .unwrap();
    sync(&fixture.lineage, &fixture.alice.1, &node, &key).await;
    for frame in
        crate::tests::stored_frames(&fixture.bob.1, "SELECT header,body FROM secure_operations")
            .await
    {
        if frame.operation().unwrap().header.public_key == fixture.bob.0.public_key() {
            node.receive(frame).await.unwrap();
        }
    }
    node
}

/// Before the database was coordinated, bob's seal of genesis lost to
/// alice's by digest on both devices, and the row bob wrote after his seal
/// is held on both. The owner then names a coordinator and alice's finality
/// pass ends genesis's log with her seal, which every graph held first: the
/// row stays held and out of every view, so re-authoring it duplicates
/// nothing.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_loss_graphs_decided_before_the_settings_keeps_its_rows_held_after_them() {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let (alice, bob) = (&fixture.alice.1, &fixture.bob.1);
    let domain = fixture.lineage.domain;
    let genesis = tip_log(&fixture.lineage).tip;
    add(alice, "0001", "common").await;
    exchange(&fixture).await;
    let losing = seal_marked(&fixture, bob, genesis, 0).await;
    let mut winning = seal_marked(&fixture, alice, genesis, 1).await;
    let mut mark = 1;
    while winning.id().unwrap() > losing.id().unwrap() {
        mark += 1;
        winning = seal_marked(&fixture, alice, genesis, mark).await;
    }
    let lost = publish_moving_main(&fixture, bob, &losing).await;
    add(bob, "0002", "after the losing seal").await;
    publish_moving_main(&fixture, alice, &winning).await;
    add(alice, "0003", "after the winning seal").await;
    exchange(&fixture).await;
    for node in [alice, bob] {
        assert_eq!(keys(node).await, text(&["0001", "0003"]));
        assert_eq!(held(node).await, counts(1, 1), "bob's row is held on both");
    }

    let settings = coordinated_by(&fixture.lineage, 1, &fixture.mesh.log);
    receive_each(alice, std::slice::from_ref(&settings)).await;
    receive_each(bob, &[settings]).await;
    super::seals::compose(&fixture, &fixture.alice);
    super::seals::compose(&fixture, &fixture.bob);
    assert_eq!(alice.finalize_seals(domain).await.unwrap().admitted, 1);
    let ended = fixture
        .mesh
        .log
        .head(TipLog {
            scope: domain,
            tip: genesis,
        })
        .unwrap()
        .terminal
        .unwrap();
    assert!(
        ended.digest == winning.id().unwrap() && ended.confirms,
        "the log confirms the seal graphs had chosen"
    );
    exchange(&fixture).await;
    let next = seal_marked(&fixture, alice, winning.successor().unwrap().unwrap(), 0).await;
    publish_moving_main(&fixture, alice, &next).await;
    exchange(&fixture).await;
    let carol = joined_after(&fixture, "carol").await;
    for node in [alice, bob, &carol] {
        let graph = node.tip_graph(domain).await.unwrap();
        assert!(graph.is_final(winning.id().unwrap()).unwrap());
        assert_eq!(
            keys(node).await,
            text(&["0001", "0003"]),
            "the held row never becomes a late write, on a device that held it or one that learns of it after the settings"
        );
        assert_eq!(held(node).await, counts(1, 1), "the held row stays held");
        assert_eq!(graph.late_anchor(lost).unwrap(), lost);
    }
}

/// What bob does between his losing seal and the row he writes after it.
#[derive(Clone, Copy, PartialEq)]
enum Between {
    Nothing,
    /// Bob compacts at his losing seal, listing it as his base.
    Compacts,
    /// A new key epoch of `items` reaches both devices: bob's row is
    /// written under it.
    RotatesTheKey,
}
/// Bob seals genesis without a log before the settings name the
/// coordinator and writes after it; alice's seal then ends genesis's log
/// there, deciding the tip before any graph held either. Returns the
/// fixture, bob's losing continuation and alice's winning seal, both
/// devices having exchanged everything.
async fn decided_loss(between: Between) -> (Fixture, TipId, rocky_credentials::SignedSeal) {
    let fixture = Fixture::new(1).await;
    follow_main_uncomposed(&fixture).await;
    let (alice, bob) = (&fixture.alice.1, &fixture.bob.1);
    let domain = fixture.lineage.domain;
    let genesis = tip_log(&fixture.lineage).tip;
    add(alice, "0001", "common").await;
    exchange(&fixture).await;
    let losing = seal_marked(&fixture, bob, genesis, 0).await;
    let lost = publish_moving_main(&fixture, bob, &losing).await;
    match between {
        Between::Nothing => {}
        Between::Compacts => {
            bob.compact(domain, 0).await.unwrap();
            assert_eq!(
                bob.compaction_status(domain).await.unwrap().compacted,
                Some(losing.id().unwrap())
            );
        }
        Between::RotatesTheKey => {
            for node in [alice, bob] {
                node.install_key_epoch(crate::KeyMaterial::new(
                    fixture.lineage.scope("items"),
                    2,
                    [77; 32],
                ))
                .await
                .unwrap();
            }
        }
    }
    add(bob, "0002", "after the losing seal").await;

    let settings = coordinated_by(&fixture.lineage, 1, &fixture.mesh.log);
    receive_each(alice, std::slice::from_ref(&settings)).await;
    super::seals::compose(&fixture, &fixture.alice);
    let winning = seal_marked(&fixture, alice, genesis, 1).await;
    publish_moving_main(&fixture, alice, &winning).await;
    add(alice, "0003", "after the winning seal").await;
    assert_eq!(alice.finalize_seals(domain).await.unwrap().admitted, 1);
    assert!(
        !fixture
            .mesh
            .log
            .head(TipLog {
                scope: domain,
                tip: genesis
            })
            .unwrap()
            .terminal
            .unwrap()
            .confirms,
        "the log decided the tip"
    );
    exchange(&fixture).await;
    exchange(&fixture).await;
    (fixture, lost, winning)
}

/// Bob's row after his losing seal is a late write of genesis on both
/// devices and held on neither, also after a later seal and on a device
/// that joins after it. A revoked losing seal would carry nothing: the
/// owner's revocation, not the coordinator's log, ended it.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_loss_the_named_coordinators_log_decided_carries_its_rows_to_the_winning_line() {
    let (fixture, lost, winning) = decided_loss(Between::Nothing).await;
    let (alice, bob) = (&fixture.alice.1, &fixture.bob.1);
    let domain = fixture.lineage.domain;
    let genesis = tip_log(&fixture.lineage).tip;
    for node in [alice, bob] {
        let graph = node.tip_graph(domain).await.unwrap();
        assert!(graph.decided_by_named(winning.id().unwrap()).unwrap());
        assert_eq!(graph.late_anchor(lost).unwrap(), genesis);
        assert_eq!(
            keys(node).await,
            text(&["0001", "0002", "0003"]),
            "the losing seal never ended genesis: bob's row is its late write"
        );
        assert_eq!(held(node).await.0, counts(0, 0).0, "nothing is held");
        let mut revoked = graph.clone();
        let losing = graph
            .seal_by_id(
                graph
                    .seals
                    .iter()
                    .find(|seal| seal.successor().unwrap() == Some(lost))
                    .unwrap()
                    .id()
                    .unwrap(),
            )
            .unwrap()
            .id()
            .unwrap();
        revoked.revoked.insert(losing);
        assert_eq!(revoked.late_anchor(lost).unwrap(), lost);
    }
    let next = seal_marked(&fixture, alice, winning.successor().unwrap().unwrap(), 0).await;
    publish_moving_main(&fixture, alice, &next).await;
    exchange(&fixture).await;
    let carol = joined_after(&fixture, "carol").await;
    for node in [alice, bob, &carol] {
        assert_eq!(
            keys(node).await,
            text(&["0001", "0002", "0003"]),
            "a later seal keeps the late write, on a device that joins after it too"
        );
    }
}

/// Bob compacted at his losing seal before the coordinator decided
/// genesis: his row still reaches alice as a late write. Bob's base left
/// the path, so he waits for the rebootstrap any permitted peer serves,
/// keeping every frame meanwhile.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_loss_decided_after_the_loser_compacted_at_it_still_carries_its_rows() {
    let (fixture, lost, _) = decided_loss(Between::Compacts).await;
    let (alice, bob) = (&fixture.alice.1, &fixture.bob.1);
    let genesis = tip_log(&fixture.lineage).tip;
    assert_eq!(
        alice
            .tip_graph(fixture.lineage.domain)
            .await
            .unwrap()
            .late_anchor(lost)
            .unwrap(),
        genesis
    );
    assert_eq!(keys(alice).await, text(&["0001", "0002", "0003"]));
    assert!(
        bob.blockers()
            .await
            .unwrap()
            .iter()
            .any(|blocker| blocker.kind == crate::BlockerKind::LostCompaction),
        "bob's base left the path"
    );
    let frame_kept = crate::storage::rows(
        &bob.inner.lock(crate::replica::Class::Background).await.conn,
        "SELECT count(*) FROM secure_operations WHERE status='applied' OR status='held'",
    )
    .await
    .unwrap();
    assert_eq!(
        frame_kept,
        vec![vec![SqlValue::Integer(3)]],
        "bob keeps every frame, his carried row too, for the rebootstrap a peer serves"
    );
}

/// A key epoch rotated between bob's losing seal and his row: the row,
/// written under the new epoch, is carried like any other.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_loss_decided_across_a_key_rotation_carries_rows_of_the_new_epoch() {
    let (fixture, _, _) = decided_loss(Between::RotatesTheKey).await;
    for node in [&fixture.alice.1, &fixture.bob.1] {
        assert_eq!(keys(node).await, text(&["0001", "0002", "0003"]));
        assert_eq!(held(node).await.0, counts(0, 0).0);
    }
}
