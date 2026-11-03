//! The frame rule under a learned revocation set, and the statements a
//! handshake exchanges.
use super::*;
use crate::network::link::Link;

pub(super) async fn open(
    f: &Fixture,
    dir: &Scratch,
    name: &str,
    node: &PrivateKey,
) -> EncryptedReplica<Employees> {
    EncryptedReplica::open(
        dir.path().join(name),
        f.app(),
        f.config(node, f.full(), &[f.public]),
    )
    .await
    .unwrap()
}
pub(super) async fn insert(
    replica: &EncryptedReplica<Employees>,
    f: &Fixture,
    row: u8,
    name: &str,
) -> EncryptedFrame {
    replica
        .author(
            [row; 32],
            vec![change(f.public, row, None, Some(f.app().name(name)))],
            vec![],
        )
        .await
        .unwrap()
}
async fn names(replica: &EncryptedReplica<Employees>, f: &Fixture) -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = replica
        .snapshot(f.public)
        .await
        .unwrap()
        .rows
        .values()
        .filter_map(|row| row.get(&f.app().field("display_name")).cloned())
        .collect();
    names.sort();
    names
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_learned_revocation_refuses_the_revoked_author_and_admits_every_other() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, carol, dave) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "alice.db", &alice).await;
    let c = open(&f, &dir, "carol.db", &carol).await;
    let d = open(&f, &dir, "dave.db", &dave).await;
    // Both write before the revocation; Dave receives neither yet.
    let early_alice = insert(&a, &f, 1, "alice early").await;
    let early_carol = insert(&c, &f, 2, "carol early").await;

    let revoked = revoking(
        &f.publisher,
        f.app().definition().app,
        &[carol.public_key()],
        &[],
    );
    assert_eq!(d.learn_revocations(&revoked).await.unwrap().len(), 1);
    // A frame Dave lacked from another author, signed before the
    // revocation, is admitted; the statement's time plays no part.
    assert_eq!(
        d.receive(early_alice).await.unwrap(),
        ReceiveStatus::Applied
    );
    // Every frame of the revoked author is refused, the earlier one too:
    // its rows arrive with a checkpoint.
    assert!(d.receive(early_carol).await.is_err());
    let late_carol = insert(&c, &f, 3, "carol late").await;
    assert!(d.receive(late_carol).await.is_err());
    let late_alice = insert(&a, &f, 4, "alice late").await;
    assert_eq!(d.receive(late_alice).await.unwrap(), ReceiveStatus::Applied);
    assert_eq!(
        names(&d, &f).await,
        [b"alice early".to_vec(), b"alice late".to_vec()]
    );
    // The set survives a reopen of the file without being supplied again.
    drop(d);
    let d = open(&f, &dir, "dave.db", &dave).await;
    assert_eq!(d.revocations().await, revoked);
}

/// `right` dials `left` over an in-memory pipe; returns both sides' results.
pub(super) async fn hello(
    (left_key, left): (&PrivateKey, &EncryptedReplica<Employees>),
    (right_key, right): (&PrivateKey, &EncryptedReplica<Employees>),
    f: &Fixture,
) -> (Result<Link, Error>, Result<Link, Error>) {
    let (near, far) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
    let (dialed, accepted) = crate::network::link::testing::links(
        near,
        far,
        (right, right_key.clone(), f.credentials(right_key, f.full())),
        (left, left_key.clone(), f.credentials(left_key, f.full())),
    )
    .await;
    (accepted, dialed)
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn statements_travel_inside_the_handshake_and_both_sides_end_with_the_union() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (xavier, yara) = (PrivateKey::new(), PrivateKey::new());
    let x = open(&f, &dir, "x.db", &xavier).await;
    let y = open(&f, &dir, "y.db", &yara).await;
    let app = f.app().definition().app;
    let first = revoking(&f.publisher, app, &[PrivateKey::new().public_key()], &[]);
    let second = revoking(&f.publisher, app, &[PrivateKey::new().public_key()], &[]);
    x.learn_revocations(&first).await.unwrap();
    y.learn_revocations(&second).await.unwrap();
    assert_ne!(
        x.revocation_digest().await.unwrap(),
        y.revocation_digest().await.unwrap()
    );
    // Each side sends the statement the other lacks; neither refuses.
    insert(&x, &f, 1, "after").await;
    let (left, right) = hello((&xavier, &x), (&yara, &y), &f).await;
    let (left, right) = (left.unwrap(), right.unwrap());
    let union = x.revocations().await;
    assert_eq!(union.len(), 2);
    assert_eq!(y.revocations().await, union);
    // Rows flow over the same link.
    let (mut client, mut server) = crate::network::link::testing::requests(right, left)
        .await
        .unwrap();
    let (served, pulled) = tokio::join!(
        async {
            server.serve_once(&x).await.unwrap();
            server.serve_once(&x).await.unwrap();
        },
        client.catch_up(&y, f.public, MAX_SECURE_FRAME / 2)
    );
    let () = served;
    pulled.unwrap();
    assert_eq!(names(&y, &f).await, [b"after".to_vec()]);
}

