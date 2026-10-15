//! Owner-signed cuts: a stuck log continues past positions nobody holds,
//! members that applied them revert them, tainted bases are guarded until
//! a clean import, rollback resumes after a crash at any step, and gaps
//! before the hash window are endorsed by the cut alone. Decision 0074.
use super::*;
use crate::replica::CutEvidence;
use rocky_credentials::{CutPoint, CutStatement, SignedCut};

/// The running hash after `position`, from the head's genesis anchor.
fn running(head: &LogHead, position: u64) -> Commitment {
    assert_eq!(head.hashes.len() as u64, head.commit);
    (1..=position).fold(head.genesis, |running, p| {
        rocky_core::coordinated_chain(running, p, head.hashes[p as usize - 1])
    })
}
/// The owner's first cut of `head`, keeping `retained` and verified at
/// `verified`.
fn sign_cut(lineage: &Lineage, head: &LogHead, verified: u64, retained: u64) -> CutEvidence {
    let point = |position| CutPoint {
        position,
        running: running(head, position),
    };
    let record = SignedCut::sign(
        CutStatement {
            log: tip_log(lineage),
            sequence: 1,
            previous: None,
            verified: point(verified),
            retained: point(retained),
            replaced: point(head.commit),
            epoch: head.epoch,
            issued: 0,
        },
        lineage.publisher(),
    )
    .unwrap();
    CutEvidence {
        record,
        authority: CredentialChain(Vec::new()),
    }
}
async fn submit(mesh: &Mesh, lineage: &Lineage, node: &PrivateKey, cut: &CutEvidence) -> u64 {
    match mesh
        .client(node)
        .cut(tip_log(lineage), cut.to_log().unwrap())
        .await
        .unwrap()
    {
        LogResult::Committed { position, .. } => position,
        other => panic!("{other:?}"),
    }
}
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
fn domain_head(mesh: &Mesh, lineage: &Lineage) -> LogHead {
    mesh.log.head(tip_log(lineage)).unwrap().port()
}

/// Entry 1 reached every member and entry 2 only `carol`; then the whole
/// in-sync set is lost. A fresh trusted node continues only after the
/// owner cuts entry 2 away.
struct Stuck {
    fixture: Fixture,
    carol: (PrivateKey, Replica),
    cut: CutEvidence,
}
impl Stuck {
    /// The fresh node is `fixture.alice`, the member `dave` `fixture.bob`.
    async fn new() -> Self {
        let fixture = Fixture::new(1).await;
        fixture.invoice(&fixture.alice, "kept").await;
        let dave_key = PrivateKey::new();
        let dave = open(&fixture.lineage, &fixture.dir, "dave", &dave_key).await;
        sync(&fixture.lineage, &fixture.alice.1, &dave, &dave_key).await;
        fixture.invoice(&fixture.alice, "abandoned").await;
        let carol_key = PrivateKey::new();
        let carol = open(&fixture.lineage, &fixture.dir, "carol", &carol_key).await;
        sync(&fixture.lineage, &fixture.alice.1, &carol, &carol_key).await;
        assert_eq!(
            rows(&carol).await,
            [row("0001", "kept"), row("0002", "abandoned")]
        );
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
        // Carol is away: no reachable device holds entry 2.
        mesh.add(&dave_key, &dave);
        let fresh_key = PrivateKey::new();
        let fresh = open(&lineage, &dir, "fresh", &fresh_key).await;
        mesh.add(&fresh_key, &fresh);
        mesh.log
            .set_owner(lineage.domain, lineage.publisher().public_key());
        let cut = sign_cut(&lineage, &domain_head(&mesh, &lineage), 1, 1);
        Self {
            fixture: Fixture {
                lineage,
                dir,
                mesh,
                alice: (fresh_key, fresh),
                bob: (dave_key, dave),
            },
            carol: (carol_key, carol),
            cut,
        }
    }
    /// Submits the cut, then the fresh node writes the next invoice.
    async fn continue_past_the_cut(&self) {
        let f = &self.fixture;
        assert_eq!(submit(&f.mesh, &f.lineage, &f.alice.0, &self.cut).await, 3);
        f.invoice(&f.alice, "continued").await;
        assert_eq!(
            f.alice
                .1
                .coordinated_status(tip_log(&f.lineage))
                .await
                .unwrap()
                .position,
            4
        );
    }
}

