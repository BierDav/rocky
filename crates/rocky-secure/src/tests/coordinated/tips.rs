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
        carry: None,
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

/// Alice commits invoice 0001, then stages 0002, which bob stores without
/// acknowledging while bob seals `main` with `terminal`. Returns alice's
/// outcome of the pending write and the seal's position.
async fn seal_while_pending(
    fixture: &Fixture,
    terminal: rocky_core::LogTerminal,
) -> (
    Result<crate::replica::CoordinatedCommit, CoordinatedError>,
    u64,
) {
    let (alice, bob) = (&fixture.alice, &fixture.bob);
    fixture.invoice(alice, "before").await;
    let slot = fixture.slot(alice).await.unwrap();
    fixture.stage(alice, slot, "pending").await.unwrap();
    fixture.mesh.set(&bob.0, true, false);
    let main = tip_log(&fixture.lineage);
    let (pending, sealed) = tokio::join!(fixture.propose(alice), async {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        bob.1
            .seal_log(
                main,
                terminal,
                &fixture.mesh.client(&bob.0),
                &fixture.mesh.peers(&fixture.lineage, &bob.0),
            )
            .await
    });
    fixture.mesh.set(&bob.0, true, true);
    (pending, sealed.unwrap())
}

#[tokio::test]
async fn a_pending_write_at_a_close_is_rolled_back_and_held() {
    let fixture = Fixture::new(1).await;
    let terminal = rocky_core::LogTerminal {
        digest: [7; 32],
        continuation: None,
    };
    let (pending, position) = seal_while_pending(&fixture, terminal).await;
    assert!(
        matches!(pending, Err(CoordinatedError::Sealed)),
        "{pending:?}"
    );
    assert_eq!(position, 2);
    let alice = &fixture.alice.1;
    assert_eq!(invoices(alice).await, numbered(1));
    let held = alice.held_changes().await.unwrap();
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(fixture.resolve(&fixture.alice).await.undecided, 0);
}

#[tokio::test]
async fn a_node_holding_entries_of_a_dropped_ended_log_never_rejoins_it() {
    let fixture = Fixture::new(1).await;
    fixture.invoice(&fixture.alice, "before").await;
    let main = tip_log(&fixture.lineage);
    let client = fixture.mesh.client(&fixture.bob.0);
    let end = |byte: u8| rocky_core::LogTerminal {
        digest: [byte; 32],
        continuation: None,
    };
    client.seal(main, end(1)).await.unwrap();
    // Later ended logs push `main`'s out of the coordinator's retention.
    for byte in 0..=rocky_coordinator::coordinated::ENDED_LOGS as u8 {
        let other = TipLog {
            scope: main.scope,
            tip: rocky_core::TipId([byte; 32]),
        };
        client.seal(other, end(byte)).await.unwrap();
    }
    let head = fixture.mesh.log.head(main).unwrap();
    assert_eq!((head.commit, head.terminal), (0, None));
    assert_eq!(
        fixture.slot(&fixture.alice).await.map(|_| ()),
        Err(CoordinatedError::Sealed)
    );
    assert_eq!(fixture.mesh.log.head(main).unwrap().members.len(), 0);
}
