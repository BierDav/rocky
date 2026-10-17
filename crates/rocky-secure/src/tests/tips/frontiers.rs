//! Frontier caches contain committed state and survive only audited bookkeeping.
use super::*;
mod headers;

async fn checked(replica: &Replica, scope: ResourceScope, count: u64) {
    let cached = replica.frontier(scope).await.unwrap();
    assert_eq!(
        cached.logs.iter().map(|log| log.applied).sum::<u64>(),
        count
    );
    let inner = replica.inner.lock().await;
    assert_eq!(cached, inner.compute_tip_frontier(scope).await.unwrap());
    assert_eq!(inner.frontier(scope).await.unwrap(), cached);
}

#[tokio::test]
async fn switching_back_and_sideways_invalidates_warmed_inherited_frontiers() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let identity = PrivateKey::new();
    let (replica, identity_chain) = identity_replica(
        &f,
        &dir.path().join("branches.db"),
        &identity,
        &PrivateKey::new(),
    )
    .await;
    main(&f, &replica).await;
    insert(&f, &replica, 1, "common").await;
    let signed = seal(&f, &replica).await;
    let (_, mut statements) = branch(&f, &signed);
    statements.push(TipStatement::Seal(signed));
    checked(&replica, f.public, 1).await;
    replica.receive_tip_statements(&statements).await.unwrap();
    replica
        .follow_bookmark(f.public.domain, "dev")
        .await
        .unwrap();
    let node = PrivateKey::new();
    replica
        .register_line_node(LineNode {
            credentials: identity_chain
                .complete(&identity, node.public_key(), 2)
                .unwrap(),
            key: node,
        })
        .await
        .unwrap();
    insert(&f, &replica, 2, "dev").await;
    checked(&replica, f.public, 2).await;
    // A failed follow must not damage the warmed state.
    assert!(
        replica
            .follow_bookmark(f.public.domain, "missing")
            .await
            .is_err()
    );
    checked(&replica, f.public, 2).await;
    replica
        .follow_bookmark(f.public.domain, "main")
        .await
        .unwrap();
    checked(&replica, f.public, 1).await;
    replica
        .follow_bookmark(f.public.domain, "dev")
        .await
        .unwrap();
    checked(&replica, f.public, 2).await;
    replica
        .follow_bookmark(f.public.domain, "main")
        .await
        .unwrap();
    checked(&replica, f.public, 1).await;
}

#[tokio::test]
async fn equal_size_revocation_replacements_and_graph_reload_invalidate_without_sql_changes() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("graphs.db"), &PrivateKey::new()).await;
    insert(&f, &replica, 1, "source").await;
    let source = replica.followed_tip().await;
    let statement = seal(&f, &replica).await.statement;
    let mut seals = Vec::new();
    let mut policies = Vec::new();
    for _ in 0..2 {
        let issuer = PrivateKey::new();
        let chain = CredentialChain(vec![
            Authority::owner(&f.publisher)
                .issue_issuer(issuer.public_key(), None, validity())
                .unwrap(),
        ]);
        seals.push(SignedSeal::endorse(statement.clone(), &issuer, &chain).unwrap());
        policies.push(crate::tests::revoking(
            &f.publisher,
            f.public.domain.app,
            &[issuer.public_key()],
            &[],
        ));
    }
    replica
        .receive_tip_statements(
            &seals
                .iter()
                .cloned()
                .map(TipStatement::Seal)
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    // The workspace stays at the same source; the winner changes in memory.
    let mut inner = replica.inner.lock().await;
    let original = inner.frontier(f.public).await.unwrap();
    let writes: i64 = match rows(&inner.conn, "SELECT total_changes()").await.unwrap()[0][0] {
        rocky_core::SqlValue::Integer(value) => value,
        _ => panic!("integer"),
    };
    for (index, policy) in policies.iter().enumerate() {
        let before = inner.frontier_cache_stats();
        assert!(inner.refresh_frontier_revocations(policy).await.unwrap());
        assert!(inner.frontier_cache_stats().0 > before.0);
        assert_eq!(inner.frontier_cache_stats().1, 0);
        let graph = &inner.graphs[&f.public.domain];
        assert_eq!(graph.revoked.len(), 1);
        assert_eq!(graph.seal(source).unwrap(), Some(&seals[1 - index]));
        assert_eq!(inner.frontier(f.public).await.unwrap(), original);
        assert_eq!(
            inner.compute_tip_frontier(f.public).await.unwrap(),
            original
        );
    }
    assert_eq!(
        rows(&inner.conn, "SELECT total_changes()").await.unwrap()[0][0],
        rocky_core::SqlValue::Integer(writes)
    );
    inner.reload_frontier_graphs().await.unwrap();
    assert_eq!(inner.frontier_cache_stats().1, 0);
    assert_eq!(inner.frontier(f.public).await.unwrap(), original);
}

