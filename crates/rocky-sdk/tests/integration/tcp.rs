use crate::common::{Device, Failure};
use rocky_sdk::{Configuration, ErrorKind, Network, ServiceError, SyncMode};
use std::{net::SocketAddr, sync::Arc};

fn tcp() -> Network {
    rocky_tcp::Options::new(SocketAddr::from(([127, 0, 0, 1], 0))).into()
}

fn websocket() -> Network {
    rocky_tcp::Options::new(SocketAddr::from(([127, 0, 0, 1], 0)))
        .websocket()
        .into()
}

fn dial_only() -> Network {
    rocky_tcp::Options::dialer().into()
}

#[tokio::test]
async fn devices_enroll_and_sync_both_ways_over_plain_tcp() -> Result<(), Failure> {
    enroll_and_sync_both_ways(tcp).await
}

#[tokio::test]
async fn devices_enroll_and_sync_both_ways_over_websockets() -> Result<(), Failure> {
    enroll_and_sync_both_ways(websocket).await
}

async fn enroll_and_sync_both_ways(network: fn() -> Network) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(&root.path().join("a"), Some(network())).await?;
    let invitation = a.invite().await;
    let b = Device::join(&root.path().join("b"), &invitation, network()).await?;
    b.sync().await.map_err(|error| error.to_string())?;
    let network = a.database.status().await?.network();
    assert_eq!(network.schemes, ["tcp", "ws", "wss"]);
    assert!(!network.dial_only);

    a.add(1, "Milk").await;
    let synced = b.sync_now().await?;
    assert!(synced.complete(), "{synced}");
    assert_eq!(b.titles().await, ["Milk"]);

    b.add(2, "Eggs").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    assert_eq!(a.items().await, b.items().await);

    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn a_dial_only_device_syncs_with_a_listening_peer_over_tcp() -> Result<(), Failure> {
    dial_only_syncs_with(tcp).await
}

#[tokio::test]
async fn a_dial_only_device_syncs_with_a_listening_peer_over_websockets() -> Result<(), Failure> {
    dial_only_syncs_with(websocket).await
}

async fn dial_only_syncs_with(listening: fn() -> Network) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(&root.path().join("a"), Some(listening())).await?;
    let b = Device::join(&root.path().join("b"), &a.invite().await, listening()).await?;
    let c = Device::join(&root.path().join("c"), &a.invite().await, dial_only()).await?;
    let status = c.database.status().await?.network();
    assert!(status.dial_only, "{status:?}");
    assert!(
        c.database.contact().await?.routes.is_empty(),
        "a dial-only device names no route"
    );

    a.add(1, "Milk").await;
    let synced = c.sync_now().await?;
    assert!(synced.complete(), "{synced}");
    assert_eq!(c.titles().await, ["Milk"]);

    c.add(2, "Eggs").await;
    c.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);

    b.sync().await.map_err(|error| error.to_string())?;
    let (known, complete) = known_peers_and_complete(&b).await?;
    assert!(
        known == 1 && complete,
        "the dial-only member leaves no dead route on its peers"
    );
    assert!(
        known_peers_and_complete(&a).await?.0 <= 1,
        "the dial-only member is no sync target of the admin"
    );
    assert_eq!(b.titles().await, ["Eggs", "Milk"]);

    a.close().await;
    b.close().await;
    c.close().await;
    Ok(())
}

async fn known_peers_and_complete(device: &Device) -> Result<(usize, bool), Failure> {
    match device.sync_now().await {
        Ok(report) => Ok((report.peers().len(), report.complete())),
        Err(error) => match error.kind() {
            ErrorKind::Service(ServiceError::NoPeerReached(report)) => {
                Ok((report.peers().len(), false))
            }
            _ => Err(error.into()),
        },
    }
}

