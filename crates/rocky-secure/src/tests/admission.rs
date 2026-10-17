//! Admission by chain and revocation set, the statements a handshake
//! exchanges and the bound on unconfigured peers
//! (`docs/decisions/0051-roster-free-admission.md`).
use super::revocations::{hello, insert, open};
use super::*;
use crate::network::{
    PEER_REVOKED, REVOKED, Revoked,
    link::{Link, testing},
};
use rocky_credentials::DAY;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};

/// A stream that keeps every byte its owner read: the owner's transcript.
struct Recorded {
    stream: DuplexStream,
    read: Arc<Mutex<Vec<u8>>>,
}
impl AsyncRead for Recorded {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.stream).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = polled {
            self.read
                .lock()
                .unwrap()
                .extend_from_slice(&buf.filled()[before..]);
        }
        polled
    }
}
impl AsyncWrite for Recorded {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// `count` statements revoking distinct capability IDs, or `nodes` when given.
fn statements(f: &Fixture, count: usize, nodes: &[PublicKey]) -> RevocationSet {
    let time = now().unwrap();
    let signed: Vec<_> = (0..count)
        .map(|index| {
            let statement = Revocation {
                app: f.app().definition().app,
                issuer: f.publisher.public_key(),
                time,
                nodes: nodes
                    .get(index)
                    .copied()
                    .map(rocky_credentials::PeerId::of)
                    .into_iter()
                    .collect(),
                identities: BTreeSet::new(),
                capabilities: if nodes.get(index).is_none() {
                    [rocky_core::commitment(
                        "admission.fixture",
                        &index.to_be_bytes(),
                    )]
                    .into()
                } else {
                    BTreeSet::new()
                },
            };
            SignedRevocation::sign(
                statement,
                &f.publisher,
                &rocky_credentials::CredentialChain::default(),
            )
            .unwrap()
        })
        .collect();
    RevocationSet::try_from(signed).unwrap()
}

#[tokio::test]
async fn foreign_expired_and_self_signed_chains_are_refused_before_anything_is_recorded() {
    let (f, other) = (Fixture::new(), Fixture::new());
    let dir = TempDir::new().unwrap();
    let (alice, bob, stranger) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "a.db", &alice).await;
    let b = open(&f, &dir, "b.db", &bob).await;
    let time = now().unwrap();
    let refused = [
        ("another domain", other.credentials(&stranger, other.full())),
        (
            "expired",
            f.chain(
                &stranger,
                f.full(),
                &f.publisher,
                (time - 10 * DAY, time - DAY),
            ),
        ),
        (
            "self-signed",
            f.chain(&stranger, f.full(), &stranger, lifetime()),
        ),
    ];
    for (case, chain) in refused {
        let (near, far) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
        let read = Arc::new(Mutex::new(Vec::new()));
        let near = Recorded {
            stream: near,
            read: read.clone(),
        };
        let (dialed, accepted) = testing::links(
            near,
            far,
            (&b, stranger.clone(), chain),
            (&a, alice.clone(), f.credentials(&alice, f.full())),
        )
        .await;
        assert!(
            matches!(accepted, Err(Error::Rejected(_))),
            "{case}: {:?}",
            accepted.err()
        );
        assert!(dialed.is_err(), "{case}");
        // The acceptor said nothing: the stranger saw neither its hello nor its chain.
        assert!(read.lock().unwrap().is_empty(), "{case}");
        assert_eq!(a.recorded_peers().await, 0, "{case}");
    }
    // A member is recorded once it is served.
    insert(&a, &f, 1, "served").await;
    let (left, right) = hello((&alice, &a), (&bob, &b), &f).await;
    let (mut client, mut server) = testing::requests(right.unwrap(), left.unwrap())
        .await
        .unwrap();
    let (served, pulled) = tokio::join!(server.serve_once(&a), client.peer_frontier(&b, f.public));
    served.unwrap();
    pulled.unwrap();
    assert_eq!(a.recorded_peers().await, 0);
    assert_eq!(b.recorded_peers().await, 1);
}

#[tokio::test]
async fn a_revoked_peer_learns_its_statement_and_both_sides_end_the_session() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, dave, erin) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "a.db", &alice).await;
    let d = open(&f, &dir, "d.db", &dave).await;
    let e = open(&f, &dir, "e.db", &erin).await;
    let revoked = statements(&f, 2, &[dave.public_key(), erin.public_key()]);
    a.learn_revocations(&revoked).await.unwrap();
    let rejected = |result: Result<Link, Error>| match result {
        Err(Error::Rejected(message)) => message,
        other => panic!("expected a refusal, got {:?}", other.err()),
    };
    // The revoked side ends with the statement that revokes it, which its
    // replica does not keep.
    let told = |result: Result<Link, Error>, node: &PrivateKey| match result {
        Err(Error::Application(error)) => {
            let Revoked(statement) = error.downcast_ref::<Revoked>().unwrap();
            assert!(
                statement
                    .statement
                    .nodes
                    .contains(&rocky_credentials::PeerId::of(node.public_key()))
            );
            assert_eq!(error.to_string(), REVOKED);
        }
        other => panic!("expected the revocation, got {:?}", other.err()),
    };
    // Dave dials Alice: Alice refuses him, and he leaves knowing why.
    let (alice_side, dave_side) = hello((&alice, &a), (&dave, &d), &f).await;
    assert_eq!(rejected(alice_side), PEER_REVOKED);
    told(dave_side, &dave);
    // Dave keeps Erin's statement, not his own.
    let held = d.revocations().await;
    assert_eq!(held.len(), 1);
    assert!(held.revokes(&erin.public_key()));
    // Alice dials Erin, who accepts: the same, with the sides swapped.
    let (erin_side, alice_side) = hello((&erin, &e), (&alice, &a), &f).await;
    told(erin_side, &erin);
    assert_eq!(rejected(alice_side), PEER_REVOKED);
    assert!(e.revocations().await.revokes(&dave.public_key()));
    assert_eq!(a.recorded_peers().await, 0);
    // Both still open: a replica never holds the statement revoking itself.
    drop((d, e));
    open(&f, &dir, "d.db", &dave).await;
    open(&f, &dir, "e.db", &erin).await;
}

