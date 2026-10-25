//! Phase 19's gate: a stuck tip recovers with one owner `continue` seal
//! that abandons the coordinated entries nobody holds. Appends wait for it,
//! the continuation takes them, members that applied an abandoned entry
//! roll it back and hold it, ordinary writes after the retained prefix
//! apply everywhere, and a branch's recovery leaves `main`'s log alone.
use super::seals::{bookmark, compose, follow_main, invoice_on, opening, seal, share, slot_on};
use super::*;
use crate::TipStatement;
use rocky_credentials::{Recovery, SealStatement, SignedSeal};

mod carries;
mod regressions;
mod restoration;

type Node = (PrivateKey, Replica);

async fn rows(replica: &Replica) -> Vec<(String, String)> {
    select(replica, "SELECT key,title FROM items ORDER BY key")
        .await
        .into_iter()
        .map(|row| match row.as_slice() {
            [SqlValue::Text(key), SqlValue::Text(title)] => (key.clone(), title.clone()),
            other => panic!("{other:?}"),
        })
        .collect()
}
fn row(key: &str, title: &str) -> (String, String) {
    (key.into(), title.into())
}
async fn abandoned_frames(replica: &Replica) -> i64 {
    match rocky_storage::storage::rows(
        &replica.inner.lock().await.conn,
        "SELECT COUNT(*) FROM secure_operations WHERE status='abandoned'",
    )
    .await
    .unwrap()[0]
        .as_slice()
    {
        [SqlValue::Integer(count)] => *count,
        other => panic!("{other:?}"),
    }
}
/// Where `replica` indexes each coordinated frame it holds.
async fn entry_frames(replica: &Replica) -> Vec<(TipLog, u64)> {
    rocky_storage::storage::rows(
        &replica.inner.lock().await.conn,
        "SELECT log,position FROM secure_entry_frames ORDER BY hash",
    )
    .await
    .unwrap()
    .into_iter()
    .map(|row| match row.as_slice() {
        [SqlValue::Blob(log), SqlValue::Integer(position)] => {
            (crate::decode(log).unwrap(), *position as u64)
        }
        other => panic!("{other:?}"),
    })
    .collect()
}
/// The owner's `continue` seal of `tip` at the frontier `node` holds,
/// abandoning what `recovery` names.
async fn recovery_seal(
    lineage: &Lineage,
    node: &Replica,
    tip: rocky_core::TipId,
    recovery: Recovery,
) -> SignedSeal {
    let ordinary = seal(lineage, node, tip).await;
    SignedSeal::sign(
        SealStatement {
            recovery: Some(recovery),
            ..ordinary.statement
        },
        lineage.publisher(),
    )
    .unwrap()
}
/// A device that learns the graph from `from` and follows `name`.
async fn device(fixture: &Fixture, from: &Replica, name: &str, bookmark: &str) -> Node {
    let key = PrivateKey::new();
    let replica = open(&fixture.lineage, &fixture.dir, name, &key).await;
    share(from, &replica).await;
    replica
        .follow_bookmark(fixture.lineage.domain, bookmark)
        .await
        .unwrap();
    (key, replica)
}
/// A node of its own identity that takes `main`'s rows from `from`, then
/// follows `dev` under a line node: the state `dev` opens on.
async fn branch_device(fixture: &Fixture, from: &Node, name: &str) -> Node {
    let identity = PrivateKey::new();
    let key = PrivateKey::new();
    let replica = fixture
        .lineage
        .open_trusted(
            &fixture.dir.path().join(format!("{name}.db")),
            (&identity, &key),
            3,
            None,
        )
        .await
        .unwrap();
    share(&from.1, &replica).await;
    replica
        .follow_bookmark(fixture.lineage.domain, "main")
        .await
        .unwrap();
    sync(&fixture.lineage, &from.1, &replica, &key).await;
    replica
        .follow_bookmark(fixture.lineage.domain, "dev")
        .await
        .unwrap();
    let line = PrivateKey::new();
    replica
        .register_line_node(crate::LineNode {
            credentials: fixture.lineage.trusted(3, &identity, &line),
            key: line.clone(),
        })
        .await
        .unwrap();
    (line, replica)
}
/// A trusted node that learns the graph from `from` and follows `main`.
async fn trusted_device(fixture: &Fixture, from: &Replica, name: &str) -> Node {
    let node = device(fixture, from, name, "main").await;
    trusted(fixture, &node);
    node
}
/// Composes `node` as a trusted node of the mesh.
fn trusted(fixture: &Fixture, node: &Node) {
    fixture.mesh.add(&node.0, &node.1);
    compose(fixture, node);
}
async fn write(replica: &Replica, key: &str, title: &str) {
    identity_insert(
        &session(replica).await,
        "INSERT INTO items(key,title) VALUES(?,?)",
        vec![text(key), text(title)],
    )
    .await
    .unwrap();
}
async fn proposal(fixture: &Fixture, node: &Node, log: TipLog) -> Recovery {
    node.1
        .recovery_proposal(
            log,
            &fixture.mesh.client(&node.0),
            &fixture.mesh.peers(&fixture.lineage, &node.0),
        )
        .await
        .unwrap()
}

