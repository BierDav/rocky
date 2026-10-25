//! Rocky's network port on iroh. `Attachment` serves Rocky's protocols on
//! an endpoint and router the application owns; `Options` is the starter
//! that binds and owns its own. See `docs/runtime/sdk-transports.md`.
use rocky_core::net::{
    BoxFuture, Connector, Discovery, Failure, NetworkPath, PathSource, Protocol, SessionStream,
    Stream, Transport,
};
use serde::Deserialize;
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
    io::{AsyncRead, AsyncReadExt, AsyncWrite, Join, ReadBuf},
    sync::{Mutex, Semaphore, mpsc, watch},
};

mod discovery;
#[cfg(feature = "test-utils")]
pub mod testing;
pub use discovery::{
    DEFAULT_RENDEZVOUS, Discover, LOCAL_SERVICE, RENDEZVOUS_AGE, RENDEZVOUS_BYTES,
    RENDEZVOUS_CONTACTS, RENDEZVOUS_INTERVAL, Topic, rendezvous_key,
};

/// The iroh version these handlers attach to; build the endpoint with it.
pub use iroh;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey, TransportAddr, Watcher,
    address_lookup::{DnsAddressLookup, PkarrPublisher, PkarrResolver},
    endpoint::{Connection, LocalTransportAddr, PortmapperConfig, RecvStream, SendStream, presets},
    protocol::{AcceptError, ProtocolHandler, Router, RouterBuilder},
};

/// The `Transport::kind` of iroh addresses.
pub const KIND: &str = "iroh";
/// Dialing and opening a stream each take at most this long.
pub const TIMEOUT: Duration = Duration::from_secs(15);
/// Inbound streams waiting for the attached service.
pub const PENDING: usize = 32;
/// Inbound connections carrying Rocky protocols at once.
pub const CONNECTIONS: usize = 64;
/// Outbound connections kept for reuse.
pub const CACHED: usize = 64;
/// Inbound streams one connection has in flight; further streams wait in
/// QUIC, so no single peer fills the shared queue.
pub const STREAMS_PER_CONNECTION: usize = 4;
/// Close codes of connections Rocky refuses.
const CLOSED: u32 = 1;
const BUSY: u32 = 2;
/// Binding a fixed port again waits this long for the dead socket to go.
const REBIND: Duration = Duration::from_secs(3);

/// One bidirectional stream; it keeps its connection open.
struct QuicStream {
    _connection: Connection,
    io: Join<RecvStream, SendStream>,
    /// An inbound stream's share of its connection.
    _slot: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl AsyncRead for QuicStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for QuicStream {
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
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}
fn stream(connection: Connection, (send, recv): (SendStream, RecvStream)) -> Stream {
    Stream::with_paths(
        QuicStream {
            _connection: connection.clone(),
            io: tokio::io::join(recv, send),
            _slot: None,
        },
        Arc::new(Paths(connection)),
    )
}

/// A link owns its connection, including the lanes hidden from the service.
struct LinkConnection(Connection);
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
            // Keep the dedicated connection alive until the peer has the
            // final bytes, including a resolution reply or signed Close.
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
    links: &std::sync::Mutex<Vec<std::sync::Weak<LinkConnection>>>,
) -> Result<Stream, Failure> {
    let owner = Arc::new(LinkConnection(connection.clone()));
    {
        let mut links = links.lock().unwrap_or_else(PoisonError::into_inner);
        links.retain(|link| link.strong_count() > 0);
        if links.len() >= 2 * CONNECTIONS {
            return Err("link connection quota".into());
        }
        links.push(Arc::downgrade(&owner));
    }
    let mut lanes: Vec<Box<dyn SessionStream>> = Vec::new();
    for lane in 0..5u8 {
        let (mut send, mut recv) = if dialing {
            connection.open_bi().await?
        } else {
            connection.accept_bi().await?
        };
        // Opening a QUIC stream becomes visible only after its first write.
        if dialing {
            send.write_all(&[lane]).await?;
        } else if recv.read_u8().await? != lane {
            return Err("link lane order".into());
        }
        lanes.push(Box::new(LinkLane {
            io: tokio::io::join(recv, send),
            shutdown: None,
            finished: false,
            _owner: owner.clone(),
        }));
    }
    let control = lanes.remove(0);
    Ok(Stream::with_paths(control, Arc::new(Paths(connection))).with_lanes(lanes))
}

