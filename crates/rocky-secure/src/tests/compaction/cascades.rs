//! Resetting a parent scope never takes rows of other scopes its delete
//! actions reach: `tags` references `items` twice, by a cascade and by a
//! `SET NULL`, and keeps its rows through every refill of `items`.
use super::super::generations::{identity_insert, select, session};
use super::*;

fn linked() -> Lineage {
    let mut lineage = Lineage::new();
    lineage.linked = true;
    lineage
}
async fn item_id(replica: &Replica, key: &str) -> SqlValue {
    select(replica, &format!("SELECT id FROM items WHERE key='{key}'"))
        .await
        .remove(0)
        .remove(0)
}
/// Tags `key` with item `item` through both references.
async fn tag(replica: &Replica, key: &str, item: &str) -> Commitment {
    let id = item_id(replica, item).await;
    let rows = identity_insert(
        &session(replica).await,
        "INSERT INTO tags(key,label,item,also) VALUES(?,'l',?,?)",
        vec![text(key), id.clone(), id],
    )
    .await
    .unwrap();
    transaction(replica, rows).await
}
async fn transaction(replica: &Replica, rows: crate::SecureRows) -> Commitment {
    let handle = rows.committed.expect("a replicated commit").handle;
    replica.transaction_of(handle)
}
async fn tags(replica: &Replica) -> Vec<Vec<SqlValue>> {
    select(
        replica,
        "SELECT t.key,i.key,a.key FROM tags t LEFT JOIN items i ON i.id=t.item LEFT JOIN items a ON a.id=t.also ORDER BY t.key",
    )
    .await
}
fn tagged(rows: &[(&str, &str)]) -> Vec<Vec<SqlValue>> {
    rows.iter()
        .map(|(tag, item)| vec![text(tag), text(item), text(item)])
        .collect()
}
/// Sends `source`'s graph and its frames of `scope` to `target`.
async fn sync_scope(
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
        let frontier = target.frontier(scope).await.unwrap();
        let page = source
            .range_page(
                key.public_key(),
                &credentials,
                &frontier,
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
            return;
        }
    }
}

/// A seal rebootstrap that replays `items` from its frames clears its rows
/// first; the tags that reference them come back once the frames stand.
#[tokio::test]
async fn a_parent_replayed_from_frames_keeps_the_rows_that_reference_it() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let replica = open(&lineage, &dir, "keep", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "a", "first").await;
    insert(&replica, "b", "second").await;
    let first = tag(&replica, "t", "a").await;
    let second = tag(&replica, "u", "b").await;
    churn(&replica, 12, "c").await;
    let sealed = rivalled_seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let before = tags(&replica).await;
    assert_eq!(before, tagged(&[("t", "a"), ("u", "b")]));
    let (_, statements) = rival(&lineage, &sealed);
    replica.receive_tip_statements(&statements).await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    replica.replay_at_seals(tag_scope, false).await.unwrap();
    replica.replay_at_seals(items, true).await.unwrap();
    assert_eq!(tags(&replica).await, before);
    for transaction in [first, second] {
        assert_eq!(
            replica.transaction_outcome(transaction).await.unwrap(),
            Some(rocky_core::replay::TransactionOutcome::Applied)
        );
    }
    replica.audit(items).await.unwrap();
    replica.audit(tag_scope).await.unwrap();
}

/// B writes a new item and a tag of it after the seal, then imports an
/// `items` base at a later seal that lacks both: the base clears the item
/// and its tag; the item applies again over the base and its tag returns.
/// B and the owner then converge.
#[tokio::test]
async fn an_imported_parent_base_keeps_children_of_rows_applied_over_it() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p", "parent").await;
    tag(&a, "old", "p").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    // B's writes after the seal, which A does not hold.
    insert(&b, "l", "late").await;
    let late = tag(&b, "x", "l").await;
    tag(&b, "y", "p").await;
    let expected = tagged(&[("old", "p"), ("x", "l"), ("y", "p")]);
    assert_eq!(tags(&b).await, expected);
    // A seals again without them; B imports A's `items` base there.
    insert(&a, "q", "owner").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    assert_eq!(tags(&b).await, expected);
    assert_eq!(
        b.transaction_outcome(late).await.unwrap(),
        Some(rocky_core::replay::TransactionOutcome::Applied)
    );
    // Both hold every frame now and agree on rows and drops.
    for scope in [items, tag_scope] {
        sync_scope(&lineage, &b, &a, &keys[0], scope).await;
        sync_scope(&lineage, &a, &b, &keys[1], scope).await;
    }
    assert_eq!(tags(&a).await, expected);
    assert_eq!(titles(&a).await, titles(&b).await);
    assert_eq!(drops(&a).await, drops(&b).await);
    b.audit(items).await.unwrap();
    b.audit(tag_scope).await.unwrap();
}

