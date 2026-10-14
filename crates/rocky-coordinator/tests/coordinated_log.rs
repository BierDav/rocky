#[allow(dead_code)]
mod support;
use rocky_coordinator::{
    coordinated::{Action, Result as LogResult},
    service::{Operation, Outcome},
};
use rocky_core::*;
use support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coordinated_log_survives_leader_change_and_deadline_without_double_commit() {
    let mut cluster = Cluster::start().await;
    let admin = cluster.client(&cluster.admin, "log-admin");
    let issuer = cluster.client(&cluster.issuer, "log-owner");
    let first = cluster.client(&cluster.executor, "log-first");
    let second = cluster.client(&cluster.alternate, "log-second");
    issuer
        .call([1; 32], Operation::Control(cluster.app_control(1)))
        .await
        .unwrap();
    for client in [&first, &second] {
        let head = client.coordinated_head(cluster.scope).await.unwrap();
        client
            .coordinated(
                cluster.scope,
                Action::Join {
                    position: head.commit,
                    running_hash: head.running_hash,
                    sequence: 1,
                },
            )
            .await
            .unwrap();
    }
    let head = first.coordinated_head(cluster.scope).await.unwrap();
    let append = Action::Append {
        expected: 0,
        epoch: head.epoch,
        hash: [7; 32],
        deadline: timestamp() + 30,
        sequence: 2,
    };
    let (a, b) = tokio::join!(
        first.coordinated(cluster.scope, append.clone()),
        second.coordinated(cluster.scope, append)
    );
    assert!(matches!(
        (&a, &b),
        (
            Ok(Outcome::Coordinated(LogResult::Assigned(_))),
            Err(CoordinationError::StaleHead)
        ) | (
            Err(CoordinationError::StaleHead),
            Ok(Outcome::Coordinated(LogResult::Assigned(_)))
        )
    ));
    let pending = first
        .coordinated_head(cluster.scope)
        .await
        .unwrap()
        .pending
        .unwrap();
    assert_eq!(
        first
            .coordinated(
                cluster.scope,
                Action::Acknowledge {
                    pending: pending.clone()
                }
            )
            .await
            .unwrap(),
        Outcome::Coordinated(LogResult::Acknowledged)
    );
    let leader = cluster.wait_leader(&admin).await;
    cluster.stop(leader);
    cluster.wait_leader(&admin).await;
    assert_eq!(
        second
            .coordinated(
                cluster.scope,
                Action::Acknowledge {
                    pending: pending.clone()
                }
            )
            .await
            .unwrap(),
        Outcome::Coordinated(LogResult::Acknowledged)
    );
    assert_eq!(
        first.coordinated_head(cluster.scope).await.unwrap().commit,
        0
    );
    assert!(matches!(
        first
            .coordinated(
                cluster.scope,
                Action::Acknowledge {
                    pending: pending.clone()
                }
            )
            .await
            .unwrap(),
        Outcome::Coordinated(LogResult::Committed { position: 1, .. })
    ));
    assert_eq!(
        second
            .coordinated(cluster.scope, Action::Acknowledge { pending })
            .await,
        Err(CoordinationError::StaleHead)
    );
    cluster.spawn(leader, false, None);
    cluster.wait_online(leader, &admin).await;
    let leader = cluster.wait_leader(&admin).await;
    let head = first.coordinated_head(cluster.scope).await.unwrap();
    let before = admin.status(leader).await.unwrap();
    let assigned = first
        .coordinated(
            cluster.scope,
            Action::Append {
                expected: 1,
                epoch: head.epoch,
                hash: [8; 32],
                deadline: timestamp() + 30,
                sequence: 3,
            },
        )
        .await
        .unwrap();
    let Outcome::Coordinated(LogResult::Assigned(pending)) = assigned else {
        panic!("assignment")
    };
    let pending = *pending;
    first
        .coordinated(
            cluster.scope,
            Action::Acknowledge {
                pending: pending.clone(),
            },
        )
        .await
        .unwrap();
    second
        .coordinated(cluster.scope, Action::Acknowledge { pending })
        .await
        .unwrap();
    let after = admin.status(leader).await.unwrap();
    assert_eq!(after.logs - before.logs, 2);
    println!(
        "two-node successful write: 3 client calls, 2 Raft entries; leader admitted-client delta {}",
        after.metrics.requests - before.metrics.requests
    );
    // Drive the production lazy timer; no application node sends a resolving call.
    let head = first.coordinated_head(cluster.scope).await.unwrap();
    let assigned = first
        .coordinated(
            cluster.scope,
            Action::Append {
                expected: 2,
                epoch: head.epoch,
                hash: [9; 32],
                deadline: timestamp() + 2,
                sequence: 4,
            },
        )
        .await
        .unwrap();
    let Outcome::Coordinated(LogResult::Assigned(pending)) = assigned else {
        panic!("assignment")
    };
    let pending = *pending;
    first
        .coordinated(cluster.scope, Action::Acknowledge { pending })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let head = first.coordinated_head(cluster.scope).await.unwrap();
            if head.pending.is_none() {
                assert_eq!(head.commit, 3);
                assert_eq!(head.members.len(), 1);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // A committed resolution survives snapshots and all process restarts.
    for id in 1..=3 {
        admin.snapshot(id).await.unwrap();
    }
    for id in 1..=3 {
        cluster.stop(id);
        cluster.spawn(id, false, None);
        cluster.wait_online(id, &admin).await;
    }
    cluster.wait_leader(&admin).await;
    let head = first.coordinated_head(cluster.scope).await.unwrap();
    assert_eq!(head.commit, 3);
    assert_eq!(head.hashes.len(), 3);
}
