//! Recovery proof, storage, collection and inherited-state regressions.
use super::*;

#[tokio::test]
async fn recovery_rolls_back_a_committed_carried_bundle_on_an_offline_member() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    let original = tip_log(&f.lineage);
    let closed = seal(&f.lineage, &f.bob.1, original.tip).await;
    let next = TipLog {
        tip: closed.successor().unwrap().unwrap(),
        ..original
    };
    stage_with(&f, &f.alice, "carried", [71; 32]).await;
    f.bob
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(closed),
            bookmark(&f.lineage, "main", Some(original.tip), Some(next.tip)),
        ])
        .await
        .unwrap();
    share(&f.bob.1, &f.alice.1).await;
    f.propose(&f.alice).await.unwrap();
    f.alice
        .1
        .resolve_coordinated(
            next,
            &f.mesh.client(&f.alice.0),
            &f.mesh.peers(&f.lineage, &f.alice.0),
        )
        .await
        .unwrap();
    assert_eq!(f.mesh.log.head(next).unwrap().commit, 1);
    let member = device(&f, &f.alice.1, "carried-member", "main").await;
    sync(&f.lineage, &f.alice.1, &member.1, &member.0).await;
    assert_eq!(rows(&member.1).await, [row("0001", "carried")]);
    let frames = stored_frames(&member.1, "SELECT header,body FROM secure_operations").await;
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0].operation().unwrap().header.extensions.tip,
        original.tip
    );
    // Alice's carry record places the frames at B's first entry.
    share(&f.alice.1, &member.1).await;
    let graph = member.1.tip_graph(f.lineage.domain).await.unwrap();
    assert_eq!(graph.carries.len(), 1);
    let carry = &graph.carries[0].statement;
    assert_eq!(
        (carry.from.0, carry.tip, carry.position),
        (original.tip, next.tip, 1)
    );
    assert_eq!(entry_frames(&member.1).await, [(next, 1)]);
    let owner = device(&f, &f.alice.1, "carried-owner", "main").await;
    f.mesh.nodes.lock().unwrap().clear();
    trusted(&f, &owner);
    f.mesh
        .log
        .set_owner(f.lineage.domain, f.lineage.publisher().public_key());
    let recovery = owner
        .1
        .recovery_proposal(next, &f.mesh.client(&owner.0), &Nobody)
        .await
        .unwrap();
    assert_eq!(
        (recovery.retained.position, recovery.replaced.position),
        (0, 1)
    );
    let recovered = recovery_seal(&f.lineage, &owner.1, next.tip, recovery).await;
    let continuation = recovered.successor().unwrap().unwrap();
    owner
        .1
        .publish_tip_statements(&[TipStatement::Seal(recovered)])
        .await
        .unwrap();
    share(&owner.1, &member.1).await;
    assert_eq!(member.1.followed_tip().await, continuation);
    assert!(rows(&owner.1).await.is_empty());
    assert_eq!(rows(&member.1).await, []);
    assert_eq!(member.1.held_changes().await.unwrap().len(), 1);
}

