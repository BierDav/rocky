//! Open-time graph exclusion changes L without retiring permanent evidence.
use super::*;
use rocky_credentials::Authority;

#[tokio::test]
async fn an_issuer_excluded_at_open_reseeds_the_workspace_ledger() {
    let lineage = Lineage::pinned(243);
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::from_bytes(&[244; 32]);
    let issuer = PrivateKey::from_bytes(&[245; 32]);
    let replica = open(&lineage, &dir, "node", &key).await;
    follow_main(&lineage, &replica).await;
    let genesis = replica.followed_tip().await;
    insert(&replica, "root", "inherited").await;
    let chain = CredentialChain(vec![
        Authority::owner(lineage.publisher())
            .issue_issuer(issuer.public_key(), None, crate::tests::validity())
            .unwrap(),
    ]);
    let statement = sign_seal(
        &lineage,
        genesis,
        replica.seal_frontier(lineage.domain, false).await.unwrap(),
    )
    .statement;
    let sealed = SignedSeal::endorse(statement, &issuer, &chain).unwrap();
    let successor = sealed.successor().unwrap().unwrap();
    let TipStatement::Bookmark(moved) = bookmark(&lineage, Some(genesis), successor) else {
        unreachable!()
    };
    let moved = SignedBookmarkMove::endorse(moved.statement, &issuer, &chain).unwrap();
    replica
        .receive_tip_statements(&[
            TipStatement::Seal(sealed.clone()),
            TipStatement::Bookmark(moved),
        ])
        .await
        .unwrap();
    assert_eq!(replica.followed_tip().await, successor);
    insert(&replica, "branch", "excluded later").await;
    let author = crate::tests::node_number(key.public_key());
    let ledger = format!("SELECT sequence FROM secure_identity_ledger WHERE author={author}");
    assert_eq!(
        crate::storage::rows(&replica.inner.lock().await.conn, &ledger)
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(2)]]
    );
    let evidence = "SELECT 'P',scope,author,sequence FROM secure_identity_marks UNION ALL SELECT 'O',scope,author,sequence FROM secure_identity_own UNION ALL SELECT 'R',entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT 'C',identity,entry,provenance FROM secure_admission_claims";
    let permanent = crate::storage::rows(&replica.inner.lock().await.conn, evidence)
        .await
        .unwrap();
    let mut config = lineage.config(3, &key);
    config.revocations = crate::tests::revoking(
        lineage.publisher(),
        lineage.domain.app,
        &[issuer.public_key()],
        &[],
    );
    let before = crate::storage::rows(
        &replica.inner.lock().await.conn,
        "SELECT revocations,revision FROM secure_metadata",
    )
    .await
    .unwrap();
    let atomic_query = "SELECT 'm',revocations,revision,identity FROM secure_metadata UNION ALL SELECT 'h',context,head,head_signature FROM secure_admission_context UNION ALL SELECT 'r',scope,row,fields FROM secure_rows UNION ALL SELECT 'L',scope,author,sequence FROM secure_identity_ledger UNION ALL SELECT 'f',hash,status,sequence FROM secure_operations UNION ALL SELECT 't',scope,bookmark,tip FROM secure_followed_tip UNION ALL SELECT 'P',scope,author,sequence FROM secure_identity_marks UNION ALL SELECT 'O',scope,author,sequence FROM secure_identity_own UNION ALL SELECT 'S',scope,author,sequence FROM secure_identity_summary UNION ALL SELECT 'R',entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT 'C',identity,entry,provenance FROM secure_admission_claims UNION ALL SELECT 'pending',scope,tip,manifest FROM secure_pending_schema UNION ALL SELECT 'pending-control',id,bookmark,moved FROM secure_pending_schema UNION ALL SELECT 'pull',scope,seal,length(signed) FROM secure_pulled UNION ALL SELECT 'tip',digest,0,0 FROM secure_tip_statements";
    let before_atomic = crate::storage::rows(&replica.inner.lock().await.conn, atomic_query)
        .await
        .unwrap();
    replica.inner.lock().await.conn.execute("CREATE TRIGGER refuse_ledger_reseed BEFORE DELETE ON secure_identity_ledger BEGIN SELECT RAISE(ABORT,'reseeding refused'); END",()).await.unwrap();
    drop(replica);
    let failed = EncryptedReplica::open(dir.path().join("node.db"), lineage.app(3), config).await;
    let error = match failed {
        Ok(_) => panic!("the reseed fault must run"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("reseeding refused"), "{error}");
    let (owner, conn) = crate::database::DatabaseOwner::open(&dir.path().join("node.db"))
        .await
        .unwrap();
    assert_eq!(
        crate::storage::collect_rows(conn.query(atomic_query, ()).await.unwrap())
            .await
            .unwrap(),
        before_atomic
    );
    assert_eq!(
        crate::storage::collect_rows(
            conn.query("SELECT revocations,revision FROM secure_metadata", ())
                .await
                .unwrap()
        )
        .await
        .unwrap(),
        before
    );
    drop(conn);
    drop(owner);
    // With the trigger still installed, a plain reopen verifies the old state.
    let plain = lineage
        .reopen(&dir.path().join("node.db"), &key, 3, 3)
        .await
        .unwrap();
    assert_eq!(plain.followed_tip().await, successor);
    assert_eq!(
        crate::storage::rows(&plain.inner.lock().await.conn, &ledger)
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(2)]]
    );
    assert_eq!(
        crate::storage::rows(&plain.inner.lock().await.conn, atomic_query)
            .await
            .unwrap(),
        before_atomic
    );
    plain.audit(lineage.scope("items")).await.unwrap();
    drop(plain);
    let (owner, conn) = crate::database::DatabaseOwner::open(&dir.path().join("node.db"))
        .await
        .unwrap();
    conn.execute("UPDATE secure_identity_ledger SET sequence=1", ())
        .await
        .unwrap();
    drop(conn);
    drop(owner);
    let mut tampered = lineage.config(3, &key);
    tampered.revocations = configured_revocations(&lineage, &issuer);
    let failed = EncryptedReplica::open(dir.path().join("node.db"), lineage.app(3), tampered).await;
    let error = match failed {
        Ok(_) => panic!("supplied exclusions never repair old L"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("identity ledger or summary differs from its history"),
        "{error}"
    );
    let (owner, conn) = crate::database::DatabaseOwner::open(&dir.path().join("node.db"))
        .await
        .unwrap();
    assert_eq!(
        crate::storage::collect_rows(
            conn.query("SELECT revocations,revision FROM secure_metadata", ())
                .await
                .unwrap()
        )
        .await
        .unwrap(),
        before
    );
    conn.execute("UPDATE secure_identity_ledger SET sequence=2", ())
        .await
        .unwrap();
    conn.execute("DROP TRIGGER refuse_ledger_reseed", ())
        .await
        .unwrap();
    drop(conn);
    drop(owner);
    let mut revoked_local = lineage.config(3, &key);
    revoked_local.revocations = crate::tests::revoking(
        lineage.publisher(),
        lineage.domain.app,
        &[key.public_key()],
        &[],
    );
    let failed =
        EncryptedReplica::open(dir.path().join("node.db"), lineage.app(3), revoked_local).await;
    let error = match failed {
        Ok(_) => panic!("a revoked local node cannot open"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("revoked"), "{error}");
    let (owner, conn) = crate::database::DatabaseOwner::open(&dir.path().join("node.db"))
        .await
        .unwrap();
    assert_eq!(
        crate::storage::collect_rows(conn.query(atomic_query, ()).await.unwrap())
            .await
            .unwrap(),
        before_atomic
    );
    assert_eq!(
        crate::storage::collect_rows(
            conn.query("SELECT revocations,revision FROM secure_metadata", ())
                .await
                .unwrap()
        )
        .await
        .unwrap(),
        before
    );
    drop(conn);
    drop(owner);
    let mut config = lineage.config(3, &key);
    config.revocations = crate::tests::revoking(
        lineage.publisher(),
        lineage.domain.app,
        &[issuer.public_key()],
        &[],
    );
    let supplied = config.revocations.clone();
    // The only new input is the config revocation, not learn_revocations.
    let replica = EncryptedReplica::open(dir.path().join("node.db"), lineage.app(3), config)
        .await
        .unwrap();
    let graph = replica.tip_graph(lineage.domain).await.unwrap();
    assert!(graph.revoked.contains(&sealed.id().unwrap()));
    assert_eq!(replica.followed_tip().await, genesis);
    assert_eq!(
        crate::storage::rows(&replica.inner.lock().await.conn, &ledger)
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(1)]]
    );
    assert_eq!(
        crate::storage::rows(&replica.inner.lock().await.conn, evidence)
            .await
            .unwrap(),
        permanent
    );
    assert_eq!(
        titles(&replica).await,
        vec![vec![text("root"), text("inherited")]]
    );
    replica.audit(lineage.scope("items")).await.unwrap();
    let stable = "SELECT revocations,revision,head,head_signature FROM secure_metadata,secure_admission_context";
    let committed = crate::storage::rows(&replica.inner.lock().await.conn, stable)
        .await
        .unwrap();
    drop(replica);
    let mut same = lineage.config(3, &key);
    same.revocations = supplied.clone();
    let replica = EncryptedReplica::open(dir.path().join("node.db"), lineage.app(3), same)
        .await
        .unwrap();
    assert_eq!(replica.followed_tip().await, genesis);
    assert_eq!(
        crate::storage::rows(&replica.inner.lock().await.conn, stable)
            .await
            .unwrap(),
        committed
    );
    replica.audit(lineage.scope("items")).await.unwrap();
    drop(replica);
    let replica = lineage
        .reopen(&dir.path().join("node.db"), &key, 3, 3)
        .await
        .unwrap();
    replica.audit(lineage.scope("items")).await.unwrap();
    assert_eq!(
        crate::storage::rows(&replica.inner.lock().await.conn, &ledger)
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(1)]]
    );
    assert_eq!(replica.revocations().await, supplied);
    assert_eq!(replica.followed_tip().await, genesis);
    assert_eq!(
        crate::storage::rows(&replica.inner.lock().await.conn, stable)
            .await
            .unwrap(),
        committed
    );
    // A new, validly signed issuer attempt remains excluded after reopen.
    let mut attempted = sealed.statement.clone();
    attempted.frontier.clear();
    let attempted = SignedSeal::endorse(attempted, &issuer, &chain).unwrap();
    assert_ne!(attempted.id().unwrap(), sealed.id().unwrap());
    let next = attempted.successor().unwrap().unwrap();
    let TipStatement::Bookmark(moved) = bookmark(&lineage, Some(genesis), next) else {
        unreachable!()
    };
    let moved = SignedBookmarkMove::endorse(moved.statement, &issuer, &chain).unwrap();
    let error = replica
        .receive_tip_statements(&[
            TipStatement::Seal(attempted.clone()),
            TipStatement::Bookmark(moved),
        ])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("invalid or revoked credential"),
        "{error}"
    );
    let graph = replica.tip_graph(lineage.domain).await.unwrap();
    assert!(graph.revoked.contains(&sealed.id().unwrap()));
    assert!(graph.seal_by_id(attempted.id().unwrap()).is_err());
    assert_eq!(graph.bookmark("main").unwrap(), Some(genesis));
    assert_eq!(replica.followed_tip().await, genesis);
    replica.audit(lineage.scope("items")).await.unwrap();
}