/// iroh's per-path QUIC measurements of one connection.
struct Paths(Connection);
impl PathSource for Paths {
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

struct Shared {
    endpoint: watch::Sender<Endpoint>,
    restart: Mutex<Option<(Options, [u8; 32])>>,
    sender: mpsc::Sender<(Protocol, Stream)>,
    incoming: Mutex<mpsc::Receiver<(Protocol, Stream)>>,
    connections: Mutex<HashMap<(EndpointId, Protocol), Connection>>,
    link_connections: std::sync::Mutex<Vec<std::sync::Weak<LinkConnection>>>,
    inbound: Semaphore,
    inbound_links: Semaphore,
    closed: watch::Sender<bool>,
    /// Whether a service takes what the handler queues.
    listening: AtomicBool,
    refused: AtomicU64,
    /// The starter's router, which owns the endpoint.
    router: std::sync::Mutex<Option<Router>>,
    /// Whether an address waits for a home relay.
    relays: bool,
    initial_relay_configuration: Option<String>,
    /// The protocols this attachment answers.
    protocols: Vec<Protocol>,
    /// Present only when the attachment was asked to find peers.
    finder: Option<discovery::Finder>,
    /// The runtime the attachment was made on, normally the endpoint's. A
    /// connection's driver runs where it was dialed: dialed on a database's
    /// runtime it would end with the database and leave the endpoint
    /// waiting for it at close.
    home: Option<tokio::runtime::Handle>,
}

/// Rocky's protocols on one endpoint. Register `handler` on the router
/// that owns the endpoint and attach `transport` to a database. Closing
/// the database, its network service or this attachment leaves the
/// endpoint and router to their owner. While no service is attached,
/// Rocky's ALPNs are refused at once.
#[derive(Clone)]
pub struct Attachment(Arc<Shared>);
impl Attachment {
    /// Uses `endpoint` to dial. Nothing is accepted until the handler is
    /// registered on a router.
    pub fn new(endpoint: Endpoint) -> Self {
        Self::build(
            endpoint,
            false,
            Protocol::ALL.to_vec(),
            Discover::default(),
            None,
        )
    }
    /// Like `new`, and finds peers as `discover` asks. Rocky announces
    /// beside the endpoint: its own lookup services stay as they are.
    pub fn discovering(endpoint: Endpoint, discover: Discover) -> Self {
        Self::build(endpoint, false, Protocol::ALL.to_vec(), discover, None)
    }
    /// Answers `protocols` instead of a database's, such as a coordinator
    /// voter's `Protocol::Coordinate`. Dialing is unrestricted.
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
        let (sender, receiver) = mpsc::channel(PENDING);
        let endpoint = watch::channel(endpoint).0;
        let finder = (discover != Discover::default())
            .then(|| discovery::Finder::new(endpoint.subscribe(), discover));
        Self(Arc::new(Shared {
            home: tokio::runtime::Handle::try_current().ok(),
            finder,
            endpoint,
            restart: Mutex::new(None),
            sender,
            incoming: Mutex::new(receiver),
            connections: Mutex::new(HashMap::new()),
            link_connections: Default::default(),
            inbound: Semaphore::new(CONNECTIONS),
            inbound_links: Semaphore::new(CONNECTIONS),
            closed: watch::channel(false).0,
            listening: AtomicBool::new(false),
            refused: AtomicU64::new(0),
            router: std::sync::Mutex::new(None),
            relays,
            initial_relay_configuration,
            protocols,
        }))
    }
    /// The ALPNs Rocky answers, each a versioned protocol name.
    pub fn alpns() -> impl Iterator<Item = &'static [u8]> {
        Protocol::ALL.into_iter().map(Protocol::name)
    }
    /// The handler of every Rocky ALPN.
    pub fn handler(&self) -> Handler {
        Handler(self.0.clone())
    }
    /// Registers this attachment's ALPNs next to the application's own
    /// protocols: Rocky's database protocols unless built with `serving`.
    pub fn register(&self, router: RouterBuilder) -> RouterBuilder {
        self.0.protocols.iter().fold(router, |router, protocol| {
            router.accept(protocol.name(), self.handler())
        })
    }
    /// The network to attach to a database as `Network::Borrowed`.
    pub fn transport(&self) -> Arc<dyn Transport> {
        Arc::new(self.clone())
    }
    pub fn endpoint(&self) -> Endpoint {
        self.0.endpoint.borrow().clone()
    }
    /// Inbound connections and streams refused: closed, busy or queue full.
    pub fn refused(&self) -> u64 {
        self.0.refused.load(Ordering::Relaxed)
    }
    /// Binds a dead starter endpoint again with the same identity; a live
    /// one is left alone. A borrowed endpoint is its owner's to rebind.
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
        // UDP sockets close with the last clone of their endpoint: the
        // router and cached connections go first.
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
        if options.bind.is_some() {
            // A closed loopback stand-in releases the watched clone.
            let stand_in = Endpoint::builder(presets::Minimal)
                .secret_key(SecretKey::from_bytes(secret))
                .relay_mode(RelayMode::Disabled)
                .clear_ip_transports()
                .bind_addr("127.0.0.1:0")?
                .bind()
                .await?;
            stand_in.close().await;
            self.0.endpoint.send_replace(stand_in);
        }
        let started = tokio::time::Instant::now();
        let replacement = loop {
            match options.bind(*secret).await {
                Ok(endpoint) => break endpoint,
                Err(_) if started.elapsed() < REBIND => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error),
            }
        };
        let router = self.register(Router::builder(replacement.clone())).spawn();
        self.0.endpoint.send_replace(replacement);
        *self.0.router.lock().unwrap_or_else(PoisonError::into_inner) = Some(router);
        Ok(())
    }
    fn refuse(&self, connection: &Connection, code: u32, reason: &[u8]) {
        self.0.refused.fetch_add(1, Ordering::Relaxed);
        connection.close(code.into(), reason);
    }
}

