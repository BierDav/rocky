//! Carry records: a coordinated write a `continue` seal aborted keeps its
//! frames, and its author's record places them at the continuation's first
//! entry on every device, in any arrival order.
use super::*;
use crate::replica::entry_hash;
use rocky_credentials::{CARRY_FORMAT, CarryStatement, SignedCarry};
use std::future::Future;
use std::pin::Pin;

const HANDLE: [u8; 32] = [71; 32];

/// Alice stages a write on `main`'s tip A; bob seals A into B before she
/// proposes, so her write is carried and commits as B's first entry.
/// `before` invoices commit on A first.
struct Carried {
    f: Fixture,
    original: TipLog,
    next: TipLog,
}
impl Carried {
    async fn new(before: usize) -> Self {
        Self::staging(Lineage::new(), before, |f| {
            Box::pin(stage_with(f, &f.alice, "carried", HANDLE))
        })
        .await
    }
    async fn staging(
        lineage: Lineage,
        before: usize,
        stage: impl for<'a> FnOnce(&'a Fixture) -> Pin<Box<dyn Future<Output = ()> + 'a>>,
    ) -> Self {
        let f = Fixture::with_lineage(1, lineage).await;
        follow_main(&f).await;
        let original = tip_log(&f.lineage);
        for index in 0..before {
            f.invoice(&f.alice, &format!("before {index}")).await;
        }
        let closed = seal(&f.lineage, &f.bob.1, original.tip).await;
        let next = TipLog {
            tip: closed.successor().unwrap().unwrap(),
            ..original
        };
        stage(&f).await;
        f.bob
            .1
            .publish_tip_statements(&[
                TipStatement::Seal(closed),
                bookmark(&f.lineage, "main", Some(original.tip), Some(next.tip)),
            ])
            .await
            .unwrap();
        share(&f.bob.1, &f.alice.1).await;
        Self { f, original, next }
    }
    /// Alice proposes; her write commits at B's first entry.
    async fn commit(&self) {
        let f = &self.f;
        f.propose(&f.alice).await.unwrap();
        self.resolve(&f.alice).await;
        assert_eq!(f.mesh.log.head(self.next).unwrap().commit, 1);
    }
    async fn resolve(&self, node: &Node) {
        node.1
            .resolve_coordinated(
                self.next,
                &self.f.mesh.client(&node.0),
                &self.f.mesh.peers(&self.f.lineage, &node.0),
            )
            .await
            .unwrap();
    }
    /// Alice's carried frames.
    async fn frames(&self) -> Vec<EncryptedFrame> {
        stored_frames(&self.f.alice.1, "SELECT header,body FROM secure_operations")
            .await
            .into_iter()
            .filter(|frame| frame.operation().unwrap().header.extensions.tip == self.original.tip)
            .collect()
    }
    /// The record alice admitted.
    async fn record(&self) -> SignedCarry {
        let graph = self
            .f
            .alice
            .1
            .tip_graph(self.f.lineage.domain)
            .await
            .unwrap();
        assert_eq!(graph.carries.len(), 1);
        graph.carries[0].clone()
    }
    /// A fresh owner that recovers B from nothing it holds, abandoning
    /// its first entry; returns the recovery seal's statements.
    async fn recover_from(&self, graph: &Replica, peers: &dyn EntryPeers) -> Vec<TipStatement> {
        let f = &self.f;
        let owner = device(f, graph, "owner", "main").await;
        f.mesh.nodes.lock().unwrap().clear();
        trusted(f, &owner);
        f.mesh
            .log
            .set_owner(f.lineage.domain, f.lineage.publisher().public_key());
        let recovery = owner
            .1
            .recovery_proposal(self.next, &f.mesh.client(&owner.0), peers)
            .await
            .unwrap();
        assert_eq!(
            (recovery.retained.position, recovery.replaced.position),
            (0, 1)
        );
        let recovered = recovery_seal(&f.lineage, &owner.1, self.next.tip, recovery).await;
        owner
            .1
            .publish_tip_statements(&[TipStatement::Seal(recovered)])
            .await
            .unwrap();
        owner.1.tip_statements(None, 32).await.unwrap()
    }
}
fn ids(frames: &[EncryptedFrame]) -> Vec<Hash> {
    frames.iter().map(|frame| frame.id().unwrap()).collect()
}
async fn chain(replica: &Replica) -> CredentialChain {
    replica.inner.lock().await.config.credentials.clone()
}