#[tokio::test]
async fn without_a_copy_appends_stay_refused_until_the_owner_cuts_then_continue_at_a_plus_two() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    assert_eq!(f.slot(&f.alice).await, Err(CoordinatedError::Behind));
    assert_eq!(f.slot(&f.alice).await, Err(CoordinatedError::Behind));
    // Only the domain's owner signs a cut the coordinator admits.
    let mut forged = stuck.cut.clone();
    forged.record = SignedCut::sign(stuck.cut.record.statement.clone(), &f.alice.0).unwrap();
    assert_eq!(
        f.mesh
            .client(&f.alice.0)
            .cut(tip_log(&f.lineage), forged.to_log().unwrap())
            .await,
        Err(CoordinationError::Denied)
    );
    assert_eq!(domain_head(&f.mesh, &f.lineage).commit, 2);
    stuck.continue_past_the_cut().await;
    // A retried record answers the same commit.
    assert_eq!(submit(&f.mesh, &f.lineage, &f.alice.0, &stuck.cut).await, 3);
    let head = domain_head(&f.mesh, &f.lineage);
    assert_eq!(
        (head.commit, head.hashes[2]),
        (4, stuck.cut.record.digest().unwrap())
    );
    assert_eq!(
        rows(&f.alice.1).await,
        [row("0001", "kept"), row("0002", "continued")]
    );
    sync(&f.lineage, &f.alice.1, &f.bob.1, &f.bob.0).await;
    assert_eq!(rows(&f.bob.1).await, rows(&f.alice.1).await);
    assert_eq!(
        f.bob.1.cuts(tip_log(&f.lineage)).await.unwrap(),
        std::slice::from_ref(&stuck.cut)
    );
}

#[tokio::test]
async fn a_member_returning_with_an_abandoned_entry_reverts_it_and_every_device_agrees() {
    let stuck = Stuck::new().await;
    stuck.continue_past_the_cut().await;
    let f = &stuck.fixture;
    let carol = &stuck.carol;
    // The cut arrives before the frames that follow it.
    sync(&f.lineage, &f.alice.1, &carol.1, &carol.0).await;
    let expected = [row("0001", "kept"), row("0002", "continued")];
    assert_eq!(rows(&carol.1).await, expected);
    assert!(abandoned_frames(&carol.1).await > 0);
    assert!(carol.1.cut_guarded_scopes().await.unwrap().is_empty());
    // Abandoned frames relayed to holders of the cut are consumed alike.
    sync(&f.lineage, &carol.1, &f.bob.1, &f.bob.0).await;
    sync(&f.lineage, &carol.1, &f.alice.1, &f.alice.0).await;
    sync(&f.lineage, &f.alice.1, &f.bob.1, &f.bob.0).await;
    for replica in [&f.alice.1, &f.bob.1, &carol.1] {
        assert_eq!(rows(replica).await, expected);
    }
    assert_eq!(
        abandoned_frames(&f.bob.1).await,
        abandoned_frames(&carol.1).await
    );
    // Readmitting the held record changes nothing.
    carol
        .1
        .receive_cuts(std::slice::from_ref(&stuck.cut))
        .await
        .unwrap();
    assert_eq!(rows(&carol.1).await, expected);
    // Another record of the same sequence is a conflict the owner caused.
    let mut other = stuck.cut.record.statement.clone();
    other.issued = 1;
    let conflict = CutEvidence {
        record: SignedCut::sign(other, f.lineage.publisher()).unwrap(),
        authority: CredentialChain(Vec::new()),
    };
    let error = carol.1.receive_cuts(&[conflict]).await.unwrap_err();
    assert!(error.to_string().contains("cut_conflict"), "{error}");
}

