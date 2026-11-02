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

/// Tags `key` with item `item` through the `SET NULL` reference only.
async fn tag_also(replica: &Replica, key: &str, item: &str) -> Commitment {
    let id = item_id(replica, item).await;
    let rows = identity_insert(
        &session(replica).await,
        "INSERT INTO tags(key,label,also) VALUES(?,'l',?)",
        vec![text(key), id],
    )
    .await
    .unwrap();
    transaction(replica, rows).await
}
/// The full node's key and credentials, which a partial replica names.
async fn full_node(replica: &Replica) -> (PublicKey, rocky_credentials::CredentialChain) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    (
        inner.config.node.public_key(),
        inner.config.credentials.clone(),
    )
}
/// Makes `replica` a partial replica of `full` holding `held`.
async fn partial(replica: &Replica, full: &Replica, held: &[ResourceScope]) {
    let (node, credentials) = full_node(full).await;
    replica
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
            full_nodes: [rocky_credentials::PeerId::of(node)].into(),
            custody: 1,
        })
        .await
        .unwrap();
    for scope in held {
        replica.subscribe(*scope).await.unwrap();
        replica
            .acknowledge_frontier(node, &credentials, &full.frontier(*scope).await.unwrap())
            .await
            .unwrap();
    }
}
/// Every tag with its references, as stored.
async fn tag_rows(replica: &Replica) -> Vec<Vec<SqlValue>> {
    select(
        replica,
        "SELECT key,item IS NULL,also IS NULL FROM tags ORDER BY key",
    )
    .await
}

/// A partial device that evicts `items` while it keeps `tags` ends with the
/// rows and outcomes of a fresh partial device that only ever held `tags`:
/// tags that reference an item drop there, whatever their delete action.
/// Holding `items` again brings them back as replay orders them.
#[tokio::test]
async fn an_evicted_parent_leaves_what_a_replica_never_holding_it_replays() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p", "parent").await;
    let cascading = tag(&a, "t", "p").await;
    let nulling = tag_also(&a, "u", "p").await;
    let unlinked = {
        let rows = identity_insert(
            &session(&a).await,
            "INSERT INTO tags(key,label) VALUES(?,'l')",
            vec![text("v")],
        )
        .await
        .unwrap();
        transaction(&a, rows).await
    };
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    let d = open(&lineage, &dir, "d", &keys[1]).await;
    for scope in [items, tag_scope] {
        sync_scope(&lineage, &a, &d, &keys[1], scope).await;
    }
    d.follow_bookmark(lineage.domain, "main").await.unwrap();
    assert_eq!(tag_rows(&d).await.len(), 3);
    partial(&d, &a, &[items, tag_scope]).await;
    d.unsubscribe(items).await.unwrap();
    d.evict(items).await.unwrap();
    // A fresh partial replica that holds only `tags`.
    let fresh = open(&lineage, &dir, "fresh", &keys[2]).await;
    partial(&fresh, &a, &[tag_scope]).await;
    sync_scope(&lineage, &a, &fresh, &keys[2], tag_scope).await;
    fresh.follow_bookmark(lineage.domain, "main").await.unwrap();
    fresh.settle().await.unwrap();
    assert_eq!(tag_rows(&d).await, tag_rows(&fresh).await);
    assert_eq!(
        tag_rows(&d).await,
        vec![vec![text("v"), SqlValue::Integer(1), SqlValue::Integer(1)]]
    );
    for transaction in [cascading, nulling, unlinked] {
        assert_eq!(
            d.transaction_outcome(transaction).await.unwrap(),
            fresh.transaction_outcome(transaction).await.unwrap()
        );
    }
    assert!(matches!(
        d.transaction_outcome(nulling).await.unwrap(),
        Some(rocky_core::replay::TransactionOutcome::Dropped {
            reason: rocky_core::replay::DropReason::Constraint,
            sealed: false
        })
    ));
    d.subscribe(items).await.unwrap();
    sync_scope(&lineage, &a, &d, &keys[1], items).await;
    d.restored(items).await.unwrap();
    d.settle().await.unwrap();
    assert_eq!(tags(&d).await, tags(&a).await);
    assert_eq!(tag_rows(&d).await, tag_rows(&a).await);
}

