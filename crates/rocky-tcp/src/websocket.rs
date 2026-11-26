use futures_util::{Sink, Stream as _};
use rocky_core::net::{Failure, SessionStream};
use std::{
    io,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll, ready},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Bytes, Error, Message,
        client::IntoClientRequest,
        handshake::derive_accept_key,
        protocol::{Role, WebSocketConfig},
    },
};

const CHUNK: usize = 64 * 1024;

fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(CHUNK))
        .max_frame_size(Some(CHUNK))
}

/// The `Sec-WebSocket-Accept` value for a client's `Sec-WebSocket-Key`, for an HTTP server that
/// upgrades connections itself.
pub fn accept_key(key: &[u8]) -> String {
    derive_accept_key(key)
}

pub(crate) async fn accept(stream: TcpStream) -> Result<Io<TcpStream>, Failure> {
    Ok(Io::new(
        tokio_tungstenite::accept_async_with_config(stream, Some(config())).await?,
    ))
}

pub(crate) async fn stream<S>(io: S) -> Io<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    Io::new(WebSocketStream::from_raw_socket(io, Role::Server, Some(config())).await)
}

pub(crate) async fn dial(address: &str, tls: &Tls) -> Result<Box<dyn SessionStream>, Failure> {
    let request = address.into_client_request()?;
    let uri = request.uri();
    let secure = match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") => false,
        _ => return Err("not a WebSocket URL".into()),
    };
    let host = uri
        .host()
        .ok_or("a WebSocket URL without a host")?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let tcp = TcpStream::connect((host.as_str(), port)).await?;
    tcp.set_nodelay(true)?;
    if !secure {
        let (socket, _) =
            tokio_tungstenite::client_async_with_config(request, tcp, Some(config())).await?;
        return Ok(Box::new(Io::new(socket)));
    }
    let name = rustls::pki_types::ServerName::try_from(host)?;
    let tls = tokio_rustls::TlsConnector::from(tls.config()?)
        .connect(name, tcp)
        .await?;
    let (socket, _) =
        tokio_tungstenite::client_async_with_config(request, tls, Some(config())).await?;
    Ok(Box::new(Io::new(socket)))
}

#[derive(Default)]
pub(crate) struct Tls(Option<Arc<rustls::ClientConfig>>);
impl Tls {
    pub(crate) fn new(roots: &[Vec<u8>]) -> Result<Self, Failure> {
        if roots.is_empty() {
            return Ok(Self(None));
        }
        let mut store = public_roots();
        for root in roots {
            store.add(rustls::pki_types::CertificateDer::from(root.as_slice()))?;
        }
        Ok(Self(Some(client_config(store)?)))
    }
    fn config(&self) -> Result<Arc<rustls::ClientConfig>, Failure> {
        if let Some(config) = &self.0 {
            return Ok(config.clone());
        }
        static PUBLIC: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
        if let Some(config) = PUBLIC.get() {
            return Ok(config.clone());
        }
        let config = client_config(public_roots())?;
        Ok(PUBLIC.get_or_init(|| config).clone())
    }
}

fn public_roots() -> rustls::RootCertStore {
    rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
}

fn client_config(roots: rustls::RootCertStore) -> Result<Arc<rustls::ClientConfig>, Failure> {
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

pub(crate) struct Io<S> {
    socket: WebSocketStream<S>,
    read: Bytes,
    write: Vec<u8>,
    ended: bool,
    replying: bool,
}
impl<S> Io<S> {
    #[cfg(test)]
    pub(crate) fn new_for_tests(socket: WebSocketStream<S>) -> Self {
        Self::new(socket)
    }
    fn new(socket: WebSocketStream<S>) -> Self {
        Self {
            socket,
            read: Bytes::new(),
            write: Vec::new(),
            ended: false,
            replying: false,
        }
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> Io<S> {
    fn poll_send(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.write.is_empty() {
            return Poll::Ready(Ok(()));
        }
        ready!(Pin::new(&mut self.socket).poll_ready(cx)).map_err(io::Error::other)?;
        let message = Message::Binary(std::mem::take(&mut self.write).into());
        Pin::new(&mut self.socket)
            .start_send(message)
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(()))
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for Io<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if !this.read.is_empty() {
                let taken = this.read.split_to(buf.remaining().min(this.read.len()));
                buf.put_slice(&taken);
                return Poll::Ready(Ok(()));
            }
            if this.ended {
                if this.replying {
                    let _ = ready!(Pin::new(&mut this.socket).poll_flush(cx));
                    this.replying = false;
                }
                return Poll::Ready(Ok(()));
            }
            match ready!(Pin::new(&mut this.socket).poll_next(cx)) {
                Some(Ok(Message::Binary(bytes))) => this.read = bytes,
                Some(Ok(Message::Close(_))) => {
                    this.ended = true;
                    this.replying = true;
                }
                None => this.ended = true,
                Some(Ok(_)) => {}
                Some(Err(Error::ConnectionClosed | Error::AlreadyClosed)) => this.ended = true,
                Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),
            }
        }
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Io<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.write.len() >= CHUNK {
            ready!(this.poll_send(cx))?;
        }
        let taken = buf.len().min(CHUNK - this.write.len());
        this.write.extend_from_slice(&buf[..taken]);
        Poll::Ready(Ok(taken))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_send(cx))?;
        Pin::new(&mut this.socket)
            .poll_flush(cx)
            .map_err(io::Error::other)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_send(cx))?;
        match ready!(Pin::new(&mut this.socket).poll_close(cx)) {
            Ok(()) | Err(Error::ConnectionClosed | Error::AlreadyClosed) => Poll::Ready(Ok(())),
            Err(error) => Poll::Ready(Err(io::Error::other(error))),
        }
    }
}
