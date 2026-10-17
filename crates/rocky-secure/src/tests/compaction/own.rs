//! Covered own-birth maxima are signed and checked before exposing rows.
use super::*;
use crate::storage::{bindings, rows};

async fn snapshot(replica: &Replica) -> Vec<Vec<SqlValue>> {
    rows(&replica.inner.lock().await.conn, "SELECT 'r',scope,row,fields FROM secure_rows UNION ALL SELECT 'p',scope,author,sequence FROM secure_identity_marks UNION ALL SELECT 'o',scope,author,sequence FROM secure_identity_own UNION ALL SELECT 'l',scope,author,sequence FROM secure_identity_ledger UNION ALL SELECT 'e',entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT 'c',identity,entry,provenance FROM secure_admission_claims UNION ALL SELECT 'h',context,head,head_signature FROM secure_admission_context UNION ALL SELECT 'b',scope,signed,sets FROM secure_checkpoints UNION ALL SELECT 's',scope,signed,cursor FROM secure_rebootstrap UNION ALL SELECT 'v',revision,0,0 FROM secure_metadata").await.unwrap()
}

#[tokio::test]
async fn a_signed_own_floor_must_cover_own_rows_before_a_direct_page_or_install() {
    let lineage = Lineage::pinned(222);
    let dir = TempDir::new().unwrap();
    let keys = [223, 224, 225].map(|seed| PrivateKey::from_bytes(&[seed; 32]));
    let source = open(&lineage, &dir, "source", &keys[0]).await;
    follow_main(&lineage, &source).await;
    insert(&source, "one", "owned").await;
    let frame = crate::tests::stored_frames(
        &source,
        "SELECT header,body FROM secure_operations ORDER BY hash",
    )
    .await
    .remove(0);
    let signed =
        crate::tests::compacted_base(&source, lineage.publisher(), lineage.scope("items")).await;
    assert_eq!(signed.statement.identities.len(), 1);
    assert_eq!(signed.statement.identities[0].own, 1);
    let sign = |own| {
        let mut statement = signed.statement.clone();
        statement.identities[0].own = own;
        crate::SignedCheckpoint::sign(statement, &keys[0], lineage.credentials(3, &keys[0]))
            .unwrap()
    };
    let lowered = sign(0);
    let invalid = sign(2);
    let direct = open(&lineage, &dir, "direct", &keys[1]).await;
    crate::tests::exchange_graphs(&source, &direct, lineage.domain).await;
    assert!(direct.begin_rebootstrap(invalid).await.is_err());
    direct.begin_rebootstrap(lowered.clone()).await.unwrap();
    assert_eq!(
        scalar(&direct, "SELECT direct FROM secure_rebootstrap").await,
        1
    );
    let scope = lineage.scope("items");
    // A malicious issuer can sign an internally inconsistent own maximum.
    source.inner.lock().await.conn.execute(
        "UPDATE secure_checkpoints SET signed=? WHERE scope=(SELECT id FROM secure_scope_state WHERE scope=?)",
        bindings(vec![SqlValue::Blob(lowered.to_bytes().unwrap()),SqlValue::Blob(encode(&scope).unwrap())]),
    ).await.unwrap();
    let credentials = lineage.credentials(3, &keys[1]);
    source
        .admit_peer(
            keys[1].public_key(),
            &credentials,
            direct.followed_tip().await,
        )
        .await
        .unwrap();
    let page = source
        .checkpoint_page(
            keys[1].public_key(),
            &credentials,
            scope,
            lowered.id().unwrap(),
            None,
            64 * 1024,
        )
        .await
        .unwrap();
    let before = snapshot(&direct).await;
    let error = direct.stage_checkpoint_page(page).await.unwrap_err();
    assert!(
        error.to_string().contains("checkpoint identity exceeds"),
        "{error}"
    );
    assert_eq!(snapshot(&direct).await, before);
    assert_eq!(scalar(&direct, "SELECT count(*) FROM secure_rows").await, 0);
    source.inner.lock().await.conn.execute(
        "UPDATE secure_checkpoints SET signed=? WHERE scope=(SELECT id FROM secure_scope_state WHERE scope=?)",
        bindings(vec![SqlValue::Blob(signed.to_bytes().unwrap()),SqlValue::Blob(encode(&scope).unwrap())]),
    ).await.unwrap();
    let staged = open(&lineage, &dir, "staged", &keys[2]).await;
    // Local state makes this a staged import, so refusal must preserve it.
    staged.receive(frame).await.unwrap();
    crate::tests::exchange_graphs(&source, &staged, lineage.domain).await;
    staged.begin_rebootstrap(signed.clone()).await.unwrap();
    assert_eq!(
        scalar(&staged, "SELECT direct FROM secure_rebootstrap").await,
        0
    );
    let credentials = lineage.credentials(3, &keys[2]);
    source
        .admit_peer(
            keys[2].public_key(),
            &credentials,
            staged.followed_tip().await,
        )
        .await
        .unwrap();
    let page = source
        .checkpoint_page(
            keys[2].public_key(),
            &credentials,
            scope,
            signed.id().unwrap(),
            None,
            64 * 1024,
        )
        .await
        .unwrap();
    staged.stage_checkpoint_page(page).await.unwrap();
    staged
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_rebootstrap SET signed=?",
            [lowered.to_bytes().unwrap()],
        )
        .await
        .unwrap();
    let before = snapshot(&staged).await;
    let error = staged.finish_rebootstrap(scope).await.unwrap_err();
    assert!(matches!(error, Error::Corrupt(_)), "{error}");
    assert_eq!(snapshot(&staged).await, before);
    assert_eq!(
        titles(&staged).await,
        vec![vec![text("one"), text("owned")]]
    );
}
