//! Rocky's iroh transport: peer-to-peer QUIC with relays and discovery, natively and in the browser.
#![deny(missing_docs)]
use rocky_core::net::{
    BoxFuture, Connected, Connector, Discovery, Failure, Incoming, Protocol, SessionStream, Stream,
    Transport,
};
use rocky_core::{
    Scope,
    net::{ScopeRefusal, framing},
};
use serde::{Deserialize, Serialize};
mod diagnostics;
pub use diagnostics::{NetworkPath, RelayStatus};
mod scoped;
use scoped::Route;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, Join, ReadBuf},
    sync::{Mutex, Semaphore, mpsc, watch},
};

mod discovery;
/// A local relay and lookup server for tests; never enable `test-utils` in a shipped build.
#[cfg(feature = "test-utils")]
pub mod testing;
pub use discovery::{
    DEFAULT_RENDEZVOUS, Discover, LOCAL_SERVICE, RENDEZVOUS_AGE, RENDEZVOUS_BYTES,
    RENDEZVOUS_CONTACTS, RENDEZVOUS_INTERVAL, Topic, rendezvous_key,
};

#[cfg(not(target_family = "wasm"))]
use iroh::address_lookup::DnsAddressLookup;

pub use iroh;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey, TransportAddr, Watcher,
    address_lookup::{PkarrPublisher, PkarrResolver},
    endpoint::{Connection, LocalTransportAddr, PortmapperConfig, RecvStream, SendStream, presets},
    protocol::{AcceptError, ProtocolHandler, Router, RouterBuilder},
};

/// The ALPN that carries one Rocky protocol over iroh.
pub fn alpn(protocol: Protocol) -> &'static [u8] {
    match protocol {
        Protocol::Rpc => b"rocky/rpc",
        Protocol::Link => b"rocky/link",
        Protocol::Coordinate => b"rocky/coordinator/3",
    }
}
/// The Rocky protocol an ALPN names, or `None` for a foreign one.
pub fn protocol_named(name: &[u8]) -> Option<Protocol> {
    Protocol::ALL
        .into_iter()
        .chain([Protocol::Coordinate])
        .find(|protocol| alpn(*protocol) == name)
}

/// The route scheme this crate claims: its routes are `iroh:` URLs that name an endpoint and its
/// relay and direct addresses.
pub const SCHEME: &str = "iroh";

/// The `iroh:` route that names an endpoint, its relay and its direct addresses.
pub fn route(address: &EndpointAddr) -> Result<String, Failure> {
    Ok(format!(
        "{SCHEME}:{}",
        data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(address)?)
    ))
}

fn endpoint_addr(route: &str) -> Result<EndpointAddr, Failure> {
    let encoded = route
        .strip_prefix(SCHEME)
        .and_then(|rest| rest.strip_prefix(':'))
        .ok_or("not an iroh: route")?;
    Ok(serde_json::from_slice(
        &data_encoding::BASE64URL_NOPAD.decode(encoded.as_bytes())?,
    )?)
}
/// How long a dial or a connection's opening handshake may take before it fails.
pub const TIMEOUT: Duration = Duration::from_secs(15);
/// Incoming streams a database scope queues before new ones are refused as busy.
pub const PENDING: usize = 32;
/// Inbound connections, and inbound link connections, an attachment accepts at once.
pub const CONNECTIONS: usize = 64;
/// Outbound connections an attachment keeps open for reuse.
pub const CACHED: usize = 64;
/// Streams one connection carries at once.
pub const STREAMS_PER_CONNECTION: usize = 4;
/// Requests one attachment serves at once; more are refused as busy.
pub const RPC_REQUESTS: usize = 32;
/// How long an unused connection stays open.
pub const IDLE: Duration = Duration::from_secs(30);
const CLOSED: u32 = 1;
const BUSY: u32 = 2;
const REBIND: Duration = Duration::from_secs(3);
const REBIND_RETRY: Duration = Duration::from_millis(100);
const LINK_LANES: u8 = 5;
const HOME_RELAY_WAIT: Duration = Duration::from_secs(5);
#[cfg(feature = "qualification")]
const LOOPBACK_INITIAL_RTT: Duration = Duration::from_millis(10);

struct QuicStream {
    _connection: Connection,
    reusable: Option<Arc<AtomicBool>>,
    io: Join<RecvStream, SendStream>,
    read_incomplete: bool,
    write_incomplete: bool,
    _slot: Option<Slot>,
    port: PortState,
}
enum PortState {
    Unbound,
    Open(Pin<Box<dyn Future<Output = ()> + Send>>),
    Closed,
}
impl PortState {
    fn of(route: &Route) -> Self {
        let mut closed = route.closed.subscribe();
        Self::Open(Box::pin(async move {
            if closed.wait_for(|closed| *closed).await.is_err() {
                std::future::pending::<()>().await;
            }
        }))
    }
}
impl QuicStream {
    fn was_cancelled(&self) -> bool {
        self.read_incomplete || self.write_incomplete
    }
    fn port_closed(&mut self, cx: &mut Context<'_>) -> Option<std::io::Error> {
        if let PortState::Open(closed) = &mut self.port
            && closed.as_mut().poll(cx).is_ready()
        {
            self.port = PortState::Closed;
            let _ = self.io.reader_mut().stop(CLOSED.into());
            let _ = self.io.writer_mut().reset(CLOSED.into());
        }
        matches!(self.port, PortState::Closed)
            .then(|| std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "the port closed"))
    }
}
impl Drop for QuicStream {
    fn drop(&mut self) {
        if self.was_cancelled()
            && let Some(reusable) = &self.reusable
        {
            reusable.store(false, Ordering::SeqCst);
            let _ = self.io.reader_mut().stop(CLOSED.into());
            let _ = self.io.writer_mut().reset(CLOSED.into());
        }
    }
}
struct Slot {
    _permit: tokio::sync::OwnedSemaphorePermit,
    active: Arc<watch::Sender<usize>>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.active
            .send_modify(|active| *active = active.saturating_sub(1));
    }
}
async fn idle(activity: &mut watch::Receiver<usize>, limit: Duration) {
    loop {
        if activity.wait_for(|active| *active == 0).await.is_err() {
            return std::future::pending().await;
        }
        tokio::select! {
            () = rocky_platform::time::sleep(limit) => return,
            changed = activity.changed() => {
                if changed.is_err() {
                    return std::future::pending().await;
                }
            }
        }
    }
}
impl AsyncRead for QuicStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if let Some(closed) = self.port_closed(cx) {
            return Poll::Ready(Err(closed));
        }
        let result = Pin::new(&mut self.io).poll_read(cx, buf);
        self.read_incomplete = !matches!(result, Poll::Ready(Ok(())));
        result
    }
}
impl AsyncWrite for QuicStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if let Some(closed) = self.port_closed(cx) {
            return Poll::Ready(Err(closed));
        }
        let result = Pin::new(&mut self.io).poll_write(cx, buf);
        self.write_incomplete = !matches!(result, Poll::Ready(Ok(_)));
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if let Some(closed) = self.port_closed(cx) {
            return Poll::Ready(Err(closed));
        }
        let result = Pin::new(&mut self.io).poll_flush(cx);
        self.write_incomplete = !matches!(result, Poll::Ready(Ok(())));
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if let Some(closed) = self.port_closed(cx) {
            return Poll::Ready(Err(closed));
        }
        let result = Pin::new(&mut self.io).poll_shutdown(cx);
        self.write_incomplete = !matches!(result, Poll::Ready(Ok(())));
        result
    }
}
fn stream(
    connection: Connection,
    (send, recv): (SendStream, RecvStream),
    reusable: Arc<AtomicBool>,
    port: PortState,
) -> Stream {
    Stream::new(QuicStream {
        _connection: connection,
        reusable: Some(reusable),
        io: tokio::io::join(recv, send),
        read_incomplete: false,
        write_incomplete: false,
        _slot: None,
        port,
    })
}
fn refuse_closed_port(route: Option<&Route>) -> Result<(), Failure> {
    if route.is_some_and(|route| *route.closed.borrow()) {
        return Err("the port is closed".into());
    }
    Ok(())
}

