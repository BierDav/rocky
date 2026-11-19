use super::super::compaction::sync;
use super::*;
use crate::tests::stored_frames;

pub(super) async fn authored(
    source: &EncryptedReplica<TypedRows>,
    key: &PrivateKey,
) -> Vec<EncryptedFrame> {
    stored_frames(source, "SELECT header,body FROM secure_operations")
        .await
        .into_iter()
        .filter(|frame| frame.operation().unwrap().header.public_key == key.public_key())
        .collect()
}
async fn import(
    lineage: &Lineage,
    source: &EncryptedReplica<TypedRows>,
    target: &EncryptedReplica<TypedRows>,
    key: &PrivateKey,
    scope: ResourceScope,
) -> crate::SignedCheckpoint {
    let credentials = lineage.credentials(3, key);
    source
        .admit_peer(key.public_key(), &credentials, target.followed_tip().await)
        .await
        .unwrap();
    let offered = source
        .checkpoint_offer(key.public_key(), &credentials, scope)
        .await
        .unwrap()
        .expect("a base");
    let checkpoint = offered.id().unwrap();
    target.begin_rebootstrap(offered.clone()).await.unwrap();
    lineage.install_required(target).await;
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
    target.finish_rebootstrap(scope).await.unwrap();
    offered
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_seal_to_a_new_schema_keeps_local_work_and_a_follower_switches_only_with_it() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let (alice_key, bob_key) = (PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open(&dir.path().join("alice.db"), &alice_key, 1, None)
        .await
        .unwrap();
    let bob = lineage
        .open(&dir.path().join("bob.db"), &bob_key, 1, None)
        .await
        .unwrap();
    let items = lineage.scope("items");
    super::super::compaction::follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    insert(&alice, 0..2).await;
    sync(&lineage, &alice, &bob, &bob_key).await;
    let third_row_unsent_at_the_schema_change = 2..3;
    insert(&alice, third_row_unsent_at_the_schema_change).await;
    let original = alice.followed_tip().await;
    let statements = lineage.publish(&alice, 2).await.unwrap();
    let target = alice
        .tip_graph(lineage.domain)
        .await
        .unwrap()
        .bookmark("main")
        .unwrap()
        .unwrap();
    let pending = alice.pending_schema().await.unwrap().unwrap();
    assert_eq!(
        (pending.tip, pending.manifest),
        (target, lineage.definition(2).hash())
    );
    assert!(
        session(&alice)
            .await
            .run("UPDATE items SET title='refused'", vec![])
            .await
            .is_err(),
        "until the switch, the workspace waits read-only at the old tip"
    );
    alice
        .install_tip_schema(target, lineage.definition(2), b"checked".to_vec())
        .await
        .unwrap();
    let mut projection = lineage.projection(2);
    projection.0.tables[0].columns[2].not_null = true;
    let mut config = lineage.config(2, &alice_key);
    config.manifest = lineage.config(1, &alice_key).manifest;
    assert!(
        alice
            .switch_schema(
                target,
                lineage.app(2),
                crate::SchemaConfig {
                    security: config,
                    tables: Some(projection)
                },
            )
            .await
            .is_err()
    );
    assert_eq!(
        alice.followed_tip().await,
        original,
        "a projection other than the tip's schema activates nothing"
    );
    assert!(
        lineage
            .follow(&alice, lineage.config(2, &alice_key), 2, &statements)
            .await
            .unwrap()
    );
    assert_eq!(alice.followed_tip().await, target);
    assert_eq!(
        select(&alice, "SELECT key,title,note FROM items ORDER BY key").await,
        (0..3)
            .map(|id| vec![
                text(&format!("{id:04}")),
                text(&format!("title {id}")),
                SqlValue::Null
            ])
            .collect::<Vec<_>>(),
        "existing rows gain the new column, empty"
    );
    let writer = session(&alice).await;
    identity_insert(
        &writer,
        "INSERT INTO items VALUES('0100','new','with a note')",
        vec![],
    )
    .await
    .unwrap();
    identity_insert(&writer, "INSERT INTO tags VALUES('t','label')", vec![])
        .await
        .unwrap();
    drop(writer);

    let credentials = lineage.credentials(1, &bob_key);
    alice
        .admit_peer(bob_key.public_key(), &credentials, bob.followed_tip().await)
        .await
        .unwrap();
    assert!(matches!(
        alice
            .transfer_page(
                bob_key.public_key(),
                &credentials,
                &[items],
                None,
                MAX_SECURE_FRAME
            )
            .await,
        Err(Error::Rejected(_))
    ));
    assert_eq!(
        select(&bob, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(2)]],
        "a peer still at the old tip is served nothing across the tips"
    );
    assert!(
        lineage
            .follow(&bob, lineage.config(2, &bob_key), 2, &statements)
            .await
            .unwrap()
    );
    sync(&lineage, &alice, &bob, &bob_key).await;
    assert_eq!(
        select(&bob, "SELECT key,note FROM items ORDER BY key").await,
        vec![
            vec![text("0000"), SqlValue::Null],
            vec![text("0001"), SqlValue::Null],
            vec![text("0002"), SqlValue::Null],
            vec![text("0100"), text("with a note")],
        ],
        "following the bookmark switches the peer, which then catches up"
    );
    alice.audit(items).await.unwrap();
    bob.audit(items).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_base_at_a_seal_of_the_previous_schema_rebootstraps_typed_rows_and_tombstones() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let (alice_key, bob_key, carol_key) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open(&dir.path().join("alice.db"), &alice_key, 1, None)
        .await
        .unwrap();
    let bob = lineage
        .open(&dir.path().join("bob.db"), &bob_key, 1, None)
        .await
        .unwrap();
    let items = lineage.scope("items");
    super::super::compaction::follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    insert(&alice, 0..40).await;
    write(&alice, "DELETE FROM items WHERE key='0003'").await;
    write(&alice, "UPDATE items SET title='kept' WHERE key='0004'").await;
    sync(&lineage, &alice, &bob, &bob_key).await;
    alice
        .acknowledge_frontier(
            bob_key.public_key(),
            &lineage.credentials(3, &bob_key),
            &bob.frontier(items).await.unwrap(),
        )
        .await
        .unwrap();
    assert!(
        lineage
            .advance(&alice, lineage.config(2, &alice_key), 2)
            .await
            .unwrap()
    );
    alice
        .set_retention_policy(
            items,
            RetentionPolicy {
                required: [rocky_credentials::PeerId::of(bob_key.public_key())].into(),
                behind: [rocky_credentials::PeerId::of(carol_key.public_key())].into(),
                copies: 1,
                window: 0,
            },
        )
        .await
        .unwrap();
    let report = alice.compact(lineage.domain, 0).await.unwrap();
    assert!(
        report.pruned > 0,
        "the schema change is a seal the writer compacts at and prunes"
    );
    let base = alice.checkpoint(items).await.unwrap();
    assert_eq!(base.statement.seal, report.compacted);
    assert_eq!(
        alice
            .tip_graph(lineage.domain)
            .await
            .unwrap()
            .schema(base.statement.tip)
            .unwrap(),
        lineage.definition(1).hash(),
        "the base sits at the seal of the first schema's tip"
    );
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items VALUES('0100','after','noted')",
        vec![],
    )
    .await
    .unwrap();

    let carol = lineage
        .open(&dir.path().join("carol.db"), &carol_key, 1, None)
        .await
        .unwrap();
    let statements = crate::tests::all_tip_statements(&alice).await;
    assert!(
        lineage
            .follow(&carol, lineage.config(2, &carol_key), 2, &statements)
            .await
            .unwrap()
    );
    let offered = import(&lineage, &alice, &carol, &carol_key, items).await;
    assert_eq!(
        offered.id().unwrap(),
        base.id().unwrap(),
        "a joiner following `main` to the second schema imports the first schema's base"
    );
    let needs = carol.seal_rebootstraps(lineage.domain).await.unwrap();
    assert_eq!(
        needs.iter().map(|need| need.scope).collect::<Vec<_>>(),
        vec![lineage.scope("tags")],
        "the empty `tags` scope has no state the joiner verified"
    );
    super::super::compaction::list_checkpoints(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &carol, lineage.domain).await;
    let tags = import(&lineage, &alice, &carol, &carol_key, lineage.scope("tags")).await;
    super::super::compaction::assert_empty_checkpoint(&tags, report.compacted.unwrap());
    assert!(
        carol
            .seal_rebootstraps(lineage.domain)
            .await
            .unwrap()
            .is_empty()
    );
    carol.audit(items).await.unwrap();
    sync(&lineage, &alice, &carol, &carol_key).await;
    carol.audit(items).await.unwrap();
    assert_eq!(
        select(&carol, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(40)]]
    );
    assert_eq!(
        select(&carol, "SELECT title,note FROM items WHERE key='0004'").await,
        vec![vec![text("kept"), SqlValue::Null]]
    );
    assert_eq!(
        select(&carol, "SELECT note FROM items WHERE key='0100'").await,
        vec![vec![text("noted")]]
    );
    let old_id =
        rocky_core::RowIdentity::new(crate::writer::writer(&alice_key.public_key()), 4).unwrap();
    assert!(
        session(&carol)
            .await
            .run(
                "INSERT INTO items(key,title,note,id) VALUES('0003','again',NULL,?)",
                vec![old_id.sql()]
            )
            .await
            .is_err(),
        "the base keeps the old numeric identity's sequence mark"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_open_replica_follows_an_additive_schema_in_place() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let (alice_key, bob_key) = (PrivateKey::new(), PrivateKey::new());
    let alice_path = dir.path().join("alice.db");
    let alice = lineage
        .open(&alice_path, &alice_key, 1, None)
        .await
        .unwrap();
    let bob = lineage
        .open(&dir.path().join("bob.db"), &bob_key, 1, None)
        .await
        .unwrap();
    let (items, tags) = (lineage.scope("items"), lineage.scope("tags"));
    super::super::compaction::follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    insert(&alice, 0..3).await;
    let statements = lineage.publish(&alice, 3).await.unwrap();
    let target = alice.pending_schema().await.unwrap().unwrap().tip;
    assert!(
        lineage
            .follow(&bob, lineage.config(3, &bob_key), 3, &statements)
            .await
            .unwrap()
    );
    identity_insert(
        &session(&bob).await,
        "INSERT INTO items VALUES('0100','ranked',NULL,7)",
        vec![],
    )
    .await
    .unwrap();
    let newer = authored(&bob, &bob_key).await;
    assert!(!newer.is_empty());
    let mut observer = alice.observe_tables(None).await.unwrap();
    let before = session(&alice).await;
    assert!(
        alice
            .install_tip_schema(target, lineage.definition(5), b"forged".to_vec())
            .await
            .is_err(),
        "a record that does not derive the tip's schema is refused"
    );
    assert!(
        alice
            .tip_schema(lineage.definition(5).hash())
            .await
            .unwrap()
            .is_none(),
        "a refused schema record saves nothing"
    );

    assert!(
        lineage
            .follow(&alice, lineage.config(3, &alice_key), 3, &statements)
            .await
            .unwrap()
    );
    assert_eq!(alice.followed_tip().await, target);
    assert!(
        before.run("SELECT 1", vec![]).await.is_ok(),
        "an additive switch keeps sessions"
    );
    assert_eq!(
        observer.try_next().unwrap(),
        None,
        "an additive switch notifies no observer"
    );
    for frame in newer {
        let _ = alice.receive(frame).await;
    }
    assert_eq!(
        select(&alice, "SELECT key,note,rank FROM items ORDER BY key").await,
        (0..3)
            .map(|id| vec![text(&format!("{id:04}")), SqlValue::Null, SqlValue::Null])
            .chain([vec![text("0100"), SqlValue::Null, SqlValue::Integer(7)]])
            .collect::<Vec<_>>()
    );
    assert!(
        !lineage
            .follow(&alice, lineage.config(3, &alice_key), 3, &statements)
            .await
            .unwrap(),
        "following the current schema again changes nothing"
    );
    identity_insert(
        &session(&alice).await,
        "INSERT INTO tags VALUES('t','label')",
        vec![],
    )
    .await
    .unwrap();
    for frame in frames(&alice, &bob, tags).await {
        bob.receive(frame).await.unwrap();
    }
    assert_eq!(
        select(&bob, "SELECT label FROM tags").await,
        vec![vec![text("label")]],
        "writes after the switch reach the peer on the same tip"
    );
    drop(before);
    drop(observer);
    drop(alice);
    let alice = lineage.reopen(&alice_path, &alice_key, 1, 3).await.unwrap();
    assert_eq!(
        alice.followed_tip().await,
        target,
        "the file reopens on the switched tip"
    );
    alice.audit(items).await.unwrap();
    assert_eq!(
        select(&alice, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(4)]]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_schema_removes_a_field_and_a_table_on_every_replica_that_follows_it() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let (alice_key, carol_key) = (PrivateKey::new(), PrivateKey::new());
    let alice_path = dir.path().join("alice.db");
    let alice = lineage
        .open(&alice_path, &alice_key, 3, None)
        .await
        .unwrap();
    let carol = lineage
        .open(&dir.path().join("carol.db"), &carol_key, 3, None)
        .await
        .unwrap();
    let items = lineage.scope("items");
    super::super::compaction::follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &carol, lineage.domain).await;
    for (key, title) in [("0001", "one"), ("0002", "two")] {
        identity_insert(
            &session(&alice).await,
            &format!("INSERT INTO items VALUES('{key}','{title}','noted',NULL)"),
            vec![],
        )
        .await
        .unwrap();
    }
    identity_insert(
        &session(&alice).await,
        "INSERT INTO tags VALUES('t','label')",
        vec![],
    )
    .await
    .unwrap();
    sync(&lineage, &alice, &carol, &carol_key).await;

    assert!(
        lineage
            .advance(&alice, lineage.config(4, &alice_key), 4)
            .await
            .unwrap()
    );
    let reader = session(&alice).await;
    assert!(
        reader.run("SELECT note FROM items", vec![]).await.is_err(),
        "the removed field is gone"
    );
    assert!(
        reader.run("SELECT * FROM tags", vec![]).await.is_err(),
        "the removed table is gone"
    );
    drop(reader);
    let expected = vec![
        vec![text("0001"), text("one")],
        vec![text("0002"), text("two")],
    ];
    assert_eq!(
        select(&alice, "SELECT key,title FROM items ORDER BY key").await,
        expected,
        "every remaining row stays visible"
    );
    alice.compact(lineage.domain, 0).await.unwrap();
    alice.audit(items).await.unwrap();

    write(
        &carol,
        "UPDATE items SET title='uno', note='carol' WHERE key='0001'",
    )
    .await;
    write(&carol, "UPDATE tags SET label='other'").await;
    for frame in authored(&carol, &carol_key).await {
        let _ = alice.receive(frame).await;
    }
    assert_eq!(
        select(&alice, "SELECT title FROM items WHERE key='0001'").await,
        vec![vec![text("one")]],
        "an offline transaction touching a removed field replays on the new schema and drops whole"
    );
    alice.audit(items).await.unwrap();
    drop(alice);
    let alice = lineage.reopen(&alice_path, &alice_key, 3, 4).await.unwrap();
    alice.audit(items).await.unwrap();
    alice.drop_unrequired_schemas().await.unwrap();
    drop(alice);
    let alice = lineage.reopen(&alice_path, &alice_key, 3, 4).await.unwrap();
    alice.audit(items).await.unwrap();
    let kept = vec![
        vec![text("0001"), text("one")],
        vec![text("0002"), text("two")],
    ];
    assert_eq!(
        select(&alice, "SELECT key,title FROM items ORDER BY key").await,
        kept,
        "the removal survives reopening and dropping schemas nothing needs"
    );
    write(&carol, "UPDATE items SET note='again' WHERE key='0002'").await;
    for frame in authored(&carol, &carol_key).await {
        let _ = alice.receive(frame).await;
    }
    assert_eq!(
        select(&alice, "SELECT key,title FROM items ORDER BY key").await,
        kept
    );
    alice.audit(items).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_readded_field_never_receives_a_value_written_before_its_removal() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys: Vec<_> = (0..4).map(|_| PrivateKey::new()).collect();
    let mut replicas = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        replicas.push(
            lineage
                .open(&dir.path().join(format!("{index}.db")), key, 3, None)
                .await
                .unwrap(),
        );
    }
    let [alice, bob, dave, erin] = &replicas[..] else {
        unreachable!()
    };
    let [alice_key, bob_key, dave_key, erin_key] = &keys[..] else {
        unreachable!()
    };
    let items = lineage.scope("items");
    super::super::compaction::follow_main(&lineage, alice).await;
    for replica in [bob, dave, erin] {
        crate::tests::exchange_graphs(alice, replica, lineage.domain).await;
    }
    let notes_written_while_the_field_exists = [("0001", "old"), ("0002", "stale")];
    for (key, note) in notes_written_while_the_field_exists {
        identity_insert(
            &session(alice).await,
            &format!("INSERT INTO items VALUES('{key}','title','{note}',NULL)"),
            vec![],
        )
        .await
        .unwrap();
    }
    for (target, key) in [(bob, bob_key), (dave, dave_key)] {
        sync(&lineage, alice, target, key).await;
    }
    write(dave, "UPDATE items SET note='late at A' WHERE key='0002'").await;
    crate::tests::check_frontier_cache(alice, items).await;
    crate::tests::check_frontier_cache(bob, items).await;
    let removed = lineage.publish(alice, 4).await.unwrap();
    assert!(
        lineage
            .follow(alice, lineage.config(4, alice_key), 4, &removed)
            .await
            .unwrap()
    );
    crate::tests::check_frontier_cache(alice, items).await;
    alice.compact(lineage.domain, 0).await.unwrap();
    crate::tests::check_frontier_cache(alice, items).await;
    let readded = lineage.publish(alice, 5).await.unwrap();
    assert!(
        lineage
            .follow(alice, lineage.config(5, alice_key), 5, &readded)
            .await
            .unwrap()
    );
    assert!(
        lineage
            .follow(bob, lineage.config(5, bob_key), 5, &readded)
            .await
            .unwrap()
    );
    assert!(
        bob.tip_schema(lineage.definition(4).hash())
            .await
            .unwrap()
            .is_some(),
        "following A to C at once still needs B's schema for the path"
    );
    let notes = "SELECT key,note FROM items ORDER BY key";
    let empty = vec![
        vec![text("0001"), SqlValue::Null],
        vec![text("0002"), SqlValue::Null],
    ];
    assert_eq!(select(alice, notes).await, empty);
    assert_eq!(select(bob, notes).await, empty);
    crate::tests::check_frontier_cache(alice, items).await;
    crate::tests::check_frontier_cache(bob, items).await;
    for replica in [alice, bob] {
        for frame in authored(dave, dave_key).await {
            let _ = replica.receive(frame).await;
        }
        assert_eq!(
            select(replica, notes).await,
            empty,
            "a late write at A crosses B, so its `note` never reaches C"
        );
        replica.audit(items).await.unwrap();
    }
    write(alice, "UPDATE items SET note='at C' WHERE key='0001'").await;
    for frame in authored(alice, alice_key).await {
        let _ = bob.receive(frame).await;
    }
    let written = vec![
        vec![text("0001"), text("at C")],
        vec![text("0002"), SqlValue::Null],
    ];
    assert_eq!(
        select(bob, notes).await,
        written,
        "a write at C carries the field again"
    );
    let statements = crate::tests::all_tip_statements(alice).await;
    assert!(
        lineage
            .follow(erin, lineage.config(5, erin_key), 5, &statements)
            .await
            .unwrap()
    );
    import(&lineage, alice, erin, erin_key, items).await;
    crate::tests::check_frontier_cache(erin, items).await;
    sync(&lineage, alice, erin, erin_key).await;
    assert_eq!(
        select(erin, notes).await,
        written,
        "a base at A's seal, imported at C, carries no earlier note either"
    );
    erin.audit(items).await.unwrap();
}

