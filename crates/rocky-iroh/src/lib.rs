//! Rocky's network port on iroh. `Attachment` serves Rocky's protocols on
//! an endpoint and router the application owns; `Options` is the starter
//! that binds and owns its own. See `docs/runtime/sdk-transports.md`.
use rocky_core::net::{BoxFuture, Connector, Failure, Protocol, Stream, Transport};
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
    io::{AsyncRead, AsyncWrite, Join, ReadBuf},
    sync::{Mutex, Semaphore, mpsc, watch},
};

/// The iroh version these handlers attach to; build the endpoint with it.
pub use iroh;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey, Watcher,
    address_lookup::{DnsAddressLookup, PkarrPublisher, PkarrResolver},
    endpoint::{Connection, RecvStream, SendStream, presets},
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
    Box::new(QuicStream {
        _connection: connection,
        io: tokio::io::join(recv, send),
        _slot: None,
    })
}

struct Shared {
    endpoint: Endpoint,
    sender: mpsc::Sender<(Protocol, Stream)>,
    incoming: Mutex<mpsc::Receiver<(Protocol, Stream)>>,
    connections: Mutex<HashMap<(EndpointId, Protocol), Connection>>,
    inbound: Semaphore,
    closed: watch::Sender<bool>,
    /// Whether a service takes what the handler queues.
    listening: AtomicBool,
    refused: AtomicU64,
    /// The starter's router, which owns the endpoint.
    router: std::sync::Mutex<Option<Router>>,
    /// Whether an address waits for a home relay.
    relays: bool,
    /// The protocols this attachment answers.
    protocols: Vec<Protocol>,
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
        Self::build(endpoint, false, Protocol::ALL.to_vec())
    }
    /// Answers `protocols` instead of a database's, such as a coordinator
    /// voter's `Protocol::Coordinate`. Dialing is unrestricted.
    pub fn serving(endpoint: Endpoint, protocols: Vec<Protocol>) -> Self {
        Self::build(endpoint, false, protocols)
    }
    fn build(endpoint: Endpoint, relays: bool, protocols: Vec<Protocol>) -> Self {
        let (sender, receiver) = mpsc::channel(PENDING);
        Self(Arc::new(Shared {
            endpoint,
            sender,
            incoming: Mutex::new(receiver),
            connections: Mutex::new(HashMap::new()),
            inbound: Semaphore::new(CONNECTIONS),
            closed: watch::channel(false).0,
            listening: AtomicBool::new(false),
            refused: AtomicU64::new(0),
            router: std::sync::Mutex::new(None),
            relays,
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
    pub fn endpoint(&self) -> &Endpoint {
        &self.0.endpoint
    }
    /// Inbound connections and streams refused: closed, busy or queue full.
    pub fn refused(&self) -> u64 {
        self.0.refused.load(Ordering::Relaxed)
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
        let Ok(_permit) = self.0.inbound.try_acquire() else {
            attachment.refuse(&connection, BUSY, b"busy");
            return Ok(());
        };
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
            let inbound = Box::new(QuicStream {
                _connection: connection.clone(),
                io: tokio::io::join(recv, send),
                _slot: Some(slot),
            });
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
        let mut addresses = self.0.endpoint.watch_addr();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = events.closed() => break,
                    changed = addresses.updated() => {
                        if changed.is_err() { break; }
                        events.send_replace(());
                    }
                }
            }
        });
        Some(receiver)
    }

    fn kind(&self) -> &str {
        KIND
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async {
            if self.0.relays {
                // An address with a home relay is reachable beyond the LAN.
                let _ =
                    tokio::time::timeout(Duration::from_secs(5), self.0.endpoint.online()).await;
            }
            Ok(serde_json::to_string(&self.0.endpoint.addr())?)
        })
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            if *self.0.closed.borrow() {
                return Err("the iroh attachment is closed".into());
            }
            let address: EndpointAddr = serde_json::from_str(address)?;
            let key = (address.id, protocol);
            let cached = self.0.connections.lock().await.get(&key).cloned();
            if let Some(connection) = cached
                && connection.close_reason().is_none()
                && let Ok(Ok(pair)) = tokio::time::timeout(TIMEOUT, connection.open_bi()).await
            {
                return Ok(stream(connection, pair));
            }
            let connection =
                tokio::time::timeout(TIMEOUT, self.0.endpoint.connect(address, protocol.name()))
                    .await??;
            let pair = tokio::time::timeout(TIMEOUT, connection.open_bi()).await??;
            let mut connections = self.0.connections.lock().await;
            connections.retain(|_, connection| connection.close_reason().is_none());
            if connections.len() >= CACHED {
                connections.clear();
            }
            connections.insert(key, connection.clone());
            Ok(stream(connection, pair))
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
    /// Ends Rocky's streams and connections. Only a starter's own router
    /// and endpoint are shut down; a borrowed endpoint stays open.
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.0.closed.send_replace(true);
            self.0.incoming.lock().await.close();
            for (_, connection) in self.0.connections.lock().await.drain() {
                connection.close(CLOSED.into(), b"closed");
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
    /// Publish and resolve endpoint addresses through n0's DNS service.
    #[serde(default = "enabled")]
    pub discovery: bool,
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
            discovery: true,
            bind: None,
        }
    }
}
impl Options {
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
        if self.discovery {
            builder = builder
                .address_lookup(PkarrPublisher::n0_dns())
                .address_lookup(PkarrResolver::n0_dns())
                .address_lookup(DnsAddressLookup::n0_dns());
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
        let attachment = Attachment::build(endpoint.clone(), self.relays(), protocols);
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
