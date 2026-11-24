use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub type Failure = Box<dyn std::error::Error + Send + Sync>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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
    pub async fn respond(self, bytes: Vec<u8>) -> Result<(), Failure> {
        self.response.send(bytes).map_err(|_| "RPC caller closed")?;
        self.delivered
            .await
            .map_err(|_| "RPC response was not delivered".into())
    }
}

pub trait SessionStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> SessionStream for T {}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    Rpc,
    Link,
    Coordinate,
}
impl Protocol {
    pub const ALL: [Self; 2] = [Self::Rpc, Self::Link];
}

pub trait Transport: Send + Sync {
    fn kind(&self) -> &str;
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>>;
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
        scope: Option<crate::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>>;
    fn accept(&self) -> BoxFuture<'_, Option<Incoming>>;
    fn listen(&self, _scope: Option<crate::Scope>, _listening: bool) -> Result<(), Failure> {
        Ok(())
    }
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: crate::Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>>;
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: crate::Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>>;
    fn close(&self) -> BoxFuture<'_, ()>;
}

pub const LOCAL_PATH: &str = "local";
pub const TOPIC_PATH: &str = "topic";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub claimed_node: [u8; 32],
    pub address: String,
    pub path: String,
    pub local: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathState {
    On,
    Off,
    Unavailable(String),
}

pub trait Discovery: Send + Sync {
    fn announce<'a>(
        &'a self,
        node: [u8; 32],
        topics: &'a [[u8; 32]],
    ) -> BoxFuture<'a, Result<(), Failure>>;
    fn found(&self) -> BoxFuture<'_, Option<Found>>;
    fn paths(&self) -> Vec<(String, PathState)>;
}

pub trait Connector: Send + Sync {
    fn start(
        &self,
        accepts: bool,
        route_secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Connected, Failure>>;
}

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
