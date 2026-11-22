use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_iroh::{
    Attachment, Options,
    iroh::{
        Endpoint, EndpointAddr,
        endpoint::Connection,
        protocol::{AcceptError, ProtocolHandler, Router},
    },
};
use rocky_sdk::Network;
use serde_json::json;
use std::sync::Arc;

use crate::common;

const ECHO: &[u8] = b"example/echo/1";

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
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
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
async fn signal_renewed_reachability(device: &Device) {
    snap::ok(device.session.set_networking_enabled(true)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_database_borrows_an_application_endpoint_and_leaves_it_open() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
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
    let starter = Network::Owned(Arc::new(rocky_iroh::Options {
        relay: Some("disabled".into()),
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
        bind: Some("127.0.0.1:0".into()),
    }));
    let b = Device::join(&root.path().join("b"), &a.invite().await, starter)?;
    a.add(1, "Milk").await;
    b.add(2, "Eggs").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    assert_eq!(echo(&caller, endpoint.addr()).await?, b"hello");
    let status = snap::ok(a.session.network()).await;
    assert_eq!(
        (&status["kind"], &status["ownership"]),
        (&json!("iroh"), &json!("application"))
    );

    a.network.as_ref().ok_or("attached")?.close().await;
    a.add(3, "Jam").await;
    assert!(
        common::exchange_failed(&b.sync().await),
        "closing Rocky's service stops sync"
    );
    assert_eq!(
        echo(&caller, endpoint.addr()).await?,
        b"hello",
        "closing Rocky's service leaves the application's protocol serving"
    );
    a.database
        .attach(Network::Borrowed(attachment.transport()))
        .await?;
    signal_renewed_reachability(&b).await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        b.titles().await,
        ["Eggs", "Jam", "Milk"],
        "the same attachment serves again once a service uses it"
    );

    a.close().await;
    assert!(
        !endpoint.is_closed() && !router.is_shutdown(),
        "closing the borrowing database leaves the endpoint and protocol usable"
    );
    assert_eq!(echo(&caller, endpoint.addr()).await?, b"hello");
    let a = Device::open(
        &root.path().join("a"),
        Some(Network::Borrowed(attachment.transport())),
    )?;
    assert_eq!(a.titles().await, ["Eggs", "Jam", "Milk"]);
    a.close().await;

    let ((), shutdown, ()) = tokio::join!(b.close(), router.shutdown(), caller.close());
    shutdown?;
    assert!(
        endpoint.is_closed(),
        "the final shutdown is the application's"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_databases_share_each_endpoint_and_detach_independently() -> Result<(), Failure> {
    use rocky_core::net::{ScopeRefusal, Transport};
    let root = tempfile::tempdir()?;
    let left = local().bind([71; 32]).await?;
    let right = local().bind([72; 32]).await?;
    let left_harness = Attachment::new(left.clone());
    let right_harness = Attachment::new(right.clone());
    let left_router = left_harness
        .register(Router::builder(left.clone()).accept(ECHO, Echo))
        .spawn();
    let right_router = right_harness
        .register(Router::builder(right.clone()))
        .spawn();
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Borrowed(left_harness.transport())),
    )?;
    let b = Device::create(
        &root.path().join("b"),
        Some(Network::Borrowed(left_harness.transport())),
    )?;
    assert_ne!(a.scope(), b.scope());
    let a_member = Device::join(
        &root.path().join("a-member"),
        &a.invite().await,
        Network::Borrowed(right_harness.transport()),
    )?;
    let b_member = Device::join(
        &root.path().join("b-member"),
        &b.invite().await,
        Network::Borrowed(right_harness.transport()),
    )?;
    a.add(1, "Only A").await;
    b.add(1, "Only B").await;
    a_member.sync().await.map_err(|error| error.to_string())?;
    b_member.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a_member.titles().await, ["Only A"]);
    assert_eq!(b_member.titles().await, ["Only B"]);

    let mut missing = *a.scope();
    missing.domain = rocky_core::DomainId([255; 16]);
    let routes = [left_harness.address().await?];
    let refusal = right_harness
        .request(&routes, missing, b"never dispatched")
        .await
        .err()
        .ok_or("unknown RPC scope accepted")?;
    assert_eq!(
        refusal.downcast_ref::<ScopeRefusal>(),
        Some(&ScopeRefusal::UnknownScope)
    );
    let refusal = right_harness
        .session(&routes, missing)
        .await
        .err()
        .ok_or("unknown link scope accepted")?;
    assert_eq!(
        refusal.downcast_ref::<ScopeRefusal>(),
        Some(&ScopeRefusal::UnknownScope)
    );

    a.network.as_ref().ok_or("a network")?.close().await;
    b.add(2, "B after A detached").await;
    b_member.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(b_member.titles().await, ["B after A detached", "Only B"]);
    assert_eq!(echo(&right, left.addr()).await?, b"hello");
    assert!(!left.is_closed());
    tokio::join!(a.close(), b.close(), a_member.close(), b_member.close());
    left_router.shutdown().await?;
    right_router.shutdown().await?;
    Ok(())
}

