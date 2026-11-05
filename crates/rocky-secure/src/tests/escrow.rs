//! Root escrow records carried with frames: a receiver keeps only those a
//! key holder of their scope vouched for, one per issuer, so forged ones
//! never displace a genuine record and are counted against the peer that
//! carried them.
use super::*;
use crate::network::NativeSession;
use crate::wire::Chain;
use rocky_credentials::{RootEscrow, RootKey, VouchedEscrow, escrow_tag};

type Replica = EncryptedReplica<Employees>;

async fn open(f: &Fixture, dir: &Scratch, node: &PrivateKey) -> (Replica, Chain) {
    let config = f.config(node, f.full(), &[f.public]);
    let chain = Chain::new(config.credentials.clone()).unwrap();
    let path = dir.path().join(format!("{}.db", node.public_key()));
    (
        EncryptedReplica::open(path, f.app(), config).await.unwrap(),
        chain,
    )
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn forged_escrow_records_sent_first_never_displace_the_genuine_one() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let (a, a_chain) = open(&f, &dir, &alice).await;
    let (b, b_chain) = open(&f, &dir, &bob).await;
    a.author(
        [3; 32],
        vec![RowChange {
            scope: f.public,
            row: [3; 32],
            before: None,
            after: Some(f.app().name("row")),
        }],
        vec![],
    )
    .await
    .unwrap();
    let epoch = *a.key_epochs(f.public).await.last().unwrap();
    let slot = (f.public, epoch);
    // Alice holds the key and escrows it to the root.
    let root = RootKey::generate();
    let seal = |secret: [u8; 32]| {
        RootEscrow::seal(
            root.public_key(),
            escrow_tag(root.public_key(), &secret),
            &secret,
        )
        .unwrap()
    };
    let genuine = seal([7; 32]);
    a.keep_escrow(vec![(Some(slot), genuine.clone())])
        .await
        .unwrap();
    // Anyone can seal to the root and sign: four junk records, vouched for
    // by keys that hold no chain of their own, go first.
    let forged: Vec<VouchedEscrow> = (0..4)
        .map(|index| {
            VouchedEscrow::sign(
                &PrivateKey::new(),
                a_chain.credentials().clone(),
                slot,
                seal([index; 32]),
                0,
            )
            .unwrap()
        })
        .collect();
    let (dialed, accepted) = crate::network::link::testing::sessions(
        (&b, bob.clone(), b_chain.credentials().clone()),
        (&a, alice.clone(), a_chain.credentials().clone()),
    )
    .await
    .unwrap();
    let (mut server, mut client): (NativeSession, NativeSession) = (accepted, dialed);
    server.forge_escrow(forged);
    let (served, report) = tokio::join!(server.serve_to_end(&a), async {
        let report = client.catch_up(&b, f.public, MAX_SECURE_FRAME / 2).await;
        client.close().await.unwrap();
        report
    });
    served.unwrap();
    assert_eq!(report.unwrap().frames, 1);

    // Bob kept the genuine record alone; the root recovers the key from it.
    let kept = b.escrow_of(&[slot].into()).await.unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].signer(), alice.public_key());
    assert_eq!(kept[0].record(), &genuine);
    assert_eq!(kept[0].record().open(&root).unwrap(), [7; 32]);
    // The forgeries are refused and surfaced against the peer.
    let refusals = b.escrow_refusals().await.unwrap();
    assert_eq!(refusals.len(), 1);
    let (peer, records, reason) = &refusals[0];
    assert_eq!((*peer, *records), (alice.public_key(), 4));
    assert!(reason.contains("could not read"), "{reason}");
}

