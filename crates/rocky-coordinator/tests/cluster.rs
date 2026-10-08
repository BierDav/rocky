mod support;
use rocky_coordinator::service::{Operation, Outcome};
use rocky_core::*;
use std::collections::BTreeSet;
use support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_persistent_voters_fence_recover_and_verify_offline() {
    let mut cluster = Cluster::start().await;
    let admin = cluster.client(&cluster.admin, "admin");
    let issuer = cluster.client(&cluster.issuer, "issuer");
    let alice = cluster.client(&cluster.executor, "alice");
    let alternate = cluster.client(&cluster.alternate, "alternate");
    issuer
        .call([1; 32], Operation::Control(cluster.app_control(1)))
        .await
        .unwrap();
    let h = alice.acquire(cluster.scope, [2; 32]).await.unwrap();
    let first = proposal(
        &h,
        cluster.phone.public_key(),
        directory_app::principal(*cluster.user.public_key().as_bytes()),
        1,
    );
    let receipt = alice.confirm(first.clone()).await.unwrap();
    assert_eq!(alice.confirm(first.clone()).await.unwrap(), receipt);
    let stale = proposal(
        &alice.head(cluster.scope).await.unwrap(),
        cluster.phone.public_key(),
        first.caller,
        2,
    );
    let fenced = alice.takeover(cluster.scope, 1, [3; 32]).await.unwrap();
    assert_eq!(fenced.epoch, 2);
    assert_eq!(
        alice.confirm(stale.clone()).await,
        Err(CoordinationError::StaleEpoch)
    );
    let moved = alternate.takeover(cluster.scope, 2, [4; 32]).await.unwrap();
    assert_eq!(moved.epoch, 3);
    assert_eq!(
        alice.confirm(stale).await,
        Err(CoordinationError::StaleEpoch)
    );
    assert_eq!(alice.confirm(first.clone()).await.unwrap(), receipt);
    let mut misuse = first.clone();
    misuse.request_hash = [0; 32];
    assert_eq!(
        alice.confirm(misuse).await,
        Err(CoordinationError::RequestConflict)
    );
    assert_eq!(
        alice
            .call(
                [2; 32],
                Operation::Takeover {
                    scope: cluster.scope,
                    expected_epoch: 3
                }
            )
            .await,
        Err(CoordinationError::RequestConflict)
    );
    let leader = cluster.wait_leader(&admin).await;
    cluster.stop(leader);
    let next = cluster.wait_leader(&admin).await;
    assert_ne!(next, leader);
    assert_eq!(
        alternate
            .lookup(cluster.scope, first.request)
            .await
            .unwrap(),
        Some(receipt.clone())
    );
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
    let head = alternate.head(cluster.scope).await.unwrap();
    assert_eq!(head.sequence, 1);
    assert_eq!(head.epoch, 3);
    for id in 1..=3 {
        admin.snapshot(id).await.unwrap();
    }
    for id in 1..=3 {
        cluster.stop(id);
        cluster.spawn(id, false, None);
        cluster.wait_online(id, &admin).await;
    }
    cluster.wait_leader(&admin).await;
    assert_eq!(alternate.head(cluster.scope).await.unwrap(), head);
    assert_eq!(
        alternate
            .lookup(cluster.scope, first.request)
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    let mut invalid = cluster.app_control(2);
    invalid.control.configuration = [0; 16];
    assert!(
        issuer
            .call([7; 32], Operation::Control(invalid))
            .await
            .is_err()
    );
    issuer
        .call([8; 32], Operation::Control(cluster.app_control(2)))
        .await
        .unwrap();
    let revised = alice.acquire(cluster.scope, [9; 32]).await.unwrap();
    assert_eq!(revised.sequence, 1);
    assert_eq!(revised.epoch, 4);
    assert_eq!(alice.confirm(first.clone()).await.unwrap(), receipt);
    let evidence = alice.evidence(&receipt).await.unwrap();
    let phone = cluster.client(&cluster.phone, "retiring-phone");
    phone
        .retire(
            cluster.scope,
            first.request.node,
            first.request.sequence.get(),
        )
        .await
        .unwrap();
    assert_eq!(
        alice.lookup(cluster.scope, first.request).await,
        Err(CoordinationError::Retired)
    );
    assert_eq!(
        alice.confirm(first.clone()).await,
        Err(CoordinationError::Retired)
    );
    assert_eq!(
        phone.verify(&receipt).await,
        Err(CoordinationError::Retired)
    );
    alice.verify(&receipt).await.unwrap();
    assert_eq!(alice.head(cluster.scope).await.unwrap(), revised);
    // A crash during retirement and subsequent snapshot recovery cannot reopen it.
    let leader = cluster.wait_leader(&admin).await;
    cluster.arm(leader, "commit.after_sync");
    let _ = phone
        .retire(
            cluster.scope,
            first.request.node,
            first.request.sequence.get() + 1,
        )
        .await;
    cluster.wait_exit(leader).await;
    cluster.wait_leader(&admin).await;
    cluster.spawn(leader, false, None);
    cluster.wait_online(leader, &admin).await;
    for id in 1..=3 {
        admin.snapshot(id).await.unwrap();
    }
    let mut hole = first.clone();
    hole.request.sequence = AuthorSequence::new(first.request.sequence.get() + 1).unwrap();
    assert_eq!(alice.confirm(hole).await, Err(CoordinationError::Retired));
    assert_eq!(alice.confirm(first).await, Err(CoordinationError::Retired));
    for id in 1..=3 {
        cluster.stop(id);
    }
    let historical = cluster.client(&cluster.phone, "historical");
    historical.retain_evidence(evidence.clone()).await.unwrap();
    historical.verify(&receipt).await.unwrap();
    assert!(
        rocky_coordinator::service::NetworkCoordinator::open(
            cluster.genesis.clone(),
            cluster.genesis.root,
            cluster.genesis.genesis.mesh,
            cluster.phone.clone(),
            cluster.dir.path().join("historical.evidence")
        )
        .is_err()
    );
    drop(historical);
    let historical = cluster.client(&cluster.phone, "historical");
    historical.verify(&receipt).await.unwrap();
    let mut forged = evidence;
    forged.receipt.confirmation.result = [0; 32];
    assert!(historical.retain_evidence(forged).await.is_err());
    assert_eq!(
        alternate.head(cluster.scope).await,
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
    let admin = cluster.client(&cluster.admin, "admin");
    let issuer = cluster.client(&cluster.issuer, "issuer");
    let alice = cluster.client(&cluster.executor, "alice");
    issuer
        .call([1; 32], Operation::Control(cluster.app_control(1)))
        .await
        .unwrap();
    alice.acquire(cluster.scope, [2; 32]).await.unwrap();
    let caller = directory_app::principal(*cluster.user.public_key().as_bytes());
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
        let before = alice.head(cluster.scope).await.unwrap();
        let p = proposal(
            &before,
            cluster.phone.public_key(),
            caller,
            index as u64 + 1,
        );
        cluster.arm(leader, stage);
        let _ = alice.confirm(p.clone()).await;
        cluster.wait_exit(leader).await;
        cluster.wait_leader(&admin).await;
        let receipt = alice.confirm(p.clone()).await.unwrap();
        assert_eq!(receipt.sequence, before.sequence + 1, "{stage}");
        assert_eq!(alice.confirm(p.clone()).await.unwrap(), receipt);
        cluster.spawn(leader, false, None);
        cluster.wait_online(leader, &admin).await;
        assert_eq!(
            alice.lookup(cluster.scope, p.request).await.unwrap(),
            Some(receipt)
        );
    }
    for stage in ["snapshot.before_rename", "snapshot.after_sync"] {
        let leader = cluster.wait_leader(&admin).await;
        let before = alice.head(cluster.scope).await.unwrap();
        cluster.arm(leader, stage);
        let _ = admin.snapshot(leader).await;
        cluster.wait_exit(leader).await;
        cluster.wait_leader(&admin).await;
        cluster.spawn(leader, false, None);
        cluster.wait_online(leader, &admin).await;
        assert_eq!(alice.head(cluster.scope).await.unwrap(), before);
    }
    let leader = cluster.wait_leader(&admin).await;
    let lagging = if leader == 1 { 2 } else { 1 };
    let lagging_applied = admin.status(lagging).await.unwrap().applied.unwrap();
    cluster.stop(lagging);
    // Reads add no log entries; idempotent acknowledgments of an empty prefix do.
    let alice_node = rocky_coordinator::certified::node_id(cluster.executor.public_key());
    for _ in 0..80 {
        Coordinator::retire(&alice, cluster.scope, alice_node, 1)
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
        "crash exercise: 5 confirmation boundaries, 2 snapshot boundaries plus interrupted snapshot transfer; leader disk bytes={} logs={} snapshots={}",
        status.disk_bytes, status.logs, status.snapshots
    );
}
