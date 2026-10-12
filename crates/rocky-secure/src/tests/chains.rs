//! Credential chains by reference: the frame header's digest, the session's
//! chain table, stored chains and the chains beside a checkpoint page
//! (`docs/decisions/0053-chains-by-reference.md`).
use super::commands::command_grants;
use super::*;
use crate::network::NativeSession;
use crate::replica::checkpoint::Stage;
use crate::wire::{Chain, INCOMPLETE_FRAME};

type Replica = EncryptedReplica<Employees>;

/// `base` and grants for other meshes, `count` in all: of the same shape,
/// or `narrow` ones that only read.
fn widened(
    f: &Fixture,
    base: Vec<ResourceGrant>,
    count: usize,
    narrow: bool,
) -> Vec<ResourceGrant> {
    let mut grants = base.clone();
    for mesh in 0..count.saturating_sub(base.len()) {
        let mut grant = base[0].clone();
        grant.scope = f
            .app()
            .scope(MeshId([(50 + base.len() + mesh) as u8; 16]), false);
        if narrow {
            grant.rights = [Right::Read].into();
            grant.fields.clear();
        }
        grants.push(grant);
    }
    assert_eq!(grants.len(), count);
    grants
}
async fn open(f: &Fixture, dir: &TempDir, node: &PrivateKey, grants: usize) -> (Replica, Chain) {
    let config = f.config(node, widened(f, f.full(), grants, false), &[f.public]);
    let chain = Chain::new(config.credentials.clone()).unwrap();
    let path = dir.path().join(format!("{}.db", node.public_key()));
    (
        EncryptedReplica::open(path, f.app(), config).await.unwrap(),
        chain,
    )
}
async fn insert(replica: &Replica, f: &Fixture, row: u16) -> EncryptedFrame {
    let mut id = [3; 32];
    id[..2].copy_from_slice(&row.to_be_bytes());
    replica
        .author(
            id,
            vec![RowChange {
                scope: f.public,
                row: id,
                before: None,
                after: Some(f.app().name("row")),
            }],
            vec![],
        )
        .await
        .unwrap()
}
async fn count(replica: &Replica, sql: &str) -> usize {
    let inner = replica.inner.lock().await;
    crate::storage::rows(&inner.conn, sql).await.unwrap().len()
}
/// `client` dials `server` with the chains the replicas were opened with.
async fn sessions(
    (server_key, server, server_chain): (&PrivateKey, &Replica, &Chain),
    (client_key, client, client_chain): (&PrivateKey, &Replica, &Chain),
) -> (NativeSession, NativeSession) {
    let (dialed, accepted) = crate::network::link::testing::sessions(
        (
            client,
            client_key.clone(),
            client_chain.credentials().clone(),
        ),
        (
            server,
            server_key.clone(),
            server_chain.credentials().clone(),
        ),
    )
    .await
    .unwrap();
    (accepted, dialed)
}

#[tokio::test]
async fn a_session_carries_an_authors_chain_once_across_its_pages() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let (a, a_chain) = open(&f, &dir, &alice, 20).await;
    let (b, b_chain) = open(&f, &dir, &bob, 2).await;
    let mut weight = 0;
    for row in 0..40 {
        let frame = insert(&a, &f, row).await;
        assert_eq!(frame.chain_digest().unwrap(), a_chain.digest());
        weight = frame.weight();
    }
    let chain = encode(&a_chain).unwrap().len();
    let quota = MAX_SECURE_FRAME / 2;
    let (mut server, mut client) = sessions((&alice, &a, &a_chain), (&bob, &b, &b_chain)).await;
    let (served, report) = tokio::join!(server.serve_to_end(&a), async {
        let report = client.catch_up(&b, f.public, quota).await.unwrap();
        let carried = client.carried_chain_bytes();
        client.close().await.unwrap();
        (report, carried)
    });
    served.unwrap();
    let (report, carried) = report;
    // Two pages of frames, and the one chain they name crossed once.
    assert_eq!((report.frames, report.incomplete, report.lag), (40, 0, 0));
    assert_eq!(report.pages, 2);
    assert_eq!(carried, chain);
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 40);
    // Stored once, too, beside the 40 operations that name it.
    assert_eq!(b.chain_census().await, ([a_chain.digest()].into(), 0));
    // With the chain inside every header a page held few frames.
    let before = (quota / (weight + chain)).min(32);
    let after = (quota / weight).min(32);
    assert!(before < 8 && after == 32, "{before} then {after}");
    println!(
        "P11 frames_per_page grants=20 quota={quota} frame={weight} chain={chain} before={before} after={after} frame_bytes={} chain_bytes={carried}",
        report.bytes
    );
}

