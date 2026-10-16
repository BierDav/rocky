//! The brief's coordinated completion gate on real seals, openings and
//! bookmarks: a tip's writes commit through its own log, entries committed
//! before a seal are its continuation's state, and a write the seal aborted
//! commits there once.
use super::*;
use crate::TipStatement;
use rocky_core::TipId;
use rocky_credentials::{
    AuthorMark, BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction,
    SealStatement, SignedBookmarkMove, SignedSeal, SignedTipOpening, TIP_OPENING_FORMAT,
    TipOpeningStatement,
};
use std::collections::BTreeMap;

type Node = (PrivateKey, Replica);

pub(super) fn bookmark(
    lineage: &Lineage,
    name: &str,
    from: Option<TipId>,
    to: Option<TipId>,
) -> TipStatement {
    TipStatement::Bookmark(
        SignedBookmarkMove::sign(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope: lineage.domain,
                name: name.into(),
                from,
                to,
            },
            lineage.publisher(),
        )
        .unwrap(),
    )
}
/// A `continue` seal of `tip` at the frontier of `tip`'s frames `node` holds.
pub(super) async fn seal(lineage: &Lineage, node: &Replica, tip: TipId) -> SignedSeal {
    let mut authors = BTreeMap::<u32, u64>::new();
    for frame in stored_frames(node, "SELECT header,body FROM secure_operations").await {
        let operation = frame.operation().unwrap();
        let extensions = &operation.header.extensions;
        if extensions.tip == tip {
            let sequence = authors.entry(extensions.author.unwrap()).or_default();
            *sequence = (*sequence).max(extensions.author_sequence);
        }
    }
    SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: lineage.domain,
            tip,
            frontier: authors
                .into_iter()
                .map(|(author, sequence)| AuthorMark { author, sequence })
                .collect(),
            action: SealAction::Continue {
                schema: lineage.definition(3).hash(),
            },
        },
        lineage.publisher(),
    )
    .unwrap()
}
fn opening(lineage: &Lineage, seal: &SignedSeal, minimum: u32) -> SignedTipOpening {
    SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            scope: lineage.domain,
            seal: seal.id().unwrap(),
            minimum_in_sync: minimum,
        },
        lineage.publisher(),
    )
    .unwrap()
}
/// Composes `node` with the mesh's log, which it relays seals to.
fn compose(fixture: &Fixture, node: &Node) {
    node.1
        .set_domain_log(Some(Arc::new(fixture.mesh.client(&node.0))));
}
/// Composes alice and bob, creates `main` on the genesis tip and has both
/// follow it.
pub(super) async fn follow_main(fixture: &Fixture) {
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    let main = bookmark(
        &fixture.lineage,
        "main",
        None,
        Some(tip_log(&fixture.lineage).tip),
    );
    alice
        .1
        .publish_tip_statements(std::slice::from_ref(&main))
        .await
        .unwrap();
    for node in [alice, bob] {
        compose(fixture, node);
        share(&alice.1, &node.1).await;
        node.1
            .follow_bookmark(fixture.lineage.domain, "main")
            .await
            .unwrap();
    }
}
/// A trusted node `node` that joins later, learns the graph from alice and
/// follows `main`.
pub(super) async fn join_main(fixture: &Fixture, node: &Node) {
    fixture.mesh.add(&node.0, &node.1);
    compose(fixture, node);
    share(&fixture.alice.1, &node.1).await;
    node.1
        .follow_bookmark(fixture.lineage.domain, "main")
        .await
        .unwrap();
}
/// Hands every graph statement `from` holds to `to`; returns those admitted.
pub(super) async fn share(from: &Replica, to: &Replica) -> usize {
    to.receive_tip_statements(&from.tip_statements(None, 32).await.unwrap())
        .await
        .unwrap()
}
async fn followed(fixture: &Fixture, node: &Node) -> TipLog {
    node.1.followed_log(fixture.lineage.domain).await
}
async fn slot_on(fixture: &Fixture, node: &Node) -> Result<Slot, CoordinatedError> {
    let log = followed(fixture, node).await;
    node.1
        .coordinated_slot(
            log,
            &fixture.mesh.client(&node.0),
            &fixture.mesh.peers(&fixture.lineage, &node.0),
        )
        .await
}
/// Issues one invoice on the followed tip's log, retrying lost races.
async fn invoice_on(fixture: &Fixture, node: &Node, title: &str) {
    for _ in 0..200 {
        let attempt = async {
            let slot = slot_on(fixture, node).await?;
            fixture.stage(node, slot, title).await?;
            node.1
                .propose(
                    slot.log,
                    &fixture.mesh.client(&node.0),
                    &fixture.mesh.peers(&fixture.lineage, &node.0),
                )
                .await
        };
        match attempt.await {
            Ok(_) => return,
            Err(CoordinatedError::Conflict | CoordinatedError::Aborted) => {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            Err(error) => panic!("{error:?}"),
        }
    }
    panic!("no invoice committed")
}

#[tokio::test]
async fn a_write_a_seal_aborted_retries_in_the_continuation_and_commits_once() {
    let fixture = Fixture::new(1).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    follow_main(&fixture).await;
    let main = tip_log(&fixture.lineage);
    // The seal's frontier predates invoice 0001, which commits before it.
    let sealed = seal(&fixture.lineage, &alice.1, main.tip).await;
    let next = TipLog {
        scope: main.scope,
        tip: sealed.successor().unwrap().unwrap(),
    };
    fixture.invoice(alice, "before").await;
    // Alice stages 0002; bob stores it unacknowledged and seals `main`.
    let slot = fixture.slot(alice).await.unwrap();
    fixture.stage(alice, slot, "pending").await.unwrap();
    fixture.mesh.set(&bob.0, true, false);
    let statements = [
        TipStatement::Seal(sealed.clone()),
        bookmark(&fixture.lineage, "main", Some(main.tip), Some(next.tip)),
    ];
    let (pending, published) = tokio::join!(fixture.propose(alice), async {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        bob.1.publish_tip_statements(&statements).await
    });
    fixture.mesh.set(&bob.0, true, true);
    assert_eq!(published.unwrap(), 2);
    assert!(
        matches!(pending, Err(CoordinatedError::Undecided)),
        "{pending:?}"
    );
    let head = fixture.mesh.log.head(main).unwrap();
    assert_eq!(head.commit, 2);
    assert_eq!(head.terminal.unwrap().digest, sealed.id().unwrap());
    assert_eq!(followed(&fixture, bob).await, next);
    // Alice learns the seal and follows `main` on; her write commits there.
    assert_eq!(share(&bob.1, &alice.1).await, 2);
    assert_eq!(followed(&fixture, alice).await, next);
    let (client, peers) = (
        fixture.mesh.client(&alice.0),
        fixture.mesh.peers(&fixture.lineage, &alice.0),
    );
    let status = alice
        .1
        .resolve_coordinated(next, &client, &peers)
        .await
        .unwrap();
    assert_eq!((status.position, status.undecided), (1, 0));
    let status = bob
        .1
        .resolve_coordinated(
            next,
            &fixture.mesh.client(&bob.0),
            &fixture.mesh.peers(&fixture.lineage, &bob.0),
        )
        .await
        .unwrap();
    assert_eq!((status.position, status.undecided), (1, 0));
    assert_eq!(fixture.mesh.log.head(next).unwrap().commit, 1);
    // 0001, committed before the seal, is the continuation's: once each.
    for node in [alice, bob] {
        assert_eq!(invoices(&node.1).await, numbered(2));
    }
    invoice_on(&fixture, bob, "after").await;
    alice
        .1
        .resolve_coordinated(next, &client, &peers)
        .await
        .unwrap();
    assert_eq!(invoices(&alice.1).await, numbered(3));
}

#[tokio::test]
async fn a_trusted_node_joins_a_continuation_holding_what_committed_before_its_seal() {
    let fixture = Fixture::new(1).await;
    let alice = &fixture.alice;
    follow_main(&fixture).await;
    let main = tip_log(&fixture.lineage);
    fixture.invoice(alice, "before").await;
    let sealed = seal(&fixture.lineage, &alice.1, main.tip).await;
    let next = sealed.successor().unwrap().unwrap();
    alice
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(sealed),
            bookmark(&fixture.lineage, "main", Some(main.tip), Some(next)),
        ])
        .await
        .unwrap();
    // Erin never held `main`'s entries or frames; nothing committed on the
    // continuation tells her of 0001, but its sealed log does.
    let key = PrivateKey::new();
    let erin = (
        key.clone(),
        open(&fixture.lineage, &fixture.dir, "erin", &key).await,
    );
    join_main(&fixture, &erin).await;
    assert_eq!(followed(&fixture, &erin).await.tip, next);
    assert!(invoices(&erin.1).await.is_empty());
    invoice_on(&fixture, &erin, "after the seal").await;
    assert_eq!(invoices(&erin.1).await, numbered(2));
    assert_eq!(erin.1.coordinated_status(main).await.unwrap().position, 1);
}

