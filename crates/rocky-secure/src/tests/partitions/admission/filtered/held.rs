//! A removed table does not erase authenticated source partition semantics.
use super::*;

#[tokio::test]
async fn a_signed_header_for_a_refuses_a_complete_source_image_for_b() {
    let dir = TempDir::new().unwrap();
    let f = Fixture::build_with(true, PrivateKey::from_bytes(&[180; 32]));
    let node = PrivateKey::from_bytes(&[181; 32]);
    let writer = f
        .open(
            &dir.path().join("writer"),
            node.clone(),
            f.grant(Some(&["A"]), &all()),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    f.key(&writer, "A", [1; 32]).await;
    let valid = writer
        .author(
            [146; 32],
            vec![f.titled("A", 1, 1, "source").change],
            vec![],
        )
        .await
        .unwrap();
    let operation = valid.operation_ref().unwrap();
    let plaintext = crate::wire::decrypt(
        &[1; 32],
        &encode(&operation.header.extensions).unwrap(),
        valid.body_bytes(),
    )
    .unwrap();
    let payload = crate::wire::RowPayload::decode(&plaintext).unwrap();
    let mut writes = payload.writes().to_vec();
    writes[0].change.after = f.titled("B", 1, 1, "source").change.after;
    let bad = crate::tests::tip_schemas::source_validation::reseal(
        &valid,
        &node,
        &[1; 32],
        valid.chain().unwrap().clone(),
        writes,
        &payload,
    );
    bad.operation_ref().unwrap();
    let source = writer.followed_tip().await;
    let absent = removed(&f);
    let close = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: f.domain,
            tip: source,
            frontier: vec![],
            action: SealAction::Close,
            recovery: None,
        },
        &f.publisher,
    )
    .unwrap();
    let opening = SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            scope: f.domain,
            seal: close.id().unwrap(),
            schema: TypedRows::new(absent.clone(), vec![f.domain.domain])
                .unwrap()
                .manifest()
                .hash(),
            minimum_in_sync: 1,
        },
        &f.publisher,
    )
    .unwrap();
    let target = opening.tip().unwrap();
    let statements = [
        bookmark(&f, "main", None, source),
        TipStatement::Seal(close),
        TipStatement::Opening(opening),
        bookmark(&f, "main", Some(source), target),
    ];
    for (index, frame) in [bad, valid].into_iter().enumerate() {
        let reader = keyed(
            &f,
            &dir.path().join(format!("reader{index}")),
            [182 + index as u8; 32],
            2 + index as u32,
        )
        .await;
        reader.receive_tip_statements(&statements).await.unwrap();
        switch(&f, &reader, "main", &absent).await;
        if index == 0 {
            let error = reader.receive_held_frames(vec![frame]).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("row image differs from its partition scope"),
                "{error}"
            );
            let before = crate::tests::compaction::audit::snapshot(&reader).await;
            let error = reader.settle().await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("row image differs from its partition scope"),
                "{error}"
            );
            assert_eq!(
                crate::tests::compaction::audit::snapshot(&reader).await,
                before
            );
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
            ] {
                assert_eq!(
                    count(&reader, &format!("SELECT 1 FROM {table}")).await,
                    0,
                    "{table}"
                );
            }
        } else {
            reader
                .receive_held_frames(vec![frame.clone()])
                .await
                .unwrap();
            assert_eq!(reader.held_changes().await.unwrap().len(), 1);
            let (page, _) = reader
                .held_frame_page(
                    node.public_key(),
                    &writer.inner.lock().await.config.credentials.clone(),
                    f.scope("A"),
                    None,
                    MAX_SECURE_FRAME,
                )
                .await
                .unwrap();
            assert_eq!(page[0].to_bytes().unwrap(), frame.to_bytes().unwrap());
            reader.audit(f.scope("A")).await.unwrap();
        }
    }
}
