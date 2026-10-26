//! A scope that waits for a seal rebootstrap holds back only itself; a lost
//! compaction holds back its whole domain.
use super::super::blockers::{assert_named, refused};
use super::super::generations::{identity_insert, select, session};
use super::*;

async fn insert_tag(replica: &Replica, key: &str) -> Result<crate::SecureRows, Error> {
    identity_insert(
        &session(replica).await,
        "INSERT INTO tags(key,label) VALUES(?,'label')",
        vec![text(key)],
    )
    .await
}
fn rebootstrap(kind: &crate::BlockerKind) -> bool {
    matches!(kind, crate::BlockerKind::SealRebootstrap { .. })
}

#[tokio::test]
async fn a_scope_awaiting_a_seal_rebootstrap_holds_back_only_itself() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "one", "one").await;
    seal(&lineage, &a).await;
    a.compact(lineage.domain, 0).await.unwrap();
    // B imports `items` at the seal; `tags` has no verified base there.
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    crate::tests::exchange_graphs(&a, &b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    import_checkpoint(&lineage, &a, &b, &keys[1], lineage.scope("items")).await;
    let (items, tags) = (lineage.scope("items"), lineage.scope("tags"));
    let needs = b.seal_rebootstraps(lineage.domain).await.unwrap();
    assert_eq!(
        needs.iter().map(|need| need.scope).collect::<Vec<_>>(),
        vec![tags]
    );
    assert_eq!(
        b.compaction_status(lineage.domain)
            .await
            .unwrap()
            .rebootstrap,
        None
    );

    // The other scope writes and reads.
    insert(&b, "two", "two").await;
    assert_eq!(
        titles(&b).await,
        vec![
            vec![text("one"), text("one")],
            vec![text("two"), text("two")]
        ]
    );

    // The waiting scope refuses writes and reads with its own blocker only.
    let label = crate::scope_label(&tags);
    let refusal = refused(insert_tag(&b, "t").await);
    assert_named(&refusal, &label, rebootstrap, crate::ClearsBy::AnyPeer);
    assert!(
        refusal.iter().all(|blocker| blocker.scope == label),
        "{refusal:?}"
    );
    let read = session(&b)
        .await
        .run("SELECT label FROM tags", vec![])
        .await;
    assert_named(
        &refused(read),
        &label,
        rebootstrap,
        crate::ClearsBy::AnyPeer,
    );

    // Once its base is imported it writes too, and both scopes audit.
    import_checkpoint(&lineage, &a, &b, &keys[1], tags).await;
    insert_tag(&b, "t").await.unwrap();
    assert_eq!(
        select(&b, "SELECT label FROM tags").await,
        vec![vec![text("label")]]
    );
    b.audit(items).await.unwrap();
    b.audit(tags).await.unwrap();
}

/// The finalized seals and the identity ledger they fold are the domain's:
/// a compaction that lost refuses every scope until each rebootstraps.
#[tokio::test]
async fn a_lost_compaction_holds_back_every_scope_of_its_domain() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let replica = open(&lineage, &dir, "keep", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "a", "first").await;
    insert_tag(&replica, "t").await.unwrap();
    churn(&replica, 12, "c").await;
    let sealed = rivalled_seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let (_, statements) = rival(&lineage, &sealed);
    replica.receive_tip_statements(&statements).await.unwrap();
    let (items, tags) = (lineage.scope("items"), lineage.scope("tags"));
    replica.replay_at_seals(tags, false).await.unwrap();
    let needs = replica.seal_rebootstraps(lineage.domain).await.unwrap();
    assert_eq!(
        needs.iter().map(|need| need.scope).collect::<Vec<_>>(),
        vec![items]
    );
    let lost = |kind: &crate::BlockerKind| *kind == crate::BlockerKind::LostCompaction;
    let domain = rocky_core::domain_label(&lineage.domain);
    let refusal = refused(insert_tag(&replica, "u").await);
    assert_named(&refusal, &domain, lost, crate::ClearsBy::AnyPeer);
    replica.replay_at_seals(items, true).await.unwrap();
    insert_tag(&replica, "u").await.unwrap();
    insert(&replica, "b", "after").await;
    replica.audit(items).await.unwrap();
    replica.audit(tags).await.unwrap();
}