/// The rows of `sql` on `replica`, read under its lock.
async fn stored(replica: &Replica, sql: &str) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(&inner.conn, sql).await.unwrap()
}
/// Sends `source`'s frames of `scope` that `target` lacks to it, except
/// the one before the last: the last then waits for it.
async fn sync_with_gap(
    lineage: &Lineage,
    source: &Replica,
    target: &Replica,
    key: &PrivateKey,
    scope: ResourceScope,
) {
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
    let mut frames = page.frames;
    frames.sort_by_key(|frame| frame.operation_ref().unwrap().header.seq_num);
    frames.remove(frames.len() - 2);
    for status in target.receive_all(frames).await {
        status.unwrap();
    }
}

/// A seals and compacts with `p` and a tag of it; B takes both bases and
/// writes an item `l`, a tag of it and another tag of `p`. A writes `q`,
/// seals and compacts again, writes `q2` and seals once more (not
/// compacted), then writes `r1` and `r2`. B receives A's `items` frames
/// except `r1`, so `r2` waits, and imports A's newer `items` base: clearing
/// `items` takes B's tags, and `items` still waits for `r1`. Returns A, B,
/// B's key, B's late tag and the tags B had.
async fn import_while_a_frame_waits(
    lineage: &Lineage,
    dir: &TempDir,
) -> (Replica, Replica, PrivateKey, Commitment, Vec<Vec<SqlValue>>) {
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(lineage, dir, "a", &keys[0]).await;
    follow_main(lineage, &a).await;
    insert(&a, "p", "parent").await;
    tag(&a, "old", "p").await;
    seal(lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    let b = open(lineage, dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    import_checkpoint(lineage, &a, &b, &keys[1], items).await;
    import_checkpoint(lineage, &a, &b, &keys[1], tag_scope).await;
    insert(&b, "l", "late").await;
    let late = tag(&b, "x", "l").await;
    tag(&b, "y", "p").await;
    let expected = tagged(&[("old", "p"), ("x", "l"), ("y", "p")]);
    assert_eq!(tags(&b).await, expected);
    insert(&a, "q", "owner").await;
    seal(lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    insert(&a, "q2", "sealed").await;
    seal(lineage, &a).await;
    insert(&a, "r1", "gap").await;
    insert(&a, "r2", "waits").await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    sync_with_gap(lineage, &a, &b, &keys[1], items).await;
    import_checkpoint(lineage, &a, &b, &keys[1], items).await;
    assert!(
        !stored(&b, "SELECT 1 FROM secure_replayed_rederive")
            .await
            .is_empty()
    );
    // Reads of `items` wait for `r1`. B's own tags redo after `l`, but
    // the clear took `old`, a row of the `tags` base.
    assert_eq!(
        stored(&b, "SELECT key FROM tags ORDER BY key").await,
        vec![vec![text("x")], vec![text("y")]]
    );
    let [key] = [keys[1].clone()];
    (a, b, key, late, expected)
}

/// The frame `items` waited for arrives by ordinary sync: settling derives
/// the rows again, and B's tags are back.
#[tokio::test]
async fn children_return_when_a_frame_that_waited_through_an_import_arrives() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let (a, b, key, late, expected) = import_while_a_frame_waits(&lineage, &dir).await;
    sync_scope(&lineage, &a, &b, &key, lineage.scope("items")).await;
    b.settle().await.unwrap();
    assert_eq!(tags(&b).await, expected);
    assert!(
        stored(&b, "SELECT 1 FROM secure_replayed_rederive")
            .await
            .is_empty()
    );
    assert_eq!(
        b.transaction_outcome(late).await.unwrap(),
        Some(rocky_core::replay::TransactionOutcome::Applied)
    );
    b.audit(lineage.scope("tags")).await.unwrap();
}

/// While the rows wait to be derived again, no seal finalizes and nothing
/// compacts, though B holds every frame of A's last seal; status says why.
/// Once they are derived again, the seal finalizes.
#[tokio::test]
async fn no_seal_finalizes_while_rows_wait_to_be_derived_again() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let (a, b, key, _, expected) = import_while_a_frame_waits(&lineage, &dir).await;
    let before = b.compaction_status(lineage.domain).await.unwrap();
    assert_eq!(b.finalize_arrived(lineage.domain).await.unwrap(), 0);
    let report = b.compact(lineage.domain, 0).await.unwrap();
    assert!(
        report
            .waiting
            .as_deref()
            .is_some_and(|waiting| waiting.contains("derived again")),
        "{report:?}"
    );
    let status = b.compaction_status(lineage.domain).await.unwrap();
    assert_eq!(
        (status.finalized, status.compacted),
        (before.finalized, before.compacted)
    );
    assert!(
        status
            .waiting
            .as_deref()
            .is_some_and(|waiting| waiting.contains("derived again")),
        "{status:?}"
    );
    sync_scope(&lineage, &a, &b, &key, lineage.scope("items")).await;
    b.settle().await.unwrap();
    assert_eq!(tags(&b).await, expected);
    assert!(b.finalize_arrived(lineage.domain).await.unwrap() > 0);
    assert_ne!(
        b.compaction_status(lineage.domain).await.unwrap().finalized,
        before.finalized
    );
}

/// B tags `p` before it hears that A deleted it; A orders the delete first
/// and drops the tag, seals and compacts. B's own record still says the
/// tag applied, provisionally: once B takes A's bases, which cover the tag,
/// it reports the transaction covered, never an outcome the base may
/// contradict. B keeps outcomes only of what it authored.
#[tokio::test]
async fn an_outcome_a_base_covers_reports_covered_never_its_stale_record() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p", "parent").await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    for scope in [items, tag_scope] {
        sync_scope(&lineage, &a, &b, &keys[1], scope).await;
    }
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    session(&a)
        .await
        .run("DELETE FROM items WHERE key=?", vec![text("p")])
        .await
        .unwrap();
    let late = tag(&b, "t", "p").await;
    let applied = Some(rocky_core::replay::TransactionOutcome::Applied);
    assert_eq!(b.transaction_outcome(late).await.unwrap(), applied);
    sync_scope(&lineage, &b, &a, &keys[0], tag_scope).await;
    let dropped = Some(rocky_core::replay::TransactionOutcome::Dropped {
        reason: rocky_core::replay::DropReason::Constraint,
        sealed: true,
    });
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    assert_eq!(a.transaction_outcome(late).await.unwrap(), dropped);
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    assert_eq!(tags(&b).await, tags(&a).await);
    assert_eq!(
        b.transaction_outcome(late).await.unwrap(),
        Some(rocky_core::replay::TransactionOutcome::Covered)
    );
    assert_eq!(b.transaction_violation(late).await.unwrap(), None);
    assert_eq!(
        stored(&b, "SELECT txn,covered FROM secure_kept_outcomes").await,
        vec![vec![SqlValue::Blob(late.to_vec()), SqlValue::Integer(1)]]
    );
}

