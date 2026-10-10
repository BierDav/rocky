//! A database on an iroh endpoint the application owns, next to the
//! application's own protocol. Loopback only: no relay, no address lookup.
#![cfg(feature = "iroh")]
use common::{Device, Failure};
use rocky_sdk::{
    Network,
    iroh::{
        Attachment, Options,
        iroh::{
            Endpoint, EndpointAddr,
            endpoint::Connection,
            protocol::{AcceptError, ProtocolHandler, Router},
        },
    },
    transport::TransportOptions,
};
use serde_json::json;
use std::sync::Arc;

mod common;

const ECHO: &[u8] = b"example/echo/1";

/// The application's own protocol. Its traffic never reaches a database.
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
    Options {
        relay: Some("disabled".into()),
        discovery: false,
        bind: Some("127.0.0.1:0".into()),
    }
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

// Opening a database blocks its caller; the application's runtime keeps
// driving the endpoint on other threads meanwhile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_database_borrows_an_application_endpoint_and_leaves_it_open() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    // The application's endpoint and router, with its protocol and Rocky's.
    let endpoint = local().bind([3; 32]).await?;
    let attachment = Attachment::new(endpoint.clone());
    let router = attachment
        .register(Router::builder(endpoint.clone()).accept(ECHO, Echo))
        .spawn();
    let caller = local().bind([4; 32]).await?;

    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Borrowed(attachment.transport())),
    )?;
    // The member uses the starter, which owns its endpoint.
    let starter = Network::Owned(Arc::new(TransportOptions::Iroh {
        relay: Some("disabled".into()),
        discovery: false,
        bind: Some("127.0.0.1:0".into()),
    }));
    let b = Device::join(&root.path().join("b"), &a.invite().await, starter)?;
    a.add(1, "Milk").await;
    b.add(2, "Eggs").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    assert_eq!(echo(&caller, endpoint.addr()).await?, b"hello");
    let status = a.ok(json!({"type": "network"})).await;
    assert_eq!(
        (&status["kind"], &status["owned"]),
        (&json!("iroh"), &json!(false))
    );

    // Closing Rocky's service stops sync and nothing else.
    a.network.as_ref().ok_or("attached")?.close().await;
    a.add(3, "Jam").await;
    assert!(b.sync().await.is_err());
    assert_eq!(echo(&caller, endpoint.addr()).await?, b"hello");
    // The same attachment serves again once a service uses it.
    a.database
        .attach(Network::Borrowed(attachment.transport()))
        .await?;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(b.titles().await, ["Eggs", "Jam", "Milk"]);

    // Closing the borrowing database leaves the endpoint and protocol usable.
    a.close().await;
    assert!(!endpoint.is_closed() && !router.is_shutdown());
    assert_eq!(echo(&caller, endpoint.addr()).await?, b"hello");
    let a = Device::open(
        &root.path().join("a"),
        Some(Network::Borrowed(attachment.transport())),
    )?;
    assert_eq!(a.titles().await, ["Eggs", "Jam", "Milk"]);
    a.close().await;

    // The final shutdown is the application's.
    let ((), shutdown, ()) = tokio::join!(b.close(), router.shutdown(), caller.close());
    shutdown?;
    assert!(endpoint.is_closed());
    Ok(())
}
