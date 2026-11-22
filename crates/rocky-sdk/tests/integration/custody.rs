use crate::common;
use crate::support::snapshot as snap;
use common::{Device, Failure, introduce};
use rocky_sdk::{Network, transport::memory::Hub};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn devices_never_online_together_converge_via_keyless_custody() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
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
    let identity = common::authority_json(&root.path().join("relay"), "identity")?;
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
    snap::ok(b.session.set_networking_enabled(false)).await;
    a.add(1, "Milk").await;
    a.sync().await.expect("upload");
    assert!(
        snap::observe(snap::read(
            &relay.database,
            "SELECT * FROM items",
            Vec::new(),
        ))
        .await
        .is_err()
    );
    relay.close().await;
    let relay = Device::open(&root.path().join("relay"), net("relay"))?;
    snap::ok(a.session.set_networking_enabled(false)).await;
    snap::ok(b.session.set_networking_enabled(true)).await;
    b.sync().await.expect("catch up");
    assert_eq!(b.titles().await, ["Milk"]);
    b.add(2, "Eggs").await;
    b.sync().await.expect("upload");
    snap::ok(b.session.set_networking_enabled(false)).await;
    snap::ok(a.session.set_networking_enabled(true)).await;
    a.sync().await.expect("catch up");
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    assert!(
        snap::observe(relay.session.provision(
            rocky_sdk::GrantProfile::Writer {
                scopes: ["items".to_owned()].into_iter().collect()
            },
            None
        ))
        .await
        .is_err(),
        "a keyless custodian cannot issue enrollment"
    );
    assert!(
        snap::observe(async {
            b.session.renew().await?;
            b.session.info().await
        })
        .await
        .is_err(),
        "a keyless custodian cannot issue renewal"
    );
    a.close().await;
    b.close().await;
    relay.close().await;
    Ok(())
}

#[tokio::test]
async fn configuration_enrolls_resumes_and_refuses_reading_identities() -> Result<(), Failure> {
    use rocky_sdk::CustodyPeer;
    let root = tempfile::tempdir()?;
    let iroh = || {
        Some(Network::Owned(Arc::new(rocky_iroh::Options::isolated(
            "127.0.0.1:0",
        ))))
    };
    let issuer = Device::create(&root.path().join("issuer"), iroh())?;
    let config = |invitation: String, directory: &str| rocky_sdk::CustodyConfiguration {
        coordination: None,
        directory: root.path().join(directory),
        schema: rocky_sdk::Schema::parse(common::SCHEMA).expect("schema"),
        transport: Arc::new(rocky_iroh::Options::isolated("127.0.0.1:0")),
        invitation: Some(invitation),
        peers: Vec::new(),
        enrollment_upstream: None,
    };
    let wrong = config(issuer.invite().await, "wrong");
    assert!(
        tokio::task::spawn_blocking(move || CustodyPeer::open(wrong))
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
    let peer = tokio::task::spawn_blocking(move || CustodyPeer::open(first)).await??;
    let contact = peer.contact().await?;
    assert!(contact.starts_with("rocky://peer/"));
    peer.backup(&root.path().join("archive")).await?;
    peer.close().await?;
    let peer = tokio::task::spawn_blocking(move || CustodyPeer::open(config)).await??;
    assert!(peer.contact().await?.starts_with("rocky://peer/"));
    peer.close().await?;
    issuer.close().await;
    Ok(())
}

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
        "transport":{"relay":"disabled", "lookup":"disabled", "topic":false, "local":false, "bind":"0.0.0.0:7447"}, "peers":[]
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
    let reopened = Device::open(&root.path().join("relay"), None)?;
    assert_eq!(
        snap::ok(reopened.session.info()).await["profile"]["kind"],
        "custody",
        "a clean exit leaves a custody file the native SDK can reopen"
    );
    reopened.close().await;
    Ok(())
}