/// Kept outcomes go once their member's record is back and a seal covers
/// it, and only the newest `KEPT_OUTCOMES` stay.
#[tokio::test]
async fn kept_outcomes_go_once_records_return_and_stay_bounded() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let replica = open(&lineage, &dir, "keep", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "a", "first").await;
    churn(&replica, 12, "c").await;
    let sealed = rivalled_seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let (_, statements) = rival(&lineage, &sealed);
    replica.receive_tip_statements(&statements).await.unwrap();
    let items = lineage.scope("items");
    replica
        .replay_at_seals(lineage.scope("tags"), false)
        .await
        .unwrap();
    replica.replay_at_seals(items, true).await.unwrap();
    let kept = "SELECT count(*) FROM secure_kept_outcomes";
    let shadowed =
        "SELECT count(*) FROM secure_kept_outcomes k JOIN secure_replayed r ON r.hash=k.hash";
    assert_eq!(
        stored(&replica, kept).await,
        stored(&replica, shadowed).await
    );
    assert_ne!(
        stored(&replica, kept).await,
        vec![vec![SqlValue::Integer(0)]]
    );
    seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    assert_eq!(
        stored(&replica, kept).await,
        vec![vec![SqlValue::Integer(0)]]
    );
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    for clock in 1..=3 {
        inner
            .conn
            .execute(
                "INSERT INTO secure_kept_outcomes VALUES(?,?,1,?,NULL,1,NULL,0)",
                crate::storage::bindings(vec![
                    SqlValue::Blob(vec![clock as u8; 32]),
                    SqlValue::Blob(vec![9; 32]),
                    SqlValue::Integer(clock),
                ]),
            )
            .await
            .unwrap();
    }
    inner.prune_kept_outcomes(2).await.unwrap();
    assert_eq!(
        crate::storage::rows(
            &inner.conn,
            "SELECT clock FROM secure_kept_outcomes ORDER BY clock"
        )
        .await
        .unwrap(),
        vec![vec![SqlValue::Integer(2)], vec![SqlValue::Integer(3)]]
    );
}

