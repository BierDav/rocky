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
                cut: Default::default(),
                mode: rocky_credentials::RevocationMode::Lost,
            };
            SignedRevocation::sign_in(
                statement,
                &f.publisher,
                &rocky_credentials::CredentialChain::default(),
                rocky_credentials::DatabaseContext {
                    publisher: f.publisher.public_key(),
                    scope: f.public.domain,
                },
            )
            .unwrap()
        })
        .collect();
    RevocationSet::try_from(signed).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn foreign_expired_and_self_signed_chains_are_refused_before_anything_is_recorded() {
    let (f, other) = (Fixture::new(), Fixture::new());
    let dir = Scratch::new().await;
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
        assert!(
            read.lock().unwrap().is_empty(),
            "the acceptor said nothing, so the stranger saw neither its hello nor its chain: {case}"
        );
        assert_eq!(a.recorded_peers().await, 0, "{case}");
    }
    insert(&a, &f, 1, "served").await;
    let (left, right) = hello((&alice, &a), (&bob, &b), &f).await;
    let (mut client, mut server) = testing::requests(right.unwrap(), left.unwrap())
        .await
        .unwrap();
    let (served, pulled) = tokio::join!(server.serve_once(&a), client.peer_frontier(&b, f.public));
    served.unwrap();
    pulled.unwrap();
    assert_eq!(a.recorded_peers().await, 0);
    assert_eq!(
        b.recorded_peers().await,
        1,
        "a member is recorded once it is served"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_address_is_a_hint_never_a_grant() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob, mallory) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let b = open(&f, &dir, "b.db", &bob).await;
    let m = open(&f, &dir, "m.db", &mallory).await;
    let (near, far) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
    let (dialed_alices_address, answered) = tokio::join!(
        Link::open(
            near,
            &b,
            bob.clone(),
            f.credentials(&bob, f.full()),
            Some(alice.public_key()),
        ),
        Link::open(
            far,
            &m,
            mallory.clone(),
            f.credentials(&mallory, f.full()),
            None
        ),
    );
    match dialed_alices_address {
        Err(Error::Rejected(message)) => assert_eq!(
            message, "unexpected link peer",
            "a credentialed member answering at the address held for alice is not alice"
        ),
        other => panic!("expected the peer refusal, got {:?}", other.err()),
    }
    assert!(answered.is_err());
    assert_eq!(
        b.admitted_tip(mallory.public_key()).await,
        None,
        "bob keeps no tip from a peer that answered at an address held for someone else"
    );
    let (near, far) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
    let (dialed_mallory, accepted) = testing::links(
        near,
        far,
        (&b, bob.clone(), f.credentials(&bob, f.full())),
        (&m, mallory.clone(), f.credentials(&mallory, f.full())),
    )
    .await;
    assert!(
        dialed_mallory.is_ok() && accepted.is_ok(),
        "the same stream admits mallory once bob dials for mallory"
    );
    assert!(b.admitted_tip(mallory.public_key()).await.is_some());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_revoked_peer_learns_its_statement_and_both_sides_end_the_session() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
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
    let told_with_the_revoking_statement =
        |result: Result<Link, Error>, node: &PrivateKey| match result {
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
    let (alice_side, dave_side) = hello((&alice, &a), (&dave, &d), &f).await;
    assert_eq!(rejected(alice_side), PEER_REVOKED);
    told_with_the_revoking_statement(dave_side, &dave);
    let held = d.revocations().await;
    assert_eq!(held.len(), 1);
    assert!(
        held.revokes(&erin.public_key()),
        "a revoked replica keeps others' statements, not its own"
    );
    let (erin_side, alice_side) = hello((&erin, &e), (&alice, &a), &f).await;
    told_with_the_revoking_statement(erin_side, &erin);
    assert_eq!(rejected(alice_side), PEER_REVOKED);
    assert!(e.revocations().await.revokes(&dave.public_key()));
    assert_eq!(a.recorded_peers().await, 0);
    drop((d, e));
    let reopens_without_the_statement_revoking_itself = [("d.db", &dave), ("e.db", &erin)];
    for (name, node) in reopens_without_the_statement_revoking_itself {
        open(&f, &dir, name, node).await;
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn another_session_version_is_refused_with_the_authentication_error() {
    let first_parent_format_and_tip_carrying_hello = [1, 10, 30];
    for version in first_parent_format_and_tip_carrying_hello {
        refused_session_version(version).await;
    }
}
async fn refused_session_version(version: u32) {
    let f = Fixture::new();
    let dir = Scratch::new().await;
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn full_revocation_sets_converge_when_equal_or_missing() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob, carol) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "a.db", &alice).await;
    let b = open(&f, &dir, "b.db", &bob).await;
    let c = open(&f, &dir, "c.db", &carol).await;
    let full = statements(&f, rocky_credentials::MAX_LIVE_REVOCATIONS, &[]);
    a.learn_revocations(&full).await.unwrap();
    b.learn_revocations(&full).await.unwrap();

    let started = web_time::Instant::now();
    let (left, right) = hello((&alice, &a), (&bob, &b), &f).await;
    let elapsed = started.elapsed();
    let spent = right.unwrap().handshake_bytes;
    assert_eq!(
        left.unwrap().handshake_bytes,
        spent,
        "with equal sets the exchange after the hellos is one digest each way"
    );
    eprintln!("1,024 equal statements: {spent} bytes after the hellos, handshake {elapsed:?}");

    let started = web_time::Instant::now();
    let (left, right) = hello((&alice, &a), (&carol, &c), &f).await;
    let elapsed = started.elapsed();
    let spent = right.unwrap().handshake_bytes;
    left.unwrap();
    eprintln!("1,024 statements against none: {spent} bytes, handshake {elapsed:?}");
    assert_eq!(
        c.revocations()
            .await
            .statements()
            .cloned()
            .collect::<Vec<_>>(),
        full.statements().cloned().collect::<Vec<_>>(),
        "against an empty set every page fits a frame and the sets end equal"
    );
    assert_eq!(
        c.revocation_digest().await.unwrap(),
        a.revocation_digest().await.unwrap()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn unconfigured_peers_stay_within_the_bound_and_the_least_recently_served_leave_first() {
    use crate::replica::MAX_UNCONFIGURED_PEERS;
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let alice = PrivateKey::new();
    let a = open(&f, &dir, "a.db", &alice).await;
    insert(&a, &f, 1, "held").await;
    let frontier = a.frontier(f.public).await.unwrap();
    let (configured, full_node) = (PrivateKey::new(), PrivateKey::new());
    a.configure_peers([configured.public_key()]).await;
    a.set_partition_custody(
        f.public,
        crate::PartitionCustody {
            peers: [rocky_credentials::PeerId::of(full_node.public_key())].into(),
            copies: 0,
        },
    )
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
    let present = async |key: &PrivateKey| {
        let inner = a.inner.lock(crate::replica::Class::Background).await;
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
    assert!(
        present(&configured).await && present(&full_node).await,
        "configured peers stay whatever the bound"
    );
    assert!(
        present(&strangers[299]).await && present(&strangers[300 - 256]).await,
        "the most recent strangers stay"
    );
    assert!(!present(&strangers[0]).await && !present(&strangers[300 - 257]).await);

    let oldest = strangers.len() - MAX_UNCONFIGURED_PEERS;
    serve(&strangers[oldest]).await;
    let newcomer = PrivateKey::new();
    serve(&newcomer).await;
    assert!(
        present(&strangers[oldest]).await && present(&newcomer).await,
        "serving an existing peer refreshes its place before the next eviction"
    );
    assert!(!present(&strangers[oldest + 1]).await);
    assert!(present(&configured).await && present(&full_node).await);
    assert_eq!(a.recorded_peers().await, kept);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_admission_storm_evicts_neither_kept_nor_recently_admitted_peer_tips() {
    use crate::replica::{MAX_REMEMBERED_PEERS, MAX_UNCONFIGURED_PEERS};
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let alice = PrivateKey::new();
    let a = open(&f, &dir, "a.db", &alice).await;
    let peer_id = |key: &PrivateKey| rocky_credentials::PeerId::of(key.public_key());
    let storm_beyond_any_earlier_bound = 5 * MAX_UNCONFIGURED_PEERS;
    let mut keys: Vec<PrivateKey> = (0..storm_beyond_any_earlier_bound + 3)
        .map(|_| PrivateKey::new())
        .collect();
    keys.sort_by_key(peer_id);
    let strangers = keys.split_off(3);
    let [configured, full_node, active] = <[PrivateKey; 3]>::try_from(keys).unwrap();
    a.configure_peers([configured.public_key()]).await;
    a.set_partition_custody(
        f.public,
        crate::PartitionCustody {
            peers: [peer_id(&full_node)].into(),
            copies: 0,
        },
    )
    .await
    .unwrap();
    let admit = async |key: &PrivateKey| {
        a.admit_peer(
            key.public_key(),
            &f.credentials(key, f.full()),
            rocky_core::TipId([7; 32]),
        )
        .await
        .unwrap();
    };
    for honest in [&configured, &full_node, &active] {
        admit(honest).await;
    }
    let readmitted_well_within_the_bound = MAX_UNCONFIGURED_PEERS / 4;
    for (index, stranger) in strangers.iter().enumerate() {
        admit(stranger).await;
        if index % readmitted_well_within_the_bound == 0 {
            admit(&active).await;
        }
    }
    let mut displaced = Vec::new();
    for (role, honest) in [
        ("configured", &configured),
        ("custody", &full_node),
        ("recently admitted", &active),
    ] {
        if a.admitted_tip(honest.public_key()).await.is_none() {
            displaced.push(role);
        }
    }
    assert!(
        displaced.is_empty(),
        "a storm of strangers displaced {displaced:?} peers despite their lowest identities"
    );
    assert_eq!(
        a.admitted_peers().await,
        MAX_REMEMBERED_PEERS + 2,
        "unconfigured tips stay within the bound beside the kept peers"
    );
    assert!(a.admitted_tip(strangers[0].public_key()).await.is_none());
    assert!(
        a.admitted_tip(strangers[strangers.len() - 1].public_key())
            .await
            .is_some()
    );
}

async fn held_by(replica: &EncryptedReplica<Employees>, key: &PrivateKey) -> Option<i64> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    let mut held = inner
        .conn
        .query(
            "SELECT held FROM secure_peer_frontiers WHERE peer=? ORDER BY held DESC LIMIT 1",
            [rocky_credentials::PeerId::of(key.public_key())
                .as_bytes()
                .to_vec()],
        )
        .await
        .unwrap();
    held.next()
        .await
        .unwrap()
        .map(|row| row.get::<i64>(0).unwrap())
}

async fn past_genesis(f: &Fixture, replica: &EncryptedReplica<Employees>) {
    super::tips::main(f, replica).await;
    super::tips::advance(f, replica).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_peer_acknowledging_frontiers_keeps_its_tip_through_an_admission_storm() {
    use crate::replica::MAX_UNCONFIGURED_PEERS;
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let a = open(&f, &dir, "a.db", &PrivateKey::new()).await;
    past_genesis(&f, &a).await;
    insert(&a, &f, 1, "held").await;
    let tip = a.followed_tip().await;
    let acknowledge = async |key: &PrivateKey| {
        let frontier = a.frontier(f.public).await.unwrap();
        a.acknowledge_frontier(key.public_key(), &f.credentials(key, f.full()), &frontier)
            .await
            .unwrap();
    };
    let admit = async |key: &PrivateKey| {
        a.admit_peer(key.public_key(), &f.credentials(key, f.full()), tip)
            .await
            .unwrap();
    };
    let unadmitted = PrivateKey::new();
    acknowledge(&unadmitted).await;
    assert_eq!(
        held_by(&a, &unadmitted).await,
        None,
        "an acknowledgment counts only from a peer whose tip is known"
    );
    let active = PrivateKey::new();
    admit(&active).await;
    acknowledge(&active).await;
    let acknowledging_well_within_the_bound = MAX_UNCONFIGURED_PEERS / 16;
    for stranger in 1..=5 * MAX_UNCONFIGURED_PEERS {
        admit(&PrivateKey::new()).await;
        if stranger % acknowledging_well_within_the_bound == 0 {
            acknowledge(&active).await;
        }
    }
    assert_eq!(a.admitted_tip(active.public_key()).await, Some(tip));
    insert(&a, &f, 2, "later").await;
    acknowledge(&active).await;
    assert_eq!(
        held_by(&a, &active).await,
        Some(2),
        "a peer acknowledging throughout its session is still heard"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn admit_only_strangers_displace_an_idle_tip_only_past_the_bound_and_an_acknowledging_one_never()
 {
    use crate::replica::MAX_REMEMBERED_PEERS;
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let a = open(&f, &dir, "a.db", &PrivateKey::new()).await;
    past_genesis(&f, &a).await;
    insert(&a, &f, 1, "held").await;
    let tip = a.followed_tip().await;
    let admit = async |key: &PrivateKey| {
        a.admit_peer(key.public_key(), &f.credentials(key, f.full()), tip)
            .await
            .unwrap();
    };
    let (idle, acknowledged_once) = (PrivateKey::new(), PrivateKey::new());
    admit(&idle).await;
    admit(&acknowledged_once).await;
    let frontier = a.frontier(f.public).await.unwrap();
    a.acknowledge_frontier(
        acknowledged_once.public_key(),
        &f.credentials(&acknowledged_once, f.full()),
        &frontier,
    )
    .await
    .unwrap();
    for _ in 0..MAX_REMEMBERED_PEERS - 2 {
        admit(&PrivateKey::new()).await;
    }
    assert_eq!(
        a.admitted_tip(idle.public_key()).await,
        Some(tip),
        "an idle peer keeps its tip while the remembered peers fit the bound"
    );
    for _ in 0..2 * MAX_REMEMBERED_PEERS {
        admit(&PrivateKey::new()).await;
    }
    assert_eq!(a.admitted_tip(idle.public_key()).await, None);
    assert_eq!(
        a.admitted_tip(acknowledged_once.public_key()).await,
        Some(tip),
        "strangers that only ever admit cannot displace a peer that has acknowledged"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn peers_that_acknowledged_once_hold_no_seat_against_a_newcomer_or_a_later_acknowledger() {
    use crate::replica::MAX_REMEMBERED_PEERS;
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let a = open(&f, &dir, "a.db", &PrivateKey::new()).await;
    past_genesis(&f, &a).await;
    insert(&a, &f, 1, "held").await;
    let tip = a.followed_tip().await;
    let frontier = a.frontier(f.public).await.unwrap();
    let admit = async |key: &PrivateKey| {
        a.admit_peer(key.public_key(), &f.credentials(key, f.full()), tip)
            .await
            .unwrap();
    };
    let acknowledge = async |key: &PrivateKey| {
        a.acknowledge_frontier(key.public_key(), &f.credentials(key, f.full()), &frontier)
            .await
            .unwrap();
    };
    let squatters: Vec<PrivateKey> = (1..MAX_REMEMBERED_PEERS)
        .map(|_| PrivateKey::new())
        .collect();
    for squatter in &squatters {
        admit(squatter).await;
        acknowledge(squatter).await;
    }
    let honest = PrivateKey::new();
    admit(&honest).await;
    acknowledge(&honest).await;
    for squatter in &squatters {
        admit(squatter).await;
    }
    let newcomer = PrivateKey::new();
    admit(&newcomer).await;
    acknowledge(&newcomer).await;
    assert_eq!(
        a.admitted_tip(newcomer.public_key()).await,
        Some(tip),
        "a peer is never evicted by its own admission"
    );
    assert_eq!(held_by(&a, &newcomer).await, Some(1));
    assert_eq!(
        a.admitted_tip(honest.public_key()).await,
        Some(tip),
        "readmission alone does not outrank a more recent acknowledgment"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_readmitted_peer_keeps_its_frontier_records_by_the_same_recency_as_its_tip() {
    use crate::replica::MAX_UNCONFIGURED_PEERS;
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let a = open(&f, &dir, "a.db", &PrivateKey::new()).await;
    past_genesis(&f, &a).await;
    insert(&a, &f, 1, "held").await;
    let tip = a.followed_tip().await;
    let frontier = a.frontier(f.public).await.unwrap();
    let admit_and_acknowledge = async |key: &PrivateKey, acknowledges: bool| {
        let credentials = f.credentials(key, f.full());
        a.admit_peer(key.public_key(), &credentials, tip)
            .await
            .unwrap();
        if acknowledges {
            a.acknowledge_frontier(key.public_key(), &credentials, &frontier)
                .await
                .unwrap();
        }
    };
    let returning = PrivateKey::new();
    admit_and_acknowledge(&returning, true).await;
    let strangers: Vec<PrivateKey> = (0..MAX_UNCONFIGURED_PEERS)
        .map(|_| PrivateKey::new())
        .collect();
    for (index, stranger) in strangers.iter().enumerate() {
        admit_and_acknowledge(stranger, true).await;
        if index == MAX_UNCONFIGURED_PEERS / 2 {
            admit_and_acknowledge(&returning, false).await;
        }
    }
    assert_eq!(a.recorded_peers().await, MAX_UNCONFIGURED_PEERS);
    assert_eq!(
        held_by(&a, &returning).await,
        Some(1),
        "readmission refreshes the records' place as it does the tip's"
    );
    assert_eq!(held_by(&a, &strangers[0]).await, None);
    assert!(a.admitted_tip(returning.public_key()).await.is_some());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn acknowledgments_without_logs_remember_nothing_and_displace_no_tip() {
    use crate::replica::MAX_REMEMBERED_PEERS;
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let a = open(&f, &dir, "a.db", &PrivateKey::new()).await;
    let admitted = PrivateKey::new();
    a.admit_peer(
        admitted.public_key(),
        &f.credentials(&admitted, f.full()),
        a.followed_tip().await,
    )
    .await
    .unwrap();
    let empty = crate::ScopeFrontier {
        scope: f.public,
        logs: vec![],
    };
    for _ in 0..=MAX_REMEMBERED_PEERS {
        let genesis_stranger = PrivateKey::new();
        a.acknowledge_frontier(
            genesis_stranger.public_key(),
            &f.credentials(&genesis_stranger, f.full()),
            &empty,
        )
        .await
        .unwrap();
    }
    assert_eq!(a.recorded_peers().await, 0);
    assert_eq!(
        a.remembered_peers().await,
        1,
        "peers that record nothing leave nothing to remember"
    );
    assert!(a.admitted_tip(admitted.public_key()).await.is_some());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn admission_reads_no_custody_policy_so_a_corrupt_one_fails_no_handshake() {
    use crate::replica::MAX_REMEMBERED_PEERS;
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let a = open(&f, &dir, "a.db", &PrivateKey::new()).await;
    let configured = PrivateKey::new();
    a.configure_peers([configured.public_key()]).await;
    a.inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "INSERT INTO secure_partition_custody VALUES(?,?)",
            [vec![0xff], vec![0xff]],
        )
        .await
        .unwrap();
    let admit = async |key: &PrivateKey| {
        a.admit_peer(
            key.public_key(),
            &f.credentials(key, f.full()),
            rocky_core::TipId([7; 32]),
        )
        .await
    };
    admit(&configured).await.unwrap();
    for _ in 0..MAX_REMEMBERED_PEERS {
        admit(&PrivateKey::new())
            .await
            .expect("kept peers leave the whole bound to unconfigured peers");
    }
    let newest = PrivateKey::new();
    admit(&newest)
        .await
        .expect("an admission beyond the bound evicts without reading storage");
    admit(&configured)
        .await
        .expect("a configured peer is admitted whatever storage holds");
    assert_eq!(a.admitted_peers().await, MAX_REMEMBERED_PEERS + 1);
    assert!(a.admitted_tip(newest.public_key()).await.is_some());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn custody_peers_stay_kept_across_reopening_and_leave_with_their_policy() {
    use crate::replica::MAX_REMEMBERED_PEERS;
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let alice = PrivateKey::new();
    let full_node = PrivateKey::new();
    let a = open(&f, &dir, "a.db", &alice).await;
    a.set_partition_custody(
        f.public,
        crate::PartitionCustody {
            peers: [rocky_credentials::PeerId::of(full_node.public_key())].into(),
            copies: 0,
        },
    )
    .await
    .unwrap();
    a.close().await;
    let a = open(&f, &dir, "a.db", &alice).await;
    let admit = async |key: &PrivateKey| {
        a.admit_peer(
            key.public_key(),
            &f.credentials(key, f.full()),
            rocky_core::TipId([7; 32]),
        )
        .await
        .unwrap();
    };
    admit(&full_node).await;
    for _ in 0..=MAX_REMEMBERED_PEERS {
        admit(&PrivateKey::new()).await;
    }
    assert!(
        a.admitted_tip(full_node.public_key()).await.is_some(),
        "a reopened replica keeps the custody peers its stored policies name"
    );
    a.set_partition_custody(f.public, crate::PartitionCustody::default())
        .await
        .unwrap();
    assert!(
        a.admitted_tip(full_node.public_key()).await.is_none(),
        "a peer whose custody ends competes as the least recently seen"
    );
    assert_eq!(a.admitted_peers().await, MAX_REMEMBERED_PEERS);
}

const HANDSHAKE_TIME_SLACK: u64 = 5_000_000;

async fn skewed<T>(seconds: u64, future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| {
        let prior = crate::CLOCK_SKEW.with(|skew| skew.replace(seconds));
        let polled = future.as_mut().poll(cx);
        crate::CLOCK_SKEW.with(|skew| skew.set(prior));
        polled
    })
    .await
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn each_side_of_a_handshake_learns_the_others_clock_offset() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "a.db", &alice).await;
    let b = open(&f, &dir, "b.db", &bob).await;
    let near = |offset: i64, expected: i64| offset.abs_diff(expected) < HANDSHAKE_TIME_SLACK;
    for ahead in [0, 3600] {
        let (near_end, far_end) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
        let (dialed, accepted) = tokio::join!(
            skewed(
                ahead,
                Link::open(
                    near_end,
                    &b,
                    bob.clone(),
                    f.credentials(&bob, f.full()),
                    Some(alice.public_key()),
                ),
            ),
            Link::open(
                far_end,
                &a,
                alice.clone(),
                f.credentials(&alice, f.full()),
                None
            ),
        );
        let (dialed, accepted) = (dialed.unwrap(), accepted.unwrap());
        let expected = ahead as i64 * 1_000_000;
        assert!(
            near(dialed.clock_offset, -expected),
            "{}",
            dialed.clock_offset
        );
        assert!(
            near(accepted.clock_offset, expected),
            "{}",
            accepted.clock_offset
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_replayed_hello_without_the_peers_key_records_no_tip_for_that_peer() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, victim, attacker) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "a.db", &alice).await;
    let v = open(&f, &dir, "v.db", &victim).await;
    let (victim_end, mut attacker_end) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
    let (dialed_attacker, harvested) = tokio::join!(
        Link::open(
            victim_end,
            &v,
            victim.clone(),
            f.credentials(&victim, f.full()),
            Some(attacker.public_key()),
        ),
        async {
            let hello = testing::harvested_hello(&mut attacker_end).await;
            drop(attacker_end);
            hello
        },
    );
    assert!(dialed_attacker.is_err());
    let replayed = harvested.unwrap();
    let (attacker_end, acceptor_end) = tokio::io::duplex(MAX_SECURE_FRAME * 2);
    let (accepted, _) = tokio::join!(
        Link::open(
            acceptor_end,
            &a,
            alice.clone(),
            f.credentials(&alice, f.full()),
            None
        ),
        testing::replay_hello(attacker_end, &replayed, &attacker),
    );
    match accepted {
        Err(Error::Rejected(message)) => assert_eq!(message, "link authentication failed"),
        other => panic!("expected the authentication error, got {:?}", other.err()),
    }
    assert_eq!(
        a.admitted_tip(victim.public_key()).await,
        None,
        "a failed handshake must not record a tip for the node its hello names"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_revoked_peer_with_its_own_key_gets_no_tip_recorded_when_alice_accepts() {
    revoked_peer_gets_no_tip(Alice::Accepts).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_revoked_peer_with_its_own_key_gets_no_tip_recorded_when_alice_dials() {
    revoked_peer_gets_no_tip(Alice::Dials).await;
}

enum Alice {
    Accepts,
    Dials,
}
async fn revoked_peer_gets_no_tip(alice_side: Alice) {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, dave) = (PrivateKey::new(), PrivateKey::new());
    let a = open(&f, &dir, "a.db", &alice).await;
    let d = open(&f, &dir, "d.db", &dave).await;
    let learned_not_pinned = statements(&f, 1, &[dave.public_key()]);
    a.learn_revocations(&learned_not_pinned).await.unwrap();
    let refused = match alice_side {
        Alice::Accepts => hello((&alice, &a), (&dave, &d), &f).await.0,
        Alice::Dials => hello((&dave, &d), (&alice, &a), &f).await.1,
    };
    match refused {
        Err(Error::Rejected(message)) => assert_eq!(message, PEER_REVOKED),
        other => panic!("expected the revocation refusal, got {:?}", other.err()),
    }
    assert_eq!(
        a.admitted_tip(dave.public_key()).await,
        None,
        "a peer refused as revoked must not have its tip recorded"
    );
}
