use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_iroh::{
    Attachment, Options,
    iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets, protocol::Router, tls::CaTlsConfig},
    testing::{LookupServer, run_relay_server},
};
use rocky_sdk::Network;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

use crate::common;

const WAIT: Duration = Duration::from_secs(60);

fn starter(lookup: Option<&str>, local: bool) -> Network {
    Network::Owned(Arc::new(rocky_iroh::Options {
        relay: Some("disabled".into()),
        lookup: Some(lookup.unwrap_or("disabled").into()),
        topic: lookup.is_some(),
        local,
        bind: Some(if local { "0.0.0.0:0" } else { "127.0.0.1:0" }.into()),
    }))
}
async fn discovered(device: &Device) -> Option<Value> {
    let peers = snap::ok(async {
        let network = device.database.status().await?.network;
        Ok::<_, rocky_sdk::Error>(json!({"peers":network.peers,"discovery":network.discovery}))
    })
    .await;
    peers["peers"]
        .as_array()?
        .iter()
        .find(|peer| peer["source"] == "discovery")
        .cloned()
}
async fn enrolled(root: &Path) -> Result<(), Failure> {
    let issuer = Device::create(&root.join("issuer"), Some(starter(None, false))).await?;
    let a = Device::join(
        &root.join("a"),
        &issuer.invite().await,
        starter(None, false),
    )
    .await?;
    let b = Device::join(
        &root.join("b"),
        &issuer.invite().await,
        starter(None, false),
    )
    .await?;
    b.sync().await.map_err(|error| error.to_string())?;
    issuer.add(1, "Milk").await;
    a.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Milk"]);
    assert!(b.titles().await.is_empty());
    let node = issuer.node().await;
    for member in [&a, &b] {
        snap::ok(member.database.remove_peer(snap::public_key(&(node)))).await;
        let peers = snap::ok(async {
            let network = member.database.status().await?.network;
            Ok::<_, rocky_sdk::Error>(json!({"peers":network.peers,"discovery":network.discovery}))
        })
        .await;
        assert!(
            peers["peers"]
                .as_array()
                .is_some_and(|peers| peers.iter().all(|peer| peer["is_configured"] == false))
        );
    }
    tokio::join!(a.close(), b.close(), issuer.close());
    Ok(())
}
async fn second_process(root: &Path, lookup: Option<&str>, local: bool) -> Result<Value, Failure> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "discovery::member_process",
            "--ignored",
            "--nocapture",
        ])
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
    )
    .await?;
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
    let status = snap::ok(b.database.status()).await;
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
    let host = Device::new(
        crate::common::configuration(
            &root.join("host"),
            &serde_json::from_str::<rocky_sdk::Schema>(&schema)?,
            None,
            None,
            Some(starter(None, true)),
        )
        .join(&invitation.parse()?)
        .await?,
    )?;
    snap::observe(host.sync_now())
        .await
        .map_err(|error| error.to_string())?;
    let found = tokio::time::timeout(WAIT, async {
        loop {
            let peers = snap::ok(async {
                let network = host.database.status().await?.network;
                Ok::<_, rocky_sdk::Error>(
                    json!({"peers":network.peers,"discovery":network.discovery}),
                )
            })
            .await;
            let local = peers["peers"]
                .as_array()
                .and_then(|peers| peers.iter().find(|peer| peer["is_local"] == true).cloned());
            if let Some(found) = local {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    assert_eq!(found["is_issuer"], true);
    std::fs::write(root.join("host-found"), found.to_string())?;
    let simulator_found_host = root.join("simulator-found");
    tokio::time::timeout(WAIT, async {
        while !simulator_found_host.exists() {
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
    let a = Device::open(&root.path().join("a"), Some(starter(None, true))).await?;
    let report = second_process(root.path(), None, true).await?;
    assert_eq!(report["titles"], json!(a.titles().await));
    assert_eq!(report["titles"], json!(["Milk"]));
    assert_eq!(report["found"]["node"], a.node().await);
    assert_eq!(report["found"]["is_local"], true);
    assert_eq!(report["peer"]["choice"], "selected_local");
    assert_eq!(report["discovery"]["announcing"], true);
    assert_eq!(report["discovery"]["local"]["state"], "on");
    assert_eq!(report["discovery"]["topic"]["state"], "off");
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
    )
    .await?;
    let topic = a.database.discovery_topic().await?;
    tokio::time::timeout(WAIT, server.wait_for_contacts(&topic, 1)).await?;
    let report = second_process(root.path(), Some(&server.url), false).await?;
    assert_eq!(report["titles"], json!(a.titles().await));
    assert_eq!(report["titles"], json!(["Milk"]));
    assert_eq!(report["found"]["node"], a.node().await);
    assert_eq!(report["found"]["is_local"], false);
    assert_eq!(report["discovery"]["topic"]["state"], "on");
    assert_eq!(report["discovery"]["local"]["state"], "off");
    assert_eq!(
        server.contacts(&topic),
        2,
        "both members merge their contact into the one record"
    );
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
    let attachment = Attachment::discovering(endpoint.clone(), options.discover());
    let router = attachment
        .register(Router::builder(endpoint.clone()))
        .spawn();
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Attached(attachment.connected())),
    )
    .await?;
    let topic = a.database.discovery_topic().await?;
    tokio::time::timeout(WAIT, server.wait_for_contacts(&topic, 1)).await?;
    let discovery = snap::ok(async { a.database.status().await.map(|status| status.network) })
        .await["discovery"]
        .clone();
    assert_eq!(
        discovery["announcing"], true,
        "a discovering attachment announces beside the endpoint's services"
    );
    assert_eq!(discovery["topic"]["state"], "on");
    assert_eq!(discovery["local"]["state"], "off");
    assert_eq!(endpoint.address_lookup()?.len(), services);
    a.database.detach().await?;
    assert_eq!(endpoint.address_lookup()?.len(), services);
    let discovery = snap::ok(async { a.database.status().await.map(|status| status.network) })
        .await["discovery"]
        .clone();
    assert_eq!(discovery["announcing"], false);
    a.close().await;
    assert!(!endpoint.is_closed());
    assert_eq!(endpoint.address_lookup()?.len(), services);

    let plain = Attachment::new(endpoint.clone());
    let a = Device::open(
        &root.path().join("a"),
        Some(Network::Borrowed(plain.transport())),
    )
    .await?;
    let discovery = snap::ok(async { a.database.status().await.map(|status| status.network) })
        .await["discovery"]
        .clone();
    assert_eq!(
        discovery["announcing"], false,
        "a plain attachment on the same endpoint has no discovery"
    );
    for path in ["local", "topic"] {
        assert_eq!(discovery[path]["state"], "unavailable");
        assert_eq!(
            discovery[path]["reason"],
            "the iroh transport has no discovery"
        );
    }
    a.close().await;
    assert_eq!(endpoint.address_lookup()?.len(), services);
    router.shutdown().await?;
    Ok(())
}

async fn relayed(secret: u8, relay: &rocky_iroh::iroh::RelayMap) -> Result<Endpoint, Failure> {
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
    let a = Device::create(&root.path().join("a"), a_network).await?;
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        b_network.ok_or("network")?,
    )
    .await?;
    a.add(1, "Milk").await;
    b.add(2, "Eggs").await;
    assert_eq!(b.sync_now().await?.reconciled(), 1);
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    assert_eq!(b.titles().await, ["Eggs", "Milk"]);
    for router in &routers {
        assert!(
            router.endpoint().addr().ip_addrs().next().is_none(),
            "no direct path exists: neither endpoint has an IP transport"
        );
    }
    tokio::join!(a.close(), b.close());
    for router in routers {
        tokio::time::timeout(Duration::from_secs(5), router.shutdown())
            .await?
            .map_err(|error| {
                format!("no connection outlives the databases that dialed it: {error}")
            })?;
        assert!(
            router.endpoint().is_closed(),
            "the application closes its own endpoints"
        );
    }
    Ok(())
}
