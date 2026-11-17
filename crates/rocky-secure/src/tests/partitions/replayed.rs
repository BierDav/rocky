//! Replayed transactions across partitions: a replica holding only some
//! members follows the outcome records of replicas that hold the rest.
use super::*;
use rocky_core::replay::{DropReason, TransactionOutcome};

/// Native only: exhaustive delivery schedules run across threads.
#[cfg(not(target_family = "wasm"))]
mod convergence;

/// Deletes every outcome record `replica` published, as if they were lost.
async fn lose_outcome_records(replica: &EncryptedReplica<TypedRows>) {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    for table in ["secure_outcomes", "secure_outcome_members"] {
        inner
            .conn
            .execute(&format!("DELETE FROM {table}"), ())
            .await
            .unwrap();
    }
}

async fn keys(
    f: &Fixture,
    replica: &EncryptedReplica<TypedRows>,
    names: &[&str],
) -> Vec<Vec<SqlValue>> {
    sql_on(f, replica, names)
        .await
        .run(
            &format!(
                "SELECT list,key,amount FROM items WHERE list IN ({}) ORDER BY list,key",
                names
                    .iter()
                    .map(|name| format!("'{name}'"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            vec![],
        )
        .await
        .unwrap()
        .rows
}

/// Frames `replica` authored in `name`'s partition that carry no intent.
async fn records(
    f: &Fixture,
    replica: &EncryptedReplica<TypedRows>,
    node: &PrivateKey,
    name: &str,
) -> Vec<EncryptedFrame> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    let rows = crate::storage::rows(
        &inner.conn,
        &format!(
            "SELECT header,body FROM secure_operations WHERE author=x'{}' ORDER BY sequence",
            node.public_key()
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
    )
    .await
    .unwrap();
    let mut frames = Vec::new();
    for row in rows {
        let [SqlValue::Blob(header), SqlValue::Blob(body)] = row.as_slice() else {
            panic!("stored operation");
        };
        let frame = inner
            .stored_frame(header.clone(), body.clone())
            .await
            .unwrap();
        if frame.scope().unwrap() == f.scope(name)
            && payload(&frame, [if name == "A" { 1 } else { 2 }; 32], f)
                .outcome()
                .is_some()
        {
            frames.push(frame);
        }
    }
    frames
}

fn payload(frame: &EncryptedFrame, key: [u8; 32], f: &Fixture) -> crate::wire::RowPayload {
    let plaintext = crate::wire::Opened::open(
        &key,
        &frame.operation_ref().unwrap().header.extensions,
        frame.body_bytes(),
    )
    .unwrap()
    .payload;
    crate::wire::RowPayload::decode_test(&plaintext, frame, &f.app()).unwrap()
}

fn transaction(frame: &EncryptedFrame, key: [u8; 32], f: &Fixture) -> rocky_core::Commitment {
    payload(frame, key, f).membership().unwrap().transaction
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_partial_holder_drops_its_member_when_a_full_holders_record_says_so() {
    let root = rocky_platform::fs::Scratch::new().await;
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let open = |name: &'static str, node: &PrivateKey, held: Option<&'static [&'static str]>| {
        let (f, root, node) = (&f, &root, node.clone());
        async move {
            let replica = f
                .open(&root.path().join(name), node.clone(), f.grant(held, &all()))
                .await;
            for (list, key) in [("A", [1; 32]), ("B", [2; 32])] {
                if held.is_none_or(|held| held.contains(&list)) {
                    f.key(&replica, list, key).await;
                }
            }
            replica
        }
    };
    let (w, x, r) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let writer = open("w.sqlite", &w, None).await;
    let full = open("x.sqlite", &x, None).await;
    let partial = open("r.sqlite", &r, Some(&["A"])).await;

    let [x_creates_k_in_b] = insert(&f, &full, &["A", "B"], &[("B", "k", 1)], 1)
        .await
        .try_into()
        .unwrap();
    let writers_bundle_unaware_of_x =
        insert(&f, &writer, &["A", "B"], &[("A", "a", 1), ("B", "k", 2)], 2).await;
    assert_eq!(writers_bundle_unaware_of_x.len(), 2);
    let member = |name| {
        writers_bundle_unaware_of_x
            .iter()
            .find(|frame| frame.scope().unwrap() == f.scope(name))
            .unwrap()
            .clone()
    };
    let mut statuses = Vec::new();
    for frame in &writers_bundle_unaware_of_x {
        statuses.push(full.receive(frame.clone()).await.unwrap());
    }
    assert_eq!(
        statuses,
        [ReceiveStatus::PendingDependencies, ReceiveStatus::Applied],
        "a bundle applies once every member this replica holds is present"
    );
    assert_eq!(
        keys(&f, &full, &["A", "B"]).await,
        vec![vec![
            SqlValue::Text("B".into()),
            SqlValue::Text("k".into()),
            SqlValue::Integer(1)
        ]]
    );
    let txn = transaction(&member("A"), [1; 32], &f);
    let dropped = Some(TransactionOutcome::Dropped {
        reason: DropReason::Constraint,
        sealed: false,
    });
    assert_eq!(
        full.transaction_outcome(txn).await.unwrap(),
        dropped,
        "X drops the writer's transaction recreating `k`"
    );

    assert_eq!(
        partial.receive(member("A")).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(
        keys(&f, &partial, &["A"]).await.len(),
        1,
        "R holds only A, so its member applies provisionally"
    );
    assert_eq!(
        partial.transaction_outcome(txn).await.unwrap(),
        Some(TransactionOutcome::Applied { sealed: false })
    );
    let [record] = records(&f, &full, &x, "A").await.try_into().unwrap();
    assert_eq!(
        partial.receive(record).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert!(
        keys(&f, &partial, &["A"]).await.is_empty(),
        "X's record in A's log names B, which R does not hold, so R drops too"
    );
    assert_eq!(partial.transaction_outcome(txn).await.unwrap(), dropped);

    assert_eq!(
        writer.receive(x_creates_k_in_b).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(
        writer.transaction_outcome(txn).await.unwrap(),
        dropped,
        "learning of the earlier create, the writer's own transaction rewinds and drops"
    );
    for record in records(&f, &full, &x, "A").await {
        assert_eq!(
            writer.receive(record).await.unwrap(),
            ReceiveStatus::Applied,
            "the writer's outcome agrees with X's record"
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_seal_covers_a_transaction_once_every_member_has_a_record() {
    let root = rocky_platform::fs::Scratch::new().await;
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let (w, r) = (PrivateKey::new(), PrivateKey::new());
    let writer = f
        .open(
            &root.path().join("w.sqlite"),
            w.clone(),
            f.grant(None, &all()),
        )
        .await;
    let partial = f
        .open(
            &root.path().join("r.sqlite"),
            r.clone(),
            f.grant(Some(&["A"]), &all()),
        )
        .await;
    f.key(&writer, "A", [1; 32]).await;
    f.key(&writer, "B", [2; 32]).await;
    f.key(&partial, "A", [1; 32]).await;
    let both = insert(&f, &writer, &["A", "B"], &[("A", "a", 1), ("B", "b", 1)], 1).await;
    let [record] = records(&f, &writer, &w, "A")
        .await
        .try_into()
        .expect("the author records its transaction in every member's log at commit");
    let member = both
        .iter()
        .find(|frame| frame.scope().unwrap() == f.scope("A"))
        .unwrap()
        .clone();
    assert_eq!(
        partial.receive(member).await.unwrap(),
        ReceiveStatus::Applied
    );
    partial.seal_replayed_for_test(f.scope("A")).await.unwrap();
    assert_eq!(
        partial.unsealed_replayed_transactions().await.unwrap(),
        1,
        "only A's own record names its member; B has none here yet"
    );
    assert_eq!(
        partial.receive(record).await.unwrap(),
        ReceiveStatus::Applied
    );
    partial.seal_replayed_for_test(f.scope("A")).await.unwrap();
    assert_eq!(partial.unsealed_replayed_transactions().await.unwrap(), 0);

    lose_outcome_records(&writer).await;
    writer.publish_missing_outcomes().await.unwrap();
    assert_eq!(
        records(&f, &writer, &w, "A").await.len(),
        2,
        "a replica holding the transaction whole fills in a lost record"
    );
    assert_eq!(records(&f, &writer, &w, "B").await.len(), 2);
    writer.publish_missing_outcomes().await.unwrap();
    assert_eq!(records(&f, &writer, &w, "A").await.len(), 2);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_rewind_waits_for_its_partition_keys_and_records_only_changed_outcomes() {
    let root = rocky_platform::fs::Scratch::new().await;
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let (w, x, r) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let open = |name: &'static str, node: &PrivateKey| {
        let (f, root, node) = (&f, &root, node.clone());
        async move {
            f.open(&root.path().join(name), node.clone(), f.grant(None, &all()))
                .await
        }
    };
    let writer = open("w.sqlite", &w).await;
    let other = open("x.sqlite", &x).await;
    let reader = open("r.sqlite", &r).await;
    for replica in [&writer, &other, &reader] {
        f.key(replica, "A", [1; 32]).await;
        f.key(replica, "B", [2; 32]).await;
    }
    let [earlier] = insert(&f, &other, &["A", "B"], &[("A", "e", 1)], 1)
        .await
        .try_into()
        .unwrap();
    let both = insert(&f, &writer, &["A", "B"], &[("A", "a", 1), ("B", "b", 1)], 2).await;
    assert_eq!(records(&f, &writer, &w, "A").await.len(), 1);
    for status in reader.receive_all(both).await {
        assert_eq!(status.unwrap(), ReceiveStatus::Applied);
    }

    drop(reader);
    let reader = open("r.sqlite", &r).await;
    f.key(&reader, "A", [1; 32]).await;
    assert_ne!(
        reader.receive(earlier.clone()).await.unwrap(),
        ReceiveStatus::Rejected,
        "with only A's key the reader cannot redo the member in B, so the earlier arrival waits"
    );
    assert!(matches!(
        sql_on(&f, &reader, &["A"])
            .await
            .run("SELECT key FROM items WHERE list='A'", vec![])
            .await,
        Err(Error::Sql(crate::sql::SqlError::Unavailable {
            reason: crate::sql::Unavailable::Incomplete,
            ..
        }))
    ));
    f.key(&reader, "B", [2; 32]).await;
    assert_eq!(keys(&f, &reader, &["A", "B"]).await.len(), 3);

    assert_eq!(
        writer.receive(earlier).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(keys(&f, &writer, &["A", "B"]).await.len(), 3);
    assert_eq!(
        records(&f, &writer, &w, "A").await.len(),
        1,
        "the writer's rewound transaction redoes with the same outcome and publishes no new record"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_disagreeing_outcome_record_is_refused_as_divergence_naming_both_engines() {
    let root = rocky_platform::fs::Scratch::new().await;
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let (w, x, r) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let open = |name: &'static str, node: &PrivateKey| {
        let (f, root, node) = (&f, &root, node.clone());
        async move {
            let replica = f
                .open(&root.path().join(name), node.clone(), f.grant(None, &all()))
                .await;
            f.key(&replica, "A", [1; 32]).await;
            f.key(&replica, "B", [2; 32]).await;
            replica
        }
    };
    let (writer, forger, reader) = (
        open("w.sqlite", &w).await,
        open("x.sqlite", &x).await,
        open("r.sqlite", &r).await,
    );
    let both = insert(&f, &writer, &["A", "B"], &[("A", "a", 1), ("B", "b", 1)], 1).await;
    for status in forger.receive_all(both.clone()).await {
        assert_eq!(status.unwrap(), ReceiveStatus::Applied);
    }
    let member = both
        .iter()
        .find(|frame| frame.scope().unwrap() == f.scope("A"))
        .unwrap();
    let txn = transaction(member, [1; 32], &f);
    forger
        .forge_outcome_for_test(
            txn,
            Some(DropReason::Constraint),
            "forged engine",
            &[f.scope("A"), f.scope("B")],
        )
        .await
        .unwrap();
    let [honest, forged] = records(&f, &forger, &x, "A").await.try_into().unwrap();
    let [authored] = records(&f, &writer, &w, "A").await.try_into().unwrap();

    for status in reader.receive_all(both).await {
        assert_eq!(status.unwrap(), ReceiveStatus::Applied);
    }
    for record in [authored, honest] {
        assert_eq!(
            reader.receive(record).await.unwrap(),
            ReceiveStatus::Applied
        );
    }
    assert_eq!(
        reader.receive(forged).await.unwrap(),
        ReceiveStatus::Rejected
    );
    let [divergence] = reader
        .outcome_divergences()
        .await
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(divergence.transaction, txn);
    assert_eq!(divergence.publisher, x.public_key());
    assert!(
        divergence.stored.starts_with("rocky-secure "),
        "{divergence:?}"
    );
    assert_eq!(divergence.refused, "forged engine");
    assert_eq!(
        reader.transaction_outcome(txn).await.unwrap(),
        Some(TransactionOutcome::Applied { sealed: false }),
        "a refused forged record changes nothing"
    );
    assert_eq!(keys(&f, &reader, &["A", "B"]).await.len(), 2);
}
