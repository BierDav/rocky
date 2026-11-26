use super::super::generations::{identity_insert, select, session};
use super::*;
use rocky_core::replay::TransactionOutcome;

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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_parent_replayed_from_frames_keeps_the_rows_that_reference_it() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "keep", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "a", "first").await;
    insert(&replica, "b", "second").await;
    let first = tag(&replica, "t", "a").await;
    let second = tag(&replica, "u", "b").await;
    churn(&replica, RIVAL_CHURN, "c").await;
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
            Some(rocky_core::replay::TransactionOutcome::Applied { sealed: false }),
            "the rival seal that won is not finalized here yet"
        );
    }
    replica.audit(items).await.unwrap();
    replica.audit(tag_scope).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_imported_parent_base_keeps_children_of_rows_applied_over_it() {
    let lineage = linked();
    let dir = Scratch::new().await;
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
    insert(&b, "l", "late").await;
    let late = tag(&b, "x", "l").await;
    tag(&b, "y", "p").await;
    let expected = tagged(&[("old", "p"), ("x", "l"), ("y", "p")]);
    assert_eq!(tags(&b).await, expected);
    insert(&a, "q", "owner").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    assert_eq!(
        tags(&b).await,
        expected,
        "writes after the seal survive importing a base that lacks them"
    );
    assert_eq!(
        b.transaction_outcome(late).await.unwrap(),
        Some(rocky_core::replay::TransactionOutcome::Applied { sealed: false })
    );
    for scope in [items, tag_scope] {
        sync_scope(&lineage, &b, &a, &keys[0], scope).await;
        sync_scope(&lineage, &a, &b, &keys[1], scope).await;
    }
    assert_eq!(tags(&a).await, expected);
    assert_eq!(titles(&a).await, titles(&b).await);
    assert_eq!(
        drops(&a).await,
        drops(&b).await,
        "holding every frame, both agree on rows and drops"
    );
    b.audit(items).await.unwrap();
    b.audit(tag_scope).await.unwrap();
}

async fn drops(replica: &Replica) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(
        &inner.conn,
        "SELECT DISTINCT txn,dropped FROM secure_replayed WHERE dropped IS NOT NULL ORDER BY txn",
    )
    .await
    .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_authored_outcome_survives_imports_and_restarts_after_compaction() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
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
    let applied = Some(rocky_core::replay::TransactionOutcome::Applied { sealed: true });
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    let a = reopen(&lineage, &dir, a, &keys[0]).await;
    assert_eq!(a.transaction_outcome(authored).await.unwrap(), applied);
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

async fn reopen(lineage: &Lineage, dir: &Scratch, replica: Replica, key: &PrivateKey) -> Replica {
    drop(replica);
    lineage
        .reopen(&dir.path().join("a.db"), key, 3, 3)
        .await
        .unwrap()
}

async fn full_node(replica: &Replica) -> (PublicKey, rocky_credentials::CredentialChain) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    (
        inner.config.node.public_key(),
        inner.config.credentials.clone(),
    )
}
async fn partial(replica: &Replica, full: &Replica, held: &[ResourceScope]) {
    let (node, credentials) = full_node(full).await;
    replica
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
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
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_tag_of_an_item_that_drops_after_a_seal_drops_in_every_rebuild() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p", "parent").await;
    insert(&a, "q", "owner's").await;
    seal(&lineage, &a).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    insert(&b, "q", "late").await;
    let late = tag(&b, "x", "q").await;
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    for scope in [items, tag_scope] {
        sync_scope(&lineage, &b, &a, &keys[0], scope).await;
    }
    assert!(tags(&a).await.is_empty());
    assert!(matches!(
        a.transaction_outcome(late).await.unwrap(),
        Some(rocky_core::replay::TransactionOutcome::Dropped { .. })
    ));
    a.audit(items).await.unwrap();
    a.audit(tag_scope).await.unwrap();
    assert!(tags(&a).await.is_empty());
}

async fn stored(replica: &Replica, sql: &str) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(&inner.conn, sql).await.unwrap()
}
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