struct LinkConnection(
    Connection,
    std::sync::Mutex<Option<Scope>>,
    std::sync::Mutex<Option<std::sync::Weak<Route>>>,
);
impl Drop for LinkConnection {
    fn drop(&mut self) {
        self.0.close(CLOSED.into(), b"link ended");
    }
}
struct LinkLane {
    io: Join<RecvStream, SendStream>,
    shutdown: Option<BoxFuture<'static, std::io::Result<()>>>,
    finished: bool,
    _owner: Arc<LinkConnection>,
}
impl AsyncRead for LinkLane {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for LinkLane {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.finished {
            return Poll::Ready(Ok(()));
        }
        if self.shutdown.is_none() {
            self.io
                .writer_mut()
                .finish()
                .map_err(std::io::Error::other)?;
            let stopped = self.io.writer().stopped();
            self.shutdown = Some(Box::pin(async move {
                match stopped.await.map_err(std::io::Error::other)? {
                    None => Ok(()),
                    Some(_) => Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "link stream stopped",
                    )),
                }
            }));
        }
        let Some(shutdown) = &mut self.shutdown else {
            return Poll::Ready(Err(std::io::Error::other("link shutdown state")));
        };
        let result = shutdown.as_mut().poll(cx);
        if result.is_ready() {
            self.finished = true;
        }
        result
    }
}
async fn link_stream(
    connection: Connection,
    dialing: bool,
    shared: &Arc<Shared>,
    scope: Option<Scope>,
    route_owner: Option<Arc<Route>>,
) -> Result<(Stream, Option<Arc<Route>>), Failure> {
    let owner = Arc::new(LinkConnection(
        connection.clone(),
        std::sync::Mutex::new(scope),
        std::sync::Mutex::new(route_owner.as_ref().map(Arc::downgrade)),
    ));
    {
        let mut links = shared
            .link_connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        links.retain(|link| link.strong_count() > 0);
        if links.len() >= 2 * CONNECTIONS {
            return Err("link connection quota".into());
        }
        links.push(Arc::downgrade(&owner));
    }
    refuse_closed_port(route_owner.as_deref())?;
    let mut lanes: Vec<Box<dyn SessionStream>> = Vec::new();
    let mut routed: Option<Arc<Route>> = None;
    for lane in 0..LINK_LANES {
        let (send, recv) = if dialing {
            connection.open_bi().await?
        } else {
            connection.accept_bi().await?
        };
        let mut io = tokio::io::join(recv, send);
        if dialing {
            scoped::outbound(&mut io, scope, lane).await?;
        } else {
            let route = scoped::inbound(&mut io, shared, lane).await?;
            if routed
                .as_ref()
                .is_some_and(|prior| !Arc::ptr_eq(prior, &route))
            {
                return Err("link lanes belong to different scopes".into());
            }
            *owner.1.lock().unwrap_or_else(PoisonError::into_inner) =
                *route.scope.lock().unwrap_or_else(PoisonError::into_inner);
            *owner.2.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(&route));
            routed = Some(route);
        }
        lanes.push(Box::new(LinkLane {
            io,
            shutdown: None,
            finished: false,
            _owner: owner.clone(),
        }));
    }
    let control = lanes.remove(0);
    Ok((Stream::new(control).with_lanes(lanes), routed))
}

struct Paths(Connection);
impl Paths {
    fn paths(&self) -> Vec<NetworkPath> {
        let list = self.0.paths();
        let mut paths: Vec<_> = list
            .iter()
            .map(|path| {
                let stats = path.stats();
                let (kind, remote) = match path.remote_addr() {
                    TransportAddr::Ip(address) => ("direct", address.to_string()),
                    TransportAddr::Relay(url) => ("relay", url.to_string()),
                    other => ("other", format!("{other:?}")),
                };
                let local = match path.local_addr() {
                    LocalTransportAddr::Ip(address) => address.map(|ip| ip.to_string()),
                    LocalTransportAddr::Relay(url) => Some(url.to_string()),
                    _ => None,
                };
                NetworkPath {
                    selected: path.is_selected(),
                    kind,
                    remote,
                    local,
                    rtt: stats.rtt,
                    mtu: stats.current_mtu,
                    window: stats.cwnd,
                    sent_bytes: stats.udp_tx.bytes,
                    sent_datagrams: stats.udp_tx.datagrams,
                    received_bytes: stats.udp_rx.bytes,
                    received_datagrams: stats.udp_rx.datagrams,
                    lost_packets: stats.lost_packets,
                    lost_bytes: stats.lost_bytes,
                    congestion_events: stats.congestion_events,
                }
            })
            .collect();
        paths.sort_by_key(|path| !path.selected);
        paths
    }
}

#[derive(Clone)]
struct CachedConnection {
    connection: Connection,
    used: rocky_platform::time::Instant,
    reusable: Arc<AtomicBool>,
}
impl CachedConnection {
    fn reusable(&self) -> bool {
        self.reusable.load(Ordering::SeqCst)
            && self.connection.close_reason().is_none()
            && self.used.elapsed() < IDLE / 2
    }
}