#[tokio::test]
async fn a_member_holds_a_carried_write_once_its_record_follows_the_recovery() {
    let c = Carried::new(0).await;
    c.commit().await;
    let f = &c.f;
    // The member learns the graph from bob, who never saw the record.
    let member = device(f, &f.bob.1, "member", "main").await;
    sync(&f.lineage, &f.alice.1, &member.1, &member.0).await;
    assert_eq!(rows(&member.1).await, [row("0001", "carried")]);
    assert_eq!(entry_frames(&member.1).await, [(c.original, 1)]);
    let statements = c.recover_from(&f.bob.1, &Nobody).await;
    member.1.receive_tip_statements(&statements).await.unwrap();
    assert_eq!(
        member.1.followed_tip().await,
        statements_continuation(&statements, c.next)
    );
    // Without the record the frames sign A: the recovery of B keeps them.
    assert_eq!(rows(&member.1).await, [row("0001", "carried")]);
    assert_eq!(abandoned_frames(&member.1).await, 0);
    share(&f.alice.1, &member.1).await;
    assert_eq!(entry_frames(&member.1).await, [(c.next, 1)]);
    assert_eq!(rows(&member.1).await, []);
    assert_eq!(member.1.held_changes().await.unwrap().len(), 1);
    // Reopening keeps the rollback.
    let (key, replica) = member;
    drop(replica);
    let replica = open(&f.lineage, &f.dir, "member", &key).await;
    assert_eq!(rows(&replica).await, []);
    assert_eq!(replica.held_changes().await.unwrap().len(), 1);
    assert_eq!(abandoned_frames(&replica).await, 1);
}
/// The continuation of the recovery seal of `log` among `statements`.
fn statements_continuation(statements: &[TipStatement], log: TipLog) -> rocky_core::TipId {
    statements
        .iter()
        .find_map(|statement| match statement {
            TipStatement::Seal(seal) if seal.statement.tip == log.tip => seal.successor().unwrap(),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn a_fresh_owner_collects_a_carried_write_from_a_member_and_abandons_nothing() {
    let c = Carried::new(0).await;
    c.commit().await;
    let f = &c.f;
    let member = device(f, &f.alice.1, "member", "main").await;
    sync(&f.lineage, &f.alice.1, &member.1, &member.0).await;
    let owner = device(f, &f.alice.1, "owner", "main").await;
    f.mesh.nodes.lock().unwrap().clear();
    trusted(f, &owner);
    f.mesh.add(&member.0, &member.1);
    f.mesh
        .log
        .set_owner(f.lineage.domain, f.lineage.publisher().public_key());
    let recovery = proposal(f, &owner, c.next).await;
    assert_eq!(
        (recovery.retained.position, recovery.replaced.position),
        (1, 1)
    );
    assert_eq!(rows(&owner.1).await, [row("0001", "carried")]);
    assert_eq!(entry_frames(&owner.1).await, [(c.next, 1)]);
}

#[tokio::test]
async fn a_two_hop_carry_resolves_to_the_deepest_record_and_its_recovery_holds_the_write() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    let a = tip_log(&f.lineage);
    let ac = seal(&f.lineage, &f.bob.1, a.tip).await;
    let c = TipLog {
        tip: ac.successor().unwrap().unwrap(),
        ..a
    };
    stage_with(&f, &f.alice, "carried", HANDLE).await;
    f.bob
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(ac),
            bookmark(&f.lineage, "main", Some(a.tip), Some(c.tip)),
        ])
        .await
        .unwrap();
    share(&f.bob.1, &f.alice.1).await;
    // Bob ends C before alice retries there.
    let cd = seal(&f.lineage, &f.bob.1, c.tip).await;
    let d = TipLog {
        tip: cd.successor().unwrap().unwrap(),
        ..a
    };
    f.bob
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(cd),
            bookmark(&f.lineage, "main", Some(c.tip), Some(d.tip)),
        ])
        .await
        .unwrap();
    assert_eq!(
        f.propose(&f.alice).await.map(|_| ()),
        Err(CoordinatedError::Undecided)
    );
    share(&f.bob.1, &f.alice.1).await;
    assert_eq!(f.alice.1.followed_tip().await, d.tip);
    for log in [c, d] {
        f.alice
            .1
            .resolve_coordinated(log, &f.mesh.client(&f.alice.0), &Nobody)
            .await
            .unwrap();
    }
    assert_eq!(f.mesh.log.head(d).unwrap().commit, 1);
    assert_eq!(f.mesh.log.head(c).unwrap().commit, 1);
    let graph = f.alice.1.tip_graph(f.lineage.domain).await.unwrap();
    assert_eq!(graph.carries.len(), 1);
    let effective = graph.carries[0].clone();
    assert_eq!(
        (effective.statement.from.0, effective.statement.tip),
        (a.tip, d.tip)
    );
    // An aborted carry's record to C, as a retry before C's seal signed it.
    let aborted = SignedCarry::sign(
        CarryStatement {
            tip: c.tip,
            ..effective.statement.clone()
        },
        &f.alice.0,
        chain(&f.alice.1).await,
    )
    .unwrap();
    f.alice
        .1
        .receive_tip_statements(&[TipStatement::Carry(Box::new(aborted))])
        .await
        .unwrap();
    let member = device(&f, &f.alice.1, "member", "main").await;
    sync(&f.lineage, &f.alice.1, &member.1, &member.0).await;
    assert_eq!(
        member
            .1
            .tip_graph(f.lineage.domain)
            .await
            .unwrap()
            .carries
            .len(),
        2
    );
    assert_eq!(entry_frames(&member.1).await, [(d, 1)]);
    assert_eq!(rows(&member.1).await, [row("0001", "carried")]);
    let carried = Carried {
        f,
        original: a,
        next: d,
    };
    let statements = carried.recover_from(&carried.f.alice.1, &Nobody).await;
    member.1.receive_tip_statements(&statements).await.unwrap();
    assert_eq!(rows(&member.1).await, []);
    assert_eq!(member.1.held_changes().await.unwrap().len(), 1);
}

