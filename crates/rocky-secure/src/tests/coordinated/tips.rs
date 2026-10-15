//! Per-tip logs on devices: a tip's coordinated writes commit through its
//! own log and reach only the trusted nodes that follow it.
use super::*;

/// The tip generation 2's devices follow, beside `main` at generation 3.
fn dev_log(lineage: &Lineage) -> TipLog {
    TipLog {
        scope: lineage.domain,
        tip: rocky_credentials::genesis_tip(lineage.definition(2).hash()),
    }
}
async fn open_dev(fixture: &Fixture, name: &str, key: &PrivateKey) -> Replica {
    let path = fixture.dir.path().join(format!("{name}.db"));
    let replica = fixture.lineage.open(&path, key, 2, None).await.unwrap();
    fixture.mesh.add(key, &replica);
    replica
}

#[tokio::test]
async fn a_dev_tips_write_commits_through_its_own_log_and_main_sees_no_entry() {
    let fixture = Fixture::new(1).await;
    fixture.invoice(&fixture.alice, "on main").await;
    let (main, dev) = (tip_log(&fixture.lineage), dev_log(&fixture.lineage));
    let before = fixture.mesh.log.head(main).unwrap();
    let key = PrivateKey::new();
    let dana = (key.clone(), open_dev(&fixture, "dana", &key).await);
    assert_eq!(dana.1.followed_log(fixture.lineage.domain).await, dev);
    let (client, peers) = (
        fixture.mesh.client(&key),
        fixture.mesh.peers(&fixture.lineage, &key),
    );
    let slot = dana.1.coordinated_slot(dev, &client, &peers).await.unwrap();
    assert_eq!((slot.log, slot.expected), (dev, 0));
    fixture.stage(&dana, slot, "on dev").await.unwrap();
    let commit = dana.1.propose(dev, &client, &peers).await.unwrap();
    assert_eq!(commit.position, 1);
    assert_eq!(fixture.mesh.log.head(dev).unwrap().commit, 1);
    // `main`'s in-sync set saw no entry of it.
    assert_eq!(fixture.mesh.log.head(main).unwrap(), before);
    for node in [&fixture.alice, &fixture.bob] {
        assert_eq!(fixture.resolve(node).await.position, 1);
        assert_eq!(node.1.coordinated_status(dev).await.unwrap().position, 0);
    }
    // Nothing routes a write of one tip to another tip's log or nodes.
    assert_eq!(
        dana.1.coordinated_slot(main, &client, &peers).await,
        Err(CoordinatedError::Unfollowed)
    );
    let alice = &fixture.alice;
    assert_eq!(
        alice
            .1
            .coordinated_slot(dev, &fixture.mesh.client(&alice.0), &peers)
            .await,
        Err(CoordinatedError::Unfollowed)
    );
    let entry = CoordinatedEntry {
        log: dev,
        pending: fixture.mesh.log.head(dev).unwrap().pending.map_or(
            LogPending {
                proposer: *key.public_key().as_bytes(),
                hash: commit.hash,
                position: commit.position,
                epoch: 0,
                deadline: 0,
            },
            |pending| pending.port(),
        ),
        frames: commit.frames,
        voids: Vec::new(),
    };
    assert_eq!(
        alice
            .1
            .store_entry(entry, &fixture.mesh.client(&alice.0))
            .await
            .map(|_| ()),
        Err(CoordinatedError::Unfollowed)
    );
}
