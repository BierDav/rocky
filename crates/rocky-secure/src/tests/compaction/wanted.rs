use super::*;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_node_that_follows_no_bookmark_pulls_the_seal_it_wants() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    let b = open(&lineage, &dir, "b", &keys[1]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "a", "one").await;
    sync(&lineage, &a, &b, &keys[1]).await;
    insert(&a, "b", "two").await;
    let first = seal(&lineage, &a).await.id().unwrap();
    a.compact(lineage.domain, 0).await.unwrap();
    b.receive_tip_statements(&a.tip_statements(None, 32).await.unwrap())
        .await
        .unwrap();
    assert_eq!(b.followed_bookmark().await, None);
    b.want_seal(lineage.domain, first).await.unwrap();
    eprintln!(
        "DBG scopes {:?}",
        b.seal_pull_scopes(lineage.domain, first).await
    );
    let (near, far) = crate::network::link::testing::connected().await;
    let (session, mut served) = crate::network::link::testing::sessions_over(
        near,
        far,
        (&b, keys[1].clone(), lineage.credentials(3, &keys[1])),
        (&a, keys[0].clone(), lineage.credentials(3, &keys[0])),
    )
    .await
    .unwrap();
    let pull = async {
        let mut session = session;
        session.pull_wanted_seals(&b).await.unwrap()
    };
    let (pulled, ()) = tokio::join!(pull, async { while served.serve_once(&a).await.is_ok() {} });
    assert!(pulled);
    assert_eq!(
        b.held_seal(lineage.domain, first).await.unwrap(),
        Some(crate::HeldSeal::Checkpoint),
        "a node asks for a seal before it follows the bookmark that starts from it"
    );
    assert!(b.wanted_seals(lineage.domain).await.unwrap().is_empty());
    assert_eq!(
        b.missing_bookmark_start(lineage.domain, "main")
            .await
            .unwrap(),
        None
    );
}