async fn import_while_a_frame_waits(
    lineage: &Lineage,
    dir: &Scratch,
) -> (Replica, Replica, PrivateKey, Commitment, Vec<Vec<SqlValue>>) {
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(lineage, dir, "a", &keys[0]).await;
    follow_main(lineage, &a).await;
    insert(&a, "p", "parent").await;
    insert(&a, "s", "swap").await;
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
    assert_eq!(
        tags(&b).await,
        tagged(&[("old", "p"), ("x", "l"), ("y", "p")])
    );
    let s = session(&a).await;
    s.run("UPDATE items SET key='tmp' WHERE key='p'", vec![])
        .await
        .unwrap();
    s.run("UPDATE items SET key='p' WHERE key='s'", vec![])
        .await
        .unwrap();
    s.run("UPDATE items SET key='s' WHERE key='tmp'", vec![])
        .await
        .unwrap();
    let expected = tagged(&[("old", "s"), ("x", "l"), ("y", "s")]);
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
    assert_eq!(
        stored(&b, "SELECT key FROM tags ORDER BY key").await,
        vec![vec![text("x")], vec![text("y")]],
        "B's own tags redo after `l`, but the clear took `old`, a row of the `tags` base"
    );
    let [key] = [keys[1].clone()];
    (a, b, key, late, expected)
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn children_return_when_a_frame_that_waited_through_an_import_arrives() {
    let lineage = linked();
    let dir = Scratch::new().await;
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
        Some(rocky_core::replay::TransactionOutcome::Applied { sealed: false })
    );
    b.audit(lineage.scope("tags")).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn no_seal_finalizes_while_rows_wait_to_be_derived_again() {
    let lineage = linked();
    let dir = Scratch::new().await;
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

async fn tagged_before_a_seal_drops_one(
    lineage: &Lineage,
    dir: &Scratch,
) -> (Replica, Replica, [PrivateKey; 2], Commitment, Commitment) {
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(lineage, dir, "a", &keys[0]).await;
    follow_main(lineage, &a).await;
    insert(&a, "p", "parent").await;
    insert(&a, "q", "other").await;
    let b = open(lineage, dir, "b", &keys[1]).await;
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    for scope in [items, tag_scope] {
        sync_scope(lineage, &a, &b, &keys[1], scope).await;
    }
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    delete(&a, "p").await;
    let late = tag(&b, "t", "p").await;
    let kept = tag(&b, "u", "q").await;
    let applied = Some(TransactionOutcome::Applied { sealed: false });
    assert_eq!(b.transaction_outcome(late).await.unwrap(), applied);
    sync_scope(lineage, &b, &a, &keys[0], tag_scope).await;
    seal(lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    assert_eq!(a.transaction_outcome(late).await.unwrap(), sealed_drop());
    assert_eq!(
        a.transaction_outcome(kept).await.unwrap(),
        Some(TransactionOutcome::Applied { sealed: true })
    );
    (a, b, keys, late, kept)
}
fn sealed_drop() -> Option<TransactionOutcome> {
    Some(TransactionOutcome::Dropped {
        reason: rocky_core::replay::DropReason::Constraint,
        sealed: true,
    })
}
async fn import_both(lineage: &Lineage, a: &Replica, b: &Replica, key: &PrivateKey) {
    crate::tests::exchange_graphs(a, b, lineage.domain).await;
    for scope in [lineage.scope("items"), lineage.scope("tags")] {
        import_checkpoint(lineage, a, b, key, scope).await;
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_child_import_replays_a_child_once_though_a_later_parent_delete_cascaded_it() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    insert(&a, "p", "parent").await;
    let child = tag(&a, "t", "p").await;
    delete(&a, "p").await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    for scope in [items, tag_scope] {
        sync_scope(&lineage, &a, &b, &keys[1], scope).await;
    }
    let applied = Some(TransactionOutcome::Applied { sealed: false });
    assert_eq!(b.transaction_outcome(child).await.unwrap(), applied);
    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    assert_eq!(
        b.transaction_outcome(child).await.unwrap(),
        applied,
        "the import replays the child once, never over the image the delete's undo kept of it"
    );
    assert!(tags(&b).await.is_empty());
    assert!(stored(&b, "SELECT key FROM items").await.is_empty());
    b.audit(items).await.unwrap();
    b.audit(tag_scope).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_child_held_from_frames_starts_from_the_seal_its_parent_base_sits_at() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p", "parent").await;
    let child = tag(&a, "t", "p").await;
    delete(&a, "p").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    list_checkpoints(&lineage, &a).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    sync_scope(&lineage, &a, &b, &keys[1], tag_scope).await;
    let needs = b.seal_rebootstraps(lineage.domain).await.unwrap();
    assert_eq!(
        needs
            .iter()
            .map(|need| (need.scope, need.replay, need.listed.len()))
            .collect::<Vec<_>>(),
        vec![(tag_scope, crate::FrameReplay::Never, 1)],
        "the child's covered frames never replay against a parent base past them"
    );
    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    assert!(
        b.seal_rebootstraps(lineage.domain)
            .await
            .unwrap()
            .is_empty()
    );
    let outcome = b.transaction_outcome(child).await.unwrap();
    assert!(
        !matches!(outcome, Some(TransactionOutcome::Dropped { .. })),
        "the base reports the child the author applied, never a drop against the parent's later base: {outcome:?}"
    );
    assert!(tags(&b).await.is_empty());
    b.audit(items).await.unwrap();
    b.audit(tag_scope).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn undoing_a_parent_insert_keeps_the_children_the_undo_does_not_own() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "undo", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "p", "parent").await;
    tag(&replica, "kept", "p").await;
    tag(&replica, "undone", "p").await;
    let read = "SELECT t.key,t.item IS NOT NULL,t.also IS NOT NULL FROM tags t ORDER BY t.key";
    let parent = item_id(&replica, "p").await;
    let undone = select(&replica, "SELECT id FROM tags WHERE key='undone'")
        .await
        .remove(0)
        .remove(0);
    assert_eq!(
        replica
            .read_after_undoing_inserts_for_test(&[("items", parent), ("tags", undone)], read)
            .await
            .unwrap(),
        vec![vec![
            text("kept"),
            SqlValue::Integer(1),
            SqlValue::Integer(1)
        ]],
        "the parent's delete neither cascades into nor nulls a child the undo does not own"
    );
    assert_eq!(
        tags(&replica).await,
        tagged(&[("kept", "p"), ("undone", "p")])
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_outcome_a_base_covers_reports_the_seals_drop_never_its_stale_record() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let (a, b, keys, late, _) = tagged_before_a_seal_drops_one(&lineage, &dir).await;
    let violation = a.transaction_violation(late).await.unwrap();
    assert!(violation.is_some());
    import_both(&lineage, &a, &b, &keys[1]).await;
    assert_eq!(tags(&b).await, tags(&a).await);
    assert_eq!(b.transaction_outcome(late).await.unwrap(), sealed_drop());
    assert_eq!(b.transaction_violation(late).await.unwrap(), violation);
    assert_eq!(
        stored(
            &b,
            "SELECT txn,dropped,sealed,based FROM secure_kept_outcomes WHERE txn IN(SELECT txn FROM secure_kept_outcomes WHERE dropped IS NOT NULL)"
        )
        .await,
        vec![vec![
            SqlValue::Blob(late.to_vec()),
            text("constraint"),
            SqlValue::Integer(1),
            SqlValue::Integer(1),
        ]]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_applied_outcome_a_base_covers_is_final() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let (a, b, keys, _, kept) = tagged_before_a_seal_drops_one(&lineage, &dir).await;
    import_both(&lineage, &a, &b, &keys[1]).await;
    assert_eq!(
        b.transaction_outcome(kept).await.unwrap(),
        Some(TransactionOutcome::Applied { sealed: true })
    );
    assert_eq!(b.transaction_violation(kept).await.unwrap(), None);
    assert_eq!(
        stored(
            &b,
            &format!(
                "SELECT dropped,sealed,based FROM secure_kept_outcomes WHERE txn=x'{}'",
                hex(&kept)
            )
        )
        .await,
        vec![vec![
            SqlValue::Null,
            SqlValue::Integer(1),
            SqlValue::Integer(1)
        ]]
    );
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn replicas_at_one_seal_report_identical_outcomes() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let (a, b, keys, late, kept) = tagged_before_a_seal_drops_one(&lineage, &dir).await;
    import_both(&lineage, &a, &b, &keys[1]).await;
    let c_key = PrivateKey::new();
    let c = open(&lineage, &dir, "c", &c_key).await;
    let tag_scope = lineage.scope("tags");
    crate::tests::exchange_graphs(&a, &c, lineage.domain).await;
    c.follow_bookmark(lineage.domain, "main").await.unwrap();
    for scope in [lineage.scope("items"), tag_scope] {
        sync_scope(&lineage, &a, &c, &c_key, scope).await;
    }
    c.finalize_arrived(lineage.domain).await.unwrap();
    c.compact(lineage.domain, 0).await.unwrap();
    for token in [late, kept] {
        let outcome = a.transaction_outcome(token).await.unwrap();
        let violation = a.transaction_violation(token).await.unwrap();
        for replica in [&b, &c] {
            assert_eq!(replica.transaction_outcome(token).await.unwrap(), outcome);
            assert_eq!(
                replica.transaction_violation(token).await.unwrap(),
                violation
            );
        }
    }
    let peer = PrivateKey::new();
    let credentials = lineage.credentials(3, &peer);
    let mut stated = Vec::new();
    for replica in [&a, &c] {
        replica
            .admit_peer(
                peer.public_key(),
                &credentials,
                replica.followed_tip().await,
            )
            .await
            .unwrap();
        let offered = replica
            .checkpoint_offer(peer.public_key(), &credentials, tag_scope)
            .await
            .unwrap()
            .expect("a checkpoint at the seal");
        stated.push((offered.statement.seal, offered.statement.outcomes));
    }
    assert_eq!(stated[0], stated[1]);
    let listed = &stated[0].1.listed;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].transaction, late);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_forged_drop_list_is_refused_on_import() {
    use crate::{SealedOutcome, SealedOutcomes, SealedTransaction};
    let lineage = linked();
    let dir = Scratch::new().await;
    let (a, b, keys, late, kept) = tagged_before_a_seal_drops_one(&lineage, &dir).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    let tag_scope = lineage.scope("tags");
    let credentials = lineage.credentials(3, &keys[1]);
    a.admit_peer(keys[1].public_key(), &credentials, b.followed_tip().await)
        .await
        .unwrap();
    let offered = a
        .checkpoint_offer(keys[1].public_key(), &credentials, tag_scope)
        .await
        .unwrap()
        .unwrap();
    let a_credentials = lineage.credentials(3, &keys[0]);
    let resigned = |edit: &dyn Fn(&mut SealedOutcomes)| {
        let mut statement = offered.statement.clone();
        edit(&mut statement.outcomes);
        crate::SignedCheckpoint::sign(statement, &keys[0], a_credentials.clone()).unwrap()
    };
    let listed = offered.statement.outcomes.listed[0].clone();
    let mut altered = offered.clone();
    altered.statement.outcomes.listed.clear();
    let mut refused = vec![altered];
    let out_of_stamp_order = |o: &mut SealedOutcomes| {
        let mut first = o.listed[0].clone();
        first.stamp.clock += 1;
        first.transaction = [7; 32];
        o.listed.insert(0, first);
    };
    let horizon_on_a_list_that_is_not_full =
        |o: &mut SealedOutcomes| o.horizon = Some(o.listed[0].stamp.clone());
    let violation_only_constraints_and_restrictions_name = |o: &mut SealedOutcomes| {
        o.listed[0].outcome = SealedOutcome::Dropped {
            reason: rocky_core::replay::DropReason::Admission,
            violation: Some("x".into()),
        }
    };
    for edit in [
        out_of_stamp_order as fn(&mut SealedOutcomes),
        horizon_on_a_list_that_is_not_full,
        violation_only_constraints_and_restrictions_name,
        |o| {
            let first = o.listed[0].clone();
            o.listed = (0..=crate::MAX_SEALED_OUTCOMES as u64)
                .map(|sequence| SealedTransaction {
                    stamp: crate::replica::Stamp {
                        sequence,
                        ..first.stamp.clone()
                    },
                    ..first.clone()
                })
                .collect();
        },
    ] {
        refused.push(resigned(&edit));
    }
    for forged in refused {
        assert!(b.begin_rebootstrap(forged).await.is_err());
        assert!(b.rebootstrap_progress(tag_scope).await.unwrap().is_none());
    }
    b.seal_replayed_for_test(tag_scope)
        .await
        .expect("B's own records, made final as a seal B finalized itself would");
    b.cover_for_test(tag_scope, &SealedOutcomes::default())
        .await
        .expect("a base stating B's own outcomes imports");
    assert!(
        b.cover_for_test(tag_scope, &offered.statement.outcomes)
            .await
            .is_err(),
        "a base that drops a tag B applied contradicts B's seal"
    );
    assert_eq!(late, listed.transaction);
    let _ = kept;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn kept_outcomes_go_once_records_return_and_stay_bounded() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "keep", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "a", "first").await;
    churn(&replica, RIVAL_CHURN, "c").await;
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
                "INSERT INTO secure_kept_outcomes VALUES(?,?,1,?,0,NULL,1,NULL,0)",
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_outcome_survives_a_replay_from_frames_and_a_restart() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = open(&lineage, &dir, "a", &key).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "a", "first").await;
    let first = tag(&replica, "t", "a").await;
    churn(&replica, RIVAL_CHURN, "c").await;
    let sealed = rivalled_seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let (_, statements) = rival(&lineage, &sealed);
    replica.receive_tip_statements(&statements).await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    replica.replay_at_seals(tag_scope, false).await.unwrap();
    replica.replay_at_seals(items, true).await.unwrap();
    let provisional_until_the_winning_rival_finalizes =
        Some(rocky_core::replay::TransactionOutcome::Applied { sealed: false });
    let applied = provisional_until_the_winning_rival_finalizes;
    assert_eq!(replica.transaction_outcome(first).await.unwrap(), applied);
    let replica = reopen(&lineage, &dir, replica, &key).await;
    assert_eq!(replica.transaction_outcome(first).await.unwrap(), applied);
    assert_eq!(tags(&replica).await, tagged(&[("t", "a")]));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_rebuild_that_cannot_hold_is_stuck_until_its_inputs_change() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = open(&lineage, &dir, "a", &key).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "p", "parent").await;
    tag(&replica, "old", "p").await;
    let rows_a_rebuild_restores_and_a_stuck_check_skips = 0..64;
    for row in rows_a_rebuild_restores_and_a_stuck_check_skips {
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
    let items = lineage.scope("items");
    clear_the_parent_base_a_child_references(&replica, items).await;
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
    let blocked = session(&replica)
        .await
        .run("SELECT key FROM tags ORDER BY key", vec![])
        .await
        .unwrap_err();
    assert!(
        matches!(
            blocked,
            Error::Blocked(ref blockers)
                if blockers.iter().any(|blocker| matches!(
                    &blocker.kind,
                    crate::BlockerKind::Rederiving { reason: Some(reason) }
                        if reason == "a row of tags references a missing row of items"
                ))
        ),
        "{blocked:?}"
    );
    assert_eq!(
        stored(
            &replica,
            "SELECT t.key,i.key,a.key FROM tags t LEFT JOIN items i ON i.id=t.item LEFT JOIN items a ON a.id=t.also ORDER BY t.key"
        )
        .await,
        before
    );
    assert!(
        stored(&replica, "SELECT * FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    let again = attempt().await;
    assert!(again * 4 < rebuilt, "{again} statements after {rebuilt}");
    let stuck_before = stored(&replica, "SELECT inputs FROM secure_replayed_stuck").await;
    assert!(!stuck_before.is_empty());
    let peer_key = PrivateKey::new();
    let peer = open(&lineage, &dir, "peer", &peer_key).await;
    crate::tests::exchange_graphs(&replica, &peer, lineage.domain).await;
    peer.follow_bookmark(lineage.domain, "main").await.unwrap();
    insert(&peer, "q", "new").await;
    let start = crate::sqlite::statements();
    sync_scope(&lineage, &peer, &replica, &key, items).await;
    let on_arrival = crate::sqlite::statements() - start;
    assert!(
        on_arrival * 4 > rebuilt,
        "a new frame changes the inputs, so the rebuild runs again ({on_arrival} vs {rebuilt})"
    );
    let stuck_after = stored(&replica, "SELECT inputs FROM secure_replayed_stuck").await;
    assert!(!stuck_after.is_empty());
    assert_ne!(stuck_after, stuck_before);
    let cached_again = attempt().await;
    assert!(
        cached_again * 4 < rebuilt,
        "{cached_again} statements after {rebuilt}"
    );
}

async fn clear_the_parent_base_a_child_references(replica: &Replica, items: ResourceScope) {
    lose_the_parent_base(replica, items).await;
    replica.mark_rederive_for_test(items).await.unwrap();
}
async fn lose_the_parent_base(replica: &Replica, items: ResourceScope) {
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
type Visible = (
    TipId,
    Vec<Vec<SqlValue>>,
    Vec<Vec<SqlValue>>,
    Vec<Vec<SqlValue>>,
);
async fn visible(replica: &Replica) -> Visible {
    (
        replica.followed_tip().await,
        titles(replica).await,
        tags(replica).await,
        stored(
            replica,
            "SELECT seal,compacted FROM secure_finalized ORDER BY depth",
        )
        .await,
    )
}
fn orphans_a_tag(refused: Result<impl std::fmt::Debug, Error>, replaying: &str) {
    let expected = format!(
        "{replaying} that break a foreign key: a row of tags references a missing row of items"
    );
    assert!(
        matches!(&refused, Err(Error::Incompatible(reason)) if *reason == expected),
        "{refused:?}"
    );
}
async fn follow_main_and_tag_a_parent(lineage: &Lineage, replica: &Replica) {
    follow_main(lineage, replica).await;
    insert(replica, "p", "parent").await;
    tag(replica, "t", "p").await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_tip_switch_whose_replay_orphans_a_child_is_refused_and_changes_nothing() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "a", &PrivateKey::new()).await;
    follow_main_and_tag_a_parent(&lineage, &replica).await;
    let sealed = seal(&lineage, &replica).await.id().unwrap();
    replica.compact(lineage.domain, 0).await.unwrap();
    insert(&replica, "q", "after the seal").await;
    replica
        .publish_tip_statements(&super::super::generations::sideways::open_dev(
            &lineage, sealed, 3,
        ))
        .await
        .unwrap();
    lose_the_parent_base(&replica, lineage.scope("items")).await;
    let before = visible(&replica).await;
    orphans_a_tag(
        replica.follow_bookmark(lineage.domain, "dev").await,
        "switching tips would replay rows",
    );
    assert_eq!(visible(&replica).await, before);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_lost_finalized_seal_whose_replay_orphans_a_child_is_refused_and_changes_nothing() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "a", &PrivateKey::new()).await;
    follow_main_and_tag_a_parent(&lineage, &replica).await;
    churn(&replica, RIVAL_CHURN, "c").await;
    let first = seal(&lineage, &replica).await;
    insert(&replica, "b", "second").await;
    let second = rivalled_seal(&lineage, &replica).await;
    let report = replica.compact(lineage.domain, 1).await.unwrap();
    assert_eq!(
        (report.compacted, report.finalized),
        (Some(first.id().unwrap()), Some(second.id().unwrap()))
    );
    lose_the_parent_base(&replica, lineage.scope("items")).await;
    let before = visible(&replica).await;
    let (_, statements) = rival(&lineage, &second);
    orphans_a_tag(
        replica.receive_tip_statements(&statements).await,
        "switching tips would replay rows",
    );
    assert_eq!(visible(&replica).await, before);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_projection_rebuild_whose_replay_orphans_a_child_is_refused_and_changes_nothing() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "a", &PrivateKey::new()).await;
    follow_main_and_tag_a_parent(&lineage, &replica).await;
    seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let items = lineage.scope("items");
    lose_the_parent_base(&replica, items).await;
    let before = visible(&replica).await;
    orphans_a_tag(
        replica.rebuild_projection(items).await,
        "rebuilding the projection would replay rows",
    );
    assert_eq!(visible(&replica).await, before);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
#[should_panic(expected = "a partitioned child cannot reference an unpartitioned parent")]
async fn a_partitioned_child_of_an_unpartitioned_parent_is_refused() {
    let mut lineage = linked();
    lineage.partitioned = true;
    let dir = Scratch::new().await;
    let _ = lineage
        .open(
            &dir.path().join("p.db"),
            &PrivateKey::new(),
            3,
            Some(lineage.projection(3)),
        )
        .await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
#[ignore = "measurement; run in release"]
async fn rederive_of_100000_rows() {
    const TRANSACTIONS: usize = 1_000;
    const ROWS: usize = 100;
    let lineage = linked();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "measure", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    let filled = web_time::Instant::now();
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
        let start = web_time::Instant::now();
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_cancelled_direct_import_reaches_no_rows_and_marks_nothing() {
    let lineage = linked();
    let dir = Scratch::new().await;
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn frames_a_cancelled_direct_import_held_back_read_incomplete_until_they_apply() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p", "parent").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let items = lineage.scope("items");
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
    insert(&a, "q", "after the checkpoint").await;
    sync_scope(&lineage, &a, &b, &keys[1], items).await;
    assert_eq!(
        stored(&b, "SELECT direct FROM secure_rebootstrap").await,
        vec![vec![SqlValue::Integer(1)]]
    );
    crate::APPLY_BUDGET.with(|budget| budget.set(Some(0)));
    let cancelled = b.cancel_rebootstrap(items, checkpoint).await;
    crate::APPLY_BUDGET.with(|budget| budget.set(None));
    assert!(matches!(cancelled, Err(Error::InjectedFailure)));
    assert!(
        matches!(
            session(&b).await.run("SELECT key FROM items", vec![]).await,
            Err(Error::Sql(crate::sql::SqlError::Unavailable {
                reason: crate::sql::Unavailable::Incomplete { .. },
                ..
            }))
        ),
        "the abort leaves the table holding only frames it has not applied, so it reads incomplete, not empty"
    );
    b.settle().await.unwrap();
    assert_eq!(
        select(&b, "SELECT key FROM items ORDER BY key").await,
        vec![vec![text("p")], vec![text("q")]]
    );
}

async fn orphaned_tags(replica: &Replica) -> Vec<Vec<SqlValue>> {
    stored(
        replica,
        "SELECT t.key FROM tags t WHERE t.item IS NOT NULL AND NOT EXISTS(SELECT 1 FROM items i WHERE i.id=t.item) ORDER BY t.key",
    )
    .await
}
async fn marked_for_rederive(replica: &Replica) -> bool {
    !stored(replica, "SELECT 1 FROM secure_replayed_rederive")
        .await
        .is_empty()
}
async fn orphan_by_hand(replica: &Replica, item: &str) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    inner
        .conn
        .execute("PRAGMA foreign_keys=OFF", ())
        .await
        .unwrap();
    inner
        .conn
        .execute("DELETE FROM items WHERE key=?", [item.to_string()])
        .await
        .unwrap();
    inner
        .conn
        .execute("PRAGMA foreign_keys=ON", ())
        .await
        .unwrap();
}
async fn tags_imported_before_items(
    lineage: &Lineage,
    dir: &Scratch,
    children: &[&str],
) -> (Replica, Replica, [PrivateKey; 2]) {
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(lineage, dir, "a", &keys[0]).await;
    follow_main(lineage, &a).await;
    insert(&a, "p", "parent").await;
    for child in children {
        tag(&a, child, "p").await;
    }
    seal(lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    let b = open(lineage, dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    import_checkpoint(lineage, &a, &b, &keys[1], lineage.scope("tags")).await;
    (a, b, keys)
}

async fn read_blocked_by_rederiving(replica: &Replica, table: &str) {
    let error = session(replica)
        .await
        .run(&format!("SELECT key FROM {table} ORDER BY key"), vec![])
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            Error::Blocked(ref blockers)
                if blockers
                    .iter()
                    .any(|blocker| matches!(blocker.kind, crate::BlockerKind::Rederiving { .. }))
        ),
        "expected Rederiving blocker on {table}, got {error:?}"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_child_imported_before_its_parent_withholds_orphans_blocks_reads_and_recovers() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let (a, b, keys) = tags_imported_before_items(&lineage, &dir, &["t"]).await;
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    assert!(orphaned_tags(&b).await.is_empty());
    assert!(
        stored(&b, "SELECT * FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    assert!(marked_for_rederive(&b).await);
    read_blocked_by_rederiving(&b, "tags").await;
    let status = b.compaction_status(lineage.domain).await.unwrap();
    assert!(status.waiting.is_some(), "{status:?}");
    b.rebuild_projection(tag_scope).await.unwrap();
    assert!(orphaned_tags(&b).await.is_empty());
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    let status = b.compaction_status(lineage.domain).await.unwrap();
    assert_eq!(status.waiting, None);
    assert_eq!(tags(&b).await, tagged(&[("t", "p")]));
    assert!(orphaned_tags(&b).await.is_empty());
    assert!(!marked_for_rederive(&b).await);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_replay_withholding_children_of_a_missing_parent_restores_all_when_parent_arrives() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let (a, b, keys) = tags_imported_before_items(&lineage, &dir, &["t", "u"]).await;
    assert!(orphaned_tags(&b).await.is_empty());
    assert!(
        stored(&b, "SELECT * FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    read_blocked_by_rederiving(&b, "tags").await;
    b.rebuild_projection(lineage.scope("tags")).await.unwrap();
    assert!(orphaned_tags(&b).await.is_empty());
    assert!(
        stored(&b, "SELECT * FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    import_checkpoint(&lineage, &a, &b, &keys[1], lineage.scope("items")).await;
    assert_eq!(tags(&b).await, tagged(&[("t", "p"), ("u", "p")]));
    assert!(orphaned_tags(&b).await.is_empty());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_rebuild_that_repairs_one_orphan_and_breaks_another_is_refused() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = open(&lineage, &dir, "a", &key).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "p2", "sealed parent").await;
    tag(&replica, "t2", "p2").await;
    seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    insert(&replica, "p", "parent after the seal").await;
    tag(&replica, "t", "p").await;
    let items = lineage.scope("items");
    lose_the_parent_base(&replica, items).await;
    orphan_by_hand(&replica, "p").await;
    assert_eq!(orphaned_tags(&replica).await, vec![vec![text("t")]]);
    let before = visible(&replica).await;
    orphans_a_tag(
        replica.rebuild_projection(items).await,
        "rebuilding the projection would replay rows",
    );
    assert_eq!(visible(&replica).await, before);
    assert_eq!(
        orphaned_tags(&replica).await,
        vec![vec![text("t")]],
        "the refused rebuild would have orphaned t2 instead"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_rival_that_takes_a_parent_off_its_base_waits_for_the_parent_and_recovers_on_reimport() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main_and_tag_a_parent(&lineage, &a).await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    let keeps_the_first_seal = open(&lineage, &dir, "c", &keys[2]).await;
    let c = &keeps_the_first_seal;
    crate::tests::exchange_graphs(&a, c, lineage.domain).await;
    import_checkpoint(&lineage, &a, c, &keys[2], items).await;
    insert(&a, "q", "later").await;
    let second = rivalled_seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    assert_eq!(tags(&b).await, tagged(&[("t", "p")]));

    let (_, statements) = rival(&lineage, &second);
    b.receive_tip_statements(&statements).await.unwrap();
    let needs = b.seal_rebootstraps(lineage.domain).await.unwrap();
    assert_eq!(
        needs.iter().map(|need| need.scope).collect::<Vec<_>>(),
        vec![items],
        "the parent's base sits at the losing seal"
    );
    assert!(
        orphaned_tags(&b).await.is_empty(),
        "no orphaned child row is committed while the parent awaits reimport"
    );
    assert!(
        stored(&b, "SELECT * FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    read_blocked_by_rederiving(&b, "tags").await;
    let status = b.compaction_status(lineage.domain).await.unwrap();
    assert!(status.waiting.is_some(), "{status:?}");

    crate::tests::exchange_graphs(&b, c, lineage.domain).await;
    import_checkpoint(&lineage, c, &b, &keys[1], items).await;
    assert!(
        b.seal_rebootstraps(lineage.domain)
            .await
            .unwrap()
            .is_empty()
    );
    let status = b.compaction_status(lineage.domain).await.unwrap();
    assert_eq!(status.waiting, None);
    assert_eq!(tags(&b).await, tagged(&[("t", "p")]));
    assert!(orphaned_tags(&b).await.is_empty());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_partial_replica_holding_a_child_without_its_parent_withholds_orphans_and_blocks_reads() {
    let lineage = linked();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let tag_scope = lineage.scope("tags");
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main_and_tag_a_parent(&lineage, &a).await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    let holding_only_tags = open(&lineage, &dir, "tags-only", &keys[1]).await;
    let b = &holding_only_tags;
    crate::tests::exchange_graphs(&a, b, lineage.domain).await;
    partial(b, &a, &[tag_scope]).await;
    import_checkpoint(&lineage, &a, b, &keys[1], tag_scope).await;
    assert!(orphaned_tags(b).await.is_empty());
    assert!(
        stored(b, "SELECT * FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    assert!(marked_for_rederive(b).await);
    read_blocked_by_rederiving(b, "tags").await;
    b.rebuild_projection(tag_scope).await.unwrap();
    assert!(orphaned_tags(b).await.is_empty());
}

fn restricted_lineage(seed: u8) -> Lineage {
    let mut lineage = Lineage::pinned(seed);
    lineage.linked = true;
    lineage.restricted = true;
    lineage
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn restricted_foreign_key_replays_from_frames_in_either_table_id_order() {
    let parent_first = (1u8..=u8::MAX)
        .find(|seed| {
            let lineage = restricted_lineage(*seed);
            lineage.scope("items").table < lineage.scope("tags").table
        })
        .expect("a seed where items sorts before tags by TableId");
    let child_first = (1u8..=u8::MAX)
        .find(|seed| {
            let lineage = restricted_lineage(*seed);
            lineage.scope("tags").table < lineage.scope("items").table
        })
        .expect("a seed where tags sorts before items by TableId");
    for seed in [parent_first, child_first] {
        let lineage = restricted_lineage(seed);
        let dir = Scratch::new().await;
        let replica = open(&lineage, &dir, "keep", &PrivateKey::new()).await;
        follow_main(&lineage, &replica).await;
        insert(&replica, "a", "first").await;
        insert(&replica, "b", "second").await;
        tag(&replica, "t", "a").await;
        tag(&replica, "u", "b").await;
        churn(&replica, RIVAL_CHURN, "c").await;
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
        replica.audit(items).await.unwrap();
        replica.audit(tag_scope).await.unwrap();
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn importing_parent_checkpoint_with_restricted_children_updates_in_place_and_deletes_obsolete()
 {
    let lineage = restricted_lineage(1);
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p1", "kept").await;
    insert(&a, "p2", "removed").await;
    tag(&a, "t1", "p1").await;
    tag(&a, "t2", "p2").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();

    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    assert_eq!(tags(&b).await, tagged(&[("t1", "p1"), ("t2", "p2")]));

    insert(&a, "p3", "added").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    assert_eq!(tags(&b).await, tagged(&[("t1", "p1"), ("t2", "p2")]));
    assert!(!marked_for_rederive(&b).await);

    session(&a)
        .await
        .run("DELETE FROM tags WHERE key='t2'", vec![])
        .await
        .unwrap();
    delete(&a, "p2").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    b.settle().await.unwrap();
    assert_eq!(tags(&b).await, tagged(&[("t1", "p1")]));
    assert!(!marked_for_rederive(&b).await);
    b.audit(items).await.unwrap();
    b.audit(tag_scope).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn rewinding_multi_table_restricted_transaction_succeeds_when_child_sorts_before_parent() {
    let mut lineage = restricted_lineage(1);
    lineage.children_first = true;
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();

    let (items, badges) = (lineage.scope("items"), lineage.scope("badges"));
    let s_a = session(&a).await;
    s_a.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
    identity_insert(
        &s_a,
        "INSERT INTO items(key,title) VALUES(?,'early')",
        vec![text("a1")],
    )
    .await
    .unwrap();
    let a_id = s_a
        .run("SELECT id FROM items WHERE key='a1'", vec![])
        .await
        .unwrap()
        .rows
        .remove(0)
        .remove(0);
    identity_insert(
        &s_a,
        "INSERT INTO badges(key,label,item,also) VALUES(?,'l',?,?)",
        vec![text("ba1"), a_id.clone(), a_id],
    )
    .await
    .unwrap();
    s_a.run("COMMIT", vec![]).await.unwrap();

    insert(&b, "bump", "advance clock").await;
    let s_b = session(&b).await;
    s_b.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
    identity_insert(
        &s_b,
        "INSERT INTO items(key,title) VALUES(?,'later')",
        vec![text("b1")],
    )
    .await
    .unwrap();
    let b_id = s_b
        .run("SELECT id FROM items WHERE key='b1'", vec![])
        .await
        .unwrap()
        .rows
        .remove(0)
        .remove(0);
    identity_insert(
        &s_b,
        "INSERT INTO badges(key,label,item,also) VALUES(?,'l',?,?)",
        vec![text("bb1"), b_id.clone(), b_id],
    )
    .await
    .unwrap();
    s_b.run("COMMIT", vec![]).await.unwrap();

    for scope in [items, badges] {
        sync_scope(&lineage, &a, &b, &keys[1], scope).await;
    }
    let rows = select(
        &b,
        "SELECT b.key,i.key FROM badges b JOIN items i ON i.id=b.item ORDER BY b.key",
    )
    .await;
    assert_eq!(
        rows,
        vec![vec![text("ba1"), text("a1")], vec![text("bb1"), text("b1")],]
    );
    b.audit(items).await.unwrap();
    b.audit(badges).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn importing_parent_checkpoint_before_child_with_restricted_fk_blocks_reads_without_orphans_and_recovers()
 {
    let lineage = restricted_lineage(1);
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "p1", "kept").await;
    insert(&a, "p2", "removed").await;
    tag(&a, "t1", "p1").await;
    tag(&a, "t2", "p2").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();

    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let (items, tag_scope) = (lineage.scope("items"), lineage.scope("tags"));
    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    assert_eq!(tags(&b).await, tagged(&[("t1", "p1"), ("t2", "p2")]));

    session(&a)
        .await
        .run("DELETE FROM tags WHERE key='t2'", vec![])
        .await
        .unwrap();
    delete(&a, "p2").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;

    import_checkpoint(&lineage, &a, &b, &keys[1], items).await;
    assert!(
        stored(&b, "SELECT * FROM pragma_foreign_key_check")
            .await
            .is_empty(),
        "importing the parent checkpoint before the child must commit no orphaned child rows"
    );
    assert!(marked_for_rederive(&b).await);
    read_blocked_by_rederiving(&b, "tags").await;
    read_blocked_by_rederiving(&b, "items").await;

    import_checkpoint(&lineage, &a, &b, &keys[1], tag_scope).await;
    b.settle().await.unwrap();
    assert!(!marked_for_rederive(&b).await);
    assert_eq!(tags(&b).await, tagged(&[("t1", "p1")]));
    assert!(
        stored(&b, "SELECT * FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    b.audit(items).await.unwrap();
    b.audit(tag_scope).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn rewinding_multi_table_restricted_transaction_with_interleaved_peer_frame_succeeds() {
    let mut lineage = restricted_lineage(1);
    lineage.children_first = true;
    let dir = Scratch::new().await;
    let mut keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    keys.sort_by_key(|key| *key.public_key().as_bytes());
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    let c = open(&lineage, &dir, "c", &keys[2]).await;
    follow_main(&lineage, &a).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    crate::tests::exchange_graphs(&a, &c, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    c.follow_bookmark(lineage.domain, "main").await.unwrap();

    let (items, badges) = (lineage.scope("items"), lineage.scope("badges"));
    assert!(badges < items);
    crate::CLOCK_SKEW.with(|skew| skew.set(60));
    insert(&a, "p1", "v0").await;
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
    sync_scope(&lineage, &a, &b, &keys[1], items).await;
    sync_scope(&lineage, &a, &c, &keys[2], items).await;

    let p1_id = item_id(&b, "p1").await;
    let s_b = session(&b).await;
    s_b.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
    s_b.run(
        "UPDATE items SET title = title || '+b' WHERE key = 'p1'",
        vec![],
    )
    .await
    .unwrap();
    identity_insert(
        &s_b,
        "INSERT INTO badges(key,label,item,also) VALUES(?,'l',?,?)",
        vec![text("bb1"), p1_id.clone(), p1_id],
    )
    .await
    .unwrap();
    s_b.run("COMMIT", vec![]).await.unwrap();

    session(&c)
        .await
        .run(
            "UPDATE items SET title = title || '+c' WHERE key = 'p1'",
            vec![],
        )
        .await
        .unwrap();
    sync_scope(&lineage, &c, &b, &keys[1], items).await;
    assert_eq!(
        select(&b, "SELECT title FROM items WHERE key = 'p1'").await,
        vec![vec![text("v0+b+c")]]
    );

    session(&a)
        .await
        .run(
            "UPDATE items SET title = title || '+a' WHERE key = 'p1'",
            vec![],
        )
        .await
        .unwrap();
    sync_scope(&lineage, &a, &b, &keys[1], items).await;
    sync_scope(&lineage, &a, &b, &keys[1], badges).await;

    assert_eq!(
        select(&b, "SELECT title FROM items WHERE key = 'p1'").await,
        vec![vec![text("v0+a+b+c")]]
    );
    let rows = select(
        &b,
        "SELECT b.key,i.key FROM badges b JOIN items i ON i.id=b.item ORDER BY b.key",
    )
    .await;
    assert_eq!(rows, vec![vec![text("bb1"), text("p1")]]);
    assert!(
        stored(&b, "SELECT * FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    b.audit(items).await.unwrap();
    b.audit(badges).await.unwrap();
}
