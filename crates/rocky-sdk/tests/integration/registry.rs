use crate::support::snapshot as snap;
use common::{Device, Failure, introduce};
use rocky_credentials::CredentialChain;
use rocky_sdk::{
    Network,
    transport::{
        BoxFuture, Connector, Protocol, Stream, Transport,
        memory::{Hub, Memory},
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Instant,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::common;

const REGISTRY_CAPACITY: usize = 256;
const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}],
 "roles":[{"name":"lister","tables":[{"table":"items","access":"write"},{"table":"parts","access":"read"}],"per_partition":["parts"]}]}"#;

fn open(directory: &Path, network: Option<Network>) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        crate::support::snapshot::schema(SCHEMA),
        None,
        common::prepared_identity(directory),
        network,
    )
}
fn memory(hub: &Arc<Hub>, name: &str) -> Network {
    Network::Owned(hub.connector(name))
}
async fn create(root: &Path, network: Network) -> Result<Device, Failure> {
    Device::new(open(root, Some(network)).create().await?)
}
async fn join(
    directory: &Path,
    network: Network,
    invitation: &rocky_sdk::InvitationCode,
) -> Result<Device, Failure> {
    let mut seed = directory.to_string_lossy().as_bytes().to_vec();
    seed.extend_from_slice(invitation.to_string().as_bytes());
    let signer_stable_across_lost_answers_and_restarts =
        rocky_core::commitment("fixture.join-identity", &seed);
    let identity = Arc::new(rocky_sdk::IdentityKey::from_secret(
        signer_stable_across_lost_answers_and_restarts,
    ));
    Device::new(
        open(directory, Some(network))
            .identity(identity)
            .join(invitation)
            .await?,
    )
}
fn identity(directory: &Path) -> Result<Value, Failure> {
    common::authority_json(directory, "identity")
}
fn issuer() -> rocky_sdk::Access {
    rocky_sdk::Access::Full
}
async fn identities(issuer: &Device) -> BTreeMap<String, Value> {
    let own = issuer.identity().await;
    snap::ok(issuer.database.people()).await["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|record| record["identity"] != own.as_str())
        .map(|record| {
            let mut record = record.clone();
            record.as_object_mut().map(|fields| fields.remove("nodes"));
            (
                record["identity"].as_str().unwrap_or_default().to_owned(),
                record,
            )
        })
        .collect()
}
async fn settle(device: &Device) -> Value {
    let mut changes = device.database.status_changes();
    let mut status = snap::ok(changes.next()).await;
    while status["data"]["bootstrap_pending"] == true {
        status = snap::ok(changes.next()).await;
    }
    status
}
async fn online(device: &Device, online: bool) {
    let mode = if online {
        rocky_sdk::SyncMode::Manual
    } else {
        rocky_sdk::SyncMode::Off
    };
    snap::ok(device.database.set_sync(mode)).await;
}
async fn learn_what_peer_issuers_recorded(issuer: &Device) -> Result<(), Failure> {
    synced(issuer).await.map(drop)
}
async fn settle_first_syncs_before_anything_else_is_recorded(devices: &[&Device]) {
    for device in devices {
        settle(device).await;
    }
}
async fn keep_enrollment_issuers_from_renewals(issuers: &[&Device]) {
    for issuer in issuers {
        online(issuer, false).await;
    }
}
async fn synced(device: &Device) -> Result<Value, Failure> {
    device
        .sync()
        .await
        .map_err(|error| error.to_string().into())
}
fn chain_capability(directory: &Path) -> Result<String, Failure> {
    let chain: CredentialChain =
        serde_json::from_value(identity(directory)?["credentials"].clone())?;
    let device_identity_link = chain
        .0
        .iter()
        .rev()
        .find(|link| link.capability.kind == rocky_core::PrincipalKind::User)
        .ok_or("empty chain")?;
    Ok(data_encoding::HEXLOWER.encode(&device_identity_link.id()?))
}
fn registry_rows(directory: &Path, entry: &str) -> Result<i64, Failure> {
    let file = rusqlite::Connection::open_with_flags(
        directory.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    Ok(file.query_row(
        "SELECT COUNT(*) FROM issuer_registry WHERE entry LIKE ?",
        [entry],
        |row| row.get(0),
    )?)
}

#[tokio::test]
async fn a_device_enrolled_at_one_issuer_is_listed_at_another_after_one_sync() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(&root.join("a"), memory(&hub, "a")).await?;
    let b = join(
        &root.join("b"),
        memory(&hub, "b"),
        &a.invite_as(issuer()).await,
    )
    .await?;
    a.admit(&b).await;
    a.database.add_peer(&b.contact().await).await?;
    settle(&b).await;
    online(&b, false).await;
    let lister = rocky_sdk::Access::Role(rocky_sdk::RoleName::from_static("lister"));
    let m = join(
        &root.join("m"),
        memory(&hub, "m"),
        &a.invite_as(lister).await,
    )
    .await?;
    settle(&m).await;
    let m_identity = m.identity().await;
    snap::ok(a.database.create_partition(&rocky_sdk::Partition::new(
        "parts",
        vec![rocky_sdk::SqlValue::Text("L".to_owned())],
    )))
    .await;
    snap::ok(a.database.grant_partition(
        rocky_sdk::PersonId::new(snap::public_key(&m_identity)),
        &rocky_sdk::Partition::new("parts", vec![rocky_sdk::SqlValue::Text("L".to_owned())]),
    ))
    .await;
    synced(&m).await?;
    let at_a = identities(&a).await[&m_identity].clone();
    assert_eq!(at_a["access"], json!({"role": "lister"}));
    assert_eq!(at_a["partitions"].as_array().map(Vec::len), Some(1));
    online(&b, true).await;
    assert!(!identities(&b).await.contains_key(&m_identity));

    synced(&b).await?;
    let listed = identities(&b).await;
    assert_eq!(listed[&m_identity], at_a);
    assert_eq!(
        listed[&a.identity().await]["access"],
        json!("full"),
        "the enrolling issuer's own record arrives with the member's"
    );
    assert!(!listed.contains_key(&b.identity().await));
    for device in [&a, &b, &m] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_member_renews_at_an_issuer_it_never_met_and_removes_the_lost_one() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(&root.join("a"), memory(&hub, "a")).await?;
    let b = join(
        &root.join("b"),
        memory(&hub, "b"),
        &a.invite_as(issuer()).await,
    )
    .await?;
    a.admit(&b).await;
    a.database.add_peer(&b.contact().await).await?;
    settle(&b).await;
    online(&b, false).await;
    let m = join(&root.join("m"), memory(&hub, "m"), &a.invite().await).await?;
    settle(&m).await;
    let (a_node, m_identity) = (a.node().await, m.identity().await);
    a.close().await;
    drop(a);
    std::fs::remove_dir_all(root.join("a"))?;
    online(&b, true).await;
    assert!(!identities(&b).await.contains_key(&m_identity));

    snap::ok(async {
        m.database.renew().await?;
        m.database.info().await
    })
    .await;
    let after = snap::ok(m.database.info()).await;
    let expiry = |info: &Value| -> Result<u64, Failure> {
        Ok(info["expires"].as_str().ok_or("expiry")?.parse()?)
    };
    assert!(
        expiry(&after)? <= expiry(&snap::ok(b.database.info()).await)?,
        "a renewal through B's chain expires no later than B does"
    );
    let record = identities(&b).await[&m_identity].clone();
    assert_eq!(record["access"], json!("full"));

    snap::ok(m.database.remove_peer(snap::public_key(&(a_node)))).await;
    assert!(
        !m.database
            .status()
            .await?
            .authority
            .renewal_routes
            .is_empty()
    );
    m.add(1, "Milk").await;
    let report = synced(&m).await?;
    assert_eq!(
        crate::support::completed_peers(&report),
        1,
        "{report}; activity: {}",
        snap::ok(m.database.activity(0_u64)).await
    );
    assert_eq!(b.titles().await, ["Milk"]);
    for device in [&b, &m] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn only_issuers_open_the_registry_and_custody_relays_it_sealed() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(&root.join("a"), memory(&hub, "a")).await?;
    let b = join(
        &root.join("b"),
        memory(&hub, "b"),
        &a.invite_as(issuer()).await,
    )
    .await?;
    a.admit(&b).await;
    let custody = rocky_sdk::Access::Store;
    let c = join(
        &root.join("c"),
        memory(&hub, "c"),
        &a.invite_as(custody).await,
    )
    .await?;
    let reader = rocky_sdk::Access::Read;
    let r = join(
        &root.join("r"),
        memory(&hub, "r"),
        &a.invite_as(reader).await,
    )
    .await?;
    let w = join(&root.join("w"), memory(&hub, "w"), &a.invite().await).await?;
    synced(&r).await?;
    synced(&w).await?;
    let (a_node, _b_node) = (a.node().await, b.node().await);
    snap::ok(b.database.remove_peer(snap::public_key(&(a_node)))).await;
    introduce(&a, &c).await;
    introduce(&b, &c).await;
    let keys = a
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?;
    let joined = join(&root.join("k"), memory(&hub, "k"), &keys.code).await?;
    synced(&a).await?;
    synced(&b).await?;
    assert!(identities(&b).await.contains_key(&joined.identity().await));

    for name in ["c", "r", "w"] {
        let directory = root.join(name);
        let held = identity(&directory)?["keys"].clone();
        assert!(held.get("issuer_registry").is_none(), "{name}: {held}");
        for entry in std::fs::read_dir(directory.join("secrets"))? {
            let file = entry?.file_name().to_string_lossy().into_owned();
            assert!(!file.contains("issuer_registry"), "{name}: {file}");
        }
        let file = rusqlite::Connection::open_with_flags(
            directory.join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let tables: i64 = file.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='issuer_registry'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(tables, 0, "{name}");
    }
    for device in [&c, &r, &w] {
        assert!(snap::observe(device.database.people()).await.is_err());
        assert!(
            snap::observe(snap::read(
                &device.database,
                "SELECT * FROM issuer_registry",
                Vec::new(),
            ))
            .await
            .is_err()
        );
    }
    assert!(
        snap::observe(snap::read(
            &a.database,
            "SELECT * FROM issuer_registry",
            Vec::new(),
        ))
        .await
        .is_err(),
        "an issuer's application session does not open the registry"
    );
    assert!(registry_rows(&root.join("b"), "identity")? >= 6);
    for device in [&a, &b, &c, &r, &w] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn concurrent_enrollments_and_renewals_at_two_issuers_converge() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path().to_path_buf();
    let hub = Arc::new(Hub::default());
    let a = create(&root.join("a"), memory(&hub, "a")).await?;
    let b = join(
        &root.join("b"),
        memory(&hub, "b"),
        &a.invite_as(issuer()).await,
    )
    .await?;
    a.admit(&b).await;
    a.database.add_peer(&b.contact().await).await?;
    settle(&b).await;
    let started = |name: &str, invitation: rocky_sdk::InvitationCode| {
        let (directory, network) = (root.join(name), memory(&hub, name));
        tokio::spawn(async move {
            join(&directory, network, &invitation)
                .await
                .map_err(|e| e.to_string())
        })
    };
    let (x, y) = tokio::join!(
        started("x", a.invite_as(issuer()).await),
        started("y", b.invite_as(issuer()).await)
    );
    let (x, y) = (x??, y??);
    a.admit(&x).await;
    learn_what_peer_issuers_recorded(&a).await?;
    a.admit(&y).await;
    settle_first_syncs_before_anything_else_is_recorded(&[&x, &y]).await;
    let mut writers = std::collections::BTreeSet::new();
    for device in [&a, &b, &x, &y] {
        writers.insert(
            snap::ok(device.database.info()).await["writer"]
                .clone()
                .to_string(),
        );
    }
    assert_eq!(
        writers.len(),
        4,
        "each issuer writes under its own node key: {writers:?}"
    );
    synced(&a).await?;
    let (x_identity, y_identity) = (x.identity().await, y.identity().await);
    for issuer in [&a, &b] {
        let listed = identities(issuer).await;
        assert!(
            listed.contains_key(&x_identity) && listed.contains_key(&y_identity),
            "both issuers list both concurrent enrollments"
        );
    }

    for issuer in [&a, &b] {
        snap::ok(issuer.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    }
    keep_enrollment_issuers_from_renewals(&[&x, &y]).await;
    online(&b, false).await;
    let m = join(&root.join("m"), memory(&hub, "m"), &a.invite().await).await?;
    settle(&m).await;
    snap::ok(m.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    let m_identity = m.identity().await;
    snap::ok(async {
        m.database.renew().await?;
        m.database.info().await
    })
    .await;
    snap::ok(a.database.set_sync(rocky_sdk::SyncMode::Off)).await;
    online(&b, true).await;
    snap::ok(async {
        m.database.renew().await?;
        m.database.info().await
    })
    .await;
    snap::ok(a.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    assert!(identities(&b).await.contains_key(&m_identity));
    synced(&a).await?;
    let (at_a, at_b) = (identities(&a).await, identities(&b).await);
    assert_eq!(at_a[&m_identity], at_b[&m_identity]);
    for name in ["a", "b"] {
        let file = rusqlite::Connection::open_with_flags(
            root.join(name).join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let entries: i64 = file.query_row(
            "SELECT COUNT(*) FROM issuer_registry WHERE identity = ? AND entry = 'identity'",
            [&m_identity],
            |row| row.get(0),
        )?;
        assert_eq!(
            entries, 1,
            "one visible identity entry per identity in {name}"
        );
    }
    for device in [&a, &b, &x, &y, &m] {
        device.close().await;
    }
    Ok(())
}

#[derive(Default)]
struct Faults {
    answer: Mutex<Option<Protocol>>,
    receipt: Mutex<Option<Protocol>>,
}
impl Faults {
    fn take(slot: &Mutex<Option<Protocol>>, protocol: Protocol) -> bool {
        let mut slot = slot.lock().unwrap();
        let armed = *slot == Some(protocol);
        if armed {
            *slot = None;
        }
        armed
    }
}
struct Faulty {
    inner: Arc<Memory>,
    faults: Arc<Faults>,
}
impl Connector for Faulty {
    fn start(
        &self,
        accepts: bool,
        route_secret: [u8; 32],
    ) -> BoxFuture<'_, Result<rocky_sdk::transport::Connected, Failure>> {
        Box::pin(async move {
            let inner = self.inner.start(accepts, route_secret).await?;
            Ok(rocky_sdk::transport::Connected {
                discovery: inner.discovery,
                changes: inner.changes,
                transport: Arc::new(FaultyTransport {
                    inner: inner.transport,
                    faults: self.faults.clone(),
                }),
            })
        })
    }
}
struct FaultyTransport {
    inner: Arc<dyn Transport>,
    faults: Arc<Faults>,
}
impl Transport for FaultyTransport {
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        Box::pin(async move {
            let mut failure: Failure = "no route".into();
            for address in addresses {
                let result =
                    rocky_platform::time::timeout(std::time::Duration::from_secs(30), async {
                        let mut stream = self.connect(address, Protocol::Rpc, Some(scope)).await?;
                        rocky_core::net::framing::write(&mut stream, request).await?;
                        let reply = rocky_core::net::framing::read(&mut stream).await?;
                        let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
                        Ok::<_, Failure>(reply)
                    })
                    .await;
                match result {
                    Ok(Ok(reply)) => return Ok(reply),
                    Ok(Err(error)) => failure = error,
                    Err(error) => failure = error.into(),
                }
            }
            Err(failure)
        })
    }
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            let stream = self.inner.session(addresses, scope).await?;
            Ok(if Faults::take(&self.faults.receipt, Protocol::Link) {
                Stream::new(Unread(stream))
            } else {
                stream
            })
        })
    }
    fn kind(&self) -> &str {
        self.inner.kind()
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        self.inner.address()
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
        scope: Option<rocky_core::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            let stream = self.inner.connect(address, protocol, scope).await?;
            Ok(if Faults::take(&self.faults.receipt, protocol) {
                Stream::new(Unread(stream))
            } else {
                stream
            })
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<rocky_core::net::Incoming>> {
        Box::pin(async move {
            let incoming = self.inner.accept().await?;
            let protocol = incoming.protocol();
            Some(if Faults::take(&self.faults.answer, protocol) {
                match incoming {
                    rocky_core::net::Incoming::Stream(protocol, stream) => {
                        rocky_core::net::Incoming::Stream(protocol, Stream::new(Mute(stream)))
                    }
                    rocky_core::net::Incoming::Request(original) => {
                        let (request, answer, delivered) =
                            rocky_core::net::Request::channel(original.bytes.clone());
                        rocky_platform::task::spawn(async move {
                            let _ = answer.await;
                            drop(original);
                            drop(delivered);
                        });
                        rocky_core::net::Incoming::Request(request)
                    }
                }
            } else {
                incoming
            })
        })
    }
    fn listen(&self, scope: Option<rocky_core::Scope>, listening: bool) -> Result<(), Failure> {
        self.inner.listen(scope, listening)
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        self.inner.close()
    }
}
struct Mute(Stream);
impl AsyncRead for Mute {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}
impl AsyncWrite for Mute {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
struct Unread(Stream);
impl AsyncRead for Unread {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut scratch = [0; 4096];
        loop {
            let mut buffer = ReadBuf::new(&mut scratch);
            match Pin::new(&mut self.0).poll_read(cx, &mut buffer) {
                Poll::Ready(Ok(())) if buffer.filled().is_empty() => {
                    return Poll::Ready(Err(std::io::ErrorKind::ConnectionReset.into()));
                }
                Poll::Ready(Ok(())) => {}
                other => return other,
            }
        }
    }
}
impl AsyncWrite for Unread {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
fn faulty(hub: &Arc<Hub>, name: &str, faults: &Arc<Faults>) -> Network {
    Network::Owned(Arc::new(Faulty {
        inner: hub.connector(name),
        faults: faults.clone(),
    }))
}

#[tokio::test]
async fn interrupted_answers_leave_a_record_and_retry_as_the_same_device() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let issuing = Arc::new(Faults::default());
    let a = create(&root.join("a"), faulty(&hub, "a", &issuing)).await?;
    let mut members = Vec::new();
    for name in ["m", "n"] {
        let directory = root.join(name);
        let requesting = Arc::new(Faults::default());
        let invitation = a.invite().await;
        let before = identities(&a).await;
        let member = join(&directory, faulty(&hub, name, &requesting), &invitation).await?;
        let identity = member.identity().await;
        let recorded = identities(&a).await;
        assert_eq!(recorded.len(), before.len() + 1, "{name}");
        assert!(recorded.contains_key(&identity), "{name}: {recorded:?}");
        members.push((member, identity, requesting));
    }
    let profile = rocky_sdk::Access::Full;
    let mut granted = Vec::new();
    for name in ["g", "h"] {
        let keys = a
            .database
            .invite(rocky_sdk::Invite::new(profile.clone()))
            .await?;
        let before = identities(&a).await;
        let requesting = Arc::new(Faults::default());
        let device = Device::new(
            open(&root.join(name), Some(faulty(&hub, name, &requesting)))
                .join(&keys.code)
                .await?,
        )?;
        let identity = device.identity().await;
        let listed = identities(&a).await;
        assert_eq!(listed.len(), before.len() + 1);
        granted.push((device, identity, requesting));
    }
    for (index, (device, identity, requesting)) in granted.iter().enumerate() {
        let directory = root.join(["g", "h"][index]);
        let held = chain_capability(&directory)?;
        let stops_before_the_answer_is_written = index == 0;
        if stops_before_the_answer_is_written {
            *issuing.answer.lock().unwrap() = Some(Protocol::Rpc);
        } else {
            *requesting.receipt.lock().unwrap() = Some(Protocol::Rpc);
        }
        assert!(
            snap::observe(async {
                device.database.renew().await?;
                device.database.info().await
            })
            .await
            .is_err()
        );
        assert!(
            identities(&a).await.contains_key(identity),
            "an interrupted answer leaves the identity recorded"
        );
        assert_eq!(chain_capability(&directory)?, held, "no chain was stored");
        snap::ok(async {
            device.database.renew().await?;
            device.database.info().await
        })
        .await;
        assert!(identities(&a).await.contains_key(identity));
    }
    for (device, _, _) in &granted {
        device.close().await;
    }
    a.close().await;
    for (member, _, _) in &members {
        member.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_full_registry_refuses_the_257th_device_and_syncs_to_a_new_issuer() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let root: PathBuf = root.path().to_path_buf();
    let hub = Arc::new(Hub::default());
    let a = create(&root.join("a"), memory(&hub, "a")).await?;
    let b = join(
        &root.join("b"),
        memory(&hub, "b"),
        &a.invite_as(issuer()).await,
    )
    .await?;
    a.admit(&b).await;
    a.database.add_peer(&b.contact().await).await?;
    settle(&b).await;
    let reader = rocky_sdk::Access::Read;
    let invitation = a.invite_as(reader.clone()).await;
    let filled = Instant::now();
    let a_b_and_the_pending_invitation = 3;
    for index in 0..REGISTRY_CAPACITY - a_b_and_the_pending_invitation {
        let name = format!("f{index}");
        let code = a.invite_as(reader.clone()).await;
        rocky_sdk::Configuration::new(root.join(&name), common::parsed_schema())?
            .network(memory(&hub, &name))
            .join(&code)
            .await?
            .database
            .close()
            .await?;
    }
    let filled = filled.elapsed();
    let joined = join(&root.join("j"), memory(&hub, "j"), &invitation).await?;
    assert_eq!(identities(&a).await.len(), 255);

    a.close().await;
    drop(a);
    let unreachable_so_only_these_calls_can_write = None;
    let a = Device::new(
        open(&root.join("a"), unreachable_so_only_these_calls_can_write)
            .open()
            .await?,
    )?;
    let before = (
        common::authority_bytes(&root.join("a"), "identity")?,
        registry_rows(&root.join("a"), "%")?,
    );
    let refused = snap::observe(a.database.invite(rocky_sdk::Invite::new(reader.clone())))
        .await
        .err()
        .ok_or("invited a 257th device")?;
    assert_eq!(refused["code"], "registry_full", "{refused}");
    let after = (
        common::authority_bytes(&root.join("a"), "identity")?,
        registry_rows(&root.join("a"), "%")?,
    );
    assert_eq!(before, after);
    assert_eq!(identities(&a).await.len(), 255);
    a.close().await;
    drop(a);
    let a = Device::new(
        open(&root.join("a"), Some(memory(&hub, "a")))
            .open()
            .await?,
    )?;

    let start = Instant::now();
    synced(&b).await?;
    let sync = start.elapsed();
    assert_eq!(identities(&b).await.len(), 255);
    b.close().await;
    drop(b);
    let start = Instant::now();
    let b = Device::new(
        open(&root.join("b"), Some(memory(&hub, "b")))
            .open()
            .await?,
    )?;
    let reopen = start.elapsed();
    assert_eq!(identities(&b).await.len(), 255);
    eprintln!(
        "registry of 256 devices: 253 grants {filled:?}, sync to a new issuer {sync:?}, reopen {reopen:?}"
    );
    for device in [&a, &b, &joined] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn invitation_identity_and_label_survive_replacement_and_reopen() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let path = root.path().join("issuer");
    let owner = create(&path, memory(&hub, "issuer")).await?;
    let label = b"application-defined label";
    let invitation = owner
        .database
        .invite(rocky_sdk::Invite::new(issuer()).label(String::from_utf8(label.to_vec())?))
        .await?;
    let member = join(
        &root.path().join("member"),
        memory(&hub, "member"),
        &invitation.code,
    )
    .await?;
    let invitation = json!({"id": invitation.id});
    let identity = member.identity().await;
    let record = identities(&owner).await.remove(&identity).ok_or("record")?;
    assert_eq!(record["invitation"], invitation["id"]);
    assert_eq!(record["label"], json!(String::from_utf8(label.to_vec())?));
    snap::ok(owner.database.invite(rocky_sdk::Invite::new(issuer()))).await;
    member.database.close().await?;
    owner.database.close().await?;
    let reopened = Device::new(open(&path, None).open().await?)?;
    let record = identities(&reopened)
        .await
        .remove(&identity)
        .ok_or("reopened record")?;
    assert_eq!(
        record["invitation"], invitation["id"],
        "a later invitation does not erase the earlier enrollment's association"
    );
    assert_eq!(record["label"], json!(String::from_utf8(label.to_vec())?));
    reopened.database.close().await?;
    Ok(())
}
