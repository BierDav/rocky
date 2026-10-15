//! The network port: reliable ordered byte streams between two nodes, each
//! carrying one named, versioned protocol. A transport adds no security the
//! protocols rely on, and an address is never a grant. See
//! `docs/runtime/sdk-transports.md`.
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub type Failure = Box<dyn std::error::Error + Send + Sync>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A reliable ordered byte stream a protocol session runs over.
pub trait SessionStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> SessionStream for T {}

/// One network path to the peer of a stream, as its transport measures it
/// when asked. Counters cover the path's lifetime so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPath {
    /// Whether the transport sends application data on this path now.
    pub selected: bool,
    /// `direct` for an IP path, `relay` through a relay server.
    pub kind: &'static str,
    /// The peer's address on this path: a socket address or relay URL.
    pub remote: String,
    /// This node's address on it, where the transport knows one.
    pub local: Option<String>,
    pub rtt: Duration,
    /// The largest payload the path currently carries, in bytes.
    pub mtu: u16,
    /// The congestion window, in bytes.
    pub window: u64,
    pub sent_bytes: u64,
    pub sent_datagrams: u64,
    pub received_bytes: u64,
    pub received_datagrams: u64,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub congestion_events: u64,
}

/// A transport's live view of the paths under a stream's connection.
pub trait PathSource: Send + Sync {
    /// The open paths, the selected one first. Empty once the connection closed.
    fn paths(&self) -> Vec<NetworkPath>;
}

/// A stream and, where its transport measures them, its network paths.
pub struct Stream {
    io: Box<dyn SessionStream>,
    paths: Option<Arc<dyn PathSource>>,
}
impl Stream {
    pub fn new(io: impl SessionStream + 'static) -> Self {
        Self {
            io: Box::new(io),
            paths: None,
        }
    }
    pub fn with_paths(io: impl SessionStream + 'static, paths: Arc<dyn PathSource>) -> Self {
        Self {
            io: Box::new(io),
            paths: Some(paths),
        }
    }
    /// The stream's paths, which outlive the stream for status reads.
    pub fn paths(&self) -> Option<Arc<dyn PathSource>> {
        self.paths.clone()
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
    /// Enrollment of a device with an invitation.
    Pair,
    /// An authenticated, symmetric live link.
    Link,
    /// Renewal of an enrolled device's credentials.
    Renew,
    /// A signed request to a metadata coordinator and its voter's answer.
    /// Coordinator voters serve it; a database does not.
    Coordinate,
}
impl Protocol {
    /// The protocols a database serves.
    pub const ALL: [Self; 3] = [Self::Pair, Self::Link, Self::Renew];
    /// The protocol's versioned name, used as the QUIC ALPN. A changed wire
    /// format takes a new name.
    pub fn name(self) -> &'static [u8] {
        match self {
            Self::Pair => b"rocky/sdk-pair/1",
            Self::Link => b"rocky/link/4",
            Self::Renew => b"rocky/sdk-renew/2",
            Self::Coordinate => b"rocky/coordinator/3",
        }
    }
    pub fn named(name: &[u8]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .chain([Self::Coordinate])
            .find(|protocol| protocol.name() == name)
    }
}

/// Connects nodes. An address is a reachability hint whose format belongs
/// to the transport kind; who answers is authenticated above the stream.
pub trait Transport: Send + Sync {
    /// Names the address format, such as `iroh`.
    fn kind(&self) -> &str;
    /// Where peers reach this node. Fails for a node that does not accept.
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>>;
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
    ) -> BoxFuture<'a, Result<Stream, Failure>>;
    /// The next inbound stream, or `None` once closed or not accepting.
    fn accept(&self) -> BoxFuture<'_, Option<(Protocol, Stream)>>;
    /// Whether a service takes inbound streams from `accept`. While none
    /// does, a transport may refuse them at once instead of queueing.
    fn listen(&self, _listening: bool) {}
    /// Optional reachability notifications. Adapters without a signal use the
    /// SDK connection call when the application learns of a network change.
    fn connectivity(&self) -> Option<tokio::sync::watch::Receiver<()>> {
        None
    }
    /// The attachment's initial relay configuration, if it owns that configuration.
    fn initial_relay_configuration(&self) -> Option<String> {
        None
    }
    /// Actual home-relay observations; None if the adapter cannot expose them.
    fn relays(&self) -> Option<Vec<RelayStatus>> {
        None
    }
    /// Finds peers whose address this node does not know. Transports
    /// without it work from contacts.
    fn discovery(&self) -> Option<Arc<dyn Discovery>> {
        None
    }
    /// Drops what the transport keeps for reaching `address`, such as a
    /// cached connection, after an exchange over it failed.
    fn forget(&self, _address: &str) {}
    /// Qualification-only simulation of sockets reclaimed during suspension.
    #[cfg(feature = "qualification")]
    fn fault_close_sockets(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async { Err("socket fault injection is unsupported by this transport".into()) })
    }
    /// Refreshes sockets after suspension. Borrowed endpoints must report a
    /// dead endpoint instead of closing or replacing application resources.
    fn foreground(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async { Ok(()) })
    }
    /// Releases what the transport itself started. Its owner calls this.
    fn close(&self) -> BoxFuture<'_, ()>;
}

/// One home relay's actual connection observation, supplied by the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayStatus {
    pub url: String,
    pub connected: bool,
    pub last_error: Option<String>,
    pub auth_denied_reason: Option<String>,
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

/// A transport's optional way to find admitted peers under a topic that
/// only they derive. See `docs/decisions/0057-discovery.md`.
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
    fn start(
        &self,
        accepts: bool,
        secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Arc<dyn Transport>, Failure>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_current_protocols_are_named() {
        assert_eq!(Protocol::named(b"rocky/command/2"), None);
        assert_eq!(Protocol::named(b"rocky/link/1"), None);
        assert_eq!(Protocol::named(b"rocky/link/4"), Some(Protocol::Link));
    }
}