#[tokio::test]
async fn a_tainted_checkpoint_guards_its_scope_until_a_clean_base_is_imported() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let carol = &stuck.carol;
    let scope = f.lineage.scope("items");
    // Carol's base covers the abandoned entry; so does another member's.
    carol.1.checkpoint(scope).await.unwrap();
    let erin_key = PrivateKey::new();
    let erin = open(&f.lineage, &f.dir, "erin", &erin_key).await;
    sync(&f.lineage, &carol.1, &erin, &erin_key).await;
    let tainted = erin.checkpoint(scope).await.unwrap();
    assert_eq!(tainted.statement.coordinated.highest, 2);
    stuck.continue_past_the_cut().await;
    carol
        .1
        .receive_cuts(std::slice::from_ref(&stuck.cut))
        .await
        .unwrap();
    assert_eq!(carol.1.cut_guarded_scopes().await.unwrap(), [scope]);
    // Writes and sync stop with `cut_base_missing`; no tainted base helps.
    let sql = session(&carol.1).await;
    let write = identity_insert(
        &sql,
        "INSERT INTO items(key,title) VALUES(?,?)",
        vec![text("0009"), text("refused")],
    )
    .await;
    let error = match write {
        Err(error) => error,
        Ok(_) => sql.run("COMMIT", vec![]).await.unwrap_err(),
    };
    assert!(error.to_string().contains("cut_base_missing"), "{error}");
    drop(sql);
    assert!(carol.1.horizon_rebootstrap_required(scope).await.unwrap());
    let error = carol.1.begin_rebootstrap(tainted).await.unwrap_err();
    assert!(
        error.to_string().contains("abandoned coordinated effects"),
        "{error}"
    );
    assert!(carol.1.checkpoint(scope).await.is_err());
    // A clean base from the fresh node ends the guard.
    let clean = f.alice.1.checkpoint(scope).await.unwrap();
    assert_eq!(clean.statement.coordinated.cuts, 1);
    let credentials = f.lineage.credentials(3, &carol.0);
    carol.1.begin_rebootstrap(clean.clone()).await.unwrap();
    let mut cursor = None;
    loop {
        let page = f
            .alice
            .1
            .checkpoint_page(
                carol.0.public_key(),
                &credentials,
                scope,
                clean.id().unwrap(),
                cursor,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        cursor = page.next;
        carol.1.stage_checkpoint_page(page).await.unwrap();
        if cursor.is_none() {
            break;
        }
    }
    carol.1.finish_rebootstrap(scope).await.unwrap();
    assert!(carol.1.cut_guarded_scopes().await.unwrap().is_empty());
    sync(&f.lineage, &f.alice.1, &carol.1, &carol.0).await;
    let expected = [row("0001", "kept"), row("0002", "continued")];
    assert_eq!(rows(&carol.1).await, expected);
    assert_eq!(rows(&f.alice.1).await, expected);
}

#[tokio::test]
async fn a_crash_after_any_rollback_step_resumes_on_reopen_to_the_same_rows() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let template = f.dir.path().join("carol-template.db");
    stuck.carol.1.backup_into(&template).await.unwrap();
    let expected = [row("0001", "kept")];
    for step in 1..=5 {
        let name = format!("carol-{step}");
        std::fs::copy(&template, f.dir.path().join(format!("{name}.db"))).unwrap();
        let replica = open(&f.lineage, &f.dir, &name, &stuck.carol.0).await;
        replica.fail_after_cut_step(step).await;
        let error = replica
            .receive_cuts(std::slice::from_ref(&stuck.cut))
            .await
            .unwrap_err();
        assert!(matches!(error, Error::InjectedFailure), "{step}: {error}");
        drop(replica);
        let replica = open(&f.lineage, &f.dir, &name, &stuck.carol.0).await;
        assert_eq!(rows(&replica).await, expected, "after step {step}");
        assert!(abandoned_frames(&replica).await > 0, "after step {step}");
        replica
            .receive_cuts(std::slice::from_ref(&stuck.cut))
            .await
            .unwrap();
        assert_eq!(
            rows(&replica).await,
            expected,
            "readmitted after step {step}"
        );
        assert_eq!(
            replica.cuts(tip_log(&f.lineage)).await.unwrap(),
            std::slice::from_ref(&stuck.cut)
        );
    }
}