#[tokio::test]
async fn custody_forwards_pairing_and_renewal_to_an_explicit_issuer_with_creator_offline()
-> Result<(), Failure> {
    use rocky_sdk::route_enrollment;
    let root = crate::support::small_directory()?;
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
    snap::ok(
        relay
            .session
            .set_enrollment_upstream(Some(&(snap::text(&(issuer_contact))))),
    )
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
    let original = snap::ok(device.session.info()).await;
    assert_eq!(
        original["profile"],
        json!({"kind":"writer","scopes":["items"]})
    );
    snap::ok(async {
        device.session.renew().await?;
        device.session.info().await
    })
    .await;
    assert_eq!(
        snap::ok(device.session.info()).await["profile"],
        original["profile"]
    );
    introduce(&device, &relay).await;
    device.add(90, "paired through custody").await;
    device
        .sync()
        .await
        .expect("proxy enrollment route must not be used for row sync");
    let contact = route_enrollment(&issuer_contact, &relay_contact)?;
    assert_eq!(
        device.add_peer(&contact).await.expect("renewal route")["node"],
        issuer.node().await,
        "a routed contact updates only renewal routes and keeps the issuer key"
    );
    assert!(
        snap::observe(relay.session.provision(
            rocky_sdk::GrantProfile::Writer {
                scopes: ["items".to_owned()].into_iter().collect()
            },
            None
        ))
        .await
        .is_err(),
        "a forwarding custodian cannot issue enrollment"
    );
    relay.close().await;
    let relay = Device::open(&root.path().join("relay"), net("relay"))?;
    snap::ok(async {
        device.session.renew().await?;
        device.session.info().await
    })
    .await;
    let identity = common::authority_json(&root.path().join("relay"), "identity")?;
    assert_eq!(
        identity["keys"],
        json!({}),
        "forwarding survives a restart without acquiring any key"
    );
    assert_eq!(identity["authority"], json!(null));
    issuer.close().await;
    let error = snap::observe(async {
        device.session.renew().await?;
        device.session.info().await
    })
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
async fn custody_forwards_an_invitation_to_its_issuer_and_the_invited_node_renews_through_it()
-> Result<(), Failure> {
    use rocky_sdk::{Configuration, IdentityKey, IdentityProvider, InviteTerms, route_enrollment};
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let net = |name| Some(Network::Owned(hub.connector(name)));
    let issuer = Device::create(&root.path().join("issuer"), net("issuer"))?;
    let relay = Device::join(
        &root.path().join("relay"),
        &issuer
            .invite_as(json!({"kind": "custody", "scopes": ["items"]}))
            .await,
        net("relay").ok_or("network")?,
    )?;
    let issuer_contact = issuer.contact().await;
    snap::ok(
        relay
            .session
            .set_enrollment_upstream(Some(&snap::text(&issuer_contact))),
    )
    .await;
    let person: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::generate());
    let identity = person.public_key()?;
    let writer: rocky_sdk::GrantProfile =
        serde_json::from_value(json!({"kind": "writer", "scopes": ["items"]}))?;
    let invitation = issuer
        .session
        .invite(identity, writer.clone(), InviteTerms::default())
        .await?;
    let routed = route_enrollment(&invitation.uri, &relay.contact().await)?;
    let other = Device::create(&root.path().join("other"), net("other"))?;
    assert!(
        route_enrollment(&invitation.uri, &other.contact().await).is_err(),
        "a relay of another database does not route the invitation"
    );
    other.close().await;
    let opened = Configuration::new(root.path().join("member"), common::parsed_schema())?
        .network(Network::Owned(hub.connector("member")))
        .identity(person.clone())
        .accept_invitation(&routed)
        .await?;
    let member = Device::new(opened)?.with_identity(person.clone());
    let info = member.session.info().await?;
    assert_eq!(info.principal, identity);
    assert_eq!(info.profile, writer);
    assert_eq!(
        issuer.session.open_invitations().await?[0].redeemed_by,
        Some(info.node),
        "the relay forwarded the redemption to the issuer the invitation names"
    );
    let identity_json = common::authority_json(&root.path().join("relay"), "identity")?;
    assert_eq!(
        identity_json["authority"],
        json!(null),
        "forwarding an invitation gives the relay no authority"
    );
    snap::ok(async {
        member.session.renew().await?;
        member.session.info().await
    })
    .await;
    member.close().await;
    relay.close().await;
    issuer.close().await;
    Ok(())
}

#[tokio::test]
async fn routed_records_preserve_authority_with_the_current_version() -> Result<(), Failure> {
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
    assert_eq!(routed["v"], 1);
    for name in ["publisher", "database", "node_key", "identity_key"] {
        assert_eq!(routed[name], original[name]);
    }
    let original = decode(&contact)?;
    let routed = decode(&route_enrollment(&contact, &contact)?)?;
    assert_eq!(routed["v"], 3);
    assert_eq!(routed["enrollment_only"], true);
    assert_eq!(routed["node"], original["node"]);
    assert_eq!(routed["publisher"], original["publisher"]);
    issuer.close().await;
    Ok(())
}
