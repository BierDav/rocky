//! Privileged retained-store corruption never proves restoration.
use super::*;
use crate::storage::{bindings, collect_rows};

pub(super) async fn filled() -> HeldPrefix {
    let f = HeldPrefix::new(3).await;
    for frame in &f.frames {
        assert_eq!(
            f.replica.receive(frame.clone()).await.unwrap(),
            crate::ReceiveStatus::Held
        );
    }
    f
}

pub(super) async fn preserved(f: &HeldPrefix, error: bool) {
    let before = super::super::audit::snapshot(&f.replica).await;
    let result = f.replica.restored(f.lineage.scope("items")).await;
    assert_eq!(result.is_err(), error, "{result:?}");
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        1
    );
    assert_eq!(super::super::audit::snapshot(&f.replica).await, before);
}

#[tokio::test]
async fn unsettled_or_corrupt_original_bytes_cannot_discharge_the_guard() {
    let f = filled().await;
    let endpoint = &f.frames[2];
    let hash = endpoint.id().unwrap().as_bytes().to_vec();
    for status in ["pending", "rejected"] {
        f.replica
            .inner
            .lock()
            .await
            .conn
            .execute(
                "UPDATE secure_operations SET status=? WHERE hash=?",
                bindings(vec![
                    SqlValue::Text(status.into()),
                    SqlValue::Blob(hash.clone()),
                ]),
            )
            .await
            .unwrap();
        preserved(&f, false).await;
    }
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET status='held',body=zeroblob(length(body)) WHERE hash=?",
            [hash.clone()],
        )
        .await
        .unwrap();
    preserved(&f, true).await;
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET body=? WHERE hash=?",
            [endpoint.body_bytes().to_vec(), hash.clone()],
        )
        .await
        .unwrap();
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET sequence=sequence+1 WHERE hash=?",
            [hash.clone()],
        )
        .await
        .unwrap();
    preserved(&f, true).await;
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_operations SET sequence=sequence-1 WHERE hash=?",
            [hash],
        )
        .await
        .unwrap();
    f.replica.restored(f.lineage.scope("items")).await.unwrap();
    f.replica.audit(f.lineage.scope("items")).await.unwrap();
}

#[tokio::test]
async fn a_wrong_endpoint_or_higher_log_without_its_bridge_still_waits() {
    let f = filled().await;
    let scope = f.lineage.scope("items");
    let blob = crate::encode(&scope).unwrap();
    let endpoint = f.frames[2].id().unwrap();
    let wrong = Hash::new(b"not the recorded original endpoint");
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_evicted_logs SET hash=? WHERE scope=?",
            [wrong.as_bytes().to_vec(), blob.clone()],
        )
        .await
        .unwrap();
    preserved(&f, false).await;
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_evicted_logs SET hash=? WHERE scope=?",
            [endpoint.as_bytes().to_vec(), blob.clone()],
        )
        .await
        .unwrap();
    // A raw higher-log claim is corruption, not a signed bridge through H.
    let ids = {
        let inner = f.replica.inner.lock().await;
        collect_rows(
            inner
                .conn
                .query(
                    "SELECT id FROM secure_scope_state WHERE scope=?",
                    [blob.clone()],
                )
                .await
                .unwrap(),
        )
        .await
        .unwrap()
    };
    let [SqlValue::Integer(id)] = ids[0].as_slice() else {
        panic!("scope ID");
    };
    let id = *id;
    let clock = f.frames[2].operation_ref().unwrap().header.timestamp;
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "INSERT INTO secure_logs VALUES(?,?,?,?,?,-1)",
            bindings(vec![
                SqlValue::Integer(id),
                SqlValue::Blob(f.key.public_key().as_bytes().to_vec()),
                SqlValue::Integer(3),
                SqlValue::Blob(wrong.as_bytes().to_vec()),
                SqlValue::Integer(clock as i64 + 1),
            ]),
        )
        .await
        .unwrap();
    preserved(&f, false).await;
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute("DELETE FROM secure_logs WHERE scope=?", [id])
        .await
        .unwrap();
    f.replica.restored(scope).await.unwrap();
    f.replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn restoration_never_admits_unknown_own_frames_without_or_above_its_exact_guard() {
    let f = HeldPrefix::new(3).await;
    let mut higher = f.frames[2].operation_ref().unwrap().header.clone();
    higher.seq_num += 1;
    higher.timestamp += 1;
    higher.backlink = Some(f.frames[2].id().unwrap());
    higher.sign(&f.key);
    let before = super::super::audit::snapshot(&f.replica).await;
    let error = f
        .replica
        .receive(f.frames[2].with_header(higher.to_bytes()))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown operation from local signing identity"),
        "{error}"
    );
    assert_eq!(super::super::audit::snapshot(&f.replica).await, before);
    let mut wrong = f.frames[2].operation_ref().unwrap().header.clone();
    wrong.timestamp += 1;
    wrong.sign(&f.key);
    let error = f
        .replica
        .receive(f.frames[2].with_header(wrong.to_bytes()))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown operation from local signing identity"),
        "{error}"
    );
    assert_eq!(super::super::audit::snapshot(&f.replica).await, before);
    let fresh = open(&f.lineage, &f.directory, "without-guard", &f.key).await;
    fresh
        .receive_tip_statements(&crate::tests::all_tip_statements(&f.replica).await)
        .await
        .unwrap();
    fresh
        .follow_bookmark(f.lineage.domain, "main")
        .await
        .unwrap();
    assert_eq!(
        scalar(&fresh, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    let error = fresh.receive(f.frames[0].clone()).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown operation from local signing identity"),
        "{error}"
    );
    assert_eq!(
        scalar(&fresh, "SELECT count(*) FROM secure_operations").await,
        0
    );
}

#[tokio::test]
async fn a_complete_signed_header_placement_prefix_is_not_retained_payload() {
    let f = HeldPrefix::new(3).await;
    let scope = f.lineage.scope("items");
    let saved = rows(
        &f.replica.inner.lock().await.conn,
        "SELECT * FROM secure_placements",
    )
    .await
    .unwrap();
    // Privileged placement-store fixture: no claim that held readable scopes
    // can use the off-custody placement protocol to bypass ordinary intake.
    for frame in &f.frames {
        let operation = frame.operation_ref().unwrap();
        let header = &operation.header;
        f.replica
            .inner
            .lock()
            .await
            .conn
            .execute(
                "INSERT OR REPLACE INTO secure_placements VALUES(?,?,?,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(crate::encode(&scope.domain).unwrap()),
                    SqlValue::Integer(header.extensions.author.unwrap().into()),
                    SqlValue::Integer(header.extensions.author_sequence as i64),
                    SqlValue::Blob(crate::encode(&scope).unwrap()),
                    SqlValue::Blob(frame.id().unwrap().as_bytes().to_vec()),
                    SqlValue::Blob(frame.header_bytes().to_vec()),
                ]),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_operations").await,
        0
    );
    f.waits().await;
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute("DELETE FROM secure_placements", ())
        .await
        .unwrap();
    for record in saved {
        f.replica
            .inner
            .lock()
            .await
            .conn
            .execute(
                "INSERT INTO secure_placements VALUES(?,?,?,?,?,?)",
                bindings(record),
            )
            .await
            .unwrap();
    }
    for frame in &f.frames {
        f.replica.receive(frame.clone()).await.unwrap();
    }
    f.replica.restored(scope).await.unwrap();
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_applied").await,
        0
    );
    f.replica.audit(scope).await.unwrap();
}