#[tokio::test]
async fn recovery_candidates_wait_for_two_trusted_nodes_before_any_member_rolls_back() {
    use rocky_credentials::{GENESIS_LINE_FORMAT, GenesisLineStatement, SignedGenesisLine};
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let main = tip_log(&f.lineage);
    let line = TipStatement::GenesisLine(
        SignedGenesisLine::sign(
            GenesisLineStatement {
                format: GENESIS_LINE_FORMAT,
                scope: main.scope,
                minimum_in_sync: 2,
            },
            f.lineage.publisher(),
        )
        .unwrap(),
    );
    f.alice.1.publish_tip_statements(&[line]).await.unwrap();
    share(&f.alice.1, &stuck.carol.1).await;
    share(&f.alice.1, &f.bob.1).await;
    let sealed = stuck.seal().await;
    let before = rows(&stuck.carol.1).await;
    let error = f
        .alice
        .1
        .publish_tip_statements(&[TipStatement::Seal(sealed.clone())])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("recovery_pending"), "{error}");
    assert_eq!(f.alice.1.followed_tip().await, main.tip);
    assert_eq!(f.mesh.log.head(main).unwrap().acknowledgments.len(), 1);
    assert!(
        f.alice
            .1
            .tip_statements(None, 32)
            .await
            .unwrap()
            .iter()
            .all(|s| !matches!(s, TipStatement::Seal(s) if s.statement.recovery.is_some()))
    );
    for batch in f.alice.1.pending_tip_candidates().await.unwrap() {
        stuck.carol.1.receive_tip_statements(&batch).await.unwrap();
    }
    assert_eq!(stuck.carol.1.followed_tip().await, main.tip);
    assert_eq!(rows(&stuck.carol.1).await, before);
    assert_eq!(abandoned_frames(&stuck.carol.1).await, 0);
    // A trusted node outside the old in-sync set acknowledges the pending
    // candidate. Receipt does not first activate its recovery effects.
    compose(f, &f.bob);
    for batch in f.alice.1.pending_tip_candidates().await.unwrap() {
        f.bob.1.receive_tip_statements(&batch).await.unwrap();
    }
    assert_eq!(f.mesh.log.head(main).unwrap().acknowledgments.len(), 2);
    assert_eq!(
        f.bob.1.followed_tip().await,
        sealed.successor().unwrap().unwrap()
    );
    share(&f.bob.1, &stuck.carol.1).await;
    assert_eq!(
        stuck.carol.1.followed_tip().await,
        sealed.successor().unwrap().unwrap()
    );
    assert_eq!(abandoned_frames(&stuck.carol.1).await, 1);
    assert_eq!(
        rows(&stuck.carol.1).await,
        [row("0001", "kept"), row("late", "carol's")]
    );
    f.alice.1.settle_tip_statements(&Nobody).await.unwrap();
    assert_eq!(
        f.alice.1.followed_tip().await,
        sealed.successor().unwrap().unwrap()
    );
}

#[tokio::test]
async fn an_ordinary_terminal_cannot_confirm_a_recovery_at_another_head() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    let log = tip_log(&f.lineage);
    let genesis = f.mesh.log.head(log).unwrap().genesis;
    f.invoice(&f.alice, "valid").await;
    let point = rocky_credentials::LogPoint {
        position: 0,
        running: genesis,
    };
    let seal = recovery_seal(
        &f.lineage,
        &f.alice.1,
        log.tip,
        Recovery {
            retained: point,
            replaced: point,
        },
    )
    .await;
    f.mesh
        .client(&f.alice.0)
        .seal(
            log,
            rocky_core::LogTerminal {
                digest: seal.id().unwrap(),
                continuation: seal.successor().unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.bob
            .1
            .receive_tip_statements(&[TipStatement::Seal(seal)])
            .await
            .unwrap(),
        0
    );
    assert_eq!(f.bob.1.followed_tip().await, log.tip);
    assert_eq!(abandoned_frames(&f.bob.1).await, 0);
}

