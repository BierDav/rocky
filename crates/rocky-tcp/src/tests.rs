use super::*;
use rocky_core::{AppId, DomainId, MeshId};
use tokio::io::AsyncReadExt;

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
    let address = port.address().await?;
    Ok((attachment, port, address))
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
    let caller = Options::dialer().transport();
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
    let unused = TcpListener::bind("127.0.0.1:0")
        .await?
        .local_addr()?
        .to_string();
    let caller = Options::dialer().transport();
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
    let caller = Options::dialer().transport();
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
    let caller = Options::dialer().transport();
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
    let caller = Options::dialer().transport();
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
    let caller = Options::dialer().transport();
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
    let caller = Options::dialer().transport();
    for protocol in [Protocol::Rpc, Protocol::Link] {
        let Err(error) = caller.connect("127.0.0.1:1", protocol, None).await else {
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
async fn a_dialer_cannot_listen_or_name_an_address() -> Result<(), Failure> {
    let dialer = Options::dialer().transport();
    assert!(dialer.listen(Some(scope(1)), true).is_err());
    assert!(dialer.address().await.is_err());
    Ok(())
}

#[tokio::test]
async fn the_advertised_address_replaces_the_bound_one() -> Result<(), Failure> {
    let attachment = loopback()
        .advertise("david-pixel.netbird.cloud:7411")
        .start()
        .await?;
    assert_eq!(
        attachment.transport().address().await?,
        "david-pixel.netbird.cloud:7411"
    );
    attachment.close().await;
    Ok(())
}

#[tokio::test]
async fn an_unsupported_hello_version_is_refused_before_dispatch() -> Result<(), Failure> {
    let (server, port, address) = served().await?;
    port.listen(Some(scope(1)), true)?;
    let mut raw = TcpStream::connect(&address).await?;
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
    let address = connected.address().await?;
    let transport = connected.transport.clone();
    let accept = tokio::spawn(async move { transport.accept().await.is_none() });
    connected.close().await;
    assert!(accept.await?, "accept outlived the closed port");
    let caller = Options::dialer().transport();
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
    let caller = Options::dialer().transport();
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
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

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
        let address = port.address().await?;
        assert!(address.starts_with("ws://127.0.0.1:"), "{address}");
        port.listen(Some(scope(1)), true)?;
        let answer = tokio::spawn(answer_once(port.clone()));
        let caller = Options::dialer().transport();
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
        let caller = Options::dialer().transport();
        let mut session = caller.session(&[port.address().await?], scope(1)).await?;
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
        assert_eq!(port.address().await?, "wss://prism.example/rocky");
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

    #[test]
    fn the_accept_key_matches_rfc_6455() {
        assert_eq!(
            websocket_accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
}
