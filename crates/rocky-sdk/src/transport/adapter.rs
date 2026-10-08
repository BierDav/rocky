//! The language-neutral transport contract: a transport and its streams as
//! calls that take and return strings and byte chunks, so a host language
//! supplies them through a callback interface and needs no Rust trait or
//! native build. `adapt` turns one into the network port.
use super::{BoxFuture, Failure, Protocol, Stream, Transport};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf},
    sync::oneshot,
};

/// The largest chunk asked of `read` or given to `write`.
pub const CHUNK: usize = 16 * 1024;
/// Bytes buffered per stream and direction before the sender waits.
pub const BUFFER: usize = 64 * 1024;

/// One reliable ordered byte stream, as chunks.
pub trait ChunkStream: Send + Sync {
    /// The next bytes, at most `max`. Empty once the peer finished.
    fn read(&self, max: u32) -> BoxFuture<'_, Result<Vec<u8>, Failure>>;
    /// Delivers `bytes` in order; returns once they are accepted.
    fn write(&self, bytes: Vec<u8>) -> BoxFuture<'_, Result<(), Failure>>;
    /// No more writes follow: the peer's `read` ends after what was sent.
    fn finish(&self) -> BoxFuture<'_, Result<(), Failure>>;
    /// Abandons the stream; a pending `read` ends. Called exactly once.
    fn close(&self) -> BoxFuture<'_, ()>;
}
/// An inbound stream and the protocol name its dialer gave.
pub type Inbound = (String, Arc<dyn ChunkStream>);
/// A transport as a host language defines it. Its owner starts and closes
/// it; Rocky only calls these.
pub trait ChunkTransport: Send + Sync {
    /// Names the address format; routes of another kind are not dialed.
    fn kind(&self) -> String;
    /// Where peers reach this node.
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>>;
    /// A stream to `address` carrying the protocol named `protocol`.
    fn connect(
        &self,
        address: String,
        protocol: String,
    ) -> BoxFuture<'_, Result<Arc<dyn ChunkStream>, Failure>>;
    /// The next inbound stream; `None` once closed.
    fn accept(&self) -> BoxFuture<'_, Result<Option<Inbound>, Failure>>;
}

/// The network port over `transport`. Its `close` does nothing: the
/// application owns the transport.
pub fn adapt(transport: Arc<dyn ChunkTransport>) -> Arc<dyn Transport> {
    Arc::new(Adapted {
        kind: transport.kind(),
        transport,
    })
}
struct Adapted {
    kind: String,
    transport: Arc<dyn ChunkTransport>,
}
impl Transport for Adapted {
    fn kind(&self) -> &str {
        &self.kind
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        self.transport.address()
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            let name = String::from_utf8(protocol.name().to_vec())?;
            Ok(bridge(self.transport.connect(address.into(), name).await?))
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<(Protocol, Stream)>> {
        Box::pin(async {
            loop {
                let (name, stream) = self.transport.accept().await.ok()??;
                match Protocol::named(name.as_bytes()).filter(|p| Protocol::ALL.contains(p)) {
                    Some(protocol) => return Some((protocol, bridge(stream))),
                    None => stream.close().await,
                }
            }
        })
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// Rocky's end of a bridged stream; dropping it abandons the remote one.
struct Local {
    io: DuplexStream,
    _dropped: oneshot::Sender<()>,
}
impl AsyncRead for Local {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for Local {
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

/// Pumps chunks between `remote` and a bounded pipe. What Rocky wrote is
/// delivered before the stream is finished, also when Rocky drops its end.
fn bridge(remote: Arc<dyn ChunkStream>) -> Stream {
    let (local, far) = tokio::io::duplex(BUFFER);
    let (dropped, mut gone) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(far);
        let inbound = async {
            while let Ok(chunk) = remote.read(CHUNK as u32).await {
                if chunk.is_empty() || writer.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            let _ = writer.shutdown().await;
        };
        let outbound = async {
            let mut buffer = vec![0; CHUNK];
            while let Ok(read) = reader.read(&mut buffer).await {
                if read == 0 || remote.write(buffer[..read].to_vec()).await.is_err() {
                    break;
                }
            }
            let _ = remote.finish().await;
        };
        tokio::pin!(inbound, outbound);
        let mut ended = false;
        loop {
            tokio::select! {
                () = &mut outbound => break,
                () = &mut inbound, if !ended => ended = true,
            }
        }
        if !ended {
            tokio::select! {
                () = &mut inbound => {}
                _ = &mut gone => {}
            }
        }
        remote.close().await;
    });
    Box::new(Local {
        io: local,
        _dropped: dropped,
    })
}
