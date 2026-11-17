use super::*;
use crate::KeptSeals;

async fn own_mark(replica: &Replica, scope: ResourceScope) -> (crate::writer::WriterId, u64) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    let records = crate::storage::collect_rows(
        inner
            .conn
            .query(
                "SELECT author,sequence FROM secure_identity_marks WHERE scope=?",
                [encode(&scope).unwrap()],
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let [record] = records.as_slice() else {
        panic!("one identity author: {records:?}");
    };
    let [SqlValue::Blob(author), SqlValue::Integer(sequence)] = record.as_slice() else {
        panic!("{record:?}");
    };
    (
        crate::writer::stored_writer(author).unwrap(),
        u64::try_from(*sequence).unwrap(),
    )
}
async fn rewind_mark(replica: &Replica, scope: ResourceScope, sequence: u64) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    inner
        .conn
        .execute(
            "UPDATE secure_identity_marks SET sequence=? WHERE scope=?",
            crate::storage::bindings(vec![
                SqlValue::Integer(sequence as i64),
                SqlValue::Blob(encode(&scope).unwrap()),
            ]),
        )
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "UPDATE secure_identity_own SET sequence=? WHERE scope=?",
            crate::storage::bindings(vec![
                SqlValue::Integer(sequence as i64),
                SqlValue::Blob(encode(&scope).unwrap()),
            ]),
        )
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "DELETE FROM secure_identity_ledger WHERE scope=?",
            [encode(&scope).unwrap()],
        )
        .await
        .unwrap();
}
async fn dropped(replica: &Replica, hash: Hash) -> Option<String> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    match crate::storage::collect_rows(
        inner
            .conn
            .query(
                "SELECT dropped FROM secure_replayed WHERE hash=?",
                [hash.as_bytes().to_vec()],
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap()
    .first()
    .map(Vec::as_slice)
    {
        Some([SqlValue::Text(reason)]) => Some(reason.clone()),
        _ => None,
    }
}
async fn compacted_seal(
    lineage: &Lineage,
    writer: &Replica,
    server: &Replica,
    key: &PrivateKey,
) -> Commitment {
    sync(lineage, writer, server, key).await;
    server
        .follow_bookmark(lineage.domain, "main")
        .await
        .unwrap();
    let sealed = seal(lineage, writer).await.id().unwrap();
    crate::tests::exchange_graphs(writer, server, lineage.domain).await;
    assert_eq!(
        server.compact(lineage.domain, 0).await.unwrap().compacted,
        Some(sealed)
    );
    sealed
}
async fn compact_own(lineage: &Lineage, forger: &Replica) -> Commitment {
    let sealed = seal(lineage, forger).await.id().unwrap();
    assert_eq!(
        forger.compact(lineage.domain, 0).await.unwrap().compacted,
        Some(sealed)
    );
    sealed
}
async fn import_all(
    lineage: &Lineage,
    server: &Replica,
    target: &Replica,
    key: &PrivateKey,
) -> (crate::SignedCheckpoint, crate::RebootstrapReport) {
    crate::tests::exchange_graphs(server, target, lineage.domain).await;
    target
        .follow_bookmark(lineage.domain, "main")
        .await
        .unwrap();
    import_checkpoint(lineage, server, target, key, lineage.scope("tags")).await;
    let credentials = lineage.credentials(3, key);
    server
        .admit_peer(key.public_key(), &credentials, target.followed_tip().await)
        .await
        .unwrap();
    let offered = server
        .checkpoint_offer(key.public_key(), &credentials, lineage.scope("items"))
        .await
        .unwrap()
        .unwrap();
    let report =
        import_checkpoint_report(lineage, server, target, key, lineage.scope("items")).await;
    (offered, report)
}
fn marks(signed: &crate::SignedCheckpoint) -> Vec<(crate::writer::WriterId, u64)> {
    signed
        .statement
        .identities
        .iter()
        .map(|mark| (mark.author, mark.sequence))
        .collect()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_reused_identity_beyond_the_seal_applies_once() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let scope = lineage.scope("items");
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let s = open(&lineage, &dir, "s", &keys[1]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "x", "covered").await;
    compacted_seal(&lineage, &a, &s, &keys[1]).await;
    let (author, covered) = own_mark(&a, scope).await;

    insert(&a, "y", "first").await;
    let y = last_operation(&a, scope).await;
    delete(&a, "y").await;
    compact_own(&lineage, &a).await;
    rewind_mark(&a, scope, covered).await;
    insert(&a, "z", "reused").await;
    let z_second_birth_of_ys_identity = last_operation(&a, scope).await;
    assert_eq!(own_mark(&a, scope).await, (author, covered + 1));

    let j = open(&lineage, &dir, "j", &keys[2]).await;
    crate::tests::exchange_graphs(&a, &j, lineage.domain).await;
    sync(&lineage, &a, &j, &keys[2]).await;
    let (offered, report) = import_all(&lineage, &s, &j, &keys[2]).await;
    assert_eq!(marks(&offered), vec![(author, covered)]);
    assert!(
        report.reapplied.contains(&y),
        "y beyond the imported base reapplies: {report:?}"
    );
    assert_eq!(
        dropped(&j, z_second_birth_of_ys_identity).await.as_deref(),
        Some("admission"),
        "a second birth of y's identity drops"
    );
    assert_eq!(titles(&j).await, vec![vec![text("x"), text("covered")]]);
    assert_eq!(own_mark(&j, scope).await, (author, covered + 1));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_retired_covered_birth_cannot_be_reused_after_an_import() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let scope = lineage.scope("items");
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let s = open(&lineage, &dir, "s", &keys[1]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "x", "kept").await;
    let (author, kept) = own_mark(&a, scope).await;
    insert(&a, "r", "retired").await;
    delete(&a, "r").await;
    compacted_seal(&lineage, &a, &s, &keys[1]).await;
    let j = open(&lineage, &dir, "j", &keys[2]).await;
    let (offered, _) = import_all(&lineage, &s, &j, &keys[2]).await;
    let retired = kept + 1;
    assert_eq!(
        marks(&offered),
        vec![(author, retired)],
        "the deleted row's birth is covered, so the mark reaches past it"
    );
    assert_eq!(offered.statement.rows.count, 1);
    assert_eq!(own_mark(&j, scope).await, (author, retired));

    compact_own(&lineage, &a).await;
    rewind_mark(&a, scope, kept).await;
    insert(&a, "r2", "reused").await;
    let second_birth_of_rs_identity = last_operation(&a, scope).await;
    sync(&lineage, &a, &j, &keys[2]).await;
    assert_eq!(
        dropped(&j, second_birth_of_rs_identity).await.as_deref(),
        Some("admission"),
        "the importer's covered mark refuses the reused identity, as applying the retired birth would"
    );
    assert_eq!(own_mark(&j, scope).await, (author, retired));
    assert_eq!(titles(&j).await, vec![vec![text("x"), text("kept")]]);
}

async fn import_kept(
    lineage: &Lineage,
    source: &Replica,
    target: &Replica,
    key: &PrivateKey,
    scope: ResourceScope,
    sealed: Commitment,
) -> (crate::SignedCheckpoint, crate::RebootstrapReport) {
    let credentials = lineage.credentials(3, key);
    source
        .admit_peer(key.public_key(), &credentials, target.followed_tip().await)
        .await
        .unwrap();
    let offered = source
        .seal_checkpoint_offer(key.public_key(), &credentials, scope, sealed)
        .await
        .unwrap()
        .expect("a kept checkpoint");
    assert_eq!(offered.statement.seal, Some(sealed));
    let checkpoint = offered.id().unwrap();
    target.begin_rebootstrap(offered.clone()).await.unwrap();
    let mut after = None;
    loop {
        let page = source
            .checkpoint_page(
                key.public_key(),
                &credentials,
                scope,
                checkpoint,
                after,
                64 * 1024,
            )
            .await
            .unwrap();
        after = page.next;
        target.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
    let report = target.finish_rebootstrap(scope).await.unwrap();
    (offered, report)
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_kept_checkpoint_signs_the_covered_marks_and_later_births_apply_on_top() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let scope = lineage.scope("items");
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    a.keep_seals(KeptSeals {
        every: true,
        ..KeptSeals::default()
    })
    .await
    .unwrap();
    insert(&a, "x", "kept").await;
    let (author, covered) = own_mark(&a, scope).await;
    let sealed = seal(&lineage, &a).await.id().unwrap();
    insert(&a, "y", "later").await;
    a.flush().await.unwrap();
    assert!(a.maintain().await);
    assert_eq!(own_mark(&a, scope).await, (author, covered + 1));
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    import_kept(&lineage, &a, &b, &keys[1], lineage.scope("tags"), sealed).await;
    let (offered, report) = import_kept(&lineage, &a, &b, &keys[1], scope, sealed).await;
    assert_eq!(
        marks(&offered),
        vec![(author, covered)],
        "births after the seal raise the live mark, not the kept seal's"
    );
    assert!(report.rejected.is_empty(), "{report:?}");
    sync(&lineage, &a, &b, &keys[1]).await;
    assert_eq!(
        titles(&b).await,
        titles(&a).await,
        "the importer takes the kept base and applies y beyond it"
    );
    assert_eq!(titles(&b).await.len(), 2);
    assert_eq!(own_mark(&b, scope).await, (author, covered + 1));
}

async fn identified(replica: &Replica) -> Vec<Vec<SqlValue>> {
    super::super::generations::select(replica, "SELECT key,id,title FROM items ORDER BY key").await
}
async fn transaction(
    replica: &Replica,
    statements: &[(&str, Option<rocky_core::RowIdentity>)],
) -> Result<(), Error> {
    let sql = session(replica).await;
    sql.run("BEGIN", vec![]).await?;
    for (statement, id) in statements {
        match id {
            Some(id) => {
                sql.run(&statement.replace("?id", &format!("X'{id}'")), vec![])
                    .await?;
            }
            None if statement.contains("?id") => {
                let id = sql.next_id("items").await?;
                sql.run(&statement.replace("?id", &format!("X'{id}'")), vec![])
                    .await?;
            }
            None => {
                sql.run(statement, vec![]).await?;
            }
        }
    }
    sql.run("COMMIT", vec![]).await.map(|_| ())
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn births_of_one_frame_compare_with_the_marks_it_entered_with() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let scope = lineage.scope("items");
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let s = open(&lineage, &dir, "s", &keys[1]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "x", "first").await;
    let (higher, lower) = {
        let sql = session(&a).await;
        sql.run("BEGIN", vec![]).await.unwrap();
        let lower = sql.next_id("items").await.unwrap();
        let higher = sql.next_id("items").await.unwrap();
        sql.run("ROLLBACK", vec![]).await.unwrap();
        (higher, lower)
    };
    transaction(
        &a,
        &[
            (
                "INSERT INTO items(key,title,id) VALUES('h','higher',?id)",
                Some(higher),
            ),
            (
                "INSERT INTO items(key,title,id) VALUES('l','lower',?id)",
                Some(lower),
            ),
        ],
    )
    .await
    .unwrap();
    let duplicate = transaction(
        &a,
        &[
            (
                "INSERT INTO items(key,title,id) VALUES('d1','one',?id)",
                None,
            ),
            (
                "INSERT INTO items(key,title,id) VALUES('d2','two',?id)",
                Some(higher),
            ),
        ],
    )
    .await;
    assert!(
        duplicate.is_err(),
        "a duplicate identity in one frame stays refused"
    );
    sync(&lineage, &a, &s, &keys[1]).await;
    s.follow_bookmark(lineage.domain, "main").await.unwrap();
    let expected = identified(&a).await;
    assert_eq!(
        expected.len(),
        3,
        "two births in descending sequence order in one frame both stand"
    );
    assert_eq!(identified(&s).await, expected);
    seal(&lineage, &a).await;
    crate::tests::exchange_graphs(&a, &s, lineage.domain).await;
    assert!(
        s.compact(lineage.domain, 0)
            .await
            .unwrap()
            .compacted
            .is_some()
    );
    s.rebuild_projection(scope).await.unwrap();
    assert_eq!(
        identified(&s).await,
        expected,
        "replaying the frame through the ledger at compaction keeps both births"
    );
    s.audit(scope).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_signed_empty_base_carries_its_retired_marks() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let scope = lineage.scope("items");
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let s = open(&lineage, &dir, "s", &keys[1]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "r", "retired").await;
    let (author, retired) = own_mark(&a, scope).await;
    delete(&a, "r").await;
    compacted_seal(&lineage, &a, &s, &keys[1]).await;
    let j = open(&lineage, &dir, "j", &keys[2]).await;
    let (offered, _) = import_all(&lineage, &s, &j, &keys[2]).await;
    assert_eq!(offered.statement.rows.count, 0);
    assert_eq!(marks(&offered), vec![(author, retired)]);
    compact_own(&lineage, &a).await;
    rewind_mark(&a, scope, retired - 1).await;
    insert(&a, "r2", "reused").await;
    let reused = last_operation(&a, scope).await;
    sync(&lineage, &a, &j, &keys[2]).await;
    assert_eq!(dropped(&j, reused).await.as_deref(), Some("admission"));
    j.rebuild_projection(scope).await.unwrap();
    assert!(titles(&j).await.is_empty());
    j.audit(scope).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_kept_prefix_seed_counts_retired_births_and_is_unavailable_once_pruned() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let scope = lineage.scope("items");
    let a = open(&lineage, &dir, "a", &key).await;
    follow_main(&lineage, &a).await;
    a.keep_seals(KeptSeals {
        every: true,
        ..KeptSeals::default()
    })
    .await
    .unwrap();
    insert(&a, "x", "kept").await;
    insert(&a, "y", "retired").await;
    let (author, retired) = own_mark(&a, scope).await;
    delete(&a, "y").await;
    let sealed = seal(&lineage, &a).await.id().unwrap();
    a.finalize_path(lineage.domain).await.unwrap();
    insert(&a, "w", "later").await;
    assert_eq!(
        a.kept_prefix_marks(scope, sealed).await,
        Some(vec![(author, retired)]),
        "marks run through the kept bound: y's retired birth, not w's later one"
    );
    {
        let inner = a.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute(
                "DELETE FROM secure_operations WHERE hash=(SELECT a.hash FROM secure_applied a JOIN secure_scope_state s ON s.id=a.scope WHERE s.scope=? ORDER BY a.position LIMIT 1)",
                [encode(&scope).unwrap()],
            )
            .await
            .unwrap();
    }
    assert_eq!(
        a.kept_prefix_marks(scope, sealed).await,
        None,
        "with the prefix pruned and no base at or before the bound, the seed is unavailable, not empty"
    );
}
