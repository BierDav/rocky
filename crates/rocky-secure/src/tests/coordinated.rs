//! Coordinated writes: invoices numbered without gaps on every device,
//! recovery at every step, members that only see committed entries and
//! catch-up before rejoining. See `docs/runtime/coordinated-writes.md`.
use super::generations::{Lineage, identity_insert, select, session, text};
use super::*;
use crate::replica::{
    CommittedEntry, CoordinatedEntry, CoordinatedError, ENTRY_DEADLINE, EntryPeers, PeerFuture,
};
use rocky_coordinator::coordinated::MemoryLog;
use rocky_core::{
    Append, CoordinationError, CoordinationFuture, DomainLog, LogHead, LogPending, LogResult,
    NodeKey, Scope, SqlValue, TipLog, TypedRows,
};
use std::sync::{Arc, Mutex as StdMutex};

mod finality;
mod recovery;
mod relay;
mod seals;
mod tips;

type Replica = EncryptedReplica<TypedRows>;

/// The log of the tip the fixtures' replicas follow: generation 3's.
fn tip_log(lineage: &Lineage) -> TipLog {
    TipLog {
        scope: lineage.domain,
        tip: rocky_credentials::genesis_tip(lineage.definition(3).hash()),
    }
}

/// The in-process transport between trusted nodes.
#[derive(Clone)]
struct Mesh {
    log: MemoryLog,
    nodes: Arc<StdMutex<Vec<Peer>>>,
}
#[derive(Clone)]
struct Peer {
    key: PrivateKey,
    replica: Replica,
    reachable: bool,
    /// Stores entries but crashes before acknowledging them.
    acknowledges: bool,
}
impl Mesh {
    fn new(minimum: usize, domain: Scope) -> Self {
        let log = MemoryLog::new();
        log.configure(domain, minimum).unwrap();
        Self {
            log,
            nodes: Arc::default(),
        }
    }
    fn add(&self, key: &PrivateKey, replica: &Replica) {
        let mut nodes = self.nodes.lock().unwrap();
        nodes.retain(|peer| peer.key.public_key() != key.public_key());
        nodes.push(Peer {
            key: key.clone(),
            replica: replica.clone(),
            reachable: true,
            acknowledges: true,
        });
    }
    fn set(&self, key: &PrivateKey, reachable: bool, acknowledges: bool) {
        for peer in self.nodes.lock().unwrap().iter_mut() {
            if peer.key.public_key() == key.public_key() {
                peer.reachable = reachable;
                peer.acknowledges = acknowledges;
            }
        }
    }
    fn peer(&self, key: PublicKey) -> Option<Peer> {
        self.nodes
            .lock()
            .unwrap()
            .iter()
            .find(|peer| peer.key.public_key() == key && peer.reachable)
            .cloned()
    }
    fn client(&self, key: &PrivateKey) -> Client {
        Client {
            inner: self.log.client(key.public_key()),
            acknowledges: true,
            observed: None,
        }
    }
    fn peers(&self, lineage: &Lineage, key: &PrivateKey) -> Peers {
        Peers {
            mesh: self.clone(),
            me: key.public_key(),
            credentials: lineage.credentials(3, key),
        }
    }
    /// Moves the coordinator's and the replicas' clocks together.
    fn advance(&self, seconds: u64) {
        self.log.advance(seconds);
        crate::CLOCK_SKEW.with(|skew| skew.set(skew.get() + seconds));
    }
}
struct Client {
    inner: rocky_coordinator::coordinated::MemoryLogClient,
    acknowledges: bool,
    observed: Option<Arc<tokio::sync::Notify>>,
}
impl DomainLog for Client {
    fn node(&self) -> NodeKey {
        self.inner.node()
    }
    fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()> {
        self.inner.configure(log, minimum)
    }
    fn seal(
        &self,
        log: TipLog,
        terminal: rocky_core::LogTerminal,
    ) -> CoordinationFuture<'_, LogResult> {
        self.inner.seal(log, terminal)
    }
    fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead> {
        self.inner.log_head(log)
    }
    fn join(
        &self,
        log: TipLog,
        position: u64,
        running_hash: Commitment,
        sequence: u64,
    ) -> CoordinationFuture<'_, u64> {
        self.inner.join(log, position, running_hash, sequence)
    }
    fn append(&self, log: TipLog, append: Append) -> CoordinationFuture<'_, LogPending> {
        self.inner.append(log, append)
    }
    fn acknowledge(&self, log: TipLog, pending: LogPending) -> CoordinationFuture<'_, LogResult> {
        if !self.acknowledges {
            return Box::pin(async { Err(CoordinationError::Unavailable) });
        }
        Box::pin(async move {
            let result = self.inner.acknowledge(log, pending).await;
            if result.is_ok()
                && let Some(observed) = &self.observed
            {
                observed.notify_one();
            }
            result
        })
    }
    fn recover(&self, log: TipLog, seal: Vec<u8>) -> CoordinationFuture<'_, LogResult> {
        self.inner.recover(log, seal)
    }
    fn acknowledge_terminal(
        &self,
        log: TipLog,
        acknowledgment: Vec<u8>,
    ) -> CoordinationFuture<'_, ()> {
        self.inner.acknowledge_terminal(log, acknowledgment)
    }
}
struct Peers {
    mesh: Mesh,
    me: PublicKey,
    credentials: CredentialChain,
}
impl EntryPeers for Peers {
    fn store<'a>(&'a self, node: PublicKey, entry: &'a CoordinatedEntry) -> PeerFuture<'a, ()> {
        Box::pin(async move {
            let peer = self
                .mesh
                .peer(node)
                .ok_or_else(|| Error::Rejected("unreachable".into()))?;
            let mut client = self.mesh.client(&peer.key);
            client.acknowledges = peer.acknowledges;
            peer.replica
                .store_entry(entry.clone(), &client)
                .await
                .map(|_| ())
                .map_err(|error| Error::Rejected(error.to_string()))
        })
    }
    fn committed(&self, log: TipLog, after: u64) -> PeerFuture<'_, Vec<CommittedEntry>> {
        Box::pin(async move {
            let nodes = self.mesh.nodes.lock().unwrap().clone();
            // A live link's handshake claims the requester's followed tip.
            let me = nodes.iter().find(|peer| peer.key.public_key() == self.me);
            let tip = match me {
                Some(me) => Some(me.replica.followed_tip().await),
                None => None,
            };
            let mut collected = std::collections::BTreeMap::<u64, CommittedEntry>::new();
            for peer in nodes
                .iter()
                .filter(|peer| peer.reachable && peer.key.public_key() != self.me)
            {
                if let Some(tip) = tip
                    && peer
                        .replica
                        .admit_peer(self.me, &self.credentials, tip)
                        .await
                        .is_err()
                {
                    continue;
                }
                let Ok(entries) = peer
                    .replica
                    .committed_entries(self.me, &self.credentials, log, after)
                    .await
                else {
                    continue;
                };
                for entry in entries {
                    if let Some(held) = collected.get(&entry.position)
                        && held.hash != entry.hash
                    {
                        return Err(Error::Rejected(
                            "invalid committed evidence: conflicting peer entries".into(),
                        ));
                    }
                    collected.entry(entry.position).or_insert(entry);
                }
            }
            Ok(collected.into_values().collect())
        })
    }
}

