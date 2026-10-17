//! Tip graphs are admitted independently of row subscriptions and tip selection.
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
                author: 1,
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

#[tokio::test]
async fn an_existing_link_refreshes_graph_pages_and_both_followed_tip_claims() {
    let f = Fixture::with_publisher(PrivateKey::from_bytes(&[71; 32]));
    let dir = TempDir::new().unwrap();
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
        a.inner.lock().await.peer_tip(bob.public_key()),
        Some(genesis)
    );
    assert_eq!(
        b.inner.lock().await.peer_tip(alice.public_key()),
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

#[tokio::test]
async fn graph_handshake_verifies_dependencies_across_digest_sorted_pages() {
    let f = Fixture::with_publisher(PrivateKey::from_bytes(&[71; 32]));
    let dir = TempDir::new().unwrap();
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let a = super::revocations::open(&f, &dir, "a.db", &alice).await;
    let b = super::revocations::open(&f, &dir, "b.db", &bob).await;
    // Choose a seal whose dependents can precede it in digest order cheaply.
    let seal = (0..256)
        .map(|sequence| seal(&f, sequence))
        .find(|seal| seal.id().unwrap()[0] >= 192)
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

#[tokio::test]
async fn different_followed_tips_exchange_graph_union_and_refresh_peer_tips() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
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
        a.inner.lock().await.peer_tip(bob.public_key()),
        Some(branch)
    );
    assert_eq!(
        b.inner.lock().await.peer_tip(alice.public_key()),
        Some(successor)
    );
}

#[tokio::test]
async fn a_forged_graph_record_refuses_the_handshake_without_installing_its_prefix() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
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
    // Bypass the sender's admission deliberately; the receiving peer must verify.
    a.inner
        .lock()
        .await
        .conn
        .execute(
            "INSERT INTO secure_tip_statements(digest,statement) VALUES(?,?)",
            [forged.id().unwrap().to_vec(), encode(&forged).unwrap()],
        )
        .await
        .unwrap();
    let (left, right) = super::revocations::hello((&alice, &a), (&bob, &b), &f).await;
    assert!(left.is_err());
    assert!(right.is_err());
    assert!(ids(&b).await.is_empty());
    assert!(b.snapshot(f.public).await.unwrap().rows.is_empty());
}
