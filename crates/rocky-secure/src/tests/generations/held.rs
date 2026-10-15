//! Held changes: unsent bundles a removal would split are held, voided and
//! re-authored. See `docs/runtime/held-changes.md`.
use super::*;
use crate::replica::{HeldKind, SignedVoid};

fn encoded(value: SqlValue) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.encode(&mut bytes);
    bytes
}
/// Catches `target` up on `scope` from `source` by range pages, voids
/// first, as a session does.
async fn sync(
    source: &EncryptedReplica<TypedRows>,
    target: &EncryptedReplica<TypedRows>,
    target_key: &PrivateKey,
    lineage: &Lineage,
    generation: u32,
    scope: ResourceScope,
) {
    let credentials = lineage.credentials(generation, target_key);
    // As the target's session hello declares its generation.
    let manifest = target.inner.lock().await.manifest;
    source
        .admit_peer(
            target_key.public_key(),
            &credentials,
            rocky_credentials::genesis_tip(manifest),
        )
        .await
        .unwrap();
    let mut after = None;
    loop {
        let frontier = target.frontier(scope).await.unwrap();
        let page = source
            .range_page(
                target_key.public_key(),
                &credentials,
                &frontier,
                after,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        for status in target.receive_voids(&page.voids).await {
            status.unwrap();
        }
        for status in target.receive_all(page.frames).await {
            status.unwrap();
        }
        after = page.next;
        if after.is_none() {
            return;
        }
    }
}
async fn title(replica: &EncryptedReplica<TypedRows>) -> Vec<Vec<SqlValue>> {
    select(replica, "SELECT title FROM items WHERE key='0001'").await
}

#[tokio::test]
async fn an_unsent_bundle_a_removal_splits_is_held_voided_and_reauthored() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let (alice_key, carol_key) = (PrivateKey::new(), PrivateKey::new());
    let (alice_path, carol_path) = (dir.path().join("alice.db"), dir.path().join("carol.db"));
    let alice = lineage
        .open(&alice_path, &alice_key, 3, Some(lineage.upgrade(1, 3)))
        .await
        .unwrap();
    let carol = lineage
        .open(&carol_path, &carol_key, 3, Some(lineage.upgrade(1, 3)))
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
    sync(&alice, &carol, &carol_key, &lineage, 3, items).await;
    // An offered write of the removed field stays sent, field by field.
    write(&carol, "UPDATE items SET note='sent' WHERE key='0001'").await;
    sync(&carol, &alice, &alice_key, &lineage, 3, items).await;
    let signer = carol_key.clone();
    let offered = frames(&carol, &alice, items)
        .await
        .into_iter()
        .find(|frame| frame.operation().unwrap().header.public_key == signer.public_key())
        .unwrap();
    // Never offered: one bundle over a kept and a removed table.
    let bundle = session(&carol).await;
    bundle.run("BEGIN", vec![]).await.unwrap();
    bundle
        .run("UPDATE items SET title='mine' WHERE key='0001'", vec![])
        .await
        .unwrap();
    identity_insert(&bundle, "INSERT INTO tags VALUES('t','label')", vec![])
        .await
        .unwrap();
    bundle
        .commit(rocky_core::commitment("held.bundle", b"carol"))
        .await
        .unwrap();
    drop(bundle);
    assert_eq!(title(&carol).await, vec![vec![text("mine")]]);

    for (replica, key) in [(&carol, &carol_key), (&alice, &alice_key)] {
        assert!(
            replica
                .adopt_generation(
                    lineage.app(4),
                    lineage.config(4, key),
                    lineage.upgrade(1, 4)
                )
                .await
                .unwrap()
        );
    }
    // Held whole: its kept effect is rolled back at once.
    assert_eq!(title(&carol).await, vec![vec![text("one")]]);
    let held = carol.held_changes().await.unwrap();
    assert_eq!(held.len(), 1, "{held:?}");
    let change = &held[0];
    let (kept, removed): (Vec<_>, Vec<_>) = change.rows.iter().partition(|row| !row.removed);
    assert_eq!((kept.len(), removed.len()), (1, 1));
    assert_eq!(
        (kept[0].table.as_str(), kept[0].kind),
        ("items", HeldKind::Update)
    );
    assert_eq!(
        (removed[0].table.as_str(), removed[0].kind),
        ("tags", HeldKind::Insert)
    );
    let written = |row: &crate::replica::HeldRow, name: &str| {
        row.fields
            .iter()
            .find(|field| field.name == name && field.written)
            .cloned()
            .unwrap()
    };
    let field = written(kept[0], "title");
    assert_eq!(field.value, encoded(text("mine")));
    let current = field.current.unwrap();
    assert_eq!(current.value, Some(encoded(text("one"))));
    assert!(written(removed[0], "label").removed);

    // Its frames are voided: carol's later edit applies on alice.
    write(&carol, "UPDATE items SET rank=7 WHERE key='0001'").await;
    sync(&carol, &alice, &alice_key, &lineage, 4, items).await;
    assert_eq!(
        select(&alice, "SELECT title,rank FROM items WHERE key='0001'").await,
        vec![vec![text("one"), SqlValue::Integer(7)]]
    );
    let (theirs, ours) = (
        alice.frontier(items).await.unwrap(),
        carol.frontier(items).await.unwrap(),
    );
    assert_eq!(theirs.lag_behind(&ours), 0);
    alice.audit(items).await.unwrap();
    carol.audit(items).await.unwrap();

    // Alice writes meanwhile; the held change lists her value.
    write(&alice, "UPDATE items SET title='alice' WHERE key='0001'").await;
    sync(&alice, &carol, &carol_key, &lineage, 4, items).await;
    let held = carol.held_changes().await.unwrap();
    let row = held[0].rows.iter().find(|row| !row.removed).unwrap();
    let current = written(row, "title").current.unwrap();
    assert_eq!(current.value, Some(encoded(text("alice"))));
    assert!(current.time > change.time);
    // A missing held id rolls the new writes back with the resolution.
    let failed = session(&carol).await;
    failed.run("BEGIN", vec![]).await.unwrap();
    failed
        .run("UPDATE items SET title='invalid' WHERE key='0001'", vec![])
        .await
        .unwrap();
    assert!(failed.commit_reauthoring([91; 32], [92; 32]).await.is_err());
    drop(failed);
    assert_eq!(title(&carol).await, vec![vec![text("alice")]]);
    assert_eq!(carol.held_changes().await.unwrap().len(), 1);
    // Re-authoring is a new write that wins on every device.
    let reauthor = session(&carol).await;
    reauthor.run("BEGIN", vec![]).await.unwrap();
    reauthor
        .run("UPDATE items SET title='mine' WHERE key='0001'", vec![])
        .await
        .unwrap();
    reauthor
        .commit_reauthoring(rocky_core::commitment("held.reauthor", b"carol"), change.id)
        .await
        .unwrap();
    drop(reauthor);
    assert!(carol.held_changes().await.unwrap().is_empty());
    // The handle also binds which change was resolved.
    write(&carol, "UPDATE items SET title='alice' WHERE key='0001'").await;
    let retry = session(&carol).await;
    retry.run("BEGIN", vec![]).await.unwrap();
    retry
        .run("UPDATE items SET title='mine' WHERE key='0001'", vec![])
        .await
        .unwrap();
    assert!(
        retry
            .commit_reauthoring(rocky_core::commitment("held.reauthor", b"carol"), [93; 32])
            .await
            .is_err()
    );
    drop(retry);
    assert_eq!(title(&carol).await, vec![vec![text("alice")]]);
    write(&carol, "UPDATE items SET title='mine' WHERE key='0001'").await;
    sync(&carol, &alice, &alice_key, &lineage, 4, items).await;
    for replica in [&alice, &carol] {
        assert_eq!(title(replica).await, vec![vec![text("mine")]]);
    }
    assert!(carol.drop_held(change.id).await.is_err());

    // A void of a frame a peer holds is refused there.
    let void = SignedVoid::sign(&signer, vec![offered.header.clone()]).unwrap();
    let statuses = alice.receive_voids(&[void]).await;
    assert!(
        matches!(statuses[..], [Ok(ReceiveStatus::Rejected)]),
        "{statuses:?}"
    );
    assert_eq!(title(&alice).await, vec![vec![text("mine")]]);

    // The held state survives a restart.
    drop(carol);
    let carol = lineage
        .open(&carol_path, &carol_key, 4, Some(lineage.upgrade(1, 4)))
        .await
        .unwrap();
    carol.audit(items).await.unwrap();
    assert_eq!(title(&carol).await, vec![vec![text("mine")]]);
}

