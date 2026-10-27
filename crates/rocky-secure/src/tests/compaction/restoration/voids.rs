//! Incoming author-signed cancellation records remain distinct from payloads.
use super::*;

#[tokio::test]
async fn held_frames_and_separate_signed_void_records_require_every_link() {
    let mut f = HeldPrefix::new(4).await;
    let scope = f.lineage.scope("items");
    f.replica.receive(f.frames[3].clone()).await.unwrap();
    // Exercise actual incoming records, not the local unsent-cancellation policy.
    let middle =
        crate::SignedVoid::sign(&f.key, vec![f.frames[2].header_bytes().to_vec()]).unwrap();
    let first = crate::SignedVoid::sign(&f.key, vec![f.frames[0].header_bytes().to_vec()]).unwrap();
    assert!(matches!(
        f.replica.receive_voids(std::slice::from_ref(&middle)).await[..],
        [Ok(crate::ReceiveStatus::Applied)]
    ));
    f.waits().await;
    assert!(matches!(
        f.replica.receive_voids(std::slice::from_ref(&first)).await[..],
        [Ok(crate::ReceiveStatus::Applied)]
    ));
    f.waits().await;
    assert_eq!(
        f.replica.receive(f.frames[1].clone()).await.unwrap(),
        crate::ReceiveStatus::Held
    );
    let records = rows(
        &f.replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT sequence,record FROM secure_voids ORDER BY sequence",
    )
    .await
    .unwrap();
    let [first_record, middle_record] = records.as_slice() else {
        panic!("two void positions")
    };
    let [SqlValue::Integer(0), SqlValue::Blob(first_digest)] = first_record.as_slice() else {
        panic!("first void")
    };
    let [SqlValue::Integer(2), SqlValue::Blob(digest)] = middle_record.as_slice() else {
        panic!("middle void")
    };
    let (first_digest, digest) = (first_digest.clone(), digest.clone());
    // Privileged corruption controls: a signed header alone does not replace its record.
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_void_records SET record=zeroblob(length(record)) WHERE digest=?",
            [digest.to_vec()],
        )
        .await
        .unwrap();
    let corrupt = super::super::audit::snapshot(&f.replica).await;
    assert!(f.replica.restored(scope).await.is_err());
    assert_eq!(super::super::audit::snapshot(&f.replica).await, corrupt);
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_void_records SET record=? WHERE digest=?",
            [crate::encode(&middle).unwrap(), digest.to_vec()],
        )
        .await
        .unwrap();
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_voids SET record=? WHERE hash=?",
            [
                first_digest.clone(),
                f.frames[2].id().unwrap().as_bytes().to_vec(),
            ],
        )
        .await
        .unwrap();
    let corrupt = super::super::audit::snapshot(&f.replica).await;
    assert!(f.replica.restored(scope).await.is_err());
    assert_eq!(super::super::audit::snapshot(&f.replica).await, corrupt);
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "UPDATE secure_voids SET record=? WHERE hash=?",
            [
                digest.to_vec(),
                f.frames[2].id().unwrap().as_bytes().to_vec(),
            ],
        )
        .await
        .unwrap();
    let held_before = held(&f.replica).await;
    let voids_before = rows(
        &f.replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT * FROM secure_voids ORDER BY sequence",
    )
    .await
    .unwrap();
    f.replica.restored(scope).await.unwrap();
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_applied").await,
        0
    );
    assert_eq!(held(&f.replica).await, held_before);
    assert_eq!(
        rows(
            &f.replica
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .conn,
            "SELECT * FROM secure_voids ORDER BY sequence"
        )
        .await
        .unwrap(),
        voids_before
    );
    assert!(titles(&f.replica).await.is_empty());
    f.replica.audit(scope).await.unwrap();
    f = f.reopen().await;
    assert_eq!(held(&f.replica).await, held_before);
    insert(&f.replica, "new", "target line").await;
    f.replica.audit(scope).await.unwrap();
}
