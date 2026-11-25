//! Rocky's TCP and WebSocket transport: plain TCP for networks that already route and encrypt
//! between peers, and WebSocket for servers that browsers and proxies reach.
#![deny(missing_docs)]

use rocky_core::{
    Scope,
    net::{
        BoxFuture, Connected, Connector, Failure, Incoming, Protocol, ScopeRefusal, Stream,
        Transport, framing,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, Semaphore, mpsc, watch},
};

/// How long a dial or a connection's opening handshake may take before it fails.
pub const TIMEOUT: Duration = Duration::from_secs(15);
/// Incoming connections a database scope queues before new ones are refused as busy.
pub const PENDING: usize = 32;
/// Requests one attachment serves at once; more are refused as busy.
pub const RPC_REQUESTS: usize = 32;
const VERSION: u32 = 1;
const MAX_HEADER: usize = 512;
#[cfg(not(target_family = "wasm"))]
const TCP: &str = "tcp";
const WS: &str = "ws";
const WSS: &str = "wss";

#[cfg(all(not(target_family = "wasm"), feature = "websocket"))]
const SCHEMES: &[&str] = &[TCP, WS, WSS];
#[cfg(all(not(target_family = "wasm"), not(feature = "websocket")))]
const SCHEMES: &[&str] = &[TCP];
#[cfg(target_family = "wasm")]
const SCHEMES: &[&str] = &[WS, WSS];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Wire {
    Rpc,
    Link,
    Coordinate,
}
impl From<Protocol> for Wire {
    fn from(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Rpc => Self::Rpc,
            Protocol::Link => Self::Link,
            Protocol::Coordinate => Self::Coordinate,
        }
    }
}
impl From<Wire> for Protocol {
    fn from(wire: Wire) -> Self {
        match wire {
            Wire::Rpc => Self::Rpc,
            Wire::Link => Self::Link,
            Wire::Coordinate => Self::Coordinate,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    version: u32,
    protocol: Wire,
    scope: Option<Scope>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    version: u32,
    result: Result<(), ScopeRefusal>,
}

struct Inbox {
    scope: std::sync::Mutex<Option<Scope>>,
    #[cfg_attr(
        target_family = "wasm",
        expect(dead_code, reason = "a browser attachment admits no connections")
    )]
    sender: mpsc::Sender<Incoming>,
    incoming: Mutex<mpsc::Receiver<Incoming>>,
    listening: AtomicBool,
    closed: watch::Sender<bool>,
}
impl Inbox {
    fn new() -> Self {
        let (sender, incoming) = mpsc::channel(PENDING);
        Self {
            scope: Default::default(),
            sender,
            incoming: Mutex::new(incoming),
            listening: AtomicBool::new(false),
            closed: watch::channel(false).0,
        }
    }
}

struct Shared {
    local: Option<SocketAddr>,
    advertise: Option<String>,
    owned: bool,
    dialer: platform::Dialer,
    inboxes: std::sync::Mutex<HashMap<Option<Scope>, Weak<Inbox>>>,
    #[cfg_attr(
        target_family = "wasm",
        expect(dead_code, reason = "a browser attachment admits no connections")
    )]
    rpc: Arc<Semaphore>,
    refused: AtomicU64,
    closed: watch::Sender<bool>,
}

/// Where a TCP attachment listens, the route it tells peers to dial and whom it trusts when it
/// dials; pass it to `Network::from` or call `start`.
#[derive(Clone, Debug)]
pub struct Options {
    #[cfg(not(target_family = "wasm"))]
    listener: Option<platform::Listener>,
    #[cfg(not(target_family = "wasm"))]
    roots: Vec<Vec<u8>>,
}
impl Options {
    /// Binds and returns an attachment the application keeps, for `Network::Borrowed`; closing a
    /// database never closes it.
    pub async fn start(&self) -> Result<Attachment, Failure> {
        self.serve(false).await
    }
}
impl Connector for Options {
    fn start(
        &self,
        _accepts: bool,
        _secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Connected, Failure>> {
        Box::pin(async move { Ok(self.serve(true).await?.connected()) })
    }
}

/// A running TCP endpoint that one or more databases share; clones share it, and `close` stops it.
#[derive(Clone)]
pub struct Attachment(Arc<Shared>);
impl Attachment {
    fn build(
        local: Option<SocketAddr>,
        advertise: Option<String>,
        owned: bool,
        dialer: platform::Dialer,
    ) -> Self {
        Self(Arc::new(Shared {
            local,
            advertise,
            owned,
            dialer,
            inboxes: Default::default(),
            rpc: Arc::new(Semaphore::new(RPC_REQUESTS)),
            refused: AtomicU64::new(0),
            closed: watch::channel(false).0,
        }))
    }
    /// The bound listener address, or `None` for a dial-only or external attachment.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.0.local
    }
    /// Connections refused so far because no database listened on the scope or the queue was full.
    pub fn refused(&self) -> u64 {
        self.0.refused.load(Ordering::Relaxed)
    }
    /// A transport for one database, passed as `Network::Borrowed`; each database takes its own.
    pub fn transport(&self) -> Arc<dyn Transport> {
        Arc::new(Port {
            attachment: self.clone(),
            inbox: Arc::new(Inbox::new()),
        })
    }
    /// This attachment as a started connector, for `Network::Attached`.
    pub fn connected(&self) -> Connected {
        Connected::new(self.transport())
    }
    /// Stops listening and closes every database's route on this attachment.
    pub fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.0.closed.send_replace(true);
            let inboxes: Vec<_> = self
                .0
                .inboxes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .drain()
                .filter_map(|(_, inbox)| inbox.upgrade())
                .collect();
            for inbox in inboxes {
                inbox.closed.send_replace(true);
                inbox.incoming.lock().await.close();
            }
        })
    }
    async fn dial(
        &self,
        address: &str,
        protocol: Protocol,
        scope: Option<Scope>,
    ) -> Result<Stream, Failure> {
        if *self.0.closed.borrow() {
            return Err("the TCP attachment is closed".into());
        }
        let scope = match protocol {
            Protocol::Coordinate => None,
            Protocol::Rpc | Protocol::Link => Some(scope.ok_or(ScopeRefusal::UnknownScope)?),
        };
        let mut stream =
            rocky_platform::time::timeout(TIMEOUT, self.0.dialer.open(address)).await??;
        rocky_platform::time::timeout(TIMEOUT, async {
            framing::write(
                &mut stream,
                &rocky_credentials::encode(&Hello {
                    version: VERSION,
                    protocol: protocol.into(),
                    scope,
                })?,
            )
            .await?;
            let reply: Accepted = rocky_credentials::decode_bounded(
                &framing::read_bounded(&mut stream, MAX_HEADER).await?,
                MAX_HEADER,
            )?;
            if reply.version != VERSION {
                return Err::<(), Failure>(ScopeRefusal::UnsupportedVersion.into());
            }
            reply.result.map_err(Into::into)
        })
        .await??;
        Ok(Stream::new(stream))
    }
}