#[tokio::test]
async fn a_live_dial_only_device_redials_a_server_that_restarted_on_its_route()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let route = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let listening = || -> Network { rocky_tcp::Options::new(route).into() };
    let server = Device::create(&root.path().join("server"), Some(listening())).await?;
    server.database.set_sync(SyncMode::Live).await?;
    let device = Device::join(
        &root.path().join("device"),
        &server.invite().await,
        dial_only(),
    )
    .await?;
    device.database.set_sync(SyncMode::Live).await?;
    let mut changes = device.database.observe(&["items"]).await?;

    server.add(1, "Milk").await;
    received(&device, &mut changes, "Milk").await?;

    server.close().await;
    let server = Device::open(&root.path().join("server"), Some(listening())).await?;
    server.database.set_sync(SyncMode::Live).await?;
    server.add(2, "Eggs").await;
    received(&device, &mut changes, "Eggs").await?;

    server.close().await;
    device.close().await;
    Ok(())
}

async fn received(
    device: &Device,
    changes: &mut rocky_sdk::TableEvents,
    title: &str,
) -> Result<(), Failure> {
    let outlasts_a_loaded_ci_host = std::time::Duration::from_secs(60);
    tokio::time::timeout(outlasts_a_loaded_ci_host, async {
        while !device.titles().await.iter().any(|seen| seen == title) {
            changes.next().await?;
        }
        Ok::<_, Failure>(())
    })
    .await
    .map_err(|_| format!("the live device never received {title}"))?
}

#[tokio::test]
async fn the_issuer_reaches_its_joiner_without_add_peer() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let issuer = Device::create(&root.path().join("issuer"), Some(tcp())).await?;
    let invitation = issuer.invite().await;
    let joiner = Device::join(&root.path().join("joiner"), &invitation, tcp()).await?;

    joiner.add(1, "Milk").await;
    let synced = issuer.sync_now().await?;
    assert!(synced.complete(), "{synced}");
    assert_eq!(issuer.titles().await, ["Milk"]);

    joiner.close().await;
    let reopened = Device::open(&root.path().join("joiner"), Some(tcp())).await?;
    reopened.add(2, "Eggs").await;
    reopened.sync_now().await?;
    reopened.add(3, "Bread").await;
    let synced = issuer.sync_now().await?;
    assert!(
        synced.complete(),
        "the issuer dials the port the joiner advertised after reopening: {synced}"
    );
    assert_eq!(issuer.titles().await, ["Bread", "Eggs", "Milk"]);

    reopened.close().await;
    issuer.close().await;
    Ok(())
}

fn untrusted_tls() -> Result<Arc<rustls::ServerConfig>, Failure> {
    let key = rcgen::KeyPair::generate()?;
    let certificate =
        rcgen::CertificateParams::new(vec!["localhost".to_owned()])?.self_signed(&key)?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
    )?;
    Ok(Arc::new(config))
}

#[tokio::test]
async fn a_join_the_issuer_certificate_fails_names_the_tls_error_as_its_source()
-> Result<(), Failure> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let route = format!("wss://localhost:{}", listener.local_addr()?.port());
    let acceptor = tokio_rustls::TlsAcceptor::from(untrusted_tls()?);
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let _ = acceptor.accept(tcp).await;
        }
    });
    let root = tempfile::tempdir()?;
    let issuer = Device::create(
        &root.path().join("issuer"),
        Some(
            rocky_tcp::Options::new(SocketAddr::from(([127, 0, 0, 1], 0)))
                .advertise(route)
                .into(),
        ),
    )
    .await?;
    let code = issuer.invite().await;
    let failed = Configuration::new(root.path().join("member"), crate::common::parsed_schema())?
        .network(websocket())
        .join(&code)
        .await
        .err()
        .ok_or("joined through a certificate no root signed")?;
    assert!(
        matches!(
            failed.kind(),
            ErrorKind::Service(ServiceError::Unreachable(_))
        ),
        "{failed:?}"
    );
    let tls = std::iter::successors(
        Some(&failed as &(dyn std::error::Error + 'static)),
        |error| error.source(),
    )
    .filter_map(|error| error.downcast_ref::<std::io::Error>())
    .find_map(|io| io.get_ref()?.downcast_ref::<rustls::Error>());
    assert_eq!(
        tls,
        Some(&rustls::Error::InvalidCertificate(
            rustls::CertificateError::UnknownIssuer
        )),
        "{failed:?}"
    );
    issuer.close().await;
    Ok(())
}