/// An authored outcome stays readable after a seal rebootstrap replays its
/// scope from frames, and across a restart.
#[tokio::test]
async fn an_outcome_survives_a_replay_from_frames_and_a_restart() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let replica = open(&lineage, &dir, "a", &key).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "a", "first").await;
    let first = tag(&replica, "t", "a").await;
    churn(&replica, 12, "c").await;
    let sealed = rivalled_seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let (_, statements) = rival(&lineage, &sealed);
    replica.receive_tip_statements(&statements).await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    replica.replay_at_seals(tag_scope, false).await.unwrap();
    replica.replay_at_seals(items, true).await.unwrap();
    let applied = Some(rocky_core::replay::TransactionOutcome::Applied);
    assert_eq!(replica.transaction_outcome(first).await.unwrap(), applied);
    let replica = reopen(&lineage, &dir, replica, &key).await;
    assert_eq!(replica.transaction_outcome(first).await.unwrap(), applied);
    assert_eq!(tags(&replica).await, tagged(&[("t", "a")]));
}

/// A rebuild that leaves a foreign key violated rolls back, says why and
/// is not tried again until what it derives from changes.
#[tokio::test]
async fn a_rebuild_that_cannot_hold_is_stuck_until_its_inputs_change() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let replica = open(&lineage, &dir, "a", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "p", "parent").await;
    tag(&replica, "old", "p").await;
    // Rows a rebuild restores, which a stuck check does not touch.
    for row in 0..64 {
        identity_insert(
            &session(&replica).await,
            "INSERT INTO tags(key,label) VALUES(?,'filler')",
            vec![text(&format!("k{row}"))],
        )
        .await
        .unwrap();
    }
    seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let before = tags(&replica).await;
    // A base of `items` without `p`, which `old` in the `tags` base
    // references, and a mark as a clear of `items` leaves.
    let items = lineage.scope("items");
    {
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute(
                "DELETE FROM secure_replayed_base WHERE scope=(SELECT id FROM secure_scope_state WHERE scope=?)",
                [encode(&items).unwrap()],
            )
            .await
            .unwrap();
    }
    replica.mark_rederive_for_test(items).await.unwrap();
    let attempt = async || {
        let start = crate::sqlite::statements();
        replica.rederive().await.unwrap();
        crate::sqlite::statements() - start
    };
    let rebuilt = attempt().await;
    let status = replica.compaction_status(lineage.domain).await.unwrap();
    assert!(
        status.waiting.as_deref().is_some_and(|waiting| waiting
            .contains("cannot be derived again: a row of tags references a missing row of items")),
        "{status:?}"
    );
    assert_eq!(tags(&replica).await, before);
    let again = attempt().await;
    assert!(again * 4 < rebuilt, "{again} statements after {rebuilt}");
    // A new frame changes the inputs: the rebuild runs again.
    insert(&replica, "q", "new").await;
    assert!(attempt().await * 4 > rebuilt);
    assert!(
        !stored(&replica, "SELECT 1 FROM secure_replayed_stuck")
            .await
            .is_empty()
    );
}