/// A two-node fixture: alice and bob are trusted nodes, carol a member.
struct Fixture {
    lineage: Lineage,
    dir: TempDir,
    mesh: Mesh,
    alice: (PrivateKey, Replica),
    bob: (PrivateKey, Replica),
}
impl Fixture {
    async fn new(minimum: usize) -> Self {
        Self::with_lineage(minimum, Lineage::new()).await
    }
    async fn with_lineage(minimum: usize, lineage: Lineage) -> Self {
        Self::with_keys(minimum, lineage, [PrivateKey::new(), PrivateKey::new()]).await
    }
    async fn with_keys(
        minimum: usize,
        lineage: Lineage,
        [alice_key, bob_key]: [PrivateKey; 2],
    ) -> Self {
        let dir = TempDir::new().unwrap();
        let mesh = Mesh::new(minimum, lineage.domain);
        let alice = open(&lineage, &dir, "alice", &alice_key).await;
        let bob = open(&lineage, &dir, "bob", &bob_key).await;
        mesh.add(&alice_key, &alice);
        mesh.add(&bob_key, &bob);
        let fixture = Self {
            lineage,
            dir,
            mesh,
            alice: (alice_key, alice),
            bob: (bob_key, bob),
        };
        // Both join; the first is alone below a minimum of 2.
        let first = fixture.slot(&fixture.alice).await;
        assert!(
            first.is_ok() || (minimum == 2 && first == Err(CoordinatedError::BelowMinimum)),
            "{first:?}"
        );
        fixture.slot(&fixture.bob).await.unwrap();
        fixture
    }
    /// Crashes a node: every handle is dropped, then the file reopens.
    async fn restart(self, alice: bool) -> Self {
        let Self {
            lineage,
            dir,
            mesh,
            alice: first,
            bob: second,
        } = self;
        let (name, (key, replica), other) = if alice {
            ("alice", first, second)
        } else {
            ("bob", second, first)
        };
        mesh.nodes
            .lock()
            .unwrap()
            .retain(|peer| peer.key.public_key() != key.public_key());
        drop(replica);
        let replica = open(&lineage, &dir, name, &key).await;
        mesh.add(&key, &replica);
        let node = (key, replica);
        let (alice, bob) = if alice { (node, other) } else { (other, node) };
        Self {
            lineage,
            dir,
            mesh,
            alice,
            bob,
        }
    }
    async fn slot(&self, node: &(PrivateKey, Replica)) -> Result<Slot, CoordinatedError> {
        node.1
            .coordinated_slot(
                tip_log(&self.lineage),
                &self.mesh.client(&node.0),
                &self.mesh.peers(&self.lineage, &node.0),
            )
            .await
    }
    /// Stages the next invoice computed on the slot's committed state.
    async fn stage(
        &self,
        node: &(PrivateKey, Replica),
        slot: Slot,
        title: &str,
    ) -> Result<Vec<EncryptedFrame>, CoordinatedError> {
        let sql = session(&node.1).await;
        sql.run("BEGIN", vec![]).await?;
        let count = match sql
            .run("SELECT COUNT(*) FROM items", vec![])
            .await?
            .rows
            .as_slice()
        {
            [row] => match row.as_slice() {
                [SqlValue::Integer(count)] => *count,
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        identity_insert(
            &sql,
            "INSERT INTO items(key,title) VALUES(?,?)",
            vec![text(&format!("{:04}", count + 1)), text(title)],
        )
        .await?;
        let mut handle = [0; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut handle);
        let committed = sql.commit_coordinated(handle, slot).await?;
        Ok(committed.committed.unwrap().frames)
    }
    async fn propose(
        &self,
        node: &(PrivateKey, Replica),
    ) -> Result<crate::replica::CoordinatedCommit, CoordinatedError> {
        node.1
            .propose(
                tip_log(&self.lineage),
                &self.mesh.client(&node.0),
                &self.mesh.peers(&self.lineage, &node.0),
            )
            .await
    }
    async fn resolve(&self, node: &(PrivateKey, Replica)) -> crate::replica::CoordinatedStatus {
        node.1
            .resolve_coordinated(
                tip_log(&self.lineage),
                &self.mesh.client(&node.0),
                &self.mesh.peers(&self.lineage, &node.0),
            )
            .await
            .unwrap()
    }
    /// Issues one invoice, retrying lost races; returns its frames.
    async fn invoice(&self, node: &(PrivateKey, Replica), title: &str) -> Vec<EncryptedFrame> {
        for _ in 0..200 {
            let attempt = async {
                let slot = self.slot(node).await?;
                self.stage(node, slot, title).await?;
                self.propose(node).await
            };
            match attempt.await {
                Ok(commit) => return commit.frames,
                Err(
                    CoordinatedError::Conflict
                    | CoordinatedError::Aborted
                    | CoordinatedError::Undecided,
                ) => tokio::time::sleep(std::time::Duration::from_millis(2)).await,
                Err(error) => panic!("{error:?}"),
            }
        }
        panic!("no invoice committed")
    }
}
async fn open(lineage: &Lineage, dir: &TempDir, name: &str, key: &PrivateKey) -> Replica {
    lineage
        .open_trusted(
            &dir.path().join(format!("{name}.db")),
            (&PrivateKey::new(), key),
            3,
            None,
        )
        .await
        .unwrap()
}
async fn invoices(replica: &Replica) -> Vec<String> {
    select(replica, "SELECT key FROM items ORDER BY key")
        .await
        .into_iter()
        .map(|row| match row.as_slice() {
            [SqlValue::Text(key)] => key.clone(),
            other => panic!("{other:?}"),
        })
        .collect()
}
fn numbered(count: usize) -> Vec<String> {
    (1..=count).map(|number| format!("{number:04}")).collect()
}

#[tokio::test]
async fn a_fresh_trusted_node_rebuilds_from_a_member_after_the_in_sync_set_is_lost() {
    let fixture = Fixture::new(1).await;
    let domain = tip_log(&fixture.lineage);
    for _ in 0..MAX_CATCH_UP_ENTRIES + 1 {
        fixture.invoice(&fixture.alice, "retained").await;
    }
    let member_key = PrivateKey::new();
    let member = open(&fixture.lineage, &fixture.dir, "member", &member_key).await;
    sync(&fixture.lineage, &fixture.alice.1, &member, &member_key).await;
    assert_eq!(invoices(&member).await, numbered(MAX_CATCH_UP_ENTRIES + 1));
    assert_eq!(member.coordinated_status(domain).await.unwrap().position, 0);
    let Fixture {
        lineage,
        dir,
        mesh,
        alice,
        bob,
    } = fixture;
    mesh.nodes.lock().unwrap().clear();
    drop(alice);
    drop(bob);
    for name in ["alice", "bob"] {
        std::fs::remove_file(dir.path().join(format!("{name}.db"))).unwrap();
    }
    mesh.add(&member_key, &member);
    let key = PrivateKey::new();
    let replacement = open(&lineage, &dir, "replacement", &key).await;
    mesh.add(&key, &replacement);
    let mut client = mesh.client(&key);
    let acknowledged = Arc::new(tokio::sync::Notify::new());
    client.observed = Some(acknowledged.clone());
    let peers = mesh.peers(&lineage, &key);
    let slot = replacement
        .coordinated_slot(domain, &client, &peers)
        .await
        .unwrap();
    assert_eq!(slot.expected, MAX_CATCH_UP_ENTRIES as u64 + 1);
    assert_eq!(
        invoices(&replacement).await,
        numbered(MAX_CATCH_UP_ENTRIES + 1)
    );
    // The old set still exists at the coordinator. Missing acknowledgments
    // evict it at the deadline; the recovered node alone meets the minimum.
    let sql = session(&replacement).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    identity_insert(
        &sql,
        "INSERT INTO items(key,title) VALUES(?,?)",
        vec![
            text(&format!("{:04}", MAX_CATCH_UP_ENTRIES + 2)),
            text("continued"),
        ],
    )
    .await
    .unwrap();
    sql.commit_coordinated([9; 32], slot).await.unwrap();
    drop(sql);
    let (commit, ()) = tokio::join!(replacement.propose(domain, &client, &peers), async {
        acknowledged.notified().await;
        mesh.advance(ENTRY_DEADLINE + 1);
    });
    assert_eq!(commit.unwrap().position, MAX_CATCH_UP_ENTRIES as u64 + 2);
    sync(&lineage, &replacement, &member, &member_key).await;
    assert_eq!(invoices(&member).await, numbered(MAX_CATCH_UP_ENTRIES + 2));
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
}

#[tokio::test]
async fn a_missing_copy_waits_and_a_complete_chain_outside_the_hash_window_rejoins() {
    let fixture = Fixture::new(1).await;
    fixture.invoice(&fixture.alice, "retained").await;
    let key = PrivateKey::new();
    let replica = open(&fixture.lineage, &fixture.dir, "behind", &key).await;
    let node = (key, replica);
    let member_key = PrivateKey::new();
    let member = open(&fixture.lineage, &fixture.dir, "member", &member_key).await;
    sync(&fixture.lineage, &fixture.alice.1, &member, &member_key).await;
    fixture.mesh.nodes.lock().unwrap().clear();
    fixture.mesh.add(&node.0, &node.1);
    assert_eq!(fixture.slot(&node).await, Err(CoordinatedError::Behind));
    assert!(invoices(&node.1).await.is_empty());
    // A head without the entry hash models a gap older than its window.
    struct OutsideWindow(rocky_coordinator::coordinated::MemoryLogClient);
    impl DomainLog for OutsideWindow {
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
        fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead> {
            Box::pin(async move {
                let mut head = self.0.log_head(log).await?;
                head.hashes.clear();
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
        fn acknowledge(
            &self,
            log: TipLog,
            pending: LogPending,
        ) -> CoordinationFuture<'_, LogResult> {
            self.0.acknowledge(log, pending)
        }
        fn recover(&self, log: TipLog, seal: Vec<u8>) -> CoordinationFuture<'_, LogResult> {
            self.0.recover(log, seal)
        }
        fn acknowledge_terminal(
            &self,
            log: TipLog,
            acknowledgment: Vec<u8>,
        ) -> CoordinationFuture<'_, ()> {
            self.0.acknowledge_terminal(log, acknowledgment)
        }
    }
    fixture.mesh.add(&member_key, &member);
    let client = OutsideWindow(fixture.mesh.log.client(node.0.public_key()));
    node.1
        .coordinated_slot(
            tip_log(&fixture.lineage),
            &client,
            &fixture.mesh.peers(&fixture.lineage, &node.0),
        )
        .await
        .unwrap();
    assert_eq!(invoices(&node.1).await, ["0001"]);
    assert_eq!(
        node.1
            .coordinated_status(tip_log(&fixture.lineage))
            .await
            .unwrap()
            .position,
        1
    );
    assert!(
        fixture
            .mesh
            .log
            .head(tip_log(&fixture.lineage))
            .unwrap()
            .member(node.0.public_key())
    );
}
/// Syncs `target` from `source` by range pages, voids first, as a member does.
async fn sync(lineage: &Lineage, source: &Replica, target: &Replica, key: &PrivateKey) {
    let scope = lineage.scope("items");
    let credentials = lineage.credentials(3, key);
    source
        .admit_peer(key.public_key(), &credentials, target.followed_tip().await)
        .await
        .unwrap();
    let mut after = None;
    loop {
        let frontier = target.frontier(scope).await.unwrap();
        let page = source
            .range_page(
                key.public_key(),
                &credentials,
                &frontier,
                after,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        for status in target.receive_voids(&page.voids).await {
            status.unwrap();
        }
        for status in target.receive_all(page.frames).await {
            status.unwrap();
        }
        after = page.next;
        if after.is_none() {
            return;
        }
    }
}
async fn held(replica: &Replica) -> BTreeSet<Hash> {
    stored_frames(replica, "SELECT header,body FROM secure_operations")
        .await
        .iter()
        .map(|frame| frame.id().unwrap())
        .collect()
}

#[tokio::test]
async fn concurrent_invoices_are_sequential_on_every_device_and_members_see_no_aborted_entry() {
    let fixture = Fixture::new(2).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    // Both compute invoice 1 on the same committed state.
    let alice_slot = fixture.slot(alice).await.unwrap();
    let bob_slot = fixture.slot(bob).await.unwrap();
    assert_eq!(alice_slot.expected, bob_slot.expected);
    fixture.stage(alice, alice_slot, "alice").await.unwrap();
    let lost = fixture.stage(bob, bob_slot, "bob").await.unwrap();
    assert_eq!(invoices(&bob.1).await, numbered(1));
    // Neither staged entry leaves its node before it commits.
    let carol_key = PrivateKey::new();
    let carol = open(&fixture.lineage, &fixture.dir, "carol", &carol_key).await;
    sync(&fixture.lineage, &bob.1, &carol, &carol_key).await;
    assert!(invoices(&carol).await.is_empty());
    let page = bob
        .1
        .transfer_page(
            carol_key.public_key(),
            &fixture.lineage.credentials(3, &carol_key),
            &[fixture.lineage.scope("items")],
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(page.frames.is_empty());
    // A checkpoint could not roll the entry back; none is made.
    assert!(
        bob.1
            .checkpoint(fixture.lineage.scope("items"))
            .await
            .is_err()
    );
    let (won, refused) = tokio::join!(fixture.propose(alice), async {
        tokio::task::yield_now().await;
        fixture.propose(bob).await
    });
    let won = won.unwrap();
    assert_eq!(won.position, 1);
    assert!(
        matches!(refused, Err(CoordinatedError::Conflict)),
        "{refused:?}"
    );
    // The loser's invoice 1 is voided and rolled back; alice's is stored
    // on bob and applies there once bob resolves.
    fixture.resolve(bob).await;
    assert_eq!(invoices(&bob.1).await, numbered(1));
    let lost: BTreeSet<Hash> = lost.iter().map(|frame| frame.id().unwrap()).collect();
    assert!(held(&bob.1).await.is_disjoint(&lost));

    // More invoices from both nodes at once.
    let issue = |node: &'static str| {
        let fixture = &fixture;
        async move {
            let party = if node == "alice" {
                &fixture.alice
            } else {
                &fixture.bob
            };
            let mut frames = Vec::new();
            for _ in 0..4 {
                frames.extend(fixture.invoice(party, node).await);
            }
            frames
        }
    };
    tokio::join!(issue("alice"), issue("bob"));
    fixture.resolve(alice).await;
    fixture.resolve(bob).await;
    for replica in [&alice.1, &bob.1] {
        assert_eq!(invoices(replica).await, numbered(9));
    }
    // A member syncing from both trusted nodes sees the same numbers and
    // never the aborted entry.
    sync(&fixture.lineage, &bob.1, &carol, &carol_key).await;
    sync(&fixture.lineage, &alice.1, &carol, &carol_key).await;
    assert_eq!(invoices(&carol).await, numbered(9));
    assert!(held(&carol).await.is_disjoint(&lost));
    let status = alice
        .1
        .coordinated_status(tip_log(&fixture.lineage))
        .await
        .unwrap();
    assert_eq!((status.position, status.undecided), (9, 0));
}

#[tokio::test]
async fn importing_a_checkpoint_never_reuses_a_voided_author_position() {
    let fixture = Fixture::new(1).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    seals::follow_main(&fixture).await;
    let scope = fixture.lineage.scope("items");
    let first = fixture.slot(alice).await.unwrap();
    let second = fixture.slot(bob).await.unwrap();
    fixture.stage(alice, first, "alice").await.unwrap();
    let lost = fixture.stage(bob, second, "bob").await.unwrap();
    fixture.propose(alice).await.unwrap();
    assert!(matches!(
        fixture.propose(bob).await,
        Err(CoordinatedError::Conflict)
    ));
    fixture.resolve(bob).await;

    // Alice's base at the seal has never seen Bob's consumed position.
    let main = tip_log(&fixture.lineage);
    let sealed = seals::seal(&fixture.lineage, &alice.1, main.tip).await;
    let next = TipLog {
        scope: main.scope,
        tip: sealed.successor().unwrap().unwrap(),
    };
    alice
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(sealed),
            seals::bookmark(&fixture.lineage, "main", Some(main.tip), Some(next.tip)),
        ])
        .await
        .unwrap();
    // Joining the continuation's log gives alice every held scope, so she
    // compacts each, the unwritten one too.
    alice
        .1
        .resolve_coordinated(
            next,
            &fixture.mesh.client(&alice.0),
            &fixture.mesh.peers(&fixture.lineage, &alice.0),
        )
        .await
        .unwrap();
    alice.1.compact(scope.domain, 0).await.unwrap();
    link_up(&alice.1, &bob.1, scope.domain).await;
    let credentials = fixture.lineage.credentials(3, &bob.0);
    // Bob takes every scope's base at the seal, as a rebootstrap at seals does.
    let scopes: Vec<_> = alice
        .1
        .inner
        .lock()
        .await
        .config
        .keys
        .iter()
        .map(|key| key.scope)
        .collect();
    for scope in scopes {
        let Some(offered) = alice
            .1
            .checkpoint_offer(bob.0.public_key(), &credentials, scope)
            .await
            .unwrap()
        else {
            continue;
        };
        let checkpoint = offered.id().unwrap();
        bob.1.begin_rebootstrap(offered).await.unwrap();
        let mut after = None;
        loop {
            let page = alice
                .1
                .checkpoint_page(
                    bob.0.public_key(),
                    &credentials,
                    scope,
                    checkpoint,
                    after,
                    64 * 1024,
                )
                .await
                .unwrap();
            after = page.next;
            bob.1.stage_checkpoint_page(page).await.unwrap();
            if after.is_none() {
                break;
            }
        }
        bob.1.finish_rebootstrap(scope).await.unwrap();
    }
    seals::invoice_on(&fixture, bob, "bob").await;
    let frames = stored_frames(&bob.1, "SELECT header,body FROM secure_operations").await;
    let continued = frames
        .iter()
        .map(|frame| frame.operation().unwrap())
        .filter(|operation| operation.header.public_key == bob.0.public_key())
        .max_by_key(|operation| operation.header.seq_num)
        .unwrap();
    let previous = lost.last().unwrap().operation().unwrap();
    assert_eq!(continued.header.seq_num, previous.header.seq_num + 1);
    assert_eq!(continued.header.backlink, Some(previous.hash));
    alice
        .1
        .resolve_coordinated(
            next,
            &fixture.mesh.client(&alice.0),
            &fixture.mesh.peers(&fixture.lineage, &alice.0),
        )
        .await
        .unwrap();
    for node in [alice, bob] {
        assert_eq!(invoices(&node.1).await, numbered(2));
        node.1.audit(scope).await.unwrap();
    }
}

/// A live link disappears once after assignment, then becomes available again.
struct Reconnecting {
    peers: Peers,
    attempts: std::sync::atomic::AtomicUsize,
}
impl EntryPeers for Reconnecting {
    fn store<'a>(&'a self, node: PublicKey, entry: &'a CoordinatedEntry) -> PeerFuture<'a, ()> {
        Box::pin(async move {
            if self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                return Err(Error::Rejected("link is reconnecting".into()));
            }
            self.peers.store(node, entry).await
        })
    }
    fn committed(&self, log: TipLog, after: u64) -> PeerFuture<'_, Vec<CommittedEntry>> {
        self.peers.committed(log, after)
    }
}