#[tokio::test]
async fn every_scope_of_an_abandoned_bundle_rolls_back_atomically_and_after_reopening() {
    let mut lineage = Lineage::new();
    lineage.existence = true;
    let f = Fixture::with_lineage(1, lineage).await;
    follow_main(&f).await;
    let log = tip_log(&f.lineage);
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
    sql.commit_coordinated([99; 32], slot).await.unwrap();
    f.propose(&f.alice).await.unwrap();
    f.resolve(&f.bob).await;
    assert_eq!(rows(&f.bob.1).await, [row("bundle", "item")]);
    let evidence = "SELECT 'P',scope,author,sequence FROM secure_identity_marks UNION ALL SELECT 'O',scope,author,sequence FROM secure_identity_own UNION ALL SELECT 'R',entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT 'C',identity,entry,provenance FROM secure_admission_claims";
    let permanent = crate::storage::rows(&f.bob.1.inner.lock().await.conn, evidence)
        .await
        .unwrap();
    assert_eq!(
        crate::tests::compaction::scalar(&f.bob.1, "SELECT count(*) FROM secure_identity_ledger")
            .await,
        2
    );
    assert_eq!(
        select(&f.bob.1, "SELECT label FROM tags").await,
        [vec![text("tag")]]
    );
    f.mesh.set(&f.alice.0, false, true);
    f.mesh.set(&f.bob.0, false, true);
    f.mesh
        .log
        .set_owner(log.scope, f.lineage.publisher().public_key());
    let owner = trusted_device(&f, &f.alice.1, "owner").await;
    let recovery = proposal(&f, &owner, log).await;
    assert_eq!(
        (recovery.retained.position, recovery.replaced.position),
        (0, 1)
    );
    let sealed = recovery_seal(&f.lineage, &owner.1, log.tip, recovery).await;
    owner
        .1
        .publish_tip_statements(&[TipStatement::Seal(sealed.clone())])
        .await
        .unwrap();
    let statements = owner.1.tip_statements(None, 32).await.unwrap();
    f.bob.1.fail_publication_after(3).await;
    assert!(f.bob.1.receive_tip_statements(&statements).await.is_err());
    assert_eq!(f.bob.1.followed_tip().await, log.tip);
    assert_eq!(abandoned_frames(&f.bob.1).await, 0);
    assert_eq!(rows(&f.bob.1).await, [row("bundle", "item")]);
    assert_eq!(
        crate::storage::rows(&f.bob.1.inner.lock().await.conn, evidence)
            .await
            .unwrap(),
        permanent
    );
    assert_eq!(
        crate::tests::compaction::scalar(&f.bob.1, "SELECT count(*) FROM secure_identity_ledger")
            .await,
        2
    );
    assert_eq!(
        select(&f.bob.1, "SELECT label FROM tags").await,
        [vec![text("tag")]]
    );
    drop(sql);
    let f = f.restart(false).await;
    f.bob.1.settle_tip_statements(&Nobody).await.unwrap();
    assert_eq!(
        f.bob.1.followed_tip().await,
        sealed.successor().unwrap().unwrap()
    );
    assert!(rows(&f.bob.1).await.is_empty());
    assert!(select(&f.bob.1, "SELECT * FROM tags").await.is_empty());
    assert_eq!(abandoned_frames(&f.bob.1).await, 2);
    assert_eq!(
        crate::storage::rows(&f.bob.1.inner.lock().await.conn, evidence)
            .await
            .unwrap(),
        permanent
    );
    assert_eq!(
        crate::tests::compaction::scalar(&f.bob.1, "SELECT count(*) FROM secure_identity_ledger")
            .await,
        0
    );
    for scope in [f.lineage.scope("items"), f.lineage.scope("tags")] {
        f.bob.1.audit(scope).await.unwrap();
    }
    assert_eq!(f.bob.1.held_changes().await.unwrap().len(), 1);
    f.alice.1.receive_tip_statements(&statements).await.unwrap();
    assert!(
        f.alice
            .1
            .handle_committed([99; 32])
            .await
            .unwrap_err()
            .to_string()
            .contains("abandoned")
    );
}

#[tokio::test]
async fn an_opening_with_only_ordinary_inherited_rows_is_not_a_writable_empty_branch() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    write(&f.alice.1, "ordinary", "inherited").await;
    let log = tip_log(&f.lineage);
    assert_eq!(f.mesh.log.head(log).unwrap().commit, 0);
    let sealed = seal(&f.lineage, &f.alice.1, log.tip).await;
    let opened = opening(&f.lineage, &sealed, 1);
    let tip = opened.tip().unwrap();
    f.alice
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(sealed),
            TipStatement::Opening(opened),
            bookmark(&f.lineage, "dev", None, Some(tip)),
        ])
        .await
        .unwrap();
    let follower = device(&f, &f.alice.1, "missing-ordinary", "dev").await;
    trusted(&f, &follower);
    assert_eq!(
        slot_on(&f, &follower).await.map(|_| ()),
        Err(CoordinatedError::Behind)
    );
    assert_eq!(f.mesh.log.head(TipLog { tip, ..log }).unwrap().commit, 0);
}

