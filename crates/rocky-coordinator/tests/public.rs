//! A public coordination service: three voter processes with iroh client
//! lanes, two independent application owners, quotas, fairness, outages
//! and fenced restore. See `docs/runtime/public-coordination.md`.
#![cfg(feature = "iroh")]
#[allow(dead_code)]
mod support;
use rocky_coordinator::{
    Limits,
    certified::node_id,
    service::{NetworkCoordinator, Operation, Usage, backup, restore},
};
use rocky_core::*;
use std::{sync::Arc, time::Duration};
use support::{membership_after_election, proposal, public::*};

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
    let owner = tenant.client(cluster, &tenant.key, "owner").await;
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
    let executor = tenant.client(cluster, &tenant.executor, "executor").await;
    executor.acquire(scope, [1; 32]).await.unwrap();
    (owner, executor)
}
async fn confirm(
    client: &NetworkCoordinator,
    tenant: &Tenant,
    scope: Scope,
    n: u64,
) -> Result<StrongReceipt, CoordinationError> {
    let head = client.head(scope).await?;
    let mut p = proposal(&head, tenant.phone.public_key(), tenant.principal(), n);
    p.scope = scope;
    client.confirm(p).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenants_register_isolated_domains_and_cannot_starve_each_other() {
    let cluster = PublicCluster::start(limits(), 3).await;
    let (a, b) = (Tenant::new("notes"), Tenant::new("ledger"));
    let (sa, sb) = (a.scope(1), b.scope(2));
    let (a_owner, a_exec) = registered(&cluster, &a, sa).await;
    let (b_owner, b_exec) = registered(&cluster, &b, sb).await;
    let first = confirm(&a_exec, &a, sa, 1).await.unwrap();
    confirm(&b_exec, &b, sb, 1).await.unwrap();
    // Receipt evidence verifies offline for a tenant's own mesh.
    a_exec
        .evidence(&first)
        .await
        .unwrap()
        .verify(&cluster.genesis)
        .unwrap();

    // Cross-tenant acquire, control, lookup, invocation and registration fail.
    let b_on_a = b.client(&cluster, &b.executor, "intruder").await;
    assert_eq!(
        b_on_a.acquire(sa, [9; 32]).await,
        Err(CoordinationError::Denied)
    );
    assert_eq!(b_on_a.head(sa).await, Err(CoordinationError::Denied));
    assert_eq!(
        b_on_a.lookup(sa, first.confirmation.request).await,
        Err(CoordinationError::Denied)
    );
    let mut stolen = proposal(
        &a_exec.head(sa).await.unwrap(),
        b.phone.public_key(),
        b.principal(),
        2,
    );
    stolen.scope = sa;
    assert_eq!(b_on_a.confirm(stolen).await, Err(CoordinationError::Denied));
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
        cluster.dir.path().join("tcp.evidence"),
    )
    .unwrap();
    assert_eq!(tcp.head(sa).await, Err(CoordinationError::Unavailable));

    // The owner changes executors: the head stays, the old executor is fenced.
    let before = a_exec.head(sa).await.unwrap();
    a_owner
        .control(a.control(&cluster.genesis, sa, 2, &[&a.alternate]), [5; 32])
        .await
        .unwrap();
    assert_eq!(
        confirm(&a_exec, &a, sa, 2).await,
        Err(CoordinationError::Denied)
    );
    let a_alt = a.client(&cluster, &a.alternate, "alternate").await;
    let acquired = a_alt.acquire(sa, [4; 32]).await.unwrap();
    assert_eq!(
        (acquired.sequence, acquired.head),
        (before.sequence, before.head)
    );
    assert!(acquired.epoch > before.epoch);
    confirm(&a_alt, &a, sa, 2).await.unwrap();

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

    // Fairness: four of A's connections flood its lanes; B confirms within
    // its latency budget.
    let node = node_id(a.alternate.public_key());
    let mut flooders = Vec::new();
    for n in 0..4 {
        flooders.push(Arc::new(
            a.client(&cluster, &a.alternate, &format!("flood-{n}"))
                .await,
        ));
    }
    let flood: Vec<_> = (0..64)
        .map(|n| {
            let client = flooders[n % 4].clone();
            tokio::spawn(async move { Coordinator::retire(&*client, sa, node, 1).await })
        })
        .collect();
    let started = tokio::time::Instant::now();
    confirm(&b_exec, &b, sb, 2).await.unwrap();
    let latency = started.elapsed();
    // A's excess is refused or left without an answer, never confirmed for B.
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
    assert!(latency < Duration::from_secs(3), "{latency:?}");
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
        "public service: tenant B confirmed in {latency:?} while A had {refused}/64 requests refused and {unanswered} unanswered; leader state bytes={} disk bytes={} reads={} requests={}",
        status.state_bytes, status.disk_bytes, status.metrics.reads, status.metrics.requests
    );
    // Voter logs hold lifecycle events, never request content.
    for id in 1..=3 {
        let log = cluster.log(id);
        assert!(!log.contains(&format!("{:?}", first.confirmation.request_hash)));
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
    confirm(&exec, &a, scope, 1).await.unwrap();

    // Leader loss: the remaining quorum continues.
    let leader = cluster.wait_leader(&admin).await;
    cluster.stop(leader);
    cluster.wait_leader(&admin).await;
    confirm(&exec, &a, scope, 2).await.unwrap();
    cluster.spawn(leader, false);
    cluster.wait_online(leader, &admin).await;

    // Quorum outage: no authoritative answer, no fabricated success.
    let before = exec.head(scope).await.unwrap();
    let survivor = cluster.wait_leader(&admin).await;
    let down: Vec<u64> = (1..=3).filter(|id| *id != survivor).collect();
    for id in &down {
        cluster.stop(*id);
    }
    let mut pending = proposal(&before, a.phone.public_key(), a.principal(), 3);
    pending.scope = scope;
    assert_eq!(
        exec.confirm(pending.clone()).await,
        Err(CoordinationError::Unavailable)
    );
    assert_eq!(
        exec.lookup(scope, pending.request).await,
        Err(CoordinationError::Unavailable)
    );
    // Controlled restart of every voter preserves the head.
    cluster.stop(survivor);
    for id in 1..=3 {
        cluster.spawn(id, false);
    }
    for id in 1..=3 {
        cluster.wait_online(id, &admin).await;
    }
    cluster.wait_leader(&admin).await;
    // `Unavailable` was unknown, not failure: the survivor's log may hold
    // the proposal and a restored quorum may commit it.
    let after = exec.head(scope).await.unwrap();
    assert!(
        after == before || after.sequence == before.sequence + 1,
        "{after:?}"
    );
    // Retrying the same identity yields its one outcome.
    let receipt = exec.confirm(pending.clone()).await.unwrap();
    assert_eq!(receipt.sequence, before.sequence + 1);
    assert_eq!(exec.head(scope).await.unwrap().sequence, receipt.sequence);
    assert_eq!(
        exec.lookup(scope, pending.request).await.unwrap(),
        Some(receipt)
    );

    // A running voter cannot be backed up; a stopped one can.
    let backups = cluster.dir.path().join("backup-3");
    assert!(backup(&cluster.data(3), cluster.genesis.clone(), 3, &backups).is_err());
    cluster.stop(3);
    let saved = backup(&cluster.data(3), cluster.genesis.clone(), 3, &backups).unwrap();
    cluster.spawn(3, false);
    cluster.wait_online(3, &admin).await;
    confirm(&exec, &a, scope, 4).await.unwrap();
    let latest = exec.head(scope).await.unwrap();
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
    assert_eq!(exec.head(scope).await.unwrap(), latest);
    confirm(&exec, &a, scope, 5).await.unwrap();
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
    assert_eq!(exec.head(scope).await, Err(CoordinationError::Unavailable));
    for id in 1..=3 {
        let status = admin.status(id).await.unwrap();
        assert!(status.fenced && status.leader.is_none(), "{status:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owner_closure_drops_only_its_cached_head_and_frees_only_open_quota() {
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
    let head = owner.head(first).await.unwrap();
    owner.head(second).await.unwrap();
    assert_eq!(owner.usage().await.unwrap().open_domains, 2);
    assert_eq!(
        executor.close_domain(head.clone(), [8; 32]).await,
        Err(CoordinationError::Denied)
    );
    assert_eq!(owner.usage().await.unwrap().closed_domains, 0);
    let closed = owner.close_domain(head.clone(), [9; 32]).await.unwrap();
    assert!(closed.epoch > head.epoch);
    assert_eq!(owner.close_domain(head, [9; 32]).await.unwrap(), closed);
    drop(owner);
    let owner = tenant.client(&cluster, &tenant.key, "owner").await;
    #[derive(serde::Deserialize)]
    struct Heads {
        heads: Vec<StrongHead>,
    }
    // The reopened client is backed by the same durable evidence file.
    let bytes = std::fs::read(cluster.dir.path().join("closure-owner.evidence")).unwrap();
    let cache: Heads = p2panda_core::cbor::decode_cbor(bytes.as_slice()).unwrap();
    assert_eq!(
        cache
            .heads
            .iter()
            .map(|head| head.scope)
            .collect::<Vec<_>>(),
        vec![second]
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
