use super::*;
use rocky_credentials::{
    AuthorMark, BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction,
    SealStatement, SignedBookmarkMove, SignedSeal, SignedTipOpening, TIP_OPENING_FORMAT,
    TipOpeningStatement,
};

async fn ids(replica: &EncryptedReplica<Employees>) -> Vec<Commitment> {
    let mut ids = Vec::new();
    let mut after = None;
    loop {
        let page = replica.tip_statements(after, 32).await.unwrap();
        let last = page.len() < 32;
        for statement in page {
            ids.push(statement.id().unwrap());
        }
        after = ids.last().copied();
        if last {
            return ids;
        }
    }
}

fn movement(
    f: &Fixture,
    name: &str,
    from: Option<rocky_core::TipId>,
    to: rocky_core::TipId,
) -> TipStatement {
    TipStatement::Bookmark(
        SignedBookmarkMove::sign(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope: f.public.domain,
                name: name.into(),
                from,
                to: Some(to),
            },
            &f.publisher,
        )
        .unwrap(),
    )
}
fn seal(f: &Fixture, sequence: u64) -> SignedSeal {
    let manifest = f.manifest.verify(f.publisher.public_key()).unwrap().hash();
    SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: f.public.domain,
            tip: rocky_credentials::genesis_tip(manifest),
            frontier: vec![AuthorMark {
                author: rocky_core::WriterId::from_bytes([1; 32]),
                sequence,
            }],
            action: SealAction::Continue { schema: manifest },
            recovery: None,
        },
        &f.publisher,
    )
    .unwrap()
}
fn opening(f: &Fixture, seal: &SignedSeal, minimum: u32) -> SignedTipOpening {
    SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            schema: f.manifest.verify(f.publisher.public_key()).unwrap().hash(),
            scope: f.public.domain,
            seal: seal.id().unwrap(),
            minimum_in_sync: minimum,
        },
        &f.publisher,
    )
    .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_existing_link_refreshes_graph_pages_and_both_followed_tip_claims() {
    let f = Fixture::with_publisher(PrivateKey::from_bytes(&[71; 32]));
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = super::revocations::open(&f, &dir, "refresh-a.db", &alice).await;
    let b = super::revocations::open(&f, &dir, "refresh-b.db", &bob).await;
    let (left, right) = super::revocations::hello((&alice, &a), (&bob, &b), &f).await;
    let (mut client, mut server) =
        crate::network::link::testing::requests(right.unwrap(), left.unwrap())
            .await
            .unwrap();
    let genesis = a.followed_tip().await;
    let seal = (0..256)
        .map(|sequence| seal(&f, sequence))
        .find(|seal| seal.id().unwrap()[0] >= 192)
        .unwrap();
    let seal_id = seal.id().unwrap();
    let successor = seal.successor().unwrap().unwrap();
    let mut statements = Vec::new();
    for minimum in 1..=256 {
        let opened = opening(&f, &seal, minimum);
        if opened.id().unwrap() < seal_id {
            statements.push(TipStatement::Opening(opened));
        }
        if statements.len() == 32 {
            break;
        }
    }
    assert_eq!(statements.len(), 32);
    statements.extend([
        TipStatement::Seal(seal),
        movement(&f, "main", None, genesis),
        movement(&f, "main", Some(genesis), successor),
    ]);
    a.receive_tip_statements(&statements).await.unwrap();
    a.follow_bookmark(f.public.domain, "main").await.unwrap();
    b.receive_tip_statements(&[movement(&f, "late-bookmark", None, genesis)])
        .await
        .unwrap();
    let (synced, served) = tokio::join!(
        async {
            client.sync_tip_graph(&b).await?;
            client.close().await
        },
        server.serve_to_end(&a),
    );
    synced.unwrap();
    served.unwrap();
    assert_eq!(ids(&a).await, ids(&b).await);
    assert_eq!(
        a.inner
            .lock(crate::replica::Class::Background)
            .await
            .peer_tip(bob.public_key()),
        Some(genesis)
    );
    assert_eq!(
        b.inner
            .lock(crate::replica::Class::Background)
            .await
            .peer_tip(alice.public_key()),
        Some(successor)
    );
    assert_eq!(
        a.tip_graph(f.public.domain)
            .await
            .unwrap()
            .bookmark("late-bookmark")
            .unwrap(),
        Some(genesis)
    );
}