#[tokio::test]
async fn another_session_version_is_refused_with_the_authentication_error() {
    // The first version and the parent format, `rocky.link.v10`.
    for version in [1, 10] {
        refused_session_version(version).await;
    }
}
async fn refused_session_version(version: u32) {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "a.db", &alice).await;
    let b = open(&f, &dir, "b.db", &bob).await;
    let (near, far) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
    let (accepted, dialed) = tokio::join!(
        Link::open(
            far,
            &a,
            alice.clone(),
            f.credentials(&alice, f.full()),
            None
        ),
        Link::open_as_version(
            near,
            &b,
            (bob.clone(), f.credentials(&bob, f.full())),
            Some(alice.public_key()),
            version,
        )
    );
    match accepted {
        Err(Error::Rejected(message)) => {
            assert_eq!(message, "link authentication failed");
        }
        other => panic!("expected the authentication error, got {:?}", other.err()),
    }
    assert!(dialed.is_err());
}

#[tokio::test]
async fn a_full_set_costs_two_digests_when_equal_and_fits_its_pages_when_missing() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob, carol) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "a.db", &alice).await;
    let b = open(&f, &dir, "b.db", &bob).await;
    let c = open(&f, &dir, "c.db", &carol).await;
    let full = statements(&f, rocky_credentials::MAX_LIVE_REVOCATIONS, &[]);
    a.learn_revocations(&full).await.unwrap();
    b.learn_revocations(&full).await.unwrap();

    // Equal sets: the exchange after the hellos is one digest each way.
    let started = std::time::Instant::now();
    let (left, right) = hello((&alice, &a), (&bob, &b), &f).await;
    let elapsed = started.elapsed();
    let spent = right.unwrap().handshake_bytes;
    assert_eq!(left.unwrap().handshake_bytes, spent);
    eprintln!("1,024 equal statements: {spent} bytes after the hellos, handshake {elapsed:?}");
    assert!(spent < 1024, "{spent}");

    // 1,024 against none: every page fits a frame and the sets end equal.
    let started = std::time::Instant::now();
    let (left, right) = hello((&alice, &a), (&carol, &c), &f).await;
    let elapsed = started.elapsed();
    let spent = right.unwrap().handshake_bytes;
    left.unwrap();
    eprintln!("1,024 statements against none: {spent} bytes, handshake {elapsed:?}");
    assert_eq!(c.revocations().await, full);
    assert_eq!(
        c.revocation_digest().await.unwrap(),
        a.revocation_digest().await.unwrap()
    );
}

#[tokio::test]
async fn unconfigured_peers_stay_within_the_bound_and_the_least_recently_served_leave_first() {
    use crate::replica::MAX_UNCONFIGURED_PEERS;
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let alice = PrivateKey::new();
    let a = open(&f, &dir, "a.db", &alice).await;
    insert(&a, &f, 1, "held").await;
    let frontier = a.frontier(f.public).await.unwrap();
    let (configured, full_node) = (PrivateKey::new(), PrivateKey::new());
    a.configure_peers([configured.public_key()]).await;
    a.set_role(RoleConfig {
        full_nodes: [rocky_credentials::PeerId::of(full_node.public_key())].into(),
        ..RoleConfig::default()
    })
    .await
    .unwrap();
    let serve = async |key: &PrivateKey| {
        let chain = f.credentials(key, f.full());
        a.acknowledge_frontier(key.public_key(), &chain, &frontier)
            .await
            .unwrap();
    };
    serve(&configured).await;
    serve(&full_node).await;
    let strangers: Vec<PrivateKey> = (0..300).map(|_| PrivateKey::new()).collect();
    for stranger in &strangers {
        serve(stranger).await;
    }
    let kept = MAX_UNCONFIGURED_PEERS + 2;
    assert_eq!(a.recorded_peers().await, kept);
    // The two kept whatever the bound, and the most recent strangers, stay.
    let present = async |key: &PrivateKey| {
        let inner = a.inner.lock().await;
        let mut rows = inner
            .conn
            .query(
                "SELECT 1 FROM secure_peer_frontiers WHERE peer=?",
                [rocky_credentials::PeerId::of(key.public_key())
                    .as_bytes()
                    .to_vec()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().is_some()
    };
    assert!(present(&configured).await && present(&full_node).await);
    assert!(present(&strangers[299]).await && present(&strangers[300 - 256]).await);
    assert!(!present(&strangers[0]).await && !present(&strangers[300 - 257]).await);

    // Serving an existing peer refreshes its place before the next eviction.
    let oldest = strangers.len() - MAX_UNCONFIGURED_PEERS;
    serve(&strangers[oldest]).await;
    let newcomer = PrivateKey::new();
    serve(&newcomer).await;
    assert!(present(&strangers[oldest]).await && present(&newcomer).await);
    assert!(!present(&strangers[oldest + 1]).await);
    assert!(present(&configured).await && present(&full_node).await);
    assert_eq!(a.recorded_peers().await, kept);
}
