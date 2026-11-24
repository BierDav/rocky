use super::*;
use crate::TipStatement;
use rocky_core::{RowApplication, TipId};
use rocky_credentials::{
    BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction, SealStatement,
    SignedBookmarkMove, SignedSeal,
};

fn bookmark(c: &Constrained, from: Option<TipId>, to: TipId) -> TipStatement {
    TipStatement::Bookmark(
        SignedBookmarkMove::sign(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope: c.domain,
                name: "main".into(),
                from,
                to: Some(to),
            },
            &c.publisher,
        )
        .unwrap(),
    )
}

async fn follow_main(c: &Constrained, replica: &EncryptedReplica<TypedRows>) {
    let genesis = replica.followed_tip().await;
    replica
        .publish_tip_statements(&[bookmark(c, None, genesis)])
        .await
        .unwrap();
    replica.follow_bookmark(c.domain, "main").await.unwrap();
}

async fn seal_and_compact(c: &Constrained, replica: &EncryptedReplica<TypedRows>) {
    let tip = replica.followed_tip().await;
    let seal = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: c.domain,
            tip,
            frontier: replica.seal_frontier(c.domain, false).await.unwrap(),
            action: SealAction::Continue {
                schema: c.app().manifest().hash(),
            },
        },
        &c.publisher,
    )
    .unwrap();
    let next = seal.successor().unwrap().unwrap();
    replica
        .publish_tip_statements(&[TipStatement::Seal(seal), bookmark(c, Some(tip), next)])
        .await
        .unwrap();
    replica.compact(c.domain, 0).await.unwrap();
}

async fn execute(replica: &EncryptedReplica<TypedRows>, sql: &str, scope: Option<ResourceScope>) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    match scope {
        Some(scope) => inner.conn.execute(sql, [encode(&scope).unwrap()]).await,
        None => inner.conn.execute(sql, ()).await,
    }
    .unwrap();
}

async fn orphaned_memberships(replica: &EncryptedReplica<TypedRows>) -> usize {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(
        &inner.conn,
        "SELECT 1 FROM memberships m WHERE NOT EXISTS(SELECT 1 FROM teams t WHERE t.tenant=m.tenant AND t.id=m.team)",
    )
    .await
    .unwrap()
    .len()
}

async fn keep_an_orphaned_membership(replica: &EncryptedReplica<TypedRows>, tenant: i64) {
    let missing = format!("X'{}'", "00".repeat(40));
    execute(replica, "PRAGMA foreign_keys=OFF", None).await;
    execute(
        replica,
        &format!(
            "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES({tenant},{missing},{missing},'member',1,1,{missing})"
        ),
        None,
    )
    .await;
    execute(replica, "PRAGMA foreign_keys=ON", None).await;
}

async fn memberships(replica: &EncryptedReplica<TypedRows>) -> usize {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(&inner.conn, "SELECT 1 FROM memberships")
        .await
        .unwrap()
        .len()
}