struct Shared {
    endpoint: watch::Sender<Endpoint>,
    restart: Mutex<Option<(Options, [u8; 32])>>,
    routes: std::sync::Mutex<HashMap<Option<Scope>, std::sync::Weak<Route>>>,
    default_route: Arc<Route>,
    connections: Mutex<HashMap<(EndpointId, Protocol), CachedConnection>>,
    link_connections: std::sync::Mutex<Vec<std::sync::Weak<LinkConnection>>>,
    inbound: Semaphore,
    rpc: Arc<Semaphore>,
    inbound_links: Semaphore,
    closed: watch::Sender<bool>,
    refused: AtomicU64,
    router: std::sync::Mutex<Option<Router>>,
    relays: bool,
    initial_relay_configuration: Option<String>,
    protocols: Vec<Protocol>,
    finder: Option<discovery::Finder>,
    discover: Discover,
    home: Option<rocky_platform::task::Home>,
}

/// An iroh endpoint shared by one or more databases; clones share it, and `close` stops it.
#[derive(Clone)]
pub struct Attachment(Arc<Shared>);
impl Attachment {
    /// Attaches to an endpoint the application built and routes, with no discovery.
    pub fn new(endpoint: Endpoint) -> Self {
        Self::build(
            endpoint,
            false,
            Protocol::ALL.to_vec(),
            Discover::default(),
            None,
        )
    }
    /// Attaches to an application's endpoint and finds peers as `discover` says.
    pub fn discovering(endpoint: Endpoint, discover: Discover) -> Self {
        Self::build(endpoint, false, Protocol::ALL.to_vec(), discover, None)
    }
    /// Attaches to an application's endpoint and accepts only the named protocols.
    pub fn serving(endpoint: Endpoint, protocols: Vec<Protocol>) -> Self {
        Self::build(endpoint, false, protocols, Discover::default(), None)
    }
    fn build(
        endpoint: Endpoint,
        relays: bool,
        protocols: Vec<Protocol>,
        discover: Discover,
        initial_relay_configuration: Option<String>,
    ) -> Self {
        let endpoint = watch::channel(endpoint).0;
        let finder = (discover != Discover::default())
            .then(|| discovery::Finder::new(endpoint.subscribe(), discover.clone()));
        Self(Arc::new(Shared {
            home: rocky_platform::task::Home::current(),
            finder,
            discover,
            endpoint,
            restart: Mutex::new(None),
            routes: Default::default(),
            default_route: Arc::new(Route::new()),
            connections: Mutex::new(HashMap::new()),
            link_connections: Default::default(),
            inbound: Semaphore::new(CONNECTIONS),
            rpc: Arc::new(Semaphore::new(RPC_REQUESTS)),
            inbound_links: Semaphore::new(CONNECTIONS),
            closed: watch::channel(false).0,
            refused: AtomicU64::new(0),
            router: std::sync::Mutex::new(None),
            relays,
            initial_relay_configuration,
            protocols,
        }))
    }
    /// Every ALPN Rocky accepts, for an application's own router.
    pub fn alpns() -> impl Iterator<Item = &'static [u8]> {
        Protocol::ALL.into_iter().map(alpn)
    }
    /// The protocol handler for an application's own iroh router.
    pub fn handler(&self) -> Handler {
        Handler(self.0.clone())
    }
    /// Adds this attachment's protocols to an application's iroh router.
    pub fn register(&self, router: RouterBuilder) -> RouterBuilder {
        self.0.protocols.iter().fold(router, |router, protocol| {
            router.accept(alpn(*protocol), self.handler())
        })
    }
    /// A transport for one database, passed as `Network::Borrowed`; each database takes its own.
    pub fn transport(&self) -> Arc<dyn Transport> {
        Arc::new(Port {
            attachment: self.clone(),
            route: Arc::new(Route::new()),
        })
    }
    /// The current endpoint, which changes when the attachment rebinds after its sockets closed.
    pub fn endpoint(&self) -> Endpoint {
        self.0.endpoint.borrow().clone()
    }
    /// Connections and streams refused so far because no database listened or the queue was full.
    pub fn refused(&self) -> u64 {
        self.0.refused.load(Ordering::Relaxed)
    }
    async fn revive(&self) -> Result<(), Failure> {
        let restart = self.0.restart.lock().await;
        if *self.0.closed.borrow() {
            return Err("the iroh attachment is closed".into());
        }
        if !self.endpoint().is_closed() {
            return Ok(());
        }
        let Some((options, secret)) = restart.as_ref() else {
            return Err("application-owned endpoint is dead; its owner must rebind it".into());
        };
        self.drop_endpoint_clones().await;
        #[cfg(not(target_family = "wasm"))]
        if options.bind.is_some() {
            self.0.endpoint.send_replace(closed_stand_in(secret).await?);
        }
        let started = rocky_platform::time::Instant::now();
        let replacement = loop {
            match options.bind(*secret).await {
                Ok(endpoint) => break endpoint,
                Err(_) if started.elapsed() < REBIND => {
                    rocky_platform::time::sleep(REBIND_RETRY).await;
                }
                Err(error) => return Err(error),
            }
        };
        let router = self.register(Router::builder(replacement.clone())).spawn();
        self.0.endpoint.send_replace(replacement);
        *self.0.router.lock().unwrap_or_else(PoisonError::into_inner) = Some(router);
        Ok(())
    }
    async fn drop_endpoint_clones(&self) {
        let router = self
            .0
            .router
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(router) = router {
            let _ = router.shutdown().await;
        }
        self.0.connections.lock().await.clear();
    }
    fn refuse(&self, connection: &Connection, code: u32, reason: &[u8]) {
        self.0.refused.fetch_add(1, Ordering::Relaxed);
        connection.close(code.into(), reason);
    }
}

#[cfg(not(target_family = "wasm"))]
async fn closed_stand_in(secret: &[u8; 32]) -> Result<Endpoint, Failure> {
    let stand_in = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(secret))
        .relay_mode(RelayMode::Disabled)
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0")?
        .bind()
        .await?;
    stand_in.close().await;
    Ok(stand_in)
}

