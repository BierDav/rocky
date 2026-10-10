//! The frame rule under a learned revocation set
//! (`docs/decisions/0049-mergeable-revocation-set.md`), and the statements
//! a handshake exchanges (`docs/decisions/0051-roster-free-admission.md`).
use super::*;
use crate::network::NativeSession;

pub(super) async fn open(
    f: &Fixture,
    dir: &TempDir,
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

#[tokio::test]
async fn a_learned_revocation_refuses_the_revoked_author_and_admits_every_other() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
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
) -> (Result<NativeSession, Error>, Result<NativeSession, Error>) {
    let (near, far) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
    tokio::join!(
        NativeSession::accept(
            far,
            left,
            left_key.clone(),
            f.credentials(left_key, f.full()),
            MAX_SECURE_BYTES,
        ),
        NativeSession::authenticate(
            near,
            right,
            right_key.clone(),
            f.credentials(right_key, f.full()),
            left_key.public_key(),
            MAX_SECURE_BYTES,
        )
    )
}

#[tokio::test]
async fn statements_travel_inside_the_handshake_and_both_sides_end_with_the_union() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
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
    let (mut server, mut client) = (left.unwrap(), right.unwrap());
    let union = x.revocations().await;
    assert_eq!(union.len(), 2);
    assert_eq!(y.revocations().await, union);
    // Rows flow in the same session.
    let (served, pulled) = tokio::join!(
        async {
            server.serve_once(&x).await.unwrap();
            server.serve_once(&x).await.unwrap();
        },
        client.pull(&y, vec![f.public], None, MAX_SECURE_FRAME / 2)
    );
    let () = served;
    pulled.unwrap();
    assert_eq!(names(&y, &f).await, [b"after".to_vec()]);
}
