use super::*;
use crate::network::{PublicationOutcome, PublicationReceipt};

type Replica = EncryptedReplica<Employees>;
pub(crate) async fn publish(
    f: &Fixture,
    a: &Replica,
    b: &Replica,
    alice: &PrivateKey,
    bob: &PrivateKey,
    cursors: &[ScopeFrontier],
) -> (PublicationOutcome, PublicationReceipt, usize) {
    let (near, far) = super::revocations::hello((alice, a), (bob, b), f).await;
    let (near, far) = (near.unwrap(), far.unwrap());
    let handle = near.channels.handle.clone();
    let (send, mut incoming) = tokio::sync::mpsc::channel(4);
    let left = rocky_platform::task::spawn(near.drive(tokio::sync::mpsc::channel(1).0));
    let right = rocky_platform::task::spawn(far.drive(send));
    let (outcome, (receipt, chain_bytes)) = tokio::join!(
        async {
            let outcome = handle.publish_committed(a, cursors).await.unwrap();
            assert!(matches!(outcome, PublicationOutcome::Published(_)));
            outcome
        },
        async {
            let mut session = incoming.recv().await.unwrap();
            assert!(session.is_publication());
            let receipt = session.receive_publication(b).await.unwrap();
            let bytes = session.carried_chain_bytes();
            session.close().await.unwrap();
            (receipt, bytes)
        }
    );
    left.abort();
    right.abort();
    (outcome, receipt, chain_bytes)
}
pub(crate) async fn pair(f: &Fixture, dir: &Scratch) -> (Replica, Replica, PrivateKey, PrivateKey) {
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let keys = [f.public, f.private];
    let a = EncryptedReplica::open(
        dir.path().join("a.db"),
        f.app(),
        f.config(&alice, f.full(), &keys),
    )
    .await
    .unwrap();
    let b = EncryptedReplica::open(
        dir.path().join("b.db"),
        f.app(),
        f.config(&bob, f.full(), &keys),
    )
    .await
    .unwrap();
    (a, b, alice, bob)
}
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn publication_shares_chains_across_pages_and_settles_a_cross_scope_bundle() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    let cursors = [
        b.frontier(f.public).await.unwrap(),
        b.frontier(f.private).await.unwrap(),
    ];
    for row in 1..=31 {
        super::revocations::insert(&a, &f, row, "preceding").await;
    }
    let mut writes: Vec<_> = (32..=64)
        .map(|row| RowWrite::changed(change(f.public, row, None, Some(f.app().name("bundle")))))
        .collect();
    writes.push(RowWrite::changed(change(
        f.private,
        1,
        None,
        Some(f.app().salary("123")),
    )));
    let frames = a.author_bundle([200; 32], writes, vec![]).await.unwrap();
    let chain_bytes = encode(frames[0].chain().unwrap()).unwrap().len();
    let (outcome, receipt, carried) = publish(&f, &a, &b, &alice, &bob, &cursors).await;
    assert!(matches!(outcome, PublicationOutcome::Published(_)));
    assert!(receipt.repair.is_empty());
    assert_eq!(carried, chain_bytes);
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 64);
    assert_eq!(b.snapshot(f.private).await.unwrap().rows.len(), 1);
    assert!(
        receipt
            .frontiers
            .iter()
            .all(|f| f.logs.iter().all(|l| l.held == l.applied))
    );
}
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn publication_preflight_sends_no_channel_when_a_bundle_scope_is_missing() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    a.author_bundle(
        [1; 32],
        vec![
            RowWrite::changed(change(f.public, 1, None, Some(f.app().name("bundle")))),
            RowWrite::changed(change(f.private, 1, None, Some(f.app().salary("123")))),
        ],
        vec![],
    )
    .await
    .unwrap();
    let (near, far) = super::revocations::hello((&alice, &a), (&bob, &b), &f).await;
    let mut near = near.unwrap();
    let _far = far.unwrap();
    let outcome = near
        .channels
        .handle
        .publish_committed(&a, &[b.frontier(f.public).await.unwrap()])
        .await
        .unwrap();
    assert!(matches!(outcome, PublicationOutcome::Repair { .. }));
    assert!(near.channels.outgoing.try_recv().is_err());
    assert!(b.snapshot(f.public).await.unwrap().rows.is_empty());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn publication_only_backup_keeps_root_escrow_for_key_recovery() {
    use rocky_credentials::{RootEscrow, RootKey, escrow_tag};
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (a, b, alice, bob) = pair(&f, &dir).await;
    let cursor = b.frontier(f.public).await.unwrap();
    super::revocations::insert(&a, &f, 1, "recover me").await;
    let epoch = *a.key_epochs(f.public).await.last().unwrap();
    let slot = (f.public, epoch);
    let root = RootKey::generate();
    let record = RootEscrow::seal(
        root.public_key(),
        escrow_tag(root.public_key(), &[11; 32]),
        &[11; 32],
    )
    .unwrap();
    a.keep_escrow(vec![(Some(slot), record.clone())])
        .await
        .unwrap();
    let (_, receipt, _) = publish(&f, &a, &b, &alice, &bob, &[cursor]).await;
    assert!(receipt.repair.is_empty());
    b.evict_key(f.public).await;
    let backup = dir.path().join("backup.db");
    b.backup_into(&backup).await.unwrap();
    let recovered_node = PrivateKey::new();
    let recovered = EncryptedReplica::open_restored(
        backup,
        f.app(),
        f.config(&recovered_node, f.full(), &[]),
        bob.public_key(),
    )
    .await
    .unwrap();
    let carried = recovered.escrow_of(&[slot].into()).await.unwrap();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].record(), &record);
    let secret = carried[0].record().open(&root).unwrap();
    recovered
        .install_key_epoch(KeyMaterial::at(slot.0, slot.1, secret.try_into().unwrap()))
        .await
        .unwrap();
    recovered.settle().await.unwrap();
    assert_eq!(recovered.snapshot(f.public).await.unwrap().rows.len(), 1);
}