#[tokio::test]
async fn a_dev_write_commits_through_devs_log_while_mains_in_sync_set_sees_no_entry() {
    let fixture = Fixture::new(2).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    follow_main(&fixture).await;
    let main = tip_log(&fixture.lineage);
    fixture.invoice(alice, "on main").await;
    // `main` continues; `dev` opens on the same seal with a minimum of one.
    let sealed = seal(&fixture.lineage, &alice.1, main.tip).await;
    let next = TipLog {
        scope: main.scope,
        tip: sealed.successor().unwrap().unwrap(),
    };
    let branch = opening(&fixture.lineage, &sealed, 1);
    let dev = TipLog {
        scope: main.scope,
        tip: branch.tip().unwrap(),
    };
    alice
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(sealed),
            bookmark(&fixture.lineage, "main", Some(main.tip), Some(next.tip)),
            TipStatement::Opening(branch),
            bookmark(&fixture.lineage, "dev", None, Some(dev.tip)),
        ])
        .await
        .unwrap();
    assert_eq!(share(&alice.1, &bob.1).await, 4);
    // Carol, a trusted node of her own identity, joins `main` and then
    // switches to `dev` under a node enrolled for that line.
    let identity = PrivateKey::new();
    let key = PrivateKey::new();
    let carol = (
        key.clone(),
        fixture
            .lineage
            .open_certified(
                &fixture.dir.path().join("carol.db"),
                (&identity, &key),
                3,
                Some(lineage_upgrade(&fixture.lineage)),
            )
            .await
            .unwrap(),
    );
    join_main(&fixture, &carol).await;
    // Bob joins first, alone below the domain's minimum of two.
    assert_eq!(
        slot_on(&fixture, bob).await.map(|_| ()),
        Err(CoordinatedError::BelowMinimum)
    );
    invoice_on(&fixture, alice, "main again").await;
    slot_on(&fixture, &carol).await.unwrap();
    assert_eq!(fixture.mesh.log.head(next).unwrap().members.len(), 3);
    carol
        .1
        .follow_bookmark(fixture.lineage.domain, "dev")
        .await
        .unwrap();
    let line = PrivateKey::new();
    carol
        .1
        .register_line_node(crate::LineNode {
            credentials: fixture.lineage.certified(3, &identity, &line),
            key: line,
        })
        .await
        .unwrap();
    assert_eq!(followed(&fixture, &carol).await, dev);
    invoice_on(&fixture, &carol, "on dev").await;
    let head = fixture.mesh.log.head(dev).unwrap();
    assert_eq!((head.commit, head.minimum, head.members.len()), (1, 1, 1));
    // `main`'s in-sync set lost carol when she joined `dev`, and saw no
    // entry of it: its writes neither wait for her nor include hers.
    let main_head = fixture.mesh.log.head(next).unwrap();
    assert_eq!((main_head.commit, main_head.members.len()), (1, 2));
    assert!(!main_head.hashes.contains(&head.hashes[0]));
    invoice_on(&fixture, alice, "main after dev").await;
    assert_eq!(fixture.mesh.log.head(next).unwrap().commit, 2);
    for node in [alice, bob] {
        assert_eq!(node.1.coordinated_status(dev).await.unwrap().position, 0);
        assert_eq!(slot_on(&fixture, node).await.map(|slot| slot.log), Ok(next));
    }
    assert_eq!(invoices(&carol.1).await, numbered(2));
    assert_eq!(
        select(&carol.1, "SELECT title FROM items WHERE key='0002'").await,
        vec![vec![text("on dev")]]
    );
    assert_eq!(invoices(&alice.1).await, numbered(3));
    assert_eq!(
        alice
            .1
            .coordinated_slot(
                dev,
                &fixture.mesh.client(&alice.0),
                &fixture.mesh.peers(&fixture.lineage, &alice.0)
            )
            .await,
        Err(CoordinatedError::Unfollowed)
    );
}
