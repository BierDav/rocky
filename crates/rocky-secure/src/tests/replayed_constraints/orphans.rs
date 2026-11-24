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
    let receiver = &devices[1].replica;
    let missing = format!("X'{}'", "00".repeat(40));
    execute(receiver, "PRAGMA foreign_keys=OFF", None).await;
    execute(
        receiver,
        &format!(
            "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(2,{missing},{missing},'member',1,1,{missing})"
        ),
        None,
    )
    .await;
    execute(receiver, "PRAGMA foreign_keys=ON", None).await;
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
