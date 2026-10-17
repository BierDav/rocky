//! Another author's carry cannot retire this author's earlier ordinary log.
use super::*;

async fn state(replica: &EncryptedReplica<TypedRows>) -> Vec<Vec<SqlValue>> {
    rows(&replica.inner.lock().await.conn, "SELECT 'P',scope,author,sequence FROM secure_identity_marks UNION ALL SELECT 'O',scope,author,sequence FROM secure_identity_own UNION ALL SELECT 'L',scope,author,sequence FROM secure_identity_ledger UNION ALL SELECT 'R',entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT 'C',identity,provenance,entry FROM secure_admission_claims UNION ALL SELECT 'H',context,head,head_signature FROM secure_admission_context UNION ALL SELECT 'V',revision,0,0 FROM secure_metadata UNION ALL SELECT 'F',hash,status,0 FROM secure_operations").await.unwrap()
}

pub(super) async fn mark(
    replica: &EncryptedReplica<TypedRows>,
    table: &str,
    scope: ResourceScope,
) -> Option<i64> {
    crate::storage::collect_rows(
        replica
            .inner
            .lock()
            .await
            .conn
            .query(
                format!("SELECT sequence FROM {table} WHERE scope=? AND author=1"),
                [encode(&scope).unwrap()],
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap()
    .first()
    .map(|row| match row[0] {
        SqlValue::Integer(sequence) => sequence,
        _ => panic!("identity sequence"),
    })
}

#[tokio::test]
async fn an_earlier_own_birth_and_a_foreign_carry_converge_in_both_orders() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::build_with(true, PrivateKey::from_bytes(&[214; 32]));
    let writer = keyed(&f, &root.path().join("writer"), [215; 32], 1).await;
    let mover = keyed(&f, &root.path().join("mover"), [216; 32], 2).await;
    let mut frames = Vec::new();
    for (sequence, partition) in [(1, "A"), (2, "B"), (3, "A"), (4, "B")] {
        frames.push(
            writer
                .author(
                    [sequence; 32],
                    vec![
                        f.titled(partition, sequence.into(), 1, &format!("row{sequence}"))
                            .change,
                    ],
                    vec![],
                )
                .await
                .unwrap(),
        );
    }
    for frame in [&frames[0], &frames[2]] {
        assert_eq!(
            mover.receive(frame.clone()).await.unwrap(),
            ReceiveStatus::Applied
        );
    }
    let carry = mover
        .session([f.scope("A"), f.scope("B")].into())
        .await
        .unwrap()
        .move_row(
            [5; 32],
            "items",
            RowIdentity::new(1, 3).unwrap(),
            &[SqlValue::Text("A".into())],
            &[SqlValue::Text("B".into())],
        )
        .await
        .unwrap();
    let mut final_rows = None;
    for (index, carry_first) in [true, false].into_iter().enumerate() {
        let path = root.path().join(format!("reader{index}"));
        let reader = keyed(&f, &path, [217 + index as u8; 32], 3 + index as u32).await;
        for frame in [&frames[0], &frames[2]] {
            reader.receive(frame.clone()).await.unwrap();
        }
        if !carry_first {
            assert_eq!(
                reader.receive(frames[1].clone()).await.unwrap(),
                ReceiveStatus::Applied
            );
        }
        for frame in &carry {
            reader.receive(frame.clone()).await.unwrap();
        }
        assert_eq!(
            mark(&reader, "secure_identity_marks", f.scope("B")).await,
            Some(3)
        );
        assert_eq!(
            mark(&reader, "secure_identity_own", f.scope("B")).await,
            if carry_first { None } else { Some(2) }
        );
        assert_eq!(
            mark(&reader, "secure_identity_ledger", f.scope("B")).await,
            if carry_first { None } else { Some(2) }
        );
        if carry_first {
            let before = state(&reader).await;
            reader
                .inject_group_fault(Some(crate::replica::group::GroupFault::FailBeforeCommit))
                .await;
            assert!(reader.receive(frames[1].clone()).await.is_err());
            reader.inject_group_fault(None).await;
            assert_eq!(state(&reader).await, before);
            assert_eq!(
                reader.receive(frames[1].clone()).await.unwrap(),
                ReceiveStatus::Applied
            );
        }
        assert_eq!(
            reader.receive(frames[3].clone()).await.unwrap(),
            ReceiveStatus::Applied
        );
        // Before birth4 P was3. Afterwards O<=P and both maxima are4.
        for table in [
            "secure_identity_marks",
            "secure_identity_own",
            "secure_identity_ledger",
        ] {
            assert_eq!(mark(&reader, table, f.scope("B")).await, Some(4), "{table}");
        }
        let current = items(&f, &reader).await;
        assert_eq!(current.len(), 4);
        if let Some(expected) = &final_rows {
            assert_eq!(&current, expected);
        } else {
            final_rows = Some(current);
        }
        assert_eq!(
            count(
                &reader,
                "SELECT 1 FROM secure_admission_claims WHERE kind=0"
            )
            .await,
            4
        );
        assert_eq!(
            count(
                &reader,
                "SELECT 1 FROM secure_admission_claims WHERE kind=1"
            )
            .await,
            1
        );
        reader.audit(f.scope("A")).await.unwrap();
        reader.audit(f.scope("B")).await.unwrap();
        let config = super::filtered::security(&f, &reader).await;
        drop(reader);
        let reader = EncryptedReplica::open(&path, f.app(), config)
            .await
            .unwrap();
        reader.audit(f.scope("B")).await.unwrap();
        assert_eq!(items(&f, &reader).await, final_rows.clone().unwrap());
    }
}
