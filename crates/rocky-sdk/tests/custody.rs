//! Custody-only enrollment and staggered device availability.
mod common;
use common::{Device, Failure, introduce};
use rocky_sdk::{Network, transport::memory::Hub};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn devices_never_online_together_converge_via_keyless_custody() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let net = |name| Some(Network::Owned(hub.connector(name)));
    let issuer = Device::create(&root.path().join("issuer"), net("issuer"))?;
    let invitation = issuer
        .invite_as(json!({"kind": "custody", "scopes": ["items"]}))
        .await;
    let relay = Device::join(
        &root.path().join("relay"),
        &invitation,
        net("relay").ok_or("network")?,
    )?;
    let identity: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("relay/identity.json"))?)?;
    assert_eq!(identity["keys"], json!({}));
    assert_eq!(identity["authority"], json!(null));
    assert_eq!(identity["profile"]["kind"], "custody");
    for file in std::fs::read_dir(root.path().join("relay/secrets"))? {
        let name = file?.file_name().to_string_lossy().into_owned();
        assert!(
            !name.contains("reader") && !name.contains("publisher"),
            "{name}"
        );
    }
    let a = Device::join(
        &root.path().join("a"),
        &issuer.invite().await,
        net("a").ok_or("network")?,
    )?;
    let b = Device::join(
        &root.path().join("b"),
        &issuer.invite().await,
        net("b").ok_or("network")?,
    )?;
    introduce(&a, &relay).await;
    introduce(&b, &relay).await;
    issuer.close().await;
    b.ok(json!({"type": "connection", "online": false})).await;
    a.add(1, "Milk").await;
    a.sync().await.expect("upload");
    assert!(
        relay
            .call(json!({"type": "read", "query": {"sql": "SELECT * FROM items", "max_rows": 64}}))
            .await
            .is_err()
    );
    relay.close().await;
    let relay = Device::open(&root.path().join("relay"), net("relay"))?;
    a.ok(json!({"type": "connection", "online": false})).await;
    b.ok(json!({"type": "connection", "online": true})).await;
    b.sync().await.expect("catch up");
    assert_eq!(b.titles().await, ["Milk"]);
    b.add(2, "Eggs").await;
    b.sync().await.expect("upload");
    b.ok(json!({"type": "connection", "online": false})).await;
    a.ok(json!({"type": "connection", "online": true})).await;
    a.sync().await.expect("catch up");
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    // A keyless custodian cannot issue enrollment or renewal.
    assert!(
        relay
            .call(json!({"type": "pairing", "profile": {"kind": "writer", "scopes": ["items"]}}))
            .await
            .is_err()
    );
    assert!(b.call(json!({"type": "renew"})).await.is_err());
    a.close().await;
    b.close().await;
    relay.close().await;
    Ok(())
}

#[tokio::test]
async fn configuration_enrolls_resumes_and_refuses_reading_identities() -> Result<(), Failure> {
    use rocky_sdk::{CustodyPeer, transport::TransportOptions};
    let root = tempfile::tempdir()?;
    let tcp = || {
        Some(Network::Owned(Arc::new(TransportOptions::Tcp {
            listen: Some("127.0.0.1:0".into()),
            advertised: None,
        })))
    };
    let issuer = Device::create(&root.path().join("issuer"), tcp())?;
    let config = |invitation: String, directory: &str| {
        json!({
        "directory": root.path().join(directory), "schema": serde_json::from_str::<serde_json::Value>(common::SCHEMA).expect("schema"),
        "transport": {"kind": "tcp", "listen": "127.0.0.1:0"}, "invitation": invitation,
    }).to_string()
    };
    let wrong = config(issuer.invite().await, "wrong");
    assert!(
        tokio::task::spawn_blocking(move || CustodyPeer::open(&wrong))
            .await?
            .is_err()
    );
    assert!(!root.path().join("wrong").exists());
    let config = config(
        issuer
            .invite_as(json!({"kind": "custody", "scopes": ["items"]}))
            .await,
        "relay",
    );
    let first = config.clone();
    let peer = tokio::task::spawn_blocking(move || CustodyPeer::open(&first)).await??;
    let contact = peer.contact().await?;
    assert!(contact.starts_with("rocky://peer/"));
    peer.backup(&root.path().join("archive")).await?;
    peer.close().await?;
    let peer = tokio::task::spawn_blocking(move || CustodyPeer::open(&config)).await??;
    assert!(peer.contact().await?.starts_with("rocky://peer/"));
    peer.close().await?;
    issuer.close().await;
    Ok(())
}