/// Stages the next invoice on `node` under `handle`; the caller proposes.
async fn stage_with(fixture: &Fixture, node: &Node, title: &str, handle: [u8; 32]) {
    let slot = fixture.slot(node).await.unwrap();
    let key = format!("{:04}", invoices(&node.1).await.len() + 1);
    let sql = session(&node.1).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    identity_insert(
        &sql,
        "INSERT INTO items(key,title) VALUES(?,?)",
        vec![text(&key), text(title)],
    )
    .await
    .unwrap();
    sql.commit_coordinated(handle, slot).await.unwrap();
}
const ABANDONED: [u8; 32] = [7; 32];

/// Bob's invoice 0001 reached every member; Alice's 0002 reached only
/// `carol`, who then wrote an ordinary row; Alice also staged 0003 without
/// proposing it. Then `main`'s in-sync set is gone: Bob for good, Alice
/// away. `fixture.alice` is a fresh trusted node and `fixture.bob` the
/// member `dave`. `main` and `release` point at the stuck tip.
struct Stuck {
    fixture: Fixture,
    carol: Node,
    author: Node,
}
impl Stuck {
    async fn new() -> Self {
        Self::with_pending(true).await
    }
    async fn with_pending(pending: bool) -> Self {
        let fixture = if pending {
            Fixture::new(1).await
        } else {
            Fixture::with_keys(
                1,
                Lineage::pinned(193),
                [194, 195].map(|seed| PrivateKey::from_bytes(&[seed; 32])),
            )
            .await
        };
        follow_main(&fixture).await;
        let main = tip_log(&fixture.lineage).tip;
        fixture
            .alice
            .1
            .publish_tip_statements(&[bookmark(&fixture.lineage, "release", None, Some(main))])
            .await
            .unwrap();
        fixture.invoice(&fixture.bob, "kept").await;
        let dave = device(&fixture, &fixture.alice.1, "dave", "main").await;
        sync(&fixture.lineage, &fixture.bob.1, &dave.1, &dave.0).await;
        stage_with(&fixture, &fixture.alice, "abandoned", ABANDONED).await;
        assert_eq!(fixture.propose(&fixture.alice).await.unwrap().position, 2);
        let carol = device(&fixture, &fixture.alice.1, "carol", "main").await;
        sync(&fixture.lineage, &fixture.alice.1, &carol.1, &carol.0).await;
        write(&carol.1, "late", "carol's").await;
        assert_eq!(
            rows(&carol.1).await,
            [
                row("0001", "kept"),
                row("0002", "abandoned"),
                row("late", "carol's")
            ]
        );
        if pending {
            stage_with(&fixture, &fixture.alice, "pending", [8; 32]).await;
        }
        // Alice goes away before proposing it, as after a crash.
        let Fixture {
            lineage,
            dir,
            mesh,
            alice,
            bob,
        } = fixture;
        mesh.nodes.lock().unwrap().clear();
        drop(bob);
        std::fs::remove_file(dir.path().join("bob.db")).unwrap();
        mesh.log
            .set_owner(lineage.domain, lineage.publisher().public_key());
        let (alice_key, replica) = alice;
        drop(replica);
        let alice = (
            alice_key.clone(),
            open(&lineage, &dir, "alice", &alice_key).await,
        );
        let fresh_key = PrivateKey::new();
        let fresh = open(&lineage, &dir, "fresh", &fresh_key).await;
        let fixture = Fixture {
            lineage,
            dir,
            mesh,
            alice: (fresh_key, fresh),
            bob: dave,
        };
        // Alice and Carol are away: no reachable device holds entry 2.
        fixture.mesh.add(&fixture.bob.0, &fixture.bob.1);
        trusted(&fixture, &fixture.alice);
        share(&fixture.bob.1, &fixture.alice.1).await;
        fixture
            .alice
            .1
            .follow_bookmark(fixture.lineage.domain, "main")
            .await
            .unwrap();
        Self {
            fixture,
            carol,
            author: alice,
        }
    }
    /// The owner's recovery of `main`'s tip, retaining what dave holds.
    async fn seal(&self) -> SignedSeal {
        let f = &self.fixture;
        let main = tip_log(&f.lineage);
        let recovery = proposal(f, &f.alice, main).await;
        assert_eq!(
            (recovery.retained.position, recovery.replaced.position),
            (1, 2)
        );
        recovery_seal(&f.lineage, &f.alice.1, main.tip, recovery).await
    }
    /// Publishes the recovery seal alone from the fresh node, which then
    /// writes the next invoice on the continuation. Returns the seal.
    async fn recover(&self) -> SignedSeal {
        let f = &self.fixture;
        let sealed = self.seal().await;
        assert_eq!(
            f.alice
                .1
                .publish_tip_statements(&[TipStatement::Seal(sealed.clone())])
                .await
                .unwrap(),
            2
        );
        self.continue_on(&sealed).await;
        sealed
    }
    /// Checks that both bookmarks followed the recovery, then invoices.
    async fn continue_on(&self, sealed: &SignedSeal) {
        let f = &self.fixture;
        let next = sealed.successor().unwrap();
        let graph = f.alice.1.tip_graph(f.lineage.domain).await.unwrap();
        assert_eq!(graph.bookmark("main").unwrap(), next);
        assert_eq!(graph.bookmark("release").unwrap(), next);
        invoice_on(f, &f.alice, "continued").await;
        assert_eq!(rows(&f.alice.1).await[1], row("0002", "continued"));
    }
}

