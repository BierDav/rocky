use rocky_core::net::{Failure, Incoming, Protocol, Transport};
use rocky_iroh::{
    Attachment, Options,
    iroh::{
        Endpoint, EndpointAddr,
        endpoint::Connection,
        protocol::{AcceptError, ProtocolHandler, Router},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ECHO: &[u8] = b"example/echo/1";
const TEST_SCOPE: rocky_core::Scope = rocky_core::Scope {
    mesh: rocky_core::MeshId([1; 16]),
    app: rocky_core::AppId([2; 16]),
    domain: rocky_core::DomainId([3; 16]),
};

#[derive(Debug)]
struct Echo;
impl ProtocolHandler for Echo {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        tokio::io::copy(&mut recv, &mut send).await?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }
}
fn local() -> Options {
    Options::isolated("127.0.0.1:0")
}
async fn echo(from: &Endpoint, to: EndpointAddr) -> Result<Vec<u8>, Failure> {
    let connection = from.connect(to, ECHO).await?;
    let (mut send, mut recv) = connection.open_bi().await?;
    send.write_all(b"hello").await?;
    send.finish()?;
    let reply = recv.read_to_end(64).await?;
    connection.close(0u32.into(), b"done");
    Ok(reply)
}
async fn rocky(from: &dyn Transport, to: &dyn Transport) -> Result<(), Failure> {
    let address = to.address().await?;
    let exchange = from.request(std::slice::from_ref(&address), TEST_SCOPE, b"ping");
    let answering = async {
        let Incoming::Request(request) = to.accept().await.ok_or("closed")? else {
            return Err("RPC expected".into());
        };
        assert_eq!(request.bytes, b"ping");
        request.respond(b"pong".to_vec()).await
    };
    let (reply, ()) = tokio::try_join!(exchange, answering)?;
    assert_eq!(reply, b"pong");
    Ok(())
}

async fn refused(from: &dyn Transport, address: &str) -> bool {
    from.request(&[address.into()], TEST_SCOPE, b"ping")
        .await
        .is_err()
}

#[tokio::test]
async fn rocky_and_an_application_protocol_share_an_application_endpoint() -> Result<(), Failure> {
    let endpoint = local().bind([7; 32]).await?;
    let attachment = Attachment::new(endpoint.clone());
    let application_router = attachment
        .register(Router::builder(endpoint.clone()).accept(ECHO, Echo))
        .spawn();
    let served = attachment.transport();
    let other = local().start([8; 32]).await?;
    let dialing = other.transport();
    assert!(
        refused(&*dialing, &served.address().await?).await,
        "Rocky's ALPNs are refused until a service listens"
    );
    assert_eq!(attachment.refused(), 1);
    assert_eq!(
        echo(&other.endpoint(), endpoint.addr()).await?,
        b"hello",
        "the application's protocol answers while Rocky's are refused"
    );
    served.listen(Some(TEST_SCOPE), true).expect("listen");
    dialing.listen(Some(TEST_SCOPE), true).expect("listen");

    rocky(&*dialing, &*served).await?;
    rocky(&*served, &*dialing).await?;
    assert_eq!(echo(&other.endpoint(), endpoint.addr()).await?, b"hello");

    served.close().await;
    served.close().await;
    assert!(
        served.accept().await.is_none(),
        "closing ends Rocky's streams"
    );
    let address = served.address().await?;
    let (on_earlier_connection, fresh_dial) = (
        refused(&*dialing, &address).await,
        refused(&*dialing, &address).await,
    );
    assert!(
        on_earlier_connection && fresh_dial,
        "a closed attachment refuses its ALPNs on old and new connections"
    );
    assert!(attachment.refused() >= 1);
    assert!(
        !endpoint.is_closed() && !application_router.is_shutdown(),
        "closing Rocky's side leaves the application's endpoint and router"
    );
    assert_eq!(echo(&other.endpoint(), endpoint.addr()).await?, b"hello");

    application_router.shutdown().await?;
    assert!(endpoint.is_closed());
    dialing.close().await;
    Ok(())
}

#[tokio::test]
async fn the_starter_shuts_down_everything_it_bound() -> Result<(), Failure> {
    let first = local().start([1; 32]).await?;
    let second = local().start([2; 32]).await?;
    assert_ne!(first.endpoint().id(), second.endpoint().id());
    assert_eq!(
        first.endpoint().id(),
        local().bind([1; 32]).await?.id(),
        "the endpoint key follows the stable transport key"
    );
    let (near, far) = (first.transport(), second.transport());
    far.listen(Some(TEST_SCOPE), true).expect("listen");
    rocky(&*near, &*far).await?;
    near.close().await;
    far.close().await;
    far.close().await;
    assert!(first.endpoint().is_closed() && second.endpoint().is_closed());
    assert!(
        near.connect(&far.address().await?, Protocol::Rpc, None)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn foreground_rebinds_dead_starter_with_the_same_identity() -> Result<(), Failure> {
    let first = local().start([31; 32]).await?;
    let second = local().start([32; 32]).await?;
    let identity = first.endpoint().id();
    first.listen(Some(TEST_SCOPE), true).expect("listen");
    second.listen(Some(TEST_SCOPE), true).expect("listen");
    first.endpoint().close().await;
    first.foreground().await?;
    assert_eq!(first.endpoint().id(), identity);
    assert!(!first.endpoint().is_closed());
    rocky(&first, &second).await?;
    rocky(&second, &first).await?;
    first.close().await;
    second.close().await;
    assert!(first.foreground().await.is_err());
    Ok(())
}

#[tokio::test]
async fn a_dial_rebinds_a_dead_starter_on_its_fixed_port() -> Result<(), Failure> {
    let port = std::net::UdpSocket::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let first = Options::isolated(&format!("127.0.0.1:{port}"))
        .start([34; 32])
        .await?;
    let second = local().start([35; 32]).await?;
    first.listen(Some(TEST_SCOPE), true).expect("listen");
    second.listen(Some(TEST_SCOPE), true).expect("listen");
    rocky(&first, &second).await?;
    first.endpoint().close().await;
    rocky(&first, &second).await?;
    rocky(&second, &first).await?;
    assert!(
        first.address().await?.contains(&format!(":{port}")),
        "the next dial heals the starter on its fixed port without a foreground"
    );
    first.endpoint().close().await;
    first.foreground().await?;
    rocky(&second, &first).await?;
    first.close().await;
    second.close().await;
    Ok(())
}

#[tokio::test]
async fn foreground_reports_a_dead_borrowed_endpoint() -> Result<(), Failure> {
    let endpoint = local().bind([33; 32]).await?;
    let attachment = Attachment::new(endpoint.clone());
    endpoint.close().await;
    let error = attachment
        .foreground()
        .await
        .err()
        .ok_or("dead endpoint accepted")?;
    assert!(
        error
            .to_string()
            .contains("application-owned endpoint is dead")
    );
    assert_eq!(attachment.endpoint().id(), endpoint.id());
    Ok(())
}

#[tokio::test]
async fn relay_notifications_preserve_an_existing_direct_stream() -> Result<(), Failure> {
    use rocky_iroh::iroh::{RelayMode, endpoint::presets, tls::CaTlsConfig};
    let (map, url, server) = rocky_iroh::testing::run_relay_server().await?;
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Custom(map))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0")?
        .bind()
        .await?;
    let attachment = Attachment::new(endpoint.clone());
    let router = attachment
        .register(Router::builder(endpoint.clone()))
        .spawn();
    attachment.listen(Some(TEST_SCOPE), true).expect("listen");
    let mut changes = attachment.connectivity().ok_or("missing relay watcher")?;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if attachment.relays().is_some_and(|relays| {
                relays
                    .iter()
                    .any(|relay| relay.url == url.to_string() && relay.connected)
            }) {
                break;
            }
            changes.changed().await?;
        }
        Ok::<_, Failure>(())
    })
    .await??;
    assert_eq!(attachment.initial_relay_configuration(), None);
    let other = local().start([61; 32]).await?;
    let mut near = other
        .session(&[attachment.address().await?], TEST_SCOPE)
        .await?;
    near.write_all(b"init").await?;
    let Incoming::Stream(_, mut far) = attachment.accept().await.ok_or("closed")? else {
        return Err("stream expected".into());
    };
    let mut initial = [0; 4];
    far.read_exact(&mut initial).await?;
    assert!(
        other
            .paths(&attachment.address().await?)
            .await?
            .iter()
            .any(|path| path.selected && path.kind == "direct")
    );
    server.shutdown().await?;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if attachment
                .relays()
                .is_some_and(|relays| !relays.iter().any(|relay| relay.connected))
            {
                break;
            }
            changes.changed().await?;
        }
        Ok::<_, Failure>(())
    })
    .await??;
    near.write_all(b"ping").await?;
    let mut request = [0; 4];
    far.read_exact(&mut request).await?;
    assert_eq!(&request, b"ping");
    drop((near, far));
    other.close().await;
    attachment.close().await;
    assert!(!endpoint.is_closed());
    router.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_link_has_four_request_streams_and_an_independent_control_stream() -> Result<(), Failure>
{
    let a = local().start([51; 32]).await?;
    let b = local().start([52; 32]).await?;
    let (a, b) = (a.transport(), b.transport());
    b.listen(Some(TEST_SCOPE), true).expect("listen");
    let mut near = a.session(&[b.address().await?], TEST_SCOPE).await?;
    let Incoming::Stream(protocol, mut far) = b.accept().await.ok_or("closed")? else {
        return Err("stream expected".into());
    };
    assert_eq!(protocol, Protocol::Link);
    let mut outgoing = near.take_lanes();
    let mut incoming = far.take_lanes();
    assert_eq!((outgoing.len(), incoming.len()), (4, 4));
    for (index, (send, receive)) in outgoing.iter_mut().zip(&mut incoming).enumerate() {
        send.write_u8(index as u8).await?;
        assert_eq!(receive.read_u8().await?, index as u8);
    }
    outgoing[0].shutdown().await?;
    near.write_u8(7).await?;
    assert_eq!(
        far.read_u8().await?,
        7,
        "ending a request lane leaves the control stream usable"
    );
    far.write_u8(9).await?;
    assert_eq!(
        near.read_u8().await?,
        9,
        "ending a request lane leaves the control stream usable"
    );
    far.write_u8(11).await?;
    far.shutdown().await?;
    drop(incoming);
    drop(far);
    assert_eq!(near.read_u8().await?, 11);
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn closing_a_borrowed_attachment_ends_its_link_lanes_and_keeps_the_endpoint()
-> Result<(), Failure> {
    let endpoint = local().bind([53; 32]).await?;
    let attachment = Attachment::new(endpoint.clone());
    let router = attachment
        .register(Router::builder(endpoint.clone()))
        .spawn();
    let a = attachment.transport();
    let other = local().start([54; 32]).await?;
    let b = other.transport();
    b.listen(Some(TEST_SCOPE), true).expect("listen");
    let _near = a.session(&[b.address().await?], TEST_SCOPE).await?;
    let Incoming::Stream(_, mut far) = b.accept().await.ok_or("closed")? else {
        return Err("stream expected".into());
    };
    let mut lanes = far.take_lanes();
    a.close().await;
    assert!(!endpoint.is_closed());
    assert!(far.read_u8().await.is_err());
    for lane in &mut lanes {
        assert!(lane.read_u8().await.is_err());
    }
    b.close().await;
    router.shutdown().await?;
    Ok(())
}

const HANG_GUARD: std::time::Duration = std::time::Duration::from_secs(30);

#[tokio::test]
async fn closing_ends_a_pending_accept() -> Result<(), Failure> {
    let served = local().start([51; 32]).await?.transport();
    served.listen(Some(TEST_SCOPE), true).expect("listen");
    let accept = served.accept();
    tokio::pin!(accept);
    tokio::select! {
        biased;
        _ = &mut accept => return Err("nothing was dialed".into()),
        () = tokio::task::yield_now() => {}
    }
    let (_, accepted) = tokio::time::timeout(HANG_GUARD, async {
        tokio::join!(served.close(), &mut accept)
    })
    .await
    .map_err(|_| "close waited for a pending accept")?;
    assert!(accepted.is_none());
    Ok(())
}

#[tokio::test]
async fn an_unknown_scope_keeps_another_databases_link_open() -> Result<(), Failure> {
    let server = local().start([91; 32]).await?;
    let client = local().start([92; 32]).await?;
    let scope = rocky_core::Scope {
        mesh: rocky_core::MeshId([1; 16]),
        app: rocky_core::AppId([2; 16]),
        domain: rocky_core::DomainId([3; 16]),
    };
    let port = server.transport();
    port.listen(Some(scope), true)?;
    let routes = [server.address().await?];
    let mut near = client.session(&routes, scope).await?;
    let Incoming::Stream(Protocol::Link, mut far) = port.accept().await.ok_or("closed")? else {
        return Err("link expected".into());
    };
    let missing = rocky_core::Scope {
        domain: rocky_core::DomainId([4; 16]),
        ..scope
    };
    let refused = client
        .request(&routes, missing, b"never dispatched")
        .await
        .err()
        .ok_or("unknown scope accepted")?;
    assert_eq!(
        refused.downcast_ref::<rocky_core::net::ScopeRefusal>(),
        Some(&rocky_core::net::ScopeRefusal::UnknownScope)
    );
    let refused = client
        .session(&routes, missing)
        .await
        .err()
        .ok_or("unknown scope accepted")?;
    assert_eq!(
        refused.downcast_ref::<rocky_core::net::ScopeRefusal>(),
        Some(&rocky_core::net::ScopeRefusal::UnknownScope)
    );
    near.write_all(b"still open").await?;
    let mut bytes = [0; 10];
    far.read_exact(&mut bytes).await?;
    assert_eq!(&bytes, b"still open");
    drop((near, far));
    tokio::join!(server.close(), client.close());
    Ok(())
}