#[tokio::test]
async fn a_fully_collected_log_with_full_membership_recovers_without_abandonment() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    f.invoice(&f.alice, "retained").await;
    let log = tip_log(&f.lineage);
    let owner = trusted_device(&f, &f.alice.1, "full-owner").await;
    let recovery = proposal(&f, &owner, log).await;
    assert_eq!(recovery.retained, recovery.replaced);
    let head = f.mesh.log.head(log).unwrap();
    for _ in head.members.len()..32 {
        f.mesh
            .log
            .client(PrivateKey::new().public_key())
            .join(log, head.commit, head.running_hash, 1)
            .await
            .unwrap();
    }
    let head = f.mesh.log.head(log).unwrap();
    assert_eq!(head.members.len(), 32);
    assert_eq!(
        f.mesh
            .client(&owner.0)
            .join(log, head.commit, head.running_hash, 1)
            .await,
        Err(CoordinationError::Backpressure)
    );
    f.mesh
        .log
        .set_owner(log.scope, f.lineage.publisher().public_key());
    let sealed = recovery_seal(&f.lineage, &owner.1, log.tip, recovery).await;
    owner
        .1
        .publish_tip_statements(&[TipStatement::Seal(sealed.clone())])
        .await
        .unwrap();
    assert_eq!(rows(&owner.1).await, [row("0001", "retained")]);
    assert_eq!(abandoned_frames(&owner.1).await, 0);
    assert!(f.mesh.log.head(log).unwrap().members.is_empty());
    invoice_on(&f, &owner, "continued").await;
    assert_eq!(
        rows(&owner.1).await,
        [row("0001", "retained"), row("0002", "continued")]
    );
}

#[tokio::test]
async fn a_zero_length_recovery_aborts_pending_work_and_clears_membership() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    let log = tip_log(&f.lineage);
    let head = f.mesh.log.head(log).unwrap();
    f.mesh
        .client(&f.alice.0)
        .append(
            log,
            Append {
                expected: 0,
                epoch: head.epoch,
                hash: [88; 32],
                deadline: now().unwrap() + ENTRY_DEADLINE,
                sequence: rocky_core::next_request_sequence(0, now().unwrap() + 1)
                    .unwrap()
                    .get(),
            },
        )
        .await
        .unwrap();
    let owner = trusted_device(&f, &f.alice.1, "empty-owner").await;
    let recovery = proposal(&f, &owner, log).await;
    assert_eq!(recovery.retained, recovery.replaced);
    assert_eq!(recovery.retained.position, 0);
    f.mesh
        .log
        .set_owner(log.scope, f.lineage.publisher().public_key());
    let sealed = recovery_seal(&f.lineage, &owner.1, log.tip, recovery).await;
    owner
        .1
        .publish_tip_statements(&[TipStatement::Seal(sealed)])
        .await
        .unwrap();
    let head = f.mesh.log.head(log).unwrap();
    assert!(head.pending.is_none());
    assert!(head.members.is_empty());
    assert_eq!(head.commit, 1);
}