const LATE_DIGEST_FIRST_BYTE: u8 = 192;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn graph_handshake_verifies_dependencies_across_digest_sorted_pages() {
    let f = Fixture::with_publisher(PrivateKey::from_bytes(&[71; 32]));
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = super::revocations::open(&f, &dir, "a.db", &alice).await;
    let b = super::revocations::open(&f, &dir, "b.db", &bob).await;
    let seal = (0..256)
        .map(|sequence| seal(&f, sequence))
        .find(|seal| seal.id().unwrap()[0] >= LATE_DIGEST_FIRST_BYTE)
        .unwrap();
    let seal_id = seal.id().unwrap();
    let mut statements = vec![TipStatement::Seal(seal.clone())];
    for minimum in 1..=256 {
        let opened = opening(&f, &seal, minimum);
        if opened.id().unwrap() < seal_id {
            statements.push(TipStatement::Opening(opened));
        }
        if statements.len() == 33 {
            break;
        }
    }
    assert_eq!(statements.len(), 33);
    a.receive_tip_statements(&statements).await.unwrap();
    let page = a.tip_statements(None, 32).await.unwrap();
    assert!(page.iter().all(|s| matches!(s, TipStatement::Opening(_))));
    let before = b.snapshot(f.public).await.unwrap();
    let (left, right) = super::revocations::hello((&alice, &a), (&bob, &b), &f).await;
    let (_left, _right) = (left.unwrap(), right.unwrap());
    assert_eq!(ids(&a).await, ids(&b).await);
    assert_eq!(b.snapshot(f.public).await.unwrap().rows, before.rows);
    drop(b);
    let b = super::revocations::open(&f, &dir, "b.db", &bob).await;
    assert_eq!(
        b.tip_graph(f.public.domain).await.unwrap().openings.len(),
        32
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn different_followed_tips_exchange_graph_union_and_refresh_peer_tips() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = super::revocations::open(&f, &dir, "a.db", &alice).await;
    let b = super::revocations::open(&f, &dir, "b.db", &bob).await;
    let genesis = a.followed_tip().await;
    let seal = seal(&f, 0);
    let successor = seal.successor().unwrap().unwrap();
    let opened = opening(&f, &seal, 2);
    let branch = opened.tip().unwrap();
    let common = [
        movement(&f, "main", None, genesis),
        TipStatement::Seal(seal),
        TipStatement::Opening(opened),
        movement(&f, "main", Some(genesis), successor),
        movement(&f, "dev", None, branch),
    ];
    for replica in [&a, &b] {
        replica.receive_tip_statements(&common).await.unwrap();
    }
    a.follow_bookmark(f.public.domain, "main").await.unwrap();
    b.follow_bookmark(f.public.domain, "dev").await.unwrap();
    a.receive_tip_statements(&[movement(&f, "release", None, successor)])
        .await
        .unwrap();
    b.receive_tip_statements(&[movement(&f, "draft", None, branch)])
        .await
        .unwrap();
    let (left, right) = super::revocations::hello((&alice, &a), (&bob, &b), &f).await;
    let (_left, _right) = (left.unwrap(), right.unwrap());
    assert_ne!(a.followed_tip().await, b.followed_tip().await);
    assert_eq!(ids(&a).await, ids(&b).await);
    assert_eq!(
        a.inner
            .lock(crate::replica::Class::Background)
            .await
            .peer_tip(bob.public_key()),
        Some(branch)
    );
    assert_eq!(
        b.inner
            .lock(crate::replica::Class::Background)
            .await
            .peer_tip(alice.public_key()),
        Some(successor)
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_forged_graph_record_refuses_the_handshake_without_installing_its_prefix() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = super::revocations::open(&f, &dir, "a.db", &alice).await;
    let b = super::revocations::open(&f, &dir, "b.db", &bob).await;
    let genesis = a.followed_tip().await;
    a.receive_tip_statements(&[movement(&f, "valid", None, genesis)])
        .await
        .unwrap();
    let mut forged = movement(&f, "original", None, genesis);
    let TipStatement::Bookmark(moved) = &mut forged else {
        panic!("bookmark fixture");
    };
    moved.statement.name = "forged".into();
    let bypassing_sender_admission = a.inner.lock(crate::replica::Class::Background).await;
    bypassing_sender_admission
        .conn
        .execute(
            "INSERT INTO secure_tip_statements(digest,statement) VALUES(?,?)",
            [forged.id().unwrap().to_vec(), encode(&forged).unwrap()],
        )
        .await
        .unwrap();
    drop(bypassing_sender_admission);
    let (left, right) = super::revocations::hello((&alice, &a), (&bob, &b), &f).await;
    assert!(left.is_err());
    assert!(
        right.is_err(),
        "the receiving peer verifies a record the sender stored without admission"
    );
    assert!(ids(&b).await.is_empty());
    assert!(b.snapshot(f.public).await.unwrap().rows.is_empty());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_stalled_request_lane_does_not_hold_control_or_another_lane() {
    use crate::network::link::{Frame, Message, channels::Packet};
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = super::revocations::open(&f, &dir, "lane-a.db", &alice).await;
    let b = super::revocations::open(&f, &dir, "lane-b.db", &bob).await;
    let (left, right) = super::revocations::hello((&alice, &a), (&bob, &b), &f).await;
    let (mut left, mut right) = (left.unwrap(), right.unwrap());
    let mut near: Vec<Box<dyn network::SessionStream>> = Vec::new();
    let mut far: Vec<Box<dyn network::SessionStream>> = Vec::new();
    for _ in 0..4 {
        let (a, b) = tokio::io::duplex(1);
        near.push(Box::new(a));
        far.push(Box::new(b));
    }
    left.attach_lanes(near).unwrap();
    let frame_overflowing_an_unread_one_byte_lane = Frame {
        channel: 2,
        through: Vec::new(),
        direction: false,
        message: Message::Channel(Packet::Data {
            generation: 0,
            bytes: vec![1; 8192],
            last: true,
        }),
    };
    left.writer
        .send(frame_overflowing_an_unread_one_byte_lane)
        .await
        .unwrap();
    left.writer
        .send(Frame {
            channel: 4,
            through: Vec::new(),
            direction: false,
            message: Message::Channel(Packet::Open { generation: 0 }),
        })
        .await
        .unwrap();
    left.writer
        .send(Frame::new(false, Message::Ready { persistent: true }))
        .await
        .unwrap();
    let control = rocky_platform::time::timeout(
        std::time::Duration::from_millis(500),
        right.reader.receive(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(control.message, Message::Ready { persistent: true }),
        "control is delivered while the peer reads none of the added lanes"
    );
    right.attach_lanes(far).unwrap();
    left.writer
        .send(Frame {
            channel: 3,
            through: Vec::new(),
            direction: false,
            message: Message::Channel(Packet::Open { generation: 1 }),
        })
        .await
        .unwrap();
    let mut found = false;
    let mut pushed = false;
    for _ in 0..3 {
        let frame = right.reader.receive().await.unwrap();
        found |= frame.channel == 3;
        pushed |= frame.channel == 4;
    }
    assert!(found && pushed);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn graph_reconciliation_sends_only_missing_statements_and_none_when_unchanged() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = super::revocations::open(&f, &dir, "difference-a.db", &alice).await;
    let b = super::revocations::open(&f, &dir, "difference-b.db", &bob).await;
    let tip = a.followed_tip().await;
    let common: Vec<_> = (0..65)
        .map(|i| movement(&f, &format!("shared-{i}"), None, tip))
        .collect();
    a.receive_tip_statements(&common).await.unwrap();
    b.receive_tip_statements(&common).await.unwrap();
    a.receive_tip_statements(&[movement(&f, "only-a", None, tip)])
        .await
        .unwrap();
    b.receive_tip_statements(&[movement(&f, "only-b", None, tip)])
        .await
        .unwrap();
    let (left, right) = super::revocations::hello((&alice, &a), (&bob, &b), &f).await;
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_eq!((left.statements_sent, right.statements_sent), (1, 1));
    assert_eq!(ids(&a).await, ids(&b).await);
    let (mut client, mut server) = network::link::testing::requests(right, left).await.unwrap();
    let ((), served) = tokio::join!(
        async {
            client.sync_tip_graph(&b).await.unwrap();
            assert_eq!(client.sent_counts.statements, 0);
            client.close().await.unwrap();
        },
        server.serve_to_end(&a)
    );
    served.unwrap();
    assert_eq!(server.sent_counts.statements, 0);
    let (left, right) = super::revocations::hello((&alice, &a), (&bob, &b), &f).await;
    assert_eq!(
        (
            left.unwrap().statements_sent,
            right.unwrap().statements_sent
        ),
        (0, 0)
    );
}
