//! Acceptance 6 of the mesh scope: two members in two processes on one
//! host find each other with no trusted node reachable, discovery their
//! only source of contacts, once over mDNS and once over the rendezvous
//! record on a local lookup server. And the first relay run: two endpoints
//! with no direct path converge through a local relay. See
//! `docs/runtime/sdk-transports.md#qualification`.
#![cfg(feature = "iroh")]
use common::{Device, Failure};
use rocky_sdk::{
    Database, Network, Open,
    iroh::{
        Attachment, Options,
        iroh::{
            Endpoint, RelayMode, SecretKey, endpoint::presets, protocol::Router, tls::CaTlsConfig,
        },
        testing::{LookupServer, run_relay_server},
    },
    transport::TransportOptions,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

mod common;

const WAIT: Duration = Duration::from_secs(60);

fn starter(lookup: Option<&str>, local: bool) -> Network {
    Network::Owned(Arc::new(TransportOptions::Iroh {
        relay: Some("disabled".into()),
        lookup: Some(lookup.unwrap_or("disabled").into()),
        topic: lookup.is_some(),
        local,
        bind: Some("127.0.0.1:0".into()),
    }))
}
async fn discovered(device: &Device) -> Option<Value> {
    let peers = device.ok(json!({"type": "peers"})).await;
    peers["peers"]
        .as_array()?
        .iter()
        .find(|peer| peer["source"] == "discovery")
        .cloned()
}
/// An issuer and two members that each met it once. Only `a` holds the
/// issuer's row. The issuer then stops for good and both forget it.
async fn enrolled(root: &Path) -> Result<(), Failure> {
    let issuer = Device::create(&root.join("issuer"), Some(starter(None, false)))?;
    let a = Device::join(
        &root.join("a"),
        &issuer.invite().await,
        starter(None, false),
    )?;
    let b = Device::join(
        &root.join("b"),
        &issuer.invite().await,
        starter(None, false),
    )?;
    b.sync().await.map_err(|error| error.to_string())?;
    issuer.add(1, "Milk").await;
    a.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Milk"]);
    assert!(b.titles().await.is_empty());
    let node = issuer.node().await;
    for member in [&a, &b] {
        member
            .ok(json!({"type": "peer_remove", "node": node}))
            .await;
        let peers = member.ok(json!({"type": "peers"})).await;
        assert!(
            peers["peers"]
                .as_array()
                .is_some_and(|peers| peers.iter().all(|peer| peer["configured"] == false))
        );
    }
    tokio::join!(a.close(), b.close(), issuer.close());
    Ok(())
}
/// Runs member `b` in another process until it found `a` and holds its row.
async fn second_process(root: &Path, lookup: Option<&str>, local: bool) -> Result<Value, Failure> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "member_process", "--ignored", "--nocapture"])
        .env("ROCKY_DISCOVERY_MEMBER", root.join("b"))
        .env("ROCKY_DISCOVERY_REPORT", root.join("report.json"))
        .env("ROCKY_DISCOVERY_LOCAL", local.to_string());
    if let Some(lookup) = lookup {
        command.env("ROCKY_DISCOVERY_LOOKUP", lookup);
    }
    let output = tokio::task::spawn_blocking(move || command.output()).await??;
    assert!(
        output.status.success(),
        "the second process failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&std::fs::read(
        root.join("report.json"),
    )?)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "the second process of the discovery acceptance runs"]
