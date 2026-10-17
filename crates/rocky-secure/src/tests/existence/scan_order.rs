//! Conflicting retained obligations cannot be lowered by a physical scan order.
use super::*;
use crate::storage::rows;

async fn evidence(device: &Device) -> Vec<Vec<Vec<SqlValue>>> {
    let inner = device.replica.inner.lock().await;
    let mut result = Vec::new();
    for table in [
        "secure_finalized",
        "secure_seal_bounds",
        "secure_checkpoints",
        "secure_kept_marks",
        "secure_metadata",
        "secure_admission_context",
        "secure_admission_entries",
        "secure_admission_claims",
    ] {
        result.push(
            rows(&inner.conn, &format!("SELECT * FROM {table} ORDER BY 1"))
                .await
                .unwrap(),
        );
    }
    result
}

#[tokio::test]
async fn conflicting_existence_counts_take_the_maximum_before_finalization() {
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let device = erp
        .open(
            &dir.path().join("count-order.db"),
            &PrivateKey::new(),
            11,
            &TABLES,
        )
        .await;
    let (_, frames) = commit(
        &erp.sql(&device).await,
        &[("INSERT INTO lists(title,id) VALUES('counted',?id)", vec![])],
    )
    .await;
    let original: p2panda_core::Header<crate::wire::Extensions> =
        decode(frames[0].header_bytes()).unwrap();
    assert_eq!(original.extensions.existence, 1);
    let (seal, _) = super::seals::seal(&erp, &device).await;
    let scope = erp.scope("lists");
    let before = evidence(&device).await;
    let inner = device.replica.inner.lock().await;
    let id = match rows(&inner.conn, "SELECT id FROM secure_scope_state WHERE scope=(SELECT scope FROM secure_operations LIMIT 1)").await.unwrap()[0][0] {
        SqlValue::Integer(id) => id,
        _ => panic!("scope id"),
    };
    let scopes = [(scope, Some(id))];
    let held = BTreeSet::from([scope]);
    assert!(
        inner
            .seal_waits_for_test(&seal, &scopes)
            .await
            .unwrap()
            .is_none()
    );
    for count in [2, 1, 0] {
        inner
            .conn
            .execute("SAVEPOINT conflicting_count", ())
            .await
            .unwrap();
        let mut conflicting = original.clone();
        conflicting.extensions.existence = count;
        conflicting.sign(&device.node);
        let mut repeated = original.clone();
        repeated.timestamp += 1;
        repeated.sign(&device.node);
        // Deliberate retained SQL/header mismatch, never an admissible intake.
        inner
            .conn
            .execute(
                "INSERT INTO secure_voids(scope,author,sequence,hash,header) VALUES(?,?,?,?,?)",
                crate::storage::bindings(vec![
                    SqlValue::Blob(vec![0]),
                    SqlValue::Blob(original.public_key.as_bytes().to_vec()),
                    SqlValue::Integer(original.seq_num as i64),
                    SqlValue::Blob(conflicting.hash().as_bytes().to_vec()),
                    SqlValue::Blob(conflicting.to_bytes()),
                ]),
            )
            .await
            .unwrap();
        inner
            .conn
            .execute(
                "INSERT INTO secure_voids(scope,author,sequence,hash,header) VALUES(?,?,?,?,?)",
                crate::storage::bindings(vec![
                    SqlValue::Blob(vec![1]),
                    SqlValue::Blob(original.public_key.as_bytes().to_vec()),
                    SqlValue::Integer(original.seq_num as i64),
                    SqlValue::Blob(repeated.hash().as_bytes().to_vec()),
                    SqlValue::Blob(repeated.to_bytes()),
                ]),
            )
            .await
            .unwrap();
        let mut orders = Vec::new();
        for reverse in [false, true] {
            inner
                .conn
                .execute(
                    if reverse {
                        "PRAGMA reverse_unordered_selects=ON"
                    } else {
                        "PRAGMA reverse_unordered_selects=OFF"
                    },
                    (),
                )
                .await
                .unwrap();
            let mut order = Vec::new();
            for row in rows(&inner.conn, "SELECT header FROM secure_voids")
                .await
                .unwrap()
            {
                let [SqlValue::Blob(bytes)] = row.as_slice() else {
                    panic!("void header")
                };
                order.push(
                    decode::<p2panda_core::Header<crate::wire::Extensions>>(bytes)
                        .unwrap()
                        .extensions
                        .existence,
                );
            }
            orders.push(order);
            let positions = inner
                .scan_positions_for_test(erp.domain, &scopes, &held, &held)
                .await
                .unwrap();
            assert_eq!(
                positions.2[&(scope, original.extensions.author.unwrap())]
                    [&original.extensions.author_sequence],
                u64::from(count.max(1))
            );
            let waiting = inner.seal_waits_for_test(&seal, &scopes).await.unwrap();
            if count == 2 {
                assert!(waiting.unwrap().contains("missing existence records"));
            } else {
                assert!(waiting.is_none());
            }
        }
        if count != 1 {
            assert_ne!(
                orders[0], orders[1],
                "both physical visitation orders were exercised"
            );
        }
        inner
            .conn
            .execute("ROLLBACK TO conflicting_count", ())
            .await
            .unwrap();
        inner
            .conn
            .execute("RELEASE conflicting_count", ())
            .await
            .unwrap();
    }
    inner
        .conn
        .execute("PRAGMA reverse_unordered_selects=OFF", ())
        .await
        .unwrap();
    assert!(
        inner
            .seal_waits_for_test(&seal, &scopes)
            .await
            .unwrap()
            .is_none()
    );
    drop(inner);
    assert_eq!(evidence(&device).await, before);
    device.replica.audit(scope).await.unwrap();
}