#[tokio::test]
async fn a_proposal_retries_storage_after_a_transient_link_failure() {
    let fixture = Fixture::new(2).await;
    let alice = &fixture.alice;
    let slot = fixture.slot(alice).await.unwrap();
    fixture.stage(alice, slot, "alice").await.unwrap();
    let peers = Reconnecting {
        peers: fixture.mesh.peers(&fixture.lineage, &alice.0),
        attempts: Default::default(),
    };
    let client = fixture.mesh.client(&alice.0);
    let committed = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        alice.1.propose(tip_log(&fixture.lineage), &client, &peers),
    )
    .await
    .expect("storage must retry before the entry deadline")
    .unwrap();
    assert_eq!(committed.position, 1);
    assert_eq!(peers.attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    fixture.resolve(&fixture.bob).await;
    for node in [&fixture.alice, &fixture.bob] {
        assert_eq!(invoices(&node.1).await, numbered(1));
        assert_eq!(fixture.resolve(node).await.undecided, 0);
    }
}

#[tokio::test]
async fn a_proposer_crashing_before_its_append_commits_it_after_restart() {
    let fixture = Fixture::new(2).await;
    let slot = fixture.slot(&fixture.alice).await.unwrap();
    fixture.stage(&fixture.alice, slot, "alice").await.unwrap();
    let fixture = fixture.restart(true).await;
    // Unsent until the restart; resolving appends, stores and commits it.
    assert_eq!(
        fixture
            .alice
            .1
            .coordinated_status(tip_log(&fixture.lineage))
            .await
            .unwrap()
            .undecided,
        1
    );
    let status = fixture.resolve(&fixture.alice).await;
    assert_eq!((status.position, status.undecided), (1, 0));
    fixture.resolve(&fixture.bob).await;
    assert_eq!(invoices(&fixture.alice.1).await, numbered(1));
    assert_eq!(invoices(&fixture.bob.1).await, numbered(1));
}