#[tokio::test]
async fn failed_graph_admission_restores_the_warmed_workspace() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("admission.db"), &PrivateKey::new()).await;
    main(&f, &replica).await;
    insert(&f, &replica, 1, "source").await;
    let original = replica.tip_graph(f.public.domain).await.unwrap();
    let tip = replica.followed_tip().await;
    let signed = seal(&f, &replica).await;
    let statements = [
        TipStatement::Seal(signed.clone()),
        bookmark(&f, "main", Some(tip), signed.successor().unwrap()),
    ];
    checked(&replica, f.public, 1).await;
    replica.fail_publication_after(3).await;
    assert!(replica.receive_tip_statements(&statements).await.is_err());
    assert_eq!(replica.inner.lock().await.frontier_cache_stats().1, 0);
    assert_eq!(replica.tip_graph(f.public.domain).await.unwrap(), original);
    assert_eq!(replica.followed_tip().await, tip);
    checked(&replica, f.public, 1).await;
    replica.receive_tip_statements(&statements).await.unwrap();
    checked(&replica, f.public, 1).await;
    assert_ne!(replica.followed_tip().await, tip);
}

#[tokio::test]
async fn pending_bundle_and_missing_prefix_invalidate_cached_readiness() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let source = open(&f, &dir.path().join("source.db"), &PrivateKey::new()).await;
    let target = open(&f, &dir.path().join("target.db"), &PrivateKey::new()).await;
    let writes = (1..=40)
        .map(|row| RowWrite::changed(change(f.public, row, None, Some(f.app().name("bundle")))))
        .collect();
    let frames = source.author_bundle([4; 32], writes, vec![]).await.unwrap();
    assert_eq!(frames.len(), 2);
    assert!(target.frontier(f.public).await.unwrap().logs.is_empty());
    assert_eq!(
        target.receive(frames[1].clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    assert!(
        target.frontier(f.public).await.unwrap().logs.is_empty(),
        "a gap cannot become a prefix"
    );
    let pending = open(&f, &dir.path().join("pending.db"), &PrivateKey::new()).await;
    pending.frontier(f.public).await.unwrap();
    assert_eq!(
        pending.receive(frames[0].clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    let partial = pending.frontier(f.public).await.unwrap();
    assert_eq!((partial.logs[0].held, partial.logs[0].applied), (1, 0));
    for (replica, frame) in [(&target, &frames[0]), (&pending, &frames[1])] {
        replica.receive(frame.clone()).await.unwrap();
        checked(replica, f.public, 2).await;
        assert_eq!(replica.snapshot(f.public).await.unwrap().rows.len(), 40);
    }
}

#[tokio::test]
async fn a_signed_void_fills_a_cached_gap_without_applying_its_payload() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let source = open(&f, &dir.path().join("void-source.db"), &key).await;
    let target = open(&f, &dir.path().join("void-target.db"), &PrivateKey::new()).await;
    let cancelled = insert(&f, &source, 1, "cancelled").await;
    let kept = insert(&f, &source, 2, "kept").await;
    target.receive(kept).await.unwrap();
    assert!(target.frontier(f.public).await.unwrap().logs.is_empty());
    let void = crate::SignedVoid::sign(&key, vec![cancelled.header_bytes().to_vec()]).unwrap();
    let results = target.receive_voids(&[void]).await;
    for result in results {
        result.unwrap();
    }
    checked(&target, f.public, 2).await;
    let rows = target.snapshot(f.public).await.unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows.values().next().unwrap(), &f.app().name("kept"));
}

#[tokio::test]
async fn rollback_and_retry_never_publish_transactional_frontiers() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("retry.db"), &PrivateKey::new()).await;
    main(&f, &replica).await;
    insert(&f, &replica, 1, "one").await;
    insert(&f, &replica, 2, "two").await;
    let committed = replica.frontier(f.public).await.unwrap();
    assert_eq!(committed.logs[0].applied, 2);
    let mut inner = replica.inner.lock().await;
    let generation = inner.frontier_cache_stats().0;
    let failed: Result<(), Error> = transaction!(inner, |inner| {
        inner
            .conn
            .execute(
                "UPDATE secure_operations SET status='pending' WHERE sequence=1",
                (),
            )
            .await?;
        let pending = inner.frontier(f.public).await?;
        assert_eq!(pending.logs[0].held, 2);
        assert_eq!(pending.logs[0].applied, 1);
        assert_eq!(inner.frontier_cache_stats().1, 0);
        Err(Error::Corrupt("injected after transactional read".into()))
    });
    assert!(failed.is_err());
    assert!(inner.frontier_cache_stats().0 > generation);
    assert_eq!(inner.frontier_cache_stats().1, 0);
    assert_eq!(inner.frontier(f.public).await.unwrap(), committed);
    let mut attempts = 0;
    transaction!(inner, |inner| {
        attempts += 1;
        inner
            .conn
            .execute(
                "UPDATE secure_operations SET status='pending' WHERE sequence=1",
                (),
            )
            .await?;
        assert_eq!(inner.frontier(f.public).await?.logs[0].applied, 1);
        assert_eq!(inner.frontier_cache_stats().1, 0);
        if attempts == 1 {
            return Err(Error::from(crate::sqlite::Error::Busy(
                "retry after write and read".into(),
            )));
        }
        inner
            .conn
            .execute(
                "UPDATE secure_operations SET status='applied' WHERE sequence=1",
                (),
            )
            .await?;
        Ok(())
    })
    .unwrap();
    assert_eq!(attempts, 2);
    assert_eq!(inner.frontier(f.public).await.unwrap(), committed);
    assert_eq!(
        inner.compute_tip_frontier(f.public).await.unwrap(),
        committed
    );
}

