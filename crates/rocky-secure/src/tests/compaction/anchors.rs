//! Losing-seal coverage is retained accounting, never a state bound at a new seal.
use super::*;

#[tokio::test]
async fn empty_scope_coverage_survives_two_earlier_seals_and_an_items_only_import() {
    coverage_after_import(false, false).await;
}
#[tokio::test]
async fn a_missing_position_beyond_the_anchor_requires_a_listed_checkpoint() {
    coverage_after_import(true, false).await;
}
/// The same, with the first base switch interrupted and the restoration of
/// this node's own pruned suffix checked step by step.
#[tokio::test]
async fn an_import_that_leaves_own_pruned_frames_uncovered_waits_for_them() {
    coverage_after_import(true, true).await;
}
/// Rows of this node's restoration obligations.
async fn guards(replica: &Replica) -> i64 {
    scalar(replica, "SELECT count(*) FROM secure_evicted_logs").await
}
async fn coverage_after_import(missing: bool, restoration: bool) {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    sync(&lineage, &a, &b, &keys[1]).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let items = lineage.scope("items");
    let tags = lineage.scope("tags");
    a.set_retention_policy(
        items,
        crate::RetentionPolicy {
            required: BTreeSet::from([rocky_credentials::PeerId::of(keys[1].public_key())]),
            behind: BTreeSet::new(),
            copies: 1,
            window: 0,
        },
    )
    .await
    .unwrap();
    insert(&a, "first", "first").await;
    let first = seal(&lineage, &a).await;
    insert(&a, "second", "second").await;
    let second = seal(&lineage, &a).await;
    churn(&a, 12, "c").await;
    let losing = rivalled_seal(&lineage, &a).await;
    sync(&lineage, &a, &b, &keys[1]).await;
    a.acknowledge_frontier(
        keys[1].public_key(),
        &lineage.credentials(3, &keys[1]),
        &b.frontier(items).await.unwrap(),
    )
    .await
    .unwrap();
    assert!(a.compact(lineage.domain, 0).await.unwrap().pruned > 0);
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM secure_operations").await,
        0
    );
    let (winner, statements) = rival(&lineage, &losing);
    b.receive_tip_statements(&statements).await.unwrap();
    b.compact(lineage.domain, 0).await.unwrap();
    let records: Vec<_> = b
        .unlisted_checkpoints(lineage.domain)
        .await
        .unwrap()
        .into_iter()
        .filter(|record| record.table == items)
        .map(|record| {
            TipStatement::Checkpoint(Box::new(
                SignedCheckpointRecord::sign(record, lineage.publisher()).unwrap(),
            ))
        })
        .collect();
    assert_eq!(records.len(), 1);
    b.publish_tip_statements(&records).await.unwrap();
    a.receive_tip_statements(&crate::tests::all_tip_statements(&b).await)
        .await
        .unwrap();
    a.replay_at_seals(tags, false).await.unwrap();
    assert!(
        a.seal_rebootstraps(lineage.domain)
            .await
            .unwrap()
            .iter()
            .all(|need| need.scope != tags)
    );
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM secure_seal_anchors").await,
        1
    );
    drop(a);
    let mut a = lineage
        .reopen(&dir.path().join("a.db"), &keys[0], 3, 3)
        .await
        .unwrap();
    assert!(
        a.seal_rebootstraps(lineage.domain)
            .await
            .unwrap()
            .iter()
            .all(|need| need.scope != tags)
    );
    if restoration {
        // A switch that fails leaves the old base and no obligation.
        let credentials = lineage.credentials(3, &keys[0]);
        b.admit_peer(keys[0].public_key(), &credentials, a.followed_tip().await)
            .await
            .unwrap();
        let offered = b
            .checkpoint_offer(keys[0].public_key(), &credentials, items)
            .await
            .unwrap()
            .unwrap();
        let checkpoint = offered.id().unwrap();
        a.begin_rebootstrap(offered).await.unwrap();
        let mut after = None;
        loop {
            let page = b
                .checkpoint_page(
                    keys[0].public_key(),
                    &credentials,
                    items,
                    checkpoint,
                    after,
                    64 * 1024,
                )
                .await
                .unwrap();
            after = page.next;
            a.stage_checkpoint_page(page).await.unwrap();
            if after.is_none() {
                break;
            }
        }
        a.inject_stage_fault(crate::replica::checkpoint::Stage::Switch, 0)
            .await;
        assert!(matches!(
            a.finish_rebootstrap(items).await,
            Err(Error::InjectedFailure)
        ));
        assert_eq!(guards(&a).await, 0);
    }
    import_checkpoint(&lineage, &b, &a, &keys[0], items).await;
    if restoration {
        // The new base and the obligation for A's uncovered own frames
        // committed together.
        assert_eq!(guards(&a).await, 1);
    }
    assert!(a.finish_seal_rebootstrap(lineage.domain).await.unwrap());
    assert!(
        a.seal_rebootstraps(lineage.domain)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        a.tip_graph(lineage.domain)
            .await
            .unwrap()
            .checkpoints_at(winner.id().unwrap())
            .unwrap()
            .iter()
            .all(|record| record.statement.table != tags)
    );
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM secure_seal_anchors").await,
        1
    );
    a.finalize_path(lineage.domain).await.unwrap();
    let finalized = crate::storage::rows(
        &a.inner.lock().await.conn,
        "SELECT seal FROM secure_finalized ORDER BY depth",
    )
    .await
    .unwrap();
    assert_eq!(
        finalized,
        vec![
            vec![SqlValue::Blob(first.id().unwrap().to_vec())],
            vec![SqlValue::Blob(second.id().unwrap().to_vec())],
            vec![SqlValue::Blob(winner.id().unwrap().to_vec())]
        ]
    );
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM secure_seal_anchors").await,
        1
    );
    if missing {
        identity_insert(
            &session(&b).await,
            "INSERT INTO tags(key,label) VALUES('late','payload required')",
            vec![],
        )
        .await
        .unwrap();
        let later = seal(&lineage, &b).await.id().unwrap();
        a.receive_tip_statements(&crate::tests::all_tip_statements(&b).await)
            .await
            .unwrap();
        let status = a.compaction_status(lineage.domain).await.unwrap();
        assert!(
            status
                .waiting
                .unwrap()
                .contains("has no verified state at a seal of the path")
        );
        let needs = a.seal_rebootstraps(lineage.domain).await.unwrap();
        let need = needs.iter().find(|need| need.scope == tags).unwrap();
        assert_eq!(need.replay, crate::FrameReplay::Peers);
        assert!(need.listed.is_empty());
        b.compact(lineage.domain, 0).await.unwrap();
        list_checkpoints(&lineage, &b).await;
        a.receive_tip_statements(&crate::tests::all_tip_statements(&b).await)
            .await
            .unwrap();
        let needs = a.seal_rebootstraps(lineage.domain).await.unwrap();
        let need = needs.iter().find(|need| need.scope == tags).unwrap();
        assert_eq!(need.listed.len(), 1);
        let checkpoint = import_checkpoint(&lineage, &b, &a, &keys[0], tags).await;
        assert_eq!(need.listed, vec![checkpoint]);
        assert_eq!(
            scalar(&a, "SELECT count(*) FROM secure_seal_anchors").await,
            0
        );
        // A later items frame supplies this author's placement beyond the old anchor.
        insert(&b, "after", "after the new seal").await;
        if restoration {
            // Authoring waits for A's own frames, also across a reopen.
            assert!(
                identity_insert(
                    &session(&a).await,
                    "INSERT INTO items(key,title) VALUES(?,?)",
                    vec![text("early"), text("too early")],
                )
                .await
                .is_err()
            );
            drop(a);
            a = lineage
                .reopen(&dir.path().join("a.db"), &keys[0], 3, 3)
                .await
                .unwrap();
            assert_eq!(guards(&a).await, 1);
        }
        sync(&lineage, &b, &a, &keys[0]).await;
        if restoration {
            insert(&a, "restored", "own log is whole").await;
            assert_eq!(guards(&a).await, 0);
        }
        a.finalize_path(lineage.domain).await.unwrap();
        let status = a.compaction_status(lineage.domain).await.unwrap();
        assert_eq!(status.finalized, Some(later), "{status:?}");
        assert_eq!(scalar(&a, "SELECT count(*) FROM tags").await, 1);
        return;
    }
    a.compact(lineage.domain, 0).await.unwrap();
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM secure_seal_anchors").await,
        0
    );
    b.admit_peer(
        keys[0].public_key(),
        &lineage.credentials(3, &keys[0]),
        a.followed_tip().await,
    )
    .await
    .unwrap();
    a.admit_peer(
        keys[1].public_key(),
        &lineage.credentials(3, &keys[1]),
        b.followed_tip().await,
    )
    .await
    .unwrap();
    let empty = a
        .checkpoint_offer(
            keys[1].public_key(),
            &lineage.credentials(3, &keys[1]),
            tags,
        )
        .await
        .unwrap()
        .unwrap();
    assert_empty_checkpoint(&empty, winner.id().unwrap());
    assert_eq!(scalar(&a, "SELECT count(*) FROM tags").await, 0);
}

