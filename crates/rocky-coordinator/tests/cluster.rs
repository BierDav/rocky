//! Persistent voters: controls, logs, counter claims, crashes and restarts.
mod support;
use rocky_coordinator::{
    coordinated::{Action, Result as LogResult},
    counters::{Claim, Lookup, Name},
    service::{NetworkCoordinator, Operation, Outcome},
};
use rocky_core::*;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
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

fn claim(n: u8) -> Claim {
    Claim {
        id: [n; 32],
        issued: timestamp(),
    }
}
/// Claims until the answer is definite: an uncertain result (a crashed or
/// replaced leader) is retried with the same claim, as applications do.
async fn claim_definitely(
    client: &NetworkCoordinator,
    scope: Scope,
    counter: &Name,
    claim: Claim,
) -> Result<u64, CoordinationError> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match client.claim(scope, counter.clone(), claim).await {
                Err(CoordinationError::Unavailable) => continue,
                result => return result,
            }
        }
    })
    .await
    .unwrap_or(Err(CoordinationError::Unavailable))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn voters_agree_on_claimed_values_across_crashes_leader_changes_and_snapshots() {
    let mut cluster = Cluster::start().await;
    let admin = cluster.client(&cluster.admin);
    let issuer = cluster.client(&cluster.issuer);
    let nodes = [
        Arc::new(cluster.client(&cluster.executor)),
        Arc::new(cluster.client(&cluster.alternate)),
    ];
    issuer
        .call([1; 32], Operation::Control(cluster.app_control(1)))
        .await
        .unwrap();
    let scope = cluster.scope;
    let invoices = Name::new("company-1/invoices").unwrap();

    // Concurrent claims from both trusted nodes take distinct values, and
    // each node's own sequence of answers strictly increases.
    let claims: Vec<Claim> = (0..24).map(claim).collect();
    let tasks: Vec<_> = (0..2)
        .map(|node| {
            let client = nodes[node].clone();
            let counter = invoices.clone();
            let mine: Vec<_> = claims.iter().copied().skip(node).step_by(2).collect();
            tokio::spawn(async move {
                let mut values = Vec::new();
                for claim in mine {
                    values.push((claim, client.claim(scope, counter.clone(), claim).await));
                }
                values
            })
        })
        .collect();
    let mut taken = Vec::new();
    for task in tasks {
        let values: Vec<_> = task.await.unwrap();
        let ordered: Vec<u64> = values.iter().map(|(_, v)| *v.as_ref().unwrap()).collect();
        assert!(
            ordered.windows(2).all(|pair| pair[0] < pair[1]),
            "{ordered:?}"
        );
        taken.extend(values.into_iter().map(|(claim, v)| (claim, v.unwrap())));
    }
    let distinct: BTreeSet<u64> = taken.iter().map(|(_, value)| *value).collect();
    assert_eq!(distinct, (1..=24).collect());
    // Either node's retry of either node's claim answers the same value.
    for (n, (claim, value)) in taken.iter().enumerate() {
        let retried = nodes[n % 2]
            .claim(scope, invoices.clone(), *claim)
            .await
            .unwrap();
        assert_eq!(retried, *value);
    }

    // The leader crashes after applying a claim and before replying: the
    // retry answers the value it took, once.
    let leader = cluster.wait_leader(&admin).await;
    let crashed = claim(100);
    cluster.arm(leader, "reply.before_send");
    let _ = nodes[0].claim(scope, invoices.clone(), crashed).await;
    cluster.wait_exit(leader).await;
    assert_eq!(
        claim_definitely(&nodes[1], scope, &invoices, crashed).await,
        Ok(25)
    );
    assert_eq!(
        claim_definitely(&nodes[0], scope, &invoices, claim(101)).await,
        Ok(26)
    );
    cluster.spawn(leader, false, None);
    cluster.wait_online(leader, &admin).await;

    // A leader change keeps every value: the new leader answers retries.
    let leader = cluster.wait_leader(&admin).await;
    cluster.stop(leader);
    assert_ne!(cluster.wait_leader(&admin).await, leader);
    for (claim, value) in &taken {
        assert_eq!(
            claim_definitely(&nodes[1], scope, &invoices, *claim).await,
            Ok(*value)
        );
    }
    assert_eq!(
        claim_definitely(&nodes[1], scope, &invoices, claim(102)).await,
        Ok(27)
    );
    cluster.spawn(leader, false, None);
    cluster.wait_online(leader, &admin).await;

    // Every voter restarts from its snapshot with the same counter.
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
        claim_definitely(&nodes[0], scope, &invoices, crashed).await,
        Ok(25)
    );
    let reading = nodes[0]
        .counter(scope, invoices.clone(), Some(taken[3].0))
        .await
        .unwrap();
    assert_eq!(reading.value, 27);
    assert_eq!(reading.claim, Some(Lookup::Claimed(taken[3].1)));
    assert_eq!(
        claim_definitely(&nodes[1], scope, &invoices, claim(103)).await,
        Ok(28)
    );

    // Only the domain's trusted nodes claim or read; the control authority
    // and other keys are refused.
    let stranger = cluster.client(&cluster.user);
    for client in [&issuer, &stranger] {
        assert_eq!(
            client.claim(scope, invoices.clone(), claim(104)).await,
            Err(CoordinationError::Denied)
        );
        assert_eq!(
            client.counter(scope, invoices.clone(), None).await,
            Err(CoordinationError::Denied)
        );
    }
    assert_eq!(
        nodes[0]
            .counter(scope, invoices.clone(), Some(claim(104)))
            .await
            .unwrap()
            .claim,
        Some(Lookup::Unclaimed)
    );
}
