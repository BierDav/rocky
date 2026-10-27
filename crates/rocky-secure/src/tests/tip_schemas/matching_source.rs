//! Matching TypedRows must bind every held image to its signed business key.
use super::*;
use rocky_core::{RowApplication, RowWrite, SqlValue};

#[tokio::test]
async fn a_matching_source_refuses_a_later_update_with_another_business_key() {
    let lineage = Lineage::pinned(210);
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[211; 32]);
    let source = lineage
        .open(&dir.path().join("source.db"), &node, 3, None)
        .await
        .unwrap();
    let sql = session(&source).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    for key in ["first", "later"] {
        super::super::generations::identity_insert(
            &sql,
            "INSERT INTO items(key,title,note) VALUES(?,'before','source note')",
            vec![SqlValue::Text(key.into())],
        )
        .await
        .unwrap();
    }
    sql.commit([149; 32]).await.unwrap();
    sql.run("BEGIN", vec![]).await.unwrap();
    sql.run("UPDATE items SET title='after'", vec![])
        .await
        .unwrap();
    let originals = sql
        .commit([150; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert_eq!(originals.len(), 1);
    let original = &originals[0];
    let payload = super::staging::payload(&lineage, 3, &node, original);
    assert_eq!(payload.writes().len(), 2);
    let app = lineage.app(3);
    for write in payload.writes() {
        assert!(write.change.before.is_some() && write.change.after.is_some());
        assert!(payload.proof(&write.change.row).is_none());
        app.validate_changes(std::slice::from_ref(&write.change))
            .unwrap();
        assert!(app.row_identity(&write.change).unwrap().is_some());
        assert_eq!(app.row_partition(&write.change).unwrap(), None);
    }
    let mut writes = payload.writes().to_vec();
    let table = app
        .tables()
        .find(|table| table.sql_name == "items")
        .unwrap();
    let mut other = app
        .values(table, writes[1].change.after.as_ref().unwrap())
        .unwrap();
    other[0] = SqlValue::Text("another business key".into());
    writes[1].change.row = app.row_id(lineage.domain.domain, table, &other).unwrap();
    assert_ne!(writes[1].change.row, payload.writes()[1].change.row);
    assert_eq!(writes[0], payload.writes()[0]);
    let bad = super::source_validation::reseal(
        original,
        &node,
        &[40; 32],
        original.chain().unwrap().clone(),
        writes.clone(),
        &payload,
    );
    bad.operation_ref().unwrap();
    assert_eq!(
        super::staging::payload(&lineage, 3, &node, &bad).writes(),
        writes
    );
    let mut restored: Vec<RowWrite> = writes;
    restored[1].change.row = payload.writes()[1].change.row;
    assert_eq!(restored, payload.writes());
    for (name, receiver_key, frame, invalid) in [
        ("bad", 212, bad, true),
        ("good", 213, original.clone(), false),
    ] {
        let receiver = lineage
            .open(
                &dir.path().join(name),
                &PrivateKey::from_bytes(&[receiver_key; 32]),
                3,
                None,
            )
            .await
            .unwrap();
        // Empty F excludes the later source update; the successor keeps exactly source3.
        let statements = super::source_validation::closed_statements(
            lineage.domain,
            receiver.followed_tip().await,
            app.manifest().hash(),
            lineage.publisher(),
        );
        receiver.receive_tip_statements(&statements).await.unwrap();
        receiver
            .follow_bookmark(lineage.domain, "main")
            .await
            .unwrap();
        let source_tip = frame.operation_ref().unwrap().header.extensions.tip;
        assert_eq!(
            receiver
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .app
                .manifest()
                .hash(),
            app.manifest().hash()
        );
        assert_eq!(
            receiver
                .tip_graph(lineage.domain)
                .await
                .unwrap()
                .schema_any(source_tip)
                .unwrap(),
            app.manifest().hash()
        );
        let result = receiver.receive_held_frames(vec![frame.clone()]).await;
        if invalid {
            let error = result.unwrap_err();
            assert!(matches!(error, Error::Rejected(_)), "{error}");
            assert!(
                error
                    .to_string()
                    .contains("items row identity differs from its key"),
                "{error}"
            );
            // Custody intake already committed; repeated apply must not add successful evidence.
            let before = super::super::compaction::audit::snapshot(&receiver).await;
            assert!(
                receiver
                    .settle()
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("items row identity differs from its key")
            );
            assert_eq!(
                super::super::compaction::audit::snapshot(&receiver).await,
                before
            );
            let inner = receiver.inner.lock(crate::replica::Class::Background).await;
            for table in [
                "secure_held",
                "secure_held_frames",
                "secure_held_resolved",
                "secure_voids",
                "secure_applied",
                "secure_admission_claims",
                "secure_admission_entries",
                "secure_identity_marks",
                "secure_identity_own",
                "secure_identity_summary",
                "secure_identity_ledger",
                "items",
            ] {
                assert!(
                    crate::storage::rows(&inner.conn, &format!("SELECT * FROM {table}"))
                        .await
                        .unwrap()
                        .is_empty(),
                    "{table}"
                );
            }
        } else {
            result.unwrap();
            let held = receiver.held_changes().await.unwrap();
            assert_eq!(held.len(), 1);
            assert_eq!(held[0].rows.len(), 2);
            let credentials = lineage.credentials(3, &node);
            let (stored, next) = receiver
                .held_frame_page(
                    node.public_key(),
                    &credentials,
                    lineage.scope("items"),
                    None,
                    MAX_SECURE_FRAME,
                )
                .await
                .unwrap();
            assert!(next.is_none());
            assert_eq!(stored.len(), 1);
            assert_eq!(stored[0].header_bytes(), frame.header_bytes());
            assert_eq!(stored[0].body_bytes(), frame.body_bytes());
            receiver.audit(lineage.scope("items")).await.unwrap();
        }
    }
}
