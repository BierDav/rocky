//! A device's backlog reaching a peer whose storage stays busy for longer
//! than one read deadline at a time, as a browser replica applying a large
//! batch does.
use super::*;
use crate::network::link::{Link, testing};
use crate::replica::Class;
use std::time::Duration;

type Replica = EncryptedReplica<Employees>;

/// Longer than any single read deadline the link keeps.
const BUSY: Duration = Duration::from_secs(11);
/// Frames each device authored before going online, per scope.
const BACKLOG: u16 = 150;
const QUOTA: usize = MAX_SECURE_FRAME / 2;

async fn open(f: &Fixture, dir: &Scratch, node: &PrivateKey) -> Replica {
    EncryptedReplica::open(
        dir.path().join(format!("{}.db", node.public_key())),
        f.app(),
        f.config(node, f.full(), &[f.public, f.private]),
    )
    .await
    .unwrap()
}
async fn author(replica: &Replica, f: &Fixture, scope: ResourceScope, row: u16) {
    let mut id = [9; 32];
    id[..2].copy_from_slice(&row.to_be_bytes());
    id[2] = u8::from(scope == f.private);
    let after = if scope == f.private {
        f.app().salary(&row.to_string())
    } else {
        f.app().name(&format!("entry {row}"))
    };
    replica
        .author(
            id,
            vec![RowChange {
                scope,
                row: id,
                before: None,
                after: Some(after),
            }],
            vec![],
        )
        .await
        .unwrap();
}
/// Frames of `scope` that `replica` holds, applied or not.
async fn held(replica: &Replica, scope: ResourceScope) -> u64 {
    replica
        .frontier(scope)
        .await
        .unwrap()
        .logs
        .iter()
        .map(|log| log.held)
        .sum()
}
/// Holds `replica`'s storage in stretches of `BUSY` until `stop` is set,
/// releasing it only for the waiter queued meanwhile.
async fn keep_busy(replica: Replica, stop: tokio::sync::watch::Receiver<bool>) {
    while !*stop.borrow() {
        let held = replica.inner.lock(Class::Background).await;
        rocky_platform::time::sleep(BUSY).await;
        drop(held);
        tokio::task::yield_now().await;
    }
}
/// Dials `server` until a link is admitted, at most `attempts` times; the
/// failures in between, in order.
async fn admitted(
    (client, client_key): (&Replica, &PrivateKey),
    (server, server_key): (&Replica, &PrivateKey),
    f: &Fixture,
    attempts: usize,
) -> (Option<(Link, Link)>, Vec<String>) {
    let mut failures = Vec::new();
    for _ in 0..attempts {
        let (near, far) = tokio::io::duplex(64 * 1024);
        let (dialed, accepted) = testing::links(
            near,
            far,
            (
                client,
                client_key.clone(),
                f.credentials(client_key, f.full()),
            ),
            (
                server,
                server_key.clone(),
                f.credentials(server_key, f.full()),
            ),
        )
        .await;
        match (dialed, accepted) {
            (Ok(dialed), Ok(accepted)) => return (Some((dialed, accepted)), failures),
            (dialed, accepted) => {
                failures.push(format!(
                    "{:?} / {:?}",
                    dialed.err().map(|error| error.to_string()),
                    accepted.err().map(|error| error.to_string())
                ));
                rocky_platform::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    (None, failures)
}

#[tokio::test(start_paused = true)]
async fn a_backlog_reaches_a_peer_whose_storage_stays_busy_past_each_read_deadline() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let (native, browser) = (PrivateKey::new(), PrivateKey::new());
    let device = open(&f, &dir, &native).await;
    let peer = open(&f, &dir, &browser).await;
    let scopes = [f.public, f.private];
    for scope in scopes {
        for row in 0..BACKLOG {
            author(&device, &f, scope, row).await;
        }
    }
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let busy = rocky_platform::task::spawn(keep_busy(peer.clone(), stopped));
    let (link, failures) = admitted((&device, &native), (&peer, &browser), &f, 8).await;
    let Some((mut dialed, mut accepted)) = link else {
        panic!("a busy but working peer never admitted the link: {failures:?}");
    };
    let (near, far) = tokio::join!(dialed.ready(false), accepted.ready(false));
    near.unwrap();
    far.unwrap();
    let handle = dialed.channels.handle.clone();
    let (served, mut incoming) = tokio::sync::mpsc::channel(4);
    rocky_platform::task::spawn(dialed.drive(tokio::sync::mpsc::channel(1).0));
    rocky_platform::task::spawn(accepted.drive(served));
    let serving = {
        let peer = peer.clone();
        rocky_platform::task::spawn(async move {
            while let Some(mut session) = incoming.recv().await {
                let peer = peer.clone();
                rocky_platform::task::spawn(async move {
                    let _ended_by_the_requester = session.serve_to_end(&peer).await;
                });
            }
        })
    };
    for scope in scopes {
        let mut reached = held(&peer, scope).await;
        for attempt in 0.. {
            assert!(attempt < 64, "the push of {scope:?} stalled at {reached}");
            let mut session = handle.open().await.unwrap();
            let pushed = session.push(&device, scope, QUOTA).await;
            let _closed_unless_the_request_already_ended_it = session.close().await;
            match pushed {
                Ok(pushed) if !pushed.exhausted => break,
                Ok(_) => {}
                Err(error) => assert!(
                    matches!(&error, Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut),
                    "only the request's lifetime ends it: {error}"
                ),
            }
            let now = held(&peer, scope).await;
            assert!(
                now > reached,
                "attempt {attempt} on {scope:?} left the peer at {reached}: the next one would resend the same page"
            );
            reached = now;
        }
    }
    stop.send_replace(true);
    busy.await.unwrap();
    serving.abort();
    for scope in scopes {
        let own = device.frontier(scope).await.unwrap();
        assert_eq!(
            peer.frontier(scope).await.unwrap(),
            own,
            "the peer holds the whole backlog of {scope:?}"
        );
        assert!(own.logs.iter().all(|log| log.applied == u64::from(BACKLOG)));
    }
    let status = device.replica_status().await.unwrap();
    for scope in scopes {
        let entry = status
            .scopes
            .iter()
            .find(|entry| entry.scope == scope)
            .unwrap();
        assert!(
            !entry.peers.is_empty() && entry.peers.iter().all(|lag| lag.behind == 0),
            "the device records that the peer holds all of {scope:?}: {:?}",
            entry.peers
        );
    }
}