#[tokio::test]
async fn complementary_local_and_peer_bundles_are_collected_as_one_contiguous_prefix() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    f.invoice(&f.alice, "first").await;
    let slot = f.slot(&f.bob).await.unwrap();
    let sql = session(&f.bob.1).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    identity_insert(
        &sql,
        "INSERT INTO tags(key,label) VALUES('second','second')",
        vec![],
    )
    .await
    .unwrap();
    sql.commit_coordinated([90; 32], slot).await.unwrap();
    f.propose(&f.bob).await.unwrap();
    f.invoice(&f.alice, "third").await;
    let local = trusted_device(&f, &f.alice.1, "complementary").await;
    // This reader can reconstruct entries 1 and 3 from its own items;
    // entry 2 belongs to a scope it cannot decrypt.
    local
        .1
        .inner
        .lock()
        .await
        .config
        .keys
        .retain(|key| key.scope != f.lineage.scope("tags"));
    sync(&f.lineage, &f.alice.1, &local.1, &local.0).await;
    let log = tip_log(&f.lineage);
    let second = f
        .alice
        .1
        .committed_entries(
            local.0.public_key(),
            &f.lineage.credentials(3, &local.0),
            log,
            1,
        )
        .await
        .unwrap()
        .remove(0);
    struct OnlySecond(CommittedEntry, std::sync::atomic::AtomicU64);
    impl EntryPeers for OnlySecond {
        fn store<'a>(&'a self, _: PublicKey, _: &'a CoordinatedEntry) -> PeerFuture<'a, ()> {
            Box::pin(async { unreachable!() })
        }
        fn committed(&self, _: TipLog, after: u64) -> PeerFuture<'_, Vec<CommittedEntry>> {
            Box::pin(async move {
                self.1.fetch_min(after, std::sync::atomic::Ordering::SeqCst);
                Ok(if after < 2 {
                    vec![self.0.clone()]
                } else {
                    vec![]
                })
            })
        }
    }
    let peers = OnlySecond(second, std::sync::atomic::AtomicU64::new(u64::MAX));
    let recovery = local
        .1
        .recovery_proposal(log, &f.mesh.client(&local.0), &peers)
        .await
        .unwrap();
    assert_eq!(recovery.retained.position, 3);
    assert_eq!(recovery.retained, recovery.replaced);
    assert_eq!(peers.1.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        rows(&local.1).await,
        [row("0001", "first"), row("0002", "third")]
    );
}

#[tokio::test]
async fn collects_every_entry_across_the_real_hash_window_before_recovery_is_signed() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    f.invoice(&f.alice, "first").await;
    let count = rocky_coordinator::coordinated::HASH_WINDOW + 1;
    for position in 2..=count {
        if position % 30 == 0 {
            f.mesh.advance(60);
        }
        let slot = f.slot(&f.alice).await.unwrap();
        let sql = session(&f.alice.1).await;
        sql.run("BEGIN", vec![]).await.unwrap();
        sql.run(
            "UPDATE items SET title=? WHERE key='0001'",
            vec![text(&position.to_string())],
        )
        .await
        .unwrap();
        let handle = rocky_core::commitment("recovery-window-test", &position.to_le_bytes());
        sql.commit_coordinated(handle, slot).await.unwrap();
        f.propose(&f.alice).await.unwrap();
    }
    let owner = trusted_device(&f, &f.alice.1, "window-owner").await;
    let log = tip_log(&f.lineage);
    assert!(
        f.mesh
            .client(&owner.0)
            .log_head(log)
            .await
            .unwrap()
            .committed(1)
            .is_none()
    );
    let peers = f.mesh.peers(&f.lineage, &owner.0);
    let head = f.mesh.client(&owner.0).log_head(log).await.unwrap();
    let mut at = 0;
    let mut running = head.genesis;
    while at < head.commit {
        let entries = peers.committed(log, at).await.unwrap();
        assert!(!entries.is_empty(), "reachable entries ended at {at}");
        for entry in entries {
            assert_eq!(entry.position, at + 1);
            running = rocky_core::coordinated_chain(running, entry.position, entry.hash);
            at = entry.position;
        }
    }
    assert_eq!(running, head.running_hash);
    let resolved = owner
        .1
        .resolve_coordinated(log, &f.mesh.client(&owner.0), &peers)
        .await;
    assert!(resolved.is_ok(), "complete reachable chain: {resolved:?}");
    let recovery = proposal(&f, &owner, log).await;
    assert_eq!(recovery.retained.position, count as u64);
    assert_eq!(recovery.retained, recovery.replaced);
    assert_eq!(rows(&owner.1).await, [row("0001", &count.to_string())]);
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
}