async fn tags_with_parents(replica: &EncryptedReplica<TypedRows>) -> Vec<Vec<SqlValue>> {
    select(
        replica,
        "SELECT t.key,i.key,a.key FROM tags t LEFT JOIN items i ON i.id=t.item LEFT JOIN items a ON a.id=t.also ORDER BY t.key",
    )
    .await
}
async fn declared_references(replica: &EncryptedReplica<TypedRows>) -> (Vec<Vec<SqlValue>>, bool) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    let violations = rows(
        &inner.conn,
        "SELECT * FROM pragma_foreign_key_check('tags')",
    )
    .await
    .unwrap();
    let deferred = rows(
        &inner.conn,
        "SELECT 1 FROM sqlite_master WHERE name='tags' AND sql LIKE '%DEFERRABLE INITIALLY DEFERRED%'",
    )
    .await
    .unwrap();
    (violations, deferred.len() == 1)
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_changed_reference_rebuilds_children_of_an_indexed_identity_alike_before_and_after_their_rows_arrive()
 {
    let mut lineage = Lineage::new();
    lineage.linked = true;
    lineage.deferred_from = Some(3);
    let dir = Scratch::new().await;
    let (alice_key, carol_key) = (PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open(&dir.path().join("alice.db"), &alice_key, 2, None)
        .await
        .unwrap();
    let carol = lineage
        .open(&dir.path().join("carol.db"), &carol_key, 2, None)
        .await
        .unwrap();
    super::super::compaction::follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &carol, lineage.domain).await;
    let mut parents = Vec::new();
    for key in ["0001", "0002"] {
        identity_insert(
            &session(&alice).await,
            &format!("INSERT INTO items VALUES('{key}','title',NULL)"),
            vec![],
        )
        .await
        .unwrap();
        let id = format!("SELECT id FROM items WHERE key='{key}'");
        parents.extend(select(&alice, &id).await.remove(0));
    }
    identity_insert(
        &session(&alice).await,
        "INSERT INTO tags(key,label,item,also) VALUES('t','l',?,?)",
        parents,
    )
    .await
    .unwrap();
    let statements = lineage.publish(&alice, 3).await.unwrap();
    for (replica, key) in [(&alice, &alice_key), (&carol, &carol_key)] {
        assert!(
            lineage
                .follow(replica, lineage.config(3, key), 3, &statements)
                .await
                .unwrap(),
            "a switch that rebuilds a child holding references to an identity kept unique by an index succeeds"
        );
    }
    for table in ["items", "tags"] {
        super::sync(
            &alice,
            &carol,
            &carol_key,
            &lineage,
            3,
            lineage.scope(table),
        )
        .await;
    }
    let expected = vec![vec![text("t"), text("0001"), text("0002")]];
    for replica in [&alice, &carol] {
        assert_eq!(tags_with_parents(replica).await, expected);
        assert_eq!(
            declared_references(replica).await,
            (Vec::new(), true),
            "the rebuilt child declares the new reference and every row satisfies it"
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_field_readded_with_a_reference_is_followed_from_before_its_removal() {
    let mut lineage = Lineage::new();
    lineage.note_references_from = Some(5);
    let dir = Scratch::new().await;
    let (alice_key, bob_key) = (PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open(&dir.path().join("alice.db"), &alice_key, 3, None)
        .await
        .unwrap();
    let bob = lineage
        .open(&dir.path().join("bob.db"), &bob_key, 3, None)
        .await
        .unwrap();
    super::super::compaction::follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items VALUES('0001','title','not an item',NULL)",
        vec![],
    )
    .await
    .unwrap();
    sync(&lineage, &alice, &bob, &bob_key).await;
    let notes = "SELECT key,note FROM items ORDER BY key";
    assert_eq!(
        select(&bob, notes).await,
        vec![vec![text("0001"), text("not an item")]]
    );
    for version in [4, 5] {
        let statements = lineage.publish(&alice, version).await.unwrap();
        assert!(
            lineage
                .follow(
                    &alice,
                    lineage.config(version, &alice_key),
                    version,
                    &statements
                )
                .await
                .unwrap()
        );
    }
    let statements = crate::tests::all_tip_statements(&alice).await;
    assert!(
        lineage
            .follow(&bob, lineage.config(5, &bob_key), 5, &statements)
            .await
            .unwrap(),
        "a peer following A to C at once carries no value of a field B removed, so C may retype it and reference it"
    );
    let empty = vec![vec![text("0001"), SqlValue::Null]];
    assert_eq!(select(&alice, notes).await, empty);
    assert_eq!(select(&bob, notes).await, empty);
    let inner = bob.inner.lock(crate::replica::Class::Background).await;
    assert_eq!(
        rows(
            &inner.conn,
            "SELECT * FROM pragma_foreign_key_check('items')"
        )
        .await
        .unwrap(),
        Vec::<Vec<SqlValue>>::new()
    );
}

async fn force_rebuild(replica: &EncryptedReplica<TypedRows>, raw: &[&str]) -> Result<(), Error> {
    plan_rebuild(replica, raw).await;
    replica.run_plan().await
}
async fn plan_rebuild(replica: &EncryptedReplica<TypedRows>, raw: &[&str]) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    for sql in raw.iter().chain(&[
        "INSERT INTO secure_projection_plan SELECT 1,descriptor,domain,readers FROM secure_typed WHERE id=1",
    ]) {
        inner.conn.execute(*sql, ()).await.unwrap();
    }
}
const ORPHAN_TAG: [&str; 3] = [
    "PRAGMA foreign_keys=OFF",
    "UPDATE tags SET also=x'00'",
    "PRAGMA foreign_keys=ON",
];
struct Tagged {
    _dir: Scratch,
    path: std::path::PathBuf,
    key: PrivateKey,
    replica: EncryptedReplica<TypedRows>,
}
async fn linked_with_a_tag(lineage: &Lineage) -> Tagged {
    let dir = Scratch::new().await;
    let path = dir.path().join("alice.db");
    let key = PrivateKey::new();
    let replica = lineage.open(&path, &key, 2, None).await.unwrap();
    identity_insert(
        &session(&replica).await,
        "INSERT INTO items VALUES('0001','title',NULL)",
        vec![],
    )
    .await
    .unwrap();
    let parent = select(&replica, "SELECT id FROM items").await.remove(0);
    identity_insert(
        &session(&replica).await,
        "INSERT INTO tags(key,label,item,also) VALUES('t','l',?,NULL)",
        parent,
    )
    .await
    .unwrap();
    Tagged {
        _dir: dir,
        path,
        key,
        replica,
    }
}
fn linked() -> Lineage {
    let mut lineage = Lineage::new();
    lineage.linked = true;
    lineage
}
fn refused_for_an_orphan(result: &Result<impl std::fmt::Debug, Error>) -> bool {
    matches!(result, Err(Error::Incompatible(reason)) if reason == "the projection rebuild would keep rows that break a foreign key: a row of tags references a missing row of items")
}
async fn raw_rows(replica: &EncryptedReplica<TypedRows>, sql: &str) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    rows(&inner.conn, sql).await.unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_rebuild_fails_rather_than_commit_rows_that_break_a_foreign_key() {
    let lineage = linked();
    let tagged = linked_with_a_tag(&lineage).await;
    let refused = force_rebuild(&tagged.replica, &ORPHAN_TAG).await;
    assert!(refused_for_an_orphan(&refused), "{refused:?}");
    let orphan = vec![vec![SqlValue::Blob(vec![0])]];
    assert_eq!(
        raw_rows(&tagged.replica, "SELECT also FROM tags").await,
        orphan,
        "the refused rebuild leaves the tables as they were"
    );
    assert_eq!(tagged.replica.projection_plan().await.unwrap(), None);
    drop(tagged.replica);
    let replica = lineage.open(&tagged.path, &tagged.key, 2, None).await;
    let replica = replica.expect("a refused rebuild leaves the database openable");
    assert_eq!(raw_rows(&replica, "SELECT also FROM tags").await, orphan);
    let again = force_rebuild(&replica, &[]).await;
    assert!(
        refused_for_an_orphan(&again),
        "a retried rebuild is refused for the same reason: {again:?}"
    );
}

async fn plan_schema(replica: &EncryptedReplica<TypedRows>, raw: &[&str], schema: &LogicalSchema) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    for sql in raw {
        inner.conn.execute(*sql, ()).await.unwrap();
    }
    inner
        .conn
        .execute(
            "INSERT INTO secure_projection_plan SELECT 1,?1,domain,readers FROM secure_typed WHERE id=1",
            [schema.canonical().unwrap()],
        )
        .await
        .unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_rebuild_refused_before_its_plan_ran_never_blocks_opening() {
    let mut lineage = linked();
    let tagged = linked_with_a_tag(&lineage).await;
    lineage.deferred_from = Some(2);
    let deferred = lineage.schema(2);
    lineage.deferred_from = None;
    plan_schema(&tagged.replica, &ORPHAN_TAG, &deferred).await;
    let revision = "SELECT revision FROM secure_metadata";
    let planned_at = raw_rows(&tagged.replica, revision).await;
    drop(tagged.replica);
    let replica = lineage.open(&tagged.path, &tagged.key, 2, None).await;
    let replica = replica
        .expect("a database whose recorded rebuild is refused opens on its installed schema");
    assert_eq!(replica.projection_plan().await.unwrap(), None);
    assert_eq!(
        raw_rows(&replica, revision).await,
        planned_at,
        "opening discards the plan without running it"
    );
    let (orphans, now_deferred) = declared_references(&replica).await;
    assert_eq!(orphans.len(), 1, "the orphan stays: {orphans:?}");
    assert!(!now_deferred);
    {
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute("UPDATE tags SET also=NULL", ())
            .await
            .unwrap();
    }
    plan_schema(&replica, &[], &deferred).await;
    let fixed_at = raw_rows(&replica, revision).await;
    drop(replica);
    let replica = lineage
        .open(&tagged.path, &tagged.key, 2, None)
        .await
        .unwrap();
    assert_eq!(replica.projection_plan().await.unwrap(), None);
    assert_ne!(
        raw_rows(&replica, revision).await,
        fixed_at,
        "the same plan runs on opening once no row breaks the reference, so only the orphan refused it"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_refused_schema_switch_is_withdrawn_and_the_peer_stays_on_its_source() {
    let mut lineage = linked();
    lineage.deferred_from = Some(3);
    let tagged = linked_with_a_tag(&lineage).await;
    let alice = &tagged.replica;
    super::super::compaction::follow_main(&lineage, alice).await;
    let source = alice.followed_tip().await;
    {
        let inner = alice.inner.lock(crate::replica::Class::Background).await;
        for sql in ORPHAN_TAG {
            inner.conn.execute(sql, ()).await.unwrap();
        }
    }
    let statements = lineage.publish(alice, 3).await.unwrap();
    let refused = lineage
        .follow(alice, lineage.config(3, &tagged.key), 3, &statements)
        .await;
    assert!(refused_for_an_orphan(&refused), "{refused:?}");
    let staged = "SELECT (SELECT count(*) FROM secure_schema_switch),(SELECT count(*) FROM secure_projection_plan)";
    let nothing_staged = vec![vec![SqlValue::Integer(0), SqlValue::Integer(0)]];
    assert_eq!(
        raw_rows(alice, staged).await,
        nothing_staged,
        "a refused switch is withdrawn"
    );
    assert_eq!(alice.followed_tip().await, source);
    drop(tagged.replica);
    let alice = lineage
        .reopen(&tagged.path, &tagged.key, 2, 2)
        .await
        .expect("a peer whose switch was refused reopens on its source schema");
    assert_eq!(alice.followed_tip().await, source);
    assert_eq!(
        declared_references(&alice).await,
        (Vec::new(), false),
        "withdrawing replays the source tables from their writes, which hold no orphan"
    );
    assert_eq!(
        tags_with_parents(&alice).await,
        vec![vec![text("t"), text("0001"), SqlValue::Null]]
    );
    let retried = lineage
        .follow(&alice, lineage.config(3, &tagged.key), 3, &statements)
        .await;
    assert!(
        matches!(retried, Ok(true)),
        "the switch succeeds once the rows that broke it are gone: {retried:?}"
    );
    assert_eq!(raw_rows(&alice, staged).await, nothing_staged);
    assert_eq!(declared_references(&alice).await, (Vec::new(), true));
}

const ORPHAN_AFTER_REPLAY: [&str; 2] =
    ["PRAGMA defer_foreign_keys=ON", "UPDATE tags SET also=x'01'"];
fn tag_also(value: SqlValue) -> Vec<Vec<SqlValue>> {
    vec![vec![value]]
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_withdrawal_whose_replay_breaks_a_foreign_key_keeps_the_source_tables_and_reopens() {
    let mut lineage = linked();
    lineage.deferred_from = Some(3);
    let tagged = linked_with_a_tag(&lineage).await;
    let alice = &tagged.replica;
    super::super::compaction::follow_main(&lineage, alice).await;
    let source = alice.followed_tip().await;
    plant_orphan(alice).await;
    alice
        .run_after_next_withdrawal_replay(ORPHAN_AFTER_REPLAY.to_vec())
        .await;
    let statements = lineage.publish(alice, 3).await.unwrap();
    let refused = lineage
        .follow(alice, lineage.config(3, &tagged.key), 3, &statements)
        .await;
    assert!(
        matches!(&refused, Err(Error::Incompatible(reason)) if reason == "the projection rebuild would keep rows that break a foreign key: a row of tags references a missing row of items; the switch was withdrawn keeping the source tables, because replaying them would break a foreign key: a row of tags references a missing row of items"),
        "the refusal names the switch's rows and the replay it set aside: {refused:?}"
    );
    assert_eq!(
        raw_rows(alice, MID_SWITCH).await,
        mid_switch(0, 0),
        "the withdrawal commits without its replay, so no switch is left half done"
    );
    assert_eq!(alice.followed_tip().await, source);
    let planted = tag_also(SqlValue::Blob(vec![0]));
    assert_eq!(
        raw_rows(alice, "SELECT also FROM tags").await,
        planted,
        "the source tables stay as they were before the switch, not as the broken replay left them"
    );
    drop(tagged.replica);
    let alice = lineage
        .reopen(&tagged.path, &tagged.key, 2, 2)
        .await
        .expect("a peer whose withdrawal kept its source tables reopens on its source schema");
    assert_eq!(alice.followed_tip().await, source);
    assert_eq!(raw_rows(&alice, MID_SWITCH).await, mid_switch(0, 0));
    assert_eq!(raw_rows(&alice, "SELECT also FROM tags").await, planted);
    let again = lineage
        .follow(&alice, lineage.config(3, &tagged.key), 3, &statements)
        .await;
    assert!(
        refused_for_an_orphan(&again),
        "the next switch is refused for its own rows, not stuck behind the last one: {again:?}"
    );
    assert_eq!(raw_rows(&alice, MID_SWITCH).await, mid_switch(0, 0));
    assert_eq!(
        raw_rows(&alice, "SELECT also FROM tags").await,
        tag_also(SqlValue::Null),
        "a replay that keeps every reference is committed"
    );
    let switched = lineage
        .follow(&alice, lineage.config(3, &tagged.key), 3, &statements)
        .await;
    assert!(matches!(switched, Ok(true)), "{switched:?}");
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn withdrawing_a_stage_whose_switch_was_interrupted_keeps_the_source_tables_over_a_broken_replay()
 {
    let mut lineage = linked();
    lineage.deferred_from = Some(3);
    let tagged = linked_with_a_tag(&lineage).await;
    let staged = stage(&lineage, &tagged).await;
    tagged.replica.fail_next_plan_switch().await;
    let interrupted = switch_to_stage(&lineage, &tagged, &staged).await;
    assert!(
        matches!(interrupted, Err(Error::InjectedFailure)),
        "{interrupted:?}"
    );
    assert_eq!(
        raw_rows(&tagged.replica, MID_SWITCH).await,
        mid_switch(1, 1),
        "the switch to the stage waits with its intent and plan"
    );
    plant_orphan(&tagged.replica).await;
    tagged
        .replica
        .run_after_next_withdrawal_replay(ORPHAN_AFTER_REPLAY.to_vec())
        .await;
    withdraw_stage(&lineage, &tagged.key, &tagged.replica, staged.publication)
        .await
        .expect("withdrawing the stage sets the broken replay aside rather than refusing");
    assert_eq!(tagged.replica.followed_tip().await, staged.source);
    assert_eq!(stage_state(&tagged.replica).await, None);
    assert_eq!(
        raw_rows(&tagged.replica, MID_SWITCH).await,
        mid_switch(0, 0)
    );
    assert_eq!(
        raw_rows(&tagged.replica, "SELECT also FROM tags").await,
        tag_also(SqlValue::Blob(vec![0])),
        "the source tables the switch never replaced are kept"
    );
    drop(tagged.replica);
    let alice = lineage
        .reopen(&tagged.path, &tagged.key, 2, 2)
        .await
        .expect("the withdrawn stage leaves its source schema openable");
    assert_eq!(alice.followed_tip().await, staged.source);
    assert_eq!(stage_state(&alice).await, None);
}

struct StagedSwitch {
    source: rocky_core::TipId,
    target: rocky_core::TipId,
    publication: Commitment,
}
async fn stage(lineage: &Lineage, tagged: &Tagged) -> StagedSwitch {
    let alice = &tagged.replica;
    super::super::compaction::follow_main(lineage, alice).await;
    let source = alice.followed_tip().await;
    let mut statement = super::super::compaction::sign_seal(
        lineage,
        source,
        alice.seal_frontier(lineage.domain, false).await.unwrap(),
    )
    .statement;
    statement.action = rocky_credentials::SealAction::Continue {
        schema: lineage.definition(3).hash(),
    };
    let seal = rocky_credentials::SignedSeal::sign(statement, lineage.publisher()).unwrap();
    let target = seal.successor().unwrap().unwrap();
    let publication = rocky_core::commitment("refused.withdrawal", &target.0);
    alice
        .stage_schema(crate::SchemaStage {
            publication,
            scope: lineage.domain,
            bookmark: "main".into(),
            source,
            target,
            from_version: 2,
            coordinated: false,
            statements: vec![
                crate::TipStatement::Seal(seal),
                super::super::compaction::bookmark(lineage, Some(source), target),
            ],
            program: vec![],
            parents: vec![],
        })
        .await
        .unwrap();
    alice
        .install_tip_schema(target, lineage.definition(3), b"verified".to_vec())
        .await
        .unwrap();
    StagedSwitch {
        source,
        target,
        publication,
    }
}
async fn switch_to_stage(
    lineage: &Lineage,
    tagged: &Tagged,
    staged: &StagedSwitch,
) -> Result<bool, Error> {
    let mut security = lineage.config(3, &tagged.key);
    security.manifest = lineage.config(2, &tagged.key).manifest;
    tagged
        .replica
        .switch_schema(
            staged.target,
            lineage.app(3),
            crate::SchemaConfig {
                security,
                tables: Some(lineage.projection(3)),
            },
        )
        .await
}
async fn stage_and_switch(lineage: &Lineage, tagged: &Tagged) -> StagedSwitch {
    let staged = stage(lineage, tagged).await;
    assert!(switch_to_stage(lineage, tagged, &staged).await.unwrap());
    staged
}
async fn plant_orphan(replica: &EncryptedReplica<TypedRows>) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    for sql in ORPHAN_TAG {
        inner.conn.execute(sql, ()).await.unwrap();
    }
}
async fn withdraw_stage(
    lineage: &Lineage,
    key: &PrivateKey,
    replica: &EncryptedReplica<TypedRows>,
    publication: Commitment,
) -> Result<(), Error> {
    replica
        .withdraw_staged_schema(
            publication,
            lineage.app(2),
            crate::SchemaConfig {
                security: lineage.config(2, key),
                tables: Some(lineage.projection(2)),
            },
        )
        .await
}
async fn stage_state(
    replica: &EncryptedReplica<TypedRows>,
) -> Option<(Commitment, crate::StageState)> {
    replica
        .staged_schema()
        .await
        .unwrap()
        .map(|(stage, state)| (stage.publication, state))
}
const MID_SWITCH: &str = "SELECT (SELECT count(*) FROM secure_schema_switch),(SELECT count(*) FROM secure_projection_plan)";
fn mid_switch(intents: i64, plans: i64) -> Vec<Vec<SqlValue>> {
    vec![vec![SqlValue::Integer(intents), SqlValue::Integer(plans)]]
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_stage_whose_withdrawal_rebuild_is_refused_stays_installed_with_its_stage() {
    let mut lineage = linked();
    lineage.deferred_from = Some(3);
    let tagged = linked_with_a_tag(&lineage).await;
    let alice = &tagged.replica;
    let StagedSwitch {
        source,
        target,
        publication,
    } = stage_and_switch(&lineage, &tagged).await;
    {
        let inner = alice.inner.lock(crate::replica::Class::Background).await;
        for sql in ORPHAN_TAG {
            inner.conn.execute(sql, ()).await.unwrap();
        }
    }
    let refused = withdraw_stage(&lineage, &tagged.key, alice, publication).await;
    assert!(refused_for_an_orphan(&refused), "{refused:?}");
    assert_eq!(
        raw_rows(alice, MID_SWITCH).await,
        mid_switch(0, 0),
        "a refused withdrawal leaves no switch half done"
    );
    let refusal = crate::StageState::WithdrawalRefused(
        "the projection rebuild would keep rows that break a foreign key: a row of tags references a missing row of items".into(),
    );
    assert_eq!(alice.followed_tip().await, target);
    assert_eq!(
        stage_state(alice).await,
        Some((publication, refusal.clone())),
        "the stage records why its withdrawal was refused"
    );
    drop(tagged.replica);
    let alice = lineage
        .reopen(&tagged.path, &tagged.key, 2, 3)
        .await
        .expect("a peer whose withdrawal was refused reopens on the staged schema");
    assert_eq!(alice.followed_tip().await, target);
    assert_eq!(stage_state(&alice).await, Some((publication, refusal)));
    {
        let inner = alice.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute("UPDATE tags SET also=NULL", ())
            .await
            .unwrap();
    }
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items(key,title) VALUES('0002','after refusal')",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(
        select(&alice, "SELECT title FROM items ORDER BY key").await,
        vec![vec![text("title")], vec![text("after refusal")]],
        "the peer keeps working on the staged schema"
    );
    withdraw_stage(&lineage, &tagged.key, &alice, publication)
        .await
        .expect("the withdrawal runs again once no row breaks the restored reference");
    assert_eq!(alice.followed_tip().await, source);
    assert_eq!(stage_state(&alice).await, None);
    assert_eq!(raw_rows(&alice, MID_SWITCH).await, mid_switch(0, 0));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn no_withdrawal_holds_a_write_made_on_the_staged_schema_refused_or_not() {
    let mut lineage = linked();
    lineage.deferred_from = Some(3);
    let tagged = linked_with_a_tag(&lineage).await;
    let StagedSwitch {
        source,
        target,
        publication,
    } = stage_and_switch(&lineage, &tagged).await;
    write(&tagged.replica, "UPDATE items SET rank=7").await;
    plant_orphan(&tagged.replica).await;
    let refused = withdraw_stage(&lineage, &tagged.key, &tagged.replica, publication).await;
    assert!(refused_for_an_orphan(&refused), "{refused:?}");
    assert!(
        tagged.replica.held_changes().await.unwrap().is_empty(),
        "a refused withdrawal holds none of the writes the staged schema keeps"
    );
    drop(tagged.replica);
    let alice = lineage
        .reopen(&tagged.path, &tagged.key, 2, 3)
        .await
        .unwrap();
    assert_eq!(alice.followed_tip().await, target);
    assert_eq!(
        select(&alice, "SELECT rank FROM items").await,
        vec![vec![SqlValue::Integer(7)]],
        "the write stays live on the staged schema across a reopen"
    );
    let again = withdraw_stage(&lineage, &tagged.key, &alice, publication).await;
    assert!(refused_for_an_orphan(&again), "{again:?}");
    assert!(alice.held_changes().await.unwrap().is_empty());
    {
        let inner = alice.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute("UPDATE tags SET also=NULL", ())
            .await
            .unwrap();
    }
    withdraw_stage(&lineage, &tagged.key, &alice, publication)
        .await
        .expect("the withdrawal runs once no row breaks the restored reference");
    assert_eq!(alice.followed_tip().await, source);
    assert!(
        alice.held_changes().await.unwrap().is_empty(),
        "a write made on the staged schema is not on its source's path, so no withdrawal holds it"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_withdrawal_interrupted_between_its_transactions_is_abandoned_at_the_next_open() {
    let mut lineage = linked();
    lineage.deferred_from = Some(3);
    let tagged = linked_with_a_tag(&lineage).await;
    let StagedSwitch {
        target,
        publication,
        ..
    } = stage_and_switch(&lineage, &tagged).await;
    tagged.replica.fail_next_plan_switch().await;
    let interrupted = withdraw_stage(&lineage, &tagged.key, &tagged.replica, publication).await;
    assert!(
        matches!(interrupted, Err(Error::InjectedFailure)),
        "{interrupted:?}"
    );
    assert_eq!(
        raw_rows(&tagged.replica, MID_SWITCH).await,
        mid_switch(1, 1),
        "the interruption leaves the withdrawal's intent and plan behind"
    );
    drop(tagged.replica);
    let alice = lineage
        .reopen(&tagged.path, &tagged.key, 2, 3)
        .await
        .unwrap();
    assert_eq!(
        raw_rows(&alice, MID_SWITCH).await,
        mid_switch(0, 0),
        "the next open abandons the interrupted withdrawal"
    );
    assert_eq!(alice.followed_tip().await, target);
    assert_eq!(
        stage_state(&alice).await,
        Some((publication, crate::StageState::Staged)),
        "an interrupted withdrawal is no refusal: the migration may run again"
    );
    let migration = alice
        .session([lineage.scope("items")].into())
        .await
        .unwrap()
        .migration_sql(publication, crate::sql::SqlOptions::default())
        .await
        .unwrap();
    migration.run("BEGIN", vec![]).await.unwrap();
    migration.commit_publication(publication).await.unwrap();
    drop(migration);
    alice.author_staged_publication(publication).await.unwrap();
    assert_eq!(stage_state(&alice).await, None, "the stage was released");
    let other = rocky_credentials::SignedBookmarkMove::sign(
        rocky_credentials::BookmarkMoveStatement {
            format: rocky_credentials::BOOKMARK_MOVE_FORMAT,
            scope: lineage.domain,
            name: "other".into(),
            from: None,
            to: Some(target),
        },
        lineage.publisher(),
    )
    .unwrap();
    alice
        .publish_tip_statements(&[crate::TipStatement::Bookmark(other)])
        .await
        .unwrap();
    alice
        .follow_bookmark(lineage.domain, "other")
        .await
        .expect("no stale withdrawal blocks following another bookmark");
    assert_eq!(alice.followed_tip().await, target);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_rebuild_that_cannot_set_a_table_aside_rewrites_trigger_bodies_on_rename_again() {
    let tagged = linked_with_a_tag(&linked()).await;
    let refused = force_rebuild(
        &tagged.replica,
        &[
            "CREATE TABLE secure_rebuild_items(x)",
            "CREATE TABLE secure_rebuild_tags(x)",
        ],
    )
    .await;
    assert!(
        matches!(&refused, Err(Error::Storage(_)))
            && format!("{refused:?}")
                .contains("another table or index with this name: secure_rebuild_items"),
        "the rebuild fails renaming items aside: {refused:?}"
    );
    assert_eq!(
        raw_rows(&tagged.replica, "PRAGMA legacy_alter_table").await,
        vec![vec![SqlValue::Integer(0)]]
    );
}

fn injected_incompatible(result: &Result<impl std::fmt::Debug, Error>) -> bool {
    matches!(result, Err(Error::Incompatible(reason)) if reason == "an injected incompatible switch")
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_rebuild_failing_for_another_incompatibility_keeps_its_plan() {
    let lineage = linked();
    let tagged = linked_with_a_tag(&lineage).await;
    plan_rebuild(&tagged.replica, &[]).await;
    let plan = tagged.replica.projection_plan().await.unwrap();
    assert!(plan.is_some());
    tagged.replica.refuse_next_switch_as_incompatible().await;
    let failed = tagged.replica.run_plan().await;
    assert!(injected_incompatible(&failed), "{failed:?}");
    assert_eq!(
        tagged.replica.projection_plan().await.unwrap(),
        plan,
        "only rows that break a foreign key discard the plan"
    );
    tagged.replica.run_plan().await.unwrap();
    assert_eq!(tagged.replica.projection_plan().await.unwrap(), None);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_schema_switch_failing_for_another_incompatibility_stays_staged() {
    let mut lineage = linked();
    lineage.deferred_from = Some(3);
    let tagged = linked_with_a_tag(&lineage).await;
    let alice = &tagged.replica;
    super::super::compaction::follow_main(&lineage, alice).await;
    let statements = lineage.publish(alice, 3).await.unwrap();
    alice.refuse_next_switch_as_incompatible().await;
    let failed = lineage
        .follow(alice, lineage.config(3, &tagged.key), 3, &statements)
        .await;
    assert!(injected_incompatible(&failed), "{failed:?}");
    assert_eq!(
        raw_rows(alice, "SELECT count(*) FROM secure_schema_switch").await,
        vec![vec![SqlValue::Integer(1)]],
        "only rows that break a foreign key withdraw the switch"
    );
    let retried = lineage
        .follow(alice, lineage.config(3, &tagged.key), 3, &statements)
        .await;
    assert!(matches!(retried, Ok(true)), "{retried:?}");
    assert_eq!(declared_references(alice).await, (Vec::new(), true));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_refused_schema_switch_discards_its_plan_only_with_its_intent() {
    let mut lineage = linked();
    lineage.deferred_from = Some(3);
    let tagged = linked_with_a_tag(&lineage).await;
    let alice = &tagged.replica;
    super::super::compaction::follow_main(&lineage, alice).await;
    {
        let inner = alice.inner.lock(crate::replica::Class::Background).await;
        for sql in ORPHAN_TAG.iter().chain(&[
            "CREATE TEMP TRIGGER fail_withdrawal AFTER DELETE ON secure_schema_switch WHEN EXISTS(SELECT 1 FROM secure_projection_plan) BEGIN SELECT RAISE(ABORT,'withdraw rollback'); END",
        ]) {
            inner.conn.execute(*sql, ()).await.unwrap();
        }
    }
    let statements = lineage.publish(alice, 3).await.unwrap();
    let failed = lineage
        .follow(alice, lineage.config(3, &tagged.key), 3, &statements)
        .await;
    assert!(
        format!("{failed:?}").contains("withdraw rollback"),
        "{failed:?}"
    );
    assert_eq!(
        raw_rows(
            alice,
            "SELECT (SELECT count(*) FROM secure_schema_switch),(SELECT count(*) FROM secure_projection_plan)"
        )
        .await,
        vec![vec![SqlValue::Integer(1), SqlValue::Integer(1)]],
        "a withdrawal that fails keeps the switch intent together with its plan"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_stored_descriptor_the_current_rules_refuse_is_incompatible_rather_than_corrupt() {
    let lineage = linked();
    let tagged = linked_with_a_tag(&lineage).await;
    {
        let inner = tagged
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        let stored = rows(
            &inner.conn,
            "SELECT descriptor FROM secure_typed WHERE id=1",
        )
        .await
        .unwrap();
        let [SqlValue::Blob(descriptor)] = stored[0].as_slice() else {
            panic!("{stored:?}");
        };
        let mut schema: LogicalSchema = serde_json::from_slice(descriptor).unwrap();
        let tags = schema
            .tables
            .iter_mut()
            .find(|table| table.sql_name == "tags")
            .unwrap();
        tags.columns
            .iter_mut()
            .find(|column| column.sql_name == "item")
            .unwrap()
            .class = rocky_core::ColumnClass::LocalOnly;
        inner
            .conn
            .execute(
                "UPDATE secure_typed SET descriptor=?",
                [serde_json::to_vec(&schema).unwrap()],
            )
            .await
            .unwrap();
    }
    drop(tagged.replica);
    let refused = lineage.open(&tagged.path, &tagged.key, 2, None).await;
    assert!(
        matches!(&refused, Err(Error::Incompatible(reason)) if reason == "the stored typed table descriptor no longer validates: invalid logical schema: tags.item is local-only and cannot hold a foreign key"),
        "{:?}",
        refused.err()
    );
}

async fn with_a_draft(
    lineage: &Lineage,
    dir: &Scratch,
    key: &PrivateKey,
) -> EncryptedReplica<TypedRows> {
    let alice = lineage
        .open(&dir.path().join("alice.db"), key, 3, None)
        .await
        .unwrap();
    super::super::compaction::follow_main(lineage, &alice).await;
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items(key,title,note) VALUES('0001','title','noted')",
        vec![],
    )
    .await
    .unwrap();
    write(&alice, "UPDATE items SET draft='mine' WHERE key='0001'").await;
    alice
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_switch_that_removes_a_field_keeps_local_values_of_the_rows_it_rebuilds() {
    let mut lineage = Lineage::new();
    lineage.local_draft = true;
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let alice = with_a_draft(&lineage, &dir, &key).await;
    let drafts = "SELECT key,draft FROM items";
    let kept = vec![vec![text("0001"), text("mine")]];
    assert_eq!(select(&alice, drafts).await, kept);
    assert!(
        lineage
            .advance(&alice, lineage.config(4, &key), 4)
            .await
            .unwrap()
    );
    assert_eq!(
        select(&alice, drafts).await,
        kept,
        "a local value is no field of the switch path and stays on its row"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_switch_that_removes_a_field_keeps_local_values_in_the_undo_of_unsealed_writes() {
    let mut lineage = Lineage::new();
    lineage.local_draft = true;
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let alice = with_a_draft(&lineage, &dir, &key).await;
    let writer = session(&alice).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    writer
        .run(
            "UPDATE items SET title='retitled',draft='new' WHERE key='0001'",
            vec![],
        )
        .await
        .unwrap();
    identity_insert(&writer, "INSERT INTO tags VALUES('t','label')", vec![])
        .await
        .unwrap();
    writer.run("COMMIT", vec![]).await.unwrap();
    drop(writer);
    assert!(
        lineage
            .advance(&alice, lineage.config(4, &key), 4)
            .await
            .unwrap()
    );
    assert_eq!(
        select(&alice, "SELECT key,title,draft FROM items").await,
        vec![vec![text("0001"), text("title"), text("mine")]],
        "a write the switch drops is rewound through its undo, which restores the local value"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_replicated_field_back_as_a_local_one_starts_empty_on_any_path() {
    let mut lineage = Lineage::new();
    lineage.local_note_from = Some(5);
    let dir = Scratch::new().await;
    let (alice_key, bob_key) = (PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open(&dir.path().join("alice.db"), &alice_key, 3, None)
        .await
        .unwrap();
    let bob = lineage
        .open(&dir.path().join("bob.db"), &bob_key, 3, None)
        .await
        .unwrap();
    super::super::compaction::follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items(key,title,note) VALUES('0001','title','replicated')",
        vec![],
    )
    .await
    .unwrap();
    sync(&lineage, &alice, &bob, &bob_key).await;
    let notes = "SELECT key,note FROM items";
    assert_eq!(
        select(&bob, notes).await,
        vec![vec![text("0001"), text("replicated")]]
    );
    for version in [4, 5] {
        let statements = lineage.publish(&alice, version).await.unwrap();
        assert!(
            lineage
                .follow(
                    &alice,
                    lineage.config(version, &alice_key),
                    version,
                    &statements
                )
                .await
                .unwrap()
        );
    }
    let statements = crate::tests::all_tip_statements(&alice).await;
    assert!(
        lineage
            .follow(&bob, lineage.config(5, &bob_key), 5, &statements)
            .await
            .unwrap()
    );
    let empty = vec![vec![text("0001"), SqlValue::Null]];
    assert_eq!(select(&alice, notes).await, empty);
    assert_eq!(
        select(&bob, notes).await,
        empty,
        "a local column only keeps values it held as a local column"
    );
}
