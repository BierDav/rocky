//! An evicted receipt still refuses another origin before direct exposure.
use super::*;
use crate::storage::rows;

async fn evidence(replica: &Replica) -> Vec<Vec<SqlValue>> {
    rows(&replica.inner.lock(crate::replica::Class::Background).await.conn,"SELECT 'P',scope,author,sequence FROM secure_identity_marks UNION ALL SELECT 'O',scope,author,sequence FROM secure_identity_own UNION ALL SELECT 'R',entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT 'C',identity,entry,provenance FROM secure_admission_claims").await.unwrap()
}
async fn direct_state(replica: &Replica) -> Vec<Vec<SqlValue>> {
    rows(&replica.inner.lock(crate::replica::Class::Background).await.conn,"SELECT 'r',scope,row,fields FROM secure_row_records UNION ALL SELECT 'c',scope,row,fields FROM secure_checkpoint_rows UNION ALL SELECT 's',scope,signed,cursor FROM secure_rebootstrap UNION ALL SELECT 'l',scope,author,sequence FROM secure_identity_ledger UNION ALL SELECT 'h',context,head,head_signature FROM secure_admission_context UNION ALL SELECT 'v',revision,0,0 FROM secure_metadata").await.unwrap()
}

#[tokio::test]
async fn a_direct_checkpoint_page_cannot_replace_a_known_evicted_origin() {
    let lineage = Lineage::pinned(246);
    let dir = TempDir::new().unwrap();
    let writer_key = PrivateKey::from_bytes(&[247; 32]);
    let target_key = PrivateKey::from_bytes(&[248; 32]);
    let original = open(&lineage, &dir, "original", &writer_key).await;
    let target = open(&lineage, &dir, "target", &target_key).await;
    insert(&original, "one", "known origin").await;
    let frame = crate::tests::stored_frames(&original, "SELECT header,body FROM secure_operations")
        .await
        .remove(0);
    assert_eq!(target.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    let permanent = evidence(&target).await;
    target
        .set_role(crate::RoleConfig {
            role: crate::ReplicaRole::Partial,
            full_nodes: [rocky_credentials::PeerId::of(writer_key.public_key())].into(),
            custody: 1,
        })
        .await
        .unwrap();
    target.evict(lineage.scope("items")).await.unwrap();
    assert_eq!(evidence(&target).await, permanent);
    assert_eq!(
        scalar(&target, "SELECT count(*) FROM secure_identity_ledger").await,
        0
    );
    assert_eq!(
        scalar(&target, "SELECT count(*) FROM secure_operations").await,
        0
    );
    assert_eq!(
        scalar(&target, "SELECT count(*) FROM secure_stubs").await,
        0
    );
    // A second file using the identity author's key signs a distinct valid
    // first origin. Its checkpoint is internally valid, but conflicts with R.
    let forger = open(&lineage, &dir, "forger", &writer_key).await;
    insert(&forger, "two", "different origin").await;
    let scope = lineage.scope("items");
    let signed = crate::tests::compacted_base(&forger, lineage.publisher(), scope).await;
    crate::tests::exchange_graphs(&forger, &target, lineage.domain).await;
    let credentials = target
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .credentials
        .clone();
    forger
        .admit_peer(
            target_key.public_key(),
            &credentials,
            target.followed_tip().await,
        )
        .await
        .unwrap();
    target.begin_rebootstrap(signed.clone()).await.unwrap();
    assert_eq!(
        scalar(&target, "SELECT direct FROM secure_rebootstrap").await,
        1
    );
    let page = forger
        .checkpoint_page(
            target_key.public_key(),
            &credentials,
            scope,
            signed.id().unwrap(),
            None,
            64 * 1024,
        )
        .await
        .unwrap();
    let before = direct_state(&target).await;
    let error = target.stage_checkpoint_page(page).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("contradicts a known original creation"),
        "{error}"
    );
    assert_eq!(direct_state(&target).await, before);
    assert_eq!(evidence(&target).await, permanent);
    assert_eq!(
        scalar(&target, "SELECT count(*) FROM secure_row_records").await,
        0
    );
    assert_eq!(
        scalar(&target, "SELECT count(*) FROM secure_stubs").await,
        0
    );
}
