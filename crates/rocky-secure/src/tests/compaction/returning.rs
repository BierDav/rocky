//! Exact old births return over a lower base without lowering permanent P.
use super::*;

// Publisher seed 74 and node seed 73 make frontier 20 beat frontier 25.
// Scope-log and domain coordinates below are independently asserted.
async fn returning(retained: bool, descending: bool) {
    let lineage = Lineage::pinned(74);
    let dir = TempDir::new().unwrap();
    let keys = [73, 76].map(|seed| PrivateKey::from_bytes(&[seed; 32]));
    let a_path = dir.path().join("a.db");
    let mut a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    let scope = lineage.scope("items");
    insert(&a, "first", "at zero").await;
    for position in 1..=25 {
        if position == 22 {
            insert(&a, "return", "birth at 22").await;
        } else {
            update(&a, "first", &format!("at {position}")).await;
        }
    }
    let mut frames = crate::tests::stored_frames(
        &a,
        "SELECT header,body FROM secure_operations ORDER BY sequence",
    )
    .await;
    frames.sort_by_key(|frame| frame.operation().unwrap().header.seq_num);
    assert_eq!(frames.len(), 26);
    let author = crate::tests::node_number(keys[0].public_key());
    assert_eq!(frames[22].operation().unwrap().header.seq_num, 22);
    assert_eq!(
        frames[22]
            .operation()
            .unwrap()
            .header
            .extensions
            .author_sequence,
        22
    );
    assert_eq!(
        frames[22].operation().unwrap().header.extensions.author,
        Some(author)
    );
    let endpoint = frames[25].id().unwrap();
    let tip = a.followed_tip().await;
    let losing = sign_seal(
        &lineage,
        tip,
        vec![AuthorMark {
            author,
            sequence: 25,
        }],
    );
    let winner = sign_seal(
        &lineage,
        tip,
        vec![AuthorMark {
            author,
            sequence: 20,
        }],
    );
    assert!(
        winner.id().unwrap() < losing.id().unwrap(),
        "pinned seal ordering"
    );
    publish_seal(&lineage, &a, losing).await;
    b.receive_tip_statements(&crate::tests::all_tip_statements(&a).await)
        .await
        .unwrap();
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    sync(&lineage, &a, &b, &keys[1]).await;
    a.set_retention_policy(
        scope,
        crate::RetentionPolicy {
            required: BTreeSet::from([rocky_credentials::PeerId::of(keys[1].public_key())]),
            behind: BTreeSet::new(),
            copies: 1,
            window: if retained { 5 } else { 0 },
        },
    )
    .await
    .unwrap();
    a.acknowledge_frontier(
        keys[1].public_key(),
        &lineage.credentials(3, &keys[1]),
        &b.frontier(scope).await.unwrap(),
    )
    .await
    .unwrap();
    assert!(a.compact(lineage.domain, 0).await.unwrap().pruned > 0);
    let journal = scalar(&a, "SELECT count(*) FROM secure_admission_entries").await;
    assert_eq!(journal, 2);
    assert_eq!(live_mark(&a, scope, author).await, 2);
    b.receive_tip_statements(&[
        TipStatement::Seal(winner.clone()),
        bookmark(&lineage, Some(tip), winner.successor().unwrap().unwrap()),
    ])
    .await
    .unwrap();
    b.compact(lineage.domain, 0).await.unwrap();
    list_checkpoints(&lineage, &b).await;
    a.receive_tip_statements(&crate::tests::all_tip_statements(&b).await)
        .await
        .unwrap();
    let report = import_checkpoint_report(&lineage, &b, &a, &keys[0], scope).await;
    // The empty tags scope's own base sat at the losing seal too.
    a.replay_at_seals(lineage.scope("tags"), false)
        .await
        .unwrap();
    assert!(
        a.seal_rebootstraps(lineage.domain)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(live_mark(&a, scope, author).await, 2);
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM secure_admission_entries").await,
        journal
    );
    if retained {
        assert!(
            report.reapplied.contains(&frames[22].id().unwrap()),
            "{report:?}"
        );
        assert!(report.rejected.is_empty(), "{report:?}");
    } else {
        assert_eq!(
            scalar(&a, "SELECT count(*) FROM secure_evicted_logs").await,
            1
        );
        let order = if descending {
            vec![25, 24, 23, 22, 21]
        } else {
            vec![21, 22, 23, 24, 25]
        };
        for (index, position) in order.into_iter().enumerate() {
            a.receive(frames[position].clone()).await.unwrap();
            a.restored(scope).await.unwrap();
            if index < 4 {
                assert_eq!(
                    scalar(&a, "SELECT count(*) FROM secure_evicted_logs").await,
                    1
                );
                assert!(
                    identity_insert(
                        &session(&a).await,
                        "INSERT INTO items(key,title) VALUES('blocked','before endpoint')",
                        vec![]
                    )
                    .await
                    .is_err()
                );
            }
            drop(a);
            a = lineage.reopen(&a_path, &keys[0], 3, 3).await.unwrap();
            a.audit(scope).await.unwrap();
        }
        let log = a.frontier(scope).await.unwrap();
        let own = log.log(keys[0].public_key()).unwrap();
        assert_eq!(own.held, 26);
        assert_eq!(frames[25].id().unwrap(), endpoint);
        assert_eq!(
            scalar(&a, "SELECT count(*) FROM secure_evicted_logs").await,
            0
        );
    }
    assert_eq!(
        titles(&a).await,
        vec![
            vec![text("first"), text("at 25")],
            vec![text("return"), text("birth at 22")]
        ]
    );
    a.rebuild_projection(scope).await.unwrap();
    a.audit(scope).await.unwrap();
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM secure_admission_entries").await,
        journal
    );
    insert(&a, "after", "after exact endpoint").await;
    assert_eq!(live_mark(&a, scope, author).await, 3);
}

#[tokio::test]
async fn an_exact_birth_at_22_reapplies_with_its_retained_suffix() {
    returning(true, false).await;
}
#[tokio::test]
async fn an_exact_pruned_birth_at_22_returns_in_reverse_order_through_25() {
    returning(false, true).await;
}
#[tokio::test]
async fn an_exact_pruned_birth_at_22_returns_in_order_through_25() {
    returning(false, false).await;
}
