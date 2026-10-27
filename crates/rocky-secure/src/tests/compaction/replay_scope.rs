use super::*;
use crate::wire::{Chain, Extensions, RowPayload};

#[tokio::test]
async fn replay_binds_the_header_and_every_original_write_before_filtering() {
    let lineage = Lineage::pinned(239);
    let directory = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[240; 32]);
    let replica = open(&lineage, &directory, "replay", &node).await;
    follow_main(&lineage, &replica).await;
    for name in ["one", "two"] {
        insert(&replica, name, name).await;
    }
    let sql = session(&replica).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    sql.run("UPDATE items SET title='updated'", vec![])
        .await
        .unwrap();
    sql.run("COMMIT", vec![]).await.unwrap();
    let scope = lineage.scope("items");
    let other = lineage.scope("tags");
    let source_config = lineage.config(3, &node);
    let frames = stored_frames(
        &replica,
        "SELECT header,body FROM secure_operations ORDER BY sequence DESC LIMIT 1",
    )
    .await;
    let source_operation = frames[0].operation_ref().unwrap();
    let payload = {
        let operation = frames[0].operation_ref().unwrap();
        let key = source_config
            .keys
            .iter()
            .find(|key| key.scope == scope)
            .unwrap();
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .decode_row_payload(
                &frames[0],
                &crate::wire::decrypt(
                    &key.key,
                    &encode(&operation.header.extensions).unwrap(),
                    frames[0].body_bytes(),
                )
                .unwrap(),
            )
            .unwrap()
    };
    let changes = payload
        .clone()
        .into_writes()
        .into_iter()
        .map(|write| {
            assert!(write.change.before.is_some());
            write.change
        })
        .collect::<Vec<_>>();
    assert_eq!(changes.len(), 2);
    let signer = PrivateKey::from_bytes(&[241; 32]);
    let config = lineage.config(3, &signer);
    assert_ne!(config.keys[0].key, config.keys[1].key);
    let chain = Chain::new(config.credentials.clone()).unwrap();
    let tip = replica.followed_tip().await;
    let make = |header_scope, changes: &[RowChange]| {
        let key = config
            .keys
            .iter()
            .find(|key| key.scope == header_scope)
            .unwrap();
        crate::wire::author(
            &signer,
            Extensions {
                tip,
                scope: header_scope,
                generation: key.generation,
                chain: chain.digest(),
                author: Some(config.credentials.author_number().unwrap()),
                author_sequence: 0,
                row_format: crate::wire::ROW_FORMAT,
                births: Vec::new(),
                partition: None,
                existence: 0,
            },
            chain.clone(),
            &key.key,
            (0, None, source_operation.header.timestamp),
            if header_scope == scope {
                source_operation
                    .header
                    .previous
                    .iter()
                    .copied()
                    .chain(source_operation.header.backlink)
                    .collect()
            } else {
                Vec::new()
            },
            &payload
                .compact_test(
                    &RowPayload::witness(RowPayload::rows(changes).unwrap(), Vec::new()).unwrap(),
                    header_scope,
                )
                .unwrap(),
        )
        .unwrap()
    };
    for changes in [Vec::new(), changes.clone()] {
        let frame = make(other, &changes);
        let error = replica
            .replay_injected_frame_for_test(scope, &frame, None)
            .await
            .unwrap_err();
        let expected = if changes.is_empty() {
            "replay scope mismatch"
        } else {
            "frame ordinal is outside its source table schema"
        };
        assert!(
            matches!(error, Error::Rejected(ref reason) if reason == expected),
            "{error}"
        );
    }
    for filtered in [false, true] {
        let mut wrong = changes.clone();
        wrong[1].scope = if filtered {
            ResourceScope {
                table: [242; 32],
                ..other
            }
        } else {
            other
        };
        let frame = make(scope, &wrong);
        let error = replica
            .replay_injected_frame_for_test(scope, &frame, filtered.then_some(1))
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::Rejected(ref reason) if reason == "frame ordinal is outside its source table schema"),
            "{error}"
        );
    }
    let valid = make(scope, &changes);
    assert_eq!(
        replica
            .replay_injected_frame_for_test(scope, &valid, None)
            .await
            .unwrap(),
        2
    );
    replica.audit(scope).await.unwrap();
}