#[tokio::test]
async fn a_frame_whose_chain_the_receiver_lacks_is_refused_and_arrives_on_the_next_pass() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let (a, a_chain) = open(&f, &dir, &alice, 2).await;
    let (b, b_chain) = open(&f, &dir, &bob, 2).await;
    let mut frame = insert(&a, &f, 1).await;
    // Outside a session a frame without its chain is refused whole.
    frame.take_chain();
    let error = b.receive(frame.clone()).await.unwrap_err();
    assert!(error.to_string().contains(INCOMPLETE_FRAME), "{error}");
    // So is one beside a chain its header does not name.
    let error = b.receive(frame.with_chain(b_chain.clone())).await;
    assert!(error.unwrap_err().to_string().contains(INCOMPLETE_FRAME));
    // A peer that claims to have carried the chain sends the frame bare.
    let (mut server, mut client) = sessions((&alice, &a, &a_chain), (&bob, &b, &b_chain)).await;
    server.withhold_chains();
    let (served, report) = tokio::join!(server.serve_to_end(&a), async {
        let report = client.catch_up(&b, f.public, MAX_SECURE_FRAME / 2).await;
        client.close().await.unwrap();
        report.unwrap()
    });
    served.unwrap();
    assert_eq!((report.frames, report.incomplete), (1, 1));
    assert_eq!(report.lag, 1);
    // Neither admitted nor stored, and nothing of it is kept.
    assert_eq!(count(&b, "SELECT hash FROM secure_operations").await, 0);
    assert_eq!(b.chain_census().await, (BTreeSet::new(), 0));
    assert!(b.snapshot(f.public).await.unwrap().rows.is_empty());
    // The next pass, on its own session, carries the chain and the frame.
    let (mut server, mut client) = sessions((&alice, &a, &a_chain), (&bob, &b, &b_chain)).await;
    let (served, report) = tokio::join!(server.serve_to_end(&a), async {
        let report = client.catch_up(&b, f.public, MAX_SECURE_FRAME / 2).await;
        client.close().await.unwrap();
        report.unwrap()
    });
    served.unwrap();
    assert_eq!((report.frames, report.incomplete, report.lag), (1, 0, 0));
    assert_eq!(b.snapshot(f.public).await.unwrap().rows.len(), 1);
}

#[tokio::test]
async fn a_pushed_frame_without_its_chain_is_not_received() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let (a, a_chain) = open(&f, &dir, &alice, 2).await;
    let (b, b_chain) = open(&f, &dir, &bob, 2).await;
    insert(&a, &f, 1).await;
    for withhold in [true, false] {
        let (mut server, mut client) = sessions((&bob, &b, &b_chain), (&alice, &a, &a_chain)).await;
        if withhold {
            client.withhold_chains();
        }
        let (served, pushed) = tokio::join!(server.serve_to_end(&b), async {
            let pushed = client.push(&a, f.public, MAX_SECURE_FRAME / 2).await;
            client.close().await.unwrap();
            pushed.unwrap()
        });
        served.unwrap();
        assert_eq!(pushed.frames, 1);
        let held = usize::from(!withhold);
        assert_eq!(count(&b, "SELECT hash FROM secure_operations").await, held);
        // The chain itself is stored either way: it is the one Alice
        // authenticated with, which her horizon credential names. Holding
        // its bytes does not complete a frame the session never carried.
        assert_eq!(b.chain_census().await, ([a_chain.digest()].into(), 0));
    }
}