#[tokio::test]
async fn without_a_copy_appends_wait_for_the_owners_recovery_and_continue_on_its_tip() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let main = tip_log(&f.lineage);
    assert_eq!(f.slot(&f.alice).await, Err(CoordinatedError::Behind));
    // Nobody but the owner recovers: a node's own recovery is refused by
    // the coordinator and by every graph.
    let forged = SignedSeal::sign(stuck.seal().await.statement, &f.alice.0).unwrap();
    assert_eq!(
        f.mesh
            .client(&f.alice.0)
            .recover(main, forged.to_bytes().unwrap())
            .await,
        Err(CoordinationError::Denied)
    );
    assert!(
        f.alice
            .1
            .publish_tip_statements(&[TipStatement::Seal(forged)])
            .await
            .is_err()
    );
    // An ordinary seal needs the in-sync set, which nobody can rejoin.
    let ordinary = seal(&f.lineage, &f.alice.1, main.tip).await;
    let error = f
        .alice
        .1
        .publish_tip_statements_through(
            &[TipStatement::Seal(ordinary)],
            &f.mesh.peers(&f.lineage, &f.alice.0),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("behind:"), "{error}");
    let head = f.mesh.log.head(main).unwrap();
    assert_eq!((head.commit, head.terminal), (2, None));

    let sealed = stuck.recover().await;
    let head = f.mesh.log.head(main).unwrap();
    assert_eq!(head.commit, 3);
    assert_eq!(head.terminal.unwrap().digest, sealed.id().unwrap());
    let next = TipLog {
        scope: main.scope,
        tip: sealed.successor().unwrap().unwrap(),
    };
    assert_eq!(f.alice.1.followed_log(main.scope).await, next);
    // The continuation's own position 2 is not the abandoned one of `main`.
    invoice_on(f, &f.alice, "more").await;
    assert_eq!(f.mesh.log.head(next).unwrap().commit, 2);
    // A retried recovery answers the same commit; another one is `Sealed`.
    assert!(matches!(
        f.mesh
            .client(&f.alice.0)
            .recover(main, sealed.to_bytes().unwrap())
            .await,
        Ok(LogResult::Committed { position: 3, .. })
    ));
    let expected = [
        row("0001", "kept"),
        row("0002", "continued"),
        row("0003", "more"),
    ];
    assert_eq!(rows(&f.alice.1).await, expected);
    share(&f.alice.1, &f.bob.1).await;
    sync(&f.lineage, &f.alice.1, &f.bob.1, &f.bob.0).await;
    assert_eq!(rows(&f.bob.1).await, expected);
}

