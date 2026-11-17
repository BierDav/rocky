//! Placement privacy across partitions, and replayed partitions across a
//! schema switch.
use super::*;

mod filtered;

async fn items(f: &Fixture, replica: &EncryptedReplica<TypedRows>) -> Vec<Vec<SqlValue>> {
    replica
        .session([f.scope("A"), f.scope("B")].into())
        .await
        .unwrap()
        .sql(Default::default())
        .await
        .unwrap()
        .run("SELECT list,key,title FROM items ORDER BY list,key", vec![])
        .await
        .unwrap()
        .rows
}

async fn keyed(
    f: &Fixture,
    path: &Path,
    node: [u8; 32],
    grant: PartitionGrant,
) -> EncryptedReplica<TypedRows> {
    let replica = f.open(path, PrivateKey::from_bytes(&node), grant).await;
    f.key(&replica, "A", [1; 32]).await;
    f.key(&replica, "B", [2; 32]).await;
    replica
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_revoked_authors_unheld_partition_header_is_omitted() {
    let root = rocky_platform::fs::Scratch::new().await;
    let f = Fixture::new();
    let writer = f
        .open(
            &root.path().join("writer"),
            PrivateKey::new(),
            f.grant(None, &all()),
        )
        .await;
    let other = f
        .open(
            &root.path().join("other"),
            PrivateKey::new(),
            f.grant(None, &all()),
        )
        .await;
    let reader_key = PrivateKey::new();
    let reader = f
        .open(
            &root.path().join("reader"),
            reader_key.clone(),
            f.grant(Some(&["A"]), &[Right::Read]),
        )
        .await;
    for replica in [&writer, &other] {
        f.key(replica, "A", [1; 32]).await;
        f.key(replica, "secret-partition", [2; 32]).await;
    }
    f.key(&reader, "A", [1; 32]).await;
    let secret = |key| [("secret-partition", key, 30)];
    let visible = insert(&f, &writer, &["A"], &[("A", "one", 10)], 1)
        .await
        .remove(0);
    let hidden = insert(&f, &other, &["secret-partition"], &secret("two"), 2)
        .await
        .remove(0);
    let eligible = insert(&f, &writer, &["secret-partition"], &secret("three"), 3)
        .await
        .remove(0);
    writer.receive(hidden.clone()).await.unwrap();
    assert_eq!(
        reader.receive(visible).await.unwrap(),
        ReceiveStatus::Applied
    );
    seal_main(&writer, &f.publisher, f.domain).await;
    exchange_graphs(&writer, &reader, f.domain).await;
    let credentials = reader
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .credentials
        .clone();
    let requests = reader.wanted_placements().await.unwrap();
    assert_eq!(requests.len(), 2, "{requests:?}");
    let of = |frame: &EncryptedFrame| {
        let author = crate::writer::writer(&frame.operation().unwrap().header.public_key);
        requests
            .iter()
            .find(|request| request.author == author)
            .expect("a placement request of the frame's writer")
    };
    let (eligible_request, revoked_request) = (of(&eligible), of(&hidden));
    let page = |request| {
        let (writer, credentials) = (writer.clone(), credentials.clone());
        let reader = reader_key.public_key();
        async move {
            writer
                .placement_page(reader, &credentials, request)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        page(*revoked_request).await.headers.len(),
        1,
        "a domain member sees a partition it does not hold by its keyed scope"
    );
    let grant = other
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .credentials
        .0[0]
        .id()
        .unwrap();
    writer
        .learn_revocations(&crate::tests::revoking(
            &f.publisher,
            f.schema.app_id(),
            &[],
            &[grant],
        ))
        .await
        .unwrap();
    let omitted = page(*revoked_request).await;
    assert!(
        omitted.headers.is_empty() && omitted.chains.is_empty(),
        "revoked candidates alone answer like an empty position: {omitted:?}"
    );
    assert_eq!(page(*eligible_request).await.headers.len(), 1);
    assert_eq!(
        reader
            .receive_placements(*revoked_request, omitted)
            .await
            .unwrap(),
        0
    );
}