async fn member_process() -> Result<(), Failure> {
    let directory = std::env::var_os("ROCKY_DISCOVERY_MEMBER").ok_or("member directory")?;
    let report = std::env::var_os("ROCKY_DISCOVERY_REPORT").ok_or("report path")?;
    let lookup = std::env::var("ROCKY_DISCOVERY_LOOKUP").ok();
    let local = std::env::var("ROCKY_DISCOVERY_LOCAL").is_ok_and(|local| local == "true");
    let b = Device::open(
        Path::new(&directory),
        Some(starter(lookup.as_deref(), local)),
    )?;
    let found = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(found) = discovered(&b).await {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    b.sync().await.map_err(|error| error.to_string())?;
    let status = b.ok(json!({"type": "status"})).await;
    std::fs::write(
        report,
        json!({
            "titles": b.titles().await,
            "found": found,
            "peer": b.peer(found["node"].as_str().unwrap_or_default()).await,
            "discovery": status["network"]["discovery"],
        })
        .to_string(),
    )?;
    b.close().await;
    Ok(())
}

/// The host side of the iOS simulator check (`sdks/native/qualify-ios-discovery`):
/// joins the domain the simulator created and finds its issuer over mDNS.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "the host process of the iOS simulator discovery check"]
async fn simulator_peer() -> Result<(), Failure> {
    let root = std::env::var_os("ROCKY_DISCOVERY_ROOT").ok_or("shared directory")?;
    let root = Path::new(&root);
    let read = |name: &str| std::fs::read_to_string(root.join(name));
    let (schema, invitation) = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            if let (Ok(schema), Ok(invitation)) = (read("schema.json"), read("invitation")) {
                return (schema, invitation);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await?;
    let host = Device::new(Database::join(
        Open {
            directory: &root.join("host"),
            schema: &schema,
            secrets: None,
            network: Some(starter(None, true)),
            commands: None,
        },
        invitation.trim(),
    )?)?;
    host.call(json!({"type": "sync"}))
        .await
        .map_err(|error| error.to_string())?;
    let found = tokio::time::timeout(WAIT, async {
        loop {
            let peers = host.ok(json!({"type": "peers"})).await;
            let local = peers["peers"]
                .as_array()
                .and_then(|peers| peers.iter().find(|peer| peer["local"] == true).cloned());
            if let Some(found) = local {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    assert_eq!(found["issuer"], true);
    std::fs::write(root.join("host-found"), found.to_string())?;
    // Stay discoverable until the simulator has found this process too.
    tokio::time::timeout(WAIT, async {
        while !root.join("simulator-found").exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    host.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_processes_find_each_other_over_mdns_and_converge() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    enrolled(root.path()).await?;
    let a = Device::open(&root.path().join("a"), Some(starter(None, true)))?;
    let report = second_process(root.path(), None, true).await?;
    assert_eq!(report["titles"], json!(a.titles().await));
    assert_eq!(report["titles"], json!(["Milk"]));
    assert_eq!(report["found"]["node"], a.node().await);
    assert_eq!(report["found"]["local"], true);
    assert_eq!(report["peer"]["choice"], "selected_local");
    assert_eq!(report["discovery"]["announcing"], true);
    assert_eq!(report["discovery"]["paths"]["local"]["state"], "on");
    assert_eq!(report["discovery"]["paths"]["topic"]["state"], "off");
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_processes_find_each_other_through_the_rendezvous_record_and_converge()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let server = LookupServer::start().await?;
    enrolled(root.path()).await?;
    let a = Device::open(
        &root.path().join("a"),
        Some(starter(Some(&server.url), false)),
    )?;
    let topics = a.ok(json!({"type": "discovery_topics"})).await;
    let topic: [u8; 32] = data_encoding::HEXLOWER
        .decode(topics["current"].as_str().ok_or("topic")?.as_bytes())?
        .try_into()
        .map_err(|_| "topic")?;
    tokio::time::timeout(WAIT, async {
        while server.contacts(&topic) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let report = second_process(root.path(), Some(&server.url), false).await?;
    assert_eq!(report["titles"], json!(a.titles().await));
    assert_eq!(report["titles"], json!(["Milk"]));
    assert_eq!(report["found"]["node"], a.node().await);
    assert_eq!(report["found"]["local"], false);
    assert_eq!(report["discovery"]["paths"]["topic"]["state"], "on");
    assert_eq!(report["discovery"]["paths"]["local"]["state"], "off");
    // Both members merged their contact into the one record.
    assert_eq!(server.contacts(&topic), 2);
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attached_endpoint_has_the_same_lookup_services_after_a_database_detaches()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let server = LookupServer::start().await?;
    let options = Options {
        lookup: Some(server.url.clone()),
        topic: true,
        ..Options::isolated("127.0.0.1:0")
    };
    let endpoint = options.bind([31; 32]).await?;
    let services = endpoint.address_lookup()?.len();
    assert_eq!(services, 2);
    // Asked for it, the attachment announces beside the endpoint's services.
    let attachment = Attachment::discovering(endpoint.clone(), options.discover());
    let router = attachment
        .register(Router::builder(endpoint.clone()))
        .spawn();
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Borrowed(attachment.transport())),
    )?;
    let topics = a.ok(json!({"type": "discovery_topics"})).await;
    let topic: [u8; 32] = data_encoding::HEXLOWER
        .decode(topics["current"].as_str().ok_or("topic")?.as_bytes())?
        .try_into()
        .map_err(|_| "topic")?;
    tokio::time::timeout(WAIT, async {
        while server.contacts(&topic) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let discovery = a.ok(json!({"type": "network"})).await["discovery"].clone();
    assert_eq!(discovery["announcing"], true);
    assert_eq!(discovery["paths"]["topic"]["state"], "on");
    assert_eq!(discovery["paths"]["local"]["state"], "off");
    assert_eq!(endpoint.address_lookup()?.len(), services);
    a.network.as_ref().ok_or("attached")?.close().await;
    assert_eq!(endpoint.address_lookup()?.len(), services);
    let discovery = a.ok(json!({"type": "network"})).await["discovery"].clone();
    assert_eq!(discovery["announcing"], false);
    a.close().await;
    assert!(!endpoint.is_closed());
    assert_eq!(endpoint.address_lookup()?.len(), services);

    // Not asked, an attachment on the same endpoint has no discovery.
    let plain = Attachment::new(endpoint.clone());
    let a = Device::open(
        &root.path().join("a"),
        Some(Network::Borrowed(plain.transport())),
    )?;
    let discovery = a.ok(json!({"type": "network"})).await["discovery"].clone();
    assert_eq!(discovery["announcing"], false);
    for path in ["local", "topic"] {
        assert_eq!(discovery["paths"][path]["state"], "unavailable");
        assert_eq!(
            discovery["paths"][path]["reason"],
            "the iroh transport has no discovery"
        );
    }
    a.close().await;
    assert_eq!(endpoint.address_lookup()?.len(), services);
    router.shutdown().await?;
    Ok(())
}

/// An application-owned endpoint that reaches others through `relay` only.
async fn relayed(secret: u8, relay: &rocky_sdk::iroh::iroh::RelayMap) -> Result<Endpoint, Failure> {
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&[secret; 32]))
        .relay_mode(RelayMode::Custom(relay.clone()))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .clear_ip_transports()
        .bind()
        .await?;
    tokio::time::timeout(WAIT, endpoint.online()).await?;
    assert!(endpoint.addr().ip_addrs().next().is_none());
    assert!(endpoint.addr().relay_urls().next().is_some());
    Ok(endpoint)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_endpoints_without_a_direct_path_converge_through_a_relay() -> Result<(), Failure> {
    let (relay, _url, _server) = run_relay_server().await?;
    let root = tempfile::tempdir()?;
    let mut routers = Vec::new();
    let mut network = Vec::new();
    let (first, second) = tokio::try_join!(relayed(21, &relay), relayed(22, &relay))?;
    for endpoint in [first, second] {
        let attachment = Attachment::new(endpoint.clone());
        routers.push(attachment.register(Router::builder(endpoint)).spawn());
        network.push(Network::Borrowed(attachment.transport()));
    }
    let (b_network, a_network) = (network.pop(), network.pop());
    let a = Device::create(&root.path().join("a"), a_network)?;
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        b_network.ok_or("network")?,
    )?;
    a.add(1, "Milk").await;
    b.add(2, "Eggs").await;
    let pass = b.call(json!({"type": "sync"})).await;
    assert_eq!(pass.map_err(|error| error.to_string())?["peers"], 1);
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    assert_eq!(b.titles().await, ["Eggs", "Milk"]);
    // No direct path existed at any point: neither endpoint has an IP transport.
    for router in &routers {
        assert!(router.endpoint().addr().ip_addrs().next().is_none());
    }
    tokio::join!(a.close(), b.close());
    // The endpoints are the application's to close, and close at once: no
    // connection outlives the databases that dialed it.
    for router in routers {
        tokio::time::timeout(Duration::from_secs(5), router.shutdown()).await??;
        assert!(router.endpoint().is_closed());
    }
    Ok(())
}
