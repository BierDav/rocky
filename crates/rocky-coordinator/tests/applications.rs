#[allow(dead_code)]
mod support;
use directory_app::{Directory, DirectoryApp, Ownership, Transition};
use inventory_app::{InventoryApp, RESERVE};
use rocky_coordinator::{certified::node_id, service::Operation};
use rocky_core::*;
use rocky_runtime::{
    CommandHandle, CommandState, DomainInstallation, Identity, MutationHandle, Runtime,
    RuntimeConfig,
};
use support::*;

fn app(cluster: &Cluster) -> DirectoryApp {
    DirectoryApp {
        publisher: *cluster.directory_key.public_key().as_bytes(),
        domain: cluster.directory_scope.domain,
        bootstrap_registrar: directory_app::principal(*cluster.admin.public_key().as_bytes()),
        initial_ownership: vec![Ownership {
            scope: cluster.scope,
            owner: directory_app::principal(*cluster.user.public_key().as_bytes()),
        }],
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directory_and_alice_share_metadata_voters_and_quorum_loss_keeps_local_work() {
    let mut cluster = Cluster::start().await;
    let admission_time = timestamp();
    let directory = app(&cluster);
    assert_eq!(
        directory.manifest().hash(),
        cluster.genesis.genesis.directory_definition
    );
    let registrar = directory.bootstrap_registrar;
    let caller = directory_app::principal(*cluster.user.public_key().as_bytes());
    let directory_node = node_id(cluster.directory_key.public_key());
    let alice_node = node_id(cluster.executor.public_key());
    let phone_node = node_id(cluster.phone.public_key());
    let dirclient = cluster.client(&cluster.directory_key, "directory");
    let alice = cluster.client(&cluster.executor, "alice");
    let issuer = cluster.client(&cluster.issuer, "issuer");
    dirclient
        .acquire(cluster.directory_scope, [1; 32])
        .await
        .unwrap();
    let directory_runtime = Runtime::open(
        cluster.dir.path().join("directory.db"),
        vec![DomainInstallation {
            app: directory.clone(),
            policy: directory_app::Policy {
                scope: cluster.directory_scope,
                caller: registrar,
                reference: [9; 32],
                permitted_callers: vec![caller],
            },
            identity: Identity {
                scope: cluster.directory_scope,
                node: directory_node,
                caller: registrar,
            },
            config: RuntimeConfig {
                authors: vec![
                    (directory_node, registrar),
                    (alice_node, caller),
                    (phone_node, caller),
                ],
                delivery_targets: vec![],
                full_nodes: Vec::new(),
            },
        }],
    )
    .await
    .unwrap();
    let replica = directory_runtime.domain(cluster.directory_scope).unwrap();
    replica
        .submit_command_with_arguments(
            CommandHandle([1; 32]),
            directory_app::COMMAND,
            serde_json::to_vec(&Transition::RegisterUser {
                key: *cluster.user.public_key().as_bytes(),
            })
            .unwrap(),
            [9; 32],
        )
        .unwrap()
        .committed()
        .await
        .unwrap();
    replica.execute_pending(&dirclient).await.unwrap();
    let result = replica
        .confirmed_publications()
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        Directory::decode(&result.package.rows).unwrap().users[0].principal,
        caller
    );
    let phone_runtime = Runtime::open(
        cluster.dir.path().join("directory-client.db"),
        vec![DomainInstallation {
            app: directory.clone(),
            policy: directory_app::Policy {
                scope: cluster.directory_scope,
                caller,
                reference: [9; 32],
                permitted_callers: vec![],
            },
            identity: Identity {
                scope: cluster.directory_scope,
                node: alice_node,
                caller,
            },
            config: RuntimeConfig {
                authors: vec![
                    (directory_node, registrar),
                    (alice_node, caller),
                    (phone_node, caller),
                ],
                delivery_targets: vec![],
                full_nodes: Vec::new(),
            },
        }],
    )
    .await
    .unwrap();
    let alice_registration = phone_runtime
        .domain(cluster.directory_scope)
        .unwrap()
        .submit_command_with_arguments(
            CommandHandle([10; 32]),
            directory_app::COMMAND,
            serde_json::to_vec(&Transition::RegisterNode {
                key: *cluster.executor.public_key().as_bytes(),
                owner: caller,
                deployment: None,
                time: admission_time,
            })
            .unwrap(),
            [9; 32],
        )
        .unwrap()
        .committed()
        .await
        .unwrap();
    assert!(rocky_secure::SignedCommandRequest::sign(&alice_registration, &cluster.phone).is_err());
    replica
        .receive_request(
            rocky_secure::SignedCommandRequest::sign(&alice_registration, &cluster.executor)
                .unwrap()
                .admitted()
                .unwrap(),
        )
        .await
        .unwrap();
    replica.execute_pending(&dirclient).await.unwrap();
    let registration = phone_runtime
        .domain(cluster.directory_scope)
        .unwrap()
        .submit_command_with_arguments(
            CommandHandle([2; 32]),
            directory_app::COMMAND,
            serde_json::to_vec(&Transition::RegisterNode {
                key: *cluster.phone.public_key().as_bytes(),
                owner: caller,
                deployment: None,
                time: admission_time,
            })
            .unwrap(),
            [9; 32],
        )
        .unwrap()
        .committed()
        .await
        .unwrap();
    replica
        .receive_request(
            rocky_secure::SignedCommandRequest::sign(&registration, &cluster.executor)
                .unwrap()
                .admitted()
                .unwrap(),
        )
        .await
        .unwrap();
    replica.execute_pending(&dirclient).await.unwrap();
    let results = replica.confirmed_publications().await.unwrap();
    let last = results.iter().max_by_key(|r| r.receipt.sequence).unwrap();
    let registry = Directory::decode(&last.package.rows).unwrap();
    assert!(registry.registered_node(*cluster.phone.public_key().as_bytes()));
    assert!(registry.registered_node(*cluster.executor.public_key().as_bytes()));
    assert_ne!(cluster.phone.public_key(), cluster.executor.public_key());
    // Client-only directory credentials cannot acquire execution or introduce app controls.
    let phone = cluster.client(&cluster.phone, "phone");
    assert_eq!(
        phone.acquire(cluster.directory_scope, [3; 32]).await,
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        phone
            .call([4; 32], Operation::Control(cluster.app_control(1)))
            .await,
        Err(CoordinationError::Denied)
    );
    issuer
        .call([5; 32], Operation::Control(cluster.app_control(1)))
        .await
        .unwrap();
    alice.acquire(cluster.scope, [6; 32]).await.unwrap();
    let inventory = Runtime::open(
        cluster.dir.path().join("alice.db"),
        vec![DomainInstallation {
            app: InventoryApp,
            policy: inventory_app::Policy {
                scope: cluster.scope,
                caller,
                reference: [9; 32],
                note_prefix: None,
            },
            identity: Identity {
                scope: cluster.scope,
                node: alice_node,
                caller,
            },
            config: RuntimeConfig {
                authors: vec![(alice_node, caller)],
                delivery_targets: vec![],
                full_nodes: Vec::new(),
            },
        }],
    )
    .await
    .unwrap();
    let inventory_replica = inventory.domain(cluster.scope).unwrap();
    inventory_replica
        .submit_command_with_arguments(
            CommandHandle([7; 32]),
            RESERVE,
            7u64.to_le_bytes().to_vec(),
            [9; 32],
        )
        .unwrap()
        .committed()
        .await
        .unwrap();
    inventory_replica.execute_pending(&alice).await.unwrap();
    assert_eq!(
        inventory_app::Reservation::decode(
            &inventory_replica.confirmed_publications().await.unwrap()[0]
                .package
                .rows
        )
        .unwrap()
        .remaining,
        3
    );
    cluster.stop(2);
    cluster.stop(3);
    inventory_replica
        .submit_command_with_arguments(
            CommandHandle([8; 32]),
            RESERVE,
            1u64.to_le_bytes().to_vec(),
            [9; 32],
        )
        .unwrap()
        .committed()
        .await
        .unwrap();
    assert!(inventory_replica.execute_pending(&alice).await.is_err());
    assert!(
        inventory_replica
            .strong_status()
            .await
            .unwrap()
            .commands
            .iter()
            .any(|c| matches!(
                c.state,
                CommandState::Queued | CommandState::OutcomeUnknown | CommandState::Prepared
            ))
    );
    let phone_inventory = Runtime::open(
        cluster.dir.path().join("phone-inventory.db"),
        vec![DomainInstallation {
            app: InventoryApp,
            policy: inventory_app::Policy {
                scope: cluster.scope,
                caller,
                reference: [9; 32],
                note_prefix: None,
            },
            identity: Identity {
                scope: cluster.scope,
                node: phone_node,
                caller,
            },
            config: RuntimeConfig {
                authors: vec![(alice_node, caller), (phone_node, caller)],
                delivery_targets: vec![],
                full_nodes: Vec::new(),
            },
        }],
    )
    .await
    .unwrap();
    let historical = inventory_replica
        .confirmed_publications()
        .await
        .unwrap()
        .pop()
        .unwrap();
    phone
        .retain_evidence(alice.evidence(&historical.receipt).await.unwrap())
        .await
        .unwrap();
    let phone_replica = phone_inventory.domain(cluster.scope).unwrap();
    phone_replica
        .receive_result(
            historical.package.encode().unwrap(),
            historical.receipt.encode(),
            &phone,
        )
        .await
        .unwrap();
    assert_eq!(phone_replica.strong_status().await.unwrap().sequence, 1);
    inventory_replica
        .mutate_scoped(
            MutationHandle([9; 32]),
            None,
            b"permitted offline edit".to_vec(),
            cluster.scope,
        )
        .unwrap()
        .committed()
        .await
        .unwrap();
    assert_eq!(inventory_replica.query().await.unwrap().rows.len(), 1);
    cluster.spawn(2, false, None);
    cluster.spawn(3, false, None);
    let admin = cluster.client(&cluster.admin, "admin");
    cluster.wait_online(2, &admin).await;
    cluster.wait_online(3, &admin).await;
    cluster.wait_leader(&admin).await;
    inventory_replica.execute_pending(&alice).await.unwrap();
    assert_eq!(inventory_replica.strong_status().await.unwrap().sequence, 2);
    let deletion = phone_runtime
        .domain(cluster.directory_scope)
        .unwrap()
        .submit_command_with_arguments(
            CommandHandle([11; 32]),
            directory_app::COMMAND,
            serde_json::to_vec(&Transition::DeleteUser {
                disposition: vec![(cluster.scope, directory_app::Disposition::Retire)],
            })
            .unwrap(),
            [9; 32],
        )
        .unwrap()
        .committed()
        .await
        .unwrap();
    replica
        .receive_request(
            rocky_secure::SignedCommandRequest::sign(&deletion, &cluster.executor)
                .unwrap()
                .admitted()
                .unwrap(),
        )
        .await
        .unwrap();
    replica.execute_pending(&dirclient).await.unwrap();
    let publications = replica.confirmed_publications().await.unwrap();
    let deleted = Directory::decode(
        &publications
            .iter()
            .max_by_key(|p| p.receipt.sequence)
            .unwrap()
            .package
            .rows,
    )
    .unwrap();
    assert!(!deleted.registered_node(*cluster.phone.public_key().as_bytes()));
    assert!(!deleted.registered_node(*cluster.executor.public_key().as_bytes()));
    let status = admin.status(1).await.unwrap();
    println!(
        "shared service: directory sequence=4, Alice sequence=2; voter disk bytes={} logs={} sessions={} retained responses={}",
        status.disk_bytes, status.logs, status.sessions, status.entries
    );
    phone_inventory.close().await.unwrap();
    phone_runtime.close().await.unwrap();
    directory_runtime.close().await.unwrap();
    inventory.close().await.unwrap();
    // The directory projection and command predecessor survive app process restart.
    let reopened = Runtime::open(
        cluster.dir.path().join("directory.db"),
        vec![DomainInstallation {
            app: directory,
            policy: directory_app::Policy {
                scope: cluster.directory_scope,
                caller: registrar,
                reference: [9; 32],
                permitted_callers: vec![caller],
            },
            identity: Identity {
                scope: cluster.directory_scope,
                node: directory_node,
                caller: registrar,
            },
            config: RuntimeConfig {
                authors: vec![
                    (directory_node, registrar),
                    (alice_node, caller),
                    (phone_node, caller),
                ],
                delivery_targets: vec![],
                full_nodes: Vec::new(),
            },
        }],
    )
    .await
    .unwrap();
    assert_eq!(
        reopened
            .domain(cluster.directory_scope)
            .unwrap()
            .strong_status()
            .await
            .unwrap()
            .sequence,
        4
    );
    reopened.close().await.unwrap();
}
