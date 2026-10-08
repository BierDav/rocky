//! Typed strong commands through a coordination service: three voters in
//! this process with iroh client lanes on loopback, a domain its owner
//! registered, an executing issuer and a member that joined through the
//! starter's JSON options. See `docs/runtime/public-coordination.md`.
#![cfg(feature = "iroh")]
mod support;
use rocky_coordinator::{
    certified::*,
    service::{NetworkCoordinator, VoterService},
};
use rocky_core::{
    HandlerInput, HandlerOutput, MeshId, Outcome, SqlValue, TableWrite, net::Protocol,
};
use rocky_sdk::credentials::PrivateKey;
use rocky_sdk::{
    CommandHandler, Commands, Coordination, Database, Engine, Network, Open,
    transport::TransportOptions,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc, time::Duration};

type Failure = Box<dyn std::error::Error + Send + Sync>;

const SCHEMA: &str = r#"{"application":"counter","version":1,"commands":{"format":1,"handler":1,"commands":[{"name":"Add","version":1,"handler":"counts.add.v1","arguments":[{"name":"amount","type":"integer"}],"accepted":[{"name":"value","type":"integer"}],"rejected":[],"invocation":"counts","result":"counts","resources":["counts"]}]},"format":3,"tables":[{"name":"counts","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"value","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;

struct Counter;
impl CommandHandler for Counter {
    fn handle(&self, input: &HandlerInput) -> Result<HandlerOutput, String> {
        let SqlValue::Integer(amount) = input.arguments[0] else {
            return Err("not a number".into());
        };
        let value = match input.reads.scan("counts", None, None, 1)?.first() {
            Some(row) => match row[1] {
                SqlValue::Integer(value) => value,
                _ => return Err("not a number".into()),
            },
            None => 0,
        } + amount;
        Ok(HandlerOutput {
            outcome: Outcome::Accepted(vec![SqlValue::Integer(value)]),
            writes: vec![TableWrite::Put {
                table: "counts".into(),
                row: support::identity_row(vec![
                    SqlValue::Text("total".into()),
                    SqlValue::Integer(value),
                ]),
            }],
            jobs: Vec::new(),
        })
    }
}

