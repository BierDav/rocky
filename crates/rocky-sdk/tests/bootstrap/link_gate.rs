//! The bootstrap fault boundary is a logical request channel, not a new dial.
use super::*;
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(super) struct LinkGate {
    pub inner: rocky_sdk::transport::Stream,
    pub allowed: tokio::sync::watch::Sender<bool>,
    pub attempts: tokio::sync::watch::Sender<u64>,
    pub refuse: Arc<AtomicBool>,
    pub max_attempts: Arc<AtomicU64>,
    pub pause_on_attempt: Arc<AtomicU64>,
    buffer: Vec<u8>,
    sent: usize,
    waiting: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    complete: bool,
    denied: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Signed {
    sequence: u64,
    bytes: Vec<u8>,
    signature: rocky_credentials::Signature,
}
impl LinkGate {
    pub fn new(inner: rocky_sdk::transport::Stream, gate: &Gate) -> Self {
        Self {
            inner,
            allowed: gate.allowed.clone(),
            attempts: gate.attempts.clone(),
            refuse: gate.refuse.clone(),
            max_attempts: gate.max_attempts.clone(),
            pause_on_attempt: gate.pause_on_attempt.clone(),
            buffer: Vec::new(),
            sent: 0,
            waiting: None,
            complete: false,
            denied: false,
        }
    }
    fn observe(&mut self) {
        use rocky_secure::network::link::{Frame, Message, channels::Packet};
        let opened = rocky_credentials::decode_bounded::<Signed>(
            &self.buffer[4..],
            rocky_secure::MAX_SECURE_FRAME,
        )
        .ok()
        .and_then(|signed| {
            rocky_credentials::decode_bounded::<Frame>(
                &signed.bytes,
                rocky_secure::MAX_SECURE_FRAME,
            )
            .ok()
        })
        .is_some_and(|frame| matches!(frame.message, Message::Channel(Packet::Open { .. })));
        if !opened {
            return;
        }
        self.attempts.send_modify(|count| *count += 1);
        let count = *self.attempts.borrow();
        self.denied =
            self.refuse.load(Ordering::SeqCst) || count > self.max_attempts.load(Ordering::SeqCst);
        if count == self.pause_on_attempt.load(Ordering::SeqCst) {
            self.allowed.send_replace(false);
        }
        if !*self.allowed.borrow() {
            let mut allowed = self.allowed.subscribe();
            self.waiting = Some(Box::pin(async move {
                let _ = allowed.wait_for(|allowed| *allowed).await;
            }));
        }
    }
    fn drain(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.denied {
            return Poll::Ready(Err(std::io::Error::other("injected sync refusal")));
        }
        if let Some(waiting) = &mut self.waiting {
            std::task::ready!(waiting.as_mut().poll(cx));
            self.waiting = None;
        }
        while self.sent < self.buffer.len() {
            let count = std::task::ready!(
                Pin::new(&mut self.inner).poll_write(cx, &self.buffer[self.sent..])
            )?;
            if count == 0 {
                return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
            }
            self.sent += count;
        }
        self.buffer.clear();
        self.sent = 0;
        self.complete = false;
        Poll::Ready(Ok(()))
    }
}
impl AsyncRead for LinkGate {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for LinkGate {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.complete {
            std::task::ready!(self.drain(cx))?;
        }
        let remaining = if self.buffer.len() < 4 {
            4 - self.buffer.len()
        } else {
            let length = u32::from_be_bytes(
                self.buffer[..4]
                    .try_into()
                    .map_err(|_| std::io::Error::other("frame header"))?,
            ) as usize;
            if length > rocky_secure::MAX_SECURE_FRAME {
                return Poll::Ready(Err(std::io::Error::other("frame quota")));
            }
            length + 4 - self.buffer.len()
        };
        let count = remaining.min(bytes.len());
        self.buffer.extend_from_slice(&bytes[..count]);
        if self.buffer.len() >= 4 {
            let length = u32::from_be_bytes(
                self.buffer[..4]
                    .try_into()
                    .map_err(|_| std::io::Error::other("frame header"))?,
            ) as usize;
            if self.buffer.len() == length + 4 {
                self.complete = true;
                self.observe();
            }
        }
        Poll::Ready(Ok(count))
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        std::task::ready!(self.drain(cx))?;
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        std::task::ready!(self.drain(cx))?;
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