#[tokio::test]
async fn bounds_without_a_base_gain_coverage_before_the_settle_wipe() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = lineage
        .open_scoped(&dir.path().join("a.db"), &keys[0], false, &["items"])
        .await
        .unwrap();
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    sync(&lineage, &a, &b, &keys[1]).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let items = lineage.scope("items");
    let tags = lineage.scope("tags");
    a.set_retention_policy(
        items,
        crate::RetentionPolicy {
            required: BTreeSet::from([rocky_credentials::PeerId::of(keys[1].public_key())]),
            behind: BTreeSet::new(),
            copies: 1,
            window: 0,
        },
    )
    .await
    .unwrap();
    insert(&a, "x", "x").await;
    churn(&a, 12, "c").await;
    let losing = rivalled_seal(&lineage, &a).await;
    seal(&lineage, &a).await;
    a.finalize_path(lineage.domain).await.unwrap();
    a.renew_credentials(lineage.credentials(3, &keys[0]))
        .await
        .unwrap();
    a.install_key_epoch(
        lineage
            .config(3, &keys[0])
            .keys
            .into_iter()
            .find(|key| key.scope == tags)
            .unwrap(),
    )
    .await
    .unwrap();
    a.finalize_path(lineage.domain).await.unwrap();
    sync(&lineage, &a, &b, &keys[1]).await;
    a.acknowledge_frontier(
        keys[1].public_key(),
        &lineage.credentials(3, &keys[1]),
        &b.frontier(items).await.unwrap(),
    )
    .await
    .unwrap();
    let compacted = a.compact(lineage.domain, 1).await.unwrap();
    assert_eq!(compacted.compacted, Some(losing.id().unwrap()));
    assert!(compacted.pruned > 0);
    let tag_history=crate::storage::collect_rows(a.inner.lock().await.conn.query("SELECT (SELECT count(*) FROM secure_seal_bounds b WHERE b.scope=s.id),(SELECT count(*) FROM secure_checkpoints c WHERE c.scope=s.id) FROM secure_scope_state s WHERE scope=?",[encode(&tags).unwrap()]).await.unwrap()).await.unwrap();
    assert_eq!(
        tag_history,
        vec![vec![SqlValue::Integer(1), SqlValue::Integer(0)]]
    );
    let (winner, moves) = rival(&lineage, &losing);
    b.receive_tip_statements(&moves).await.unwrap();
    a.receive_tip_statements(&moves).await.unwrap();
    a.replay_at_seals(items, true).await.unwrap();
    sync(&lineage, &b, &a, &keys[0]).await;
    a.restored(items).await.unwrap();
    a.restored(items).await.unwrap();
    assert_eq!(
        a.compaction_status(lineage.domain)
            .await
            .unwrap()
            .rebootstrap,
        None
    );
    let anchored: Vec<ResourceScope> = crate::storage::rows(
        &a.inner.lock().await.conn,
        "SELECT s.scope FROM secure_seal_anchors a JOIN secure_scope_state s ON s.id=a.scope",
    )
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        let SqlValue::Blob(bytes) = &row[0] else {
            panic!("scope")
        };
        decode(bytes).unwrap()
    })
    .collect();
    assert_eq!(anchored, vec![tags]);

    a.finalize_path(lineage.domain).await.unwrap();
    assert_eq!(
        a.compaction_status(lineage.domain).await.unwrap().finalized,
        Some(winner.id().unwrap())
    );
    assert_eq!(scalar(&a, "SELECT count(*) FROM tags").await, 0);
}

#[tokio::test]
async fn restoration_endpoints_merge_as_pairs() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let a = open(&lineage, &dir, "a", &PrivateKey::new()).await;
    let items = lineage.scope("items");
    let (one, two) = (Hash::new(b"one"), Hash::new(b"two"));
    assert_eq!(
        a.merge_restoration_for_test(items, (5, Some(one)))
            .await
            .unwrap(),
        Some((5, Some(one)))
    );
    // A lower endpoint never replaces a higher one or its hash.
    assert_eq!(
        a.merge_restoration_for_test(items, (3, Some(two)))
            .await
            .unwrap(),
        Some((5, Some(one)))
    );
    // Two hashes at one sequence are corruption.
    assert!(
        a.merge_restoration_for_test(items, (5, Some(two)))
            .await
            .is_err()
    );
    assert_eq!(
        a.merge_restoration_for_test(items, (7, Some(two)))
            .await
            .unwrap(),
        Some((7, Some(two)))
    );
}