/// Accepts Rocky's protocols on an application's iroh router.
pub struct Handler(Arc<Shared>);
impl std::fmt::Debug for Handler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("rocky_iroh::Handler")
    }
}
impl ProtocolHandler for Handler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let attachment = Attachment(self.0.clone());
        let mut closed = self.0.closed.subscribe();
        let Some(protocol) =
            protocol_named(connection.alpn()).filter(|p| self.0.protocols.contains(p))
        else {
            attachment.refuse(&connection, CLOSED, b"unknown protocol");
            return Ok(());
        };
        if *closed.borrow() {
            attachment.refuse(&connection, CLOSED, b"no service");
            return Ok(());
        }
        let budget = if protocol == Protocol::Link {
            &self.0.inbound_links
        } else {
            &self.0.inbound
        };
        let Ok(_permit) = budget.try_acquire() else {
            attachment.refuse(&connection, BUSY, b"busy");
            return Ok(());
        };
        if protocol == Protocol::Link {
            match rocky_platform::time::timeout(
                TIMEOUT,
                link_stream(connection.clone(), false, &self.0, None, None),
            )
            .await
            {
                Ok(Ok((stream, Some(route)))) => {
                    if route
                        .sender
                        .try_send(Incoming::Stream(protocol, stream))
                        .is_err()
                    {
                        self.0.refused.fetch_add(1, Ordering::Relaxed);
                    }
                    tokio::select! {
                        _ = connection.closed() => {},
                        _ = closed.wait_for(|closed| *closed) => { connection.close(CLOSED.into(), b"closed"); },
                    }
                }
                _ => attachment.refuse(&connection, CLOSED, b"link lanes"),
            }
            return Ok(());
        }
        let slots = Arc::new(Semaphore::new(STREAMS_PER_CONNECTION));
        let (active, mut activity) = watch::channel(0);
        let active = Arc::new(active);
        loop {
            let accepted = tokio::select! {
                accepted = async {
                    let slot = slots.clone().acquire_owned().await;
                    (slot, connection.accept_bi().await)
                } => accepted,
                _ = closed.wait_for(|closed| *closed) => {
                    connection.close(CLOSED.into(), b"closed");
                    break;
                }
                () = idle(&mut activity, IDLE) => {
                    connection.close(CLOSED.into(), b"idle");
                    break;
                }
            };
            let (Ok(slot), Ok((send, recv))) = accepted else {
                break;
            };
            active.send_modify(|active| *active += 1);
            let mut inbound = QuicStream {
                _connection: connection.clone(),
                reusable: None,
                io: tokio::io::join(recv, send),
                read_incomplete: false,
                write_incomplete: false,
                _slot: Some(Slot {
                    _permit: slot,
                    active: active.clone(),
                }),
                port: PortState::Unbound,
            };
            let route = if protocol == Protocol::Coordinate {
                self.0
                    .routes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&None)
                    .and_then(std::sync::Weak::upgrade)
            } else {
                rocky_platform::time::timeout(TIMEOUT, scoped::inbound(&mut inbound, &self.0, 0))
                    .await
                    .ok()
                    .and_then(Result::ok)
            };
            let Some(route) = route else { continue };
            inbound.port = PortState::of(&route);
            let inbound = Stream::new(inbound);
            if protocol == Protocol::Rpc {
                let Ok(permit) = self.0.rpc.clone().try_acquire_owned() else {
                    self.0.refused.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                rocky_platform::task::spawn(async move {
                    let _permit = permit;
                    let mut closed = route.closed.subscribe();
                    tokio::select! {
                        _ = rocky_platform::time::timeout(2 * TIMEOUT, framing::serve(inbound, &route.sender)) => {},
                        _ = closed.wait_for(|closed| *closed) => {},
                    }
                });
            } else if route
                .sender
                .try_send(Incoming::Stream(protocol, inbound))
                .is_err()
            {
                self.0.refused.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }
}

impl Attachment {
    /// Fires when the endpoint's addresses or home relay change.
    pub fn connectivity(&self) -> Option<watch::Receiver<()>> {
        let (events, receiver) = watch::channel(());
        let mut endpoints = self.0.endpoint.subscribe();
        let mut addresses = self
            .endpoint()
            .watch_addr()
            .or(self.endpoint().home_relay_status());
        rocky_platform::task::spawn(async move {
            loop {
                tokio::select! {
                    _ = events.closed() => break,
                    changed = endpoints.changed() => {
                        if changed.is_err() { break; }
                        let endpoint = endpoints.borrow_and_update();
                        addresses = endpoint.watch_addr().or(endpoint.home_relay_status());
                        events.send_replace(());
                    }
                    changed = addresses.updated() => {
                        let endpoint_closed = changed.is_err();
                        if endpoint_closed {
                            tokio::select! {
                                _ = events.closed() => break,
                                replaced = endpoints.changed() => { if replaced.is_err() { break; } }
                            }
                            let endpoint = endpoints.borrow_and_update();
                            addresses = endpoint.watch_addr().or(endpoint.home_relay_status());
                        }
                        events.send_replace(());
                    }
                }
            }
        });
        Some(receiver)
    }

    /// The relay setting the attachment started with, or `None` for an application's own endpoint.
    pub fn initial_relay_configuration(&self) -> Option<String> {
        self.0.initial_relay_configuration.clone()
    }
    /// The state of each home relay connection.
    pub fn relays(&self) -> Option<Vec<RelayStatus>> {
        Some(
            self.endpoint()
                .home_relay_status()
                .get()
                .iter()
                .map(|relay| RelayStatus {
                    url: relay.url().to_string(),
                    connected: relay.is_connected(),
                    last_error: relay.last_error().map(ToString::to_string),
                    auth_denied_reason: relay.auth_denied_reason().map(str::to_owned),
                })
                .collect(),
        )
    }
    /// The peer finder, or `None` when discovery is off.
    pub fn discovery(&self) -> Option<Arc<dyn Discovery>> {
        let finder = self.0.finder.clone()?;
        Some(Arc::new(finder))
    }
    /// This attachment as a started connector with its discovery, for `Network::Attached`.
    pub fn connected(&self) -> Connected {
        let finder = (self.0.discover != Discover::default())
            .then(|| discovery::Finder::new(self.0.endpoint.subscribe(), self.0.discover.clone()));
        let discovery = finder
            .clone()
            .map(|finder| Arc::new(finder) as Arc<dyn Discovery>);
        let route = Arc::new(Route::with_finder(finder));
        Connected {
            transport: Arc::new(Port {
                attachment: self.clone(),
                route,
            }),
            discovery,
            changes: self.connectivity(),
        }
    }
}

