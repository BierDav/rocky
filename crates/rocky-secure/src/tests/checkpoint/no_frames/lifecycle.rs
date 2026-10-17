//! Each no-frames record is checked in its own store; covered evidence is
//! refused under an installed base and drops a pulled candidate in its own
//! commit; a candidate is checked again when it is used.
use super::*;
use std::path::PathBuf;

/// A no-frames claim at a seal that covers `covered`, a private frame of
/// E that C holds as ciphertext, and `voidable`, another E frame under it.
struct Contradicted {
    f: Fixture,
    dir: TempDir,
    a: Replica,
    c: Replica,
    e: Replica,
    e_key: PrivateKey,
    empty: SignedCheckpoint,
    covered: EncryptedFrame,
    voidable: EncryptedFrame,
    claim: SignedCheckpoint,
    sealed: Commitment,
}

async fn contradicted() -> Contradicted {
    let f = Fixture::with_publisher(PrivateKey::from_bytes(&[199; 32]));
    let dir = TempDir::new().unwrap();
    let keys = [200, 201, 202].map(|seed| PrivateKey::from_bytes(&[seed; 32]));
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    let empty = a.checkpoint(f.private).await.unwrap();
    let (e, e_credentials) = open_keyed(&f, &dir.path().join("e.db"), &keys[1]).await;
    let (c, c_credentials) = open(&f, &dir.path().join("c.db"), &keys[2]).await;
    for (replica, key, credentials) in [
        (&e, &keys[1], &e_credentials),
        (&c, &keys[2], &c_credentials),
    ] {
        share_graph(&f, &a, (replica, key, credentials)).await;
    }
    let covered = private_row(&f, &e, 9).await;
    let voidable = private_row(&f, &e, 8).await;
    for frame in [&covered, &voidable] {
        assert_ne!(
            c.receive(frame.clone()).await.unwrap(),
            ReceiveStatus::Applied
        );
    }
    sync(&f, &a, (&c, &keys[2], &c_credentials)).await;
    seal_main(&c, &f.publisher, f.public.domain).await;
    let graph = c.tip_graph(f.public.domain).await.unwrap();
    let seal = graph
        .seal(voidable.operation().unwrap().header.extensions.tip)
        .unwrap()
        .unwrap()
        .clone();
    let claim = resigned(&empty, &keys[0], &a_credentials, |s| {
        s.seal = Some(seal.id().unwrap());
        s.tip = seal.statement.tip;
        s.sequence += 1;
        s.previous = Some(empty.id().unwrap());
    });
    e.receive_tip_statements(&crate::tests::all_tip_statements(&c).await)
        .await
        .unwrap();
    Contradicted {
        f,
        dir,
        a,
        c,
        e,
        e_key: keys[1].clone(),
        empty,
        covered,
        voidable,
        claim,
        sealed: seal.id().unwrap(),
    }
}
impl Contradicted {
    /// A node with the graph that holds none of the private history.
    async fn bystander(&self, name: &str, key: &PrivateKey) -> (Replica, PathBuf) {
        let path = self.dir.path().join(format!("{name}.db"));
        let (replica, _) = open(&self.f, &path, key).await;
        for statements in [
            crate::tests::all_tip_statements(&self.c).await,
            crate::tests::all_tip_statements(&self.a).await,
        ] {
            replica.receive_tip_statements(&statements).await.unwrap();
        }
        replica
            .follow_bookmark(self.f.public.domain, "main")
            .await
            .unwrap();
        (replica, path)
    }
    /// A bystander holding the claim as a pulled candidate.
    async fn pulled(&self, name: &str, key: &PrivateKey) -> (Replica, PathBuf) {
        let (replica, path) = self.bystander(name, key).await;
        replica.begin_seal_pull(self.claim.clone()).await.unwrap();
        assert_eq!(
            replica.finish_seal_pull(self.f.private).await.unwrap(),
            self.sealed
        );
        assert_eq!(pulled_rows(&replica).await, 1);
        (replica, path)
    }
    /// A private frame of E after the claimed seal.
    async fn after(&self, row: u8) -> EncryptedFrame {
        private_row(&self.f, &self.e, row).await
    }
    fn void(&self, frame: &EncryptedFrame) -> crate::SignedVoid {
        crate::SignedVoid::sign(&self.e_key, vec![frame.header_bytes().to_vec()]).unwrap()
    }
}
/// Rows of every pulled-state table.
async fn pulled_rows(replica: &Replica) -> i64 {
    count(
        replica,
        "SELECT (SELECT COUNT(*) FROM secure_pulled)+(SELECT COUNT(*) FROM secure_pulled_rows)+(SELECT COUNT(*) FROM secure_pulled_pages)+(SELECT COUNT(*) FROM secure_pulled_stubs)",
    )
    .await
}
async fn pulled_stores(replica: &Replica) -> Vec<Vec<Vec<rocky_core::SqlValue>>> {
    let inner = replica.inner.lock().await;
    let mut stores = Vec::new();
    for table in [
        "secure_pulled",
        "secure_pulled_rows",
        "secure_pulled_pages",
        "secure_pulled_stubs",
    ] {
        stores.push(
            rows(&inner.conn, &format!("SELECT * FROM {table} ORDER BY 1,2"))
                .await
                .unwrap(),
        );
    }
    stores
}
async fn reopen(f: &Fixture, path: &Path, key: &PrivateKey) -> Replica {
    let replica = open(f, path, key).await.0;
    replica.audit(f.public).await.unwrap();
    replica.audit(f.private).await.unwrap();
    replica
}