/// Committed entries as any reachable device serves them, with one
/// entry's frames swapped for a consistent but different identity.
struct Tampered(Peers);
impl EntryPeers for Tampered {
    fn store<'a>(&'a self, node: PublicKey, entry: &'a CoordinatedEntry) -> PeerFuture<'a, ()> {
        self.0.store(node, entry)
    }
    fn committed(&self, log: TipLog, after: u64) -> PeerFuture<'_, Vec<CommittedEntry>> {
        Box::pin(async move {
            let mut entries = self.0.committed(log, after).await?;
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
/// A head whose window starts at the cut: older positions are out of it.
struct Trimmed(rocky_coordinator::coordinated::MemoryLogClient);
impl DomainLog for Trimmed {
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
            head.hashes = head.hashes.split_off(head.hashes.len() - 1);
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
    fn cut(&self, log: TipLog, cut: rocky_core::LogCut) -> CoordinationFuture<'_, LogResult> {
        self.0.cut(log, cut)
    }
}

#[tokio::test]
async fn a_gap_before_the_window_is_endorsed_by_the_cut_and_a_tampered_entry_is_refused() {
    let fixture = Fixture::new(1).await;
    for title in ["first", "second"] {
        fixture.invoice(&fixture.alice, title).await;
    }
    let member_key = PrivateKey::new();
    let member = open(&fixture.lineage, &fixture.dir, "member", &member_key).await;
    sync(&fixture.lineage, &fixture.alice.1, &member, &member_key).await;
    fixture.invoice(&fixture.alice, "lost").await;
    let Fixture {
        lineage,
        dir,
        mesh,
        alice,
        bob,
    } = fixture;
    mesh.nodes.lock().unwrap().clear();
    drop((alice, bob));
    mesh.add(&member_key, &member);
    mesh.log
        .set_owner(lineage.domain, lineage.publisher().public_key());
    // Verified only at the genesis anchor; the cut endorses 1..=2.
    let cut = sign_cut(&lineage, &domain_head(&mesh, &lineage), 0, 2);
    let tampered_key = PrivateKey::new();
    let tampered = open(&lineage, &dir, "tampered", &tampered_key).await;
    assert_eq!(submit(&mesh, &lineage, &tampered_key, &cut).await, 4);
    let slot = tampered
        .coordinated_slot(
            tip_log(&lineage),
            &Trimmed(mesh.log.client(tampered_key.public_key())),
            &Tampered(mesh.peers(&lineage, &tampered_key)),
        )
        .await;
    assert_eq!(slot, Err(CoordinatedError::Behind));
    assert!(rows(&tampered).await.is_empty());
    let key = PrivateKey::new();
    let fresh = open(&lineage, &dir, "fresh", &key).await;
    let slot = fresh
        .coordinated_slot(
            tip_log(&lineage),
            &Trimmed(mesh.log.client(key.public_key())),
            &mesh.peers(&lineage, &key),
        )
        .await
        .unwrap();
    assert_eq!(slot.expected, 4);
    assert_eq!(
        rows(&fresh).await,
        [row("0001", "first"), row("0002", "second")]
    );
}

#[tokio::test]
async fn a_base_finalized_by_a_mergeable_horizon_over_abandoned_effects_fails_closed() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let carol = &stuck.carol;
    let scope = f.lineage.scope("items");
    let statement = carol.1.horizon_statement(scope, &[]).await.unwrap();
    let signed = rocky_credentials::SignedHorizon::sign(statement, f.lineage.publisher()).unwrap();
    carol.1.receive_horizon(signed).await.unwrap();
    let finalized = carol.1.finalize_horizon(scope).await.unwrap();
    assert!(finalized.statement.finalized.is_some());
    assert_eq!(finalized.statement.coordinated.highest, 2);
    stuck.continue_past_the_cut().await;
    carol
        .1
        .receive_cuts(std::slice::from_ref(&stuck.cut))
        .await
        .unwrap();
    // No replay from an earlier base: the scope stays guarded, its rows as
    // they were, until a clean checkpoint is imported.
    assert_eq!(carol.1.cut_guarded_scopes().await.unwrap(), [scope]);
    assert_eq!(
        rows(&carol.1).await,
        [row("0001", "kept"), row("0002", "abandoned")]
    );
    let error = carol.1.checkpoint(scope).await.unwrap_err();
    assert!(error.to_string().contains("cut_base_missing"), "{error}");
    // The finalized base is no base for another holder of the cut either.
    let error = f.alice.1.begin_rebootstrap(finalized).await.unwrap_err();
    assert!(
        error.to_string().contains("abandoned coordinated effects"),
        "{error}"
    );
}

#[tokio::test]
async fn a_returning_trusted_node_reverts_its_own_abandoned_entry_and_its_handle_reports_it() {
    let fixture = Fixture::new(1).await;
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    fixture.invoice(alice, "kept").await;
    let dave_key = PrivateKey::new();
    let dave = open(&fixture.lineage, &fixture.dir, "dave", &dave_key).await;
    sync(&fixture.lineage, &alice.1, &dave, &dave_key).await;
    let write = |sql: SecureSql<TypedRows>, key: &'static str| async move {
        sql.run("BEGIN", vec![]).await?;
        identity_insert(
            &sql,
            "INSERT INTO items(key,title) VALUES(?,?)",
            vec![text(key), text("abandoned")],
        )
        .await?;
        Ok::<_, Error>(sql)
    };
    let slot = fixture.slot(alice).await.unwrap();
    let sql = write(session(&alice.1).await, "0002").await.unwrap();
    sql.commit_coordinated([7; 32], slot).await.unwrap();
    drop(sql);
    assert_eq!(fixture.propose(alice).await.unwrap().position, 2);
    // The whole in-sync set goes away; a fresh node continues past a cut.
    fixture.mesh.nodes.lock().unwrap().clear();
    fixture.mesh.add(&dave_key, &dave);
    let fresh_key = PrivateKey::new();
    let fresh = (
        fresh_key.clone(),
        open(&fixture.lineage, &fixture.dir, "fresh", &fresh_key).await,
    );
    fixture.mesh.add(&fresh.0, &fresh.1);
    fixture.mesh.log.set_owner(
        fixture.lineage.domain,
        fixture.lineage.publisher().public_key(),
    );
    let cut = sign_cut(
        &fixture.lineage,
        &domain_head(&fixture.mesh, &fixture.lineage),
        1,
        1,
    );
    assert_eq!(
        submit(&fixture.mesh, &fixture.lineage, &fresh.0, &cut).await,
        3
    );
    fixture.invoice(&fresh, "continued").await;
    // Alice returns: the head's cut reverts her own committed entry 2.
    fixture.mesh.add(&alice.0, &alice.1);
    let status = fixture.resolve(alice).await;
    assert_eq!(status.position, 4);
    let expected = [row("0001", "kept"), row("0002", "continued")];
    assert_eq!(rows(&alice.1).await, expected);
    assert_eq!(rows(&fresh.1).await, expected);
    // The abandoned write's handle reports it, not success.
    let error = alice.1.handle_committed([7; 32]).await.unwrap_err();
    assert!(error.to_string().contains("abandoned"), "{error}");
    let slot = fixture.slot(alice).await.unwrap();
    let sql = write(session(&alice.1).await, "0003").await.unwrap();
    let error = sql.commit_coordinated([7; 32], slot).await.unwrap_err();
    assert!(error.to_string().contains("abandoned"), "{error}");
    drop(sql);
    // Bob stored entry 2 but never applied it; he resolves past the cut.
    fixture.mesh.add(&bob.0, &bob.1);
    assert_eq!(fixture.resolve(bob).await.position, 4);
    assert_eq!(rows(&bob.1).await, expected);
}

