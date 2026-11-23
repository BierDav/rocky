use super::*;
use crate::storage::{collect_rows, rows};
use employees_app::Employees;
use rocky_core::SqlValue;
use std::path::{Path, PathBuf};

struct Device {
    key: PrivateKey,
    credentials: CredentialChain,
    path: PathBuf,
}
impl Device {
    fn new(fixture: &Fixture, key: PrivateKey, path: PathBuf) -> Self {
        Self {
            credentials: fixture.credentials(&key, fixture.full()),
            key,
            path,
        }
    }
    fn at(&self, path: PathBuf) -> Self {
        Self {
            key: self.key.clone(),
            credentials: self.credentials.clone(),
            path,
        }
    }
    fn config(&self, fixture: &Fixture) -> SecureConfig {
        SecureConfig {
            credentials: self.credentials.clone(),
            ..fixture.config(&self.key, fixture.full(), &[fixture.public])
        }
    }
    async fn open(&self, fixture: &Fixture) -> EncryptedReplica<Employees> {
        EncryptedReplica::open(&self.path, fixture.app(), self.config(fixture))
            .await
            .unwrap()
    }
    async fn restore(&self, fixture: &Fixture) -> EncryptedReplica<Employees> {
        EncryptedReplica::open(
            &self.path,
            fixture.app(),
            OpenConfig::from(self.config(fixture)).possibly_restored(),
        )
        .await
        .unwrap()
    }
    async fn rejoin(
        &self,
        fixture: &Fixture,
        graph: rocky_credentials::TipGraph,
    ) -> EncryptedReplica<Employees> {
        EncryptedReplica::open_following(
            &self.path,
            fixture.app(),
            OpenConfig::from(self.config(fixture)).possibly_restored(),
            graph,
            None,
        )
        .await
        .unwrap()
    }
    fn end<'a>(
        &self,
        replica: &'a EncryptedReplica<Employees>,
    ) -> (&'a EncryptedReplica<Employees>, PrivateKey, CredentialChain) {
        (replica, self.key.clone(), self.credentials.clone())
    }
}

async fn pull(
    (source, from): (&EncryptedReplica<Employees>, &Device),
    (target, to): (&EncryptedReplica<Employees>, &Device),
    scope: ResourceScope,
) -> Result<(), Error> {
    let (near, far) = network::link::testing::connected().await;
    let (mut session, mut served) =
        network::link::testing::sessions_over(near, far, to.end(target), from.end(source)).await?;
    let server = served.serve_to_end(source);
    let client = async {
        let caught_up = session
            .catch_up(target, scope, MAX_SECURE_FRAME / 2)
            .await
            .map(|_| ());
        session.close().await?;
        caught_up
    };
    let (served, pulled) = tokio::join!(server, client);
    pulled?;
    served
}

async fn write(
    replica: &EncryptedReplica<Employees>,
    fixture: &Fixture,
    row: u8,
    name: &str,
) -> Result<EncryptedFrame, Error> {
    replica
        .author(
            [row; 32],
            vec![change(
                fixture.public,
                row,
                None,
                Some(fixture.app().name(name)),
            )],
            vec![],
        )
        .await
}

fn blocked_by(result: Result<EncryptedFrame, Error>) -> Vec<crate::BlockerKind> {
    match result {
        Err(Error::Blocked(blockers)) => blockers.into_iter().map(|blocker| blocker.kind).collect(),
        other => panic!("expected a retryable blocker, got {other:?}"),
    }
}

fn superseded(result: Result<EncryptedFrame, Error>) -> crate::StoreSuperseded {
    match result {
        Err(Error::Application(error)) => error
            .downcast_ref::<crate::StoreSuperseded>()
            .cloned()
            .unwrap_or_else(|| panic!("expected a superseded store, got {error}")),
        other => panic!("expected a superseded store, got {other:?}"),
    }
}

async fn own_log(replica: &EncryptedReplica<Employees>, author: &PrivateKey) -> Vec<(i64, Hash)> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    collect_rows(
        inner
            .conn
            .query(
                "SELECT sequence,hash FROM secure_operations WHERE author=? ORDER BY sequence",
                [author.public_key().as_bytes().to_vec()],
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap()
    .into_iter()
    .map(|record| match record.as_slice() {
        [SqlValue::Integer(sequence), SqlValue::Blob(hash)] => (
            *sequence,
            Hash::from_bytes(hash.as_slice().try_into().unwrap()),
        ),
        other => panic!("own log row {other:?}"),
    })
    .collect()
}

fn wipe(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        let _ = rocky_platform::fs::remove_file(PathBuf::from(file));
    }
}