#[tokio::test]
async fn a_dropped_held_change_leaves_the_list_and_writes_nothing() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let carol = lineage
        .open(
            &dir.path().join("carol.db"),
            &key,
            3,
            Some(lineage.upgrade(1, 3)),
        )
        .await
        .unwrap();
    identity_insert(
        &session(&carol).await,
        "INSERT INTO items VALUES('0001','one','draft',NULL)",
        vec![],
    )
    .await
    .unwrap();
    // A default value of the removed field holds nothing.
    identity_insert(
        &session(&carol).await,
        "INSERT INTO items VALUES('0002','two',NULL,NULL)",
        vec![],
    )
    .await
    .unwrap();
    EncryptedReplica::<TypedRows>::fail_next_activation();
    let failed = carol
        .adopt_generation(
            lineage.app(4),
            lineage.config(4, &key),
            lineage.upgrade(1, 4),
        )
        .await;
    assert!(matches!(failed, Err(Error::InjectedFailure)));
    assert!(carol.held_changes().await.unwrap().is_empty());
    assert_eq!(
        select(&carol, "SELECT key FROM items ORDER BY key").await,
        vec![vec![text("0001")], vec![text("0002")]]
    );
    carol
        .adopt_generation(
            lineage.app(4),
            lineage.config(4, &key),
            lineage.upgrade(1, 4),
        )
        .await
        .unwrap();
    assert_eq!(
        select(&carol, "SELECT key FROM items ORDER BY key").await,
        vec![vec![text("0002")]]
    );
    let held = carol.held_changes().await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].rows[0].kind, HeldKind::Insert);
    // Restart before resolution: the values and rollback are durable even
    // before the table author is registered to sign its voids.
    drop(carol);
    let carol = lineage
        .open(
            &dir.path().join("carol.db"),
            &key,
            4,
            Some(lineage.upgrade(1, 4)),
        )
        .await
        .unwrap();
    assert_eq!(carol.held_changes().await.unwrap(), held);
    carol.drop_held(held[0].id).await.unwrap();
    assert!(carol.held_changes().await.unwrap().is_empty());
    assert_eq!(
        select(&carol, "SELECT key FROM items ORDER BY key").await,
        vec![vec![text("0002")]]
    );
}