#[tokio::test]
async fn crashes_before_acknowledging_never_leave_an_entry_true_on_one_node_only() {
    for minimum in [1, 2] {
        // The proposer stores on bob, then crashes before its own
        // acknowledgment: the deadline decides for both.
        let fixture = Fixture::new(minimum).await;
        let slot = fixture.slot(&fixture.alice).await.unwrap();
        fixture.stage(&fixture.alice, slot, "alice").await.unwrap();
        let mut crashing = fixture.mesh.client(&fixture.alice.0);
        crashing.acknowledges = false;
        let peers = fixture.mesh.peers(&fixture.lineage, &fixture.alice.0);
        let proposal = fixture
            .alice
            .1
            .propose(tip_log(&fixture.lineage), &crashing, &peers);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), proposal)
                .await
                .is_err()
        );
        let fixture = fixture.restart(true).await;
        fixture.mesh.advance(ENTRY_DEADLINE + 1);
        let alice = fixture.resolve(&fixture.alice).await;
        let bob = fixture.resolve(&fixture.bob).await;
        // With a minimum of 1 bob alone commits it; with 2 it aborts.
        let committed = u64::from(minimum == 1);
        assert_eq!((alice.position, bob.position), (committed, committed));
        let expected = numbered(committed as usize);
        assert_eq!(
            invoices(&fixture.alice.1).await,
            expected,
            "minimum {minimum}"
        );
        assert_eq!(
            invoices(&fixture.bob.1).await,
            expected,
            "minimum {minimum}"
        );
        crate::CLOCK_SKEW.with(|skew| skew.set(0));

        // Bob stores alice's next entry and crashes before acknowledging.
        let fixture = Fixture::new(minimum).await;
        let slot = fixture.slot(&fixture.alice).await.unwrap();
        fixture.stage(&fixture.alice, slot, "alice").await.unwrap();
        fixture.mesh.set(&fixture.bob.0, true, false);
        let (decided, ()) = tokio::join!(fixture.propose(&fixture.alice), async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            fixture.mesh.advance(ENTRY_DEADLINE + 1);
        });
        let fixture = fixture.restart(false).await;
        let bob = &fixture.bob;
        let status = fixture.resolve(bob).await;
        if minimum == 1 {
            assert_eq!(decided.unwrap().position, 1);
            assert_eq!(status.position, 1);
        } else {
            assert!(
                matches!(decided, Err(CoordinatedError::Aborted)),
                "{decided:?}"
            );
            assert_eq!(status.position, 0);
        }
        let expected = numbered(if minimum == 1 { 1 } else { 0 });
        assert_eq!(invoices(&fixture.alice.1).await, expected);
        assert_eq!(invoices(&bob.1).await, expected);
        assert_eq!(status.undecided, 0);
        crate::CLOCK_SKEW.with(|skew| skew.set(0));
    }
}

