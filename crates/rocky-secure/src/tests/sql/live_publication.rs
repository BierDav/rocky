use super::*;
use crate::network::{PublicationOutcome, link};

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn publication_delivers_an_individual_operation_above_64_kib() {
    let f = Typed::new();
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = f
        .open(&dir.path().join("a.db"), &alice, f.full(), &[f.items])
        .await;
    let b = f
        .open(&dir.path().join("b.db"), &bob, f.full(), &[f.items])
        .await;
    let cursor = b.frontier(f.items).await.unwrap();
    // The signed SQL intent also carries its encoded parameter bytes.
    let title = "x".repeat(36 * 1024);
    insert(
        &sql(&a, &[f.items]).await,
        "INSERT INTO items VALUES('large',?,0)",
        vec![text(&title)],
    )
    .await
    .unwrap();
    let frames = a
        .range_page(
            bob.public_key(),
            &f.credentials(&bob, f.full()),
            &cursor,
            None,
            crate::network::MAX_PAGE_BYTES,
        )
        .await
        .unwrap()
        .frames;
    assert_eq!(frames.len(), 1);
    assert!(frames[0].to_bytes().unwrap().len() > link::FRAME_BYTES);
    let (near, far) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
    let (near, far) = link::testing::links(
        near,
        far,
        (&b, bob.clone(), f.credentials(&bob, f.full())),
        (&a, alice.clone(), f.credentials(&alice, f.full())),
    )
    .await;
    let (near, far) = (near.unwrap(), far.unwrap());
    let handle = far.channels.handle.clone();
    let (send, mut incoming) = tokio::sync::mpsc::channel(4);
    let left = rocky_platform::task::spawn(far.drive(tokio::sync::mpsc::channel(1).0));
    let right = rocky_platform::task::spawn(near.drive(send));
    let (outcome, receipt) = tokio::join!(
        async {
            let outcome = handle.publish_committed(&a, &[cursor]).await.unwrap();
            assert!(matches!(outcome, PublicationOutcome::Published(_)));
            outcome
        },
        async {
            let mut session = incoming.recv().await.unwrap();
            let receipt = session.receive_publication(&b).await.unwrap();
            session.close().await.unwrap();
            receipt
        }
    );
    assert!(matches!(outcome, PublicationOutcome::Published(_)));
    assert!(receipt.repair.is_empty());
    assert_eq!(
        select(&sql(&b, &[f.items]).await, "SELECT title FROM items").await,
        vec![vec![text(&title)]]
    );
    let rejected = insert(
        &sql(&a, &[f.items]).await,
        "INSERT INTO items VALUES('unsupported',?,0)",
        vec![text(&"x".repeat(70 * 1024))],
    )
    .await;
    assert!(matches!(
        rejected,
        Err(Error::Quota {
            resource: "intent frame bytes",
            ..
        })
    ));
    assert_eq!(
        select(&sql(&a, &[f.items]).await, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(1)]]
    );
    left.abort();
    right.abort();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_over_budget_publication_sends_nothing_and_native_repair_recovers_large_items() {
    let f = Typed::new();
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = f
        .open(&dir.path().join("a.db"), &alice, f.full(), &[f.items])
        .await;
    let b = f
        .open(&dir.path().join("b.db"), &bob, f.full(), &[f.items])
        .await;
    let cursor = b.frontier(f.items).await.unwrap();
    let writer = sql(&a, &[f.items]).await;
    for n in 0..4 {
        insert(
            &writer,
            "INSERT INTO items VALUES(?,?,0)",
            vec![text(&format!("{n}")), text(&"x".repeat(36 * 1024))],
        )
        .await
        .unwrap();
    }
    let (near, far) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
    let (near, far) = link::testing::links(
        near,
        far,
        (&b, bob.clone(), f.credentials(&bob, f.full())),
        (&a, alice.clone(), f.credentials(&alice, f.full())),
    )
    .await;
    let (near, mut far) = (near.unwrap(), far.unwrap());
    let handle = far.channels.handle.clone();
    assert!(matches!(
        handle.publish_committed(&a, &[cursor]).await.unwrap(),
        PublicationOutcome::Repair { .. }
    ));
    assert!(far.channels.outgoing.try_recv().is_err());
    assert!(b.frontier(f.items).await.unwrap().logs.is_empty());
    let (send, mut incoming) = tokio::sync::mpsc::channel(4);
    let left = rocky_platform::task::spawn(far.drive(tokio::sync::mpsc::channel(1).0));
    let right = rocky_platform::task::spawn(near.drive(send));
    tokio::join!(
        async {
            let mut session = handle.open().await.unwrap();
            let report = session
                .push(&a, f.items, crate::network::MAX_PAGE_BYTES)
                .await
                .unwrap();
            assert_eq!(report.frames, 4);
            assert!(!report.behind && !report.exhausted);
            session.close().await.unwrap();
        },
        async {
            let mut session = incoming.recv().await.unwrap();
            session.serve(&b).await.unwrap();
            session.close().await.unwrap();
        }
    );
    assert_eq!(
        select(&sql(&b, &[f.items]).await, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(4)]]
    );
    left.abort();
    right.abort();
}