fn personal(name: &str) -> (rocky_sdk::VerifiedPasskeyPrf, rocky_sdk::PersonalNamespace) {
    let namespace = rocky_sdk::PersonalNamespace::new(format!("{name}.example"), "account root")
        .expect("namespace");
    let input = namespace.prf_input().expect("input");
    (
        rocky_sdk::VerifiedPasskeyPrf::new(
            &input,
            rocky_core::commitment("fixture", name.as_bytes()),
        ),
        namespace,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_personal_databases_share_an_endpoint_and_a_coordinator_and_detach_independently()
-> Result<(), Failure> {
    use rocky_sdk::{Configuration, GrantProfile, Schema, counters};
    let root = tempfile::tempdir()?;
    let left = local().bind([81; 32]).await?;
    let right = local().bind([82; 32]).await?;
    let left_harness = Attachment::new(left.clone());
    let right_harness = Attachment::new(right.clone());
    let left_router = left_harness
        .register(Router::builder(left.clone()).accept(ECHO, Echo))
        .spawn();
    let right_router = right_harness
        .register(Router::builder(right.clone()))
        .spawn();
    let logs = rocky_coordinator::coordinated::MemoryLog::new();
    let shared_coordinator: Arc<dyn rocky_sdk::Coordinator> =
        Arc::new(logs.client(rocky_sdk::credentials::PrivateKey::new().public_key()));
    let schema = Schema::parse(common::SCHEMA)?;
    let mut owners = Vec::new();
    let mut custodians = Vec::new();
    for name in ["a", "b"] {
        let (prf, namespace) = personal(name);
        let owner = Device::new(
            Configuration::new(root.path().join(name), &schema)?
                .network(Network::Borrowed(left_harness.transport()))
                .coordination(shared_coordinator.clone())
                .create_personal(prf, namespace)
                .await?
                .opened,
        )?;
        let invitation = owner
            .session
            .provision(
                GrantProfile::Custody {
                    scopes: ["items".to_owned()].into(),
                },
                None,
            )
            .await?
            .uri;
        let custodian = Device::new(
            Configuration::new(root.path().join(format!("{name}-custody")), &schema)?
                .network(Network::Borrowed(right_harness.transport()))
                .join(&invitation)
                .await?,
        )?;
        common::introduce(&owner, &custodian).await;
        owners.push(owner);
        custodians.push(custodian);
    }
    let (a, b) = (&owners[0], &owners[1]);
    assert_ne!(a.scope(), b.scope());
    a.add(1, "Only A").await;
    b.add(1, "Only B").await;
    for owner in [a, b] {
        let report = owner.sync().await.map_err(|error| error.to_string())?;
        assert!(common::reconciled(&report), "{report}");
    }
    let counter = counters::Name::new("orders")?;
    assert_eq!(
        a.database
            .claim(counter.clone(), counters::Claim::fresh()?)
            .await?,
        1
    );
    assert_eq!(
        b.database
            .claim(counter.clone(), counters::Claim::fresh()?)
            .await?,
        1
    );

    a.network.as_ref().ok_or("a network")?.close().await;
    assert!(common::exchange_failed(&a.sync().await));
    b.add(2, "B after A detached").await;
    let report = b.sync().await.map_err(|error| error.to_string())?;
    assert!(
        common::reconciled(&report),
        "detaching A leaves B's routes: {report}"
    );
    assert_eq!(
        b.database
            .claim(counter.clone(), counters::Claim::fresh()?)
            .await?,
        2,
        "detaching A leaves the coordinator serving B"
    );
    assert_eq!(
        a.database
            .claim(counter.clone(), counters::Claim::fresh()?)
            .await?,
        2,
        "detaching A leaves the coordinator serving A"
    );
    assert_eq!(echo(&right, left.addr()).await?, b"hello");
    assert!(
        !left.is_closed() && !left_router.is_shutdown(),
        "detaching A leaves the endpoint open"
    );

    let mut owners = owners.into_iter();
    let (a, b) = (owners.next().ok_or("a")?, owners.next().ok_or("b")?);
    a.close().await;
    b.add(3, "B after A closed").await;
    let report = b.sync().await.map_err(|error| error.to_string())?;
    assert!(
        common::reconciled(&report),
        "closing A leaves B syncing: {report}"
    );
    assert_eq!(
        b.database.claim(counter, counters::Claim::fresh()?).await?,
        3,
        "closing A leaves the coordinator serving B"
    );
    assert_eq!(echo(&right, left.addr()).await?, b"hello");
    b.close().await;
    for custodian in custodians {
        custodian.close().await;
    }
    left_router.shutdown().await?;
    right_router.shutdown().await?;
    Ok(())
}