/// Carol read Alice's frame and names it as a head; Alice is revoked.
/// Revoked frames are never served, so Dave, who never received Alice's
/// frame, names the head his pending frame waits on, and Carol answers with
/// its signed header and chain. Dave then decides as Carol does, once his
/// own revocation set ends that chain.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_revoked_head_resolves_by_evidence_from_a_peer_that_holds_it() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, carol, dave, erin) = (
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    );
    let a = open(&f, &dir, "alice.db", &alice).await;
    let c = open(&f, &dir, "carol.db", &carol).await;
    let d = open(&f, &dir, "dave.db", &dave).await;
    let e = open(&f, &dir, "erin.db", &erin).await;
    let read = insert(&a, &f, 1, "alice").await;
    let unnamed = insert(&a, &f, 3, "alice later").await;
    for frame in [&read, &unnamed] {
        assert_eq!(
            c.receive(frame.clone()).await.unwrap(),
            ReceiveStatus::Applied
        );
    }
    let head = read.id().unwrap();
    let write = c
        .author(
            [2; 32],
            vec![change(f.public, 2, None, Some(f.app().name("carol")))],
            vec![head],
        )
        .await
        .unwrap();
    let revoked = revoking(
        &f.publisher,
        f.app().definition().app,
        &[alice.public_key()],
        &[],
    );
    let ask = |key: &PrivateKey, heads: Vec<Hash>| {
        let credentials = f.credentials(key, f.full());
        let (c, key) = (&c, key.public_key());
        async move {
            c.revoked_heads(key, &credentials, f.public, &heads)
                .await
                .unwrap()
        }
    };

    // Carol knows of no revocation yet: she shows nothing.
    assert_eq!(
        d.receive(write.clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    assert!(ask(&dave, vec![head]).await.is_empty());
    c.learn_revocations(&revoked).await.unwrap();
    // Dave asks only once he knows of a revocation himself.
    assert!(d.missing_heads(f.public).await.unwrap().is_empty());
    d.learn_revocations(&revoked).await.unwrap();
    let missing = d.missing_heads(f.public).await.unwrap();
    assert_eq!(missing, [head]);

    // Evidence for a head Dave does not wait on changes nothing.
    let other = ask(&dave, vec![unnamed.id().unwrap()]).await;
    assert_eq!(other.len(), 1);
    d.receive_revoked_heads(f.public, other).await.unwrap();
    assert!(names(&d, &f).await.is_empty());

    let shown = ask(&dave, missing.clone()).await;
    assert_eq!(shown.len(), 1);
    d.receive_revoked_heads(f.public, shown.clone())
        .await
        .unwrap();
    assert_eq!(names(&d, &f).await, [b"carol".to_vec()]);
    assert!(d.missing_heads(f.public).await.unwrap().is_empty());

    // Erin keeps nothing while her revocation set ends no chain, and asks
    // nothing; once it does, the same evidence decides as for Dave.
    assert_eq!(
        e.receive(write).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    e.receive_revoked_heads(f.public, shown.clone())
        .await
        .unwrap();
    assert!(names(&e, &f).await.is_empty());
    e.learn_revocations(&revoked).await.unwrap();
    e.receive_revoked_heads(f.public, shown).await.unwrap();
    assert_eq!(names(&e, &f).await, [b"carol".to_vec()]);
}
