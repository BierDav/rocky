use super::*;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_keyless_scope_without_covered_payload_has_a_truthful_zero_bound() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "one", "one").await;
    sync(&lineage, &a, &b, &keys[1]).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let held = std::mem::take(
        &mut b
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .config
            .keys,
    );
    let sealed = seal(&lineage, &a).await.id().unwrap();
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.finalize_path(lineage.domain).await.unwrap();
    let bounds=crate::storage::collect_rows(b.inner.lock(crate::replica::Class::Background).await.conn.query("SELECT b.position FROM secure_seal_bounds b JOIN secure_scope_state s ON s.id=b.scope WHERE s.scope=?",[encode(&lineage.scope("tags")).unwrap()]).await.unwrap()).await.unwrap();
    assert_eq!(bounds, vec![vec![SqlValue::Integer(0)]]);
    b.inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .keys = held;
    b.compact(lineage.domain, 0).await.unwrap();
    b.admit_peer(
        keys[0].public_key(),
        &lineage.credentials(3, &keys[0]),
        a.followed_tip().await,
    )
    .await
    .unwrap();
    let empty = b
        .checkpoint_offer(
            keys[0].public_key(),
            &lineage.credentials(3, &keys[0]),
            lineage.scope("tags"),
        )
        .await
        .unwrap()
        .unwrap();
    assert_empty_checkpoint(&empty, sealed);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn deferred_intermediate_seals_are_never_signed_from_an_earlier_empty_base() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    let initial = seal(&lineage, &a).await.id().unwrap();
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    b.compact(lineage.domain, 0).await.unwrap();
    let held = std::mem::take(
        &mut b
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .config
            .keys,
    );
    insert(&a, "one", "one").await;
    let frames = crate::tests::stored_frames(&a, "SELECT header,body FROM secure_operations").await;
    let intermediate = seal(&lineage, &a).await.id().unwrap();
    let last = seal(&lineage, &a).await.id().unwrap();
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    for frame in frames {
        b.receive(frame).await.unwrap();
    }
    b.finalize_path(lineage.domain).await.unwrap();
    assert_eq!(
        b.checkpoint(lineage.scope("items"))
            .await
            .unwrap()
            .statement
            .seal,
        Some(initial)
    );
    b.inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .keys = held;
    b.settle().await.unwrap();
    b.finalize_path(lineage.domain).await.unwrap();
    b.compact(lineage.domain, 1).await.unwrap();
    b.admit_peer(
        keys[0].public_key(),
        &lineage.credentials(3, &keys[0]),
        a.followed_tip().await,
    )
    .await
    .unwrap();
    assert!(
        b.seal_checkpoint_offer(
            keys[0].public_key(),
            &lineage.credentials(3, &keys[0]),
            lineage.scope("items"),
            intermediate
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        b.unlisted_checkpoints(lineage.domain)
            .await
            .unwrap()
            .iter()
            .all(|record| record.table != lineage.scope("items") || record.seal != intermediate)
    );
    b.compact(lineage.domain, 0).await.unwrap();
    let signed = b.checkpoint(lineage.scope("items")).await.unwrap();
    assert_eq!(signed.statement.seal, Some(last));
    assert_eq!(signed.statement.operations, 1);
    assert_eq!(signed.statement.rows.count, 1);
}
