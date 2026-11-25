use super::*;
use rocky_core::{AppId, DomainId, MeshId};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
};

fn scope(byte: u8) -> Scope {
    Scope {
        mesh: MeshId([byte; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    }
}

fn loopback() -> Options {
    Options::new(SocketAddr::from(([127, 0, 0, 1], 0)))
}

async fn served() -> Result<(Attachment, Arc<dyn Transport>, String), Failure> {
    let attachment = loopback().start().await?;
    let port = attachment.transport();
    let address = port.address().await?.ok_or("no route")?;
    Ok((attachment, port, address))
}

async fn dialer() -> Result<Arc<dyn Transport>, Failure> {
    Ok(Options::dialer().start().await?.transport())
}

fn refusal(error: &Failure) -> Option<ScopeRefusal> {
    error.downcast_ref::<ScopeRefusal>().cloned()
}

#[tokio::test]
async fn a_request_reaches_the_listening_scope_and_returns_its_answer() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    port.listen(Some(scope(1)), true)?;
    let answering = port.clone();
    let answer = tokio::spawn(async move {
        match answering.accept().await {
            Some(Incoming::Request(request)) => {
                let reply: Vec<u8> = request.bytes.iter().rev().copied().collect();
                request.respond(reply).await
            }
            _ => Err("expected a request".into()),
        }
    });
    let caller = dialer().await?;
    let reply = caller.request(&[address], scope(1), b"ping").await?;
    assert_eq!(reply, b"gnip");
    answer.await??;
    server.close().await;
    Ok(())
}

#[tokio::test]
async fn a_request_tries_the_next_address_when_one_fails() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    port.listen(Some(scope(1)), true)?;
    let answering = port.clone();
    let answer = tokio::spawn(async move {
        match answering.accept().await {
            Some(Incoming::Request(request)) => request.respond(b"ok".to_vec()).await,
            _ => Err("expected a request".into()),
        }
    });
    let unused = format!(
        "tcp://{}",
        TcpListener::bind("127.0.0.1:0").await?.local_addr()?
    );
    let caller = dialer().await?;
    let reply = caller.request(&[unused, address], scope(1), b"hi").await?;
    assert_eq!(reply, b"ok");
    answer.await??;
    server.close().await;
    Ok(())
}

#[tokio::test]
async fn a_session_hands_its_link_stream_to_accept() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    port.listen(Some(scope(1)), true)?;
    let caller = dialer().await?;
    let mut session = caller.session(&[address], scope(1)).await?;
    framing::write(&mut session, b"hello").await?;
    let Some(Incoming::Stream(Protocol::Link, mut inbound)) = port.accept().await else {
        return Err("expected a link stream".into());
    };
    assert_eq!(framing::read(&mut inbound).await?, b"hello");
    framing::write(&mut inbound, b"back").await?;
    assert_eq!(framing::read(&mut session).await?, b"back");
    server.close().await;
    Ok(())
}

#[tokio::test]
async fn coordination_reaches_the_unscoped_route() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    port.listen(None, true)?;
    let caller = dialer().await?;
    let mut stream = caller.connect(&address, Protocol::Coordinate, None).await?;
    framing::write(&mut stream, b"vote").await?;
    let Some(Incoming::Stream(Protocol::Coordinate, mut inbound)) = port.accept().await else {
        return Err("expected a coordination stream".into());
    };
    assert_eq!(framing::read(&mut inbound).await?, b"vote");
    server.close().await;
    Ok(())
}

#[tokio::test]
async fn an_unknown_scope_is_refused_and_counted() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    port.listen(Some(scope(1)), true)?;
    let caller = dialer().await?;
    let Err(error) = caller
        .connect(&address, Protocol::Rpc, Some(scope(9)))
        .await
    else {
        return Err("a foreign scope was admitted".into());
    };
    assert_eq!(refusal(&error), Some(ScopeRefusal::UnknownScope));
    assert_eq!(server.refused(), 1);
    server.close().await;
    Ok(())
}