#[tokio::test]
async fn an_installed_no_frames_base_reopens_under_an_interrupted_keyed_import() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let (a, _) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    let path = dir.path().join("b.db");
    let (b, b_credentials) = open(&f, &path, &keys[1]).await;
    share_graph(&f, &a, (&b, &keys[1], &b_credentials)).await;
    let empty = import_no_frames(&f, &a, (&b, &keys[1], &b_credentials)).await;
    // K holds the key, writes the private scope and compacts a later seal.
    let (k, k_credentials) = open_keyed(&f, &dir.path().join("k.db"), &keys[2]).await;
    sync(&f, &a, (&k, &keys[2], &k_credentials)).await;
    private_row(&f, &k, 9).await;
    seal_main(&k, &f.publisher, f.public.domain).await;
    k.compact(f.public.domain, 0).await.unwrap();
    let keyed = k.checkpoint(f.private).await.unwrap();
    assert!(keyed.statement.key_epoch().is_some());
    assert_eq!(keyed.statement.rows.count, 1);
    // B learns the key and stages K's page over its no-frames base.
    b.install_key_epoch(KeyMaterial::new(f.private, 1, [22; 32]))
        .await
        .unwrap();
    share_graph(&f, &k, (&b, &keys[1], &b_credentials)).await;
    b.begin_rebootstrap(keyed.clone()).await.unwrap();
    let page = page_of(&k, (&keys[1], &b_credentials), &keyed, None).await;
    b.stage_checkpoint_page(page).await.unwrap();
    assert!(
        count(
            &b,
            "SELECT (SELECT COUNT(*) FROM secure_rebootstrap_rows)+(SELECT COUNT(*) FROM secure_rebootstrap_pages)"
        )
        .await
            > 0
    );
    drop(b);
    // The installed base is checked in its own store, the import in its.
    // The application supplies the key again at open.
    let b = open_keyed(&f, &path, &keys[1]).await.0;
    b.audit(f.public).await.unwrap();
    b.audit(f.private).await.unwrap();
    assert_eq!(
        b.checkpoint(f.private).await.unwrap().id().unwrap(),
        empty.id().unwrap()
    );
    let (_, _, complete) = b.rebootstrap_progress(f.private).await.unwrap().unwrap();
    assert!(complete);
    b.finish_rebootstrap(f.private).await.unwrap();
    assert_eq!(
        b.checkpoint(f.private).await.unwrap().id().unwrap(),
        keyed.id().unwrap()
    );
    drop(b);
    open_keyed(&f, &path, &keys[1])
        .await
        .0
        .audit(f.private)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_installed_keyed_base_reopens_under_a_staged_no_frames_import_or_pull() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let keys = [
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    ];
    // K holds the key: its base of the empty private scope is keyed.
    let (k, _) = open_keyed(&f, &dir.path().join("k.db"), &keys[0]).await;
    k.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &k).await;
    let keyed = k.checkpoint(f.private).await.unwrap();
    assert!(keyed.statement.key_epoch().is_some());
    // A, keyless, signs a no-frames base at a later seal.
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[1]).await;
    sync(&f, &k, (&a, &keys[1], &a_credentials)).await;
    seal_main(&a, &f.publisher, f.public.domain).await;
    a.compact(f.public.domain, 0).await.unwrap();
    let empty = a.checkpoint(f.private).await.unwrap();
    assert!(empty.statement.no_frames());
    let sealed = empty.statement.seal.unwrap();
    for (index, pull) in [(2, false), (3, true)] {
        let path = dir.path().join(format!("b{index}.db"));
        let (b, b_credentials) = open_keyed(&f, &path, &keys[index]).await;
        share_graph(&f, &k, (&b, &keys[index], &b_credentials)).await;
        b.begin_rebootstrap(keyed.clone()).await.unwrap();
        let mut after = None;
        loop {
            let page = page_of(&k, (&keys[index], &b_credentials), &keyed, after).await;
            after = page.next;
            b.stage_checkpoint_page(page).await.unwrap();
            if after.is_none() {
                break;
            }
        }
        b.finish_rebootstrap(f.private).await.unwrap();
        assert!(count(&b, "SELECT COUNT(*) FROM secure_checkpoint_pages").await > 0);
        let epochs = b.key_epochs(f.private).await;
        share_graph(&f, &a, (&b, &keys[index], &b_credentials)).await;
        match pull {
            false => b.begin_rebootstrap(empty.clone()).await.unwrap(),
            true => {
                let offered = a
                    .seal_checkpoint_offer(
                        keys[index].public_key(),
                        &b_credentials,
                        f.private,
                        sealed,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                b.begin_seal_pull(offered).await.unwrap();
            }
        }
        drop(b);
        let reopened = open_keyed(&f, &path, &keys[index]).await.0;
        reopened.audit(f.private).await.unwrap();
        let b = reopened;
        match pull {
            false => {
                let report = b.finish_rebootstrap(f.private).await.unwrap();
                assert_eq!(report.superseded, 0);
                assert_eq!(
                    b.checkpoint(f.private).await.unwrap().id().unwrap(),
                    empty.id().unwrap()
                );
            }
            true => {
                assert_eq!(b.finish_seal_pull(f.private).await.unwrap(), sealed);
                assert_eq!(
                    b.checkpoint(f.private).await.unwrap().id().unwrap(),
                    keyed.id().unwrap()
                );
            }
        }
        assert_eq!(b.key_epochs(f.private).await, epochs);
        drop(b);
        open_keyed(&f, &path, &keys[index])
            .await
            .0
            .audit(f.private)
            .await
            .unwrap();
    }
}

/// Copies the closed file at `from`, with its log, to `to`.
fn copy_file(from: &Path, to: &Path) {
    std::fs::copy(from, to).unwrap();
    let mut wal = from.as_os_str().to_owned();
    wal.push("-wal");
    let mut copied = to.as_os_str().to_owned();
    copied.push("-wal");
    if std::path::Path::new(&wal).exists() {
        std::fs::copy(&wal, &copied).unwrap();
    }
}
/// Each `tamper` applied to a copy of the closed file at `path` fails the
/// full audit of `scope` and the next open.
async fn refused_when_tampered(
    f: &Fixture,
    path: &Path,
    key: &PrivateKey,
    tampers: &[&str],
    bindings: Vec<Vec<u8>>,
) {
    for (index, sql) in tampers.iter().enumerate() {
        let copy = path.with_extension(format!("tamper{index}.db"));
        copy_file(path, &copy);
        let replica = open(f, &copy, key).await.0;
        {
            let mut inner = replica.inner.lock().await;
            let values: Vec<_> = bindings
                .iter()
                .take(sql.matches('?').count())
                .cloned()
                .map(rocky_core::SqlValue::Blob)
                .collect();
            inner
                .conn
                .execute(*sql, crate::storage::bindings(values))
                .await
                .unwrap();
            inner.flush().unwrap();
        }
        assert!(replica.audit(f.private).await.is_err(), "{sql}");
        drop(replica);
        let reopened =
            EncryptedReplica::open(&copy, f.app(), f.config(key, f.full(), &[f.public])).await;
        assert!(reopened.is_err(), "{sql}");
    }
}

const PRIVATE: &str = "(SELECT MAX(id) FROM secure_scope_state)";

#[tokio::test]
async fn tampered_no_frames_records_fail_open_and_audit_in_their_own_store() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let a_path = dir.path().join("a.db");
    let (a, _) = open(&f, &a_path, &keys[0]).await;
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    let empty = a.checkpoint(f.private).await.unwrap();
    let sealed = empty.statement.seal.unwrap();
    // A staged and a pulled no-frames record, each on its own node.
    let staged_path = dir.path().join("staged.db");
    let (staged, staged_credentials) = open(&f, &staged_path, &keys[1]).await;
    share_graph(&f, &a, (&staged, &keys[1], &staged_credentials)).await;
    staged.begin_rebootstrap(empty.clone()).await.unwrap();
    let pulled_path = dir.path().join("pulled.db");
    let (pulled, pulled_credentials) = open(&f, &pulled_path, &keys[2]).await;
    share_graph(&f, &a, (&pulled, &keys[2], &pulled_credentials)).await;
    pulled.begin_seal_pull(empty.clone()).await.unwrap();
    pulled.finish_seal_pull(f.private).await.unwrap();
    for replica in [&a, &staged, &pulled] {
        replica.audit(f.private).await.unwrap();
    }
    drop((a, staged, pulled));
    let page = format!("INSERT INTO secure_checkpoint_pages VALUES({PRIVATE},x'00',NULL,x'00')");
    refused_when_tampered(&f, &a_path, &keys[0], &[&page], vec![]).await;
    let staged_page =
        format!("INSERT INTO secure_rebootstrap_pages VALUES({PRIVATE},x'00',NULL,x'00')");
    refused_when_tampered(
        &f,
        &staged_path,
        &keys[1],
        &[
            &staged_page,
            "UPDATE secure_rebootstrap SET cursor=zeroblob(32)",
            "UPDATE secure_rebootstrap SET complete=0",
            "UPDATE secure_rebootstrap SET direct=1",
        ],
        vec![],
    )
    .await;
    let mut forged = empty.clone();
    forged.statement.sequence += 1;
    let pulled_tampers = [
        format!("INSERT INTO secure_pulled_rows VALUES({PRIVATE},x'00',x'00')"),
        format!("INSERT INTO secure_pulled_pages VALUES({PRIVATE},x'00',NULL,x'00')"),
        format!("INSERT INTO secure_pulled_stubs VALUES({PRIVATE},zeroblob(32),0)"),
        "UPDATE secure_pulled SET sets=x'00'".to_string(),
        "UPDATE secure_pulled SET signed=?".to_string(),
        "UPDATE secure_pulled SET scope=scope+100".to_string(),
        "UPDATE secure_pulled SET seal=zeroblob(32)".to_string(),
    ];
    refused_when_tampered(
        &f,
        &pulled_path,
        &keys[2],
        &pulled_tampers
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![forged.to_bytes().unwrap()],
    )
    .await;
    // The untampered files still open and audit.
    for (path, key) in [
        (&a_path, &keys[0]),
        (&staged_path, &keys[1]),
        (&pulled_path, &keys[2]),
    ] {
        reopen(&f, path, key).await;
    }
    let _ = sealed;
}