#[tokio::test]
async fn returning_devices_hold_abandoned_writes_and_ordinary_writes_after_them_apply() {
    let stuck = Stuck::new().await;
    let sealed = stuck.recover().await;
    let f = &stuck.fixture;
    let (carol, alice) = (&stuck.carol, &stuck.author);
    let scope = f.lineage.scope("items");
    // Carol admits the recovery: the abandoned invoice rolls back and is
    // held; her ordinary write stays.
    assert!(share(&f.alice.1, &carol.1).await > 0);
    assert_eq!(
        rows(&carol.1).await,
        [row("0001", "kept"), row("late", "carol's")]
    );
    assert!(abandoned_frames(&carol.1).await > 0);
    assert_eq!(carol.1.held_changes().await.unwrap().len(), 1);
    let credentials = carol.1.inner.lock().await.config.credentials.clone();
    let (offered, next) = carol
        .1
        .held_frame_page(
            carol.0.public_key(),
            &credentials,
            scope,
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(next.is_none());
    let originals = {
        let inner = carol.1.inner.lock().await;
        crate::storage::rows(&inner.conn, "SELECT frame FROM secure_held_frames")
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                let [rocky_core::SqlValue::Blob(bytes)] = row.as_slice() else {
                    panic!("held frame")
                };
                crate::decode::<EncryptedFrame>(bytes)
                    .unwrap()
                    .to_bytes()
                    .unwrap()
            })
            .collect::<BTreeSet<_>>()
    };
    assert!(!originals.is_empty());
    assert_eq!(
        offered
            .iter()
            .map(|frame| frame.to_bytes().unwrap())
            .collect::<BTreeSet<_>>(),
        originals
    );
    assert_eq!(
        rows(&carol.1).await,
        [row("0001", "kept"), row("late", "carol's")]
    );
    // With a tip graph no base is cut beside a seal, so none can hold an
    // abandoned write.
    let error = carol.1.checkpoint(scope).await.unwrap_err();
    assert!(error.to_string().contains("sits at a seal"), "{error}");
    sync(&f.lineage, &f.alice.1, &carol.1, &carol.0).await;
    // Her abandoned and ordinary frames reach the others: the first are
    // held there too, the second is a late frame of the continuation.
    sync(&f.lineage, &carol.1, &f.alice.1, &f.alice.0).await;
    share(&f.alice.1, &f.bob.1).await;
    sync(&f.lineage, &carol.1, &f.bob.1, &f.bob.0).await;
    sync(&f.lineage, &f.alice.1, &f.bob.1, &f.bob.0).await;
    let expected = [
        row("0001", "kept"),
        row("0002", "continued"),
        row("late", "carol's"),
    ];
    for replica in [&f.alice.1, &f.bob.1, &carol.1] {
        assert_eq!(rows(replica).await, expected);
        assert_eq!(
            abandoned_frames(replica).await,
            abandoned_frames(&carol.1).await
        );
        assert_eq!(replica.held_changes().await.unwrap().len(), 1);
    }
    // Admitting the recovery again changes nothing.
    assert_eq!(
        carol
            .1
            .receive_tip_statements(&[TipStatement::Seal(sealed.clone())])
            .await
            .unwrap(),
        0
    );
    assert_eq!(rows(&carol.1).await, expected);
    // Alice, the author, returns: her committed write rolls back and its
    // handle reports it; her staged one, computed on the abandoned state,
    // is held instead of carried into the continuation.
    trusted(f, alice);
    assert!(share(&f.alice.1, &alice.1).await > 0);
    let error = alice.1.handle_committed(ABANDONED).await.unwrap_err();
    assert!(error.to_string().contains("abandoned"), "{error}");
    let next = TipLog {
        scope: scope.domain,
        tip: sealed.successor().unwrap().unwrap(),
    };
    let before = f.mesh.log.head(next).unwrap();
    let main = tip_log(&f.lineage);
    // No resolution of the old log is needed after admission.
    let status = alice.1.coordinated_status(main).await.unwrap();
    assert_eq!(status.undecided, 0);
    assert_eq!(f.mesh.log.head(next).unwrap(), before);
    assert_eq!(alice.1.held_changes().await.unwrap().len(), 2);
    sync(&f.lineage, &f.alice.1, &alice.1, &alice.0).await;
    sync(&f.lineage, &carol.1, &alice.1, &alice.0).await;
    assert_eq!(rows(&alice.1).await, expected);
    // Carol and Alice join the continuation's log and number on.
    trusted(f, carol);
    invoice_on(f, carol, "after").await;
    assert!(rows(&carol.1).await.contains(&row("0004", "after")));
    // A base compacted at the recovery holds none of the abandoned writes.
    let report = carol.1.compact(scope.domain, 0).await.unwrap();
    assert_eq!(report.compacted, Some(sealed.id().unwrap()));
    assert_eq!(
        rows(&carol.1).await,
        [
            row("0001", "kept"),
            row("0002", "continued"),
            row("0004", "after"),
            row("late", "carol's"),
        ]
    );
}

