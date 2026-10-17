//! Only installed verified prefixes root restoration; staging does not.
use super::*;

async fn stage(f: &HeldPrefix) {
    let (_, keeper) = f.keeper.as_ref().unwrap();
    let scope = f.lineage.scope("items");
    let credentials = f.lineage.credentials(3, &f.key);
    keeper
        .admit_peer(
            f.key.public_key(),
            &credentials,
            f.replica.followed_tip().await,
        )
        .await
        .unwrap();
    let offered = keeper
        .checkpoint_offer(f.key.public_key(), &credentials, scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offered.statement.frontier.len(), 1);
    assert_eq!(offered.statement.frontier[0].author, f.key.public_key());
    let checkpoint = offered.id().unwrap();
    f.replica.begin_rebootstrap(offered).await.unwrap();
    let mut after = None;
    loop {
        let page = keeper
            .checkpoint_page(
                f.key.public_key(),
                &credentials,
                scope,
                checkpoint,
                after,
                64 * 1024,
            )
            .await
            .unwrap();
        after = page.next;
        f.replica.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
}

#[tokio::test]
async fn a_verified_lower_prefix_anchors_a_held_and_signed_void_suffix() {
    let mut f = HeldPrefix::with_anchor(2, 3).await;
    let scope = f.lineage.scope("items");
    // Positions 0/1 are evicted and never returned as frames.
    f.replica.receive(f.frames[4].clone()).await.unwrap();
    let void = crate::SignedVoid::sign(&f.key, vec![f.frames[3].header_bytes().to_vec()]).unwrap();
    assert!(f.replica.receive_voids(&[void]).await[0].is_ok());
    stage(&f).await;
    f.waits().await;
    f.replica.finish_rebootstrap(scope).await.unwrap();
    f.waits().await;
    f = f.reopen().await;
    f.waits().await;
    f.replica.receive(f.frames[2].clone()).await.unwrap();
    let before = held(&f.replica).await;
    f.replica.restored(scope).await.unwrap();
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    assert_eq!(held(&f.replica).await, before);
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_applied").await,
        0
    );
    assert_eq!(
        titles(&f.replica).await,
        vec![
            vec![text("base0"), text("installed prefix")],
            vec![text("base1"), text("installed prefix")]
        ]
    );
    f.replica.audit(scope).await.unwrap();
    insert(&f.replica, "new", "target line").await;
    f = f.reopen().await;
    f.replica.audit(scope).await.unwrap();
    assert_eq!(titles(&f.replica).await.len(), 3);
}

#[tokio::test]
async fn an_exact_verified_installed_endpoint_covers_its_evicted_frames() {
    let mut f = HeldPrefix::with_anchor(3, 0).await;
    let scope = f.lineage.scope("items");
    stage(&f).await;
    f.waits().await;
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_operations").await,
        0
    );
    f.replica.finish_rebootstrap(scope).await.unwrap();
    // Privileged obligation control over a genuine higher installed base:
    // its frontier at 2 does not reveal the exact hash at 1.
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_evicted_logs SET sequence=1,hash=? WHERE scope=?",
            [
                f.frames[1].id().unwrap().as_bytes().to_vec(),
                crate::encode(&scope).unwrap(),
            ],
        )
        .await
        .unwrap();
    super::negatives::preserved(&f, false).await;
    f.replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_evicted_logs SET sequence=2,hash=? WHERE scope=?",
            [
                f.frames[2].id().unwrap().as_bytes().to_vec(),
                crate::encode(&scope).unwrap(),
            ],
        )
        .await
        .unwrap();
    f.replica.restored(scope).await.unwrap();
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_evicted_logs").await,
        0
    );
    assert_eq!(
        scalar(&f.replica, "SELECT count(*) FROM secure_operations").await,
        0
    );
    assert_eq!(titles(&f.replica).await.len(), 3);
    f.replica.audit(scope).await.unwrap();
    // The sibling scope needs its own authenticated target state before writes.
    let (_, keeper) = f.keeper.as_ref().unwrap();
    import_checkpoint(
        &f.lineage,
        keeper,
        &f.replica,
        &f.key,
        f.lineage.scope("tags"),
    )
    .await;
    f = f.reopen().await;
    insert(&f.replica, "new", "after exact installed endpoint").await;
    f.replica.audit(scope).await.unwrap();
    assert_eq!(titles(&f.replica).await.len(), 4);
}
