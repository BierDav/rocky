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
        Bytes, Error, Message, client::IntoClientRequest, handshake::derive_accept_key,
        protocol::Role,
    },
};

const CHUNK: usize = 64 * 1024;

pub fn accept_key(key: &[u8]) -> String {
    derive_accept_key(key)
}

pub(crate) fn is_url(address: &str) -> bool {
    address.starts_with("ws://") || address.starts_with("wss://")
}

pub(crate) async fn accept(stream: TcpStream) -> Result<Io<TcpStream>, Failure> {
    Ok(Io::new(tokio_tungstenite::accept_async(stream).await?))
}

pub async fn stream<S>(io: S) -> Io<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    Io::new(WebSocketStream::from_raw_socket(io, Role::Server, None).await)
}

pub(crate) async fn dial(address: &str) -> Result<Box<dyn SessionStream>, Failure> {
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
        let (socket, _) = tokio_tungstenite::client_async(request, tcp).await?;
        return Ok(Box::new(Io::new(socket)));
    }
    let name = rustls::pki_types::ServerName::try_from(host)?;
    let tls = tokio_rustls::TlsConnector::from(client_config()?)
        .connect(name, tcp)
        .await?;
    let (socket, _) = tokio_tungstenite::client_async(request, tls).await?;
    Ok(Box::new(Io::new(socket)))
}

fn client_config() -> Result<Arc<rustls::ClientConfig>, Failure> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(CONFIG.get_or_init(|| Arc::new(config)).clone())
}

pub struct Io<S> {
    socket: WebSocketStream<S>,
    read: Bytes,
    write: Vec<u8>,
    ended: bool,
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
                return Poll::Ready(Ok(()));
            }
            match ready!(Pin::new(&mut this.socket).poll_next(cx)) {
                Some(Ok(Message::Binary(bytes))) => this.read = bytes,
                Some(Ok(Message::Close(_))) | None => this.ended = true,
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