fn unsupported(address: &str) -> Failure {
    let schemes = SCHEMES
        .iter()
        .map(|scheme| format!("{scheme}://"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("rocky-tcp here dials only {schemes} routes, not {address}").into()
}

struct Port {
    attachment: Attachment,
    inbox: Arc<Inbox>,
}
impl Transport for Port {
    fn schemes(&self) -> &[&str] {
        SCHEMES
    }
    fn dial_only(&self) -> bool {
        self.attachment.0.advertise.is_none()
    }
    fn address(&self) -> BoxFuture<'_, Result<Option<String>, Failure>> {
        Box::pin(async { Ok(self.attachment.0.advertise.clone()) })
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
        scope: Option<Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(self.attachment.dial(address, protocol, scope))
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
                        .attachment
                        .dial(address, Protocol::Rpc, Some(scope))
                        .await?;
                    framing::write(&mut stream, request).await?;
                    let reply = framing::read(&mut stream).await?;
                    let _ = stream.shutdown().await;
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
        Box::pin(async move {
            let mut failure: Failure = "no route".into();
            for address in addresses {
                match rocky_platform::time::timeout(
                    3 * TIMEOUT,
                    self.attachment.dial(address, Protocol::Link, Some(scope)),
                )
                .await
                {
                    Ok(Ok(stream)) => return Ok(stream),
                    Ok(Err(error)) => failure = error,
                    Err(error) => failure = error.into(),
                }
            }
            Err(failure)
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<Incoming>> {
        Box::pin(async {
            let mut closed = self.inbox.closed.subscribe();
            let mut incoming = tokio::select! {
                biased;
                _ = closed.wait_for(|closed| *closed) => return None,
                incoming = self.inbox.incoming.lock() => incoming,
            };
            tokio::select! {
                biased;
                _ = closed.wait_for(|closed| *closed) => None,
                accepted = incoming.recv() => accepted,
            }
        })
    }
    fn listen(&self, scope: Option<Scope>, listening: bool) -> Result<(), Failure> {
        let shared = &self.attachment.0;
        let mut inboxes = shared
            .inboxes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut bound = self
            .inbox
            .scope
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        inboxes.retain(|_, inbox| inbox.strong_count() > 0);
        if listening {
            if self.dial_only() {
                return Err("this TCP attachment only dials and accepts no connections".into());
            }
            if *self.inbox.closed.borrow() || *shared.closed.borrow() {
                return Err(ScopeRefusal::NotListening.into());
            }
            if self.inbox.listening.load(Ordering::SeqCst) && *bound != scope {
                return Err(ScopeRefusal::AlreadyAttached.into());
            }
            if inboxes
                .get(&scope)
                .and_then(Weak::upgrade)
                .is_some_and(|inbox| !Arc::ptr_eq(&inbox, &self.inbox))
            {
                return Err(ScopeRefusal::AlreadyAttached.into());
            }
            *bound = scope;
            inboxes.insert(scope, Arc::downgrade(&self.inbox));
        } else if inboxes
            .get(&scope)
            .and_then(Weak::upgrade)
            .is_some_and(|inbox| Arc::ptr_eq(&inbox, &self.inbox))
        {
            inboxes.remove(&scope);
        }
        self.inbox.listening.store(listening, Ordering::SeqCst);
        Ok(())
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let scope = *self
                .inbox
                .scope
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let _ = self.listen(scope, false);
            self.inbox.closed.send_replace(true);
            self.inbox.incoming.lock().await.close();
            if self.attachment.0.owned {
                self.attachment.close().await;
            }
        })
    }
}

#[cfg_attr(not(target_family = "wasm"), path = "native.rs")]
#[cfg_attr(target_family = "wasm", path = "browser.rs")]
mod platform;

#[cfg(all(not(target_family = "wasm"), feature = "websocket"))]
mod websocket;
#[cfg(all(not(target_family = "wasm"), feature = "websocket"))]
pub use websocket::{
    Io as WebSocketIo, accept_key as websocket_accept_key, stream as websocket_stream,
};

#[cfg(all(test, not(target_family = "wasm")))]
mod tests;