#[tokio::test]
async fn a_branch_gets_ancestor_bundles_only_through_its_inherited_frontier() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    f.invoice(&f.alice, "inherited").await;
    let log = tip_log(&f.lineage);
    let sealed = seal(&f.lineage, &f.alice.1, log.tip).await;
    // This concurrent seal predates the second bundle, which is a late
    // write on main's continuation, outside the branch's inherited state.
    f.invoice(&f.alice, "late on main").await;
    let opening = opening(&f.lineage, &sealed, 1);
    let branch = opening.tip().unwrap();
    f.alice
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(sealed),
            TipStatement::Opening(opening),
            bookmark(&f.lineage, "dev", None, Some(branch)),
        ])
        .await
        .unwrap();
    let follower = device(&f, &f.alice.1, "frontier-follower", "dev").await;
    let credentials = f.lineage.credentials(3, &follower.0);
    f.alice
        .1
        .admit_peer(follower.0.public_key(), &credentials, branch)
        .await
        .unwrap();
    let entries = f
        .alice
        .1
        .committed_entries(follower.0.public_key(), &credentials, log, 0)
        .await
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.position)
            .collect::<Vec<_>>(),
        [1]
    );
    assert!(
        f.alice
            .1
            .committed_entries(follower.0.public_key(), &credentials, log, 1)
            .await
            .unwrap()
            .is_empty()
    );
    trusted(&f, &follower);
    slot_on(&f, &follower).await.unwrap();
    assert_eq!(rows(&follower.1).await, [row("0001", "inherited")]);
}

struct HeadUnavailable(Client);
impl DomainLog for HeadUnavailable {
    fn node(&self) -> NodeKey {
        self.0.node()
    }
    fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()> {
        self.0.configure(log, minimum)
    }
    fn log_head(&self, _: TipLog) -> CoordinationFuture<'_, LogHead> {
        Box::pin(async { Err(CoordinationError::Unavailable) })
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
    fn acknowledge_terminal(&self, log: TipLog, ack: Vec<u8>) -> CoordinationFuture<'_, ()> {
        self.0.acknowledge_terminal(log, ack)
    }
}

#[tokio::test]
async fn a_repeated_publication_keeps_the_only_bytes_after_a_lost_reply_and_head_outage() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let sealed = stuck.seal().await;
    let statements = vec![TipStatement::Seal(sealed.clone())];
    f.alice
        .1
        .set_domain_log(Some(Arc::new(LostReply(f.mesh.client(&f.alice.0)))));
    assert!(f.alice.1.publish_tip_statements(&statements).await.is_err());
    f.alice
        .1
        .set_domain_log(Some(Arc::new(HeadUnavailable(f.mesh.client(&f.alice.0)))));
    assert!(f.alice.1.publish_tip_statements(&statements).await.is_err());
    assert_eq!(
        f.alice.1.pending_tip_candidates().await.unwrap(),
        [statements]
    );
    compose(f, &f.alice);
    f.alice.1.settle_tip_statements(&Nobody).await.unwrap();
    assert_eq!(
        f.alice.1.followed_tip().await,
        sealed.successor().unwrap().unwrap()
    );
    assert!(f.alice.1.pending_tip_candidates().await.unwrap().is_empty());
}

