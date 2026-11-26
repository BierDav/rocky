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

async fn foreign_key_violations(replica: &EncryptedReplica<TypedRows>) -> usize {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(&inner.conn, "SELECT * FROM pragma_foreign_key_check")
        .await
        .unwrap()
        .len()
}

async fn has_rederiving_blocker(replica: &EncryptedReplica<TypedRows>) -> bool {
    replica
        .blockers()
        .await
        .unwrap()
        .iter()
        .any(|blocker| matches!(blocker.kind, crate::BlockerKind::Rederiving { .. }))
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
async fn a_replay_orphaning_a_row_of_a_parent_partition_not_held_here_withholds_it_and_blocks() {
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
    assert_eq!(orphaned_memberships(&device.replica).await, 0);
    assert_eq!(foreign_key_violations(&device.replica).await, 0);
    assert!(has_rederiving_blocker(&device.replica).await);
}

async fn withhold_child_in_tenant(c: &Constrained, root: &Path, target: &Device, tenant: i64) {
    let other = c.for_tenant(tenant);
    let mut writer = other.open(&root.join(format!("tenant-{tenant}.db"))).await;
    let team = writer
        .commit(
            &other,
            &[&format!(
                "INSERT INTO teams(tenant,name,id) VALUES({tenant},'other',#id)"
            )],
        )
        .await
        .unwrap()[0];
    let _held = writer.fresh().await;
    let own_frames_not_back_since_a_reset = format!(
        "INSERT INTO secure_restoration_guards VALUES(?,7,X'{}')",
        "11".repeat(32)
    );
    execute(
        &target.replica,
        &own_frames_not_back_since_a_reset,
        Some(other.scope("teams")),
    )
    .await;
    writer
        .commit(
            &other,
            &[
                &format!(
                    "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES({tenant},{team},#next(people),'member',1,1,#id)"
                ),
                &format!(
                    "INSERT INTO people(tenant,nickname,id) VALUES({tenant},'beta',#id)"
                ),
            ],
        )
        .await
        .unwrap();
    let withheld = writer.fresh().await;
    target.receive(&withheld).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_withheld_child_in_another_partition_drops_no_replayed_transaction() {
    let c = graph().with_extra_tenant(2);
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    withhold_child_in_tenant(&c, root.path(), &devices[1], 2).await;
    assert_eq!(foreign_key_violations(&devices[1].replica).await, 0);
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
        "a transaction in tenant 1 applies beside a withheld child of tenant 2"
    );
    assert_eq!(
        devices[1]
            .replica
            .transaction_outcome(c.transaction(&frames))
            .await
            .unwrap(),
        Some(TransactionOutcome::Applied { sealed: false }),
    );
    assert_eq!(orphaned_memberships(&devices[1].replica).await, 0);
    assert_eq!(foreign_key_violations(&devices[1].replica).await, 0);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_replay_orphaning_a_row_of_a_held_parent_partition_still_being_restored_withholds_it_and_blocks()
 {
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
    assert_eq!(orphaned_memberships(&device.replica).await, 0);
    assert_eq!(foreign_key_violations(&device.replica).await, 0);
    assert!(has_rederiving_blocker(&device.replica).await);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_valid_transaction_into_the_partition_of_a_withheld_child_applies_as_on_its_peers() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, ids, _) = devices(
        &c,
        root.path(),
        3,
        &["INSERT INTO teams(tenant,name,id) VALUES(1,'ten2',#id)"],
    )
    .await;
    let team2 = ids[0];
    let own_frames_not_back_since_a_reset = format!(
        "INSERT INTO secure_restoration_guards VALUES(?,7,X'{}')",
        "11".repeat(32)
    );
    execute(
        &devices[1].replica,
        &own_frames_not_back_since_a_reset,
        Some(c.scope("teams")),
    )
    .await;
    let _withheld = a_membership_of_a_team_this_device_never_received(&c, &mut devices).await;
    assert_eq!(foreign_key_violations(&devices[1].replica).await, 0);
    devices[2]
        .commit(
            &c,
            &[
                &format!(
                    "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{team2},#next(people),'member',1,1,#id)"
                ),
                "INSERT INTO people(tenant,nickname,id) VALUES(1,'gamma',#id)",
            ],
        )
        .await
        .unwrap();
    let frames = devices[2].fresh().await;
    let statuses = devices[1].replica.receive_all(frames.clone()).await;
    assert_eq!(
        statuses.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec![ReceiveStatus::Applied; frames.len()]
    );
    let transaction = c.transaction(&frames);
    let applied = Some(TransactionOutcome::Applied { sealed: false });
    assert_eq!(
        devices[2]
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
        "a valid transaction applies beside a withheld child in the same partition"
    );
    assert_eq!(orphaned_memberships(&devices[1].replica).await, 0);
    assert_eq!(foreign_key_violations(&devices[1].replica).await, 0);
    assert_eq!(memberships(&devices[1].replica).await, 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_transaction_orphaning_a_row_beside_a_withheld_child_in_another_partition_still_drops() {
    let c = graph().with_extra_tenant(2);
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
    withhold_child_in_tenant(&c, root.path(), &devices[1], 2).await;
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
    assert_eq!(orphaned_memberships(&devices[1].replica).await, 0);
    assert_eq!(foreign_key_violations(&devices[1].replica).await, 0);
    assert_eq!(memberships(&devices[1].replica).await, 0);
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

async fn a_membership_of_a_team_this_device_never_received(
    c: &Constrained,
    devices: &mut [Device],
) -> Commitment {
    let team = devices[0]
        .commit(
            c,
            &["INSERT INTO teams(tenant,name,id) VALUES(1,'ten',#id)"],
        )
        .await
        .unwrap()[0];
    let team_frames_kept_from_device_1 = devices[0].fresh().await;
    assert!(!team_frames_kept_from_device_1.is_empty());
    devices[0]
        .commit(
            c,
            &[
                &format!(
                    "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{team},#next(people),'member',1,1,#id)"
                ),
                "INSERT INTO people(tenant,nickname,id) VALUES(1,'alpha',#id)",
            ],
        )
        .await
        .unwrap();
    let joined = devices[0].fresh().await;
    devices[1].receive(&joined).await;
    c.transaction(&joined)
}

async fn outcome(device: &Device, transaction: Commitment) -> Option<TransactionOutcome> {
    device
        .replica
        .transaction_outcome(transaction)
        .await
        .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_new_reference_into_a_parent_scope_not_held_here_applies_as_a_rebuild_keeps_it() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    devices[1]
        .replica
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
        })
        .await
        .unwrap();
    for name in ["people", "notes", "memberships"] {
        devices[1].replica.subscribe(c.scope(name)).await.unwrap();
    }
    let transaction = a_membership_of_a_team_this_device_never_received(&c, &mut devices).await;
    let applied = Some(TransactionOutcome::Applied { sealed: false });
    assert_eq!(outcome(&devices[0], transaction).await, applied);
    assert_eq!(
        outcome(&devices[1], transaction).await,
        applied,
        "a membership of a team in a scope not held here is not dropped as a constraint violation"
    );
    assert_eq!(orphaned_memberships(&devices[1].replica).await, 0);
    assert_eq!(foreign_key_violations(&devices[1].replica).await, 0);
    assert!(has_rederiving_blocker(&devices[1].replica).await);
    device_rebuild_keeps_zero_orphans(&c, &devices[1], transaction).await;
}

async fn device_rebuild_keeps_zero_orphans(
    c: &Constrained,
    device: &Device,
    transaction: Commitment,
) {
    device
        .replica
        .rebuild_projection(c.scope("memberships"))
        .await
        .unwrap();
    assert_eq!(orphaned_memberships(&device.replica).await, 0);
    assert_eq!(foreign_key_violations(&device.replica).await, 0);
    assert_eq!(
        outcome(device, transaction).await,
        Some(TransactionOutcome::Applied { sealed: false })
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_new_reference_into_a_parent_scope_still_being_restored_withholds_until_parent_arrives() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    let own_frames_not_back_since_a_reset = format!(
        "INSERT INTO secure_restoration_guards VALUES(?,7,X'{}')",
        "11".repeat(32)
    );
    execute(
        &devices[1].replica,
        &own_frames_not_back_since_a_reset,
        Some(c.scope("teams")),
    )
    .await;
    let team = devices[0]
        .commit(
            &c,
            &["INSERT INTO teams(tenant,name,id) VALUES(1,'ten',#id)"],
        )
        .await
        .unwrap()[0];
    let team_frames = devices[0].fresh().await;
    devices[0]
        .commit(
            &c,
            &[
                &format!(
                    "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{team},#next(people),'member',1,1,#id)"
                ),
                "INSERT INTO people(tenant,nickname,id) VALUES(1,'alpha',#id)",
            ],
        )
        .await
        .unwrap();
    let joined = devices[0].fresh().await;
    devices[1].receive(&joined).await;
    let transaction = c.transaction(&joined);
    assert_eq!(
        outcome(&devices[1], transaction).await,
        Some(TransactionOutcome::Applied { sealed: false }),
        "a held team scope with a restoration guard is not settled"
    );
    assert_eq!(orphaned_memberships(&devices[1].replica).await, 0);
    assert_eq!(foreign_key_violations(&devices[1].replica).await, 0);
    assert!(has_rederiving_blocker(&devices[1].replica).await);
    device_rebuild_keeps_zero_orphans(&c, &devices[1], transaction).await;
    execute(
        &devices[1].replica,
        "DELETE FROM secure_restoration_guards",
        None,
    )
    .await;
    devices[1].receive(&team_frames).await;
    devices[1].replica.rederive().await.unwrap();
    assert!(!has_rederiving_blocker(&devices[1].replica).await);
    assert_eq!(memberships(&devices[1].replica).await, 1);
    assert_eq!(foreign_key_violations(&devices[1].replica).await, 0);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_new_reference_to_a_missing_row_of_a_held_and_settled_parent_scope_drops() {
    let c = graph();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    let transaction = a_membership_of_a_team_this_device_never_received(&c, &mut devices).await;
    assert_eq!(
        outcome(&devices[1], transaction).await,
        Some(TransactionOutcome::Dropped {
            reason: DropReason::Constraint,
            sealed: false,
        }),
        "a membership of a team missing from a held, settled scope is a violation"
    );
    assert_eq!(memberships(&devices[1].replica).await, 0);
}