impl Attachment {
    async fn connect_scoped(
        &self,
        address: &str,
        protocol: Protocol,
        scope: Option<Scope>,
        route_owner: Option<Arc<Route>>,
    ) -> Result<Stream, Failure> {
        let span = tracing::debug_span!("iroh_connect", outcome = tracing::field::Empty);
        async move {
            let result = tracing::Instrument::instrument(
                async move {
                    if *self.0.closed.borrow() {
                        return Err("the iroh attachment is closed".into());
                    }
                    refuse_closed_port(route_owner.as_deref())?;
                    if self.endpoint().is_closed() {
                        self.revive().await?;
                    }
                    let address = endpoint_addr(address)?;
                    let port = || {
                        route_owner
                            .as_deref()
                            .map_or(PortState::Unbound, PortState::of)
                    };
                    let key = (address.id, protocol);
                    let cached = self.0.connections.lock().await.get(&key).cloned();
                    if protocol != Protocol::Link
                        && let Some(cached) = cached
                        && cached.reusable()
                        && let Ok(Ok(pair)) =
                            rocky_platform::time::timeout(TIMEOUT, cached.connection.open_bi())
                                .await
                    {
                        self.0.connections.lock().await.insert(
                            key,
                            CachedConnection {
                                used: rocky_platform::time::Instant::now(),
                                ..cached.clone()
                            },
                        );
                        let mut stream = stream(cached.connection, pair, cached.reusable, port());
                        if protocol != Protocol::Coordinate {
                            scoped::outbound(&mut stream, scope, 0).await?;
                        }
                        return Ok(stream);
                    }
                    let dial = |address: EndpointAddr| {
                        let endpoint = self.endpoint();
                        async move {
                            rocky_platform::time::timeout(
                                TIMEOUT,
                                endpoint.connect(address, alpn(protocol)),
                            )
                            .await
                        }
                    };
                    let dialed_at_home = match &self.0.home {
                        Some(home) => home.spawn(dial(address.clone())).await.ok(),
                        None => None,
                    };
                    let connection = match dialed_at_home {
                        Some(dialed) => dialed,
                        None => dial(address).await,
                    }??;
                    if protocol == Protocol::Link {
                        return rocky_platform::time::timeout(
                            TIMEOUT,
                            link_stream(connection, true, &self.0, scope, route_owner),
                        )
                        .await?
                        .map(|(stream, _)| stream);
                    }
                    let pair =
                        rocky_platform::time::timeout(TIMEOUT, connection.open_bi()).await??;
                    let reusable = Arc::new(AtomicBool::new(true));
                    let mut connections = self.0.connections.lock().await;
                    connections.retain(|_, cached| cached.reusable());
                    if connections.len() >= CACHED {
                        connections.clear();
                    }
                    connections.insert(
                        key,
                        CachedConnection {
                            connection: connection.clone(),
                            used: rocky_platform::time::Instant::now(),
                            reusable: reusable.clone(),
                        },
                    );
                    drop(connections);
                    let mut stream = stream(connection, pair, reusable, port());
                    if protocol != Protocol::Coordinate {
                        scoped::outbound(&mut stream, scope, 0).await?;
                    }
                    Ok(stream)
                },
                span.clone(),
            )
            .await;
            span.record("outcome", if result.is_ok() { "ok" } else { "error" });
            result
        }
        .await
    }
    async fn dial_routes(
        &self,
        addresses: &[String],
        protocol: Protocol,
        scope: Scope,
        route_owner: Arc<Route>,
    ) -> Result<Stream, Failure> {
        refuse_closed_port(Some(&route_owner))?;
        let mut failure: Failure = "no route".into();
        for address in addresses {
            match rocky_platform::time::timeout(
                3 * TIMEOUT,
                self.connect_scoped(address, protocol, Some(scope), Some(route_owner.clone())),
            )
            .await
            {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(error)) => {
                    if !error.is::<ScopeRefusal>() {
                        self.forget_route(address, protocol, scope);
                    }
                    failure = error;
                }
                Err(error) => {
                    self.forget_route(address, protocol, scope);
                    failure = error.into();
                }
            }
        }
        Err(failure)
    }
}

impl Attachment {
    fn default_port(&self) -> Port {
        Port {
            attachment: self.clone(),
            route: self.0.default_route.clone(),
        }
    }
    /// The network paths to a peer at a route Rocky printed, with their statistics.
    pub async fn paths(&self, address: &str) -> Result<Vec<NetworkPath>, Failure> {
        let address = endpoint_addr(address)?;
        let connections: Vec<_> = self
            .0
            .connections
            .lock()
            .await
            .iter()
            .filter(|((id, _), _)| *id == address.id)
            .map(|(_, cached)| cached.connection.clone())
            .collect();
        let mut paths: Vec<_> = connections
            .into_iter()
            .flat_map(|connection| Paths(connection).paths())
            .collect();
        for link in self
            .0
            .link_connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(std::sync::Weak::upgrade)
        {
            if link.0.remote_id() == address.id {
                paths.extend(Paths(link.0.clone()).paths());
            }
        }
        Ok(paths)
    }
}
impl Transport for Attachment {
    fn schemes(&self) -> &[&str] {
        &[SCHEME]
    }
    fn address(&self) -> BoxFuture<'_, Result<Option<String>, Failure>> {
        Box::pin(async move { self.default_port().address().await })
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
        scope: Option<rocky_core::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move { self.default_port().connect(address, protocol, scope).await })
    }
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        Box::pin(async move { self.default_port().request(addresses, scope, request).await })
    }
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move { self.default_port().session(addresses, scope).await })
    }
    fn accept(&self) -> BoxFuture<'_, Option<rocky_core::net::Incoming>> {
        Box::pin(async move { self.default_port().accept().await })
    }
    fn listen(&self, scope: Option<Scope>, listening: bool) -> Result<(), Failure> {
        self.default_port().listen(scope, listening)
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Attachment::close(self)
    }
}