/// A partitioned `tags` cannot reference the unpartitioned `items`, so no
/// reference reaches across partitions into another table's scopes; the
/// fixture's schema is refused before any replica opens.
#[tokio::test]
#[should_panic(expected = "a partitioned child cannot reference an unpartitioned parent")]
async fn a_partitioned_child_of_an_unpartitioned_parent_is_refused() {
    let mut lineage = linked();
    lineage.partitioned = true;
    let dir = TempDir::new().unwrap();
    let _ = lineage
        .open(
            &dir.path().join("p.db"),
            &PrivateKey::new(),
            3,
            Some(lineage.projection(3)),
        )
        .await;
}

/// Measurement, not a gate: how long one rederive of a domain of 100,000
/// replayed rows holds the replica, once from frames and once from a base.
/// `cargo test --release -p rocky-secure --lib
/// tests::compaction::cascades::rederive_of_100000_rows -- --ignored --nocapture`
#[tokio::test]
#[ignore = "measurement; run in release"]
async fn rederive_of_100000_rows() {
    const TRANSACTIONS: usize = 1_000;
    const ROWS: usize = 100;
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let replica = open(&lineage, &dir, "measure", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    let filled = std::time::Instant::now();
    for transaction in 0..TRANSACTIONS {
        let session = session(&replica).await;
        session.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
        for row in 0..ROWS {
            identity_insert(
                &session,
                "INSERT INTO items(key,title) VALUES(?,?)",
                vec![text(&format!("k{transaction}-{row}")), text("measured")],
            )
            .await
            .unwrap();
        }
        session.run("COMMIT", vec![]).await.unwrap();
    }
    println!("fill: {:?}", filled.elapsed());
    let items = lineage.scope("items");
    let measure = async |label: &str| {
        replica.mark_rederive_for_test(items).await.unwrap();
        let start = std::time::Instant::now();
        replica.rederive().await.unwrap();
        println!(
            "rederive from {label}, {} rows: {:?}",
            TRANSACTIONS * ROWS,
            start.elapsed()
        );
        assert!(
            stored(&replica, "SELECT 1 FROM secure_replayed_rederive")
                .await
                .is_empty()
        );
    };
    measure("frames").await;
    seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    measure("a base").await;
    assert_eq!(
        stored(&replica, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer((TRANSACTIONS * ROWS) as i64)]]
    );
}

/// A direct import fills a scope that holds nothing, so cancelling it
/// (`abort_direct`) clears no row: it marks nothing and B's tags stay.
#[tokio::test]
async fn a_cancelled_direct_import_reaches_no_rows_and_marks_nothing() {
    let lineage = linked();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p", "parent").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    identity_insert(
        &session(&b).await,
        "INSERT INTO tags(key,label) VALUES(?,'l')",
        vec![text("v")],
    )
    .await
    .unwrap();
    let credentials = lineage.credentials(3, &keys[1]);
    a.admit_peer(keys[1].public_key(), &credentials, b.followed_tip().await)
        .await
        .unwrap();
    let offered = a
        .checkpoint_offer(keys[1].public_key(), &credentials, items)
        .await
        .unwrap()
        .unwrap();
    let checkpoint = offered.id().unwrap();
    b.begin_rebootstrap(offered).await.unwrap();
    let page = a
        .checkpoint_page(
            keys[1].public_key(),
            &credentials,
            items,
            checkpoint,
            None,
            64 * 1024,
        )
        .await
        .unwrap();
    b.stage_checkpoint_page(page).await.unwrap();
    b.cancel_rebootstrap(items, checkpoint).await.unwrap();
    assert!(
        stored(&b, "SELECT 1 FROM secure_replayed_rederive")
            .await
            .is_empty()
    );
    assert_eq!(
        stored(&b, "SELECT key FROM tags").await,
        vec![vec![text("v")]]
    );
}
