//! Owner resets on replicas: the fence keeps what the signer applied of an
//! ended writer and refuses the rest, whichever order frames and the reset
//! arrive in.
use super::revocations::{insert, open};
use super::*;
use rocky_credentials::{PeerId, Reset, RootDecision};

/// Signs, under one lock with `signer`'s fence, a reset keeping `keep`.
async fn reset(
    f: &Fixture,
    signer: &EncryptedReplica<Employees>,
    keep: &[&PrivateKey],
    follows: BTreeSet<Commitment>,
) -> SignedRevocation {
    try_reset(f, signer, keep, follows).await.unwrap()
}
async fn try_reset(
    f: &Fixture,
    signer: &EncryptedReplica<Employees>,
    keep: &[&PrivateKey],
    follows: BTreeSet<Commitment>,
) -> Result<SignedRevocation, Box<dyn std::error::Error>> {
    let keep: BTreeSet<PeerId> = keep
        .iter()
        .map(|key| PeerId::of(key.public_key()))
        .collect();
    let app = f.app().definition().app;
    signer
        .install_reset(&keep, |_, fence| {
            SignedRevocation::sign_in(
                Revocation {
                    app,
                    issuer: f.publisher.public_key(),
                    time: now().unwrap(),
                    nodes: BTreeSet::new(),
                    identities: BTreeSet::new(),
                    capabilities: BTreeSet::new(),
                    cut: Vec::new(),
                    mode: rocky_credentials::RevocationMode::Lost,
                    recovery: Some(RootDecision::Reset(Box::new(Reset {
                        follows,
                        keep: keep.clone(),
                        fence,
                    }))),
                },
                &f.publisher,
                &CredentialChain::default(),
                rocky_credentials::DatabaseContext {
                    publisher: f.publisher.public_key(),
                    scope: f.public.domain,
                },
            )
            .map_err(Box::<dyn std::error::Error>::from)
        })
        .await
}
fn set(statements: &[&SignedRevocation]) -> RevocationSet {
    RevocationSet::try_from(
        statements
            .iter()
            .map(|signed| (*signed).clone())
            .collect::<Vec<_>>(),
    )
    .unwrap()
}
async fn status(replica: &EncryptedReplica<Employees>, frame: &EncryptedFrame) -> ReceiveStatus {
    replica.receipt_status(frame.id().unwrap()).await.unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_reset_keeps_fenced_frames_and_refuses_hidden_and_later_ones_in_both_orders() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (owner, laptop, custodian, xavier, yara) = (
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    );
    let o = open(&f, &dir, "owner.db", &owner).await;
    let l = open(&f, &dir, "laptop.db", &laptop).await;
    let c = open(&f, &dir, "custodian.db", &custodian).await;
    let x = open(&f, &dir, "x.db", &xavier).await;
    let y = open(&f, &dir, "y.db", &yara).await;
    let seen = insert(&l, &f, 1, "laptop seen").await;
    let hidden = insert(&l, &f, 2, "laptop hidden").await;
    assert_eq!(
        o.receive(seen.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    let kept_observers_and_custodians = [&owner, &custodian, &xavier, &yara];
    let statement = reset(&f, &o, &kept_observers_and_custodians, BTreeSet::new()).await;
    let fence = &statement.statement.reset().unwrap().fence;
    assert_eq!(fence.len(), 1);
    assert_eq!(fence[0].logs[0].writer, laptop.public_key());
    assert_eq!(fence[0].logs[0].next, 1);
    let later = insert(&l, &f, 3, "laptop later").await;
    let kept = insert(&c, &f, 4, "custodian").await;
    let own = insert(&o, &f, 5, "owner").await;

    x.learn_revocations(&set(&[&statement])).await.unwrap();
    assert_eq!(
        x.receive(seen.clone()).await.unwrap(),
        ReceiveStatus::Applied,
        "learning the reset first, x keeps the write the owner applied"
    );
    assert!(
        x.receive(hidden.clone()).await.is_err(),
        "learning the reset first, x refuses the write hidden from the owner"
    );
    assert!(x.receive(later.clone()).await.is_err());
    assert_eq!(
        y.receive(hidden.clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    y.learn_revocations(&set(&[&statement])).await.unwrap();
    assert_eq!(
        y.receive(seen.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(
        status(&y, &hidden).await,
        ReceiveStatus::Rejected,
        "a hidden frame waiting on its predecessor is rejected once the reset arrives"
    );
    assert!(y.receive(later.clone()).await.is_err());
    assert!(
        o.receive(hidden.clone()).await.is_err(),
        "the signer refuses the fenced frames too"
    );
    for replica in [&o, &x, &y] {
        for frame in [&kept, &own] {
            let received = replica.receive(frame.clone()).await.unwrap();
            assert!(
                matches!(received, ReceiveStatus::Applied | ReceiveStatus::Duplicate),
                "survivors and custodians keep syncing: {received:?}"
            );
        }
    }
    let rows = |replica: EncryptedReplica<Employees>| {
        let public = f.public;
        let field = f.app().field("display_name");
        async move {
            let mut names: Vec<Vec<u8>> = replica
                .snapshot(public)
                .await
                .unwrap()
                .rows
                .values()
                .filter_map(|row| row.get(&field).cloned())
                .collect();
            names.sort();
            names
        }
    };
    let expected = vec![
        b"custodian".to_vec(),
        b"laptop seen".to_vec(),
        b"owner".to_vec(),
    ];
    assert_eq!(rows(x).await, expected);
    assert_eq!(rows(y).await, expected);
    assert_eq!(rows(o).await, expected);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn concurrent_resets_end_both_signers_and_a_following_reset_readmits() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob, custodian, xavier, fresh) = (
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    );
    let a = open(&f, &dir, "alice.db", &alice).await;
    let b = open(&f, &dir, "bob.db", &bob).await;
    let x = open(&f, &dir, "x.db", &xavier).await;
    let n = open(&f, &dir, "fresh.db", &fresh).await;
    let shared = insert(&a, &f, 1, "alice shared").await;
    assert_eq!(
        b.receive(shared.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    let by_alice = reset(&f, &a, &[&alice, &custodian, &xavier], BTreeSet::new()).await;
    let by_bob = reset(&f, &b, &[&bob, &custodian, &xavier], BTreeSet::new()).await;
    let alice_late = insert(&a, &f, 2, "alice late").await;
    let bob_late = insert(&b, &f, 3, "bob late").await;
    for order in [[&by_alice, &by_bob], [&by_bob, &by_alice]] {
        let held = set(&order);
        assert!(held.reset_ends(&PeerId::of(alice.public_key())));
        assert!(held.reset_ends(&PeerId::of(bob.public_key())));
        assert!(!held.reset_ends(&PeerId::of(custodian.public_key())));
    }
    x.learn_revocations(&set(&[&by_bob])).await.unwrap();
    x.learn_revocations(&set(&[&by_alice])).await.unwrap();
    assert_eq!(
        x.receive(shared.clone()).await.unwrap(),
        ReceiveStatus::Applied,
        "Bob's fence keeps what he applied of Alice"
    );
    assert!(
        x.receive(alice_late.clone()).await.is_err(),
        "nothing after Bob's fence stands"
    );
    assert!(x.receive(bob_late.clone()).await.is_err());
    assert_eq!(
        n.receive(shared.clone()).await.unwrap(),
        ReceiveStatus::Applied,
        "the owner's new node holds what it fences before it learns the resets that end it"
    );
    n.learn_revocations(&set(&[&by_alice, &by_bob]))
        .await
        .unwrap();
    let follows = n.revocations().await.reset_heads();
    assert_eq!(follows.len(), 2);
    let following_reset = reset(&f, &n, &[&fresh, &custodian, &xavier], follows).await;
    let first = insert(&n, &f, 4, "signed in again").await;
    assert!(
        x.receive(first.clone()).await.is_err(),
        "before the following reset, the new node's frame is refused and not stored"
    );
    x.learn_revocations(&set(&[&following_reset]))
        .await
        .unwrap();
    assert_eq!(
        x.receive(first.clone()).await.unwrap(),
        ReceiveStatus::Applied,
        "sent again after the following reset, the refused frame applies"
    );
    assert!(x.receive(alice_late).await.is_err());
    let c = open(&f, &dir, "custodian.db", &custodian).await;
    let second = insert(&n, &f, 5, "signed in again later").await;
    assert_eq!(
        c.receive(shared.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(
        c.receive(second.clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    c.learn_revocations(&set(&[&by_alice, &by_bob]))
        .await
        .unwrap();
    assert_eq!(
        status(&c, &second).await,
        ReceiveStatus::Rejected,
        "a frame stored before the resets that end its writer is rejected with them"
    );
    c.learn_revocations(&set(&[&following_reset]))
        .await
        .unwrap();
    assert_eq!(
        status(&c, &second).await,
        ReceiveStatus::PendingDependencies,
        "the following reset returns it to pending without a resend"
    );
    assert_eq!(
        c.receive(first.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(status(&c, &second).await, ReceiveStatus::Applied);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_replica_never_holds_any_reset_that_ends_it() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob, xavier) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "alice.db", &alice).await;
    let b = open(&f, &dir, "bob.db", &bob).await;
    let x = open(&f, &dir, "x.db", &xavier).await;
    let by_alice = reset(&f, &a, &[&alice], BTreeSet::new()).await;
    let by_bob = reset(&f, &b, &[&bob], BTreeSet::new()).await;
    let own = x
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .credentials
        .clone();
    let concurrent_resets_each_ending_x = vec![by_alice.clone(), by_bob.clone()];
    let revoking = x
        .learn_session_revocations(concurrent_resets_each_ending_x, &own)
        .await
        .unwrap();
    assert!(revoking.is_some());
    let held = x.revocations().await;
    for statement in [&by_alice, &by_bob] {
        assert!(held.get(&statement.id().unwrap()).is_none());
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn resets_too_large_to_share_a_frame_travel_in_the_handshake() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (xavier, yara) = (PrivateKey::new(), PrivateKey::new());
    let x = open(&f, &dir, "x.db", &xavier).await;
    let y = open(&f, &dir, "y.db", &yara).await;
    let keep: BTreeSet<PeerId> = [&xavier, &yara]
        .iter()
        .map(|key| PeerId::of(key.public_key()))
        .collect();
    let fencing_unseen_writers_beyond_one_frame = || {
        let mut logs: Vec<_> = (0..rocky_credentials::MAX_FENCE_LOGS)
            .map(|_| rocky_credentials::FenceLog {
                writer: PrivateKey::new().public_key(),
                next: 1,
            })
            .collect();
        logs.sort();
        SignedRevocation::sign_in(
            Revocation {
                app: f.app().definition().app,
                issuer: f.publisher.public_key(),
                time: now().unwrap(),
                nodes: BTreeSet::new(),
                identities: BTreeSet::new(),
                capabilities: BTreeSet::new(),
                cut: Vec::new(),
                mode: rocky_credentials::RevocationMode::Lost,
                recovery: Some(RootDecision::Reset(Box::new(Reset {
                    follows: BTreeSet::new(),
                    keep: keep.clone(),
                    fence: vec![rocky_credentials::FenceScope {
                        scope: f.public,
                        logs,
                    }],
                }))),
            },
            &f.publisher,
            &CredentialChain::default(),
            rocky_credentials::DatabaseContext {
                publisher: f.publisher.public_key(),
                scope: f.public.domain,
            },
        )
        .unwrap()
    };
    let resets = [
        fencing_unseen_writers_beyond_one_frame(),
        fencing_unseen_writers_beyond_one_frame(),
        fencing_unseen_writers_beyond_one_frame(),
    ];
    let bytes: usize = resets
        .iter()
        .map(|signed| signed.to_bytes().unwrap().len())
        .sum();
    assert!(bytes > MAX_SECURE_FRAME);
    x.learn_revocations(&set(&resets.iter().collect::<Vec<_>>()))
        .await
        .unwrap();
    let (left, right) = super::revocations::hello((&xavier, &x), (&yara, &y), &f).await;
    left.unwrap();
    right.unwrap();
    assert_eq!(y.revocations().await, x.revocations().await);
    assert_eq!(y.revocations().await.reset_heads().len(), 3);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_reset_that_would_end_its_signer_records_nothing() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (owner, custodian) = (PrivateKey::new(), PrivateKey::new());
    let o = open(&f, &dir, "owner.db", &owner).await;
    let held = o.revocations().await;
    let error = try_reset(&f, &o, &[&custodian], BTreeSet::new())
        .await
        .unwrap_err();
    assert!(error.is::<crate::ResetEndsSigner>(), "{error}");
    assert_eq!(o.revocations().await, held);
}