struct Port {
    attachment: Attachment,
    route: Arc<Route>,
}
impl std::ops::Deref for Port {
    type Target = Attachment;
    fn deref(&self) -> &Attachment {
        &self.attachment
    }
}
impl Transport for Port {
    fn schemes(&self) -> &[&str] {
        &[SCHEME]
    }
    fn address(&self) -> BoxFuture<'_, Result<Option<String>, Failure>> {
        Box::pin(async {
            if self.0.relays {
                let _ =
                    rocky_platform::time::timeout(HOME_RELAY_WAIT, self.endpoint().online()).await;
            }
            Ok(Some(route(&self.endpoint().addr())?))
        })
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
        scope: Option<rocky_core::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(
            self.attachment
                .connect_scoped(address, protocol, scope, Some(self.route.clone())),
        )
    }
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        Box::pin(async move {
            let mut failure: Failure = "no route".into();
            for address in addresses {
                let result = rocky_platform::time::timeout(3 * TIMEOUT, async {
                    let mut stream = self
                        .connect_scoped(
                            address,
                            Protocol::Rpc,
                            Some(scope),
                            Some(self.route.clone()),
                        )
                        .await?;
                    framing::write(&mut stream, request).await?;
                    let reply = framing::read(&mut stream).await?;
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
        scope: Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(self.dial_routes(addresses, Protocol::Link, scope, self.route.clone()))
    }
    fn accept(&self) -> BoxFuture<'_, Option<rocky_core::net::Incoming>> {
        Box::pin(async {
            let mut closed = self.route.closed.subscribe();
            let mut incoming = tokio::select! {
                biased;
                _ = closed.wait_for(|closed| *closed) => return None,
                incoming = self.route.incoming.lock() => incoming,
            };
            tokio::select! {
                biased;
                _ = closed.wait_for(|closed| *closed) => None,
                accepted = incoming.recv() => accepted,
            }
        })
    }
    fn listen(&self, scope: Option<Scope>, listening: bool) -> Result<(), Failure> {
        let mut routes = self.0.routes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut bound = self
            .route
            .scope
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        routes.retain(|_, route| route.strong_count() > 0);
        if listening {
            if scope.is_none()
                && self
                    .0
                    .protocols
                    .iter()
                    .any(|protocol| *protocol != Protocol::Coordinate)
            {
                return Err(ScopeRefusal::UnknownScope.into());
            }
            if *self.route.closed.borrow() {
                return Err(ScopeRefusal::NotListening.into());
            }
            if self.route.listening.load(Ordering::SeqCst) && *bound != scope {
                return Err(ScopeRefusal::AlreadyAttached.into());
            }
            if routes
                .get(&scope)
                .and_then(std::sync::Weak::upgrade)
                .is_some_and(|route| !Arc::ptr_eq(&route, &self.route))
            {
                return Err(ScopeRefusal::AlreadyAttached.into());
            }
            *bound = scope;
            routes.insert(scope, Arc::downgrade(&self.route));
        } else if routes
            .get(&scope)
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|route| Arc::ptr_eq(&route, &self.route))
        {
            routes.remove(&scope);
        }
        self.route.listening.store(listening, Ordering::SeqCst);
        Ok(())
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let scope = *self
                .route
                .scope
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let _ = self.listen(scope, false);
            self.route.closed.send_replace(true);
            if let Some(finder) = &self.route.finder {
                finder.close();
            }
            self.route.incoming.lock().await.close();
            for link in self
                .0
                .link_connections
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter_map(std::sync::Weak::upgrade)
            {
                let owner = link
                    .2
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade);
                if owner.is_some_and(|owner| Arc::ptr_eq(&owner, &self.route)) {
                    link.0.close(CLOSED.into(), b"database detached");
                }
            }
            if self.0.restart.lock().await.is_some() {
                self.attachment.close().await;
            }
        })
    }
}
impl Attachment {
    fn forget_route(&self, address: &str, protocol: Protocol, scope: Scope) {
        let Ok(address) = endpoint_addr(address) else {
            return;
        };
        if protocol == Protocol::Link {
            for link in self
                .0
                .link_connections
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter_map(std::sync::Weak::upgrade)
            {
                if link.0.remote_id() == address.id
                    && *link.1.lock().unwrap_or_else(PoisonError::into_inner) == Some(scope)
                {
                    link.0.close(CLOSED.into(), b"route failed");
                }
            }
        } else if let Ok(mut connections) = self.0.connections.try_lock() {
            connections.remove(&(address.id, protocol));
        }
    }
    /// Closes every connection to the peer at a route, so the next dial starts afresh.
    pub fn forget(&self, address: &str) {
        let Ok(address) = endpoint_addr(address) else {
            return;
        };
        for link in self
            .0
            .link_connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(std::sync::Weak::upgrade)
        {
            if link.0.remote_id() == address.id {
                link.0.close(CLOSED.into(), b"forgotten");
            }
        }
        if let Ok(mut connections) = self.0.connections.try_lock() {
            connections.retain(|(id, _), cached| {
                let keep = *id != address.id;
                if !keep {
                    cached.connection.close(CLOSED.into(), b"forgotten");
                }
                keep
            });
        }
    }
    /// Closes the endpoint's sockets as the system does to a backgrounded app, for fault tests.
    #[cfg(feature = "qualification")]
    pub fn fault_close_sockets(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async {
            self.endpoint().close().await;
            Ok(())
        })
    }
    /// Revives the endpoint after an app returns to the foreground, rebinding sockets the system
    /// closed.
    pub fn foreground(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async {
            if self.endpoint().is_closed() {
                return self.revive().await;
            }
            if *self.0.closed.borrow() {
                return Err("the iroh attachment is closed".into());
            }
            if self.0.restart.lock().await.is_some() {
                self.endpoint().network_change().await;
            }
            Ok(())
        })
    }
    /// Stops every database's route on it and, for an endpoint `Options` bound, the endpoint
    /// and its router; an application's own endpoint stays open.
    pub fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let restart = self.0.restart.lock().await;
            self.0.closed.send_replace(true);
            self.0.default_route.closed.send_replace(true);
            if let Some(finder) = &self.0.finder {
                finder.close();
            }
            let routes: Vec<_> = self
                .0
                .routes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .drain()
                .filter_map(|(_, route)| route.upgrade())
                .collect();
            for route in routes {
                route.closed.send_replace(true);
                if let Some(finder) = &route.finder {
                    finder.close();
                }
                route.incoming.lock().await.close();
            }
            for (_, cached) in self.0.connections.lock().await.drain() {
                cached.connection.close(CLOSED.into(), b"closed");
            }
            for link in self
                .0
                .link_connections
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .drain(..)
                .filter_map(|link| link.upgrade())
            {
                link.0.close(CLOSED.into(), b"closed");
            }
            let router = self
                .0
                .router
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(router) = router {
                let _ = router.shutdown().await;
            }
            if restart.is_some() {
                self.endpoint().close().await;
            }
        })
    }
}

