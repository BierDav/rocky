use crate::support;
use rocky_coordinator::CoordinationError;
use rocky_coordinator::{
    Limits,
    counters::Claim,
    service::{NetworkCoordinator, Operation, Usage},
};
use rocky_core::*;
use rocky_voter::{backup, restore};
use std::sync::Arc;
use support::{fresh_claim, invoices, membership_after_election, public::*, reading};

fn limits() -> Limits {
    Limits {
        tenant_domains: 2,
        tenant_bytes: 1024 * 1024,
        ..Limits::default()
    }
}
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
    (owner, executor)
}
async fn commit(
    client: &NetworkCoordinator,
    scope: Scope,
    claim: Claim,
) -> Result<u64, CoordinationError> {
    client.claim(scope, invoices(), claim).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenants_register_isolated_domains_and_cannot_starve_each_other() {
    let cluster = PublicCluster::start(limits(), 3).await;
    let (a, b) = (Tenant::new("notes"), Tenant::new("ledger"));
    let (sa, sb) = (a.scope(1), b.scope(2));
    let ((a_owner, a_exec), (b_owner, b_exec)) =
        tokio::join!(registered(&cluster, &a, sa), registered(&cluster, &b, sb));
    assert_eq!(commit(&a_exec, sa, fresh_claim(0x5a)).await, Ok(1));
    assert_eq!(commit(&b_exec, sb, fresh_claim(1)).await, Ok(1));

    let b_on_a = b.client(&cluster, &b.executor).await;
    assert_eq!(
        reading(&b_on_a, sa).await,
        Err(CoordinationError::Denied),
        "a tenant cannot read another tenant's counters"
    );
    assert_eq!(
        commit(&b_on_a, sa, fresh_claim(2)).await,
        Err(CoordinationError::Denied),
        "a tenant cannot claim from another tenant's counters"
    );
    assert_eq!(
        b_owner
            .control(b.control(&cluster.genesis, sa, 2, &[&b.executor]), [8; 32])
            .await,
        Err(CoordinationError::Denied),
        "a tenant cannot control another tenant's domain"
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
        Err(CoordinationError::Denied),
        "a tenant cannot register in another tenant's namespace"
    );
    assert_eq!(
        b_owner
            .register(
                b.grant(),
                Some(cluster.admission(&a)),
                b.control(&cluster.genesis, b.scope(4), 1, &[&b.executor]),
                [6; 32],
            )
            .await,
        Err(CoordinationError::Denied),
        "a tenant cannot reuse another tenant's admission"
    );
    let voter_tcp = NetworkCoordinator::open(
        cluster.genesis.clone(),
        cluster.genesis.root,
        cluster.genesis.genesis.mesh,
        a.executor.clone(),
    )
    .unwrap();
    assert_eq!(
        reading(&voter_tcp, sa).await,
        Err(CoordinationError::Unavailable),
        "the voters' TCP carries administration only, never tenants"
    );

    let a_alt = a.client(&cluster, &a.alternate).await;
    assert_eq!(
        commit(&a_alt, sa, fresh_claim(3)).await,
        Ok(2),
        "either trusted node takes the next number"
    );

    a_owner
        .register(
            a.grant(),
            Some(cluster.admission(&a)),
            a.control(&cluster.genesis, a.scope(5), 1, &[&a.executor]),
            [3; 32],
        )
        .await
        .expect("a tenant's second domain fits its quota");
    assert_eq!(
        a_owner
            .register(
                a.grant(),
                Some(cluster.admission(&a)),
                a.control(&cluster.genesis, a.scope(6), 1, &[&a.executor]),
                [2; 32],
            )
            .await,
        Err(CoordinationError::Backpressure),
        "a third open domain exceeds the tenant quota"
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
        .expect("another tenant's quota is unaffected");

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
            tokio::spawn(async move { reading(&client, sa).await.map(|_| ()) })
        })
        .collect();
    assert_eq!(
        commit(&b_exec, sb, fresh_claim(4)).await,
        Ok(2),
        "B commits while A floods its own lanes"
    );
    let (mut refused, mut unanswered) = (0, 0);
    for task in flood {
        match task.await.unwrap() {
            Ok(()) => {}
            Err(CoordinationError::Backpressure) => refused += 1,
            Err(CoordinationError::Unavailable) => unanswered += 1,
            Err(error) => panic!("A's excess must be refused or unanswered: {error:?}"),
        }
    }
    assert!(refused > 0, "A saturated its lanes");
    let admin = cluster.admin();
    let leader = cluster.wait_unfenced_leader(&admin).await;
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
    for id in 1..=3 {
        let log = cluster.log(id);
        assert!(
            !log.contains(&format!("{:?}", [0x5au8; 32])),
            "voter logs never hold request content"
        );
        assert!(
            log.lines().count() < 20,
            "voter logs hold lifecycle events only: {log}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outages_and_restarts_keep_state_and_restore_is_fenced() {
    let mut cluster = PublicCluster::start(limits(), 3).await;
    let admin = cluster.admin();
    let a = Tenant::new("notes");
    let scope = a.scope(1);
    let (_, exec) = registered(&cluster, &a, scope).await;
    commit(&exec, scope, fresh_claim(1)).await.unwrap();

    let leader = cluster.wait_unfenced_leader(&admin).await;
    cluster.stop(leader);
    cluster.wait_unfenced_leader(&admin).await;
    commit(&exec, scope, fresh_claim(2))
        .await
        .expect("the remaining quorum continues after leader loss");
    cluster.spawn(leader, false);
    cluster.wait_online(leader, &admin).await;

    let before = reading(&exec, scope).await.unwrap();
    let pending = fresh_claim(3);
    let survivor = cluster.wait_unfenced_leader(&admin).await;
    let down: Vec<u64> = (1..=3).filter(|id| *id != survivor).collect();
    for id in &down {
        cluster.stop(*id);
    }
    assert_eq!(
        commit(&exec, scope, pending).await,
        Err(CoordinationError::Unavailable),
        "a quorum outage fabricates no success"
    );
    assert_eq!(
        reading(&exec, scope).await,
        Err(CoordinationError::Unavailable),
        "a quorum outage gives no authoritative answer"
    );
    cluster.stop(survivor);
    for id in 1..=3 {
        cluster.spawn(id, false);
    }
    for id in 1..=3 {
        cluster.wait_online(id, &admin).await;
    }
    cluster.wait_unfenced_leader(&admin).await;
    let restarted = reading(&exec, scope).await.unwrap();
    assert!(
        restarted == before || restarted == before + 1,
        "a controlled restart of every voter preserves the counters, with the unanswered claim at most once"
    );
    assert_eq!(
        commit(&exec, scope, pending).await,
        Ok(before + 1),
        "`Unavailable` was unknown, not failure: retrying the claim yields its one number"
    );
    assert_eq!(reading(&exec, scope).await, Ok(before + 1));

    let backups = cluster.dir.path().join("backup-3");
    assert!(
        backup(&cluster.data(3), cluster.genesis.clone(), 3, &backups).is_err(),
        "a running voter cannot be backed up"
    );
    cluster.stop(3);
    let saved = backup(&cluster.data(3), cluster.genesis.clone(), 3, &backups).unwrap();
    cluster.spawn(3, false);
    cluster.wait_online(3, &admin).await;
    commit(&exec, scope, fresh_claim(4)).await.unwrap();
    let latest = reading(&exec, scope).await.unwrap();
    let committed = admin
        .status(cluster.wait_unfenced_leader(&admin).await)
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

    cluster.stop(3);
    assert!(
        restore(&backups, &cluster.genesis, 3, &cluster.data(3)).is_err(),
        "restoring needs an empty directory"
    );
    assert!(
        restore(
            &backups,
            &cluster.genesis,
            2,
            &cluster.dir.path().join("empty")
        )
        .is_err(),
        "restoring needs the same voter"
    );
    remove_before_rolling_back(&admin).await;
    std::fs::remove_dir_all(cluster.data(3)).unwrap();
    let restored = restore(&backups, &cluster.genesis, 3, &cluster.data(3)).unwrap();
    assert_eq!(restored, saved);
    cluster.spawn(3, false);
    cluster.wait_online(3, &admin).await;
    assert!(
        admin.status(3).await.unwrap().fenced,
        "a restored voter is fenced, so its older state cannot answer for the service"
    );
    let leader = cluster.wait_unfenced_leader(&admin).await;
    assert_ne!(leader, 3, "a fenced voter cannot be elected");
    let readmission = membership_after_election(
        &admin,
        [0x41; 32],
        Operation::Membership {
            voters: [1, 2, 3].into(),
            expected: [1, 2].into(),
            expected_revision: 1,
        },
    )
    .await;
    if readmission.is_err() {
        let mut statuses = Vec::new();
        for id in 1..=3 {
            statuses.push(admin.status(id).await);
        }
        panic!(
            "readmission: {readmission:?}; {statuses:?}\n{}\n{}\n{}",
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
    cluster.stop(1);
    cluster.wait_unfenced_leader(&admin).await;
    assert_eq!(
        reading(&exec, scope).await,
        Ok(latest),
        "the readmitted voter 3 serves current state with voter 2 while voter 1 is down"
    );
    commit(&exec, scope, fresh_claim(5)).await.unwrap();
}

async fn remove_before_rolling_back(admin: &NetworkCoordinator) {
    membership_after_election(
        admin,
        [0x40; 32],
        Operation::Membership {
            voters: [1, 2].into(),
            expected: [1, 2, 3].into(),
            expected_revision: 0,
        },
    )
    .await
    .unwrap();
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
    assert_eq!(
        reading(&exec, scope).await,
        Err(CoordinationError::Unavailable),
        "recovering a whole service is an explicit new configuration, never a silent rollback"
    );
    for id in 1..=3 {
        let status = admin.status(id).await.unwrap();
        assert!(status.fenced && status.leader.is_none(), "{status:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owner_closure_ends_the_counters_and_frees_only_open_quota() {
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
    owner
        .close_domain(first, [9; 32])
        .await
        .expect("a retried closure answers the same");
    assert_eq!(
        commit(&executor, first, fresh_claim(1)).await,
        Err(CoordinationError::Denied),
        "a closed domain takes no further claim"
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