#[tokio::test]
async fn a_frame_that_passed_the_early_check_is_refused_once_a_no_frames_base_installs() {
    let x = contradicted().await;
    let key = PrivateKey::new();
    let (d, path) = x.bystander("d", &key).await;
    d.begin_rebootstrap(x.claim.clone()).await.unwrap();
    // The frame passes the early check while no base is installed; the
    // base installs before the frame's own transaction runs.
    let (reached, resume) = d.gate_next_receive().await;
    let receiving = {
        let (d, frame) = (d.clone(), x.covered.clone());
        tokio::spawn(async move { d.receive(frame).await })
    };
    reached.await.unwrap();
    d.finish_rebootstrap(x.f.private).await.unwrap();
    resume.send(()).unwrap();
    let error = receiving.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("contradicts"), "{error}");
    assert_eq!(count(&d, "SELECT COUNT(*) FROM secure_operations").await, 0);
    // A covered void is refused too and stores nothing; one after the seal
    // is admitted as usual.
    let results = d.receive_voids(&[x.void(&x.voidable)]).await;
    let error = results.into_iter().next().unwrap().unwrap_err();
    assert!(error.to_string().contains("contradicts"), "{error}");
    assert_eq!(count(&d, "SELECT COUNT(*) FROM secure_voids").await, 0);
    let after = x.after(10).await;
    let results = d.receive_voids(&[x.void(&after)]).await;
    assert_eq!(
        results.into_iter().next().unwrap().unwrap(),
        ReceiveStatus::Applied
    );
    // A held transfer of a live frame is refused before intake runs; the
    // contradiction check of held frames is not exercised here.
    let error = d
        .receive_held_frames(vec![x.covered.clone()])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("live or unrelated"), "{error}");
    assert_eq!(count(&d, "SELECT COUNT(*) FROM secure_operations").await, 0);
    drop(d);
    reopen(&x.f, &path, &key).await;
}

