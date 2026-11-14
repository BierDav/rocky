//! The network port: reliable ordered byte streams between two nodes, each
//! carrying one protocol. A transport adds no security the
//! protocols rely on, and an address is never a grant.
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub type Failure = Box<dyn std::error::Error + Send + Sync>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A fully framed RPC or a live protocol stream, admitted by the harness.
pub enum Incoming {
    Request(Request),
    Stream(Protocol, Stream),
}
impl Incoming {
    pub fn protocol(&self) -> Protocol {
        match self {
            Self::Request(_) => Protocol::Rpc,
            Self::Stream(protocol, _) => *protocol,
        }
    }
}
pub struct Request {
    pub bytes: Vec<u8>,
    response: tokio::sync::oneshot::Sender<Vec<u8>>,
    delivered: tokio::sync::oneshot::Receiver<()>,
}
impl Request {
    /// The harness receives the response and acknowledges its delivery.
    pub fn channel(
        bytes: Vec<u8>,
    ) -> (
        Self,
        tokio::sync::oneshot::Receiver<Vec<u8>>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (response, answer) = tokio::sync::oneshot::channel();
        let (delivered, received) = tokio::sync::oneshot::channel();
        (
            Self {
                bytes,
                response,
                delivered: received,
            },
            answer,
            delivered,
        )
    }
    /// Completes once the harness delivered the response.
    pub async fn respond(self, bytes: Vec<u8>) -> Result<(), Failure> {
        self.response.send(bytes).map_err(|_| "RPC caller closed")?;
        self.delivered
            .await
            .map_err(|_| "RPC response was not delivered".into())
    }
}

/// A reliable ordered byte stream a protocol session runs over.
pub trait SessionStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> SessionStream for T {}

/// A protocol stream and its additional ordered lanes.
pub struct Stream {
    io: Box<dyn SessionStream>,
    lanes: Vec<Box<dyn SessionStream>>,
}
impl Stream {
    pub fn new(io: impl SessionStream + 'static) -> Self {
        Self {
            io: Box::new(io),
            lanes: Vec::new(),
        }
    }
    /// Additional ordered lanes bound to this stream's peer and connection.
    pub fn with_lanes(mut self, lanes: Vec<Box<dyn SessionStream>>) -> Self {
        self.lanes = lanes;
        self
    }
    pub fn take_lanes(&mut self) -> Vec<Box<dyn SessionStream>> {
        std::mem::take(&mut self.lanes)
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for Stream {
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

/// The protocols a stream can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// One typed peer request and reply.
    Rpc,
    /// An authenticated, symmetric live link.
    Link,
    /// A signed request to a metadata coordinator and its voter's answer.
    /// Coordinator voters serve it; a database does not.
    Coordinate,
}
impl Protocol {
    /// The protocols a database serves.
    pub const ALL: [Self; 2] = [Self::Rpc, Self::Link];
}

/// Connects nodes. An address is a reachability hint whose format belongs
/// to the transport kind; who answers is authenticated above the stream.
pub trait Transport: Send + Sync {
    /// Names the address format, such as `iroh`.
    fn kind(&self) -> &str;
    /// Where peers reach this node. Fails for a node that does not accept.
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>>;
    /// Low-level dial for harnesses and coordinator streams. Database protocols
    /// carry `Some(scope)`; coordinator streams carry `None`.
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
        scope: Option<crate::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>>;
    /// The next inbound stream, or `None` once closed or not accepting.
    fn accept(&self) -> BoxFuture<'_, Option<Incoming>>;
    /// Whether a service takes inbound streams from `accept`. While none
    /// does, a transport may refuse them at once instead of queueing.
    fn listen(&self, _scope: Option<crate::Scope>, _listening: bool) -> Result<(), Failure> {
        Ok(())
    }
    /// One scoped request/reply. The harness bounds dialing and I/O per route
    /// and tries the next route after failure. It owns framing and scope admission.
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: crate::Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>>;
    /// Opens a scoped database session, with a bounded deadline per route.
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: crate::Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>>;
    /// Releases what the transport itself started. Its owner calls this.
    fn close(&self) -> BoxFuture<'_, ()>;
}

/// The discovery path of the local network, as `Discovery::paths` names it.
pub const LOCAL_PATH: &str = "local";
/// The discovery path of the wider network: the topic's rendezvous record.
pub const TOPIC_PATH: &str = "topic";

/// A contact a transport found: a hint, never a grant. The session
/// authenticates whoever answers at `address`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The node key the announcer claimed.
    pub node: [u8; 32],
    /// Where the transport reaches it, in the transport kind's format.
    pub address: String,
    /// The path that found it, as `Discovery::paths` names it.
    pub path: String,
    /// Found on the local network; ranked first by partner choice.
    pub local: bool,
}

/// Whether one discovery path runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathState {
    On,
    /// Not asked for, or stopped while the application is in the background.
    Off,
    /// Asked for but impossible here, with the reason.
    Unavailable(String),
}

/// An optional port for finding peers under a topic only they derive.
pub trait Discovery: Send + Sync {
    /// Announces `node` under `topics`, the current one first, and looks
    /// for others announcing under them. Replaces the earlier topics; an
    /// empty list withdraws.
    fn announce<'a>(
        &'a self,
        node: [u8; 32],
        topics: &'a [[u8; 32]],
    ) -> BoxFuture<'a, Result<(), Failure>>;
    /// The next contact found, or `None` once the transport closed.
    fn found(&self) -> BoxFuture<'_, Option<Found>>;
    /// Every path this transport has, by name, and whether it runs.
    fn paths(&self) -> Vec<(String, PathState)>;
    /// Paths that may run only while the application is in the foreground
    /// stop with `false` and resume with `true`.
    fn foreground(&self, _foreground: bool) {}
}

/// Starts a transport that its caller then owns and closes.
pub trait Connector: Send + Sync {
    /// A node that `accepts` enrolls devices and must be reachable. Other
    /// nodes accept where the transport can without further configuration.
    /// `secret` is the node's stable transport key where the transport has
    /// one; it identifies a route, never a device's authority.
    fn start(&self, accepts: bool, secret: [u8; 32]) -> BoxFuture<'_, Result<Connected, Failure>>;
}

/// Independently supplied network services. Discovery and change notifications
/// are optional ports, outside the byte transport contract.
#[derive(Clone)]
pub struct Connected {
    pub transport: Arc<dyn Transport>,
    pub discovery: Option<Arc<dyn Discovery>>,
    pub changes: Option<tokio::sync::watch::Receiver<()>>,
}
impl Connected {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            discovery: None,
            changes: None,
        }
    }
}
impl std::ops::Deref for Connected {
    type Target = dyn Transport;
    fn deref(&self) -> &Self::Target {
        &*self.transport
    }
}
impl From<Arc<dyn Transport>> for Connected {
    fn from(transport: Arc<dyn Transport>) -> Self {
        Self::new(transport)
    }
}
impl<T: Transport + 'static> From<Arc<T>> for Connected {
    fn from(transport: Arc<T>) -> Self {
        Self::new(transport)
    }
}

/// A scoped stream refused before any database work was admitted.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ScopeRefusal {
    UnknownScope,
    NotListening,
    Busy,
    UnsupportedVersion,
    AlreadyAttached,
}
impl std::fmt::Display for ScopeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownScope => "unknown database scope",
            Self::NotListening => "database is not listening",
            Self::Busy => "database admission budget exhausted",
            Self::UnsupportedVersion => "unsupported stream scope format",
            Self::AlreadyAttached => "database scope is already attached",
        })
    }
}
impl std::error::Error for ScopeRefusal {}

pub mod framing;
