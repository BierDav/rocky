//! The network port: reliable ordered byte streams between two nodes, each
//! carrying one named, versioned protocol. A transport adds no security the
//! protocols rely on, and an address is never a grant. See
//! `docs/runtime/sdk-transports.md`.
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};

pub type Failure = Box<dyn std::error::Error + Send + Sync>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A reliable ordered byte stream a protocol session runs over.
pub trait SessionStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> SessionStream for T {}
pub type Stream = Box<dyn SessionStream>;

/// The protocols a stream can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// Enrollment of a device with an invitation.
    Pair,
    /// An authenticated native session of the secure runtime.
    Sync,
    /// Renewal of an enrolled device's credentials.
    Renew,
    /// One signed command invocation and its executor's answer.
    Command,
    /// A signed request to a metadata coordinator and its voter's answer.
    /// Coordinator voters serve it; a database does not.
    Coordinate,
}
impl Protocol {
    /// The protocols a database serves.
    pub const ALL: [Self; 4] = [Self::Pair, Self::Sync, Self::Renew, Self::Command];
    /// The protocol's versioned name: an ALPN for QUIC, a preamble or a
    /// label elsewhere. A changed wire format takes a new name.
    pub fn name(self) -> &'static [u8] {
        match self {
            Self::Pair => b"rocky/sdk-pair/1",
            Self::Sync => b"rocky/native-session/5",
            Self::Renew => b"rocky/sdk-renew/1",
            Self::Command => b"rocky/command/1",
            Self::Coordinate => b"rocky/coordinator/1",
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
    /// Names the address format, such as `iroh` or `tcp`.
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
    /// Drops what the transport keeps for reaching `address`, such as a
    /// cached connection, after an exchange over it failed.
    fn forget(&self, _address: &str) {}
    /// Releases what the transport itself started. Its owner calls this.
    fn close(&self) -> BoxFuture<'_, ()>;
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
