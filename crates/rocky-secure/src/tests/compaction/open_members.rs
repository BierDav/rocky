use super::super::generations::{identity_insert, session};
use super::*;
use crate::tests::exchange_graphs;

async fn frames_of(
    lineage: &Lineage,
    source: &Replica,
    target: &Replica,
    key: &PrivateKey,
    scope: ResourceScope,
) -> Vec<crate::EncryptedFrame> {
    let credentials = lineage.credentials(3, key);
    source
        .admit_peer(key.public_key(), &credentials, target.followed_tip().await)
        .await
        .unwrap();
    let page = source
        .range_page(
            key.public_key(),
            &credentials,
            &target.frontier(scope).await.unwrap(),
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(page.next.is_none());
    let mut frames: Vec<_> = page
        .frames
        .into_iter()
        .filter(|frame| frame.scope().unwrap() == scope)
        .collect();
    frames.sort_by_key(|frame| frame.operation_ref().unwrap().header.seq_num);
    frames
}

async fn stored(replica: &Replica, sql: &str) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(&inner.conn, sql).await.unwrap()
}

async fn partial(replica: &Replica, full: &Replica, scope: ResourceScope) {
    let (node, credentials) = {
        let inner = full.inner.lock(crate::replica::Class::Background).await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    replica
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
        })
        .await
        .unwrap();
    replica.subscribe(scope).await.unwrap();
    replica
        .acknowledge_frontier(node, &credentials, &full.frontier(scope).await.unwrap())
        .await
        .unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_base_at_a_seal_that_left_a_member_open_converges_at_the_next_seal() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys: [PrivateKey; 4] = std::array::from_fn(|_| PrivateKey::new());
    let (items, tags) = (lineage.scope("items"), lineage.scope("tags"));
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "base", "first").await;
    let sql = session(&a).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    identity_insert(
        &sql,
        "INSERT INTO items(key,title) VALUES(?,?)",
        vec![text("x"), text("bundled")],
    )
    .await
    .unwrap();
    identity_insert(
        &sql,
        "INSERT INTO tags(key,label) VALUES(?,?)",
        vec![text("t"), text("bundled")],
    )
    .await
    .unwrap();
    let handle = sql
        .run("COMMIT", vec![])
        .await
        .unwrap()
        .committed
        .expect("a replicated commit")
        .handle;
    let bundle = a.transaction_of(handle);
    let p = open(&lineage, &dir, "p", &keys[1]).await;
    exchange_graphs(&a, &p, lineage.domain).await;

    let item_frames = frames_of(&lineage, &a, &p, &keys[1], items).await;
    let tag_frames = frames_of(&lineage, &a, &p, &keys[1], tags).await;
    assert!(
        item_frames.len() > 2,
        "A's items log holds `base`, the bundle's member, then A's record of it"
    );
    let member_sequence = [&item_frames[1], &tag_frames[0]]
        .iter()
        .map(|frame| {
            frame
                .operation_ref()
                .unwrap()
                .header
                .extensions
                .author_sequence
        })
        .max()
        .unwrap();
    let mut frontier = a.seal_frontier(lineage.domain, false).await.unwrap();
    assert_eq!(frontier.len(), 1);
    assert!(frontier[0].sequence > member_sequence);
    frontier[0].sequence = member_sequence;
    let tip = a.followed_tip().await;
    publish_seal(&lineage, &a, sign_seal(&lineage, tip, frontier)).await;
    a.compact(lineage.domain, 0).await.unwrap();

    exchange_graphs(&a, &p, lineage.domain).await;
    p.follow_bookmark(lineage.domain, "main").await.unwrap();
    partial(&p, &a, items).await;
    let through_the_member_without_as_record = item_frames[..2].to_vec();
    for status in p.receive_all(through_the_member_without_as_record).await {
        status.unwrap();
    }
    p.finalize_arrived(lineage.domain).await.unwrap();
    assert_eq!(p.awaiting_outcomes().await.unwrap(), vec![bundle]);
    p.compact(lineage.domain, 0).await.unwrap();
    let q = open(&lineage, &dir, "q", &keys[2]).await;
    exchange_graphs(&a, &q, lineage.domain).await;
    q.follow_bookmark(lineage.domain, "main").await.unwrap();
    partial(&q, &a, items).await;
    assert!(offer(&lineage, &p, &q, &keys[2], items).await.is_none());

    import_checkpoint(&lineage, &a, &q, &keys[2], items).await;
    let f_replaying_every_frame = open(&lineage, &dir, "f", &keys[3]).await;
    let f = &f_replaying_every_frame;
    exchange_graphs(&a, f, lineage.domain).await;
    f.follow_bookmark(lineage.domain, "main").await.unwrap();
    for scope in [items, tags] {
        sync(&lineage, &a, f, &keys[3], scope).await;
    }
    f.finalize_arrived(lineage.domain).await.unwrap();

    insert(&a, "z", "later").await;
    seal(&lineage, &a).await;
    a.finalize_arrived(lineage.domain).await.unwrap();
    for (replica, key, scopes) in [
        (&p, &keys[1], &[items][..]),
        (&q, &keys[2], &[items][..]),
        (f, &keys[3], &[items, tags][..]),
    ] {
        exchange_graphs(&a, replica, lineage.domain).await;
        for scope in scopes {
            sync(&lineage, &a, replica, key, *scope).await;
        }
        replica.finalize_arrived(lineage.domain).await.unwrap();
    }
    assert!(
        p.awaiting_outcomes().await.unwrap().is_empty(),
        "the next seal covers A's record and everyone reaches it"
    );
    p.compact(lineage.domain, 0).await.unwrap();
    let r_key = PrivateKey::new();
    let r = open(&lineage, &dir, "r", &r_key).await;
    exchange_graphs(&a, &r, lineage.domain).await;
    r.follow_bookmark(lineage.domain, "main").await.unwrap();
    partial(&r, &a, items).await;
    import_checkpoint(&lineage, &p, &r, &r_key, items).await;
    sync(&lineage, &a, &r, &r_key, items).await;
    let expected = titles(&a).await;
    assert!(
        expected.iter().any(|row| row[0] == text("x")),
        "{expected:?}"
    );
    let drops =
        "SELECT txn,dropped,sealed FROM secure_replayed WHERE dropped IS NOT NULL ORDER BY txn";
    let dropped = stored(&a, drops).await;
    for (name, replica) in [("f", f), ("p", &p), ("q", &q), ("r", &r)] {
        assert_eq!(titles(replica).await, expected, "{name}'s items");
        assert_eq!(stored(replica, drops).await, dropped, "{name}'s drops");
        replica.audit(items).await.unwrap();
    }
}

async fn offer(
    lineage: &Lineage,
    source: &Replica,
    target: &Replica,
    key: &PrivateKey,
    scope: ResourceScope,
) -> Option<crate::SignedCheckpoint> {
    let credentials = lineage.credentials(3, key);
    source
        .admit_peer(key.public_key(), &credentials, target.followed_tip().await)
        .await
        .unwrap();
    source
        .checkpoint_offer(key.public_key(), &credentials, scope)
        .await
        .unwrap()
}

async fn sync(
    lineage: &Lineage,
    source: &Replica,
    target: &Replica,
    key: &PrivateKey,
    scope: ResourceScope,
) {
    target
        .receive_tip_statements(&source.tip_statements(None, 32).await.unwrap())
        .await
        .unwrap();
    let credentials = lineage.credentials(3, key);
    source
        .admit_peer(key.public_key(), &credentials, target.followed_tip().await)
        .await
        .unwrap();
    let mut after = None;
    loop {
        let page = source
            .range_page(
                key.public_key(),
                &credentials,
                &target.frontier(scope).await.unwrap(),
                after,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        for status in target.receive_all(page.frames).await {
            status.unwrap();
        }
        after = page.next;
        if after.is_none() {
            break;
        }
    }
}
