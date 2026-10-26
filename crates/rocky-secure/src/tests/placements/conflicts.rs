//! Retained signed headers at one domain position must not hide a conflict.
use super::*;
use crate::storage::{bindings, rows};
use rocky_core::SqlValue;

async fn unchanged_state(replica: &Replica) -> Vec<Vec<Vec<SqlValue>>> {
    let inner = replica.inner.lock().await;
    let tables = rows(&inner.conn, "SELECT name FROM sqlite_schema WHERE type='table' AND name LIKE 'secure_%' AND name NOT IN ('secure_equivocations','secure_placements') ORDER BY name").await.unwrap();
    let mut state = Vec::new();
    for table in tables {
        let [SqlValue::Text(name)] = table.as_slice() else {
            panic!("table name")
        };
        state.push(
            rows(&inner.conn, &format!("SELECT * FROM {name} ORDER BY 1"))
                .await
                .unwrap(),
        );
    }
    state
}

#[tokio::test]
async fn placement_fallback_finds_conflicts_after_equal_matches_and_checks_its_suffix() {
    let pair = pair().await;
    let replica = &pair.alice;
    let inner = replica.inner.lock().await;
    let records = rows(
        &inner.conn,
        "SELECT header FROM secure_operations ORDER BY sequence",
    )
    .await
    .unwrap();
    let mut headers = records
        .iter()
        .map(|row| {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                panic!("header")
            };
            crate::wire::verified_header(bytes).unwrap()
        })
        .collect::<Vec<_>>();
    let first = headers
        .iter()
        .find(|h| h.extensions.scope == pair.lineage.scope("items"))
        .unwrap()
        .clone();
    let mut other = headers
        .iter()
        .find(|h| h.extensions.scope == pair.lineage.scope("tags"))
        .unwrap()
        .clone();
    other.extensions.author_sequence = first.extensions.author_sequence;
    other.sign(&inner.config.node);
    headers.clear();
    // A narrow actual-place harness: these retained headers are signed and
    // scope-bound. Public request/custody/visibility gates have separate controls.
    for header in [&first, &other] {
        crate::wire::verified_header(&header.to_bytes()).unwrap();
        inner
            .verify_header_authority(header, &inner.config.credentials, now().unwrap())
            .unwrap();
    }
    assert_ne!(first.hash(), other.hash());
    assert!(!inner.place_verified_for_test(&first).await.unwrap());
    assert!(
        rows(&inner.conn, "SELECT 1 FROM secure_equivocations")
            .await
            .unwrap()
            .is_empty()
    );
    inner
        .conn
        .execute("SAVEPOINT retained_conflicts", ())
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "INSERT INTO secure_voids(scope,author,sequence,hash,header) VALUES(?,?,?,?,?)",
            bindings(vec![
                SqlValue::Blob(encode(&other.extensions.scope).unwrap()),
                SqlValue::Blob(other.public_key.as_bytes().to_vec()),
                SqlValue::Integer(other.seq_num as i64),
                SqlValue::Blob(other.hash().as_bytes().to_vec()),
                SqlValue::Blob(other.to_bytes()),
            ]),
        )
        .await
        .unwrap();
    crate::replica::positions::insert(&inner.conn, &other, 1, false, true)
        .await
        .unwrap();
    drop(inner);
    let before = unchanged_state(replica).await;
    let inner = replica.inner.lock().await;
    for incoming in [&first, &other] {
        inner
            .conn
            .execute("SAVEPOINT pair_choice", ())
            .await
            .unwrap();
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
            assert!(!inner.place_verified_for_test(incoming).await.unwrap());
            let evidence = rows(
                &inner.conn,
                "SELECT domain,author,sequence,first,second FROM secure_equivocations",
            )
            .await
            .unwrap();
            let [
                SqlValue::Blob(domain),
                SqlValue::Integer(author),
                SqlValue::Integer(sequence),
                SqlValue::Blob(a),
                SqlValue::Blob(b),
            ] = evidence[0].as_slice()
            else {
                panic!("equivocation pair")
            };
            assert_eq!(evidence.len(), 1);
            assert_eq!(
                decode::<rocky_core::Scope>(domain).unwrap(),
                pair.lineage.domain
            );
            assert_eq!(*author as u32, first.extensions.author.unwrap());
            assert_eq!(*sequence as u64, first.extensions.author_sequence);
            let a = crate::wire::verified_header(a).unwrap();
            let b = crate::wire::verified_header(b).unwrap();
            assert_ne!(a.hash(), b.hash());
            assert_eq!(b.hash(), incoming.hash());
            assert_eq!(
                BTreeSet::from([a.hash(), b.hash()]),
                BTreeSet::from([first.hash(), other.hash()])
            );
            let mut third = other.clone();
            third.timestamp += 1;
            third.sign(&inner.config.node);
            assert!(!inner.place_verified_for_test(&third).await.unwrap());
            assert_eq!(
                rows(
                    &inner.conn,
                    "SELECT domain,author,sequence,first,second FROM secure_equivocations"
                )
                .await
                .unwrap(),
                evidence
            );
        }
        assert!(
            rows(&inner.conn, "SELECT 1 FROM secure_placements")
                .await
                .unwrap()
                .is_empty()
        );
        inner
            .conn
            .execute("ROLLBACK TO pair_choice", ())
            .await
            .unwrap();
        inner.conn.execute("RELEASE pair_choice", ()).await.unwrap();
    }
    inner
        .conn
        .execute("PRAGMA reverse_unordered_selects=OFF", ())
        .await
        .unwrap();
    drop(inner);
    assert_eq!(unchanged_state(replica).await, before);
    let inner = replica.inner.lock().await;
    inner
        .conn
        .execute("DELETE FROM secure_voids", ())
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "INSERT INTO secure_voids(scope,author,sequence,hash,header) VALUES(?,?,?,?,?)",
            bindings(vec![
                SqlValue::Blob(encode(&other.extensions.scope).unwrap()),
                SqlValue::Blob(other.public_key.as_bytes().to_vec()),
                SqlValue::Integer(other.seq_num as i64),
                SqlValue::Blob(other.hash().as_bytes().to_vec()),
                SqlValue::Blob(vec![0xff]),
            ]),
        )
        .await
        .unwrap();
    crate::replica::positions::insert(&inner.conn, &other, 1, false, true)
        .await
        .unwrap();
    assert!(
        matches!(inner.place_verified_for_test(&first).await, Err(Error::Rejected(message)) if message == "invalid secure frame")
    );
    assert!(
        rows(&inner.conn, "SELECT 1 FROM secure_equivocations")
            .await
            .unwrap()
            .is_empty()
    );
    // The existing exact-placement fast path deliberately keeps precedence.
    inner
        .conn
        .execute(
            "INSERT INTO secure_placements VALUES(?,?,?,?,?,?)",
            bindings(vec![
                SqlValue::Blob(encode(&pair.lineage.domain).unwrap()),
                SqlValue::Integer(i64::from(first.extensions.author.unwrap())),
                SqlValue::Integer(first.extensions.author_sequence as i64),
                SqlValue::Blob(encode(&first.extensions.scope).unwrap()),
                SqlValue::Blob(first.hash().as_bytes().to_vec()),
                SqlValue::Blob(first.to_bytes()),
            ]),
        )
        .await
        .unwrap();
    assert!(!inner.place_verified_for_test(&first).await.unwrap());
    inner
        .conn
        .execute("ROLLBACK TO retained_conflicts", ())
        .await
        .unwrap();
    inner
        .conn
        .execute("RELEASE retained_conflicts", ())
        .await
        .unwrap();
    drop(inner);
    replica.audit(pair.lineage.scope("items")).await.unwrap();
    replica.audit(pair.lineage.scope("tags")).await.unwrap();
}
