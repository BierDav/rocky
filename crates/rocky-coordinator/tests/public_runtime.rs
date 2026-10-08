//! A real application runtime executes strong commands through a domain
//! its owner registered on a public service. Only identities, generations
//! and commitments reach the voters; the application stays on its nodes.
#![cfg(feature = "iroh")]
#[allow(dead_code)]
mod support;
use directory_app::{Directory, DirectoryApp, Transition};
use p2panda_core::PrivateKey;
use rocky_coordinator::certified::{ControlCertificate, DomainControl, node_id};
use rocky_core::*;
use rocky_runtime::{
    CommandHandle, CommandState, DomainInstallation, Identity, Runtime, RuntimeConfig,
};
use support::{public::*, timestamp};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_application_runs_strong_commands_on_a_registered_domain() {
    let mut cluster = PublicCluster::start(
        rocky_coordinator::Limits {
            tenant_domains: 2,
            tenant_bytes: 1024 * 1024,
            ..rocky_coordinator::Limits::default()
        },
        3,
    )
    .await;
    // The publisher of "directory" grants its owner's tenant key.
    let tenant = Tenant::new("directory");
    let registrar_key = PrivateKey::new();
    let user = PrivateKey::new();
    let app = DirectoryApp {
        publisher: *tenant.publisher.public_key().as_bytes(),
        domain: DomainId([0x33; 16]),
        bootstrap_registrar: directory_app::principal(*registrar_key.public_key().as_bytes()),
        initial_ownership: Vec::new(),
    };
    let scope = Scope {
        mesh: MeshId([0x44; 16]),
        app: app.manifest().app,
        domain: app.domain,
    };
    assert_eq!(scope.app, tenant.app());
    let registrar = app.bootstrap_registrar;
    let node = node_id(tenant.executor.public_key());
    let certificate = ControlCertificate::sign(
        DomainControl {
            scope,
            configuration: cluster.genesis.genesis.configuration,
            revision: 1,
            owner: registrar,
            executors: vec![tenant.executor.public_key()],
            invokers: vec![(tenant.executor.public_key(), registrar)],
            schema: app.manifest().schema,
            handler: app.manifest().handler,
            not_before: timestamp() - 60,
            expires: timestamp() + DAY,
        },
        &tenant.key,
    )
    .unwrap();
    let owner = tenant.client(&cluster, &tenant.key, "owner").await;
    owner
        .register(
            tenant.grant(),
            Some(cluster.admission(&tenant)),
            certificate,
            [1; 32],
        )
        .await
        .unwrap();
    let client = tenant.client(&cluster, &tenant.executor, "executor").await;
    client.acquire(scope, [2; 32]).await.unwrap();

    let runtime = Runtime::open(
        cluster.dir.path().join("directory.db"),
        vec![DomainInstallation {
            app: app.clone(),
            policy: directory_app::Policy {
                scope,
                caller: registrar,
                reference: [9; 32],
                permitted_callers: vec![],
            },
            identity: Identity {
                scope,
                node,
                caller: registrar,
            },
            config: RuntimeConfig {
                authors: vec![(node, registrar)],
                delivery_targets: vec![],
                full_nodes: Vec::new(),
            },
        }],
    )
    .await
    .unwrap();
    let replica = runtime.domain(scope).unwrap();
    let register = |handle: u8, key: &PrivateKey| {
        replica
            .submit_command_with_arguments(
                CommandHandle([handle; 32]),
                directory_app::COMMAND,
                serde_json::to_vec(&Transition::RegisterUser {
                    key: *key.public_key().as_bytes(),
                })
                .unwrap(),
                [9; 32],
            )
            .unwrap()
    };
    register(1, &user).committed().await.unwrap();
    replica.execute_pending(&client).await.unwrap();
    let result = replica
        .confirmed_publications()
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        Directory::decode(&result.package.rows).unwrap().users.len(),
        1
    );
    // The receipt verifies offline against the pinned bundle.
    client
        .evidence(&result.receipt)
        .await
        .unwrap()
        .verify(&cluster.bundle.genesis().unwrap())
        .unwrap();

    // Quorum outage: the command stays pending and honest, then completes.
    let admin = cluster.admin();
    let leader = cluster.wait_leader(&admin).await;
    let down: Vec<u64> = (1..=3).filter(|id| *id != leader).collect();
    for id in &down {
        cluster.stop(*id);
    }
    register(2, &PrivateKey::new()).committed().await.unwrap();
    assert!(replica.execute_pending(&client).await.is_err());
    assert!(
        replica
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
    for id in &down {
        cluster.spawn(*id, false);
    }
    for id in &down {
        cluster.wait_online(*id, &admin).await;
    }
    cluster.wait_leader(&admin).await;
    replica.execute_pending(&client).await.unwrap();
    assert_eq!(replica.strong_status().await.unwrap().sequence, 2);

    // The voters hold no application payload: the user's key never appears.
    assert_eq!(client.head(scope).await.unwrap().sequence, 2);
    let leader = cluster.wait_leader(&admin).await;
    let status = admin.status(leader).await.unwrap();
    println!(
        "public runtime: directory sequence=2 over iroh; leader state bytes={} disk bytes={}",
        status.state_bytes, status.disk_bytes
    );
    let needle = user.public_key().as_bytes().to_vec();
    for id in 1..=3 {
        for entry in walk(&cluster.data(id)) {
            let bytes = std::fs::read(&entry).unwrap();
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle.as_slice()),
                "{} holds an application payload",
                entry.display()
            );
        }
    }
    runtime.close().await.unwrap();
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}
