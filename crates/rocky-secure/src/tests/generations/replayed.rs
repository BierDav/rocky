use super::*;

async fn outcomes(replica: &EncryptedReplica<TypedRows>) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    rows(
        &inner.conn,
        "SELECT era,dropped,sealed FROM secure_replayed ORDER BY era,clock,author,sequence",
    )
    .await
    .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_late_write_of_a_removed_column_drops_as_schema_in_the_successor() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let (alice_key, carol_key) = (PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open(
            &dir.path().join("alice.db"),
            &alice_key,
            3,
            Some(lineage.projection(3)),
        )
        .await
        .unwrap();
    let carol = lineage
        .open(
            &dir.path().join("carol.db"),
            &carol_key,
            3,
            Some(lineage.projection(3)),
        )
        .await
        .unwrap();
    let items = lineage.scope("items");
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items VALUES('0001','one',NULL,NULL)",
        vec![],
    )
    .await
    .unwrap();
    super::sync(&alice, &carol, &carol_key, &lineage, 3, items).await;
    let column_the_successor_removes = "UPDATE items SET note='late' WHERE key='0001'";
    let column_it_keeps = "UPDATE items SET title='kept' WHERE key='0001'";
    write(&carol, column_the_successor_removes).await;
    write(&carol, column_it_keeps).await;

    let statements = lineage.publish(&alice, 4).await.unwrap();
    for (replica, key) in [(&alice, &alice_key), (&carol, &carol_key)] {
        let mut config = lineage.config(4, key);
        config.credentials = lineage.credentials(3, key);
        assert!(
            lineage
                .follow(replica, config, 4, &statements)
                .await
                .unwrap()
        );
        replica.finalize_path(lineage.domain).await.unwrap();
    }
    super::sync(&carol, &alice, &alice_key, &lineage, 4, items).await;
    for replica in [&alice, &carol] {
        assert!(replica.held_changes().await.unwrap().is_empty());
        assert_eq!(
            select(replica, "SELECT key,title FROM items").await,
            vec![vec![text("0001"), text("kept")]]
        );
        assert_eq!(
            outcomes(replica).await,
            vec![
                vec![SqlValue::Integer(0), SqlValue::Null, SqlValue::Integer(1)],
                vec![SqlValue::Integer(1), text("schema"), SqlValue::Integer(0)],
                vec![SqlValue::Integer(1), SqlValue::Null, SqlValue::Integer(0)],
            ]
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_intent_on_an_abandoned_line_is_listed_as_a_held_change() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let alice = lineage
        .open(
            &dir.path().join("alice.db"),
            &key,
            3,
            Some(lineage.projection(3)),
        )
        .await
        .unwrap();
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items VALUES('0001','one',NULL,NULL)",
        vec![],
    )
    .await
    .unwrap();
    super::compaction::follow_main(&lineage, &alice).await;
    let kept = alice.followed_tip().await;
    lineage
        .advance(&alice, lineage.config(3, &key), 3)
        .await
        .unwrap();
    let abandoned = "UPDATE items SET title='abandoned' WHERE key='0001'";
    write(&alice, abandoned).await;
    let old = alice.followed_tip().await;
    assert_ne!(old, kept);
    let seal = super::compaction::sign_seal(
        &lineage,
        old,
        alice.seal_frontier(lineage.domain, false).await.unwrap(),
    );
    alice
        .receive_tip_statements(&[
            crate::TipStatement::Seal(seal),
            super::compaction::bookmark(&lineage, Some(old), kept),
        ])
        .await
        .unwrap();
    assert_eq!(alice.followed_tip().await, kept);
    assert_eq!(
        select(&alice, "SELECT title FROM items").await,
        vec![vec![text("one")]]
    );
    let held = alice.held_changes().await.unwrap();
    assert_eq!(
        held.len(),
        1,
        "the abandoned intent is not dropped silently"
    );
    assert!(held[0].rows.is_empty());
    assert_eq!(held[0].intents.len(), 1);
    assert_eq!(held[0].intents[0].table, "items");
    assert_eq!(held[0].intents[0].scope, lineage.scope("items"));
    assert_eq!(
        held[0].intents[0].statements[0].sql,
        "UPDATE items SET title = 'abandoned' WHERE key = '0001'"
    );
    alice.drop_held(held[0].id).await.unwrap();
    assert!(alice.held_changes().await.unwrap().is_empty());
}
