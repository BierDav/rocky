//! A public coordination service: three voter processes with iroh client
//! lanes, two independent application owners, quotas, fairness, outages
//! and fenced restore. See `docs/runtime/public-coordination.md`.
#![cfg(feature = "iroh")]
#[allow(dead_code)]
mod support;
use rocky_coordinator::{
    Limits,
    coordinated::{Action, Result as LogResult},
    service::{NetworkCoordinator, Operation, Outcome, Usage, backup, restore},
};
use rocky_core::*;
use std::sync::Arc;
use support::{append, join, log, membership_after_election, public::*};

fn limits() -> Limits {
    Limits {
        tenant_domains: 2,
        tenant_bytes: 1024 * 1024,
        ..Limits::default()
    }
}
/// The tenant's owner client and its executor, joined to `scope`'s log.
async fn registered(
    cluster: &PublicCluster,
    tenant: &Tenant,
    scope: Scope,
) -> (NetworkCoordinator, NetworkCoordinator) {
    let owner = tenant.client(cluster, &tenant.key).await;
    owner
        .register(
            tenant.grant(),
            Some(cluster.admission(tenant)),
            tenant.control(
                &cluster.genesis,
                scope,
                1,
                &[&tenant.executor, &tenant.alternate],
            ),
            [scope.domain.0[0]; 32],
        )
        .await
        .unwrap();
    let executor = tenant.client(cluster, &tenant.executor).await;
    join(&executor, scope, 1).await;
    (owner, executor)
}
/// Appends and acknowledges `action` as the log's one member; the position
/// it committed at.
async fn commit(
    client: &NetworkCoordinator,
    scope: Scope,
    action: Action,
) -> Result<u64, CoordinationError> {
    let Outcome::Coordinated(LogResult::Assigned(pending)) =
        client.coordinated(log(scope), action).await?
    else {
        return Err(CoordinationError::Corrupt);
    };
    match client
        .coordinated(log(scope), Action::Acknowledge { pending: *pending })
        .await?
    {
        Outcome::Coordinated(LogResult::Committed { position, .. }) => Ok(position),
        _ => Err(CoordinationError::Corrupt),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenants_register_isolated_domains_and_cannot_starve_each_other() {
    let cluster = PublicCluster::start(limits(), 3).await;
    let (a, b) = (Tenant::new("notes"), Tenant::new("ledger"));
    let (sa, sb) = (a.scope(1), b.scope(2));
    let ((a_owner, a_exec), (b_owner, b_exec)) =
        tokio::join!(registered(&cluster, &a, sa), registered(&cluster, &b, sb));
    let first = append(&a_exec, sa, 0x5a, 2).await;
    assert_eq!(commit(&a_exec, sa, first).await, Ok(1));
    let b_first = append(&b_exec, sb, 1, 2).await;
    assert_eq!(commit(&b_exec, sb, b_first).await, Ok(1));

    // Cross-tenant log calls, controls and registrations fail.
    let b_on_a = b.client(&cluster, &b.executor).await;
    assert_eq!(
        b_on_a.coordinated_head(log(sa)).await,
        Err(CoordinationError::Denied)
    );
    let head = a_exec.coordinated_head(log(sa)).await.unwrap();
    assert_eq!(
        b_on_a
            .coordinated(
                log(sa),
                Action::Join {
                    position: head.commit,
                    running_hash: head.running_hash,
                    sequence: 1,
                },
            )
            .await,
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        b_owner
            .control(b.control(&cluster.genesis, sa, 2, &[&b.executor]), [8; 32])
            .await,
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        b_owner
            .register(
                b.grant(),
                Some(cluster.admission(&b)),
                b.control(&cluster.genesis, a.scope(3), 1, &[&b.executor]),
                [7; 32],
            )
            .await,
        Err(CoordinationError::Denied)
    );
    // B cannot reuse A's admission, nor register without one.
    assert_eq!(
        b_owner
            .register(
                b.grant(),
                Some(cluster.admission(&a)),
                b.control(&cluster.genesis, b.scope(4), 1, &[&b.executor]),
                [6; 32],
            )
            .await,
        Err(CoordinationError::Denied)
    );
    // Tenants never use the voters' TCP: it carries administration only.
    let tcp = NetworkCoordinator::open(
        cluster.genesis.clone(),
        cluster.genesis.root,
        cluster.genesis.genesis.mesh,
        a.executor.clone(),
    )
    .unwrap();
    assert_eq!(
        tcp.coordinated_head(log(sa)).await,
        Err(CoordinationError::Unavailable)
    );

    // The owner changes trusted nodes: a node in the in-sync set stays.
    assert_eq!(
        a_owner
            .control(a.control(&cluster.genesis, sa, 2, &[&a.alternate]), [5; 32])
            .await,
        Err(CoordinationError::Backpressure)
    );
    let a_alt = a.client(&cluster, &a.alternate).await;
    join(&a_alt, sa, 1).await;
    let second = append(&a_alt, sa, 2, 2).await;
    let Outcome::Coordinated(LogResult::Assigned(pending)) =
        a_alt.coordinated(log(sa), second).await.unwrap()
    else {
        panic!("assignment")
    };
    // Both members must store it before it commits.
    assert_eq!(
        a_alt
            .coordinated(
                log(sa),
                Action::Acknowledge {
                    pending: (*pending).clone()
                }
            )
            .await
            .unwrap(),
        Outcome::Coordinated(LogResult::Acknowledged)
    );
    assert!(matches!(
        a_exec
            .coordinated(log(sa), Action::Acknowledge { pending: *pending })
            .await
            .unwrap(),
        Outcome::Coordinated(LogResult::Committed { position: 2, .. })
    ));

    // Quota: A's second domain fits, a third open domain does not; B is unaffected.
    a_owner
        .register(
            a.grant(),
            Some(cluster.admission(&a)),
            a.control(&cluster.genesis, a.scope(5), 1, &[&a.executor]),
            [3; 32],
        )
        .await
        .unwrap();
    assert_eq!(
        a_owner
            .register(
                a.grant(),
                Some(cluster.admission(&a)),
                a.control(&cluster.genesis, a.scope(6), 1, &[&a.executor]),
                [2; 32],
            )
            .await,
        Err(CoordinationError::Backpressure)
    );
    assert_eq!(a_owner.usage().await.unwrap().open_domains, 2);
    b_owner
        .register(
            b.grant(),
            Some(cluster.admission(&b)),
            b.control(&cluster.genesis, b.scope(7), 1, &[&b.executor]),
            [1; 32],
        )
        .await
        .unwrap();

    // Fairness: four of A's connections flood its lanes; B commits within
    // its latency budget.
    let (one, two, three, four) = tokio::join!(
        a.client(&cluster, &a.alternate),
        a.client(&cluster, &a.alternate),
        a.client(&cluster, &a.alternate),
        a.client(&cluster, &a.alternate),
    );
    let flooders = [one, two, three, four].map(Arc::new);
    let flood: Vec<_> = (0..64)
        .map(|n| {
            let client = flooders[n % 4].clone();
            tokio::spawn(async move { client.coordinated_head(log(sa)).await.map(|_| ()) })
        })
        .collect();
    let b_second = append(&b_exec, sb, 2, 3).await;
    assert_eq!(commit(&b_exec, sb, b_second).await, Ok(2));
    // A's excess is refused or left without an answer.
    let (mut refused, mut unanswered) = (0, 0);
    for task in flood {
        match task.await.unwrap() {
            Ok(()) => {}
            Err(CoordinationError::Backpressure) => refused += 1,
            Err(CoordinationError::Unavailable) => unanswered += 1,
            Err(error) => panic!("{error:?}"),
        }
    }
    assert!(refused > 0, "A saturated its lanes");
    let admin = cluster.admin();
    let leader = cluster.wait_leader(&admin).await;
    let status = admin.status(leader).await.unwrap();
    assert!(status.metrics.busy > 0);
    assert_eq!(status.tenants, 2);
    assert!(
        status.state_bytes
            >= a_owner.usage().await.unwrap().bytes + b_owner.usage().await.unwrap().bytes
    );
    assert_eq!(
        b_owner.usage().await.unwrap(),
        Usage {
            open_domains: 2,
            closed_domains: 0,
            bytes: b_owner.usage().await.unwrap().bytes,
            registrations_today: 2,
        }
    );
    println!(
        "public service: tenant B committed while A had {refused}/64 requests refused and {unanswered} unanswered; leader state bytes={} disk bytes={} reads={} requests={}",
        status.state_bytes, status.disk_bytes, status.metrics.reads, status.metrics.requests
    );
    // Voter logs hold lifecycle events, never request content.
    for id in 1..=3 {
        let log = cluster.log(id);
        assert!(!log.contains(&format!("{:?}", [0x5au8; 32])));
        assert!(log.lines().count() < 20, "{log}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outages_and_restarts_keep_state_and_restore_is_fenced() {
    let mut cluster = PublicCluster::start(limits(), 3).await;
    let admin = cluster.admin();
    let a = Tenant::new("notes");
    let scope = a.scope(1);
    let (_, exec) = registered(&cluster, &a, scope).await;
    let first = append(&exec, scope, 1, 2).await;
    commit(&exec, scope, first).await.unwrap();

    // Leader loss: the remaining quorum continues.
    let leader = cluster.wait_leader(&admin).await;
    cluster.stop(leader);
    cluster.wait_leader(&admin).await;
    let second = append(&exec, scope, 2, 3).await;
    commit(&exec, scope, second).await.unwrap();
    cluster.spawn(leader, false);
    cluster.wait_online(leader, &admin).await;

    // Quorum outage: no authoritative answer, no fabricated success.
    let before = exec.coordinated_head(log(scope)).await.unwrap();
    let pending = append(&exec, scope, 3, 4).await;
    let survivor = cluster.wait_leader(&admin).await;
    let down: Vec<u64> = (1..=3).filter(|id| *id != survivor).collect();
    for id in &down {
        cluster.stop(*id);
    }
    assert_eq!(
        exec.coordinated(log(scope), pending.clone()).await,
        Err(CoordinationError::Unavailable)
    );
    assert_eq!(
        exec.coordinated_head(log(scope)).await,
        Err(CoordinationError::Unavailable)
    );
    // Controlled restart of every voter preserves the log.
    cluster.stop(survivor);
    for id in 1..=3 {
        cluster.spawn(id, false);
    }
    for id in 1..=3 {
        cluster.wait_online(id, &admin).await;
    }
    cluster.wait_leader(&admin).await;
    // `Unavailable` was unknown, not failure: the survivor's log may hold
    // the append. Retrying the same append yields its one assignment.
    assert_eq!(
        exec.coordinated_head(log(scope)).await.unwrap().commit,
        before.commit
    );
    assert_eq!(commit(&exec, scope, pending).await, Ok(before.commit + 1));

    // A running voter cannot be backed up; a stopped one can.
    let backups = cluster.dir.path().join("backup-3");
    assert!(backup(&cluster.data(3), cluster.genesis.clone(), 3, &backups).is_err());
    cluster.stop(3);
    let saved = backup(&cluster.data(3), cluster.genesis.clone(), 3, &backups).unwrap();
    cluster.spawn(3, false);
    cluster.wait_online(3, &admin).await;
    let fourth = append(&exec, scope, 4, 5).await;
    commit(&exec, scope, fourth).await.unwrap();
    let latest = exec.coordinated_head(log(scope)).await.unwrap();
    let committed = admin
        .status(cluster.wait_leader(&admin).await)
        .await
        .unwrap()
        .applied
        .unwrap();
    assert!(committed > saved.index, "the restored backup must be stale");
    cluster
        .wait("backup voter has newer committed state", async || {
            admin
                .status(3)
                .await
                .is_ok_and(|s| s.applied.is_some_and(|index| index >= committed))
        })
        .await;

    // Restoring needs an empty directory and the same voter.
    cluster.stop(3);
    assert!(restore(&backups, &cluster.genesis, 3, &cluster.data(3)).is_err());
    assert!(
        restore(
            &backups,
            &cluster.genesis,
            2,
            &cluster.dir.path().join("empty")
        )
        .is_err()
    );
    // Remove the stopped voter before rolling back its log. The live leader
    // must discard its replication progress before the same ID returns.
    membership_after_election(
        &admin,
        [0x40; 32],
        Operation::Membership {
            voters: [1, 2].into(),
            expected: [1, 2, 3].into(),
            expected_revision: 0,
        },
    )
    .await
    .unwrap();
    std::fs::remove_dir_all(cluster.data(3)).unwrap();
    let restored = restore(&backups, &cluster.genesis, 3, &cluster.data(3)).unwrap();
    assert_eq!(restored, saved);
    cluster.spawn(3, false);
    cluster.wait_online(3, &admin).await;
    // Fenced: it serves no client and holds no vote, so its older state
    // cannot answer for the service or elect a leader.
    assert!(admin.status(3).await.unwrap().fenced);
    let leader = cluster.wait_leader(&admin).await;
    assert_ne!(leader, 3);
    // The membership authority re-adds it as a learner before promotion.
    let result = membership_after_election(
        &admin,
        [0x41; 32],
        Operation::Membership {
            voters: [1, 2, 3].into(),
            expected: [1, 2].into(),
            expected_revision: 1,
        },
    )
    .await;
    if result.is_err() {
        let mut statuses = Vec::new();
        for id in 1..=3 {
            statuses.push(admin.status(id).await);
        }
        panic!(
            "readmission: {result:?}; {statuses:?}\n{}\n{}\n{}",
            cluster.log(1),
            cluster.log(2),
            cluster.log(3),
        );
    }
    cluster
        .wait("restored voter re-added", async || {
            admin.status(3).await.is_ok_and(|s| !s.fenced)
        })
        .await;
    // Voter 3 now votes with current state: with voter 1 down, 2 and 3 serve.
    cluster.stop(1);
    cluster.wait_leader(&admin).await;
    assert_eq!(exec.coordinated_head(log(scope)).await.unwrap(), latest);
    let fifth = append(&exec, scope, 5, 6).await;
    commit(&exec, scope, fifth).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_completely_restored_service_stays_fenced() {
    let mut cluster = PublicCluster::start(limits(), 3).await;
    let admin = cluster.admin();
    let a = Tenant::new("notes");
    let scope = a.scope(1);
    let (_, exec) = registered(&cluster, &a, scope).await;
    for id in 1..=3 {
        cluster.stop(id);
    }
    for id in 1..=3 {
        let out = cluster.dir.path().join(format!("backup-{id}"));
        backup(&cluster.data(id), cluster.genesis.clone(), id, &out).unwrap();
        std::fs::remove_dir_all(cluster.data(id)).unwrap();
        restore(&out, &cluster.genesis, id, &cluster.data(id)).unwrap();
        cluster.spawn(id, false);
    }
    for id in 1..=3 {
        cluster.wait_online(id, &admin).await;
    }
    // Nobody may vote, so nothing answers: recovery of a whole service is an
    // explicit new configuration, never a silent rollback.
    assert_eq!(
        exec.coordinated_head(log(scope)).await,
        Err(CoordinationError::Unavailable)
    );
    for id in 1..=3 {
        let status = admin.status(id).await.unwrap();
        assert!(status.fenced && status.leader.is_none(), "{status:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owner_closure_ends_the_log_and_frees_only_open_quota() {
    let cluster = PublicCluster::start(limits(), 3).await;
    let tenant = Tenant::new("closure");
    let first = tenant.scope(1);
    let second = tenant.scope(2);
    let third = tenant.scope(3);
    let (owner, executor) = registered(&cluster, &tenant, first).await;
    owner
        .register(
            tenant.grant(),
            Some(cluster.admission(&tenant)),
            tenant.control(&cluster.genesis, second, 1, &[&tenant.executor]),
            [2; 32],
        )
        .await
        .unwrap();
    assert_eq!(owner.usage().await.unwrap().open_domains, 2);
    assert_eq!(
        executor.close_domain(first, [8; 32]).await,
        Err(CoordinationError::Denied)
    );
    assert_eq!(owner.usage().await.unwrap().closed_domains, 0);
    owner.close_domain(first, [9; 32]).await.unwrap();
    // A retry answers the same; the closed log takes no further entry.
    owner.close_domain(first, [9; 32]).await.unwrap();
    let action = append(&executor, first, 1, 2).await;
    assert_eq!(
        executor.coordinated(log(first), action).await,
        Err(CoordinationError::Denied)
    );
    let usage = owner.usage().await.unwrap();
    assert_eq!((usage.open_domains, usage.closed_domains), (1, 1));
    assert!(usage.bytes > 0);
    owner
        .register(
            tenant.grant(),
            Some(cluster.admission(&tenant)),
            tenant.control(&cluster.genesis, third, 1, &[&tenant.executor]),
            [3; 32],
        )
        .await
        .unwrap();
    assert_eq!(owner.usage().await.unwrap().open_domains, 2);
    assert_eq!(
        owner
            .register(
                tenant.grant(),
                Some(cluster.admission(&tenant)),
                tenant.control(&cluster.genesis, first, 1, &[&tenant.executor]),
                [4; 32]
            )
            .await,
        Err(CoordinationError::Denied)
    );
}