#[tokio::test]
async fn a_scope_that_stopped_listening_is_refused() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    port.listen(Some(scope(1)), true)?;
    let other = server.transport();
    other.listen(Some(scope(2)), true)?;
    other.listen(Some(scope(2)), false)?;
    port.listen(Some(scope(1)), false)?;
    let caller = dialer().await?;
    for scope in [scope(1), scope(2)] {
        let Err(error) = caller.connect(&address, Protocol::Link, Some(scope)).await else {
            return Err("a scope that stopped listening admitted a link".into());
        };
        assert_eq!(refusal(&error), Some(ScopeRefusal::UnknownScope));
    }
    server.close().await;
    Ok(())
}

#[tokio::test]
async fn scoped_protocols_need_a_scope_before_dialing() -> Result<(), Failure> {
    let caller = dialer().await?;
    for protocol in [Protocol::Rpc, Protocol::Link] {
        let Err(error) = caller.connect("tcp://127.0.0.1:1", protocol, None).await else {
            return Err("an unscoped dial was attempted".into());
        };
        assert_eq!(refusal(&error), Some(ScopeRefusal::UnknownScope));
    }
    Ok(())
}

#[tokio::test]
async fn another_port_cannot_take_a_listening_scope() -> Result<(), Failure> {
    let (server, port, _) = served().await?;
    port.listen(Some(scope(1)), true)?;
    let other = server.transport();
    let Err(error) = other.listen(Some(scope(1)), true) else {
        return Err("two ports listened on one scope".into());
    };
    assert_eq!(refusal(&error), Some(ScopeRefusal::AlreadyAttached));
    let Err(error) = port.listen(Some(scope(2)), true) else {
        return Err("a listening port moved to another scope".into());
    };
    assert_eq!(refusal(&error), Some(ScopeRefusal::AlreadyAttached));
    server.close().await;
    Ok(())
}

#[tokio::test]
async fn a_dialer_is_dial_only_names_no_route_and_cannot_listen() -> Result<(), Failure> {
    let dialer = dialer().await?;
    assert!(dialer.dial_only());
    assert_eq!(dialer.address().await?, None);
    assert!(dialer.listen(Some(scope(1)), true).is_err());
    Ok(())
}

#[tokio::test]
async fn a_listener_claims_tcp_routes_and_refuses_other_schemes() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    assert!(!port.dial_only());
    assert!(address.starts_with("tcp://127.0.0.1:"), "{address}");
    assert!(port.claims(&address));
    assert!(port.claims("TCP://10.0.0.2:4000"));
    assert!(!port.claims("iroh:eyJpZCI6IiJ9"));
    assert!(!port.claims("10.0.0.2:4000"));
    let Err(error) = port
        .connect("iroh:eyJpZCI6IiJ9", Protocol::Coordinate, None)
        .await
    else {
        return Err("an iroh: route was dialed over TCP".into());
    };
    assert!(error.to_string().contains("tcp://"), "{error}");
    server.close().await;
    Ok(())
}

#[tokio::test]
async fn the_advertised_address_replaces_the_bound_one() -> Result<(), Failure> {
    let attachment = loopback()
        .advertise("tcp://david-pixel.netbird.cloud:7411")
        .start()
        .await?;
    assert_eq!(
        attachment.transport().address().await?.as_deref(),
        Some("tcp://david-pixel.netbird.cloud:7411")
    );
    attachment.close().await;
    Ok(())
}

#[tokio::test]
async fn an_advertised_route_without_a_scheme_is_refused_at_start() {
    let refused = loopback()
        .advertise("david-pixel.netbird.cloud:7411")
        .start()
        .await;
    assert!(refused.is_err(), "a bare host:port was advertised");
}

#[tokio::test]
async fn an_unsupported_hello_version_is_refused_before_dispatch() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    port.listen(Some(scope(1)), true)?;
    let mut raw = TcpStream::connect(address.trim_start_matches("tcp://")).await?;
    framing::write(
        &mut raw,
        &rocky_credentials::encode(&Hello {
            version: VERSION + 1,
            protocol: Wire::Link,
            scope: Some(scope(1)),
        })?,
    )
    .await?;
    let reply: Accepted = rocky_credentials::decode_bounded(
        &framing::read_bounded(&mut raw, MAX_HEADER).await?,
        MAX_HEADER,
    )?;
    assert_eq!(reply.result, Err(ScopeRefusal::UnsupportedVersion));
    let mut rest = Vec::new();
    raw.read_to_end(&mut rest).await?;
    assert!(rest.is_empty());
    server.close().await;
    assert!(port.accept().await.is_none());
    Ok(())
}

