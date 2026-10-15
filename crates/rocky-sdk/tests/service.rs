//! Coordinated ordinary rows through a registered public service domain.
#![cfg(feature = "iroh")]
mod support;
use rocky_coordinator::{
    certified::*,
    service::{NetworkCoordinator, VoterService},
};
use rocky_core::{MeshId, net::Protocol};
use rocky_sdk::credentials::PrivateKey;
use rocky_sdk::{Coordination, Database, Network, Open, transport::TransportOptions};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc, time::Duration};

type Failure = Box<dyn std::error::Error + Send + Sync>;

const SCHEMA: &str = r#"{"application":"counter","version":1,"format":6,"tables":[{"name":"counts","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"value","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;

fn starter() -> TransportOptions {
    TransportOptions::Iroh {
        relay: Some("disabled".into()),
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
        bind: Some("127.0.0.1:0".into()),
    }
}

/// Three voters of a public service with open admission, and its bundle.
async fn service(dir: &std::path::Path) -> Result<(TrustBundle, NetworkCoordinator), Failure> {
    let root = PrivateKey::new();
    let admin = PrivateKey::new();
    let keys: Vec<_> = (0..3).map(|_| PrivateKey::new()).collect();
    let secrets: Vec<[u8; 32]> = (0..3).map(|_| *PrivateKey::new().as_bytes()).collect();
    let mut voters = Vec::new();
    let mut endpoints = Vec::new();
    let mut binds = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let id = index as u64 + 1;
        let tcp = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
        let udp = std::net::UdpSocket::bind("127.0.0.1:0")?.local_addr()?;
        voters.push(Voter {
            id,
            address: tcp.to_string(),
            key: key.public_key(),
        });
        let address = rocky_sdk::iroh::iroh::EndpointAddr::new(
            rocky_sdk::iroh::iroh::SecretKey::from_bytes(&secrets[index]).public(),
        )
        .with_ip_addr(udp);
        endpoints.push(ClientEndpoint {
            voter: id,
            kind: "iroh".into(),
            address: serde_json::to_string(&address)?,
        });
        binds.push(udp.to_string());
    }
    let genesis = CertifiedGenesis::sign(
        Genesis {
            mesh: MeshId([0x61; 16]),
            configuration: [6; 16],
            initial_voters: BTreeSet::from([1, 2, 3]),
            membership_authority: admin.public_key(),
            directory_definition: [0; 32],
            directory: None,
            authorities: Vec::new(),
            max_control_lifetime: 86_400,
            public: Some(PublicService {
                endpoints,
                admission: None,
                limits: rocky_coordinator::Limits::default(),
                tenants: 4,
                registrations_per_day: 16,
                tenant_lanes: 4,
            }),
            voters,
        },
        &root,
    )?;
    for (index, key) in keys.into_iter().enumerate() {
        let id = index as u64 + 1;
        let voter = VoterService::open(
            &dir.join(format!("voter-{id}")),
            genesis.clone(),
            root.public_key(),
            genesis.genesis.mesh,
            id,
            key,
            true,
        )
        .await?;
        let lane = rocky_sdk::iroh::Options {
            relay: Some("disabled".into()),
            lookup: Some("disabled".into()),
            topic: false,
            local: false,
            bind: Some(binds[index].clone()),
        }
        .start_serving(secrets[index], vec![Protocol::Coordinate])
        .await?;
        tokio::spawn(voter.clone().serve_clients(lane.transport()));
        tokio::spawn(voter.serve());
    }
    let admin = NetworkCoordinator::open(
        genesis.clone(),
        root.public_key(),
        genesis.genesis.mesh,
        admin,
    )?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while admin.initialize(1).await.is_err() {
        assert!(tokio::time::Instant::now() < deadline, "voters offline");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    loop {
        if admin.status(1).await.is_ok_and(|s| s.leader.is_some()) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no leader");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok((TrustBundle::new(&genesis)?, admin))
}

async fn call(
    session: &rocky_sdk::Session,
    scope: &Value,
    request: Value,
) -> Result<Value, Failure> {
    let reply: Value = serde_json::from_str(
        &session
            .call(json!({"version": 1, "scope": scope, "request": request}).to_string())
            .await?,
    )?;
    if reply["status"] == "ok" {
        Ok(reply["value"].clone())
    } else {
        Err(format!("{} failed: {}", request["type"], reply["error"]).into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registered_service_domain_commits_ordinary_rows_and_closes() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let directory = root.path().join("a");
    let opened = tokio::task::spawn_blocking(move || {
        Database::create(
            Open {
                directory: &directory,
                schema: SCHEMA,
                secrets: None,
                network: Some(Network::Owned(Arc::new(starter()))),
                coordination: Some(Coordination::Service(Box::new(bundle))),
            },
            None,
        )
    })
    .await??;
    let session = opened.database.session();
    let scope: Value = serde_json::from_str(&opened.database.scope())?;
    let identity = call(&session, &scope, json!({"type": "session"})).await?;
    let node = identity["node"].clone();
    let tenant = PrivateKey::new();
    let register = json!({"type": "coordination_register", "tenant": tenant.to_hex(),
        "registration": vec![1u8; 32], "trusted": [node], "lifetime": 3600});
    let registered = call(&session, &scope, register.clone()).await?;
    assert_eq!(registered["tenant"], tenant.public_key().to_hex());
    assert_eq!(call(&session, &scope, register).await?, registered);
    call(
        &session,
        &scope,
        json!({"type": "sql", "sql": "BEGIN", "coordinated": true}),
    )
    .await?;
    let id = call(
        &session,
        &scope,
        json!({"type":"next_id", "table":"counts"}),
    )
    .await?["id"]
        .clone();
    call(
        &session,
        &scope,
        json!({"type":"sql", "sql":"INSERT INTO counts(key,value,id) VALUES('total',5,?)",
        "values":[{"type":"integer", "value": id}]}),
    )
    .await?;
    let committed = call(
        &session,
        &scope,
        json!({"type":"sql", "sql":"COMMIT", "coordinated":true, "handle":vec![2u8;32]}),
    )
    .await?;
    assert_eq!(committed["committed"]["position"], "1");
    let usage = call(
        &session,
        &scope,
        json!({"type":"coordination_usage", "tenant":tenant.to_hex()}),
    )
    .await?;
    assert_eq!(usage["open_domains"], 1);
    call(
        &session,
        &scope,
        json!({"type":"coordination_close", "tenant":tenant.to_hex(), "id":vec![3u8;32]}),
    )
    .await?;
    let refused = call(
        &session,
        &scope,
        json!({"type":"sql", "sql":"BEGIN", "coordinated":true}),
    )
    .await;
    assert!(refused.is_err());
    session.close().await;
    if let Some(network) = opened.network {
        network.close().await;
    }
    opened.database.close().await?;
    Ok(())
}