/// Run after building the local image; excluded from ordinary workspace checks.
#[tokio::test]
#[ignore = "requires Docker and the local rocky-peer:p06 image"]
async fn container_resumes_from_configuration_and_drains_on_sigterm() -> Result<(), Failure> {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(
        &root.path().join("issuer"),
        Some(Network::Owned(hub.connector("issuer"))),
    )?;
    let invite = issuer
        .invite_as(json!({"kind":"custody", "scopes":["items"]}))
        .await;
    let relay = Device::join(
        &root.path().join("relay"),
        &invite,
        Network::Owned(hub.connector("relay")),
    )?;
    relay.close().await;
    issuer.close().await;
    std::fs::write(root.path().join("peer.json"), json!({
        "directory":"/data", "schema":serde_json::from_str::<serde_json::Value>(common::SCHEMA)?,
        "transport":{"kind":"tcp", "listen":"0.0.0.0:7447", "advertised":"127.0.0.1:7447"}, "peers":[]
    }).to_string())?;
    let name = format!(
        "rocky-p06-{}",
        root.path()
            .file_name()
            .ok_or("directory")?
            .to_string_lossy()
    );
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = Command::new("docker")
                .args(["rm", "--force", &self.0])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    let _cleanup = Cleanup(name.clone());
    let mut process = Command::new("docker")
        .args(["run", "--name", &name, "--volume"])
        .arg(format!("{}:/data", root.path().join("relay").display()))
        .arg("--volume")
        .arg(format!(
            "{}:/config.json:ro",
            root.path().join("peer.json").display()
        ))
        .args(["rocky-peer:p06", "/config.json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdout = process.stdout.take().ok_or("container stdout")?;
    let contact = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            let mut contact = String::new();
            BufReader::new(stdout).read_line(&mut contact)?;
            Ok::<_, std::io::Error>(contact)
        }),
    )
    .await???;
    assert!(contact.starts_with("rocky://peer/"), "{contact}");
    let status = Command::new("docker")
        .args(["stop", "--time", "10", &name])
        .status()?;
    assert!(status.success());
    assert!(process.wait()?.success());
    // A clean exit must leave a custody file the native SDK can reopen.
    let reopened = Device::open(&root.path().join("relay"), None)?;
    assert_eq!(
        reopened.ok(json!({"type":"session"})).await["profile"]["kind"],
        "custody"
    );
    reopened.close().await;
    Ok(())
}