async fn submissions(replica: &Replica) -> i64 {
    match rocky_storage::storage::rows(
        &replica.inner.lock().await.conn,
        "SELECT COUNT(*) FROM secure_cut_submissions",
    )
    .await
    .unwrap()[0]
        .as_slice()
    {
        [SqlValue::Integer(count)] => *count,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_proposed_cut_is_signed_submitted_once_and_never_reuses_a_position() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let (key, fresh) = &f.alice;
    let (client, peers) = (f.mesh.client(key), f.mesh.peers(&f.lineage, key));
    // Collection first: the proposal retains what dave still holds.
    let proposal = fresh
        .cut_proposal(tip_log(&f.lineage), &client, &peers)
        .await
        .unwrap();
    let mut expected = stuck.cut.record.statement.clone();
    expected.issued = proposal.statement.issued;
    assert_eq!(proposal.statement, expected);
    let scope = f.lineage.scope("items");
    assert!(proposal.bases.iter().any(|base| base.scope == scope));
    assert!(proposal.bases.iter().all(|base| base.clean));
    // A key that does not own the domain is refused before anything is
    // flushed or sent.
    let forged = proposal.sign(key, CredentialChain(Vec::new())).unwrap();
    let error = fresh.submit_cut(&forged, &client).await.unwrap_err();
    assert!(matches!(error, CoordinatedError::Storage(_)), "{error}");
    assert_eq!(submissions(fresh).await, 0);
    assert_eq!(domain_head(&f.mesh, &f.lineage).commit, 2);
    // A lost coordinator keeps the flushed record; resolution sends it again.
    let cut = proposal
        .sign(f.lineage.publisher(), CredentialChain(Vec::new()))
        .unwrap();
    f.mesh.log.set_available(false);
    assert_eq!(
        fresh.submit_cut(&cut, &client).await,
        Err(CoordinatedError::Unavailable)
    );
    assert_eq!(submissions(fresh).await, 1);
    f.mesh.log.set_available(true);
    assert_eq!(f.resolve(&f.alice).await.position, 3);
    assert_eq!(
        fresh.cuts(tip_log(&f.lineage)).await.unwrap(),
        std::slice::from_ref(&cut)
    );
    assert_eq!(submissions(fresh).await, 0);
    // The same record answers the same position; another is discarded.
    assert_eq!(fresh.submit_cut(&cut, &client).await, Ok(3));
    assert_eq!(
        fresh.submit_cut(&stuck.cut, &client).await,
        Err(CoordinatedError::Conflict)
    );
    assert_eq!(submissions(fresh).await, 0);
    // The cut holds position 3; the next entry is 4.
    f.invoice(&f.alice, "continued").await;
    let head = domain_head(&f.mesh, &f.lineage);
    assert_eq!(
        (head.commit, head.hashes[2]),
        (4, cut.record.digest().unwrap())
    );
    let expected = [row("0001", "kept"), row("0002", "continued")];
    assert_eq!(rows(fresh).await, expected);
    // Carol returns with abandoned entry 2: she reverts it, and its frames
    // relayed back to the fresh node have no effect there.
    let carol = &stuck.carol;
    sync(&f.lineage, fresh, &carol.1, &carol.0).await;
    assert_eq!(rows(&carol.1).await, expected);
    sync(&f.lineage, &carol.1, fresh, key).await;
    assert_eq!(rows(fresh).await, expected);
    assert!(abandoned_frames(fresh).await > 0);
    assert_eq!(
        abandoned_frames(fresh).await,
        abandoned_frames(&carol.1).await
    );
}

#[tokio::test]
async fn a_proposer_whose_base_holds_an_abandoned_entry_fails_closed() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let carol = &stuck.carol;
    let scope = f.lineage.scope("items");
    // Carol's verified base covers entry 2, which no other device holds.
    let tainted = carol.1.checkpoint(scope).await.unwrap();
    assert_eq!(tainted.statement.coordinated.highest, 2);
    f.mesh.add(&carol.0, &carol.1);
    let (client, peers) = (f.mesh.client(&carol.0), f.mesh.peers(&f.lineage, &carol.0));
    let proposal = carol
        .1
        .cut_proposal(tip_log(&f.lineage), &client, &peers)
        .await
        .unwrap();
    assert_eq!(proposal.statement.retained.position, 1);
    let base = proposal
        .bases
        .iter()
        .find(|base| base.scope == scope)
        .unwrap();
    assert!(!base.clean);
    assert_eq!(base.binding, Some(tainted.statement.coordinated));
    let cut = proposal
        .sign(f.lineage.publisher(), CredentialChain(Vec::new()))
        .unwrap();
    assert_eq!(carol.1.submit_cut(&cut, &client).await, Ok(3));
    // The tainted base is no replay base: the scope is guarded, its rows
    // stay as they were, and it refuses work with `cut_base_missing`.
    assert_eq!(carol.1.cut_guarded_scopes().await.unwrap(), [scope]);
    assert_eq!(
        rows(&carol.1).await,
        [row("0001", "kept"), row("0002", "abandoned")]
    );
    let error = carol.1.checkpoint(scope).await.unwrap_err();
    assert!(error.to_string().contains("cut_base_missing"), "{error}");
    // Another holder of the cut never imports it either.
    assert_eq!(f.resolve(&f.alice).await.position, 3);
    assert_eq!(rows(&f.alice.1).await, [row("0001", "kept")]);
    let error = f.alice.1.begin_rebootstrap(tainted).await.unwrap_err();
    assert!(
        error.to_string().contains("abandoned coordinated effects"),
        "{error}"
    );
}