#[tokio::test]
async fn a_node_that_missed_entries_proposes_only_after_catching_up() {
    let fixture = Fixture::new(1).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    fixture.slot(bob).await.unwrap();
    // Bob is unreachable: alice's first entry evicts it at the deadline.
    fixture.mesh.set(&bob.0, false, true);
    let slot = fixture.slot(alice).await.unwrap();
    fixture.stage(alice, slot, "alice").await.unwrap();
    let (first, ()) = tokio::join!(fixture.propose(alice), async {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        fixture.mesh.advance(ENTRY_DEADLINE + 1);
    });
    assert_eq!(first.unwrap().position, 1);
    for _ in 0..2 {
        fixture.invoice(alice, "alice").await;
    }
    let head = fixture.mesh.log.head(tip_log(&fixture.lineage)).unwrap();
    assert_eq!((head.commit, head.members.len()), (3, 1));
    // Back, but no trusted peer can supply the entries yet: no slot.
    fixture.mesh.set(&bob.0, true, true);
    fixture.mesh.set(&alice.0, false, true);
    assert!(matches!(
        fixture.slot(bob).await,
        Err(CoordinatedError::Behind)
    ));
    assert!(invoices(&bob.1).await.is_empty());
    // Caught up from alice, bob rejoins and numbers after every entry.
    fixture.mesh.set(&alice.0, true, true);
    let slot = fixture.slot(bob).await.unwrap();
    assert_eq!(slot.expected, 3);
    assert_eq!(invoices(&bob.1).await, numbered(3));
    fixture.invoice(bob, "bob").await;
    fixture.resolve(alice).await;
    assert_eq!(invoices(&alice.1).await, numbered(4));
    assert_eq!(invoices(&bob.1).await, numbered(4));
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
}

