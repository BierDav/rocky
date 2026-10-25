mod support;
use rocky_coordinator::{
    coordinated::{Action, Result as LogResult},
    service::{Operation, Outcome},
};
use rocky_core::*;
use std::collections::BTreeSet;
use support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_persistent_voters_keep_controls_and_logs_across_restarts() {
    let mut cluster = Cluster::start().await;
    let admin = cluster.client(&cluster.admin);
    let issuer = cluster.client(&cluster.issuer);
    let alice = cluster.client(&cluster.executor);
    let alternate = cluster.client(&cluster.alternate);
    issuer
        .call([1; 32], Operation::Control(cluster.app_control(1)))
        .await
        .unwrap();
    join(&alice, cluster.scope, 1).await;
    let first = append(&alice, cluster.scope, 1, 2).await;
    let Outcome::Coordinated(LogResult::Assigned(pending)) = alice
        .coordinated(log(cluster.scope), first.clone())
        .await
        .unwrap()
    else {
        panic!("assignment")
    };
    // A retried append returns its first answer; another with its sequence conflicts.
    assert_eq!(
        alice
            .coordinated(log(cluster.scope), first.clone())
            .await
            .unwrap(),
        Outcome::Coordinated(LogResult::Assigned(pending.clone()))
    );
    let Action::Append {
        expected,
        epoch,
        duration,
        sequence,
        ..
    } = first
    else {
        unreachable!()
    };
    assert_eq!(
        alice
            .coordinated(
                log(cluster.scope),
                Action::Append {
                    expected,
                    epoch,
                    hash: [9; 32],
                    duration,
                    sequence,
                }
            )
            .await,
        Err(CoordinationError::RequestConflict)
    );
    assert!(matches!(
        alice
            .coordinated(
                log(cluster.scope),
                Action::Acknowledge { pending: *pending }
            )
            .await
            .unwrap(),
        Outcome::Coordinated(LogResult::Committed { position: 1, .. })
    ));
    let leader = cluster.wait_leader(&admin).await;
    cluster.stop(leader);
    let next = cluster.wait_leader(&admin).await;
    assert_ne!(next, leader);
    let head = alternate
        .coordinated_head(log(cluster.scope))
        .await
        .unwrap();
    assert_eq!((head.commit, head.hashes.len()), (1, 1));
    cluster.spawn(leader, false, None);
    cluster.wait_online(leader, &admin).await;
    membership_after_election(
        &admin,
        [5; 32],
        Operation::Membership {
            voters: BTreeSet::from([1, 2]),
            expected: BTreeSet::from([1, 2, 3]),
            expected_revision: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        membership_after_election(
            &admin,
            [5; 32],
            Operation::Membership {
                voters: BTreeSet::from([1, 2]),
                expected: BTreeSet::from([1, 2, 3]),
                expected_revision: 0,
            }
        )
        .await
        .unwrap(),
        Outcome::Membership(BTreeSet::from([1, 2]))
    );
    membership_after_election(
        &admin,
        [6; 32],
        Operation::Membership {
            voters: BTreeSet::from([1, 2, 3]),
            expected: BTreeSet::from([1, 2]),
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    // The retained plan neither reapplies nor reverses the later one.
    assert_eq!(
        membership_after_election(
            &admin,
            [5; 32],
            Operation::Membership {
                voters: BTreeSet::from([1, 2]),
                expected: BTreeSet::from([1, 2, 3]),
                expected_revision: 0,
            }
        )
        .await
        .unwrap(),
        Outcome::Membership(BTreeSet::from([1, 2]))
    );
    let leader = cluster.wait_leader(&admin).await;
    assert_eq!(
        admin.status(leader).await.unwrap().voters,
        BTreeSet::from([1, 2, 3])
    );
    for id in 1..=3 {
        admin.snapshot(id).await.unwrap();
    }
    for id in 1..=3 {
        cluster.stop(id);
        cluster.spawn(id, false, None);
        cluster.wait_online(id, &admin).await;
    }
    cluster.wait_leader(&admin).await;
    assert_eq!(
        alternate
            .coordinated_head(log(cluster.scope))
            .await
            .unwrap(),
        head
    );
    let mut invalid = cluster.app_control(2);
    invalid.control.configuration = [0; 16];
    assert!(
        issuer
            .call([7; 32], Operation::Control(invalid))
            .await
            .is_err()
    );
    // A revision cannot drop a trusted node the in-sync set still holds.
    let mut narrowed = cluster.app_control(2);
    narrowed.control.trusted = vec![cluster.alternate.public_key()];
    let narrowed =
        rocky_coordinator::certified::ControlCertificate::sign(narrowed.control, &cluster.issuer)
            .unwrap();
    assert_eq!(
        issuer.call([8; 32], Operation::Control(narrowed)).await,
        Err(CoordinationError::Backpressure)
    );
    issuer
        .call([9; 32], Operation::Control(cluster.app_control(2)))
        .await
        .unwrap();
    assert_eq!(
        alice
            .coordinated_head(log(cluster.scope))
            .await
            .unwrap()
            .commit,
        1
    );
    for id in 1..=3 {
        cluster.stop(id);
    }
    assert_eq!(
        alternate.coordinated_head(log(cluster.scope)).await,
        Err(CoordinationError::Unavailable)
    );
    // A surviving provisioning marker prevents a missing store from becoming genesis again.
    std::fs::remove_file(cluster.store_path(1)).unwrap();
    cluster.spawn(1, true, None);
    cluster.wait_exit(1).await;
    let log = std::fs::read_to_string(cluster.dir.path().join("voter-1.log")).unwrap();
    assert!(log.contains("CoordinationStateLost"), "{log}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashes_at_log_commit_apply_reply_and_snapshot_boundaries() {
    let mut cluster = Cluster::start().await;
    let admin = cluster.client(&cluster.admin);
    let issuer = cluster.client(&cluster.issuer);
    let alice = cluster.client(&cluster.executor);
    issuer
        .call([1; 32], Operation::Control(cluster.app_control(1)))
        .await
        .unwrap();
    join(&alice, cluster.scope, 1).await;
    let mut last = None;
    for (index, stage) in [
        "log.before_sync",
        "log.after_sync",
        "commit.after_sync",
        "apply.after_apply",
        "reply.before_send",
    ]
    .into_iter()
    .enumerate()
    {
        let leader = cluster.wait_leader(&admin).await;
        let before = alice.coordinated_head(log(cluster.scope)).await.unwrap();
        let action = append(&alice, cluster.scope, index as u8 + 1, index as u64 + 2).await;
        cluster.arm(leader, stage);
        let _ = alice.coordinated(log(cluster.scope), action.clone()).await;
        cluster.wait_exit(leader).await;
        cluster.wait_leader(&admin).await;
        // The retry answers what the crashed call may have applied, once.
        let Outcome::Coordinated(LogResult::Assigned(pending)) = alice
            .coordinated(log(cluster.scope), action.clone())
            .await
            .unwrap()
        else {
            panic!("{stage}: assignment")
        };
        assert_eq!(pending.position, before.commit + 1, "{stage}");
        assert_eq!(
            alice
                .coordinated(log(cluster.scope), action.clone())
                .await
                .unwrap(),
            Outcome::Coordinated(LogResult::Assigned(pending.clone()))
        );
        assert!(matches!(
            alice
                .coordinated(
                    log(cluster.scope),
                    Action::Acknowledge { pending: *pending }
                )
                .await
                .unwrap(),
            Outcome::Coordinated(LogResult::Committed { .. })
        ));
        cluster.spawn(leader, false, None);
        cluster.wait_online(leader, &admin).await;
        assert_eq!(
            alice
                .coordinated_head(log(cluster.scope))
                .await
                .unwrap()
                .commit,
            before.commit + 1,
            "{stage}"
        );
        last = Some(action);
    }
    for stage in ["snapshot.before_rename", "snapshot.after_sync"] {
        let leader = cluster.wait_leader(&admin).await;
        let before = alice.coordinated_head(log(cluster.scope)).await.unwrap();
        cluster.arm(leader, stage);
        let _ = admin.snapshot(leader).await;
        cluster.wait_exit(leader).await;
        cluster.wait_leader(&admin).await;
        cluster.spawn(leader, false, None);
        cluster.wait_online(leader, &admin).await;
        assert_eq!(
            alice.coordinated_head(log(cluster.scope)).await.unwrap(),
            before
        );
    }
    let leader = cluster.wait_leader(&admin).await;
    let lagging = if leader == 1 { 2 } else { 1 };
    let lagging_applied = admin.status(lagging).await.unwrap().applied.unwrap();
    cluster.stop(lagging);
    // Reads add no log entries; retries of a decided append do, unchanged.
    let last = last.unwrap();
    for _ in 0..80 {
        alice
            .coordinated(log(cluster.scope), last.clone())
            .await
            .unwrap();
    }
    let leader = cluster.wait_leader(&admin).await;
    let applied = admin.status(leader).await.unwrap().applied.unwrap();
    admin.snapshot(leader).await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if admin
            .status(leader)
            .await
            .unwrap()
            .snapshot
            .is_some_and(|index| index >= applied)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "snapshot did not finish"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    cluster.arm(lagging, "install_snapshot.before_rename");
    cluster.spawn(lagging, false, None);
    cluster.wait_exit(lagging).await;
    cluster.spawn(lagging, false, None);
    cluster.wait_online(lagging, &admin).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let status = admin.status(lagging).await.unwrap();
        if status.snapshot.is_some_and(|index| index > lagging_applied)
            && status.applied.is_some_and(|index| index >= applied)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "lagging voter did not install snapshot: lagging={status:?}, required={applied}, prior={lagging_applied}, leader={:?}, log={}",
            admin.status(leader).await,
            std::fs::read_to_string(cluster.dir.path().join(format!("voter-{lagging}.log")))
                .unwrap()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let status = admin.status(leader).await.unwrap();
    println!(
        "crash exercise: 5 append boundaries, 2 snapshot boundaries plus interrupted snapshot transfer; leader disk bytes={} logs={} snapshots={}",
        status.disk_bytes, status.logs, status.snapshots
    );
}