#[tokio::test]
async fn a_chain_revoked_during_a_session_admits_no_later_frame_of_it() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob, carol) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let (a, _) = open(&f, &dir, &alice, 2).await;
    let (b, b_chain) = open(&f, &dir, &bob, 2).await;
    let (c, c_chain) = open(&f, &dir, &carol, 2).await;
    b.receive(insert(&a, &f, 1).await).await.unwrap();
    let (mut server, mut client) = sessions((&bob, &b, &b_chain), (&carol, &c, &c_chain)).await;
    let quota = MAX_SECURE_FRAME / 2;
    let (served, ()) = tokio::join!(server.serve_to_end(&b), async {
        client.pull(&c, vec![f.public], None, quota).await.unwrap();
        let carried = client.carried_chain_bytes();
        assert!(carried > 0);
        assert_eq!(c.snapshot(f.public).await.unwrap().rows.len(), 1);
        // Alice writes again; Carol then learns that Alice is revoked.
        b.receive(insert(&a, &f, 2).await).await.unwrap();
        let revoked = revoking(
            &f.publisher,
            f.app().definition().app,
            &[alice.public_key()],
            &[],
        );
        c.learn_revocations(&revoked).await.unwrap();
        // The session already carried her chain and does not send it again;
        // the frame that names it is verified, and refused, all the same.
        assert!(client.pull(&c, vec![f.public], None, quota).await.is_err());
        assert_eq!(client.carried_chain_bytes(), carried);
        client.close().await.unwrap();
    });
    served.unwrap();
    assert_eq!(c.snapshot(f.public).await.unwrap().rows.len(), 1);
    assert_eq!(count(&c, "SELECT hash FROM secure_operations").await, 1);
}

#[tokio::test]
async fn a_chain_goes_with_the_last_operation_that_names_it() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let (a, a_chain) = open(&f, &dir, &alice, 2).await;
    let (b, b_chain) = open(&f, &dir, &bob, 2).await;
    // Bob holds 12 operations of Alice and three of his own; a pruning
    // batch is eight operations in tests.
    for row in 0..12 {
        let frame = insert(&a, &f, row).await;
        let id = frame.id().unwrap();
        b.receive(frame).await.unwrap();
        b.acknowledge(alice.public_key(), a_chain.credentials(), id, true)
            .await
            .unwrap();
    }
    for row in 100..103 {
        insert(&b, &f, row).await;
    }
    let both: BTreeSet<_> = [a_chain.digest(), b_chain.digest()].into();
    assert_eq!(b.chain_census().await, (both.clone(), 0));
    b.set_retention_policy(
        f.public,
        RetentionPolicy {
            required: [rocky_credentials::PeerId::of(alice.public_key())].into(),
            copies: 1,
            window: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    b.checkpoint(f.public).await.unwrap();
    // Interrupted after the first batch: Alice's remaining operations keep
    // her chain, and no operation is left without its chain.
    b.inject_stage_fault(Stage::Prune, 1).await;
    assert!(b.prune(f.public).await.is_err());
    assert_eq!(count(&b, "SELECT hash FROM secure_operations").await, 7);
    assert_eq!(b.chain_census().await, (both, 0));
    // Alice never acknowledged Bob's own operations, so those stay; hers
    // are all pruned, and her chain goes in the batch that takes the last.
    let report = b.prune(f.public).await.unwrap();
    assert_eq!(report.pruned, 4);
    assert!(matches!(
        report.blocked,
        Some(PruneBlock::Unacknowledged { .. })
    ));
    assert_eq!(count(&b, "SELECT hash FROM secure_operations").await, 3);
    assert_eq!(b.chain_census().await, ([b_chain.digest()].into(), 0));
    assert_eq!(b.audit(f.public).await.unwrap().operations, 3);
}

#[tokio::test]
async fn a_horizon_credential_keeps_its_chain_until_a_renewal_replaces_it() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let (a, a_chain) = open(&f, &dir, &alice, 2).await;
    let (_b, b_chain) = open(&f, &dir, &bob, 2).await;
    // A peer's credential names its chain; a renewal replaces it.
    a.record_horizon_credentials(bob.public_key(), b_chain.credentials())
        .await
        .unwrap();
    assert_eq!(a.chain_census().await.0, [b_chain.digest()].into());
    assert_eq!(
        encode(
            &a.cached_peer_credentials(&[bob.public_key()])
                .await
                .unwrap()[0]
                .1
        )
        .unwrap(),
        encode(&b_chain).unwrap()
    );
    let renewed = Chain::new(f.credentials(&bob, f.full())).unwrap();
    a.record_horizon_credentials(bob.public_key(), renewed.credentials())
        .await
        .unwrap();
    assert_eq!(a.chain_census().await.0, [renewed.digest()].into());
    insert(&a, &f, 1).await;
    assert_eq!(
        a.chain_census().await,
        ([renewed.digest(), a_chain.digest()].into(), 0)
    );
}