#[tokio::test]
async fn offering_one_bundle_member_without_delivery_prevents_holding_after_restart() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let peer = PrivateKey::new();
    let path = dir.path().join("author.db");
    let replica = lineage
        .open(&path, &key, 3, Some(lineage.upgrade(1, 3)))
        .await
        .unwrap();
    let transaction = session(&replica).await;
    transaction.run("BEGIN", vec![]).await.unwrap();
    identity_insert(
        &transaction,
        "INSERT INTO items VALUES('0001','mine',NULL,NULL)",
        vec![],
    )
    .await
    .unwrap();
    identity_insert(&transaction, "INSERT INTO tags VALUES('t','label')", vec![])
        .await
        .unwrap();
    transaction.commit([94; 32]).await.unwrap();
    drop(transaction);
    let scope = lineage.scope("tags");
    let page = replica
        .range_page(
            peer.public_key(),
            &lineage.credentials(3, &peer),
            &crate::replica::ScopeFrontier {
                scope,
                logs: vec![],
                cuts: 0,
            },
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert_eq!(page.frames.len(), 1);
    // The page is never delivered: exposure, rather than acknowledgement,
    // decides whether the whole bundle may be voided.
    drop(replica);
    let replica = lineage
        .open(&path, &key, 4, Some(lineage.upgrade(1, 4)))
        .await
        .unwrap();
    assert!(replica.held_changes().await.unwrap().is_empty());
    assert_eq!(title(&replica).await, vec![vec![text("mine")]]);
}