#[tokio::test]
async fn covered_evidence_drops_a_pulled_candidate_in_its_own_commit() {
    let x = contradicted().await;
    let key = PrivateKey::new();
    let (d, path) = x.pulled("d", &key).await;
    // Evidence after the seal keeps the candidate.
    let after = x.after(10).await;
    d.receive(after).await.unwrap();
    assert_eq!(pulled_rows(&d).await, 1);
    // A store that fails before commit keeps neither the frame nor the drop.
    d.inject_group_fault(Some(crate::replica::group::GroupFault::FailBeforeCommit))
        .await;
    assert!(d.receive(x.covered.clone()).await.is_err());
    d.inject_group_fault(None).await;
    assert_eq!(pulled_rows(&d).await, 1);
    let held = count(&d, "SELECT COUNT(*) FROM secure_operations").await;
    // The covered frame is stored and the candidate leaves with it.
    assert_ne!(
        d.receive(x.covered.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(
        count(&d, "SELECT COUNT(*) FROM secure_operations").await,
        held + 1
    );
    assert_eq!(pulled_rows(&d).await, 0);
    drop(d);
    let d = reopen(&x.f, &path, &key).await;
    let domain = x.f.public.domain;
    assert_ne!(
        d.held_seal(domain, x.sealed).await.unwrap(),
        Some(crate::HeldSeal::Checkpoint)
    );
    assert!(
        d.seal_pull_scopes(domain, x.sealed)
            .await
            .unwrap()
            .contains(&x.f.private)
    );
    assert!(d.checkpoint(x.f.private).await.is_err());
    // A covered void drops a candidate the same way.
    let (v, v_path) = x.pulled("v", &key).await;
    let results = v.receive_voids(&[x.void(&x.voidable)]).await;
    assert_eq!(
        results.into_iter().next().unwrap().unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(count(&v, "SELECT COUNT(*) FROM secure_voids").await, 1);
    assert_eq!(pulled_rows(&v).await, 0);
    drop(v);
    reopen(&x.f, &v_path, &key).await;
}

#[tokio::test]
async fn a_staged_no_frames_claim_survives_covered_history_until_finish_refuses_it() {
    let x = contradicted().await;
    let key = PrivateKey::new();
    let (d, path) = x.bystander("d", &key).await;
    d.begin_rebootstrap(x.claim.clone()).await.unwrap();
    assert_ne!(
        d.receive(x.covered.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    drop(d);
    let d = reopen(&x.f, &path, &key).await;
    let error = d.finish_rebootstrap(x.f.private).await.unwrap_err();
    assert!(matches!(error, Error::Rejected(_)), "{error}");
    assert!(d.rebootstrap_progress(x.f.private).await.unwrap().is_none());
    assert!(d.checkpoint(x.f.private).await.is_err());
    drop(d);
    reopen(&x.f, &path, &key).await;
}

#[tokio::test]
async fn revoking_the_signer_refuses_a_no_frames_state_before_it_is_used() {
    let x = contradicted().await;
    let signer = PrivateKey::new();
    let credentials = x.f.credentials(&signer, x.f.full());
    // An uncontradicted statement of a signer revoked later.
    let honest = resigned(&x.empty, &signer, &credentials, |_| {});
    let revoked = crate::tests::revoking(
        &x.f.publisher,
        x.f.app().definition().app,
        &[],
        &[credentials.0[2].id().unwrap()],
    );
    let private = x.f.private;
    // Revoked between begin and finish of a pull: refused, not corrupt.
    let key = PrivateKey::new();
    let (d, path) = x.bystander("pull", &key).await;
    d.begin_seal_pull(honest.clone()).await.unwrap();
    d.learn_revocations(&revoked).await.unwrap();
    let error = d.finish_seal_pull(private).await.unwrap_err();
    assert!(matches!(error, Error::Rejected(_)), "{error}");
    assert!(d.rebootstrap_progress(private).await.unwrap().is_none());
    assert_eq!(pulled_rows(&d).await, 0);
    drop(d);
    reopen(&x.f, &path, &key).await;
    // Revoked between begin and finish of an import: nothing installs.
    let (d, path) = x.bystander("import", &key).await;
    d.begin_rebootstrap(honest.clone()).await.unwrap();
    d.learn_revocations(&revoked).await.unwrap();
    assert!(d.finish_rebootstrap(private).await.is_err());
    assert!(d.checkpoint(private).await.is_err());
    drop(d);
    reopen(&x.f, &path, &key).await;
    // Revoked once pulled: learning the revocation drops the candidate.
    let (d, path) = x.bystander("kept", &key).await;
    d.begin_seal_pull(honest).await.unwrap();
    d.finish_seal_pull(private).await.unwrap();
    assert_eq!(pulled_rows(&d).await, 1);
    d.learn_revocations(&revoked).await.unwrap();
    assert_eq!(pulled_rows(&d).await, 0);
    drop(d);
    reopen(&x.f, &path, &key).await;
}

/// B compacted at a later seal than A, which keeps its seals; B pulls A's
/// states at the first seal, where a `dev` branch opens. With `resign`,
/// another authorized signer restates the private no-frames state.
struct DevPull {
    f: Fixture,
    _dir: TempDir,
    a: Replica,
    b: Replica,
    b_key: PrivateKey,
    b_path: PathBuf,
    first: Commitment,
    pulled: BTreeMap<ResourceScope, SignedCheckpoint>,
    /// A's chain and the restating signer's.
    signers: [CredentialChain; 2],
    restater: PrivateKey,
}

/// The seal the owner just placed on `tip`.
async fn seal_on(
    replica: &Replica,
    domain: rocky_core::Scope,
    tip: rocky_core::TipId,
) -> Commitment {
    replica
        .tip_graph(domain)
        .await
        .unwrap()
        .seal(tip)
        .unwrap()
        .unwrap()
        .id()
        .unwrap()
}

async fn dev_pull(resign: bool) -> DevPull {
    use rocky_credentials::{
        BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SignedBookmarkMove, SignedTipOpening,
        TIP_OPENING_FORMAT, TipOpeningStatement,
    };
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let domain = f.public.domain;
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    a.keep_seals(crate::KeptSeals {
        every: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let b_path = dir.path().join("b.db");
    let (b, b_credentials) = open(&f, &b_path, &keys[1]).await;
    a.author_buffered(handle(1), insert(&f, 1, "one"), vec![])
        .await
        .unwrap();
    sync(&f, &a, (&b, &keys[1], &b_credentials)).await;
    let tip = a.followed_tip().await;
    seal_main(&a, &f.publisher, domain).await;
    let first = seal_on(&a, domain, tip).await;
    assert_eq!(a.compact(domain, 0).await.unwrap().compacted, Some(first));
    a.author_buffered(handle(2), insert(&f, 2, "two"), vec![])
        .await
        .unwrap();
    let tip = a.followed_tip().await;
    seal_main(&a, &f.publisher, domain).await;
    let second = seal_on(&a, domain, tip).await;
    sync(&f, &a, (&b, &keys[1], &b_credentials)).await;
    assert_eq!(b.compact(domain, 0).await.unwrap().compacted, Some(second));
    assert!(b.checkpoint(f.private).await.unwrap().statement.no_frames());
    let opening = SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            schema: b.tip_graph(domain).await.unwrap().genesis_schema,
            scope: domain,
            seal: first,
            minimum_in_sync: 1,
        },
        &f.publisher,
    )
    .unwrap();
    let bookmark = SignedBookmarkMove::sign(
        BookmarkMoveStatement {
            format: BOOKMARK_MOVE_FORMAT,
            scope: domain,
            name: "dev".into(),
            from: None,
            to: Some(opening.tip().unwrap()),
        },
        &f.publisher,
    )
    .unwrap();
    b.publish_tip_statements(&[
        crate::TipStatement::Opening(opening),
        crate::TipStatement::Bookmark(bookmark),
    ])
    .await
    .unwrap();
    let signer = f.credentials(&keys[2], f.full());
    let mut pulled = BTreeMap::new();
    for scope in b.seal_pull_scopes(domain, first).await.unwrap() {
        a.admit_peer(keys[1].public_key(), &b_credentials, b.followed_tip().await)
            .await
            .unwrap();
        let offered = a
            .checkpoint_offer(keys[1].public_key(), &b_credentials, scope)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(offered.statement.seal, Some(first));
        let checkpoint = offered.id().unwrap();
        let offered = match resign && scope == f.private {
            true => resigned(&offered, &keys[2], &signer, |_| {}),
            false => offered,
        };
        b.begin_seal_pull(offered.clone()).await.unwrap();
        let mut after = None;
        while !offered.statement.no_frames() {
            let page = a
                .checkpoint_page(
                    keys[1].public_key(),
                    &b_credentials,
                    scope,
                    checkpoint,
                    after,
                    64 * 1024,
                )
                .await
                .unwrap();
            after = page.next;
            b.stage_checkpoint_page(page).await.unwrap();
            if after.is_none() {
                break;
            }
        }
        assert_eq!(b.finish_seal_pull(scope).await.unwrap(), first);
        pulled.insert(scope, offered);
    }
    assert_eq!(pulled.len(), 2);
    DevPull {
        f,
        _dir: dir,
        a,
        b,
        b_key: keys[1].clone(),
        b_path,
        first,
        pulled,
        signers: [a_credentials, signer],
        restater: keys[2].clone(),
    }
}
impl DevPull {
    /// Revokes A's node (`0`) or the restating signer (`1`).
    fn revoked(&self, signer: usize) -> rocky_credentials::RevocationSet {
        crate::tests::revoking(
            &self.f.publisher,
            self.f.app().definition().app,
            &[],
            &[self.signers[signer].0[2].id().unwrap()],
        )
    }
    async fn pulled_scopes(&self) -> i64 {
        count(&self.b, "SELECT COUNT(*) FROM secure_pulled").await
    }
}

#[tokio::test]
async fn following_through_a_pulled_no_frames_seal_installs_it_and_serves_it_onward() {
    let x = dev_pull(false).await;
    let (f, b) = (&x.f, &x.b);
    b.follow_bookmark(f.public.domain, "dev").await.unwrap();
    assert_eq!(pulled_rows(b).await, 0);
    let empty = &x.pulled[&f.private];
    assert!(empty.statement.no_frames());
    assert_eq!(
        b.checkpoint(f.private).await.unwrap().id().unwrap(),
        empty.id().unwrap()
    );
    assert!(b.key_epochs(f.private).await.is_empty());
    assert!(
        b.seal_rebootstraps(f.public.domain)
            .await
            .unwrap()
            .is_empty()
    );
    drop(x.b);
    let b = reopen(f, &x.b_path, &x.b_key).await;
    // Onward: a joiner on the branch imports the same statement from B.
    let c_key = PrivateKey::new();
    let (c, c_credentials) = open(f, &x._dir.path().join("c.db"), &c_key).await;
    c.receive_tip_statements(&crate::tests::all_tip_statements(&b).await)
        .await
        .unwrap();
    c.follow_bookmark(f.public.domain, "dev").await.unwrap();
    let served = import_no_frames(f, &b, (&c, &c_key, &c_credentials)).await;
    assert_eq!(served.id().unwrap(), empty.id().unwrap());
}

#[tokio::test]
async fn a_pulled_state_that_no_longer_passes_neither_starts_a_branch_nor_is_dropped_by_reading() {
    let x = dev_pull(true).await;
    let (f, b) = (&x.f, &x.b);
    let domain = f.public.domain;
    // The signer's revocation reaches the set without the revalidation
    // learning it runs: reading availability must not count the state.
    b.inner.lock().await.config.revocations = x.revoked(1);
    let before = pulled_rows(b).await;
    let tip = b.followed_tip().await;
    let refused = b.follow_bookmark(domain, "dev").await.unwrap_err();
    assert!(refused.to_string().contains("does not hold"), "{refused}");
    assert_eq!(b.followed_tip().await, tip);
    assert_ne!(
        b.held_seal(domain, x.first).await.unwrap(),
        Some(crate::HeldSeal::Checkpoint)
    );
    assert_eq!(
        b.seal_pull_scopes(domain, x.first).await.unwrap(),
        vec![f.private]
    );
    // Reading writes nothing.
    assert_eq!(pulled_rows(b).await, before);
    drop(x.b);
    reopen(f, &x.b_path, &x.b_key).await;
}

/// The contradicted fixture's bystander D holding A's public base at the
/// first seal and `claim`, a pulled no-frames state of the private scope at
/// the later seal, whose frames D lacks: the scope needs it.
async fn needing(
    x: &Contradicted,
    name: &str,
    claim: &SignedCheckpoint,
) -> (Replica, PathBuf, PrivateKey) {
    let f = &x.f;
    let key = PrivateKey::new();
    let (d, path) = x.bystander(name, &key).await;
    let credentials = d.inner.lock().await.config.credentials.clone();
    x.a.receive_tip_statements(&crate::tests::all_tip_statements(&x.c).await)
        .await
        .unwrap();
    x.a.admit_peer(key.public_key(), &credentials, d.followed_tip().await)
        .await
        .unwrap();
    let base = x.a.checkpoint(f.public).await.unwrap();
    d.begin_rebootstrap(base.clone()).await.unwrap();
    let mut after = None;
    loop {
        let page = page_of(&x.a, (&key, &credentials), &base, after).await;
        after = page.next;
        d.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
    d.finish_rebootstrap(f.public).await.unwrap();
    d.begin_seal_pull(claim.clone()).await.unwrap();
    assert_eq!(d.finish_seal_pull(f.private).await.unwrap(), x.sealed);
    (d, path, key)
}
#[tokio::test]
async fn learning_a_revocation_drops_the_signers_pulled_states_before_any_follow() {
    // The restated no-frames state alone, or both of A's states, one keyed.
    for restated in [true, false] {
        let x = dev_pull(restated).await;
        let (f, b) = (&x.f, &x.b);
        let domain = f.public.domain;
        let dropped = match restated {
            true => vec![f.private],
            false => vec![f.public, f.private],
        };
        b.learn_revocations(&x.revoked(usize::from(restated)))
            .await
            .unwrap();
        assert_eq!(x.pulled_scopes().await, 2 - dropped.len() as i64);
        let mut wanted = b.seal_pull_scopes(domain, x.first).await.unwrap();
        wanted.sort();
        assert_eq!(wanted, dropped);
        // The seal is no longer held: following the branch is refused
        // until it is pulled again, and nothing was installed.
        let tip = b.followed_tip().await;
        let refused = b.follow_bookmark(domain, "dev").await.unwrap_err();
        assert!(refused.to_string().contains("does not hold"), "{refused}");
        assert_eq!(b.followed_tip().await, tip);
        assert_ne!(
            b.held_seal(domain, x.first).await.unwrap(),
            Some(crate::HeldSeal::Checkpoint)
        );
        for scope in [f.public, f.private] {
            assert_ne!(
                b.checkpoint(scope).await.unwrap().id().unwrap(),
                x.pulled[&scope].id().unwrap()
            );
        }
        drop(x.b);
        reopen(f, &x.b_path, &x.b_key).await;
    }
}

#[tokio::test]
async fn a_tampered_pulled_state_fails_install_as_corruption_and_stays() {
    let x = dev_pull(false).await;
    let (f, b) = (&x.f, &x.b);
    {
        let inner = b.inner.lock().await;
        inner
            .conn
            .execute(
                &format!("INSERT INTO secure_pulled_stubs VALUES({PRIVATE},zeroblob(32),0)"),
                (),
            )
            .await
            .unwrap();
    }
    let error = b.follow_bookmark(f.public.domain, "dev").await.unwrap_err();
    assert!(matches!(error, Error::Corrupt(_)), "{error}");
    assert_eq!(x.pulled_scopes().await, 2);
    assert!(b.audit(f.private).await.is_err());
}

#[tokio::test]
async fn a_covered_placement_is_refused_under_a_base_and_drops_a_candidate() {
    let x = contradicted().await;
    let f = &x.f;
    let header = &x.covered.operation().unwrap().header;
    let request = crate::PlacementRequest {
        seal: x.sealed,
        author: header.extensions.author.unwrap(),
        from: 0,
        to: header.extensions.author_sequence,
    };
    // Custody of the private scope ended: its positions arrive as placements.
    let public_only: Vec<_> = f
        .full()
        .into_iter()
        .filter(|grant| grant.scope == f.public)
        .collect();
    let key = PrivateKey::new();
    for installed in [true, false] {
        let name = if installed { "base" } else { "candidate" };
        let (d, path) = match installed {
            true => {
                let (d, path) = x.bystander(name, &key).await;
                d.begin_rebootstrap(x.claim.clone()).await.unwrap();
                d.finish_rebootstrap(f.private).await.unwrap();
                (d, path)
            }
            false => x.pulled(name, &key).await,
        };
        drop(d);
        let config = f.config(&key, public_only.clone(), &[f.public]);
        let credentials = config.credentials.clone();
        let d = EncryptedReplica::open(&path, f.app(), config)
            .await
            .unwrap();
        x.c.admit_peer(key.public_key(), &credentials, d.followed_tip().await)
            .await
            .unwrap();
        let page =
            x.c.placement_page(
                key.public_key(),
                &credentials,
                &std::collections::BTreeSet::new(),
                request,
            )
            .await
            .unwrap();
        let received = d.receive_placements(request, page).await;
        match installed {
            true => {
                let error = received.unwrap_err();
                assert!(error.to_string().contains("contradicts"), "{error}");
                assert_eq!(count(&d, "SELECT COUNT(*) FROM secure_placements").await, 0);
            }
            false => {
                assert!(received.unwrap() > 0);
                assert_eq!(pulled_rows(&d).await, 0);
            }
        }
        drop(d);
        let d = EncryptedReplica::open(
            &path,
            f.app(),
            f.config(&key, public_only.clone(), &[f.public]),
        )
        .await
        .unwrap();
        d.audit(f.public).await.unwrap();
    }
}

const DAY: u64 = 86_400;

#[tokio::test]
async fn an_expired_pulled_state_covers_nothing_and_install_drops_it() {
    let x = contradicted().await;
    let f = &x.f;
    let domain = f.public.domain;
    let signer = PrivateKey::new();
    let current = f.credentials(&signer, f.full());
    // Control: a valid needed state is installed before readiness is judged.
    let valid = resigned(&x.claim, &signer, &current, |_| {});
    let (d, _, _) = needing(&x, "valid", &valid).await;
    assert!(
        d.seal_rebootstraps(domain)
            .await
            .unwrap()
            .iter()
            .any(|need| need.scope == f.private)
    );
    d.finalize_path(domain).await.unwrap();
    assert_eq!(pulled_rows(&d).await, 0);
    assert_eq!(
        d.checkpoint(f.private).await.unwrap().id().unwrap(),
        valid.id().unwrap()
    );
    // The same statement under a chain valid only in the past.
    let time = crate::now().unwrap();
    let past = f.chain(
        &signer,
        f.full(),
        &f.publisher,
        (time - 2 * DAY, time - DAY),
    );
    let expired = resigned(&x.claim, &signer, &past, |_| {});
    d.verify_checkpoint_at_for_test(&expired, time - 3 * DAY / 2)
        .await
        .unwrap();
    assert!(
        d.verify_checkpoint_at_for_test(&expired, time)
            .await
            .is_err()
    );
    let (e, path, key) = needing(&x, "expired", &valid).await;
    {
        let inner = e.inner.lock().await;
        inner
            .conn
            .execute(
                "UPDATE secure_pulled SET signed=?",
                [expired.to_bytes().unwrap()],
            )
            .await
            .unwrap();
    }
    // An unrelated staged import of the scope at another seal.
    e.begin_rebootstrap(x.empty.clone()).await.unwrap();
    let staged = "SELECT quote(signed)||quote(sets)||complete||direct FROM secure_rebootstrap";
    let staged_before = crate::storage::rows(&e.inner.lock().await.conn, staged)
        .await
        .unwrap();
    drop(e);
    // Open and the full audit check only its integrity.
    let e = reopen(f, &path, &key).await;
    assert_ne!(
        e.held_seal(domain, x.sealed).await.unwrap(),
        Some(crate::HeldSeal::Checkpoint)
    );
    assert!(
        e.seal_pull_scopes(domain, x.sealed)
            .await
            .unwrap()
            .contains(&f.private)
    );
    let finalized = e.compaction_status(domain).await.unwrap().finalized;
    // A failure later in the installing transaction keeps the candidate.
    e.inject_stage_fault(Stage::Settle, 0).await;
    assert!(matches!(
        e.finish_seal_rebootstrap(domain).await,
        Err(Error::InjectedFailure)
    ));
    assert_eq!(pulled_rows(&e).await, 1);
    // Install refuses it, the drop commits and the scope keeps waiting.
    assert!(!e.finish_seal_rebootstrap(domain).await.unwrap());
    assert_eq!(pulled_rows(&e).await, 0);
    assert!(
        e.seal_rebootstraps(domain)
            .await
            .unwrap()
            .iter()
            .any(|need| need.scope == f.private)
    );
    assert_eq!(
        crate::storage::rows(&e.inner.lock().await.conn, staged)
            .await
            .unwrap(),
        staged_before
    );
    e.finalize_path(domain).await.unwrap();
    e.compact(domain, 0).await.unwrap();
    let status = e.compaction_status(domain).await.unwrap();
    assert_eq!(status.finalized, finalized);
    assert_ne!(status.finalized, Some(x.sealed));
    assert!(e.checkpoint(f.private).await.is_err());
    drop(e);
    reopen(f, &path, &key).await;
}

#[tokio::test]
async fn an_installed_no_frames_base_audits_against_its_exact_seal_after_the_sealer_is_revoked() {
    use rocky_credentials::{Authority, SEAL_FORMAT, SealAction, SealStatement, SignedSeal};
    let f = Fixture::new();
    let domain = f.public.domain;
    let dir = TempDir::new().unwrap();
    let keys = [
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    ];
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    let empty = a.checkpoint(f.private).await.unwrap();
    let (e, e_credentials) = open_keyed(&f, &dir.path().join("e.db"), &keys[1]).await;
    let (c, c_credentials) = open(&f, &dir.path().join("c.db"), &keys[2]).await;
    for (replica, key, credentials) in [
        (&e, &keys[1], &e_credentials),
        (&c, &keys[2], &c_credentials),
    ] {
        share_graph(&f, &a, (replica, key, credentials)).await;
    }
    let covered = private_row(&f, &e, 9).await;
    // Written on the same tip, but C never holds it before L.
    let suffix = private_row(&f, &e, 10).await;
    assert_ne!(
        c.receive(covered.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    sync(&f, &a, (&c, &keys[2], &c_credentials)).await;
    // An issuer closes main's tip at L; main itself stays there.
    let issuer = PrivateKey::new();
    let chain = CredentialChain(vec![
        Authority::owner(&f.publisher)
            .issue_issuer(issuer.public_key(), None, crate::tests::validity())
            .unwrap(),
    ]);
    let tip = c.followed_tip().await;
    let sealed = SignedSeal::endorse(
        SealStatement {
            format: SEAL_FORMAT,
            scope: domain,
            tip,
            frontier: c.seal_frontier(domain, false).await.unwrap(),
            action: SealAction::Close,
            recovery: None,
        },
        &issuer,
        &chain,
    )
    .unwrap();
    c.receive_tip_statements(&[crate::TipStatement::Seal(sealed.clone())])
        .await
        .unwrap();
    let claim = resigned(&empty, &keys[0], &a_credentials, |s| {
        s.seal = Some(sealed.id().unwrap());
        s.tip = tip;
        s.sequence += 1;
        s.previous = Some(empty.id().unwrap());
    });
    let path = dir.path().join("d.db");
    let (d, _) = open(&f, &path, &keys[3]).await;
    for statements in [
        crate::tests::all_tip_statements(&c).await,
        crate::tests::all_tip_statements(&a).await,
    ] {
        d.receive_tip_statements(&statements).await.unwrap();
    }
    d.follow_bookmark(domain, "main").await.unwrap();
    assert_eq!(d.followed_tip().await, tip);
    d.begin_rebootstrap(claim.clone()).await.unwrap();
    d.finish_rebootstrap(f.private).await.unwrap();
    // A frame beyond L's frontier is retained; one L includes is refused.
    // Beyond the closed tip's seal it is stored, keyless here.
    assert_eq!(d.receive(suffix).await.unwrap(), ReceiveStatus::MissingKey);
    let retained = count(&d, "SELECT COUNT(*) FROM secure_operations").await;
    assert_eq!(retained, 1);
    let refused = |d: Replica| {
        let covered = covered.clone();
        async move {
            let error = d.receive(covered).await.unwrap_err();
            assert!(error.to_string().contains("contradicts"), "{error}");
            d
        }
    };
    let d = refused(d).await;
    // The sealer's revocation excludes L: no seal wins the tip now.
    d.learn_revocations(&crate::tests::revoking(
        &f.publisher,
        domain.app,
        &[issuer.public_key()],
        &[],
    ))
    .await
    .unwrap();
    assert_eq!(d.tip_graph(domain).await.unwrap().seal(tip).unwrap(), None);
    assert_eq!(d.followed_tip().await, tip);
    let d = refused(d).await;
    assert_eq!(
        count(&d, "SELECT COUNT(*) FROM secure_operations").await,
        retained
    );
    drop(d);
    // The recorded fact still audits against exactly L.
    let d = reopen(&f, &path, &keys[3]).await;
    assert_eq!(
        count(&d, "SELECT COUNT(*) FROM secure_operations").await,
        retained
    );
    let stored = rows(
        &d.inner.lock().await.conn,
        "SELECT signed FROM secure_checkpoints",
    )
    .await
    .unwrap();
    assert!(stored.iter().any(|row| match &row[0] {
        rocky_core::SqlValue::Blob(bytes) => *bytes == claim.to_bytes().unwrap(),
        _ => false,
    }));
    // The owner closes T with a different frontier. The frame is now truly
    // Held by the live graph, but the installed fact still covers it at L.
    let close = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: domain,
            tip,
            frontier: Vec::new(),
            action: SealAction::Close,
            recovery: None,
        },
        &f.publisher,
    )
    .unwrap();
    d.receive_tip_statements(&[crate::TipStatement::Seal(close)])
        .await
        .unwrap();
    assert_eq!(
        d.inner.lock().await.frame_reach(&covered).unwrap(),
        crate::replica::tips::FrameReach::Held
    );
    let revision = d.stored_and_memory_revision().await;
    let error = d.receive_held_frames(vec![covered]).await.unwrap_err();
    assert!(error.to_string().contains("contradicts"), "{error}");
    assert_eq!(d.stored_and_memory_revision().await, revision);
    assert_eq!(
        rows(
            &d.inner.lock().await.conn,
            "SELECT signed FROM secure_checkpoints"
        )
        .await
        .unwrap(),
        stored
    );
    assert_eq!(
        count(&d, "SELECT COUNT(*) FROM secure_operations").await,
        retained
    );
    drop(d);
    reopen(&f, &path, &keys[3]).await;
}

#[tokio::test]
async fn an_author_log_fork_rolls_back_the_pulled_candidate_drop() {
    let x = contradicted().await;
    let key = PrivateKey::from_bytes(&[203; 32]);
    let (d, path) = x.pulled("d", &key).await;
    let header = x.covered.operation().unwrap().header;
    let plaintext = crate::wire::decrypt(
        &[22; 32],
        &encode(&header.extensions).unwrap(),
        x.covered.body_bytes(),
    )
    .unwrap();
    let mut extension = header.extensions.clone();
    // A different authentic frame occupies the same scope-log position,
    // but its domain sequence is past the candidate's recorded frontier.
    extension.author_sequence = x
        .voidable
        .operation()
        .unwrap()
        .header
        .extensions
        .author_sequence
        + 1;
    let fork = crate::wire::author(
        &x.e_key,
        extension,
        x.covered.chain().unwrap().clone(),
        &[22; 32],
        (header.seq_num, header.backlink, header.timestamp),
        header.previous,
        &plaintext,
    )
    .unwrap();
    assert_eq!(d.receive(fork).await.unwrap(), ReceiveStatus::MissingKey);
    let before = pulled_stores(&d).await;
    assert_eq!(pulled_rows(&d).await, 1);
    let revision = d.stored_and_memory_revision().await;
    let operations = rows(
        &d.inner.lock().await.conn,
        "SELECT * FROM secure_operations ORDER BY hash",
    )
    .await
    .unwrap();
    // Intake drops the contradicted candidate, then the real store's fork
    // check refuses this request. Its savepoint must restore the drop.
    let error = d.receive(x.covered.clone()).await.unwrap_err();
    assert!(error.to_string().contains("author log fork"), "{error}");
    assert_eq!(pulled_stores(&d).await, before);
    assert_eq!(d.stored_and_memory_revision().await, revision);
    assert_eq!(
        rows(
            &d.inner.lock().await.conn,
            "SELECT * FROM secure_operations ORDER BY hash"
        )
        .await
        .unwrap(),
        operations
    );
    assert_eq!(pulled_rows(&d).await, 1);
    drop(d);
    let d = reopen(&x.f, &path, &key).await;
    assert_eq!(pulled_stores(&d).await, before);
}

#[tokio::test]
async fn an_installed_base_refuses_at_the_early_gate_without_dropping_a_candidate() {
    let x = contradicted().await;
    let key = PrivateKey::from_bytes(&[204; 32]);
    let (d, path) = x.pulled("d", &key).await;
    d.begin_rebootstrap(x.claim.clone()).await.unwrap();
    d.finish_rebootstrap(x.f.private).await.unwrap();
    let before = pulled_stores(&d).await;
    assert_eq!(pulled_rows(&d).await, 1);
    let revision = d.stored_and_memory_revision().await;
    let error = d.receive(x.covered.clone()).await.unwrap_err();
    assert!(error.to_string().contains("contradicts"), "{error}");
    assert_eq!(pulled_stores(&d).await, before);
    assert_eq!(d.stored_and_memory_revision().await, revision);
    assert_eq!(count(&d, "SELECT COUNT(*) FROM secure_operations").await, 0);
    drop(d);
    assert_eq!(
        pulled_stores(&reopen(&x.f, &path, &key).await).await,
        before
    );
}

#[tokio::test]
async fn a_schema_unavailable_at_install_drops_only_its_pulled_candidate() {
    use rocky_credentials::{
        SEAL_FORMAT, SealAction, SealStatement, SignedSeal, SignedTipOpening, TIP_OPENING_FORMAT,
        TipOpeningStatement,
    };
    let x = contradicted().await;
    let key = PrivateKey::from_bytes(&[205; 32]);
    let (d, path) = x.bystander("d", &key).await;
    let mut extra = x.f.app().definition();
    extra.schema += 1;
    let manifest = extra.hash();
    let opening = SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            schema: manifest,
            scope: x.f.public.domain,
            seal: x.sealed,
            minimum_in_sync: 1,
        },
        &x.f.publisher,
    )
    .unwrap();
    let tip = opening.tip().unwrap();
    let sealed = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: x.f.public.domain,
            tip,
            frontier: Vec::new(),
            action: SealAction::Close,
            recovery: None,
        },
        &x.f.publisher,
    )
    .unwrap();
    d.receive_tip_statements(&[
        crate::TipStatement::Opening(opening),
        crate::TipStatement::Seal(sealed.clone()),
    ])
    .await
    .unwrap();
    d.set_catalog_entry_for_test(manifest, Some(extra.clone()))
        .await;
    let credentials = x.a.inner.lock().await.config.credentials.clone();
    let claim = resigned(
        &x.empty,
        &x.a.inner.lock().await.config.node,
        &credentials,
        |s| {
            s.tip = tip;
            s.schema = extra.schema;
            s.seal = Some(sealed.id().unwrap());
            s.sequence += 1;
            s.previous = Some(x.empty.id().unwrap());
        },
    );
    d.begin_seal_pull(claim).await.unwrap();
    d.finish_seal_pull(x.f.private).await.unwrap();
    // A separate import at the root schema must survive this refusal.
    d.begin_rebootstrap(x.empty.clone()).await.unwrap();
    let staging = rows(
        &d.inner.lock().await.conn,
        "SELECT * FROM secure_rebootstrap ORDER BY scope",
    )
    .await
    .unwrap();
    let unchanged = "SELECT * FROM secure_scope_state ORDER BY scope";
    let state = rows(&d.inner.lock().await.conn, unchanged).await.unwrap();
    let bases = rows(
        &d.inner.lock().await.conn,
        "SELECT * FROM secure_checkpoints ORDER BY scope",
    )
    .await
    .unwrap();
    let guards = rows(
        &d.inner.lock().await.conn,
        "SELECT * FROM secure_evicted_logs ORDER BY scope",
    )
    .await
    .unwrap();
    // Lose only the extra tip's catalog entry; the live/root definition stays.
    d.set_catalog_entry_for_test(manifest, None).await.unwrap();
    assert_eq!(pulled_rows(&d).await, 1);
    assert!(!d.install_pulled_for_test(x.f.private).await.unwrap());
    assert_eq!(pulled_rows(&d).await, 0);
    assert_eq!(
        rows(
            &d.inner.lock().await.conn,
            "SELECT * FROM secure_rebootstrap ORDER BY scope"
        )
        .await
        .unwrap(),
        staging
    );
    assert_eq!(
        rows(&d.inner.lock().await.conn, unchanged).await.unwrap(),
        state
    );
    assert_eq!(
        rows(
            &d.inner.lock().await.conn,
            "SELECT * FROM secure_checkpoints ORDER BY scope"
        )
        .await
        .unwrap(),
        bases
    );
    assert_eq!(
        rows(
            &d.inner.lock().await.conn,
            "SELECT * FROM secure_evicted_logs ORDER BY scope"
        )
        .await
        .unwrap(),
        guards
    );
    drop(d);
    reopen(&x.f, &path, &key).await;
}

#[tokio::test]
async fn a_pulled_seal_off_the_path_stays_cached_and_installs_nothing() {
    use rocky_credentials::{
        BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SignedBookmarkMove, SignedTipOpening,
        TIP_OPENING_FORMAT, TipOpeningStatement,
    };
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let domain = f.public.domain;
    let (a, _) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    let path = dir.path().join("b.db");
    let (b, credentials) = open(&f, &path, &keys[1]).await;
    a.author_buffered(handle(1), insert(&f, 1, "one"), vec![])
        .await
        .unwrap();
    sync(&f, &a, (&b, &keys[1], &credentials)).await;
    let tip = a.followed_tip().await;
    seal_main(&a, &f.publisher, domain).await;
    let first = seal_on(&a, domain, tip).await;
    sync(&f, &a, (&b, &keys[1], &credentials)).await;
    // B's bases sit at the first seal, A's at the second.
    assert_eq!(b.compact(domain, 0).await.unwrap().compacted, Some(first));
    a.author_buffered(handle(2), insert(&f, 2, "two"), vec![])
        .await
        .unwrap();
    let tip = a.followed_tip().await;
    seal_main(&a, &f.publisher, domain).await;
    let second = seal_on(&a, domain, tip).await;
    assert_eq!(a.compact(domain, 0).await.unwrap().compacted, Some(second));
    sync(&f, &a, (&b, &keys[1], &credentials)).await;
    // B pulls A's states at the second seal while both follow main.
    a.admit_peer(keys[1].public_key(), &credentials, b.followed_tip().await)
        .await
        .unwrap();
    let pulled = b.seal_pull_scopes(domain, second).await.unwrap();
    assert_eq!(pulled.len(), 2);
    for scope in pulled {
        let offered = a
            .checkpoint_offer(keys[1].public_key(), &credentials, scope)
            .await
            .unwrap()
            .expect("A's base");
        assert_eq!(offered.statement.seal, Some(second));
        let checkpoint = offered.id().unwrap();
        b.begin_seal_pull(offered.clone()).await.unwrap();
        let mut after = None;
        while !offered.statement.no_frames() {
            let page = a
                .checkpoint_page(
                    keys[1].public_key(),
                    &credentials,
                    scope,
                    checkpoint,
                    after,
                    64 * 1024,
                )
                .await
                .unwrap();
            after = page.next;
            b.stage_checkpoint_page(page).await.unwrap();
            if after.is_none() {
                break;
            }
        }
        assert_eq!(b.finish_seal_pull(scope).await.unwrap(), second);
    }
    let cached = "SELECT quote(signed)||quote(sets)||quote(seal) FROM secure_pulled ORDER BY scope";
    let before = rows(&b.inner.lock().await.conn, cached).await.unwrap();
    assert_eq!(before.len(), 2);
    // A branch opens at the first seal; its path does not pass the second.
    let opening = SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            schema: b.tip_graph(domain).await.unwrap().genesis_schema,
            scope: domain,
            seal: first,
            minimum_in_sync: 1,
        },
        &f.publisher,
    )
    .unwrap();
    let bookmark = SignedBookmarkMove::sign(
        BookmarkMoveStatement {
            format: BOOKMARK_MOVE_FORMAT,
            scope: domain,
            name: "dev".into(),
            from: None,
            to: Some(opening.tip().unwrap()),
        },
        &f.publisher,
    )
    .unwrap();
    b.publish_tip_statements(&[
        crate::TipStatement::Opening(opening),
        crate::TipStatement::Bookmark(bookmark),
    ])
    .await
    .unwrap();
    b.follow_bookmark(domain, "dev").await.unwrap();
    b.compact(domain, 0).await.unwrap();
    b.finish_seal_rebootstrap(domain).await.unwrap();
    assert_eq!(
        rows(&b.inner.lock().await.conn, cached).await.unwrap(),
        before
    );
    for scope in [f.public, f.private] {
        assert_eq!(
            b.checkpoint(scope).await.unwrap().statement.seal,
            Some(first)
        );
    }
    assert_eq!(
        count(&b, "SELECT COUNT(*) FROM secure_evicted_logs").await,
        0
    );
    drop(b);
    let b = reopen(&f, &path, &keys[1]).await;
    assert_eq!(
        rows(&b.inner.lock().await.conn, cached).await.unwrap(),
        before
    );
}