/// The transactions this replica dropped, by transaction id.
async fn drops(replica: &Replica) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(
        &inner.conn,
        "SELECT DISTINCT txn,dropped FROM secure_replayed WHERE dropped IS NOT NULL ORDER BY txn",
    )
    .await
    .unwrap()
}

/// An authored transaction's outcome stays readable after its scope's
/// replay restarts from an imported base that covers it, and across
/// restarts after compaction. `a_parent_replayed_from_frames_…` covers a
/// replay from frames.
#[tokio::test]
async fn an_authored_outcome_survives_imports_and_restarts_after_compaction() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    let rows = identity_insert(
        &session(&a).await,
        "INSERT INTO items(key,title) VALUES(?,?)",
        vec![text("a"), text("first")],
    )
    .await
    .unwrap();
    let authored = transaction(&a, rows).await;
    let applied = Some(rocky_core::replay::TransactionOutcome::Applied);
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    let a = reopen(&lineage, &dir, a, &keys[0]).await;
    assert_eq!(a.transaction_outcome(authored).await.unwrap(), applied);
    // B takes A's base, writes, seals and compacts; A imports B's base,
    // which covers the authored transaction.
    let items = lineage.scope("items");
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], lineage.scope("tags")).await;
    insert(&b, "b", "second").await;
    seal(&lineage, &b).await;
    b.compact(lineage.domain, 0).await.unwrap();
    crate::tests::exchange_graphs(&b, &a, lineage.domain).await;
    import_checkpoint(&lineage, &b, &a, &keys[0], items).await;
    assert_eq!(a.transaction_outcome(authored).await.unwrap(), applied);
    let a = reopen(&lineage, &dir, a, &keys[0]).await;
    assert_eq!(a.transaction_outcome(authored).await.unwrap(), applied);
}

async fn reopen(lineage: &Lineage, dir: &TempDir, replica: Replica, key: &PrivateKey) -> Replica {
    drop(replica);
    lineage
        .reopen(&dir.path().join("a.db"), key, 3, 3)
        .await
        .unwrap()
}

/// A partial device that evicts `items` while it keeps `tags`: tags of its
/// rows follow their delete actions, as SQLite applies them, and their
/// transactions keep the outcome they replayed with. Holding `items` again
/// brings them back as replay orders them.
#[tokio::test]
async fn an_evicted_parent_takes_its_children_by_their_delete_action() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p", "parent").await;
    let tagged_p = tag(&a, "t", "p").await;
    let d = open(&lineage, &dir, "d", &keys[1]).await;
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    for scope in [items, tag_scope] {
        sync_scope(&lineage, &a, &d, &keys[1], scope).await;
    }
    d.follow_bookmark(lineage.domain, "main").await.unwrap();
    assert_eq!(tags(&d).await, tagged(&[("t", "p")]));
    let (full_node, full_credentials) = {
        let inner = a.inner.lock(crate::replica::Class::Background).await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    d.set_role(crate::RoleConfig {
        role: crate::ReplicaRole::Partial,
        full_nodes: [rocky_credentials::PeerId::of(full_node)].into(),
        custody: 1,
    })
    .await
    .unwrap();
    for scope in [items, tag_scope] {
        d.subscribe(scope).await.unwrap();
        d.acknowledge_frontier(
            full_node,
            &full_credentials,
            &a.frontier(scope).await.unwrap(),
        )
        .await
        .unwrap();
    }
    d.unsubscribe(items).await.unwrap();
    d.evict(items).await.unwrap();
    let held = d
        .session([tag_scope].into())
        .await
        .unwrap()
        .sql(Default::default())
        .await
        .unwrap();
    assert!(
        held.run("SELECT key FROM tags", vec![])
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        d.transaction_outcome(tagged_p).await.unwrap(),
        Some(rocky_core::replay::TransactionOutcome::Applied)
    );
    d.subscribe(items).await.unwrap();
    sync_scope(&lineage, &a, &d, &keys[1], items).await;
    d.restored(items).await.unwrap();
    d.settle().await.unwrap();
    assert_eq!(tags(&d).await, tags(&a).await);
}