/// No reachable peer.
struct Nobody;
impl EntryPeers for Nobody {
    fn store<'a>(&'a self, _: PublicKey, _: &'a CoordinatedEntry) -> PeerFuture<'a, ()> {
        Box::pin(async { Err(Error::Rejected("unreachable".into())) })
    }
    fn committed(&self, _: TipLog, _: u64) -> PeerFuture<'_, Vec<CommittedEntry>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
/// A coordinator that commits a recovery and then loses the reply.
struct LostReply(Client);
impl DomainLog for LostReply {
    fn pending_status(
        &self,
        log: TipLog,
        position: u64,
        hash: rocky_core::Commitment,
    ) -> CoordinationFuture<'_, rocky_core::LogStatus> {
        Box::pin(async move { Ok(self.log_head(log).await?.status(position, hash)) })
    }
    fn node(&self) -> NodeKey {
        self.0.node()
    }
    fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()> {
        self.0.configure(log, minimum)
    }
    fn seal(
        &self,
        log: TipLog,
        terminal: rocky_core::LogTerminal,
    ) -> CoordinationFuture<'_, LogResult> {
        self.0.seal(log, terminal)
    }
    fn recover(&self, log: TipLog, seal: Vec<u8>) -> CoordinationFuture<'_, LogResult> {
        Box::pin(async move {
            self.0.recover(log, seal).await?;
            Err(CoordinationError::Unavailable)
        })
    }
    fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead> {
        self.0.log_head(log)
    }
    fn join(
        &self,
        log: TipLog,
        position: u64,
        running: Commitment,
        sequence: u64,
    ) -> CoordinationFuture<'_, u64> {
        self.0.join(log, position, running, sequence)
    }
    fn append(&self, log: TipLog, append: Append) -> CoordinationFuture<'_, LogPending> {
        self.0.append(log, append)
    }
    fn acknowledge(&self, log: TipLog, pending: LogPending) -> CoordinationFuture<'_, LogResult> {
        self.0.acknowledge(log, pending)
    }
    fn acknowledge_terminal(
        &self,
        log: TipLog,
        acknowledgment: Vec<u8>,
    ) -> CoordinationFuture<'_, ()> {
        self.0.acknowledge_terminal(log, acknowledgment)
    }
}

#[tokio::test]
async fn a_recovery_survives_a_crash_at_every_publication_step_and_a_lost_reply() {
    for step in 1..=3 {
        let mut stuck = Stuck::new().await;
        let sealed = stuck.seal().await;
        let f = &stuck.fixture;
        let main = tip_log(&f.lineage);
        f.alice.1.fail_publication_after(step).await;
        let error = f
            .alice
            .1
            .publish_tip_statements(&[TipStatement::Seal(sealed.clone())])
            .await
            .unwrap_err();
        assert!(matches!(error, Error::InjectedFailure), "{step}: {error}");
        let head = f.mesh.log.head(main).unwrap();
        assert_eq!(head.terminal.is_some(), step > 1, "after step {step}");
        let graph = f.alice.1.tip_graph(main.scope).await.unwrap();
        assert_eq!(graph.seal(main.tip).unwrap(), None, "after step {step}");
        // The process dies; the journal sends the same bytes after reopen.
        let key = f.alice.0.clone();
        f.mesh
            .nodes
            .lock()
            .unwrap()
            .retain(|peer| peer.key.public_key() != key.public_key());
        let Stuck { fixture, .. } = &mut stuck;
        let (_, replica) = std::mem::replace(
            &mut fixture.alice,
            (
                key.clone(),
                open(&fixture.lineage, &fixture.dir, "scratch", &key).await,
            ),
        );
        drop(replica);
        let reopened = open(&fixture.lineage, &fixture.dir, "fresh", &key).await;
        fixture.alice = (key.clone(), reopened);
        trusted(fixture, &fixture.alice);
        assert_eq!(
            fixture
                .alice
                .1
                .settle_tip_statements(&fixture.mesh.peers(&fixture.lineage, &key))
                .await
                .unwrap(),
            2,
            "after step {step}"
        );
        assert_eq!(
            fixture
                .mesh
                .log
                .head(main)
                .unwrap()
                .terminal
                .unwrap()
                .digest,
            sealed.id().unwrap()
        );
        stuck.continue_on(&sealed).await;
    }
    // A reply lost after the coordinator committed keeps the journal; the
    // next settlement admits it once.
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let main = tip_log(&f.lineage);
    let sealed = stuck.seal().await;
    f.alice
        .1
        .set_domain_log(Some(Arc::new(LostReply(f.mesh.client(&f.alice.0)))));
    let error = f
        .alice
        .1
        .publish_tip_statements(&[TipStatement::Seal(sealed.clone())])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("coordinator_unavailable"),
        "{error}"
    );
    assert_eq!(f.mesh.log.head(main).unwrap().commit, 3);
    compose(f, &f.alice);
    let peers = f.mesh.peers(&f.lineage, &f.alice.0);
    assert_eq!(f.alice.1.settle_tip_statements(&peers).await.unwrap(), 2);
    assert_eq!(f.alice.1.settle_tip_statements(&peers).await.unwrap(), 0);
    stuck.continue_on(&sealed).await;
}