#[tokio::test]
async fn trusted_nodes_store_a_carried_entry_only_with_its_authors_matching_record() {
    let c = Carried::new(0).await;
    c.commit().await;
    let f = &c.f;
    let frames = c.frames().await;
    let record = c.record().await;
    let ids = ids(&frames);
    let entry = |position: u64, carry: Option<SignedCarry>| CoordinatedEntry {
        log: c.next,
        pending: rocky_core::LogPending {
            proposer: *f.alice.0.public_key().as_bytes(),
            hash: entry_hash(c.next, position, &ids).unwrap(),
            position,
            epoch: 1,
            deadline: 0,
        },
        frames: frames.clone(),
        voids: Vec::new(),
        carry,
    };
    let refused = |result: Result<rocky_core::LogResult, CoordinatedError>| {
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("carry record") || error.contains("carried entry"),
            "{error}"
        );
    };
    let client = f.mesh.client(&f.bob.0);
    // No record.
    refused(f.bob.1.store_entry(entry(1, None), &client).await);
    // Another position than the record's.
    refused(
        f.bob
            .1
            .store_entry(entry(2, Some(record.clone())), &client)
            .await,
    );
    // Other frames than the record's.
    let other = SignedCarry::sign(
        CarryStatement {
            ids: vec![[0; 32]],
            ..record.statement.clone()
        },
        &f.alice.0,
        chain(&f.alice.1).await,
    )
    .unwrap();
    refused(f.bob.1.store_entry(entry(1, Some(other)), &client).await);
    // Another signer than the frames' author.
    let forged =
        SignedCarry::sign(record.statement.clone(), &f.bob.0, chain(&f.bob.1).await).unwrap();
    refused(f.bob.1.store_entry(entry(1, Some(forged)), &client).await);
    assert_eq!(
        f.bob.1.coordinated_status(c.next).await.unwrap().undecided,
        0
    );
}