/// Serves Rocky's ALPNs on a router. It takes no other connection and
/// never closes the endpoint.
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
            Protocol::named(connection.alpn()).filter(|p| self.0.protocols.contains(p))
        else {
            attachment.refuse(&connection, CLOSED, b"unknown protocol");
            return Ok(());
        };
        if *closed.borrow() || !self.0.listening.load(Ordering::SeqCst) {
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
            match tokio::time::timeout(
                TIMEOUT,
                link_stream(connection.clone(), false, &self.0.link_connections),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    if self.0.sender.try_send((protocol, stream)).is_err() {
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
        // Duplicate link handshakes must reach the service's tie-break/busy reply.
        let slots = Arc::new(Semaphore::new(STREAMS_PER_CONNECTION));
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
            };
            let (Ok(slot), Ok((send, recv))) = accepted else {
                break;
            };
            if !self.0.listening.load(Ordering::SeqCst) {
                attachment.refuse(&connection, CLOSED, b"no service");
                break;
            }
            let inbound = Stream::with_paths(
                QuicStream {
                    _connection: connection.clone(),
                    io: tokio::io::join(recv, send),
                    _slot: Some(slot),
                },
                Arc::new(Paths(connection.clone())),
            );
            // A full queue resets the stream: the dialer fails at once.
            if self.0.sender.try_send((protocol, inbound)).is_err() {
                self.0.refused.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }
}

impl Transport for Attachment {
    fn connectivity(&self) -> Option<watch::Receiver<()>> {
        let (events, receiver) = watch::channel(());
        let mut endpoints = self.0.endpoint.subscribe();
        let mut addresses = self
            .endpoint()
            .watch_addr()
            .or(self.endpoint().home_relay_status());
        tokio::spawn(async move {
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
                        if changed.is_err() {
                            // A closed endpoint's watcher ends. Wait for the starter's replacement.
                            tokio::select! {
                                _ = events.closed() => break,
                                changed = endpoints.changed() => { if changed.is_err() { break; } }
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

    fn initial_relay_configuration(&self) -> Option<String> {
        self.0.initial_relay_configuration.clone()
    }
    fn relays(&self) -> Option<Vec<rocky_core::net::RelayStatus>> {
        Some(
            self.endpoint()
                .home_relay_status()
                .get()
                .iter()
                .map(|relay| rocky_core::net::RelayStatus {
                    url: relay.url().to_string(),
                    connected: relay.is_connected(),
                    last_error: relay.last_error().map(ToString::to_string),
                    auth_denied_reason: relay.auth_denied_reason().map(str::to_owned),
                })
                .collect(),
        )
    }
    fn discovery(&self) -> Option<Arc<dyn Discovery>> {
        let finder = self.0.finder.clone()?;
        Some(Arc::new(finder))
    }
    fn kind(&self) -> &str {
        KIND
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async {
            if self.0.relays {
                // An address with a home relay is reachable beyond the LAN.
                let _ =
                    tokio::time::timeout(Duration::from_secs(5), self.endpoint().online()).await;
            }
            Ok(serde_json::to_string(&self.endpoint().addr())?)
        })
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        let span = tracing::debug_span!("iroh_connect", outcome = tracing::field::Empty);
        Box::pin(async move {
            let result = tracing::Instrument::instrument(
                async move {
                    if *self.0.closed.borrow() {
                        return Err("the iroh attachment is closed".into());
                    }
                    // An endpoint the OS reclaimed heals on the next dial.
                    if self.endpoint().is_closed() {
                        self.revive().await?;
                    }
                    let address: EndpointAddr = serde_json::from_str(address)?;
                    let key = (address.id, protocol);
                    let cached = self.0.connections.lock().await.get(&key).cloned();
                    if protocol != Protocol::Link
                        && let Some(connection) = cached
                        && connection.close_reason().is_none()
                        && let Ok(Ok(pair)) =
                            tokio::time::timeout(TIMEOUT, connection.open_bi()).await
                    {
                        return Ok(stream(connection, pair));
                    }
                    let dial = |address: EndpointAddr| {
                        let endpoint = self.endpoint();
                        async move {
                            tokio::time::timeout(
                                TIMEOUT,
                                endpoint.connect(address, protocol.name()),
                            )
                            .await
                        }
                    };
                    let dialed = match &self.0.home {
                        Some(home) => home.spawn(dial(address.clone())).await.ok(),
                        None => None,
                    };
                    // Without a home runtime, or once it ended, the caller's drives the dial.
                    let connection = match dialed {
                        Some(dialed) => dialed,
                        None => dial(address).await,
                    }??;
                    if protocol == Protocol::Link {
                        return tokio::time::timeout(
                            TIMEOUT,
                            link_stream(connection, true, &self.0.link_connections),
                        )
                        .await?;
                    }
                    let pair = tokio::time::timeout(TIMEOUT, connection.open_bi()).await??;
                    let mut connections = self.0.connections.lock().await;
                    connections.retain(|_, connection| connection.close_reason().is_none());
                    if connections.len() >= CACHED {
                        connections.clear();
                    }
                    connections.insert(key, connection.clone());
                    Ok(stream(connection, pair))
                },
                span.clone(),
            )
            .await;
            span.record("outcome", if result.is_ok() { "ok" } else { "error" });
            result
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<(Protocol, Stream)>> {
        Box::pin(async { self.0.incoming.lock().await.recv().await })
    }
    fn listen(&self, listening: bool) {
        self.0.listening.store(listening, Ordering::SeqCst);
    }
    /// A restarted peer leaves a cached connection that looks open.
    fn forget(&self, address: &str) {
        let Ok(address) = serde_json::from_str::<EndpointAddr>(address) else {
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
            connections.retain(|(id, _), connection| {
                let keep = *id != address.id;
                if !keep {
                    connection.close(CLOSED.into(), b"forgotten");
                }
                keep
            });
        }
    }
    #[cfg(feature = "qualification")]
    fn fault_close_sockets(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async {
            self.endpoint().close().await;
            Ok(())
        })
    }
    fn foreground(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async {
            // No clone of a dead endpoint may outlive its rebind.
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
    /// Ends Rocky's streams and connections. Only a starter's own router
    /// and endpoint are shut down; a borrowed endpoint stays open.
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let _restart = self.0.restart.lock().await;
            self.0.closed.send_replace(true);
            if let Some(finder) = &self.0.finder {
                finder.close();
            }
            self.0.incoming.lock().await.close();
            for (_, connection) in self.0.connections.lock().await.drain() {
                connection.close(CLOSED.into(), b"closed");
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
        })
    }
}

/// The starter endpoint: bound, routed and closed by Rocky.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// `default`, `staging`, `disabled` or a relay URL.
    #[serde(default)]
    pub relay: Option<String>,
    /// Where endpoint addresses are published and resolved: `default` for
    /// n0's DNS service, `disabled`, or the URL of a pkarr relay.
    #[serde(default)]
    pub lookup: Option<String>,
    /// Find domain members through the topic's rendezvous record on the
    /// lookup service.
    #[serde(default = "enabled")]
    pub topic: bool,
    /// Find domain members on the local network over mDNS.
    #[serde(default = "enabled")]
    pub local: bool,
    /// A local socket address to bind, such as a fixed port.
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
    /// Loopback and contacts only: no relay, lookup service or discovery.
    pub fn isolated(bind: &str) -> Self {
        Self {
            relay: Some("disabled".into()),
            lookup: Some("disabled".into()),
            topic: false,
            local: false,
            bind: Some(bind.into()),
        }
    }
    /// What these options ask an attachment to find peers with, for
    /// `Attachment::discovering` on an endpoint bound from them.
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
    /// Binds an endpoint with these options and `secret` as its key. The
    /// caller owns it: route it, then close it.
    pub async fn bind(&self, secret: [u8; 32]) -> Result<Endpoint, Failure> {
        let relay_mode = match self.relay.as_deref().unwrap_or("default") {
            "default" => RelayMode::Default,
            "staging" => RelayMode::Staging,
            "disabled" => RelayMode::Disabled,
            url => RelayMode::Custom(url.parse::<RelayUrl>()?.into()),
        };
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&secret))
            .relay_mode(relay_mode);
        #[cfg(feature = "qualification")]
        if self.bind.as_deref().is_some_and(|bind| {
            bind.parse::<std::net::SocketAddr>()
                .is_ok_and(|address| address.ip().is_loopback())
        }) {
            // Failed loopback dials otherwise drain for three seconds at QUIC's initial RTT.
            builder = builder.transport_config(
                iroh::endpoint::QuicTransportConfig::builder()
                    .initial_rtt(Duration::from_millis(10))
                    .build(),
            );
        }
        // Gateway discovery also uses multicast; a manual/local-disabled path must not probe it.
        if !self.local || cfg!(target_os = "ios") {
            builder = builder.portmapper_config(PortmapperConfig::Disabled);
        }
        match self.lookup.as_deref().unwrap_or("default") {
            "disabled" => {}
            "default" => {
                builder = builder
                    .address_lookup(PkarrPublisher::n0_dns())
                    .address_lookup(PkarrResolver::n0_dns())
                    .address_lookup(DnsAddressLookup::n0_dns());
            }
            url => {
                builder = builder
                    .address_lookup(PkarrPublisher::builder(url.parse()?))
                    .address_lookup(PkarrResolver::builder(url.parse()?));
            }
        }
        if let Some(bind) = &self.bind {
            builder = builder.clear_ip_transports().bind_addr(bind.as_str())?;
        }
        Ok(builder.bind().await?)
    }
    /// Binds the starter endpoint with a router of Rocky's protocols only.
    /// Closing the returned attachment's transport shuts both down.
    pub async fn start(&self, secret: [u8; 32]) -> Result<Attachment, Failure> {
        self.start_serving(secret, Protocol::ALL.to_vec()).await
    }
    /// The starter endpoint answering `protocols`, such as a voter's.
    pub async fn start_serving(
        &self,
        secret: [u8; 32],
        protocols: Vec<Protocol>,
    ) -> Result<Attachment, Failure> {
        let endpoint = self.bind(secret).await?;
        let attachment = Attachment::build(
            endpoint.clone(),
            self.relays(),
            protocols,
            self.discover(),
            Some(self.relay.clone().unwrap_or_else(|| "default".into())),
        );
        *attachment.0.restart.lock().await = Some((self.clone(), secret));
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
        secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Arc<dyn Transport>, Failure>> {
        Box::pin(async move { Ok(Options::start(self, secret).await?.transport()) })
    }
}
