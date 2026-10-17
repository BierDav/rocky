//! A destination-only issuer covers a carry before an earlier own birth.
use super::*;
use rocky_core::RowApplication;
use rocky_credentials::AuthorMark;

struct BeforeBirth {
    _root: tempfile::TempDir,
    f: Fixture,
    issuer: EncryptedReplica<TypedRows>,
    listed: EncryptedReplica<TypedRows>,
    fresh: EncryptedReplica<TypedRows>,
    birth: EncryptedFrame,
    carry: EncryptedFrame,
    dst: &'static str,
    successor: rocky_core::TipId,
    signed: crate::SignedCheckpoint,
}

async fn floor(
    replica: &EncryptedReplica<TypedRows>,
    table: &str,
    scope: ResourceScope,
) -> Option<i64> {
    super::carry_order::mark(replica, table, scope).await
}

async fn destination(f: &Fixture, path: &Path, seed: u8, dst: &str) -> EncryptedReplica<TypedRows> {
    let replica = f
        .open(
            path,
            PrivateKey::from_bytes(&[seed; 32]),
            f.grant(Some(&[dst]), &all()),
            None,
        )
        .await;
    f.key(&replica, dst, if dst == "A" { [1; 32] } else { [2; 32] })
        .await;
    replica
}

async fn setup() -> BeforeBirth {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::build_with(false, PrivateKey::from_bytes(&[233; 32]));
    let (dst, src) = if f.scope("A") < f.scope("B") {
        ("A", "B")
    } else {
        ("B", "A")
    };
    let writer = keyed(&f, &root.path().join("writer"), [234; 32], 1).await;
    let mover = keyed(&f, &root.path().join("mover"), [235; 32], 2).await;
    let raw = vec![f.write(src, 1, 10), f.write(dst, 2, 20)];
    assert_eq!(
        raw.iter()
            .map(|write| f.app().row_identity(&write.change).unwrap().unwrap())
            .collect::<Vec<_>>(),
        vec![
            RowIdentity::new(1, 1).unwrap(),
            RowIdentity::new(1, 2).unwrap()
        ]
    );
    assert_eq!(
        [raw[0].change.scope, raw[1].change.scope],
        [f.scope(src), f.scope(dst)]
    );
    let frames = writer.author_bundle([1; 32], raw, vec![]).await.unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].scope().unwrap(), f.scope(dst));
    assert_eq!(frames[1].scope().unwrap(), f.scope(src));
    for (sequence, frame) in frames.iter().enumerate() {
        let op = frame.operation().unwrap();
        assert_eq!(op.header.extensions.author, Some(1));
        assert_eq!(op.header.extensions.author_sequence, sequence as u64);
    }
    assert_eq!(
        mover.receive(frames[0].clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    assert_eq!(
        mover.receive(frames[1].clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    let origin = writer
        .author_bundle([2; 32], vec![f.write(src, 3, 30)], vec![])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        origin
            .operation()
            .unwrap()
            .header
            .extensions
            .author_sequence,
        2
    );
    assert_eq!(mover.receive(origin).await.unwrap(), ReceiveStatus::Applied);
    let moved = mover
        .session([f.scope(src), f.scope(dst)].into())
        .await
        .unwrap()
        .move_row(
            [3; 32],
            "items",
            RowIdentity::new(1, 3).unwrap(),
            &[SqlValue::Text(src.into())],
            &[SqlValue::Text(dst.into())],
        )
        .await
        .unwrap();
    assert_eq!(moved.len(), 2);
    assert_eq!(moved[0].scope().unwrap(), f.scope(dst));
    for (sequence, frame) in moved.iter().enumerate() {
        let op = frame.operation().unwrap();
        assert_eq!(op.header.extensions.author, Some(2));
        assert_eq!(op.header.extensions.author_sequence, sequence as u64);
    }
    let issuer = destination(&f, &root.path().join("issuer"), 236, dst).await;
    let listed = destination(&f, &root.path().join("listed"), 237, dst).await;
    let fresh = destination(&f, &root.path().join("fresh"), 238, dst).await;
    // These nodes receive no source member or author-a source frames.
    assert_eq!(
        issuer.receive(moved[0].clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(
        listed.receive(frames[0].clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_eq!(
        listed.receive(moved[0].clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    for replica in [&issuer, &listed, &fresh] {
        let inner = replica.inner.lock().await;
        assert!(
            inner
                .config
                .keys
                .iter()
                .all(|key| key.scope == f.scope(dst))
        );
        assert_eq!(
            inner.config.credentials.0[0]
                .capability
                .extended()
                .unwrap()
                .partitions,
            vec![f.grant(Some(&[dst]), &all())]
        );
    }
    assert_eq!(
        floor(&issuer, "secure_identity_marks", f.scope(dst)).await,
        Some(3)
    );
    assert_eq!(
        floor(&issuer, "secure_identity_own", f.scope(dst)).await,
        None
    );
    assert_eq!(
        floor(&issuer, "secure_identity_ledger", f.scope(dst)).await,
        None
    );
    let tip = issuer.followed_tip().await;
    assert_eq!(
        issuer.seal_frontier(f.domain, false).await.unwrap(),
        vec![AuthorMark {
            author: 2,
            sequence: 0
        }]
    );
    let signed = crate::tests::compacted_base(&issuer, &f.publisher, f.scope(dst)).await;
    let graph = issuer.tip_graph(f.domain).await.unwrap();
    assert_eq!(
        graph.seal(tip).unwrap().unwrap().statement.frontier,
        vec![AuthorMark {
            author: 2,
            sequence: 0
        }]
    );
    assert_eq!(signed.statement.identities.len(), 1);
    let covered = &signed.statement.identities[0];
    assert_eq!((covered.author, covered.sequence, covered.own), (1, 3, 0));
    assert_eq!(
        covered.signer,
        PrivateKey::from_bytes(&[234; 32]).public_key()
    );
    assert_eq!(signed.statement.moves.count, 1);
    issuer.audit(f.scope(dst)).await.unwrap();
    let successor = issuer.followed_tip().await;
    assert_ne!(successor, tip);
    BeforeBirth {
        _root: root,
        f,
        issuer,
        listed,
        fresh,
        birth: frames[0].clone(),
        carry: moved[0].clone(),
        dst,
        successor,
        signed,
    }
}

async fn import(f: &BeforeBirth, target: &EncryptedReplica<TypedRows>) -> crate::RebootstrapReport {
    crate::tests::exchange_graphs(&f.issuer, target, f.f.domain).await;
    assert_eq!(
        target.followed_bookmark().await,
        Some((f.f.domain, "main".into()))
    );
    assert_eq!(target.followed_tip().await, f.successor);
    let (key, credentials) = {
        let inner = target.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    f.issuer
        .admit_peer(key, &credentials, target.followed_tip().await)
        .await
        .unwrap();
    let scope = f.f.scope(f.dst);
    let offered = f
        .issuer
        .checkpoint_offer(key, &credentials, scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offered.to_bytes().unwrap(), f.signed.to_bytes().unwrap());
    target.begin_rebootstrap(offered.clone()).await.unwrap();
    let mut after = None;
    loop {
        let page = f
            .issuer
            .checkpoint_page(
                key,
                &credentials,
                scope,
                offered.id().unwrap(),
                after,
                64 * 1024,
            )
            .await
            .unwrap();
        after = page.next;
        target.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
    target.finish_rebootstrap(scope).await.unwrap()
}

async fn assert_keys(f: &Fixture, dst: &str, replica: &EncryptedReplica<TypedRows>) {
    let scope = f.scope(dst);
    let rows = replica
        .session([scope].into())
        .await
        .unwrap()
        .sql(Default::default())
        .await
        .unwrap()
        .run(
            "SELECT key FROM items WHERE list=? ORDER BY key",
            vec![SqlValue::Text(dst.into())],
        )
        .await
        .unwrap()
        .rows;
    assert_eq!(
        rows,
        vec![
            vec![SqlValue::Text("key2".into())],
            vec![SqlValue::Text("key3".into())]
        ]
    );
}

async fn assert_own_rows(f: &Fixture, dst: &str, replica: &EncryptedReplica<TypedRows>) {
    assert_keys(f, dst, replica).await;
    let scope = f.scope(dst);
    assert_eq!(floor(replica, "secure_identity_own", scope).await, Some(2));
    assert_eq!(
        floor(replica, "secure_identity_ledger", scope).await,
        Some(2)
    );
    assert_eq!(floor(replica, "secure_identity_summary", scope).await, None);
    replica.audit(scope).await.unwrap();
}

#[tokio::test]
async fn an_own_zero_carry_checkpoint_allows_a_late_birth_on_its_continuation() {
    let f = setup().await;
    import(&f, &f.fresh).await;
    let scope = f.f.scope(f.dst);
    for table in [
        "secure_identity_own",
        "secure_identity_summary",
        "secure_identity_ledger",
    ] {
        assert_eq!(floor(&f.fresh, table, scope).await, None, "{table}");
    }
    assert_eq!(
        floor(&f.fresh, "secure_identity_marks", scope).await,
        Some(3)
    );
    assert_eq!(
        f.fresh.receive(f.birth.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_own_rows(&f.f, f.dst, &f.fresh).await;
    assert_eq!(
        count(
            &f.fresh,
            "SELECT 1 FROM secure_admission_entries WHERE kind=1"
        )
        .await,
        0
    );
    // The original issuer follows T1 too; its next honest seal includes a0.
    assert_eq!(f.issuer.followed_tip().await, f.successor);
    assert_eq!(
        f.issuer.receive(f.birth.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert_own_rows(&f.f, f.dst, &f.issuer).await;
    assert_eq!(
        f.issuer.seal_frontier(f.f.domain, false).await.unwrap(),
        vec![
            AuthorMark {
                author: 1,
                sequence: 0
            },
            AuthorMark {
                author: 2,
                sequence: 0
            }
        ]
    );
    let next = crate::tests::compacted_base(&f.issuer, &f.f.publisher, scope).await;
    assert_eq!(f.signed.statement.rows.count, 1);
    assert_eq!(
        (next.statement.rows.count, next.statement.tombstones),
        (2, 0)
    );
    assert_eq!(
        (
            next.statement.identities[0].sequence,
            next.statement.identities[0].own
        ),
        (3, 2)
    );
    assert_keys(&f.f, f.dst, &f.issuer).await;
    f.issuer.audit(scope).await.unwrap();
    f.issuer.rebuild_projection(scope).await.unwrap();
    assert_keys(&f.f, f.dst, &f.issuer).await;
    f.issuer.audit(scope).await.unwrap();
    let BeforeBirth {
        _root,
        f,
        issuer,
        fresh,
        dst,
        ..
    } = f;
    for (name, replica) in [("issuer", issuer), ("fresh", fresh)] {
        let config = config(&replica).await;
        drop(replica);
        let reopened = EncryptedReplica::open(_root.path().join(name), f.app(), config)
            .await
            .unwrap();
        reopened.audit(scope).await.unwrap();
        assert_keys(&f, dst, &reopened).await;
        assert_eq!(
            floor(&reopened, "secure_identity_own", scope).await,
            Some(2)
        );
        assert!(
            reopened
                .inner
                .lock()
                .await
                .config
                .keys
                .iter()
                .all(|key| key.scope == f.scope(dst))
        );
    }
}

#[tokio::test]
async fn an_own_zero_carry_checkpoint_reapplies_the_exact_retained_birth() {
    let f = setup().await;
    let report = import(&f, &f.listed).await;
    assert!(
        report.reapplied.contains(&f.birth.id().unwrap()),
        "{report:?}"
    );
    assert!(
        !report.reapplied.contains(&f.carry.id().unwrap()),
        "{report:?}"
    );
    assert_own_rows(&f.f, f.dst, &f.listed).await;
    assert_eq!(
        count(
            &f.listed,
            "SELECT 1 FROM secure_admission_entries WHERE kind=0"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &f.listed,
            "SELECT 1 FROM secure_admission_entries WHERE kind=1"
        )
        .await,
        0
    );
    f.listed.rebuild_projection(f.f.scope(f.dst)).await.unwrap();
    assert_own_rows(&f.f, f.dst, &f.listed).await;
    let BeforeBirth {
        _root,
        f,
        listed,
        dst,
        ..
    } = f;
    let config = config(&listed).await;
    drop(listed);
    let listed = EncryptedReplica::open(_root.path().join("listed"), f.app(), config)
        .await
        .unwrap();
    assert_own_rows(&f, dst, &listed).await;
}

async fn config(replica: &EncryptedReplica<TypedRows>) -> SecureConfig {
    let inner = replica.inner.lock().await;
    let c = &inner.config;
    SecureConfig {
        publisher: c.publisher,
        manifest: c.manifest.clone(),
        revocations: c.revocations.clone(),
        node: c.node.clone(),
        credentials: c.credentials.clone(),
        keys: c.keys.clone(),
    }
}