#[tokio::test]
async fn the_graph_refuses_conflicting_and_branch_carry_records() {
    let c = Carried::new(0).await;
    c.commit().await;
    let f = &c.f;
    let record = c.record().await;
    let before = f.alice.1.tip_graph(f.lineage.domain).await.unwrap();
    // Another bundle at the committed entry: alice holds what committed.
    let mismatch = SignedCarry::sign(
        CarryStatement {
            ids: vec![[1; 32]],
            ..record.statement.clone()
        },
        &f.alice.0,
        chain(&f.alice.1).await,
    )
    .unwrap();
    assert!(
        f.alice
            .1
            .receive_tip_statements(&[TipStatement::Carry(Box::new(mismatch.clone()))])
            .await
            .is_err()
    );
    // Bob holds no record and no entry of B; the conflict still refuses
    // the second record once he learns the first.
    share(&f.alice.1, &f.bob.1).await;
    assert!(
        f.bob
            .1
            .receive_tip_statements(&[TipStatement::Carry(Box::new(mismatch))])
            .await
            .is_err()
    );
    // A branch opening is never a carry target.
    let sealed = seal(&f.lineage, &f.alice.1, c.next.tip).await;
    let opened = opening(&f.lineage, &sealed, 1);
    let branch = opened.tip().unwrap();
    f.alice
        .1
        .publish_tip_statements(&[TipStatement::Seal(sealed), TipStatement::Opening(opened)])
        .await
        .unwrap();
    let sideways = SignedCarry::sign(
        CarryStatement {
            format: CARRY_FORMAT,
            tip: branch,
            ..record.statement.clone()
        },
        &f.alice.0,
        chain(&f.alice.1).await,
    )
    .unwrap();
    let error = f
        .alice
        .1
        .receive_tip_statements(&[TipStatement::Carry(Box::new(sideways))])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cannot reach"), "{error}");
    let after = f.alice.1.tip_graph(f.lineage.domain).await.unwrap();
    assert_eq!(after.carries, before.carries);
}

#[tokio::test]
async fn a_crash_between_record_admission_and_commit_releases_the_write_once_after_restart() {
    let c = Carried::new(0).await;
    let f = &c.f;
    f.alice.1.fail_publication_after(4).await;
    assert!(matches!(
        f.propose(&f.alice).await,
        Err(CoordinatedError::Storage(Error::InjectedFailure))
    ));
    assert_eq!(f.mesh.log.head(c.next).unwrap().commit, 1);
    // The record is admitted; the frames are still unreleased.
    assert_eq!(
        f.alice
            .1
            .tip_graph(f.lineage.domain)
            .await
            .unwrap()
            .carries
            .len(),
        1
    );
    let member = device(f, &f.alice.1, "member", "main").await;
    sync(&f.lineage, &f.alice.1, &member.1, &member.0).await;
    assert_eq!(rows(&member.1).await, []);
    let Carried { f, original, next } = c;
    let f = f.restart(true).await;
    let c = Carried { f, original, next };
    c.resolve(&c.f.alice).await;
    let status = c.f.alice.1.coordinated_status(next).await.unwrap();
    assert_eq!((status.position, status.undecided), (1, 0));
    assert_eq!(rows(&c.f.alice.1).await, [row("0001", "carried")]);
    assert_eq!(c.f.mesh.log.head(next).unwrap().commit, 1);
    sync(&c.f.lineage, &c.f.alice.1, &member.1, &member.0).await;
    assert_eq!(rows(&member.1).await, [row("0001", "carried")]);
    assert_eq!(entry_frames(&member.1).await, [(next, 1)]);
    c.f.alice.1.handle_committed(HANDLE).await.unwrap();
}

#[tokio::test]
async fn a_trusted_node_catching_up_admits_the_record_and_serves_it_with_the_frames() {
    let c = Carried::new(0).await;
    c.commit().await;
    let f = &c.f;
    assert!(
        f.bob
            .1
            .tip_graph(f.lineage.domain)
            .await
            .unwrap()
            .carries
            .is_empty()
    );
    c.resolve(&f.bob).await;
    assert_eq!(
        f.bob.1.coordinated_status(c.next).await.unwrap().position,
        1
    );
    assert_eq!(rows(&f.bob.1).await, [row("0001", "carried")]);
    assert_eq!(
        f.bob.1.tip_graph(f.lineage.domain).await.unwrap().carries,
        [c.record().await]
    );
    // The very frames alice authored: identities, timestamps and handle.
    let authored: BTreeSet<Hash> = ids(&c.frames().await).into_iter().collect();
    assert!(authored.is_subset(&held(&f.bob.1).await));
    // A member that only ever reaches bob places the frames at B.
    let member = device(f, &f.bob.1, "member", "main").await;
    sync(&f.lineage, &f.bob.1, &member.1, &member.0).await;
    assert_eq!(entry_frames(&member.1).await, [(c.next, 1)]);
    assert_eq!(rows(&member.1).await, [row("0001", "carried")]);
}

