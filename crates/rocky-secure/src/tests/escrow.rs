//! Root escrow records carried with frames: a receiver keeps only those a
//! key holder of their scope vouched for, so forged ones never displace a
//! genuine record and are counted against the peer that carried them.
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