/// A command table whose latest command was called and executed under
/// chains of `grants` scope grants. Forty grants of the fixture's full shape
/// exceed the command envelope itself, so beyond 20 the others only read.
struct Commanded {
    f: Fixture,
    _dir: TempDir,
    source: Replica,
    target: Replica,
    target_key: PrivateKey,
    target_chain: Chain,
    source_key: PrivateKey,
    source_chain: Chain,
}
async fn commanded(grants: usize) -> Commanded {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (source_key, target_key) = (PrivateKey::new(), PrivateKey::new());
    let scopes = [f.public, f.private];
    let wide = |f: &Fixture| {
        let twenty = widened(f, command_grants(f), grants.min(20), false);
        widened(f, twenty, grants, true)
    };
    let ac = f.config(&source_key, wide(&f), &scopes);
    let bc = f.config(&target_key, wide(&f), &scopes);
    let (source_chain, target_chain) = (
        Chain::new(ac.credentials.clone()).unwrap(),
        Chain::new(bc.credentials.clone()).unwrap(),
    );
    let coordinator = rocky_coordinator::MemoryCoordinator::default();
    super::effects::register(&coordinator, &ac, f.public, &[&bc]);
    let source = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let target = EncryptedReplica::open(dir.path().join("b.db"), f.app(), bc)
        .await
        .unwrap();
    let command = source
        .invoke_encrypted(
            [7; 32],
            f.private,
            "SetEmployee",
            b"employee-1\nAlice\n731994",
        )
        .await
        .unwrap();
    source
        .execute_encrypted(command, &coordinator)
        .await
        .unwrap();
    Commanded {
        f,
        _dir: dir,
        source,
        target,
        target_key,
        target_chain,
        source_key,
        source_chain,
    }
}

#[tokio::test]
async fn an_installed_checkpoint_is_not_imported_again() {
    let c = commanded(20).await;
    let (mut server, mut client) = sessions(
        (&c.source_key, &c.source, &c.source_chain),
        (&c.target_key, &c.target, &c.target_chain),
    )
    .await;
    let (served, revisions) = tokio::join!(server.serve_to_end(&c.source), async {
        let mut revisions = Vec::new();
        for _ in 0..2 {
            let sync = client
                .bootstrap_scope(&c.target, c.f.private, MAX_SECURE_FRAME / 2)
                .await
                .unwrap()
                .unwrap();
            revisions.push((
                sync.rebootstrap.is_some(),
                *c.target.observe_revision().borrow(),
            ));
        }
        client.close().await.unwrap();
        revisions
    });
    served.unwrap();
    assert!(revisions[0].0 && !revisions[1].0);
    assert_eq!(revisions[0].1, revisions[1].1);
    // The checkpoint brought the strong base, so its head is known.
    let domain = c.f.private.domain;
    assert_eq!(c.target.known_result_head(domain).await.unwrap(), 1);
    assert_eq!(
        c.target.result_scopes(domain).await.unwrap(),
        BTreeSet::from([c.f.public, c.f.private])
    );
}

