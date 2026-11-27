use super::*;
use futures_util::{Sink, Stream as _};
use rocky_core::net::{SessionStream, scheme};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll, ready},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use ws_stream_wasm::{WsMessage, WsMeta, WsStream};

const CHUNK: usize = 64 * 1024;

impl Options {
    pub(super) async fn serve(&self, owned: bool) -> Result<Attachment, Failure> {
        Ok(Attachment::build(None, None, owned, Dialer))
    }
}

pub(super) struct Dialer;
impl Dialer {
    pub(super) async fn open(&self, address: &str) -> Result<Box<dyn SessionStream>, Failure> {
        if !matches!(scheme(address), Some(WS | WSS)) {
            return Err(unsupported(address));
        }
        let url = address.to_owned();
        let socket = rocky_platform::task::spawn_local(async move {
            WsMeta::connect(url, None)
                .await
                .map(|(_, socket)| socket)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|_| "the browser's WebSocket dial was cancelled")??;
        Ok(Box::new(Io {
            socket,
            read: Vec::new(),
            offset: 0,
            write: Vec::new(),
            ended: false,
        }))
    }
}

struct Io {
    socket: WsStream,
    read: Vec<u8>,
    offset: usize,
    write: Vec<u8>,
    ended: bool,
}
impl Io {
    fn poll_send(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.write.is_empty() {
            return Poll::Ready(Ok(()));
        }
        ready!(Pin::new(&mut self.socket).poll_ready(cx)).map_err(io::Error::other)?;
        let message = WsMessage::Binary(std::mem::take(&mut self.write));
        Pin::new(&mut self.socket)
            .start_send(message)
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(()))
    }
}
impl AsyncRead for Io {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            let unread = this.read.get(this.offset..).unwrap_or_default();
            if !unread.is_empty() {
                let taken = buf.remaining().min(unread.len());
                buf.put_slice(unread.get(..taken).unwrap_or_default());
                this.offset += taken;
                return Poll::Ready(Ok(()));
            }
            if this.ended {
                return Poll::Ready(Ok(()));
            }
            match ready!(Pin::new(&mut this.socket).poll_next(cx)) {
                Some(WsMessage::Binary(bytes)) => {
                    this.read = bytes;
                    this.offset = 0;
                }
                Some(WsMessage::Text(_)) => {}
                None => this.ended = true,
            }
        }
    }
}
impl AsyncWrite for Io {
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
        this.write
            .extend_from_slice(buf.get(..taken).unwrap_or_default());
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
        let _ = ready!(Pin::new(&mut this.socket).poll_close(cx));
        Poll::Ready(Ok(()))
    }
}