#[tokio::test]
async fn without_a_coordinator_or_below_the_minimum_a_write_fails_typed_and_rolls_back() {
    // Fixture::new checks that the first node alone is below the minimum.
    let fixture = Fixture::new(2).await;
    let alice = &fixture.alice;
    let slot = fixture.slot(alice).await.unwrap();
    let staged = fixture.stage(alice, slot, "alice").await.unwrap();
    fixture.mesh.log.set_available(false);
    assert!(matches!(
        fixture.propose(alice).await,
        Err(CoordinatedError::Unavailable)
    ));
    // Nothing stays: the frames are voided and the invoice rolled back.
    assert!(invoices(&alice.1).await.is_empty());
    let staged: BTreeSet<Hash> = staged.iter().map(|frame| frame.id().unwrap()).collect();
    assert!(held(&alice.1).await.is_disjoint(&staged));
    assert!(matches!(
        fixture.slot(alice).await,
        Err(CoordinatedError::Unavailable)
    ));
    fixture.mesh.log.set_available(true);
    fixture.invoice(alice, "alice").await;
    fixture.resolve(&fixture.bob).await;
    assert_eq!(invoices(&fixture.bob.1).await, numbered(1));
}

/// A trusted peer reached over a live link, each request on its own channel.
struct Linked {
    local: (PrivateKey, Replica, CredentialChain),
    remote: (PrivateKey, Replica, CredentialChain),
}
impl Linked {
    async fn session(&self) -> Result<crate::network::NativeSession, Error> {
        let (session, mut served) = crate::network::link::testing::sessions(
            (&self.local.1, self.local.0.clone(), self.local.2.clone()),
            (&self.remote.1, self.remote.0.clone(), self.remote.2.clone()),
        )
        .await?;
        let remote = self.remote.1.clone();
        tokio::spawn(async move {
            let _ = served.serve(&remote).await;
        });
        Ok(session)
    }
}
impl EntryPeers for Linked {
    fn store<'a>(&'a self, node: PublicKey, entry: &'a CoordinatedEntry) -> PeerFuture<'a, ()> {
        Box::pin(async move {
            assert_eq!(node, self.remote.0.public_key());
            let mut session = self.session().await?;
            session.store_entry(entry).await?;
            session.close().await
        })
    }
    fn committed(&self, log: TipLog, after: u64) -> PeerFuture<'_, Vec<CommittedEntry>> {
        Box::pin(async move {
            let mut session = self.session().await?;
            let entries = session.committed_entries(log, after).await?;
            session.close().await?;
            Ok(entries)
        })
    }
}