fn starter() -> TransportOptions {
    TransportOptions::Iroh {
        relay: Some("disabled".into()),
        discovery: false,
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
            discovery: false,
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
        dir.join("admin.evidence"),
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

/// Waits until the wall clock reaches its next second, the resolution of
/// signed validity times, so a retry cannot share its first signing time.
async fn next_second() -> Result<(), Failure> {
    let seconds = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let start = seconds()?.as_secs();
    loop {
        let now = seconds()?;
        if now.as_secs() > start {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_nanos(
            1_000_000_000 - u64::from(now.subsec_nanos()),
        ))
        .await;
    }
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
        Err(reply["error"].to_string().into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_run_typed_commands_through_a_registered_service_domain() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;

    // The issuer executes; the service is named by its bundle alone.
    let directory = root.path().join("a");
    let opened = {
        let bundle = bundle.clone();
        tokio::task::spawn_blocking(move || {
            Database::create(
                Open {
                    directory: &directory,
                    schema: SCHEMA,
                    secrets: None,
                    network: Some(Network::Owned(Arc::new(starter()))),
                    commands: Some(Commands {
                        coordinator: Coordination::Service(Box::new(bundle)),
                        handler: Some(Arc::new(Counter)),
                        deadline: None,
                    }),
                },
                None,
            )
        })
        .await??
    };
    let a = opened.database.session();
    let scope: Value = serde_json::from_str(&opened.database.scope())?;
    let identity = call(&a, &scope, json!({"type": "command_identity"})).await?;
    assert_eq!(identity["coordinator"], true);

    let invite = call(
        &a,
        &scope,
        json!({"type": "pairing", "profile": {"kind": "writer", "scopes": ["counts"]}}),
    )
    .await?["uri"]
        .as_str()
        .unwrap()
        .to_owned();

    // The member joins through the starter's JSON options naming the same
    // bundle; it submits and delivers, and executes nothing.
    let options = json!({
        "directory": root.path().join("b"),
        "mode": "join",
        "invitation": invite,
        "transport": {"kind": "iroh", "relay": "disabled", "discovery": false, "bind": "127.0.0.1:0"},
        "coordinator": bundle,
    })
    .to_string();
    let b = tokio::task::spawn_blocking(move || Engine::open(&options, SCHEMA, None)).await??;
    let b_identity: Value = serde_json::from_str(
        &b.call(
            json!({"version": 1, "scope": scope, "request": {"type": "command_identity"}})
                .to_string(),
        )
        .await?,
    )?;
    let b_identity = &b_identity["value"];
    // The owner registers the domain under its own tenant key; the issuer
    // signs the publisher's grant. Only identities and generations leave.
    let tenant = PrivateKey::new();
    let register = json!({"type": "command_service_register", "tenant": tenant.to_hex(),
        "registration": vec![1u8; 32], "executors": [identity], "invokers": [identity, b_identity],
        "lifetime": 3600});
    let registered = call(&a, &scope, register.clone()).await?;
    assert_eq!(registered["tenant"], tenant.public_key().to_hex());
    // The same registration ID returns the same answer, even when the retry
    // is signed in a later second than its first attempt.
    next_second().await?;
    assert_eq!(call(&a, &scope, register.clone()).await?, registered);
    // The same ID naming different devices is a conflict, not a new registration.
    let mut conflicting = register;
    conflicting["invokers"] = json!([identity]);
    let conflict = call(&a, &scope, conflicting).await;
    assert!(
        conflict
            .as_ref()
            .is_err_and(|error| error.to_string().contains("RequestConflict")),
        "{conflict:?}"
    );
    // Another tenant cannot control this domain; the owner's next revision
    // keeps the head.
    let other = call(
        &a,
        &scope,
        json!({"type": "command_service_control", "tenant": PrivateKey::new().to_hex(),
            "id": vec![2u8; 32], "revision": 2, "executors": [identity], "invokers": [identity],
            "lifetime": 3600}),
    )
    .await;
    assert!(other.is_err_and(|error| error.to_string().contains("denied")));
    call(
        &a,
        &scope,
        json!({"type": "command_service_control", "tenant": tenant.to_hex(), "id": vec![3u8; 32],
            "revision": 2, "executors": [identity], "invokers": [identity, b_identity],
            "lifetime": 3600}),
    )
    .await?;
    let usage = call(
        &a,
        &scope,
        json!({"type": "command_service_usage", "tenant": tenant.to_hex()}),
    )
    .await?;
    assert_eq!(usage["open_domains"], 1);

    // The executor's first run acquires the domain and confirms through the service.
    let submit = |handle: u8, amount: i64| {
        json!({"type": "command_submit", "handle": vec![handle; 32], "method": "Add", "version": 1,
               "arguments": [{"type": "integer", "value": amount.to_string()}]})
    };
    call(&a, &scope, submit(1, 5)).await?;
    let ticket = call(
        &a,
        &scope,
        json!({"type": "command_run", "handle": vec![1u8; 32]}),
    )
    .await?;
    assert_eq!(ticket["tickets"][0]["state"], "applied", "{ticket}");

    // The member's command is delivered to the executor and confirmed once.
    let b_call = |request: Value| {
        let b = b.clone();
        let scope = scope.clone();
        async move {
            let reply: Value = serde_json::from_str(
                &b.call(json!({"version": 1, "scope": scope, "request": request}).to_string())
                    .await?,
            )?;
            Ok::<_, Failure>(reply)
        }
    };
    b_call(submit(2, 3)).await?;
    let mut state = Value::Null;
    for _ in 0..40 {
        let reply = b_call(json!({"type": "command_run", "handle": vec![2u8; 32]})).await?;
        state = reply["value"]["tickets"][0]["state"].clone();
        if state == "applied" || state == "confirmed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(state == "applied" || state == "confirmed", "{state}");
    let a_ticket = call(
        &a,
        &scope,
        json!({"type": "command_ticket", "handle": vec![1u8; 32]}),
    )
    .await?;
    assert_eq!(a_ticket["state"], "applied");
    // A non-issuer holds no publisher key to grant coordination.
    let refused = b_call(json!({"type": "command_coordination_grant", "tenant": PrivateKey::new().public_key().to_hex(), "lifetime": 60})).await?;
    assert_eq!(refused["status"], "error");

    // A second issuer assembles the complete confirmed state from the
    // executor alone: each result carries its voter-signed evidence, so it
    // verifies after the coordinator retired the acknowledged identity.
    let invite = call(
        &a,
        &scope,
        json!({"type": "pairing", "profile": {"kind": "issuer"}}),
    )
    .await?["uri"]
        .as_str()
        .unwrap()
        .to_owned();
    let directory = root.path().join("c");
    let c = {
        let bundle = bundle.clone();
        tokio::task::spawn_blocking(move || {
            Database::join(
                Open {
                    directory: &directory,
                    schema: SCHEMA,
                    secrets: None,
                    network: Some(Network::Owned(Arc::new(starter()))),
                    commands: Some(Commands {
                        coordinator: Coordination::Service(Box::new(bundle)),
                        handler: Some(Arc::new(Counter)),
                        deadline: None,
                    }),
                },
                &invite,
            )
        })
        .await??
    };
    let c = c.database.session();
    let contact = call(&c, &scope, json!({"type": "contact"})).await?["uri"].clone();
    call(&a, &scope, json!({"type": "peer_add", "contact": contact})).await?;
    call(&c, &scope, json!({"type": "sync"})).await?;
    let rows = call(
        &c,
        &scope,
        json!({"type": "command_rows", "table": "counts"}),
    )
    .await?;
    assert_eq!(
        (&rows["complete"], &rows["sequence"]),
        (&json!(true), &json!("2")),
        "{rows}"
    );

    // The owner makes it the executor; it acquires at the same head.
    let c_identity = call(&c, &scope, json!({"type": "command_identity"})).await?;
    call(
        &a,
        &scope,
        json!({"type": "command_service_control", "tenant": tenant.to_hex(), "id": vec![4u8; 32],
            "revision": 3, "executors": [c_identity], "invokers": [identity, b_identity, c_identity],
            "lifetime": 3600}),
    )
    .await?;
    call(&c, &scope, submit(5, 4)).await?;
    let ticket = call(
        &c,
        &scope,
        json!({"type": "command_run", "handle": vec![5u8; 32]}),
    )
    .await?;
    assert_eq!(ticket["tickets"][0]["state"], "applied", "{ticket}");
    assert_eq!(
        ticket["tickets"][0]["outcome"]["values"][0]["value"], "12",
        "{ticket}"
    );

    b.close().await?;
    opened.database.close().await?;
    Ok(())
}

/// Opens a database composed with the service, its commands executed by `Counter`.
fn composed<'a>(directory: &'a std::path::Path, bundle: &TrustBundle) -> Open<'a> {
    Open {
        directory,
        schema: SCHEMA,
        secrets: None,
        network: Some(Network::Owned(Arc::new(starter()))),
        commands: Some(Commands {
            coordinator: Coordination::Service(Box::new(bundle.clone())),
            handler: Some(Arc::new(Counter)),
            deadline: None,
        }),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_granted_execute_registers_and_executes_without_being_an_issuer()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let opened = {
        let (directory, bundle) = (root.path().join("a"), bundle.clone());
        tokio::task::spawn_blocking(move || Database::create(composed(&directory, &bundle), None))
            .await??
    };
    let a = opened.database.session();
    let scope: Value = serde_json::from_str(&opened.database.scope())?;
    // The issuer grants Execute, a raw right, to a device it does not make an issuer.
    let prepare = |name: &str| Database::device(&root.path().join(name), None);
    let execute = json!([{"scope": "counts",
        "rights": ["read", "insert", "update", "delete", "invoke", "execute", "custody"]}]);
    let writes =
        json!([{"scope": "counts", "rights": ["read", "insert", "update", "invoke", "custody"]}]);
    let mut opened_devices = Vec::new();
    for (name, grants) in [("x", &execute), ("w", &writes)] {
        let keys = prepare(name)?;
        let supplied = call(
            &a,
            &scope,
            json!({"type": "grant", "device": keys, "grants": grants}),
        )
        .await?["supplied"]
            .clone();
        let (directory, bundle) = (root.path().join(name), bundle.clone());
        let device = tokio::task::spawn_blocking(move || {
            Database::install(
                composed(&directory, &bundle),
                serde_json::from_value(supplied)?,
            )
        })
        .await??;
        opened_devices.push(device);
    }
    let x = opened_devices[0].database.session();
    let w = opened_devices[1].database.session();
    assert_eq!(
        call(&x, &scope, json!({"type": "session"})).await?["issuer"],
        false
    );
    let x_identity = call(&x, &scope, json!({"type": "command_identity"})).await?;
    let a_identity = call(&a, &scope, json!({"type": "command_identity"})).await?;
    // The tenant key's grant comes from an issuer once; then whoever may
    // execute registers, and nobody else.
    let tenant = PrivateKey::new();
    let grant = call(
        &a,
        &scope,
        json!({"type": "command_coordination_grant", "tenant": tenant.public_key().to_hex(), "lifetime": 600}),
    )
    .await?["grant"]
        .clone();
    let register = |grant: Value| {
        json!({"type": "command_service_register", "tenant": tenant.to_hex(),
            "registration": vec![7u8; 32], "executors": [x_identity], "invokers": [x_identity, a_identity],
            "lifetime": 3600, "grant": grant})
    };
    for (session, grant) in [
        (&x, Value::Null),
        (&x, call(&a, &scope, json!({"type": "command_coordination_grant", "tenant": PrivateKey::new().public_key().to_hex(), "lifetime": 600})).await?["grant"].clone()),
        (&w, grant.clone()),
    ] {
        let refused = call(session, &scope, register(grant)).await;
        assert!(refused.is_err(), "{refused:?}");
    }
    let registered = call(&x, &scope, register(grant.clone())).await?;
    assert_eq!(registered["tenant"], tenant.public_key().to_hex());
    // Refused attempts under the same ID do not unsettle its retry.
    next_second().await?;
    assert_eq!(call(&x, &scope, register(grant)).await?, registered);
    // The granted device acquires the domain and executes.
    call(
        &x,
        &scope,
        json!({"type": "command_submit", "handle": vec![1u8; 32], "method": "Add", "version": 1,
            "arguments": [{"type": "integer", "value": "5"}]}),
    )
    .await?;
    let ticket = call(
        &x,
        &scope,
        json!({"type": "command_run", "handle": vec![1u8; 32]}),
    )
    .await?;
    assert_eq!(ticket["tickets"][0]["state"], "applied", "{ticket}");
    assert_eq!(
        ticket["tickets"][0]["outcome"]["values"][0]["value"], "5",
        "{ticket}"
    );
    for device in opened_devices {
        device.database.close().await?;
    }
    opened.database.close().await?;
    Ok(())
}
