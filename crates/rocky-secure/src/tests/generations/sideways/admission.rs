//! A target removal defers creation; restoring the source path mints once.
use super::*;
use crate::storage::{bindings, rows};

#[tokio::test]
async fn an_absent_tables_filtered_birth_restores_with_its_original_receipt() {
    let lineage = Lineage::pinned(131);
    let dir = TempDir::new().unwrap();
    let identity = PrivateKey::from_bytes(&[132; 32]);
    let keys = [133, 134].map(|seed| PrivateKey::from_bytes(&[seed; 32]));
    let path = dir.path().join("alice.db");
    let mut alice = lineage
        .open_certified(&path, (&identity, &keys[0]), 3, Some(lineage.projection(3)))
        .await
        .unwrap();
    let dave = lineage
        .open(
            &dir.path().join("dave.db"),
            &keys[1],
            3,
            Some(lineage.projection(3)),
        )
        .await
        .unwrap();
    follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &dave, lineage.domain).await;
    identity_insert(
        &session(&dave).await,
        "INSERT INTO tags(key,label) VALUES('late','restored')",
        vec![],
    )
    .await
    .unwrap();
    let frames = super::super::switches::authored(&dave, &keys[1]).await;
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    let op = frame.operation().unwrap();
    let author = op.header.extensions.author.unwrap();
    assert_eq!(
        (op.header.seq_num, op.header.extensions.author_sequence),
        (0, 0)
    );
    let first = super::super::super::compaction::sign_seal(
        &lineage,
        alice.followed_tip().await,
        vec![rocky_credentials::AuthorMark {
            author,
            sequence: 0,
        }],
    );
    let first_id = first.id().unwrap();
    let next = first.successor().unwrap().unwrap();
    let from = alice.followed_tip().await;
    alice
        .publish_tip_statements(&[
            TipStatement::Seal(first),
            bookmark(&lineage, Some(from), next),
        ])
        .await
        .unwrap();
    alice
        .publish_tip_statements(&open_dev(&lineage, first_id, 4))
        .await
        .unwrap();
    alice.follow_bookmark(lineage.domain, "dev").await.unwrap();
    lineage.install_required(&alice).await;
    let dev = alice
        .tip_graph(lineage.domain)
        .await
        .unwrap()
        .bookmark("dev")
        .unwrap()
        .unwrap();
    let security = || {
        let mut config = lineage.config(4, &keys[0]);
        // Removing a local table need not discard already granted reader authority.
        config.credentials = lineage.certified(3, &identity, &keys[0]);
        config.manifest = lineage.manifest(3);
        config
            .keys
            .push(crate::KeyMaterial::new(lineage.scope("tags"), 1, [41; 32]));
        config
    };
    alice
        .switch_schema(
            dev,
            lineage.app(4),
            crate::SchemaConfig {
                security: security(),
                tables: Some(lineage.projection(4)),
            },
        )
        .await
        .unwrap();
    let scope = lineage.scope("tags");
    let binding_query = "SELECT allocation FROM secure_identity_partitions WHERE scope=?";
    assert!(
        crate::storage::collect_rows(
            alice
                .inner
                .lock()
                .await
                .conn
                .query(binding_query, [encode(&scope).unwrap()])
                .await
                .unwrap()
        )
        .await
        .unwrap()
        .is_empty()
    );
    alice.receive(frame.clone()).await.unwrap();
    let receipt = rows(
        &alice.inner.lock().await.conn,
        "SELECT * FROM secure_admission_entries WHERE kind=0",
    )
    .await
    .unwrap();
    assert_eq!(receipt.len(), 1);
    assert_eq!(
        scalar(&alice, "SELECT count(*) FROM secure_admission_claims").await,
        0
    );
    assert_eq!(
        scalar(&alice, "SELECT count(*) FROM secure_identity_partitions").await,
        0
    );
    assert_eq!(
        scalar(&alice, "SELECT count(*) FROM secure_identity_marks").await,
        0
    );
    drop(alice);
    alice = lineage.open_with(&path, security(), 4, None).await.unwrap();
    alice.audit(lineage.scope("items")).await.unwrap();
    follow(&lineage, &alice, (&identity, &keys[0]), "main", 3).await;
    assert_eq!(
        select(&alice, "SELECT key,label FROM tags").await,
        vec![vec![text("late"), text("restored")]]
    );
    assert_eq!(
        rows(
            &alice.inner.lock().await.conn,
            "SELECT * FROM secure_admission_entries WHERE kind=0"
        )
        .await
        .unwrap(),
        receipt
    );
    assert_eq!(
        scalar(
            &alice,
            "SELECT count(*) FROM secure_admission_entries WHERE kind=1"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &alice,
            "SELECT count(*) FROM secure_admission_claims WHERE kind=0"
        )
        .await,
        1
    );
    alice.rebuild_projection(scope).await.unwrap();
    alice.audit(scope).await.unwrap();
    // A duplicate appends nothing; the head only re-pins a later revision.
    let journal = "SELECT entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT allocation,identity,entry FROM secure_admission_claims";
    let before = rows(&alice.inner.lock().await.conn, journal).await.unwrap();
    alice.receive(frame.clone()).await.unwrap();
    assert_eq!(
        rows(&alice.inner.lock().await.conn, journal).await.unwrap(),
        before
    );
    let inner = alice.inner.lock().await;
    let binding = crate::storage::collect_rows(
        inner
            .conn
            .query(
                binding_query,
                bindings(vec![SqlValue::Blob(encode(&scope).unwrap())]),
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(binding.len(), 1);
    drop(inner);
    drop(alice);
    let alice = lineage.reopen(&path, &keys[0], 3, 3).await.unwrap();
    alice.audit(scope).await.unwrap();
    assert_eq!(
        select(&alice, "SELECT key,label FROM tags").await,
        vec![vec![text("late"), text("restored")]]
    );
}
