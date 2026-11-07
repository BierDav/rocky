use super::*;
#[test]
fn malformed_metadata_is_rejected_before_typed_allocation() {
    for bytes in [
        &[0x9b, 255, 255, 255, 255, 255, 255, 255, 255][..],
        &[0x5a, 0, 4, 0, 0],
        &[0x9f, 0xff],
    ] {
        assert!(charge(bytes, &mut 0).is_err());
    }
    let mut nested = vec![0x81; 34];
    nested.push(0);
    assert!(charge(&nested, &mut 0).is_err());
    let many = encode(&vec![0u8; 65536]).unwrap();
    assert!(charge(&many, &mut 0).is_err());
}
#[test]
fn signed_size_reserves_the_largest_sequence_width() {
    let message = finish(1, 1, [0; 32]);
    let bound = signed_size(&message).unwrap();
    for sequence in [0, 23, 24, 255, 256, 65535, 65536, u32::MAX as u64, u64::MAX] {
        let bytes = encode(&SignedMessage {
            sequence,
            bytes: encode(&message).unwrap(),
            signature: PrivateKey::from_bytes(&[1; 32]).sign(b"test"),
        })
        .unwrap();
        assert!(bytes.len() <= bound);
    }
}

#[test]
fn publication_scans_embedded_row_metadata_before_expansion() {
    #[derive(Serialize)]
    struct Metadata {
        origins: Vec<serde_bytes::ByteBuf>,
        headers: std::collections::BTreeMap<serde_bytes::ByteArray<32>, serde_bytes::ByteBuf>,
        bindings: std::collections::BTreeMap<serde_bytes::ByteArray<32>, serde_bytes::ByteBuf>,
    }
    let nested = vec![0x9b, 255, 255, 255, 255, 255, 255, 255, 255];
    let body = encode(&Metadata {
        origins: vec![nested.into()],
        headers: Default::default(),
        bindings: Default::default(),
    })
    .unwrap();
    let envelope = crate::wire::RowPayload::witness(body, vec![]).unwrap();
    assert!(crate::wire::RowPayload::charge_publication(&envelope, &mut 0).is_err());
}

use crate::tests::{Fixture, live_publication::pair, revocations};
use employees_app::Employees;
use rocky_core::RowWrite;
use rocky_platform::fs::Scratch;
type Replica = EncryptedReplica<Employees>;

fn reseal(messages: &mut Vec<Message>) {
    let Message::Publication(PublicationMessage::Start { upper, .. }) = messages.remove(0) else {
        panic!("start");
    };
    messages.pop();
    let mut hash = [0; 32];
    let mut items = 0;
    for message in messages.iter() {
        hash = digest(hash, message).unwrap();
        if let Message::Publication(PublicationMessage::Page { frames, voids, .. }) = message {
            items += frames.len() + voids.len();
        }
    }
    let parts = messages.len();
    messages.insert(
        0,
        Message::Publication(PublicationMessage::Start {
            upper,
            parts,
            items,
            digest: hash,
        }),
    );
    messages.push(finish(parts, items, hash));
}