#[tokio::test]
async fn catching_up_the_sealed_log_from_a_member_skips_carried_frames() {
    let c = Carried::new(1).await;
    c.commit().await;
    let f = &c.f;
    let member = device(f, &f.alice.1, "member", "main").await;
    sync(&f.lineage, &f.alice.1, &member.1, &member.0).await;
    assert_eq!(
        entry_frames(&member.1)
            .await
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([(c.original, 1), (c.next, 1)])
    );
    let fresh = device(f, &f.alice.1, "fresh", "main").await;
    f.mesh.nodes.lock().unwrap().clear();
    trusted(f, &fresh);
    f.mesh.add(&member.0, &member.1);
    fresh
        .1
        .resolve_coordinated(
            c.original,
            &f.mesh.client(&fresh.0),
            &f.mesh.peers(&f.lineage, &fresh.0),
        )
        .await
        .unwrap();
    assert_eq!(
        fresh
            .1
            .coordinated_status(c.original)
            .await
            .unwrap()
            .position,
        1
    );
    assert_eq!(rows(&fresh.1).await, [row("0001", "before 0")]);
}

#[tokio::test]
async fn every_table_of_a_carried_bundle_moves_with_its_record_and_rolls_back_together() {
    let mut lineage = Lineage::new();
    lineage.existence = true;
    let c = Carried::staging(lineage, 0, |f| {
        Box::pin(async move {
            let slot = f.slot(&f.alice).await.unwrap();
            let sql = session(&f.alice.1).await;
            sql.run("BEGIN", vec![]).await.unwrap();
            identity_insert(
                &sql,
                "INSERT INTO items(key,title) VALUES('bundle','item')",
                vec![],
            )
            .await
            .unwrap();
            identity_insert(
                &sql,
                "INSERT INTO tags(key,label) VALUES('bundle','tag')",
                vec![],
            )
            .await
            .unwrap();
            sql.commit_coordinated(HANDLE, slot).await.unwrap();
        })
    })
    .await;
    c.commit().await;
    let f = &c.f;
    assert_eq!(c.record().await.statement.ids.len(), 2);
    let member = device(f, &f.bob.1, "member", "main").await;
    sync(&f.lineage, &f.alice.1, &member.1, &member.0).await;
    let tags = f.lineage.scope("tags");
    let credentials = f.lineage.credentials(3, &member.0);
    f.alice
        .1
        .admit_peer(
            member.0.public_key(),
            &credentials,
            member.1.followed_tip().await,
        )
        .await
        .unwrap();
    let page = f
        .alice
        .1
        .range_page(
            member.0.public_key(),
            &credentials,
            &member.1.frontier(tags).await.unwrap(),
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    for status in member.1.receive_all(page.frames).await {
        status.unwrap();
    }
    assert_eq!(rows(&member.1).await, [row("bundle", "item")]);
    assert_eq!(
        select(&member.1, "SELECT label FROM tags").await,
        [vec![text("tag")]]
    );
    let statements = c.recover_from(&f.bob.1, &Nobody).await;
    member.1.receive_tip_statements(&statements).await.unwrap();
    assert_eq!(abandoned_frames(&member.1).await, 0);
    share(&f.alice.1, &member.1).await;
    assert_eq!(entry_frames(&member.1).await, [(c.next, 1), (c.next, 1)]);
    assert!(rows(&member.1).await.is_empty());
    assert!(select(&member.1, "SELECT * FROM tags").await.is_empty());
    assert_eq!(abandoned_frames(&member.1).await, 2);
    assert_eq!(member.1.held_changes().await.unwrap().len(), 1);
}