async fn a_sealed_membership_whose_team_base_is_lost(c: &Constrained, path: &Path) -> Device {
    let device = c.open(path).await;
    follow_main(c, &device.replica).await;
    let parents = device
        .commit(
            c,
            &[
                "INSERT INTO teams(tenant,name,id) VALUES(1,'ten',#id)",
                "INSERT INTO people(tenant,nickname,id) VALUES(1,'alpha',#id)",
            ],
        )
        .await
        .unwrap();
    device
        .commit(
            c,
            &[&format!(
                "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{},{},'member',1,1,#id)",
                parents[0], parents[1]
            )],
        )
        .await
        .unwrap();
    seal_and_compact(c, &device.replica).await;
    execute(
        &device.replica,
        "DELETE FROM secure_replayed_base WHERE scope=(SELECT id FROM secure_scope_state WHERE scope=?)",
        Some(c.scope("teams")),
    )
    .await;
    device
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_replay_orphaning_a_row_of_a_held_and_settled_parent_partition_is_refused() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let device = a_sealed_membership_whose_team_base_is_lost(&c, &root.path().join("d.db")).await;
    let refused = device
        .replica
        .rebuild_projection(c.scope("memberships"))
        .await;
    assert!(
        matches!(&refused, Err(Error::Incompatible(reason)) if reason == "rebuilding the projection would replay rows that break a foreign key: a row of memberships references a missing row of teams"),
        "{refused:?}"
    );
    assert_eq!(orphaned_memberships(&device.replica).await, 0);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_replay_orphaning_a_row_of_a_parent_partition_not_held_here_keeps_it() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let device = a_sealed_membership_whose_team_base_is_lost(&c, &root.path().join("d.db")).await;
    device
        .replica
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
        })
        .await
        .unwrap();
    for name in ["people", "notes", "memberships"] {
        device.replica.subscribe(c.scope(name)).await.unwrap();
    }
    device
        .replica
        .rebuild_projection(c.scope("memberships"))
        .await
        .unwrap();
    assert_eq!(orphaned_memberships(&device.replica).await, 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_orphan_kept_in_another_partition_drops_no_replayed_transaction() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    keep_an_orphaned_membership(&devices[1].replica, 2).await;
    devices[0]
        .commit(
            &c,
            &[
                "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,#next(teams),#next(people),'member',1,1,#id)",
                "INSERT INTO teams(tenant,name,id) VALUES(1,'ten',#id)",
                "INSERT INTO people(tenant,nickname,id) VALUES(1,'alpha',#id)",
            ],
        )
        .await
        .unwrap();
    let frames = devices[0].fresh().await;
    let statuses = devices[1].replica.receive_all(frames.clone()).await;
    assert_eq!(
        statuses.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec![ReceiveStatus::Applied; frames.len()],
        "a transaction in tenant 1 applies beside an orphan of tenant 2"
    );
    assert_eq!(
        devices[1]
            .replica
            .transaction_outcome(c.transaction(&frames))
            .await
            .unwrap(),
        Some(TransactionOutcome::Applied { sealed: false }),
        "a membership whose team lives in a partition not held here is no violation of this transaction"
    );
    assert_eq!(orphaned_memberships(&devices[1].replica).await, 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_replay_orphaning_a_row_of_a_held_parent_partition_still_being_restored_keeps_it() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let device = a_sealed_membership_whose_team_base_is_lost(&c, &root.path().join("d.db")).await;
    device
        .replica
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
        })
        .await
        .unwrap();
    for name in ["teams", "people", "notes", "memberships"] {
        device.replica.subscribe(c.scope(name)).await.unwrap();
    }
    let own_frames_not_back_since_a_reset = format!(
        "INSERT INTO secure_restoration_guards VALUES(?,7,X'{}')",
        "11".repeat(32)
    );
    execute(
        &device.replica,
        &own_frames_not_back_since_a_reset,
        Some(c.scope("teams")),
    )
    .await;
    device
        .replica
        .rebuild_projection(c.scope("memberships"))
        .await
        .unwrap();
    assert_eq!(orphaned_memberships(&device.replica).await, 1);
    device.replica.audit(c.scope("memberships")).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_valid_transaction_into_the_partition_of_a_tolerated_orphan_applies_as_on_its_peers() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    keep_an_orphaned_membership(&devices[1].replica, 1).await;
    let ids = devices[0]
        .commit(
            &c,
            &[
                "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,#next(teams),#next(people),'member',1,1,#id)",
                "INSERT INTO teams(tenant,name,id) VALUES(1,'ten',#id)",
                "INSERT INTO people(tenant,nickname,id) VALUES(1,'alpha',#id)",
            ],
        )
        .await
        .unwrap();
    let frames = devices[0].fresh().await;
    let statuses = devices[1].replica.receive_all(frames.clone()).await;
    assert_eq!(
        statuses.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec![ReceiveStatus::Applied; frames.len()]
    );
    let transaction = c.transaction(&frames);
    let applied = Some(TransactionOutcome::Applied { sealed: false });
    assert_eq!(
        devices[0]
            .replica
            .transaction_outcome(transaction)
            .await
            .unwrap(),
        applied
    );
    assert_eq!(
        devices[1]
            .replica
            .transaction_outcome(transaction)
            .await
            .unwrap(),
        applied,
        "an orphan that was already in tenant 1 is no violation of this transaction"
    );
    assert_eq!(orphaned_memberships(&devices[1].replica).await, 1);
    assert_eq!(memberships(&devices[1].replica).await, 2);
    let team = ids[1];
    devices[1]
        .commit(
            &c,
            &[
                &format!(
                    "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{team},#next(people),'owner',1,1,#id)"
                ),
                "INSERT INTO people(tenant,nickname,id) VALUES(1,'beta',#id)",
            ],
        )
        .await
        .expect("a local commit beside the orphan is no violation either");
    assert_eq!(memberships(&devices[1].replica).await, 3);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_transaction_orphaning_a_row_beside_a_tolerated_orphan_still_drops() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, ids, _) = devices(
        &c,
        root.path(),
        2,
        &[
            "INSERT INTO teams(tenant,name,id) VALUES(1,'ten',#id)",
            "INSERT INTO people(tenant,nickname,id) VALUES(1,'alpha',#id)",
        ],
    )
    .await;
    let (team, person) = (ids[0], ids[1]);
    keep_an_orphaned_membership(&devices[1].replica, 1).await;
    devices[1]
        .commit(
            &c,
            &[&format!("DELETE FROM teams WHERE tenant=1 AND id={team}")],
        )
        .await
        .unwrap();
    let deleted = devices[1].fresh().await;
    devices[0]
        .commit(
            &c,
            &[
                &format!(
                    "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{team},{person},'member',1,1,#id)"
                ),
                "INSERT INTO notes(tenant,message,id) VALUES(1,'joined',#id)",
            ],
        )
        .await
        .unwrap();
    let joined = devices[0].fresh().await;
    let clock = |frame: &EncryptedFrame| frame.operation_ref().unwrap().header.timestamp;
    assert!(
        joined
            .iter()
            .all(|frame| deleted.iter().all(|delete| clock(frame) > clock(delete))),
        "the membership replays after the delete of its team"
    );
    devices[1].receive(&joined).await;
    assert_eq!(
        devices[1]
            .replica
            .transaction_outcome(c.transaction(&joined))
            .await
            .unwrap(),
        Some(TransactionOutcome::Dropped {
            reason: DropReason::Constraint,
            sealed: false,
        }),
        "a membership of a team deleted before it is a violation the transaction introduces"
    );
    assert_eq!(orphaned_memberships(&devices[1].replica).await, 1);
    assert_eq!(memberships(&devices[1].replica).await, 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_assertion_failing_before_a_transaction_refuses_only_what_the_transaction_breaks() {
    let c = Constrained::new();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    let unbalanced = format!("X'{}'", "11".repeat(40));
    execute(
        &devices[1].replica,
        &format!("INSERT INTO orders(total,id) VALUES(7,{unbalanced})"),
        None,
    )
    .await;
    let ids = devices[0]
        .commit(
            &c,
            &[
                "INSERT INTO lines(parent,amount,id) VALUES(#next(orders),5,#id)",
                "INSERT INTO orders(total,id) VALUES(5,#id)",
            ],
        )
        .await
        .unwrap();
    let balanced = devices[0].fresh().await;
    devices[1].receive(&balanced).await;
    assert_eq!(
        devices[1]
            .replica
            .transaction_outcome(c.transaction(&balanced))
            .await
            .unwrap(),
        Some(TransactionOutcome::Applied { sealed: false }),
        "an order that was already unbalanced is no violation of this transaction"
    );
    let order = ids[1];
    for device in &devices {
        let error = device
            .commit(
                &c,
                &[&format!("UPDATE orders SET total=6 WHERE id={order}")],
            )
            .await
            .unwrap_err();
        assert_eq!(
            constraint_error(error),
            "assertion order_total failed",
            "unbalancing another order is a violation the transaction introduces"
        );
    }
}