#[tokio::test]
async fn entries_travel_over_live_links_and_catch_up_reads_their_positions() {
    let fixture = Fixture::new(2).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    let domain = tip_log(&fixture.lineage);
    let end = |node: &(PrivateKey, Replica)| {
        (
            node.0.clone(),
            node.1.clone(),
            fixture.lineage.credentials(3, &node.0),
        )
    };
    // Bob acknowledges what it stores through its own client.
    bob.1
        .set_domain_log(Some(Arc::new(fixture.mesh.client(&bob.0))));
    let to_bob = Linked {
        local: end(alice),
        remote: end(bob),
    };
    // An entry of 70 rows: three frames.
    let slot = fixture.slot(alice).await.unwrap();
    let sql = session(&alice.1).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    for number in 1..=70 {
        identity_insert(
            &sql,
            "INSERT INTO items(key,title) VALUES(?,?)",
            vec![text(&format!("{number:04}")), text("batch")],
        )
        .await
        .unwrap();
    }
    sql.commit_coordinated([7; 32], slot).await.unwrap();
    drop(sql);
    let commit = alice
        .1
        .propose(domain, &fixture.mesh.client(&alice.0), &to_bob)
        .await
        .unwrap();
    assert_eq!((commit.position, commit.frames.len()), (1, 3));
    // Every member carries the log position; the hash commits to its frames.
    let ids: Vec<Hash> = commit
        .frames
        .iter()
        .map(|frame| frame.id().unwrap())
        .collect();
    assert_eq!(
        crate::replica::entry_hash(domain, 1, &ids).unwrap(),
        commit.hash
    );
    for frame in &commit.frames {
        let operation = frame.operation().unwrap();
        let key = {
            let inner = bob.1.inner.lock().await;
            inner
                .config
                .keys
                .iter()
                .find(|key| key.scope == operation.header.extensions.scope)
                .unwrap()
                .key
        };
        let payload = bob
            .1
            .inner
            .lock()
            .await
            .decode_row_payload(
                frame,
                &crate::wire::decrypt(
                    &key,
                    &encode(&operation.header.extensions).unwrap(),
                    frame.body_bytes(),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(payload.membership().unwrap().entry, Some(1));
    }
    fixture.resolve(bob).await;
    assert_eq!(invoices(&bob.1).await, numbered(70));
    // Bob serves the committed position and frame ids; frames travel by
    // range sync, so a node lacking them is still behind.
    let from_bob = Linked {
        local: end(alice),
        remote: end(bob),
    };
    let entries = from_bob.committed(domain, 0).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        (entries[0].position, entries[0].hash, entries[0].ids.len()),
        (1, commit.hash, 3)
    );
    assert!(entries[0].frames.is_empty());
}

#[tokio::test]
async fn a_trusted_node_that_cannot_hold_an_entrys_scope_stores_and_acknowledges_nothing() {
    let fixture = Fixture::new(1).await;
    let key = PrivateKey::new();
    let dana = fixture
        .lineage
        .open_scoped(&fixture.dir.path().join("dana.db"), &key, true, &["tags"])
        .await
        .unwrap();
    let alice = &fixture.alice;
    let slot = fixture.slot(alice).await.unwrap();
    fixture.stage(alice, slot, "invoice").await.unwrap();
    let commit = fixture.propose(alice).await.unwrap();
    let log = tip_log(&fixture.lineage);
    let entry = CoordinatedEntry {
        log,
        pending: rocky_core::LogPending {
            proposer: *alice.0.public_key().as_bytes(),
            hash: commit.hash,
            position: commit.position,
            epoch: 0,
            deadline: 0,
        },
        frames: commit.frames,
        voids: Vec::new(),
        carry: None,
    };
    let refused = dana
        .store_entry(entry, &fixture.mesh.client(&key))
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("cannot hold"), "{refused}");
    let inner = dana.inner.lock().await;
    assert!(
        crate::storage::rows(&inner.conn, "SELECT 1 FROM secure_entries")
            .await
            .unwrap()
            .is_empty()
    );
}