/// Records of `slot` vouched for by `devices` fresh devices that the
/// identity `issuer` enrolled with the full grants.
fn vouched_under(
    f: &Fixture,
    issuer: &PrivateKey,
    slot: (ResourceScope, rocky_core::KeyEpoch),
    devices: u8,
) -> Vec<VouchedEscrow> {
    let root = RootKey::generate();
    (0..devices)
        .map(|index| {
            let device = PrivateKey::new();
            let secret = [index; 32];
            VouchedEscrow::sign(
                &device,
                f.chain_under(&device, f.full(), &f.publisher, issuer, lifetime()),
                slot,
                RootEscrow::seal(
                    root.public_key(),
                    escrow_tag(root.public_key(), &secret),
                    &secret,
                )
                .unwrap(),
                now().unwrap(),
            )
            .unwrap()
        })
        .collect()
}

/// A compromised issuer's devices flood an epoch, then a holder under
/// another issuer carries its record.
async fn flooded(f: &Fixture, dir: &Scratch) -> (Replica, PrivateKey, PrivateKey, VouchedEscrow) {
    let (b, _) = open(f, dir, &PrivateKey::new()).await;
    let slot = (f.public, *b.key_epochs(f.public).await.last().unwrap());
    let (compromised, carrier) = (PrivateKey::new(), PrivateKey::new());
    let mut records = vouched_under(f, &compromised, slot, 33);
    let genuine = vouched_under(f, &PrivateKey::new(), slot, 1).remove(0);
    records.push(genuine.clone());
    b.keep_carried_escrow(carrier.public_key(), records)
        .await
        .unwrap();
    (b, compromised, carrier, genuine)
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_issuers_devices_fill_only_its_own_escrow_slot() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (b, _, _, genuine) = flooded(&f, &dir).await;
    let kept = b.escrow_of(&[genuine.slot()].into()).await.unwrap();
    // One record per issuer: the flood's latest and the genuine one.
    assert_eq!(kept.len(), 2);
    assert!(kept.contains(&genuine));
    assert!(b.escrow_refusals().await.unwrap().is_empty());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn revoking_an_issuer_drops_its_kept_escrow_records() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (b, compromised, carrier, genuine) = flooded(&f, &dir).await;
    let slot = genuine.slot();
    b.learn_revocations(&crate::tests::revoking(
        &f.publisher,
        f.app().definition().app,
        &[compromised.public_key()],
        &[],
    ))
    .await
    .unwrap();
    assert_eq!(b.escrow_of(&[slot].into()).await.unwrap(), vec![genuine]);
    // The drop is surfaced against the peer that carried the record.
    let refusals = b.escrow_refusals().await.unwrap();
    assert_eq!(refusals.len(), 1);
    let (peer, records, reason) = &refusals[0];
    assert_eq!((*peer, *records), (carrier.public_key(), 1));
    assert!(reason.contains("revocation"), "{reason}");
    // The revoked issuer's records are refused when carried again; a
    // holder under another issuer is kept.
    let later = vouched_under(&f, &PrivateKey::new(), slot, 1).remove(0);
    let mut records = vouched_under(&f, &compromised, slot, 2);
    records.push(later.clone());
    b.keep_carried_escrow(carrier.public_key(), records)
        .await
        .unwrap();
    let kept = b.escrow_of(&[slot].into()).await.unwrap();
    assert_eq!(kept.len(), 2);
    assert!(kept.contains(&later));
    assert_eq!(b.escrow_refusals().await.unwrap()[0].1, 3);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn revoking_an_epochs_only_holder_keeps_its_record() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (b, _) = open(&f, &dir, &PrivateKey::new()).await;
    let slot = (f.public, *b.key_epochs(f.public).await.last().unwrap());
    let lost = PrivateKey::new();
    let records = vouched_under(&f, &lost, slot, 1);
    b.keep_carried_escrow(PrivateKey::new().public_key(), records.clone())
        .await
        .unwrap();
    b.learn_revocations(&crate::tests::revoking(
        &f.publisher,
        f.app().definition().app,
        &[lost.public_key()],
        &[],
    ))
    .await
    .unwrap();
    // A lost holder's record is the epoch's only way back to the root.
    assert_eq!(b.escrow_of(&[slot].into()).await.unwrap(), records);
    assert!(b.escrow_refusals().await.unwrap().is_empty());
}