#[tokio::test]
async fn a_received_seal_waits_until_its_receiver_catches_up_and_joins() {
    let fixture = Fixture::new(1).await;
    follow_main(&fixture).await;
    let main = tip_log(&fixture.lineage);
    fixture.invoice(&fixture.alice, "one").await;
    let erin = device(&fixture, &fixture.alice.1, "erin", "main").await;
    trusted(&fixture, &erin);
    let sealed = seal(&fixture.lineage, &fixture.alice.1, main.tip).await;
    let head = fixture.mesh.log.head(main).unwrap();
    // Outside the in-sync set and without peers, Erin cannot end the log:
    // the seal waits outside her graph and the head is unchanged.
    assert_eq!(
        erin.1
            .receive_tip_statements(&[TipStatement::Seal(sealed.clone())])
            .await
            .unwrap(),
        0
    );
    assert_eq!(fixture.mesh.log.head(main).unwrap(), head);
    assert!(
        erin.1
            .tip_statements(None, 32)
            .await
            .unwrap()
            .iter()
            .all(|statement| statement.id().unwrap() != sealed.id().unwrap())
    );
    assert_eq!(erin.1.settle_tip_statements(&Nobody).await.unwrap(), 0);
    // Once she catches up from her peers she joins and seals.
    assert_eq!(
        erin.1
            .settle_tip_statements(&fixture.mesh.peers(&fixture.lineage, &erin.0))
            .await
            .unwrap(),
        1
    );
    let head = fixture.mesh.log.head(main).unwrap();
    assert_eq!(head.terminal.unwrap().digest, sealed.id().unwrap());
    assert_eq!(erin.1.coordinated_status(main).await.unwrap().position, 1);
}