#[tokio::test]
async fn journal_admission_and_its_deferred_remainder_commit_together() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    f.invoice(&f.alice, "before").await;
    let log = tip_log(&f.lineage);
    let receiver = trusted_device(&f, &f.alice.1, "remainder").await;
    let sealed = seal(&f.lineage, &f.alice.1, log.tip).await;
    let successor = sealed.successor().unwrap().unwrap();
    let deferred = vec![
        TipStatement::Seal(sealed),
        bookmark(&f.lineage, "main", Some(log.tip), Some(successor)),
    ];
    let mut statements = deferred.clone();
    statements.push(bookmark(&f.lineage, "independent", None, Some(log.tip)));
    receiver.1.fail_publication_after(3).await;
    assert!(
        receiver
            .1
            .receive_tip_statements(&statements)
            .await
            .is_err()
    );
    assert_eq!(journal_batches(&receiver.1).await, [statements.clone()]);
    assert_eq!(
        receiver
            .1
            .tip_graph(log.scope)
            .await
            .unwrap()
            .bookmark("independent")
            .unwrap(),
        None
    );
    receiver
        .1
        .receive_tip_statements(&statements)
        .await
        .unwrap();
    assert_eq!(journal_batches(&receiver.1).await, [deferred]);
    assert_eq!(
        receiver
            .1
            .tip_graph(log.scope)
            .await
            .unwrap()
            .bookmark("independent")
            .unwrap(),
        Some(log.tip)
    );
    receiver
        .1
        .settle_tip_statements(&f.mesh.peers(&f.lineage, &receiver.0))
        .await
        .unwrap();
    assert_eq!(receiver.1.followed_tip().await, successor);
    assert!(
        receiver
            .1
            .pending_tip_candidates()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn verified_recovery_proof_admits_after_coordinator_retention_and_without_a_head() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let sealed = stuck.recover().await;
    let main = tip_log(&f.lineage);
    for index in 1..=rocky_coordinator::coordinated::ENDED_LOGS + 1 {
        let log = TipLog {
            scope: main.scope,
            tip: rocky_core::TipId([index as u8; 32]),
        };
        f.mesh
            .client(&f.alice.0)
            .seal(
                log,
                rocky_core::LogTerminal {
                    digest: [index as u8; 32],
                    continuation: None,
                },
            )
            .await
            .unwrap();
    }
    let key = PrivateKey::new();
    let replica = open(&f.lineage, &f.dir, "after-retention", &key).await;
    replica.set_domain_log(Some(Arc::new(HeadUnavailable(f.mesh.client(&key)))));
    replica
        .receive_tip_statements(&f.alice.1.tip_statements(None, 32).await.unwrap())
        .await
        .unwrap();
    let graph = replica.tip_graph(main.scope).await.unwrap();
    assert!(graph.is_final(sealed.id().unwrap()).unwrap());
    assert_eq!(graph.seal(main.tip).unwrap(), Some(&sealed));
    assert_eq!(graph.bookmark("main").unwrap(), sealed.successor().unwrap());
}

async fn journal_batches(replica: &Replica) -> Vec<Vec<TipStatement>> {
    rocky_storage::storage::rows(
        &replica.inner.lock().await.conn,
        "SELECT statements FROM secure_pending_tips ORDER BY batch",
    )
    .await
    .unwrap()
    .iter()
    .map(|row| match row.as_slice() {
        [SqlValue::Blob(bytes)] => crate::decode_bounded(bytes, MAX_SECURE_BYTES).unwrap(),
        _ => panic!("journal batch"),
    })
    .collect()
}

struct LaterHeadUnavailable(Client, rocky_core::TipId);
impl DomainLog for LaterHeadUnavailable {
    fn node(&self) -> NodeKey {
        self.0.node()
    }
    fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()> {
        self.0.configure(log, minimum)
    }
    fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead> {
        if log.tip == self.1 {
            Box::pin(async { Err(CoordinationError::Unavailable) })
        } else {
            self.0.log_head(log)
        }
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
    fn acknowledge_terminal(&self, log: TipLog, ack: Vec<u8>) -> CoordinationFuture<'_, ()> {
        self.0.acknowledge_terminal(log, ack)
    }
}

#[tokio::test]
async fn a_partially_submitted_batch_keeps_its_bytes_through_local_failure_and_reopen() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    f.invoice(&f.alice, "retained").await;
    let log = tip_log(&f.lineage);
    let first = seal(&f.lineage, &f.alice.1, log.tip).await;
    let child = first.successor().unwrap().unwrap();
    let second = SignedSeal::sign(
        SealStatement {
            tip: child,
            ..first.statement.clone()
        },
        f.lineage.publisher(),
    )
    .unwrap();
    let next = second.successor().unwrap().unwrap();
    let statements = vec![
        TipStatement::Seal(first.clone()),
        TipStatement::Seal(second),
        bookmark(&f.lineage, "main", Some(log.tip), Some(next)),
    ];
    f.alice.1.set_domain_log(Some(Arc::new(LaterHeadUnavailable(
        f.mesh.client(&f.alice.0),
        child,
    ))));
    assert!(f.alice.1.publish_tip_statements(&statements).await.is_err());
    assert_eq!(
        f.mesh.log.head(log).unwrap().terminal.unwrap().digest,
        first.id().unwrap()
    );
    assert!(
        f.mesh
            .log
            .head(TipLog { tip: child, ..log })
            .unwrap()
            .terminal
            .is_none()
    );
    assert_eq!(
        journal_batches(&f.alice.1).await.as_slice(),
        std::slice::from_ref(&statements)
    );
    compose(&f, &f.alice);
    f.alice.1.fail_publication_after(3).await;
    assert!(f.alice.1.settle_tip_statements(&Nobody).await.is_err());
    assert_eq!(journal_batches(&f.alice.1).await, [statements]);
    let f = f.restart(true).await;
    compose(&f, &f.alice);
    f.alice.1.settle_tip_statements(&Nobody).await.unwrap();
    assert_eq!(f.alice.1.followed_tip().await, next);
    assert_eq!(rows(&f.alice.1).await, [row("0001", "retained")]);
    assert!(journal_batches(&f.alice.1).await.is_empty());
}