/// How a started endpoint reaches peers; pass it to `Network::Owned`, and the default uses n0's
/// public relays and lookup.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// `None` or `"default"` for n0's relays, `"staging"`, `"disabled"`, or a relay URL.
    #[serde(default)]
    pub relay: Option<String>,
    /// `None` or `"default"` for n0's lookup service, `"disabled"`, or a pkarr relay URL.
    #[serde(default)]
    pub lookup: Option<String>,
    /// Finds the database's other devices through a rendezvous record on the lookup service; on by
    /// default.
    #[serde(default = "enabled")]
    pub topic: bool,
    /// Finds peers on the local network with mDNS; on by default.
    #[serde(default = "enabled")]
    pub local: bool,
    /// A socket address to bind instead of every interface; a browser binds none.
    #[serde(default)]
    pub bind: Option<String>,
}
fn enabled() -> bool {
    true
}
impl Default for Options {
    fn default() -> Self {
        Self {
            relay: None,
            lookup: None,
            topic: true,
            local: true,
            bind: None,
        }
    }
}
impl Options {
    /// Binds only `bind`, with no relay, lookup or discovery, for tests and closed networks.
    pub fn isolated(bind: &str) -> Self {
        Self {
            relay: Some("disabled".into()),
            lookup: Some("disabled".into()),
            topic: false,
            local: false,
            bind: Some(bind.into()),
        }
    }
    /// The discovery these options turn on.
    pub fn discover(&self) -> Discover {
        let topic = match (self.topic, self.lookup.as_deref().unwrap_or("default")) {
            (false, _) => Topic::Off,
            (true, "disabled") => {
                Topic::Unavailable("the topic needs the lookup service, which is disabled".into())
            }
            (true, "default") => Topic::On(DEFAULT_RENDEZVOUS.into()),
            (true, url) => Topic::On(url.into()),
        };
        Discover {
            topic,
            local: self.local,
        }
    }
    fn relays(&self) -> bool {
        self.relay.as_deref() != Some("disabled")
    }
    fn allows_multicast(&self) -> bool {
        self.local && !cfg!(target_os = "ios")
    }
    /// Binds an endpoint whose iroh key is derived from the database's route secret.
    pub async fn bind(&self, route_secret: [u8; 32]) -> Result<Endpoint, Failure> {
        let relay_mode = match self.relay.as_deref().unwrap_or("default") {
            "default" => RelayMode::Default,
            "staging" => RelayMode::Staging,
            "disabled" => RelayMode::Disabled,
            url => RelayMode::Custom(url.parse::<RelayUrl>()?.into()),
        };
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&route_secret))
            .relay_mode(relay_mode);
        #[cfg(feature = "qualification")]
        if self.bind.as_deref().is_some_and(|bind| {
            bind.parse::<std::net::SocketAddr>()
                .is_ok_and(|address| address.ip().is_loopback())
        }) {
            builder = builder.transport_config(
                iroh::endpoint::QuicTransportConfig::builder()
                    .initial_rtt(LOOPBACK_INITIAL_RTT)
                    .build(),
            );
        }
        if !self.allows_multicast() {
            builder = builder.portmapper_config(PortmapperConfig::Disabled);
        }
        match self.lookup.as_deref().unwrap_or("default") {
            "disabled" => {}
            "default" => {
                builder = builder
                    .address_lookup(PkarrPublisher::n0_dns())
                    .address_lookup(PkarrResolver::n0_dns());
                #[cfg(not(target_family = "wasm"))]
                {
                    builder = builder.address_lookup(DnsAddressLookup::n0_dns());
                }
            }
            url => {
                builder = builder
                    .address_lookup(PkarrPublisher::builder(url.parse()?))
                    .address_lookup(PkarrResolver::builder(url.parse()?));
            }
        }
        if let Some(bind) = &self.bind {
            #[cfg(target_family = "wasm")]
            return Err(
                format!("a browser binds no socket such as {bind}: it is relay-only").into(),
            );
            #[cfg(not(target_family = "wasm"))]
            {
                builder = builder.clear_ip_transports().bind_addr(bind.as_str())?;
            }
        }
        if cfg!(target_family = "wasm") && !self.relays() {
            return Err("a browser reaches peers only through a relay".into());
        }
        Ok(builder.bind().await?)
    }
    /// Binds under a fresh random key and serves no protocol, for a client such as a coordinator
    /// connection; closing the attachment or any of its transports stops the endpoint.
    pub async fn client(&self) -> Result<Attachment, Failure> {
        let secret = rocky_platform::entropy::random();
        let attachment = Attachment::serving(self.bind(secret).await?, Vec::new());
        *attachment.0.restart.lock().await = Some((self.clone(), secret));
        Ok(attachment)
    }
    /// Binds and serves every Rocky protocol, returning an attachment the application keeps.
    pub async fn start(&self, route_secret: [u8; 32]) -> Result<Attachment, Failure> {
        self.start_serving(route_secret, Protocol::ALL.to_vec())
            .await
    }
    /// Binds and serves only the named protocols.
    pub async fn start_serving(
        &self,
        route_secret: [u8; 32],
        protocols: Vec<Protocol>,
    ) -> Result<Attachment, Failure> {
        let endpoint = self.bind(route_secret).await?;
        let attachment = Attachment::build(
            endpoint.clone(),
            self.relays(),
            protocols,
            self.discover(),
            Some(self.relay.clone().unwrap_or_else(|| "default".into())),
        );
        *attachment.0.restart.lock().await = Some((self.clone(), route_secret));
        let router = attachment.register(Router::builder(endpoint)).spawn();
        *attachment
            .0
            .router
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(router);
        Ok(attachment)
    }
}
impl Connector for Options {
    fn start(
        &self,
        _accepts: bool,
        route_secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Connected, Failure>> {
        Box::pin(async move { Ok(Options::start(self, route_secret).await?.connected()) })
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn coordinate(client: &dyn Transport, server: &Attachment) -> Result<(), Failure> {
        let mut near = client
            .connect(
                &server.address().await?.ok_or("no route")?,
                Protocol::Coordinate,
                None,
            )
            .await?;
        near.write_u8(7).await?;
        let Some(Incoming::Stream(Protocol::Coordinate, mut far)) = server.accept().await else {
            return Err("coordinator stream expected".into());
        };
        assert_eq!(far.read_u8().await?, 7);
        far.write_u8(9).await?;
        assert_eq!(near.read_u8().await?, 9);
        Ok(())
    }

    async fn cancel_pending_read(stream: &mut Stream) {
        let read = stream.read_u8();
        tokio::pin!(read);
        std::future::poll_fn(|cx| {
            assert!(
                read.as_mut().poll(cx).is_pending(),
                "the peer has not answered yet"
            );
            Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn cancelling_a_stream_retires_reuse_without_closing_concurrent_streams()
    -> Result<(), Failure> {
        rocky_platform::time::timeout(Duration::from_secs(30), async {
            let client = Attachment::serving(
                Options::isolated("127.0.0.1:0").bind([81; 32]).await?,
                vec![],
            );
            let server = Attachment::serving(
                Options::isolated("127.0.0.1:0").bind([82; 32]).await?,
                vec![Protocol::Coordinate],
            );
            let router = server.register(Router::builder(server.endpoint())).spawn();
            server.listen(None, true)?;
            coordinate(&client, &server).await?;
            let key = (server.endpoint().id(), Protocol::Coordinate);
            let cached = client.0.connections.lock().await[&key].connection.clone();
            assert!(
                cached.close_reason().is_none(),
                "completed exchanges remain reusable"
            );

            let mut near = client
                .connect(
                    &server.address().await?.ok_or("no route")?,
                    Protocol::Coordinate,
                    None,
                )
                .await?;
            assert_eq!(
                client.0.connections.lock().await[&key]
                    .connection
                    .stable_id(),
                cached.stable_id()
            );
            near.write_u8(7).await?;
            let Some(Incoming::Stream(Protocol::Coordinate, mut far)) = server.accept().await
            else {
                return Err("coordinator stream expected".into());
            };
            assert_eq!(far.read_u8().await?, 7);
            let mut healthy = client
                .connect(
                    &server.address().await?.ok_or("no route")?,
                    Protocol::Coordinate,
                    None,
                )
                .await?;
            healthy.write_u8(11).await?;
            let Some(Incoming::Stream(Protocol::Coordinate, mut healthy_far)) =
                server.accept().await
            else {
                return Err("second coordinator stream expected".into());
            };
            assert_eq!(healthy_far.read_u8().await?, 11);
            assert_eq!(
                client.0.connections.lock().await[&key]
                    .connection
                    .stable_id(),
                cached.stable_id()
            );
            cancel_pending_read(&mut near).await;
            let reusable = client.0.connections.lock().await[&key].reusable.clone();
            drop(near);
            coordinate(&client, &server).await?;
            assert_ne!(
                client.0.connections.lock().await[&key]
                    .connection
                    .stable_id(),
                cached.stable_id()
            );
            healthy_far.write_u8(13).await?;
            assert_eq!(healthy.read_u8().await?, 13);
            assert!(
                cached.close_reason().is_none(),
                "cancellation closed a shared connection"
            );
            assert!(
                !reusable.load(Ordering::SeqCst),
                "cancelled exchange left its connection reusable"
            );
            drop((far, healthy, healthy_far));
            client.close().await;
            client.endpoint().close().await;
            router.shutdown().await?;
            Ok::<_, Failure>(())
        })
        .await?
    }

    async fn coordinator_server(secret: u8) -> Result<(Attachment, Router), Failure> {
        let server = Attachment::serving(
            Options::isolated("127.0.0.1:0").bind([secret; 32]).await?,
            vec![Protocol::Coordinate],
        );
        let router = server.register(Router::builder(server.endpoint())).spawn();
        server.listen(None, true)?;
        Ok((server, router))
    }

    async fn open_stream(
        port: &dyn Transport,
        server: &Attachment,
    ) -> Result<(Stream, Stream), Failure> {
        let mut near = port
            .connect(
                &server.address().await?.ok_or("no route")?,
                Protocol::Coordinate,
                None,
            )
            .await?;
        near.write_u8(7).await?;
        let Some(Incoming::Stream(Protocol::Coordinate, mut far)) = server.accept().await else {
            return Err("coordinator stream expected".into());
        };
        assert_eq!(far.read_u8().await?, 7);
        Ok((near, far))
    }

    #[tokio::test]
    async fn closing_a_client_port_ends_its_streams_refuses_cached_dials_and_stops_the_endpoint()
    -> Result<(), Failure> {
        rocky_platform::time::timeout(Duration::from_secs(30), async {
            let (server, router) = coordinator_server(83).await?;
            let client = Options::isolated("127.0.0.1:0").client().await?;
            let port = client.transport();
            coordinate(&*port, &server).await?;
            let (mut open, _far) = open_stream(&*port, &server).await?;

            port.close().await;

            assert!(
                open.read_u8().await.is_err(),
                "a stream open through the port ends when the port closes"
            );
            assert!(
                port.connect(
                    &server.address().await?.ok_or("no route")?,
                    Protocol::Coordinate,
                    None
                )
                .await
                .is_err(),
                "a closed port dials nothing, not even over a cached connection"
            );
            assert!(
                client.endpoint().is_closed(),
                "closing a client's port stops the endpoint the client bound"
            );
            router.shutdown().await?;
            Ok::<_, Failure>(())
        })
        .await?
    }

    #[tokio::test]
    async fn closing_one_port_of_an_application_endpoint_leaves_its_other_ports_and_the_endpoint()
    -> Result<(), Failure> {
        rocky_platform::time::timeout(Duration::from_secs(30), async {
            let (server, router) = coordinator_server(84).await?;
            let application = Attachment::serving(
                Options::isolated("127.0.0.1:0").bind([85; 32]).await?,
                vec![],
            );
            let closing = application.transport();
            let staying = application.transport();
            coordinate(&*closing, &server).await?;
            let (mut open, _far) = open_stream(&*closing, &server).await?;
            let (mut kept, mut kept_far) = open_stream(&*staying, &server).await?;

            closing.close().await;

            assert!(
                open.read_u8().await.is_err(),
                "a stream open through the closed port ends"
            );
            assert!(
                closing
                    .connect(
                        &server.address().await?.ok_or("no route")?,
                        Protocol::Coordinate,
                        None
                    )
                    .await
                    .is_err(),
                "the closed port dials nothing"
            );
            kept_far.write_u8(9).await?;
            assert_eq!(
                kept.read_u8().await?,
                9,
                "another port's stream on the same connection carries on"
            );
            coordinate(&*staying, &server).await?;
            assert!(
                !application.endpoint().is_closed(),
                "a port never closes the application's endpoint"
            );
            application.close().await;
            application.endpoint().close().await;
            router.shutdown().await?;
            Ok::<_, Failure>(())
        })
        .await?
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_idles_only_after_its_last_stream_ends() {
        let (active, mut activity) = watch::channel(0);
        let active = Arc::new(active);
        let slots = Arc::new(Semaphore::new(STREAMS_PER_CONNECTION));
        let open = || {
            let slots = slots.clone();
            let active = active.clone();
            async move {
                active.send_modify(|active| *active += 1);
                Slot {
                    _permit: slots.acquire_owned().await.unwrap(),
                    active,
                }
            }
        };
        let start = rocky_platform::time::Instant::now();
        let long_stream = open().await;
        let streams = async {
            rocky_platform::time::sleep(IDLE * 3).await;
            drop(long_stream);
            rocky_platform::time::sleep(IDLE / 2).await;
            let opened_while_idle = open().await;
            rocky_platform::time::sleep(IDLE / 2).await;
            drop(opened_while_idle);
        };
        tokio::join!(idle(&mut activity, IDLE), streams);
        assert_eq!(start.elapsed(), IDLE * 5);
    }
}