/// A head whose window holds only its last entry.
struct Trimmed(Client);
impl DomainLog for Trimmed {
    fn pending_status(
        &self,
        log: TipLog,
        position: u64,
        hash: rocky_core::Commitment,
    ) -> CoordinationFuture<'_, rocky_core::LogStatus> {
        Box::pin(async move { Ok(self.log_head(log).await?.status(position, hash)) })
    }
    fn node(&self) -> NodeKey {
        self.0.node()
    }
    fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()> {
        self.0.configure(log, minimum)
    }
    fn seal(
        &self,
        log: TipLog,
        terminal: rocky_core::LogTerminal,
    ) -> CoordinationFuture<'_, LogResult> {
        self.0.seal(log, terminal)
    }
    fn recover(&self, log: TipLog, seal: Vec<u8>) -> CoordinationFuture<'_, LogResult> {
        self.0.recover(log, seal)
    }
    fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead> {
        Box::pin(async move {
            let mut head = self.0.log_head(log).await?;
            head.hashes = head.hashes.split_off(head.hashes.len().saturating_sub(1));
            Ok(head)
        })
    }
    fn join(
        &self,
        log: TipLog,
        position: u64,
        running: Commitment,
        sequence: u64,
    ) -> CoordinationFuture<'_, u64> {
        self.0.join(log, position, running, sequence)
    }
    fn append(&self, log: TipLog, append: Append) -> CoordinationFuture<'_, LogPending> {
        self.0.append(log, append)
    }
    fn acknowledge(&self, log: TipLog, pending: LogPending) -> CoordinationFuture<'_, LogResult> {
        self.0.acknowledge(log, pending)
    }
    fn acknowledge_terminal(
        &self,
        log: TipLog,
        acknowledgment: Vec<u8>,
    ) -> CoordinationFuture<'_, ()> {
        self.0.acknowledge_terminal(log, acknowledgment)
    }
}
/// Committed entries as reachable devices serve them, changed: one entry
/// swapped for a consistent other identity, or pages that never advance.
struct Altered {
    peers: Peers,
    stalled: bool,
}
impl EntryPeers for Altered {
    fn store<'a>(&'a self, node: PublicKey, entry: &'a CoordinatedEntry) -> PeerFuture<'a, ()> {
        self.peers.store(node, entry)
    }
    fn committed(&self, log: TipLog, after: u64) -> PeerFuture<'_, Vec<CommittedEntry>> {
        Box::pin(async move {
            // A page of one entry that never moves past the first.
            if self.stalled {
                let mut entries = self.peers.committed(log, 0).await?;
                entries.truncate(1);
                return Ok(entries);
            }
            let mut entries = self.peers.committed(log, after).await?;
            for entry in &mut entries {
                if entry.position == 2 {
                    entry.ids.pop();
                    entry
                        .frames
                        .retain(|frame| entry.ids.contains(&frame.id().unwrap()));
                    entry.hash = crate::replica::entry_hash(log, 2, &entry.ids).unwrap();
                }
            }
            Ok(entries)
        })
    }
}

#[tokio::test]
async fn a_prefix_before_the_window_is_endorsed_by_the_recovery_and_altered_entries_are_refused() {
    let fixture = Fixture::new(1).await;
    follow_main(&fixture).await;
    let main = tip_log(&fixture.lineage);
    for title in ["first", "second"] {
        fixture.invoice(&fixture.alice, title).await;
    }
    let dave = device(&fixture, &fixture.alice.1, "dave", "main").await;
    sync(&fixture.lineage, &fixture.alice.1, &dave.1, &dave.0).await;
    fixture.invoice(&fixture.alice, "lost").await;
    fixture.mesh.nodes.lock().unwrap().clear();
    fixture.mesh.add(&dave.0, &dave.1);
    fixture.mesh.log.set_owner(
        fixture.lineage.domain,
        fixture.lineage.publisher().public_key(),
    );
    // The owner retains the two entries Dave holds; a trusted node admits
    // the recovery, which ends the log.
    let head = fixture.mesh.log.head(main).unwrap().port();
    let running = |position: u64| {
        (1..=position).fold(head.genesis, |running, p| {
            rocky_core::coordinated_chain(running, p, head.hashes[p as usize - 1])
        })
    };
    let recovery = Recovery {
        retained: rocky_credentials::LogPoint {
            position: 2,
            running: running(2),
        },
        replaced: rocky_credentials::LogPoint {
            position: 3,
            running: head.running_hash,
        },
    };
    let sealed = recovery_seal(&fixture.lineage, &dave.1, main.tip, recovery).await;
    let next = TipLog {
        scope: main.scope,
        tip: sealed.successor().unwrap().unwrap(),
    };
    let fresh = trusted_device(&fixture, &dave.1, "fresh").await;
    fresh
        .1
        .receive_tip_statements(&[TipStatement::Seal(sealed)])
        .await
        .unwrap();
    assert_eq!(fresh.1.followed_log(main.scope).await, next);
    share(&fresh.1, &dave.1).await;
    // Before the window only the recovery's retained hash vouches for the
    // entries: a swapped entry or a peer that never advances is refused.
    for stalled in [false, true] {
        let name = if stalled { "stalled" } else { "tampered" };
        let altered = trusted_device(&fixture, &dave.1, name).await;
        altered
            .1
            .receive_tip_statements(&fresh.1.tip_statements(None, 32).await.unwrap())
            .await
            .unwrap();
        let slot = altered
            .1
            .coordinated_slot(
                next,
                &Trimmed(fixture.mesh.client(&altered.0)),
                &Altered {
                    peers: fixture.mesh.peers(&fixture.lineage, &altered.0),
                    stalled,
                },
            )
            .await;
        assert_eq!(
            slot,
            Err(CoordinatedError::Refused(CoordinationError::Denied)),
            "stalled: {stalled}"
        );
    }
    let slot = fresh
        .1
        .coordinated_slot(
            next,
            &Trimmed(fixture.mesh.client(&fresh.0)),
            &fixture.mesh.peers(&fixture.lineage, &fresh.0),
        )
        .await
        .unwrap();
    assert_eq!(slot.expected, 0);
    assert_eq!(fresh.1.coordinated_status(main).await.unwrap().position, 2);
    assert_eq!(
        rows(&fresh.1).await,
        [row("0001", "first"), row("0002", "second")]
    );
}