fn copy_store(from: &Path, to: &Path) {
    for suffix in ["", "-wal"] {
        let (mut source, mut target) = (from.as_os_str().to_owned(), to.as_os_str().to_owned());
        source.push(suffix);
        target.push(suffix);
        if rocky_platform::fs::exists(PathBuf::from(&source)) {
            rocky_platform::fs::copy(PathBuf::from(source), PathBuf::from(target)).unwrap();
        }
    }
}

async fn covering(
    replica: &EncryptedReplica<Employees>,
    scope: ResourceScope,
    author: &PrivateKey,
    sequence: u64,
) -> BTreeSet<rocky_credentials::PeerId> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    inner
        .covering_peers(
            &crate::encode(&scope).unwrap(),
            author.public_key(),
            sequence,
        )
        .await
        .unwrap()
        .0
}

async fn acknowledgments(replica: &EncryptedReplica<Employees>, peer: &PrivateKey) -> i64 {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    match rows(
        &inner.conn,
        &format!(
            "SELECT count(*) FROM secure_ack WHERE peer=x'{}'",
            rocky_credentials::PeerId::of(peer.public_key()).to_hex()
        ),
    )
    .await
    .unwrap()
    .as_slice()
    {
        [record] => match record.as_slice() {
            [SqlValue::Integer(count)] => *count,
            other => panic!("acknowledgment count {other:?}"),
        },
        other => panic!("acknowledgment count {other:?}"),
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_wiped_store_rejoining_with_its_node_key_catches_up_before_it_authors() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
    let scope = fixture.public;
    let peer = Device::new(&fixture, PrivateKey::new(), dir.path().join("peer.db"));
    let worker = Device::new(&fixture, PrivateKey::new(), dir.path().join("worker.db"));
    let p = peer.open(&fixture).await;
    let w = worker.open(&fixture).await;
    let peers_frame = write(&p, &fixture, 9, "Peer").await.unwrap();
    pull((&p, &peer), (&w, &worker), scope).await.unwrap();
    for row in 1..=3 {
        write(&w, &fixture, row, "Worker").await.unwrap();
    }
    pull((&w, &worker), (&p, &peer), scope).await.unwrap();
    let before_wipe = own_log(&p, &worker.key).await;
    assert_eq!(before_wipe.len(), 3);
    assert!(
        covering(&p, scope, &worker.key, 2)
            .await
            .contains(&rocky_credentials::PeerId::of(worker.key.public_key()))
    );
    assert!(
        covering(&p, scope, &peer.key, 0)
            .await
            .contains(&rocky_credentials::PeerId::of(worker.key.public_key()))
    );
    let highest_stamp = before_wipe.len() as u64;
    w.close().await;
    wipe(&worker.path);

    let rejoined = worker.at(dir.path().join("worker-again.db"));
    let w = rejoined
        .rejoin(&fixture, p.tip_graph(scope.domain).await.unwrap())
        .await;
    assert_eq!(
        blocked_by(write(&w, &fixture, 4, "Too early").await),
        vec![crate::BlockerKind::AwaitingPeerFrontier]
    );
    let peer_view = p.frontier(scope).await.unwrap();
    w.acknowledge_frontier(peer.key.public_key(), &peer.credentials, &peer_view)
        .await
        .unwrap();
    assert_eq!(
        blocked_by(write(&w, &fixture, 4, "Still behind").await),
        vec![crate::BlockerKind::OwnLogBehind {
            held: 0,
            reported: 3
        }]
    );
    assert!(
        w.blockers()
            .await
            .unwrap()
            .iter()
            .any(|blocker| blocker.kind
                == crate::BlockerKind::OwnLogBehind {
                    held: 0,
                    reported: 3
                })
    );

    let wiped_view = w.frontier(scope).await.unwrap();
    p.acknowledge_frontier(worker.key.public_key(), &worker.credentials, &wiped_view)
        .await
        .unwrap();
    let regressions = p.lane_alerts().await.unwrap().regressions;
    assert_eq!(regressions.len(), 1, "{regressions:?}");
    assert_eq!(regressions[0].node, worker.key.public_key());
    assert_eq!((regressions[0].held, regressions[0].reported), (3, 0));

    pull((&p, &peer), (&w, &rejoined), scope).await.unwrap();
    assert_eq!(own_log(&w, &worker.key).await, before_wipe);
    assert!(p.lane_alerts().await.unwrap().regressions.is_empty());

    let next = write(&w, &fixture, 4, "Caught up").await.unwrap();
    let header = next.operation().unwrap().header;
    assert_eq!(header.seq_num, 3);
    assert!(header.extensions.author_sequence >= highest_stamp);
    pull((&w, &rejoined), (&p, &peer), scope).await.unwrap();
    assert_eq!(
        own_log(&p, &worker.key).await,
        own_log(&w, &worker.key).await
    );
    assert_eq!(own_log(&p, &worker.key).await.len(), 4);
    assert!(p.lane_alerts().await.unwrap().regressions.is_empty());
    assert_eq!(
        p.snapshot(scope).await.unwrap().rows[&[4; 32]],
        fixture.app().name("Caught up")
    );
    let _ = peers_frame;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn peers_stop_counting_a_wiped_node_as_a_durable_holder() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
    let scope = fixture.public;
    let peer = Device::new(&fixture, PrivateKey::new(), dir.path().join("peer.db"));
    let worker = Device::new(&fixture, PrivateKey::new(), dir.path().join("worker.db"));
    let p = peer.open(&fixture).await;
    let w = worker.open(&fixture).await;
    let peers_frame = write(&p, &fixture, 9, "Peer").await.unwrap();
    pull((&p, &peer), (&w, &worker), scope).await.unwrap();
    write(&w, &fixture, 1, "Worker").await.unwrap();
    pull((&w, &worker), (&p, &peer), scope).await.unwrap();
    p.acknowledge(
        worker.key.public_key(),
        &worker.credentials,
        peers_frame.id().unwrap(),
        true,
    )
    .await
    .unwrap();
    let worker_id = rocky_credentials::PeerId::of(worker.key.public_key());
    assert!(covering(&p, scope, &peer.key, 0).await.contains(&worker_id));
    assert_eq!(acknowledgments(&p, &worker.key).await, 1);
    w.close().await;
    wipe(&worker.path);

    let rejoined = worker.at(dir.path().join("worker-again.db"));
    let w = rejoined
        .rejoin(&fixture, p.tip_graph(scope.domain).await.unwrap())
        .await;
    let empty = w.frontier(scope).await.unwrap();
    p.acknowledge_frontier(worker.key.public_key(), &worker.credentials, &empty)
        .await
        .unwrap();
    assert!(!covering(&p, scope, &peer.key, 0).await.contains(&worker_id));
    assert!(
        !covering(&p, scope, &worker.key, 0)
            .await
            .contains(&worker_id)
    );
    assert_eq!(acknowledgments(&p, &worker.key).await, 0);
    let regressions = p.lane_alerts().await.unwrap().regressions;
    assert_eq!(regressions.len(), 1, "{regressions:?}");
    assert_eq!(regressions[0].node, worker.key.public_key());
    assert_eq!(regressions[0].scope, scope);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_older_copy_of_a_store_never_authors_and_a_new_device_replaces_it() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
    let scope = fixture.public;
    let peer = Device::new(&fixture, PrivateKey::new(), dir.path().join("peer.db"));
    let original = Device::new(&fixture, PrivateKey::new(), dir.path().join("original.db"));
    let copy = original.at(dir.path().join("copy.db"));
    let p = peer.open(&fixture).await;
    let o = original.open(&fixture).await;
    for row in 1..=2 {
        write(&o, &fixture, row, "Original").await.unwrap();
    }
    pull((&o, &original), (&p, &peer), scope).await.unwrap();
    o.close().await;
    copy_store(&original.path, &copy.path);

    let o = original.open(&fixture).await;
    for row in 3..=4 {
        write(&o, &fixture, row, "Original").await.unwrap();
    }
    pull((&o, &original), (&p, &peer), scope).await.unwrap();

    let c = copy.restore(&fixture).await;
    assert_eq!(
        blocked_by(write(&c, &fixture, 5, "Copy").await),
        vec![crate::BlockerKind::AwaitingPeerFrontier]
    );
    pull((&p, &peer), (&c, &copy), scope).await.unwrap();
    let refused = superseded(write(&c, &fixture, 5, "Copy").await);
    assert_eq!(refused.node, original.key.public_key());
    assert_eq!((refused.held, refused.reported), (2, 4));
    assert!(refused.to_string().contains("re-invite it as a new device"));
    assert_eq!(
        c.lane_alerts().await.unwrap().superseded,
        vec![refused.clone()]
    );
    assert!(
        c.blockers()
            .await
            .unwrap()
            .iter()
            .any(|blocker| matches!(blocker.kind, crate::BlockerKind::StoreSuperseded { .. }))
    );

    write(&p, &fixture, 9, "Peer").await.unwrap();
    pull((&p, &peer), (&c, &copy), scope).await.unwrap();
    assert_eq!(
        c.snapshot(scope).await.unwrap().rows[&[9; 32]],
        fixture.app().name("Peer")
    );
    assert_eq!(
        own_log(&c, &original.key).await,
        own_log(&p, &original.key).await
    );
    superseded(write(&c, &fixture, 5, "Copy").await);
    c.close().await;
    let c = copy.open(&fixture).await;
    superseded(write(&c, &fixture, 5, "Copy after restart").await);
    c.close().await;

    let reinvited = Device::new(&fixture, PrivateKey::new(), dir.path().join("reinvited.db"));
    let r = reinvited.open(&fixture).await;
    pull((&p, &peer), (&r, &reinvited), scope).await.unwrap();
    write(&r, &fixture, 5, "New device").await.unwrap();
    pull((&r, &reinvited), (&p, &peer), scope).await.unwrap();
    pull((&p, &peer), (&o, &original), scope).await.unwrap();
    write(&o, &fixture, 6, "Original").await.unwrap();
    pull((&o, &original), (&p, &peer), scope).await.unwrap();

    let rows = p.snapshot(scope).await.unwrap().rows;
    assert_eq!(rows[&[5; 32]], fixture.app().name("New device"));
    assert_eq!(rows[&[6; 32]], fixture.app().name("Original"));
    assert_eq!(
        own_log(&p, &original.key).await,
        own_log(&o, &original.key).await
    );
    assert_eq!(own_log(&p, &original.key).await.len(), 5);
    assert!(p.lane_alerts().await.unwrap().regressions.is_empty());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn of_two_live_copies_of_one_store_only_one_authors() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
    let scope = fixture.public;
    let peer = Device::new(&fixture, PrivateKey::new(), dir.path().join("peer.db"));
    let first = Device::new(&fixture, PrivateKey::new(), dir.path().join("first.db"));
    let second = first.at(dir.path().join("second.db"));
    let p = peer.open(&fixture).await;
    let a = first.open(&fixture).await;
    write(&a, &fixture, 1, "Shared").await.unwrap();
    pull((&a, &first), (&p, &peer), scope).await.unwrap();
    a.close().await;
    copy_store(&first.path, &second.path);

    let a = first.open(&fixture).await;
    let b = second.restore(&fixture).await;
    assert_eq!(
        blocked_by(write(&b, &fixture, 3, "Second").await),
        vec![crate::BlockerKind::AwaitingPeerFrontier]
    );
    pull((&p, &peer), (&b, &second), scope).await.unwrap();
    write(&a, &fixture, 2, "First").await.unwrap();
    pull((&a, &first), (&p, &peer), scope).await.unwrap();
    pull((&p, &peer), (&b, &second), scope).await.unwrap();
    superseded(write(&b, &fixture, 3, "Second").await);

    let log = own_log(&p, &first.key).await;
    assert_eq!(log.len(), 2);
    assert_eq!(own_log(&a, &first.key).await, log);
    assert_eq!(own_log(&b, &first.key).await, log);
    write(&a, &fixture, 4, "First again").await.unwrap();
    pull((&a, &first), (&p, &peer), scope).await.unwrap();
    assert_eq!(own_log(&p, &first.key).await.len(), 3);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_reused_key_that_writes_before_its_first_exchange_is_retired_and_still_reads() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
    let scope = fixture.public;
    let peer = Device::new(&fixture, PrivateKey::new(), dir.path().join("peer.db"));
    let worker = Device::new(&fixture, PrivateKey::new(), dir.path().join("worker.db"));
    let p = peer.open(&fixture).await;
    let w = worker.open(&fixture).await;
    for row in 1..=3 {
        write(&w, &fixture, row, "Worker").await.unwrap();
    }
    pull((&w, &worker), (&p, &peer), scope).await.unwrap();
    let before_wipe = own_log(&p, &worker.key).await;
    w.close().await;
    wipe(&worker.path);

    let rejoined = worker.at(dir.path().join("worker-again.db"));
    let w = rejoined.open(&fixture).await;
    let forked = write(&w, &fixture, 4, "Forked").await.unwrap();
    assert_eq!(forked.operation().unwrap().header.seq_num, 0);
    write(&p, &fixture, 9, "Peer").await.unwrap();
    pull((&p, &peer), (&w, &rejoined), scope).await.unwrap();
    let refused = superseded(write(&w, &fixture, 5, "Again").await);
    assert_eq!((refused.held, refused.reported), (1, 3));
    assert_eq!(
        w.snapshot(scope).await.unwrap().rows[&[9; 32]],
        fixture.app().name("Peer")
    );
    assert_eq!(own_log(&p, &worker.key).await, before_wipe);
    assert!(
        pull((&w, &rejoined), (&p, &peer), scope).await.is_ok(),
        "the peer keeps its log and refuses nothing it already holds"
    );
    assert_eq!(own_log(&p, &worker.key).await, before_wipe);
}