#[tokio::test]
async fn closing_an_owned_port_ends_accept_and_the_attachment() -> Result<(), Failure> {
    let connected = Connector::start(&loopback(), true, [0; 32]).await?;
    connected.listen(Some(scope(1)), true)?;
    let address = connected.address().await?.ok_or("no route")?;
    let transport = connected.transport.clone();
    let accept = tokio::spawn(async move { transport.accept().await.is_none() });
    connected.close().await;
    assert!(accept.await?, "accept outlived the closed port");
    let caller = dialer().await?;
    assert!(
        caller.request(&[address], scope(1), b"late").await.is_err(),
        "a closed attachment answered"
    );
    Ok(())
}

#[tokio::test]
async fn closing_a_borrowed_port_keeps_the_attachment_serving() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    port.listen(Some(scope(1)), true)?;
    port.close().await;
    let next = server.transport();
    next.listen(Some(scope(1)), true)?;
    let answering = next.clone();
    let answer = tokio::spawn(async move {
        match answering.accept().await {
            Some(Incoming::Request(request)) => request.respond(b"still here".to_vec()).await,
            _ => Err("expected a request".into()),
        }
    });
    let caller = dialer().await?;
    assert_eq!(
        caller.request(&[address], scope(1), b"?").await?,
        b"still here"
    );
    answer.await??;
    server.close().await;
    Ok(())
}

#[cfg(feature = "websocket")]
mod websocket {
    use super::*;
    use tokio_tungstenite::{
        WebSocketStream,
        tungstenite::{Message, protocol::Role},
    };

    async fn answer_once(port: Arc<dyn Transport>) -> Result<(), Failure> {
        match port.accept().await {
            Some(Incoming::Request(request)) => {
                let reply: Vec<u8> = request.bytes.iter().rev().copied().collect();
                request.respond(reply).await
            }
            _ => Err("expected a request".into()),
        }
    }