#[tokio::test]
async fn custody_forwards_pairing_and_renewal_to_an_explicit_issuer_with_creator_offline()
-> Result<(), Failure> {
    use rocky_sdk::route_enrollment;
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let net = |name| Some(Network::Owned(hub.connector(name)));
    let creator = Device::create(&root.path().join("creator"), net("creator"))?;
    let issuer = Device::join(
        &root.path().join("issuer"),
        &creator.invite_as(json!({"kind":"issuer"})).await,
        net("issuer").ok_or("network")?,
    )?;
    let relay = Device::join(
        &root.path().join("relay"),
        &creator
            .invite_as(json!({"kind":"custody","scopes":["items"]}))
            .await,
        net("relay").ok_or("network")?,
    )?;
    let issuer_contact = issuer.contact().await;
    relay
        .ok(json!({"type":"enrollment_upstream","contact":issuer_contact}))
        .await;
    let relay_contact = relay.contact().await;
    let invitation = issuer.invite().await;
    let routed = route_enrollment(&invitation, &relay_contact)?;
    creator.close().await;
    let device = Device::join(
        &root.path().join("device"),
        &routed,
        net("device").ok_or("network")?,
    )?;
    let original = device.ok(json!({"type":"session"})).await;
    assert_eq!(
        original["profile"],
        json!({"kind":"writer","scopes":["items"]})
    );
    device.ok(json!({"type":"renew"})).await;
    assert_eq!(
        device.ok(json!({"type":"session"})).await["profile"],
        original["profile"]
    );
    introduce(&device, &relay).await;
    device.add(90, "paired through custody").await;
    device
        .sync()
        .await
        .expect("proxy enrollment route must not be used for row sync");
    // A routed contact updates only renewal routes; it preserves the issuer key.
    let contact = route_enrollment(&issuer_contact, &relay_contact)?;
    assert_eq!(
        device.add_peer(&contact).await.expect("renewal route")["node"],
        issuer.node().await
    );
    assert!(
        relay
            .call(json!({"type":"pairing","profile":{"kind":"writer","scopes":["items"]}}))
            .await
            .is_err()
    );
    // The forwarding configuration survives a restart without acquiring any key.
    relay.close().await;
    let relay = Device::open(&root.path().join("relay"), net("relay"))?;
    device.ok(json!({"type":"renew"})).await;
    let identity: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("relay/identity.json"))?)?;
    assert_eq!(identity["keys"], json!({}));
    assert_eq!(identity["authority"], json!(null));
    issuer.close().await;
    let error = device
        .call(json!({"type":"renew"}))
        .await
        .err()
        .ok_or("all issuers offline must refuse")?;
    assert_eq!(error["code"], "unreachable");
    assert_eq!(error["retryable"], true);
    assert!(
        route_enrollment(&relay_contact, &relay_contact).is_err(),
        "custody contact cannot become an issuer"
    );
    device.close().await;
    relay.close().await;
    Ok(())
}

#[tokio::test]
async fn version_six_migrates_and_routed_records_preserve_authority() -> Result<(), Failure> {
    use data_encoding::BASE64URL_NOPAD;
    use rocky_sdk::route_enrollment;
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(root.path(), Some(Network::Owned(hub.connector("issuer"))))?;
    let invitation = issuer.invite().await;
    let contact = issuer.contact().await;
    let decode = |uri: &str| -> Result<serde_json::Value, Failure> {
        Ok(serde_json::from_slice(&BASE64URL_NOPAD.decode(
            uri.rsplit('/').next().ok_or("URI")?.as_bytes(),
        )?)?)
    };
    let original = decode(&invitation)?;
    let routed = decode(&route_enrollment(&invitation, &contact)?)?;
    assert_eq!(routed["v"], 4);
    for name in ["token", "publisher", "profile", "expires"] {
        assert_eq!(routed[name], original[name]);
    }
    let original = decode(&contact)?;
    let routed = decode(&route_enrollment(&contact, &contact)?)?;
    assert_eq!(routed["v"], 2);
    assert_eq!(routed["enrollment_only"], true);
    assert_eq!(routed["node"], original["node"]);
    assert_eq!(routed["publisher"], original["publisher"]);
    let node = issuer.node().await;
    issuer.close().await;
    let path = root.path().join("identity.json");
    let mut identity: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    identity["version"] = json!(6);
    identity
        .as_object_mut()
        .ok_or("identity")?
        .remove("enrollment_routes");
    identity
        .as_object_mut()
        .ok_or("identity")?
        .remove("enrollment_upstream");
    std::fs::write(&path, serde_json::to_vec(&identity)?)?;
    let issuer = Device::open(root.path(), None)?;
    assert_eq!(issuer.node().await, node);
    issuer.close().await;
    let identity: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    assert_eq!(identity["version"], 8);
    assert_eq!(identity["enrollment_routes"], json!({}));
    Ok(())
}