#[tokio::test]
async fn recovery_admission_holds_a_write_carried_before_its_commitment_proof_arrives() {
    use rocky_credentials::{GENESIS_LINE_FORMAT, GenesisLineStatement, SignedGenesisLine};
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let log = tip_log(&f.lineage);
    let line = TipStatement::GenesisLine(
        SignedGenesisLine::sign(
            GenesisLineStatement {
                format: GENESIS_LINE_FORMAT,
                scope: log.scope,
                minimum_in_sync: 2,
            },
            f.lineage.publisher(),
        )
        .unwrap(),
    );
    f.alice.1.publish_tip_statements(&[line]).await.unwrap();
    share(&f.alice.1, &stuck.author.1).await;
    share(&f.alice.1, &f.bob.1).await;
    let sealed = stuck.seal().await;
    assert!(
        f.alice
            .1
            .publish_tip_statements(&[TipStatement::Seal(sealed.clone())])
            .await
            .is_err()
    );
    let next = TipLog {
        tip: sealed.successor().unwrap().unwrap(),
        ..log
    };
    trusted(f, &stuck.author);
    // The coordinator already ended the old log. This offline device has
    // no admitted recovery yet and carries its pending write, still unsent.
    stuck
        .author
        .1
        .resolve_coordinated(log, &f.mesh.client(&stuck.author.0), &Nobody)
        .await
        .unwrap();
    assert_eq!(
        stuck
            .author
            .1
            .coordinated_status(next)
            .await
            .unwrap()
            .undecided,
        1
    );
    compose(f, &f.bob);
    for batch in f.alice.1.pending_tip_candidates().await.unwrap() {
        f.bob.1.receive_tip_statements(&batch).await.unwrap();
    }
    share(&f.bob.1, &stuck.author.1).await;
    assert_eq!(
        stuck
            .author
            .1
            .coordinated_status(next)
            .await
            .unwrap()
            .undecided,
        0
    );
    assert_eq!(stuck.author.1.held_changes().await.unwrap().len(), 2);
    assert!(
        stuck
            .author
            .1
            .handle_committed([8; 32])
            .await
            .unwrap_err()
            .to_string()
            .contains("abandoned")
    );
    assert_eq!(f.mesh.log.head(next).unwrap().commit, 0);
}

#[tokio::test]
async fn collection_proves_and_applies_undecided_bundles_held_only_in_local_storage() {
    let f = Fixture::new(1).await;
    follow_main(&f).await;
    f.invoice(&f.alice, "first").await;
    f.invoice(&f.alice, "second").await;
    let log = tip_log(&f.lineage);
    assert_eq!(f.bob.1.coordinated_status(log).await.unwrap().position, 0);
    let proposal = f
        .bob
        .1
        .recovery_proposal(log, &Trimmed(f.mesh.client(&f.bob.0)), &Nobody)
        .await
        .unwrap();
    assert_eq!(proposal.retained.position, 2);
    assert_eq!(proposal.retained, proposal.replaced);
    assert_eq!(
        rows(&f.bob.1).await,
        [row("0001", "first"), row("0002", "second")]
    );
}