#[tokio::test]
async fn a_failed_commit_discards_the_transactional_frontier() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("commit.db"), &PrivateKey::new()).await;
    main(&f, &replica).await;
    insert(&f, &replica, 1, "one").await;
    let committed = replica.frontier(f.public).await.unwrap();
    let mut inner = replica.inner.lock().await;
    inner
        .conn
        .execute("CREATE TEMP TABLE cache_parent(id INTEGER PRIMARY KEY)", ())
        .await
        .unwrap();
    inner.conn.execute("CREATE TEMP TABLE cache_child(id INTEGER REFERENCES cache_parent(id) DEFERRABLE INITIALLY DEFERRED)", ()).await.unwrap();
    let result: Result<(), Error> = transaction!(inner, |inner| {
        inner
            .conn
            .execute("INSERT INTO cache_child VALUES(1)", ())
            .await?;
        inner
            .conn
            .execute("UPDATE secure_operations SET status='pending'", ())
            .await?;
        assert_eq!(inner.frontier(f.public).await?.logs[0].applied, 0);
        assert_eq!(inner.frontier_cache_stats().1, 0);
        Ok(())
    });
    assert!(result.is_err(), "deferred constraint must fail at COMMIT");
    assert!(inner.conn.connection().is_autocommit().unwrap());
    assert_eq!(inner.frontier_cache_stats().1, 0);
    assert_eq!(inner.frontier(f.public).await.unwrap(), committed);
}

#[tokio::test]
async fn offering_pages_and_recording_acknowledgments_preserve_one_scan() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&f, &dir.path().join("pages.db"), &PrivateKey::new()).await;
    main(&f, &replica).await;
    for row in 1..=66 {
        replica
            .author_buffered(
                [row; 32],
                vec![change(f.public, row, None, Some(f.app().name("row")))],
                vec![],
            )
            .await
            .unwrap();
    }
    let (commit, durable, _) = replica.watermark().await;
    assert!(
        commit > durable,
        "the serving fixture must include buffered writes"
    );
    let peer = PrivateKey::new();
    let credentials = f.credentials(&peer, f.full());
    replica
        .admit_peer(
            peer.public_key(),
            &credentials,
            replica.followed_tip().await,
        )
        .await
        .unwrap();
    let before = {
        let inner = replica.inner.lock().await;
        inner.stored_header_memo_limits(1 << 18, 32 * 1024 * 1024);
        inner.frontier_cache_stats()
    };
    let request = ScopeFrontier {
        scope: f.public,
        logs: vec![],
    };
    let mut after = None;
    let mut count = 0;
    loop {
        let page = replica
            .range_page(
                peer.public_key(),
                &credentials,
                &request,
                after,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        let (commit, durable, _) = replica.watermark().await;
        assert_eq!(
            commit, durable,
            "a cache hit cannot bypass the page's flush"
        );
        count += page.frames.len();
        for frame in &page.frames {
            replica
                .acknowledge(peer.public_key(), &credentials, frame.id().unwrap(), true)
                .await
                .unwrap();
        }
        replica
            .acknowledge_frontier(peer.public_key(), &credentials, &page.frontier)
            .await
            .unwrap();
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(count, 66);
    let warmed = replica.inner.lock().await.frontier_cache_stats();
    assert_eq!(warmed.2 - before.2, 1);
    assert_eq!(warmed.3 - before.3, 66);
    // A hit never bypasses authorization or the current peer tip.
    let denied = f.credentials(&peer, vec![]);
    assert!(
        replica
            .range_page(peer.public_key(), &denied, &request, None, MAX_SECURE_FRAME)
            .await
            .is_err()
    );
    replica
        .admit_peer(peer.public_key(), &credentials, TipId([91; 32]))
        .await
        .unwrap();
    assert!(
        replica
            .range_page(
                peer.public_key(),
                &credentials,
                &request,
                None,
                MAX_SECURE_FRAME
            )
            .await
            .is_err()
    );
    insert(&f, &replica, 67, "new history").await;
    let fresh = replica.frontier(f.public).await.unwrap();
    assert_eq!(fresh.logs[0].held, 67);
    let refreshed = replica.inner.lock().await.frontier_cache_stats();
    assert_eq!(refreshed.2, warmed.2 + 1);
    replica.frontier(f.public).await.unwrap();
    assert_eq!(
        replica.inner.lock().await.frontier_cache_stats().2,
        refreshed.2
    );
}
