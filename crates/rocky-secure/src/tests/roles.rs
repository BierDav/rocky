//! Full and partial replicas: subscriptions, range sync, eviction, custody,
//! remote reads and interrupted transfers over the native transport.
use super::*;
use network::NativeSession;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

type Replica = EncryptedReplica<Employees>;
const QUOTA: usize = MAX_SECURE_FRAME / 2;

/// Department scopes of one domain: same table, separate reader sets.
fn department(f: &Fixture, index: u8) -> ResourceScope {
    ResourceScope {
        encryption: rocky_core::commitment("roles.department.v1", &[index]),
        ..f.public
    }
}
fn departments(f: &Fixture, count: u8) -> Vec<ResourceScope> {
    (0..count).map(|index| department(f, index)).collect()
}
fn key_of(scope: &ResourceScope) -> [u8; 32] {
    rocky_core::commitment("roles.key.v1", &scope.encryption)
}
struct Node {
    key: PrivateKey,
    credentials: CredentialChain,
    replica: Replica,
}
async fn open_node(
    f: &Fixture,
    path: &Path,
    key: &PrivateKey,
    granted: &[ResourceScope],
    keyed: &[ResourceScope],
) -> Node {
    let grants = granted
        .iter()
        .map(|scope| {
            grant(
                *scope,
                &[
                    Right::Read,
                    Right::Insert,
                    Right::Update,
                    Right::Delete,
                    Right::Custody,
                ],
                &[f.app().field("display_name")],
            )
        })
        .collect();
    let credentials = f.credentials(key, grants);
    let config = SecureConfig {
        publisher: f.publisher.public_key(),
        manifest: f.manifest.clone(),
        revocations: f.policy.clone(),
        node: key.clone(),
        credentials: credentials.clone(),
        keys: keyed
            .iter()
            .map(|scope| KeyMaterial::new(*scope, 1, key_of(scope)))
            .collect(),
    };
    Node {
        key: key.clone(),
        credentials,
        replica: EncryptedReplica::open(path, f.app(), config).await.unwrap(),
    }
}
fn row(index: u32) -> [u8; 32] {
    let mut row = [5; 32];
    row[..4].copy_from_slice(&index.to_be_bytes());
    row
}
fn handle(node: &Node, index: u32) -> [u8; 32] {
    let mut bytes = node.key.public_key().as_bytes().to_vec();
    bytes.extend(index.to_le_bytes());
    rocky_core::commitment("roles.handle.v1", &bytes)
}
fn insert_handle(node: &Node, scope: ResourceScope, start: u32) -> [u8; 32] {
    let mut bytes = scope.encryption.to_vec();
    bytes.extend(start.to_le_bytes());
    bytes.extend(node.key.public_key().as_bytes());
    rocky_core::commitment("roles.insert.v1", &bytes)
}
/// Inserts rows `range` into `scope`, `per` rows per operation.
async fn insert(
    f: &Fixture,
    node: &Node,
    scope: ResourceScope,
    range: std::ops::Range<u32>,
    per: u32,
) {
    let mut start = range.start;
    while start < range.end {
        let guard = node
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        let mut pending = tokio::task::JoinSet::new();
        for _ in 0..64 {
            if start == range.end {
                break;
            }
            let end = (start + per).min(range.end);
            let changes = (start..end)
                .map(|index| RowChange {
                    scope,
                    row: row(index),
                    before: None,
                    after: Some(f.app().name(&format!("row {index}"))),
                })
                .collect();
            let handle = insert_handle(node, scope, start);
            let replica = node.replica.clone();
            let mut request =
                Box::pin(async move { replica.author_buffered(handle, changes, vec![]).await });
            // Preserve log order while enqueuing a shared transaction.
            std::future::poll_fn(|cx| {
                assert!(request.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            pending.spawn(request);
            start = end;
        }
        drop(guard);
        while let Some(result) = pending.join_next().await {
            result.unwrap().unwrap();
        }
    }
}
async fn rename(f: &Fixture, node: &Node, scope: ResourceScope, index: u32, from: &str, to: &str) {
    node.replica
        .author(
            handle(node, 900_000 + index),
            vec![RowChange {
                scope,
                row: row(index),
                before: Some(f.app().name(from)),
                after: Some(f.app().name(to)),
            }],
            vec![],
        )
        .await
        .unwrap();
}
async fn all_rows(replica: &Replica, scope: ResourceScope) -> BTreeMap<Commitment, RowFields> {
    let mut rows = BTreeMap::new();
    let mut cursor = None;
    loop {
        let page = replica
            .snapshot_page(scope, cursor, MAX_SNAPSHOT_ROWS)
            .await
            .unwrap();
        rows.extend(page.rows);
        cursor = page.next;
        if cursor.is_none() {
            return rows;
        }
    }
}
async fn stored_rows(replica: &Replica, scope: ResourceScope) -> i64 {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    let mut result = inner
        .conn
        .query(
            "SELECT COUNT(*) FROM secure_row_records WHERE scope=?",
            [encode(&scope).unwrap()],
        )
        .await
        .unwrap();
    result.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}
async fn connect(node: &Node, server: &Node) -> (NativeSession, tokio::task::JoinHandle<()>) {
    connect_with(node, server, MAX_SECURE_BYTES).await
}
/// A client session with its own byte budget; `server` answers it until the
/// client ends the channel.
async fn connect_with(
    node: &Node,
    server: &Node,
    budget: usize,
) -> (NativeSession, tokio::task::JoinHandle<()>) {
    // As a link's tip exchange does before rows move.
    let domain = *server
        .replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .graphs
        .keys()
        .next()
        .unwrap();
    exchange_graphs(&node.replica, &server.replica, domain).await;
    let (near, far) = network::link::testing::tcp().await;
    let (session, mut served) = network::link::testing::sessions_over(
        near,
        far,
        (&node.replica, node.key.clone(), node.credentials.clone()),
        (
            &server.replica,
            server.key.clone(),
            server.credentials.clone(),
        ),
    )
    .await
    .unwrap();
    let replica = server.replica.clone();
    let task = tokio::spawn(async move {
        loop {
            if let Err(error) = served.serve_once(&replica).await {
                // A disconnect ends the session; anything else is printed for the failing test.
                if !matches!(error, Error::Io(_)) {
                    eprintln!("server: {error}");
                }
                return;
            }
        }
    });
    (session.with_budget(budget), task)
}
fn partial(full: &[&Node], custody: usize) -> RoleConfig {
    RoleConfig {
        role: ReplicaRole::Partial,
        full_nodes: full
            .iter()
            .map(|node| rocky_credentials::PeerId::of(node.key.public_key()))
            .collect(),
        custody,
    }
}
fn keep_one_copy() -> RetentionPolicy {
    RetentionPolicy {
        required: Default::default(),
        behind: Default::default(),
        copies: 1,
        window: 0,
    }
}

#[tokio::test]
async fn range_sync_pages_by_frontier_and_rebootstraps_behind_a_pruned_gap() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let scope = department(&f, 0);
    let full = open_node(
        &f,
        &dir.path().join("full.db"),
        &PrivateKey::new(),
        &[scope],
        &[scope],
    )
    .await;
    let device = open_node(
        &f,
        &dir.path().join("device.db"),
        &PrivateKey::new(),
        &[scope],
        &[scope],
    )
    .await;
    insert(&f, &full, scope, 0..400, 4).await;

    let (mut session, server) = connect(&device, &full).await;
    let pass = session
        .catch_up(&device.replica, scope, QUOTA)
        .await
        .unwrap();
    assert!(!pass.behind);
    assert_eq!((pass.frames, pass.lag), (100, 0));
    // At most 32 frames per page, and nothing is sent twice in the next pass.
    assert!(pass.pages >= 4);
    assert_eq!(session.sent_counts.ranges, pass.pages);
    assert_eq!(session.sent_counts.acknowledgments, 1);
    let again = session
        .catch_up(&device.replica, scope, QUOTA)
        .await
        .unwrap();
    assert_eq!((again.frames, again.pages), (0, 1));
    assert_eq!(
        all_rows(&device.replica, scope).await,
        all_rows(&full.replica, scope).await
    );
    assert_eq!(
        device.replica.frontier(scope).await.unwrap(),
        full.replica.frontier(scope).await.unwrap()
    );

    // The device's frontier acknowledgment lets the full node prune.
    compacted_base(&full.replica, &f.publisher, scope).await;
    full.replica
        .set_retention_policy(scope, keep_one_copy())
        .await
        .unwrap();
    let report = full.replica.prune(scope).await.unwrap();
    assert_eq!((report.pruned, report.blocked), (100, None));
    drop(session);
    server.await.unwrap();

    // A fresh replica is behind the pruned gap and rebootstraps first.
    let late = open_node(
        &f,
        &dir.path().join("late.db"),
        &PrivateKey::new(),
        &[scope],
        &[scope],
    )
    .await;
    insert(&f, &full, scope, 400..420, 4).await;
    let (mut session, server) = connect(&late, &full).await;
    assert!(
        session
            .catch_up(&late.replica, scope, QUOTA)
            .await
            .unwrap()
            .behind
    );
    assert!(late.replica.frontier(scope).await.unwrap().logs.is_empty());
    let synced = session
        .sync_scope(&late.replica, scope, QUOTA)
        .await
        .unwrap();
    assert!(synced.rebootstrap.is_some());
    assert_eq!(synced.catch_up.frames, 5);
    assert_eq!(
        all_rows(&late.replica, scope).await,
        all_rows(&full.replica, scope).await
    );
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn a_partial_replica_holds_only_its_subscription() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let scopes = departments(&f, 20);
    let full = open_node(
        &f,
        &dir.path().join("full.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let device = open_node(
        &f,
        &dir.path().join("device.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    device.replica.set_role(partial(&[&full], 1)).await.unwrap();
    device.replica.subscribe(scopes[3]).await.unwrap();
    for scope in &scopes {
        insert(&f, &full, *scope, 0..20, 5).await;
    }
    let (mut session, server) = connect(&device, &full).await;
    let (offered, next) = session.peer_scopes(None).await.unwrap();
    assert_eq!((offered.len(), next), (20, None));
    let held: Vec<_> = device
        .replica
        .subscriptions()
        .await
        .unwrap()
        .into_iter()
        .filter(|(_, holding)| *holding == Holding::Held)
        .map(|(scope, _)| scope)
        .collect();
    assert_eq!(held, vec![scopes[3]]);
    for scope in &held {
        session
            .sync_scope(&device.replica, *scope, QUOTA)
            .await
            .unwrap();
    }
    assert_eq!(all_rows(&device.replica, scopes[3]).await.len(), 20);
    // Frames of another scope are refused, not stored.
    let refused = session.catch_up(&device.replica, scopes[4], QUOTA).await;
    assert!(refused.is_err());
    drop(session);
    server.await.unwrap();
    for scope in scopes.iter().filter(|scope| **scope != scopes[3]) {
        assert_eq!(stored_rows(&device.replica, *scope).await, 0);
        let page = device
            .replica
            .snapshot_page(*scope, None, 16)
            .await
            .unwrap();
        assert!(!page.complete && page.rows.is_empty());
    }
    // A full replica refuses to evict; a partial one cannot author outside its subscription.
    assert!(full.replica.evict(scopes[0]).await.is_err());
    let outside = device
        .replica
        .author(
            [1; 32],
            vec![RowChange {
                scope: scopes[5],
                row: row(1),
                before: None,
                after: Some(f.app().name("x")),
            }],
            vec![],
        )
        .await;
    assert!(outside.is_err());
    // Leaving keeps the rows but stops calling them complete.
    device.replica.unsubscribe(scopes[3]).await.unwrap();
    let page = device
        .replica
        .snapshot_page(scopes[3], None, 64)
        .await
        .unwrap();
    assert_eq!((page.rows.len(), page.complete), (20, false));
}

#[tokio::test]
async fn eviction_needs_applied_custody_and_a_resubscribed_scope_converges() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let scopes = departments(&f, 2);
    let full = open_node(
        &f,
        &dir.path().join("full.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let custodian = open_node(
        &f,
        &dir.path().join("custodian.db"),
        &PrivateKey::new(),
        &scopes,
        &[],
    )
    .await;
    let device = open_node(
        &f,
        &dir.path().join("device.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let other = open_node(
        &f,
        &dir.path().join("other.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let scope = scopes[0];
    for node in [&device, &other] {
        node.replica.set_role(partial(&[&full], 1)).await.unwrap();
        node.replica.subscribe(scope).await.unwrap();
    }
    insert(&f, &device, scope, 0..30, 3).await;
    rename(&f, &device, scope, 0, "row 0", "renamed").await;

    // No acknowledgment, then a custody-only receipt: both are refused.
    assert!(device.replica.evict(scope).await.is_err());
    let (mut session, server) = connect(&device, &custodian).await;
    assert_eq!(
        session
            .push(&device.replica, scope, QUOTA)
            .await
            .unwrap()
            .frames,
        11
    );
    drop(session);
    server.await.unwrap();
    device
        .replica
        .set_role(RoleConfig {
            full_nodes: [
                rocky_credentials::PeerId::of(full.key.public_key()),
                rocky_credentials::PeerId::of(custodian.key.public_key()),
            ]
            .into(),
            ..partial(&[], 0)
        })
        .await
        .unwrap();
    assert!(device.replica.evict(scope).await.is_err());

    // The full node applies the pushed frames; its frontier is custody.
    let (mut session, server) = connect(&device, &full).await;
    assert_eq!(
        session
            .push(&device.replica, scope, QUOTA)
            .await
            .unwrap()
            .frames,
        11
    );
    drop(session);
    server.await.unwrap();
    let expected = all_rows(&full.replica, scope).await;
    assert_eq!(all_rows(&device.replica, scope).await, expected);
    let status = device.replica.replica_status().await.unwrap();
    let entry = status
        .scopes
        .iter()
        .find(|entry| entry.scope == scope)
        .unwrap();
    assert_eq!(entry.local_operations, 11);
    assert!(
        entry
            .custody
            .contains(&(rocky_credentials::PeerId::of(full.key.public_key()), 11))
    );
    assert!(
        entry
            .custody
            .contains(&(rocky_credentials::PeerId::of(custodian.key.public_key()), 0))
    );

    let report = device.replica.evict(scope).await.unwrap();
    assert_eq!(report.operations, 11);
    assert_eq!(stored_rows(&device.replica, scope).await, 0);
    assert_eq!(
        device.replica.companion(scope, row(1)).await.unwrap(),
        Companion::Unavailable
    );
    assert!(device.replica.evict(scope).await.is_err());
    let audit = device.replica.audit(scope).await;
    assert!(audit.is_ok(), "{audit:?}");

    // Meanwhile another replica edits, and the full node prunes.
    let (mut session, server) = connect(&other, &full).await;
    session
        .sync_scope(&other.replica, scope, QUOTA)
        .await
        .unwrap();
    rename(&f, &other, scope, 1, "row 1", "elsewhere").await;
    assert_eq!(
        session
            .push(&other.replica, scope, QUOTA)
            .await
            .unwrap()
            .frames,
        1
    );
    drop(session);
    server.await.unwrap();
    compacted_base(&full.replica, &f.publisher, scope).await;
    full.replica
        .set_retention_policy(scope, keep_one_copy())
        .await
        .unwrap();
    assert!(full.replica.prune(scope).await.unwrap().pruned > 0);

    // Resubscribed: authoring waits for the own log, then everything converges.
    device.replica.subscribe(scope).await.unwrap();
    let early = device
        .replica
        .author(
            handle(&device, 77),
            vec![RowChange {
                scope,
                row: row(500),
                before: None,
                after: Some(f.app().name("early")),
            }],
            vec![],
        )
        .await;
    assert!(early.is_err());
    let (mut session, server) = connect(&device, &full).await;
    let synced = session
        .sync_scope(&device.replica, scope, QUOTA)
        .await
        .unwrap();
    assert!(synced.rebootstrap.is_some());
    assert_eq!(
        all_rows(&device.replica, scope).await,
        all_rows(&full.replica, scope).await
    );
    rename(&f, &device, scope, 2, "row 2", "after").await;
    assert_eq!(
        session
            .push(&device.replica, scope, QUOTA)
            .await
            .unwrap()
            .frames,
        1
    );
    drop(session);
    server.await.unwrap();
    let rows = all_rows(&full.replica, scope).await;
    assert_eq!(all_rows(&device.replica, scope).await, rows);
    assert_eq!(rows[&row(0)], f.app().name("renamed"));
    assert_eq!(rows[&row(1)], f.app().name("elsewhere"));
    assert_eq!(rows[&row(2)], f.app().name("after"));
    // The resumed log continues the evicted one instead of forking it.
    assert_eq!(
        device
            .replica
            .frontier(scope)
            .await
            .unwrap()
            .log(device.key.public_key())
            .unwrap()
            .applied,
        12
    );
}

#[tokio::test]
async fn a_resubscribed_scope_without_own_writes_reads_incomplete_until_restored() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let scopes = departments(&f, 1);
    let scope = scopes[0];
    let full = open_node(
        &f,
        &dir.path().join("full.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let device = open_node(
        &f,
        &dir.path().join("device.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    insert(&f, &full, scope, 0..10, 2).await;
    device.replica.set_role(partial(&[&full], 1)).await.unwrap();
    device.replica.subscribe(scope).await.unwrap();
    let restoring = || async {
        device
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .restoring(scope)
            .await
            .unwrap()
    };
    let sync = || async {
        let (mut session, server) = connect(&device, &full).await;
        session
            .sync_scope(&device.replica, scope, QUOTA)
            .await
            .unwrap();
        drop(session);
        server.await.unwrap();
    };
    sync().await;
    let expected = all_rows(&full.replica, scope).await;
    assert_eq!(all_rows(&device.replica, scope).await, expected);

    // No own log to guard, yet the resubscribed scope is not complete.
    device.replica.evict(scope).await.unwrap();
    device.replica.subscribe(scope).await.unwrap();
    assert!(restoring().await);
    sync().await;
    assert!(!restoring().await);
    assert_eq!(all_rows(&device.replica, scope).await, expected);
}

#[tokio::test]
async fn pruning_counts_full_nodes_only_once_they_applied() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let scope = department(&f, 0);
    let full = open_node(
        &f,
        &dir.path().join("full.db"),
        &PrivateKey::new(),
        &[scope],
        &[scope],
    )
    .await;
    let custodian = open_node(
        &f,
        &dir.path().join("custodian.db"),
        &PrivateKey::new(),
        &[scope],
        &[],
    )
    .await;
    let device = open_node(
        &f,
        &dir.path().join("device.db"),
        &PrivateKey::new(),
        &[scope],
        &[scope],
    )
    .await;
    insert(&f, &device, scope, 0..40, 4).await;
    device
        .replica
        .set_role(RoleConfig {
            role: ReplicaRole::Partial,
            full_nodes: [
                rocky_credentials::PeerId::of(full.key.public_key()),
                rocky_credentials::PeerId::of(custodian.key.public_key()),
            ]
            .into(),
            custody: 1,
        })
        .await
        .unwrap();
    device.replica.subscribe(scope).await.unwrap();
    device
        .replica
        .set_retention_policy(scope, keep_one_copy())
        .await
        .unwrap();
    compacted_base(&device.replica, &f.publisher, scope).await;
    // A custody-only full node holds every frame but cannot apply them.
    let (mut session, server) = connect(&device, &custodian).await;
    session.push(&device.replica, scope, QUOTA).await.unwrap();
    drop(session);
    server.await.unwrap();
    let blocked = device.replica.prune(scope).await.unwrap();
    assert_eq!(blocked.pruned, 0);
    assert!(matches!(
        blocked.blocked,
        Some(PruneBlock::Custody {
            applied: 0,
            needed: 1,
            ..
        })
    ));
    let (mut session, server) = connect(&device, &full).await;
    session.push(&device.replica, scope, QUOTA).await.unwrap();
    drop(session);
    server.await.unwrap();
    let report = device.replica.prune(scope).await.unwrap();
    assert_eq!((report.pruned, report.blocked), (10, None));
}

#[tokio::test]
async fn a_remote_read_states_its_source_and_is_never_local_state() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let scopes = departments(&f, 2);
    let full = open_node(
        &f,
        &dir.path().join("full.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let device = open_node(
        &f,
        &dir.path().join("device.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let stranger = open_node(
        &f,
        &dir.path().join("stranger.db"),
        &PrivateKey::new(),
        &scopes[..1],
        &scopes[..1],
    )
    .await;
    device.replica.set_role(partial(&[&full], 1)).await.unwrap();
    device.replica.subscribe(scopes[0]).await.unwrap();
    insert(&f, &full, scopes[1], 0..50, 10).await;
    let revision = *device.replica.observe_revision().borrow();

    let (mut session, server) = connect(&device, &full).await;
    let mut rows = BTreeMap::new();
    let mut cursor = None;
    loop {
        let page = session
            .read_remote(&device.replica, scopes[1], cursor, 16)
            .await
            .unwrap();
        assert_eq!(
            (page.source, page.scope),
            (full.key.public_key(), scopes[1])
        );
        assert!(page.complete);
        assert_eq!(page.frontier.iter().map(|log| log.applied).sum::<u64>(), 5);
        rows.extend(page.rows);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    drop(session);
    server.await.unwrap();
    assert_eq!(rows, all_rows(&full.replica, scopes[1]).await);
    assert_eq!(stored_rows(&device.replica, scopes[1]).await, 0);
    assert_eq!(*device.replica.observe_revision().borrow(), revision);
    assert!(
        !device
            .replica
            .snapshot_page(scopes[1], None, 16)
            .await
            .unwrap()
            .complete
    );

    // Without read authority over the scope nothing is served.
    let (mut session, server) = connect(&stranger, &full).await;
    assert!(
        session
            .read_remote(&stranger.replica, scopes[1], None, 16)
            .await
            .is_err()
    );
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn interrupted_transfers_resume_and_a_replacement_full_node_converges() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let scopes = departments(&f, 3);
    let full_key = PrivateKey::new();
    let full_path = dir.path().join("full.db");
    let full = open_node(&f, &full_path, &full_key, &scopes, &scopes).await;
    let device = open_node(
        &f,
        &dir.path().join("device.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    device.replica.set_role(partial(&[&full], 1)).await.unwrap();
    device.replica.subscribe(scopes[0]).await.unwrap();
    device.replica.subscribe(scopes[1]).await.unwrap();
    for scope in &scopes {
        insert(&f, &full, *scope, 0..200, 2).await;
    }
    compacted_base(&full.replica, &f.publisher, scopes[1]).await;
    full.replica
        .set_retention_policy(scopes[1], keep_one_copy())
        .await
        .unwrap();
    // Another reader acknowledged everything, so the full node prunes scope 1.
    let reader = open_node(
        &f,
        &dir.path().join("reader.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let (mut session, server) = connect(&reader, &full).await;
    session
        .catch_up(&reader.replica, scopes[1], QUOTA)
        .await
        .unwrap();
    drop(session);
    server.await.unwrap();
    assert_eq!(full.replica.prune(scopes[1]).await.unwrap().pruned, 100);

    // The full node dies after one range page.
    let (mut session, server) = connect(&device, &full).await;
    let frontier = device.replica.frontier(scopes[0]).await.unwrap();
    let page = full
        .replica
        .range_page(
            device.key.public_key(),
            &device.credentials,
            &frontier,
            None,
            QUOTA,
        )
        .await
        .unwrap();
    device.replica.receive_all(page.frames).await;
    server.abort();
    let _ = server.await;
    assert!(
        session
            .catch_up(&device.replica, scopes[0], QUOTA)
            .await
            .is_err()
    );
    drop(session);
    // ... and midway through serving a checkpoint.
    let (session, server) = connect(&device, &full).await;
    let signed = full
        .replica
        .checkpoint_offer(device.key.public_key(), &device.credentials, scopes[1])
        .await
        .unwrap()
        .unwrap();
    device
        .replica
        .begin_rebootstrap(signed.clone())
        .await
        .unwrap();
    let rows = full
        .replica
        .checkpoint_page(
            device.key.public_key(),
            &device.credentials,
            scopes[1],
            signed.id().unwrap(),
            None,
            QUOTA,
        )
        .await
        .unwrap();
    device.replica.stage_checkpoint_page(rows).await.unwrap();
    server.abort();
    let _ = server.await;
    drop(session);
    let (_, cursor, complete) = device
        .replica
        .rebootstrap_progress(scopes[1])
        .await
        .unwrap()
        .unwrap();
    assert!(cursor.is_some() && !complete);
    for scope in &scopes[..2] {
        assert!(device.replica.audit(*scope).await.is_ok());
    }
    let full = full.replica;
    drop(full);

    // The full node restarts from its file; both transfers resume.
    let full = open_node(&f, &full_path, &full_key, &scopes, &scopes).await;
    assert!(full.replica.audit(scopes[0]).await.is_ok());
    let (mut session, server) = connect(&device, &full).await;
    for scope in &scopes[..2] {
        session
            .sync_scope(&device.replica, *scope, QUOTA)
            .await
            .unwrap();
    }
    drop(session);
    server.await.unwrap();
    // Scope 0 came as frames while scope 1's positions were pruned: its
    // state at the seal is the full node's verified base.
    let imported = crate::tests::import_needed_bases(
        &full.replica,
        &device.replica,
        &device.key,
        &device.credentials,
        scopes[0].domain,
    )
    .await;
    assert_eq!(
        imported
            .iter()
            .map(|signed| signed.statement.scope)
            .collect::<Vec<_>>(),
        vec![scopes[0]]
    );
    assert_eq!(
        imported[0].id().unwrap(),
        full.replica
            .checkpoint(scopes[0])
            .await
            .unwrap()
            .id()
            .unwrap()
    );
    for scope in &scopes[..2] {
        assert_eq!(
            all_rows(&device.replica, *scope).await,
            all_rows(&full.replica, *scope).await
        );
    }

    // A replacement full node starts from the existing one.
    let replacement = open_node(
        &f,
        &dir.path().join("replacement.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let (mut session, server) = connect(&replacement, &full).await;
    let (offered, _) = session.peer_scopes(None).await.unwrap();
    assert_eq!(offered.len(), 3);
    for scope in offered {
        session
            .sync_scope(&replacement.replica, scope, QUOTA)
            .await
            .unwrap();
    }
    drop(session);
    server.await.unwrap();
    // Scopes that came as frames beside scope 1's pruned positions take
    // the full node's verified bases at the seal.
    let imported = crate::tests::import_needed_bases(
        &full.replica,
        &replacement.replica,
        &replacement.key,
        &replacement.credentials,
        scopes[0].domain,
    )
    .await;
    let needed: BTreeSet<_> = imported
        .iter()
        .map(|signed| signed.statement.scope)
        .collect();
    assert!(needed.contains(&scopes[0]));
    assert!(needed.is_subset(&BTreeSet::from([scopes[0], scopes[2]])));
    for signed in &imported {
        assert_eq!(
            signed.id().unwrap(),
            full.replica
                .checkpoint(signed.statement.scope)
                .await
                .unwrap()
                .id()
                .unwrap()
        );
    }
    for scope in &scopes {
        assert_eq!(
            all_rows(&replacement.replica, *scope).await,
            all_rows(&full.replica, *scope).await
        );
    }
    // The device continues against the replacement.
    rename(&f, &device, scopes[0], 3, "row 3", "moved").await;
    device
        .replica
        .set_role(partial(&[&replacement], 1))
        .await
        .unwrap();
    let (mut session, server) = connect(&device, &replacement).await;
    assert_eq!(
        session
            .push(&device.replica, scopes[0], QUOTA)
            .await
            .unwrap()
            .frames,
        1
    );
    session
        .sync_scope(&device.replica, scopes[0], QUOTA)
        .await
        .unwrap();
    drop(session);
    server.await.unwrap();
    assert_eq!(
        all_rows(&device.replica, scopes[0]).await,
        all_rows(&replacement.replica, scopes[0]).await
    );
    let status = device.replica.replica_status().await.unwrap();
    let entry = status
        .scopes
        .iter()
        .find(|entry| entry.scope == scopes[0])
        .unwrap();
    assert!(entry.peers.iter().any(|lag| lag.peer
        == rocky_credentials::PeerId::of(replacement.key.public_key())
        && lag.behind == 0
        && lag.ahead == 0));
}

#[tokio::test]
async fn a_far_behind_replica_catches_up_while_both_sides_keep_writing() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let scopes = departments(&f, 2);
    let full = open_node(
        &f,
        &dir.path().join("full.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let device = open_node(
        &f,
        &dir.path().join("device.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    device.replica.set_role(partial(&[&full], 1)).await.unwrap();
    device.replica.subscribe(scopes[0]).await.unwrap();
    device.replica.subscribe(scopes[1]).await.unwrap();
    const CATCH_UP_ROWS: u32 = 128;
    insert(&f, &full, scopes[0], 0..CATCH_UP_ROWS, 1).await;
    let (mut session, server) = connect(&device, &full).await;
    let done = std::sync::atomic::AtomicBool::new(false);
    let writes = |node: &'static str, replica: Replica, key: PrivateKey, scope| {
        let done = &done;
        let f = &f;
        async move {
            let mut count = 0u32;
            let mut slowest = std::time::Duration::ZERO;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let started = std::time::Instant::now();
                let mut handle = key.public_key().as_bytes().to_vec();
                handle.extend(count.to_le_bytes());
                replica
                    .author_buffered(
                        rocky_core::commitment("roles.concurrent.v1", &handle),
                        vec![RowChange {
                            scope,
                            row: row(10_000 + count),
                            before: None,
                            after: Some(f.app().name(node)),
                        }],
                        vec![],
                    )
                    .await
                    .unwrap();
                slowest = slowest.max(started.elapsed());
                count += 1;
                tokio::task::yield_now().await;
            }
            (count, slowest)
        }
    };
    let catch_up = async {
        let pass = session
            .catch_up(&device.replica, scopes[0], QUOTA)
            .await
            .unwrap();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        pass
    };
    let (pass, (local, local_slowest), (remote, remote_slowest)) = tokio::join!(
        catch_up,
        writes(
            "device",
            device.replica.clone(),
            device.key.clone(),
            scopes[1]
        ),
        writes("full", full.replica.clone(), full.key.clone(), scopes[1]),
    );
    // Bounded pages, and neither side's writes waited for the whole transfer.
    assert!(pass.frames >= CATCH_UP_ROWS as usize && pass.pages >= (CATCH_UP_ROWS / 32) as usize);
    assert!(local > 0 && remote > 0, "{local} {remote}");
    eprintln!(
        "writes during catch-up: device {local} (slowest {local_slowest:?}), full {remote} (slowest {remote_slowest:?})"
    );
    session
        .catch_up(&device.replica, scopes[0], QUOTA)
        .await
        .unwrap();
    drop(session);
    server.await.unwrap();
    assert_eq!(
        all_rows(&device.replica, scopes[0]).await,
        all_rows(&full.replica, scopes[0]).await
    );
}

#[tokio::test]
async fn a_sync_larger_than_a_session_continues_on_the_next() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let scope = department(&f, 0);
    let full = open_node(
        &f,
        &dir.path().join("full.db"),
        &PrivateKey::new(),
        &[scope],
        &[scope],
    )
    .await;
    let reader = open_node(
        &f,
        &dir.path().join("reader.db"),
        &PrivateKey::new(),
        &[scope],
        &[scope],
    )
    .await;
    let device = open_node(
        &f,
        &dir.path().join("device.db"),
        &PrivateKey::new(),
        &[scope],
        &[scope],
    )
    .await;
    insert(&f, &full, scope, 0..500, 1).await;
    // Both pruned history behind a checkpoint and frames after it.
    let (mut session, server) = connect(&reader, &full).await;
    session
        .catch_up(&reader.replica, scope, QUOTA)
        .await
        .unwrap();
    drop(session);
    server.await.unwrap();
    compacted_base(&full.replica, &f.publisher, scope).await;
    full.replica
        .set_retention_policy(scope, keep_one_copy())
        .await
        .unwrap();
    assert_eq!(full.replica.prune(scope).await.unwrap().pruned, 500);
    insert(&f, &full, scope, 500..2000, 1).await;
    let mut sessions = 0;
    let mut rebootstrapped = false;
    loop {
        let (mut session, server) = connect_with(&device, &full, 1 << 20).await;
        let synced = session
            .sync_scope(&device.replica, scope, QUOTA)
            .await
            .unwrap();
        drop(session);
        server.await.unwrap();
        sessions += 1;
        assert!(sessions <= 8, "bounded sync made insufficient progress");
        rebootstrapped |= synced.rebootstrap.is_some();
        if !synced.catch_up.exhausted {
            break;
        }
    }
    assert!(sessions > 1 && rebootstrapped, "{sessions}");
    assert_eq!(
        all_rows(&device.replica, scope).await,
        all_rows(&full.replica, scope).await
    );
}