async fn exchange(
    f: &Fixture,
    a: &Replica,
    b: &Replica,
    alice: &PrivateKey,
    bob: &PrivateKey,
    cursors: &[ScopeFrontier],
    edit: impl FnOnce(&mut Vec<Message>),
) -> Result<PublicationReceipt, Error> {
    let (near, far) = revocations::hello((alice, a), (bob, b), f).await;
    let (near, far) = (near.unwrap(), far.unwrap());
    let handle = near.channels.handle.clone();
    let (peer, credentials, _) = handle.publication_peer();
    let mut plan = Plan::build(a, peer, credentials, cursors).await.unwrap();
    edit(&mut plan.messages);
    let (send, mut incoming) = tokio::sync::mpsc::channel(4);
    let left = rocky_platform::task::spawn(near.drive(tokio::sync::mpsc::channel(1).0));
    let right = rocky_platform::task::spawn(far.drive(send));
    let (_, received) = tokio::join!(
        async {
            let mut session = handle.open_publication(MAX_BYTES).await.unwrap();
            for message in plan.messages {
                if session.send(message).await.is_err() {
                    return;
                }
            }
            // EOF deliberately exercises a missing terminal request as well.
            let _ = session.close().await;
        },
        async {
            let mut session = incoming.recv().await.unwrap();
            let result = session.receive_publication(b).await;
            let _ = session.close().await;
            result
        }
    );
    left.abort();
    right.abort();
    received
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn terminal_barrier_keeps_a_missing_sibling_pending_after_a_successful_frame() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    let cursors = [
        b.frontier(f.public).await.unwrap(),
        b.frontier(f.private).await.unwrap(),
    ];
    a.author_bundle(
        [21; 32],
        vec![
            RowWrite::changed(crate::tests::change(
                f.public,
                1,
                None,
                Some(f.app().name("bundle")),
            )),
            RowWrite::changed(crate::tests::change(
                f.private,
                1,
                None,
                Some(f.app().salary("123")),
            )),
        ],
        vec![],
    )
    .await
    .unwrap();
    revocations::insert(&a, &f, 2, "independent").await;
    let receipt = exchange(&f, &a, &b, &alice, &bob, &cursors, |messages| {
        for message in messages.iter_mut() {
            if let Message::Publication(PublicationMessage::Page { frames, .. }) = message {
                frames.retain(|frame| frame.scope().unwrap() != f.private);
            }
        }
        messages.retain(|m| !matches!(m, Message::Publication(PublicationMessage::Page { frames, voids }) if frames.is_empty() && voids.is_empty()));
        reseal(messages);
    }).await.unwrap();
    assert!(receipt.repair.contains(&f.public));
    assert!(receipt.repair.contains(&f.private));
    assert!(
        receipt
            .reasons
            .contains(&PublicationRepairReason::PendingDependencies)
    );
    assert!(b.snapshot(f.private).await.unwrap().rows.is_empty());
    // Ordinary retry on a new publication supplies the missing sibling and settles the bundle.
    let outcome = crate::tests::live_publication::publish(&f, &a, &b, &alice, &bob, &cursors).await;
    assert!(outcome.1.repair.is_empty());
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 2);
    assert_eq!(b.snapshot(f.private).await.unwrap().rows.len(), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn withheld_chain_is_never_acknowledged_and_retry_recovers() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    let cursors = [b.frontier(f.public).await.unwrap()];
    revocations::insert(&a, &f, 1, "row").await;
    let receipt = exchange(&f, &a, &b, &alice, &bob, &cursors, |messages| {
        messages.retain(|m| !matches!(m, Message::Chains(_)));
        reseal(messages);
    })
    .await
    .unwrap();
    assert!(receipt.repair_all);
    assert!(
        receipt
            .reasons
            .contains(&PublicationRepairReason::MissingChain)
    );
    assert!(receipt.frontiers[0].logs.iter().all(|l| l.held == 0));
    assert!(b.snapshot(f.public).await.unwrap().rows.is_empty());
    let outcome = crate::tests::live_publication::publish(&f, &a, &b, &alice, &bob, &cursors).await;
    assert!(outcome.1.repair.is_empty());
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn terminal_rejects_incomplete_conflicting_and_duplicate_parts() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    let cursors = [b.frontier(f.public).await.unwrap()];
    revocations::insert(&a, &f, 1, "row").await;
    for fault in 0..4 {
        let result = exchange(&f, &a, &b, &alice, &bob, &cursors, |messages| match fault {
            0 => {
                messages.pop();
            }
            1 => {
                *messages.last_mut().unwrap() = finish(0, 0, [9; 32]);
            }
            2 => {
                let page = messages
                    .iter()
                    .find(|m| matches!(m, Message::Publication(PublicationMessage::Page { .. })))
                    .unwrap();
                let copy = decode(&encode(page).unwrap()).unwrap();
                messages.insert(messages.len() - 1, copy);
                reseal(messages);
            }
            _ => {
                messages.swap(1, 2);
            }
        })
        .await;
        assert!(result.is_err(), "fault {fault} accepted");
    }
    // A rejected terminal does not erase data already durably admitted.
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn missing_key_is_held_with_explicit_unknown_repair_until_key_installation() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    b.evict_key(f.public).await;
    let cursor = b.frontier(f.public).await.unwrap();
    revocations::insert(&a, &f, 1, "row").await;
    let receipt = exchange(&f, &a, &b, &alice, &bob, &[cursor], |_| {})
        .await
        .unwrap();
    assert!(receipt.repair_all);
    assert!(
        receipt
            .reasons
            .contains(&PublicationRepairReason::MissingKey)
    );
    assert!(receipt.frontiers[0].logs.iter().any(|l| l.held > l.applied));
    let repeated = exchange(&f, &a, &b, &alice, &bob, &receipt.frontiers, |_| {})
        .await
        .unwrap();
    assert!(repeated.repair_all);
    assert!(
        repeated
            .reasons
            .contains(&PublicationRepairReason::MissingKey)
    );
    b.install_key(KeyMaterial::new(f.public, 1, [11; 32]))
        .await
        .unwrap();
    b.settle().await.unwrap();
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_ready_bundle_notifies_before_finish_or_receipt() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    let cursors = [
        b.frontier(f.public).await.unwrap(),
        b.frontier(f.private).await.unwrap(),
    ];
    a.author_bundle(
        [61; 32],
        vec![
            RowWrite::changed(crate::tests::change(
                f.public,
                1,
                None,
                Some(f.app().name("contact")),
            )),
            RowWrite::changed(crate::tests::change(
                f.private,
                1,
                None,
                Some(f.app().salary("123")),
            )),
        ],
        vec![],
    )
    .await
    .unwrap();
    let (near, far) = revocations::hello((&alice, &a), (&bob, &b), &f).await;
    let (near, far) = (near.unwrap(), far.unwrap());
    let handle = near.channels.handle.clone();
    let (peer, chain, _) = handle.publication_peer();
    let mut plan = Plan::build(&a, peer, chain, &cursors).await.unwrap();
    let terminal = plan.messages.pop().unwrap();
    let (send, mut incoming) = tokio::sync::mpsc::channel(4);
    let left = rocky_platform::task::spawn(near.drive(tokio::sync::mpsc::channel(1).0));
    let right = rocky_platform::task::spawn(far.drive(send));
    let (visible, barrier) = tokio::sync::oneshot::channel();
    let mut revision = b.observe_revision();
    revision.borrow_and_update();
    tokio::join!(
        async {
            let mut session = handle.open_publication(plan.bytes).await.unwrap();
            for message in plan.messages {
                session.send(message).await.unwrap();
            }
            barrier.await.unwrap();
            session.send(terminal).await.unwrap();
            let message = session.receive().await.unwrap();
            assert!(matches!(
                message,
                Message::Publication(PublicationMessage::Receipt(_))
            ));
            session.close().await.unwrap();
        },
        async {
            let mut session = incoming.recv().await.unwrap();
            let receipt = session.receive_publication(&b).await.unwrap();
            assert!(receipt.repair.is_empty());
            session.close().await.unwrap();
        },
        async {
            revision.changed().await.unwrap();
            assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 1);
            assert_eq!(b.snapshot(f.private).await.unwrap().rows.len(), 1);
            visible.send(()).unwrap();
        }
    );
    left.abort();
    right.abort();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn missing_history_is_repaired_even_when_a_later_independent_frame_applies() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    let cursors = [b.frontier(f.public).await.unwrap()];
    let first = revocations::insert(&a, &f, 1, "first").await.id().unwrap();
    revocations::insert(&a, &f, 2, "second").await;
    let carol = PrivateKey::new();
    let c = revocations::open(&f, &dir, "c.db", &carol).await;
    let independent = revocations::insert(&c, &f, 3, "independent").await;
    a.receive(independent).await.unwrap();
    let receipt = exchange(&f, &a, &b, &alice, &bob, &cursors, |messages| {
        for message in messages.iter_mut() {
            if let Message::Publication(PublicationMessage::Page { frames, .. }) = message {
                frames.retain(|f| f.id().unwrap() != first);
            }
        }
        reseal(messages);
    })
    .await
    .unwrap();
    assert_eq!(receipt.repair, vec![f.public]);
    assert!(
        receipt
            .reasons
            .contains(&PublicationRepairReason::MissingHistory)
    );
    assert!(
        receipt
            .reasons
            .contains(&PublicationRepairReason::PendingDependencies)
    );
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 1);
    let (_, receipt, _) =
        crate::tests::live_publication::publish(&f, &a, &b, &alice, &bob, &cursors).await;
    assert!(receipt.repair.is_empty());
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 3);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn publication_rechecks_membership_after_an_earlier_page_commits() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    let cursors = [
        b.frontier(f.public).await.unwrap(),
        b.frontier(f.private).await.unwrap(),
    ];
    revocations::insert(&a, &f, 1, "before revocation").await;
    a.author(
        [73; 32],
        vec![crate::tests::change(
            f.private,
            1,
            None,
            Some(f.app().salary("123")),
        )],
        vec![],
    )
    .await
    .unwrap();
    let (near, far) = revocations::hello((&alice, &a), (&bob, &b), &f).await;
    let (near, far) = (near.unwrap(), far.unwrap());
    let handle = near.channels.handle.clone();
    let (peer, chain, _) = handle.publication_peer();
    let mut plan = Plan::build(&a, peer, chain, &cursors).await.unwrap();
    let first = plan
        .messages
        .iter()
        .position(|m| matches!(m, Message::Publication(PublicationMessage::Page { .. })))
        .unwrap();
    let Message::Publication(PublicationMessage::Page { frames, .. }) = &mut plan.messages[first]
    else {
        unreachable!()
    };
    let first_scope = frames[0].scope().unwrap();
    let rest = frames.split_off(1);
    let unadmitted = rest[0].scope().unwrap();
    assert_ne!(first_scope, unadmitted);
    plan.messages.insert(
        first + 1,
        Message::Publication(PublicationMessage::Page {
            frames: rest,
            voids: vec![],
        }),
    );
    reseal(&mut plan.messages);
    plan.bytes = plan.messages.iter().map(|m| signed_size(m).unwrap()).sum();
    let tail = plan.messages.split_off(first + 1);
    let (send, mut incoming) = tokio::sync::mpsc::channel(4);
    let left = rocky_platform::task::spawn(near.drive(tokio::sync::mpsc::channel(1).0));
    let right = rocky_platform::task::spawn(far.drive(send));
    let (identity, peer_identity) = tokio::sync::oneshot::channel();
    let (changed, revoked) = tokio::sync::oneshot::channel();
    let mut revisions = b.observe_revision();
    revisions.borrow_and_update();
    tokio::join!(
        async {
            let mut session = handle.open_publication(plan.bytes).await.unwrap();
            for message in plan.messages {
                session.send(message).await.unwrap();
            }
            revoked.await.unwrap();
            for message in tail {
                if session.send(message).await.is_err() {
                    break;
                }
            }
            assert!(session.receive().await.is_err());
            let _ = session.close().await;
        },
        async {
            let mut session = incoming.recv().await.unwrap();
            identity
                .send(session.credentials.principal().unwrap())
                .unwrap();
            assert!(session.receive_publication(&b).await.is_err());
            let _ = session.close().await;
        },
        async {
            let principal = peer_identity.await.unwrap();
            revisions.changed().await.unwrap();
            let revoked =
                crate::tests::revoking(&f.publisher, f.public.domain.app, &[principal], &[]);
            b.learn_revocations(&revoked).await.unwrap();
            changed.send(()).unwrap();
        }
    );
    // The second scope was never admitted under the now-revoked membership.
    assert!(b.frontier(unadmitted).await.unwrap().logs.is_empty());
    left.abort();
    right.abort();
}