#[tokio::test]
async fn a_revocation_that_moves_the_followed_bookmark_through_a_pulled_seal_drops_the_signers_state_first()
 {
    use rocky_credentials::{
        Authority, BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction,
        SealStatement, SignedBookmarkMove, SignedSeal,
    };
    let x = dev_pull(true).await;
    let (f, a, b) = (&x.f, &x.a, &x.b);
    let domain = f.public.domain;
    b.follow_bookmark(domain, "dev").await.unwrap();
    let opened = b.followed_tip().await;
    assert_eq!(x.pulled_scopes().await, 0);
    // An issuer seals the branch and moves `dev` on; B compacts there.
    let issuer = PrivateKey::new();
    let chain = CredentialChain(vec![
        Authority::owner(&f.publisher)
            .issue_issuer(issuer.public_key(), None, crate::tests::validity())
            .unwrap(),
    ]);
    let sealed = SignedSeal::endorse(
        SealStatement {
            format: SEAL_FORMAT,
            scope: domain,
            tip: opened,
            frontier: b.seal_frontier(domain, false).await.unwrap(),
            action: SealAction::Continue {
                schema: b.tip_graph(domain).await.unwrap().genesis_schema,
            },
            recovery: None,
        },
        &issuer,
        &chain,
    )
    .unwrap();
    let moved = SignedBookmarkMove::endorse(
        BookmarkMoveStatement {
            format: BOOKMARK_MOVE_FORMAT,
            scope: domain,
            name: "dev".into(),
            from: Some(opened),
            to: Some(sealed.successor().unwrap().unwrap()),
        },
        &issuer,
        &chain,
    )
    .unwrap();
    b.receive_tip_statements(&[
        crate::TipStatement::Seal(sealed.clone()),
        crate::TipStatement::Bookmark(moved),
    ])
    .await
    .unwrap();
    assert_ne!(b.followed_tip().await, opened);
    assert_eq!(
        b.compact(domain, 0).await.unwrap().compacted,
        Some(sealed.id().unwrap())
    );
    // B's private base moved to the issuer's seal; it pulls the first
    // seal's private state again, restated by a signer about to be revoked.
    a.receive_tip_statements(&crate::tests::all_tip_statements(b).await)
        .await
        .unwrap();
    a.follow_bookmark(domain, "dev").await.unwrap();
    let credentials = b.inner.lock().await.config.credentials.clone();
    a.admit_peer(x.b_key.public_key(), &credentials, b.followed_tip().await)
        .await
        .unwrap();
    let mut pulled = BTreeMap::new();
    for scope in b.seal_pull_scopes(domain, x.first).await.unwrap() {
        let offered = a
            .checkpoint_offer(x.b_key.public_key(), &credentials, scope)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(offered.statement.seal, Some(x.first));
        let offered = resigned(&offered, &x.restater, &x.signers[1], |_| {});
        b.begin_seal_pull(offered.clone()).await.unwrap();
        let mut after = None;
        while !offered.statement.no_frames() {
            let page = a
                .checkpoint_page(
                    x.b_key.public_key(),
                    &credentials,
                    scope,
                    offered.id().unwrap(),
                    after,
                    64 * 1024,
                )
                .await
                .unwrap();
            after = page.next;
            b.stage_checkpoint_page(page).await.unwrap();
            if after.is_none() {
                break;
            }
        }
        b.finish_seal_pull(scope).await.unwrap();
        pulled.insert(scope, offered);
    }
    assert_eq!(pulled.keys().collect::<Vec<_>>(), vec![&f.private]);
    // One set revokes the issuer and the restating signer: `dev` returns
    // to the branch's opening through the first seal, B's private base
    // leaves the path, and the revoked state is gone before that follow
    // could install it.
    let mut revoked = x.revoked(1);
    revoked
        .merge(
            &crate::tests::revoking(&f.publisher, domain.app, &[issuer.public_key()], &[]),
            f.publisher.public_key(),
            domain.app,
            crate::now().unwrap(),
        )
        .unwrap();
    b.learn_revocations(&revoked).await.unwrap();
    assert_eq!(b.followed_tip().await, opened);
    assert_eq!(b.revocations().await, revoked);
    assert_ne!(
        b.checkpoint(f.private)
            .await
            .ok()
            .map(|signed| signed.id().unwrap()),
        Some(pulled[&f.private].id().unwrap())
    );
    assert_eq!(x.pulled_scopes().await, 0);
    assert!(
        b.seal_rebootstraps(domain)
            .await
            .unwrap()
            .iter()
            .any(|need| need.scope == f.private)
    );
    drop(x.b);
    reopen(f, &x.b_path, &x.b_key).await;
}