#[tokio::test]
async fn a_command_table_bootstraps_by_checkpoint_under_20_and_40_grant_chains() {
    for grants in [20, 40] {
        let c = commanded(grants).await;
        let (mut server, mut client) = sessions(
            (&c.source_key, &c.source, &c.source_chain),
            (&c.target_key, &c.target, &c.target_chain),
        )
        .await;
        let (served, ()) = tokio::join!(server.serve_to_end(&c.source), async {
            for scope in [c.f.public, c.f.private] {
                let sync = client
                    .bootstrap_scope(&c.target, scope, MAX_SECURE_FRAME / 2)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(sync.rebootstrap.is_some() && !sync.catch_up.exhausted);
            }
            client.close().await.unwrap();
        });
        served.unwrap();
        for scope in [c.f.public, c.f.private] {
            let (theirs, ours) = (
                c.source.command_snapshot(scope).await.unwrap(),
                c.target.command_snapshot(scope).await.unwrap(),
            );
            assert!(ours.complete);
            assert_eq!(ours.rows, theirs.rows);
        }
        assert!(
            !c.target
                .command_snapshot(c.f.private)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        // What the first page spent on the strong state: with both chains
        // embedded, of a budget of 63 KiB at two bytes a byte, and now.
        let (before, after) = c.source.strong_page_bytes(c.f.private).await;
        let chain = encode(&c.source_chain).unwrap().len();
        assert!(before > 2 * chain && after < before - 2 * chain);
        assert!(after < 16 * 1024, "{after}");
        println!(
            "P11 strong_state grants={grants} chain={chain} before={before} after={after} budget={}",
            rocky_core::MAX_PAYLOAD_BYTES - 1024
        );
    }
}

#[tokio::test]
async fn a_chain_beside_the_page_that_differs_from_its_digest_stages_nothing() {
    let c = commanded(20).await;
    let scope = c.f.private;
    let signed = c.source.checkpoint(scope).await.unwrap();
    c.target.begin_rebootstrap(signed.clone()).await.unwrap();
    let page = c
        .source
        .checkpoint_page(
            c.target_key.public_key(),
            c.target_chain.credentials(),
            scope,
            signed.id().unwrap(),
            None,
            MAX_SECURE_FRAME - 8192,
        )
        .await
        .unwrap();
    // The caller and the executor are one device here: one chain.
    assert_eq!(page.chains.len(), 1);
    let staged = async || {
        (
            c.target.rebootstrap_progress(scope).await.unwrap().unwrap(),
            count(&c.target, "SELECT scope FROM secure_rebootstrap_strong").await,
            count(&c.target, "SELECT scope FROM secure_rebootstrap_rows").await,
        )
    };
    let untouched = ((signed.id().unwrap(), None, false), 0, 0);
    let other = c.f.credentials(&c.source_key, command_grants(&c.f));
    for chains in [
        vec![other.clone()],
        vec![],
        vec![page.chains[0].clone(), other],
    ] {
        let mut altered = page.clone();
        altered.chains = chains;
        let error = c.target.stage_checkpoint_page(altered).await.unwrap_err();
        assert!(
            error.to_string().contains(crate::command::CHAIN_MISMATCH),
            "{error}"
        );
        assert_eq!(staged().await, untouched);
    }
    // The page with the chain its strong state names is staged.
    let next = page.next;
    c.target.stage_checkpoint_page(page).await.unwrap();
    assert_eq!(staged().await.1, 1);
    assert_eq!(
        c.target
            .rebootstrap_progress(scope)
            .await
            .unwrap()
            .unwrap()
            .1,
        next
    );
}