/// A coordinator that admits a cut and then loses the reply.
struct LostReply(Client);
impl DomainLog for LostReply {
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
    fn cut(&self, log: TipLog, cut: rocky_core::LogCut) -> CoordinationFuture<'_, LogResult> {
        Box::pin(async move {
            self.0.cut(log, cut).await?;
            Err(CoordinationError::Unavailable)
        })
    }
}

#[tokio::test]
async fn a_cut_whose_reply_is_lost_is_admitted_once_on_the_next_resolution() {
    let stuck = Stuck::new().await;
    let f = &stuck.fixture;
    let (key, fresh) = &f.alice;
    let lost = LostReply(f.mesh.client(key));
    let proposal = fresh
        .cut_proposal(tip_log(&f.lineage), &lost, &f.mesh.peers(&f.lineage, key))
        .await
        .unwrap();
    let cut = proposal
        .sign(f.lineage.publisher(), CredentialChain(Vec::new()))
        .unwrap();
    assert_eq!(
        fresh.submit_cut(&cut, &lost).await,
        Err(CoordinatedError::Unavailable)
    );
    // Admitted at the coordinator, undecided here until it is sent again.
    assert_eq!(domain_head(&f.mesh, &f.lineage).commit, 3);
    assert!(fresh.cuts(tip_log(&f.lineage)).await.unwrap().is_empty());
    assert_eq!(submissions(fresh).await, 1);
    assert_eq!(f.resolve(&f.alice).await.position, 3);
    assert_eq!(
        fresh.cuts(tip_log(&f.lineage)).await.unwrap(),
        std::slice::from_ref(&cut)
    );
    assert_eq!(submissions(fresh).await, 0);
    f.invoice(&f.alice, "continued").await;
    assert_eq!(domain_head(&f.mesh, &f.lineage).commit, 4);
}