fn configured_revocations(lineage: &Lineage, issuer: &PrivateKey) -> RevocationSet {
    crate::tests::revoking(
        lineage.publisher(),
        lineage.domain.app,
        &[issuer.public_key()],
        &[],
    )
}

#[tokio::test]
async fn an_open_revocation_loader_retry_keeps_supplied_statements_past_expired_stored_ones() {
    let lineage = Lineage::pinned(253);
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::from_bytes(&[254; 32]);
    let issuer = PrivateKey::from_bytes(&[255; 32]);
    let replica = open(&lineage, &dir, "node", &key).await;
    follow_main(&lineage, &replica).await;
    let expired = rocky_credentials::Revocation {
        app: lineage.domain.app,
        issuer: lineage.publisher().public_key(),
        time: 0,
        nodes: BTreeSet::new(),
        identities: [PrivateKey::from_bytes(&[252; 32]).public_key()].into(),
        capabilities: BTreeSet::new(),
    };
    assert!(!expired.live(now().unwrap()));
    let expired = RevocationSet::try_from(vec![
        rocky_credentials::SignedRevocation::sign(
            expired,
            lineage.publisher(),
            &CredentialChain::default(),
        )
        .unwrap(),
    ])
    .unwrap();
    replica
        .inner
        .lock()
        .await
        .conn
        .execute(
            "UPDATE secure_metadata SET revocations=?",
            [expired.to_bytes().unwrap()],
        )
        .await
        .unwrap();
    let before_revision =
        crate::tests::compaction::scalar(&replica, "SELECT revision FROM secure_metadata").await;
    let supplied = configured_revocations(&lineage, &issuer);
    replica.inject_failure(1, 0).await;
    // The actual E1 loader runs twice through transaction!'s Busy retry.
    let pending = replica
        .retry_open_revocations_for_test(supplied.clone())
        .await
        .unwrap();
    assert_eq!(pending, supplied);
    assert_eq!(
        crate::tests::compaction::scalar(&replica, "SELECT revision FROM secure_metadata").await,
        before_revision
    );
    replica.learn_revocations(&pending).await.unwrap();
    assert_eq!(replica.revocations().await, supplied);
    let tip = replica.followed_tip().await;
    let chain = CredentialChain(vec![
        Authority::owner(lineage.publisher())
            .issue_issuer(issuer.public_key(), None, crate::tests::validity())
            .unwrap(),
    ]);
    let sealed =
        SignedSeal::endorse(sign_seal(&lineage, tip, vec![]).statement, &issuer, &chain).unwrap();
    let error = replica
        .receive_tip_statements(&[TipStatement::Seal(sealed.clone())])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("invalid or revoked credential"),
        "{error}"
    );
    assert!(
        replica
            .tip_graph(lineage.domain)
            .await
            .unwrap()
            .seal(tip)
            .unwrap()
            .is_none()
    );
    assert_eq!(replica.followed_tip().await, tip);
    replica.audit(lineage.scope("items")).await.unwrap();
}
