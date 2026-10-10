//! Who owns the endpoint: Rocky's handlers next to an application's own
//! protocol on its router, and the starter that owns everything it binds.
//! Loopback only; see `docs/runtime/sdk-transports.md` for what that proves.
use rocky_core::net::{Failure, Protocol, Transport};
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

/// The application's own protocol: echoes one stream.
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
/// One request and reply over a Rocky stream between two transports.
async fn rocky(from: &dyn Transport, to: &dyn Transport) -> Result<(), Failure> {
    let address = to.address().await?;
    let mut near = from.connect(&address, Protocol::Sync).await?;
    near.write_all(b"ping").await?;
    let (protocol, mut far) = to.accept().await.ok_or("closed")?;
    assert_eq!(protocol, Protocol::Sync);
    let mut request = [0; 4];
    far.read_exact(&mut request).await?;
    far.write_all(b"pong").await?;
    let mut reply = [0; 4];
    near.read_exact(&mut reply).await?;
    assert_eq!((&request, &reply), (b"ping", b"pong"));
    Ok(())
}

/// Whether a Rocky stream to `address` fails before any reply.
async fn refused(from: &dyn Transport, address: &str) -> bool {
    let attempt = async {
        let mut stream = from.connect(address, Protocol::Sync).await?;
        stream.write_all(b"ping").await?;
        let mut reply = [0; 4];
        stream.read_exact(&mut reply).await?;
        Ok::<_, Failure>(())
    };
    attempt.await.is_err()
}

#[tokio::test]
async fn rocky_and_an_application_protocol_share_an_application_endpoint() -> Result<(), Failure> {
    // The application binds the endpoint and owns the router.
    let endpoint = local().bind([7; 32]).await?;
    let attachment = Attachment::new(endpoint.clone());
    let router = attachment
        .register(Router::builder(endpoint.clone()).accept(ECHO, Echo))
        .spawn();
    let served = attachment.transport();
    let other = local().start([8; 32]).await?;
    let dialing = other.transport();
    // Until a service listens, Rocky's ALPNs are refused; the echo is not.
    assert!(refused(&*dialing, &served.address().await?).await);
    assert_eq!(attachment.refused(), 1);
    assert_eq!(echo(&other.endpoint(), endpoint.addr()).await?, b"hello");
    served.listen(true);
    dialing.listen(true);

    rocky(&*dialing, &*served).await?;
    rocky(&*served, &*dialing).await?;
    assert_eq!(echo(&other.endpoint(), endpoint.addr()).await?, b"hello");

    // Closing Rocky's side releases its streams and refuses its ALPNs.
    served.close().await;
    served.close().await;
    assert!(served.accept().await.is_none());
    let address = served.address().await?;
    // A stream on the connection made earlier fails; a fresh dial is refused.
    for _ in 0..2 {
        assert!(refused(&*dialing, &address).await);
    }
    assert!(attachment.refused() >= 1);
    // The endpoint, the router and the application's protocol are untouched.
    assert!(!endpoint.is_closed() && !router.is_shutdown());
    assert_eq!(echo(&other.endpoint(), endpoint.addr()).await?, b"hello");

    // The final shutdown is the application's.
    router.shutdown().await?;
    assert!(endpoint.is_closed());
    dialing.close().await;
    Ok(())
}

#[tokio::test]
async fn the_starter_shuts_down_everything_it_bound() -> Result<(), Failure> {
    let first = local().start([1; 32]).await?;
    let second = local().start([2; 32]).await?;
    assert_ne!(first.endpoint().id(), second.endpoint().id());
    // The endpoint key follows the stable transport key.
    assert_eq!(first.endpoint().id(), local().bind([1; 32]).await?.id());
    let (near, far) = (first.transport(), second.transport());
    far.listen(true);
    rocky(&*near, &*far).await?;
    near.close().await;
    far.close().await;
    far.close().await;
    assert!(first.endpoint().is_closed() && second.endpoint().is_closed());
    assert!(
        near.connect(&far.address().await?, Protocol::Pair)
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
    first.listen(true);
    second.listen(true);
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