#[tokio::test]
async fn recovering_a_stuck_branch_tip_converges_its_rows_and_leaves_main_untouched() {
    let fixture = Fixture::new(1).await;
    let alice = &fixture.alice;
    follow_main(&fixture).await;
    let main = tip_log(&fixture.lineage);
    fixture.invoice(alice, "on main").await;
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
    invoice_on(&fixture, alice, "main again").await;
    // Carol writes on `dev`, on the rows it inherits; Erin, a member of
    // `dev`, holds her first invoice only.
    let carol = branch_device(&fixture, alice, "carol").await;
    trusted(&fixture, &carol);
    assert_eq!(rows(&carol.1).await, [row("0001", "on main")]);
    invoice_on(&fixture, &carol, "dev kept").await;
    let erin = branch_device(&fixture, alice, "erin").await;
    sync(&fixture.lineage, &carol.1, &erin.1, &erin.0).await;
    fixture.mesh.add(&erin.0, &erin.1);
    invoice_on(&fixture, &carol, "dev abandoned").await;
    assert_eq!(fixture.mesh.log.head(dev).unwrap().commit, 2);
    fixture
        .mesh
        .nodes
        .lock()
        .unwrap()
        .retain(|peer| peer.key.public_key() != carol.0.public_key());
    drop(carol);
    let before = fixture.mesh.log.head(next).unwrap();
    fixture.mesh.log.set_owner(
        fixture.lineage.domain,
        fixture.lineage.publisher().public_key(),
    );
    // Frank follows `dev` with its rows; while nobody serves `main`'s
    // entries its opening covered, he stays behind.
    let frank = branch_device(&fixture, alice, "frank").await;
    trusted(&fixture, &frank);
    for node in [&fixture.alice, &fixture.bob] {
        fixture.mesh.set(&node.0, false, true);
    }
    assert_eq!(
        slot_on(&fixture, &frank).await.map(|_| ()),
        Err(CoordinatedError::Behind)
    );
    for node in [&fixture.alice, &fixture.bob] {
        fixture.mesh.set(&node.0, true, true);
    }
    // Only Erin holds `dev`'s first entry; nobody its second.
    let recovery = proposal(&fixture, &frank, dev).await;
    assert_eq!(
        (recovery.retained.position, recovery.replaced.position),
        (1, 2)
    );
    let recovered = recovery_seal(&fixture.lineage, &frank.1, dev.tip, recovery).await;
    let resumed = TipLog {
        scope: main.scope,
        tip: recovered.successor().unwrap().unwrap(),
    };
    // The seal alone carries `dev` on; `main` stays where it was.
    frank
        .1
        .publish_tip_statements(&[TipStatement::Seal(recovered)])
        .await
        .unwrap();
    let graph = frank.1.tip_graph(main.scope).await.unwrap();
    assert_eq!(graph.bookmark("dev").unwrap(), Some(resumed.tip));
    assert_eq!(graph.bookmark("main").unwrap(), Some(next.tip));
    assert_eq!(frank.1.followed_log(main.scope).await, resumed);
    invoice_on(&fixture, &frank, "dev again").await;
    let expected = [
        row("0001", "on main"),
        row("0002", "dev kept"),
        row("0003", "dev again"),
    ];
    assert_eq!(rows(&frank.1).await, expected);
    share(&frank.1, &erin.1).await;
    assert_eq!(erin.1.followed_log(main.scope).await, resumed);
    sync(&fixture.lineage, &frank.1, &erin.1, &erin.0).await;
    assert_eq!(rows(&erin.1).await, expected);
    // `main`'s log, members and rows saw nothing of it.
    assert_eq!(fixture.mesh.log.head(next).unwrap(), before);
    invoice_on(&fixture, alice, "main after").await;
    assert_eq!(
        rows(&alice.1).await,
        [
            row("0001", "on main"),
            row("0002", "main again"),
            row("0003", "main after")
        ]
    );
}
