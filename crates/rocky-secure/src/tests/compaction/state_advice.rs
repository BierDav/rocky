use super::*;
use crate::{FrontierLog, ScopeFrontier};

#[tokio::test]
async fn state_advice_requires_64_frames_and_covers_every_requester_log() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    for n in 0..64 {
        insert(&a, &n.to_string(), "row").await;
    }
    let closing = seal(&lineage, &a).await.id().unwrap();
    a.compact(lineage.domain, 0).await.unwrap();
    list_checkpoints(&lineage, &a).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let credentials = lineage.credentials(3, &keys[1]);
    a.admit_peer(keys[1].public_key(), &credentials, b.followed_tip().await)
        .await
        .unwrap();
    let scope = lineage.scope("items");
    let request = ScopeFrontier {
        scope,
        logs: vec![],
    };
    let page = a
        .range_page(
            keys[1].public_key(),
            &credentials,
            &request,
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(!page.behind);
    assert_eq!(page.frames.len(), 32, "advice keeps the ordinary page");
    let advice = page.state.unwrap();
    assert_eq!(advice.seal, closing);
    assert_eq!(advice.rows, 64);
    let base = a
        .checkpoint_offer(keys[1].public_key(), &credentials, scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(advice.checkpoint, base.id().unwrap());
    assert_eq!(advice.signer, base.signer());
    let author = base.statement.frontier[0].author;
    for held in [1, 32, 64, 65] {
        let request = ScopeFrontier {
            scope,
            logs: vec![FrontierLog {
                author,
                held,
                applied: held,
            }],
        };
        let page = a
            .range_page(
                keys[1].public_key(),
                &credentials,
                &request,
                None,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        assert!(page.state.is_none(), "held={held}");
    }
    let unknown = ScopeFrontier {
        scope,
        logs: vec![FrontierLog {
            author: keys[1].public_key(),
            held: 0,
            applied: 0,
        }],
    };
    assert!(
        a.range_page(
            keys[1].public_key(),
            &credentials,
            &unknown,
            None,
            MAX_SECURE_FRAME
        )
        .await
        .unwrap()
        .state
        .is_none()
    );
    // Missing read keys must not prevent serving retained ciphertext frames.
    a.inner.lock().await.config.keys.clear();
    let page = a
        .range_page(
            keys[1].public_key(),
            &credentials,
            &request,
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(page.state.is_none());
    assert_eq!(page.frames.len(), 32);
}

#[tokio::test]
async fn advised_import_reapplies_a_write_authored_after_the_frontier_request() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    for n in 0..64 {
        insert(&a, &n.to_string(), "row").await;
    }
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    list_checkpoints(&lineage, &a).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let (mut session, mut serving) = crate::network::link::testing::sessions(
        (&b, keys[1].clone(), lineage.credentials(3, &keys[1])),
        (&a, keys[0].clone(), lineage.credentials(3, &keys[0])),
    )
    .await
    .unwrap();
    let scope = lineage.scope("items");
    let (served, ()) = tokio::join!(serving.serve_to_end(&a), async {
        let advice = session
            .catch_up_with_state(&b, scope, MAX_SECURE_FRAME / 2, true)
            .await
            .unwrap()
            .state
            .unwrap();
        let mut replaced = advice.clone();
        replaced.checkpoint = [0; 32];
        assert!(
            session
                .bootstrap_advised_scope(&b, scope, MAX_SECURE_FRAME / 2, Some(&replaced))
                .await
                .is_err()
        );
        assert!(b.rebootstrap_progress(scope).await.unwrap().is_none());
        // Local work can commit while the peer's response is in flight.
        insert(&b, "unsent", "local").await;
        let report = session
            .bootstrap_advised_scope(&b, scope, MAX_SECURE_FRAME / 2, Some(&advice))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.rebootstrap.unwrap().checkpoint, advice.checkpoint);
        assert_eq!(report.catch_up.frames, 0);
        assert_eq!(scalar(&b, "SELECT count(*) FROM items").await, 65);
        session.push(&b, scope, MAX_SECURE_FRAME / 2).await.unwrap();
        session.close().await.unwrap();
    });
    served.unwrap();
    assert_eq!(titles(&a).await, titles(&b).await);
    assert!(titles(&b).await.iter().any(|row| row[1] == text("local")));
}

#[tokio::test]
async fn a_cached_writable_tip_refuses_writes_after_its_bookmark_is_deleted() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let a = open(&lineage, &dir, "writer", &PrivateKey::new()).await;
    follow_main(&lineage, &a).await;
    insert(&a, "before", "warm the writability cache").await;
    let tip = a.followed_tip().await;
    let deleted = SignedBookmarkMove::sign(
        BookmarkMoveStatement {
            format: BOOKMARK_MOVE_FORMAT,
            scope: lineage.domain,
            name: "main".into(),
            from: Some(tip),
            to: None,
        },
        lineage.publisher(),
    )
    .unwrap();
    a.publish_tip_statements(&[TipStatement::Bookmark(deleted)])
        .await
        .unwrap();
    assert_eq!(a.followed_tip().await, tip);
    assert!(
        identity_insert(
            &session(&a).await,
            "INSERT INTO items(key,title) VALUES(?,?)",
            vec![text("after"), text("refused")]
        )
        .await
        .is_err()
    );
    assert_eq!(scalar(&a, "SELECT count(*) FROM items").await, 1);
}