    #[tokio::test]
    async fn a_websocket_listener_advertises_a_ws_url_and_answers() -> Result<(), Failure> {
        let server = loopback().websocket().start().await?;
        let port = server.transport();
        let address = port.address().await?.ok_or("no route")?;
        assert!(address.starts_with("ws://127.0.0.1:"), "{address}");
        port.listen(Some(scope(1)), true)?;
        let answer = tokio::spawn(answer_once(port.clone()));
        let caller = dialer().await?;
        assert_eq!(
            caller.request(&[address], scope(1), b"ping").await?,
            b"gnip"
        );
        answer.await??;
        server.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn a_link_session_carries_frames_larger_than_one_message() -> Result<(), Failure> {
        let server = loopback().websocket().start().await?;
        let port = server.transport();
        port.listen(Some(scope(1)), true)?;
        let caller = dialer().await?;
        let route = port.address().await?.ok_or("no route")?;
        let mut session = caller.session(&[route], scope(1)).await?;
        let large: Vec<u8> = (0..300_000u32).map(|value| (value % 251) as u8).collect();
        framing::write(&mut session, &large).await?;
        let Some(Incoming::Stream(Protocol::Link, mut inbound)) = port.accept().await else {
            return Err("expected a link stream".into());
        };
        assert_eq!(framing::read(&mut inbound).await?, large);
        framing::write(&mut inbound, b"back").await?;
        assert_eq!(framing::read(&mut session).await?, b"back");
        server.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn an_external_attachment_admits_an_upgraded_connection() -> Result<(), Failure> {
        let attachment = Attachment::external("wss://prism.example/rocky");
        let port = attachment.transport();
        assert_eq!(
            port.address().await?.as_deref(),
            Some("wss://prism.example/rocky")
        );
        port.listen(Some(scope(1)), true)?;
        let (client, upgraded) = tokio::io::duplex(1 << 16);
        let admitting = attachment.clone();
        let admitted = tokio::spawn(async move { admitting.admit_websocket(upgraded).await });
        let mut client = super::super::websocket::Io::new_for_tests(
            WebSocketStream::from_raw_socket(client, Role::Client, None).await,
        );
        framing::write(
            &mut client,
            &rocky_credentials::encode(&Hello {
                version: VERSION,
                protocol: Wire::Link,
                scope: Some(scope(1)),
            })?,
        )
        .await?;
        let reply: Accepted = rocky_credentials::decode_bounded(
            &framing::read_bounded(&mut client, MAX_HEADER).await?,
            MAX_HEADER,
        )?;
        assert_eq!(reply.result, Ok(()));
        admitted.await??;
        let Some(Incoming::Stream(Protocol::Link, mut inbound)) = port.accept().await else {
            return Err("expected a link stream".into());
        };
        framing::write(&mut client, b"over the proxy").await?;
        assert_eq!(framing::read(&mut inbound).await?, b"over the proxy");
        attachment.close().await;
        Ok(())
    }

    async fn pair() -> (
        WebSocketStream<tokio::io::DuplexStream>,
        super::super::websocket::Io<tokio::io::DuplexStream>,
    ) {
        let (client, server) = tokio::io::duplex(1 << 20);
        let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        (client, websocket_stream(server).await)
    }

    #[tokio::test]
    async fn a_received_close_is_answered_before_eof() -> Result<(), Failure> {
        use futures_util::{SinkExt, StreamExt};
        let (mut client, mut server) = pair().await;
        client.send(Message::Close(None)).await?;
        let mut rest = Vec::new();
        server.read_to_end(&mut rest).await?;
        assert!(rest.is_empty());
        drop(server);
        assert!(matches!(client.next().await, Some(Ok(Message::Close(_)))));
        Ok(())
    }

    #[tokio::test]
    async fn an_oversized_message_is_refused_before_admission() -> Result<(), Failure> {
        use futures_util::SinkExt;
        let (mut client, mut server) = pair().await;
        client
            .send(Message::Binary(vec![0; 200_000].into()))
            .await?;
        let mut byte = [0; 1];
        assert!(server.read(&mut byte).await.is_err());
        Ok(())
    }

    struct PrivateCa {
        root: Vec<u8>,
        server: Arc<rustls::ServerConfig>,
    }

    fn private_ca() -> Result<PrivateCa, Failure> {
        let mut authority = rcgen::CertificateParams::new(Vec::<String>::new())?;
        authority.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let authority =
            rcgen::CertifiedIssuer::self_signed(authority, rcgen::KeyPair::generate()?)?;
        let key = rcgen::KeyPair::generate()?;
        let leaf = rcgen::CertificateParams::new(vec!["localhost".to_owned()])?
            .signed_by(&key, &authority)?;
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )?;
        Ok(PrivateCa {
            root: authority.der().to_vec(),
            server: Arc::new(server),
        })
    }

    async fn serve_wss(attachment: &Attachment, ca: &PrivateCa) -> Result<String, Failure> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let route = format!("wss://localhost:{}", listener.local_addr()?.port());
        let acceptor = tokio_rustls::TlsAcceptor::from(ca.server.clone());
        let shared = attachment.0.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let shared = shared.clone();
                tokio::spawn(async move {
                    let tls = acceptor.accept(tcp).await?;
                    let socket = tokio_tungstenite::accept_async(tls).await?;
                    platform::admit(shared, super::super::websocket::Io::new_for_tests(socket))
                        .await
                });
            }
        });
        Ok(route)
    }

    #[tokio::test]
    async fn root_certificates_let_wss_trust_a_private_ca() -> Result<(), Failure> {
        let ca = private_ca()?;
        let server = Attachment::external("wss://localhost");
        let port = server.transport();
        port.listen(Some(scope(1)), true)?;
        let route = serve_wss(&server, &ca).await?;

        let public_roots_only = dialer().await?;
        assert!(
            public_roots_only
                .request(std::slice::from_ref(&route), scope(1), b"ping")
                .await
                .is_err(),
            "a certificate from a private CA passed the public web roots"
        );

        let answer = tokio::spawn(answer_once(port.clone()));
        let trusting = Options::dialer()
            .root_certificates([ca.root.clone()])
            .start()
            .await?
            .transport();
        assert_eq!(
            trusting.request(&[route], scope(1), b"ping").await?,
            b"gnip"
        );
        answer.await??;
        server.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn a_root_certificate_that_does_not_parse_fails_start() {
        let started = Options::dialer()
            .root_certificates([b"not a certificate".to_vec()])
            .start()
            .await;
        assert!(started.is_err(), "garbage was trusted as a root");
    }

    #[test]
    fn the_accept_key_matches_rfc_6455() {
        assert_eq!(
            websocket_accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
}
