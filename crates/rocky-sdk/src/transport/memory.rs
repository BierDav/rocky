//! An in-process transport for tests and deterministic faults: named
//! nodes on a hub joined by bounded pipes. The hub counts what is asked of
//! it and injects stalls, disconnects and backpressure.
use super::{BoxFuture, Connector, Failure, Protocol, Stream, Transport};
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf},
    sync::mpsc,
};

/// Inbound streams a node has not taken yet; further dials are refused.
pub const PENDING: usize = 8;

type Inbound = (Protocol, Stream);
/// A node's queue and whether a service takes from it.
type Listener = (mpsc::Sender<Inbound>, Arc<AtomicBool>);
/// The network: who listens, every start, dial and close so far, and the
/// faults to inject.
pub struct Hub {
    listeners: Mutex<HashMap<String, Listener>>,
    pub starts: AtomicUsize,
    pub dials: AtomicUsize,
    pub closes: AtomicUsize,
    /// While set, a dial never completes.
    pub stalled: AtomicBool,
    /// Bytes each pipe buffers before its writer waits.
    pub capacity: AtomicUsize,
    cut: Mutex<Option<usize>>,
    connectivity: tokio::sync::watch::Sender<()>,
}
impl Default for Hub {
    fn default() -> Self {
        Self {
            listeners: Mutex::default(),
            starts: AtomicUsize::new(0),
            dials: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            stalled: AtomicBool::new(false),
            capacity: AtomicUsize::new(64 * 1024),
            cut: Mutex::new(None),
            connectivity: tokio::sync::watch::channel(()).0,
        }
    }
}
impl Hub {
    /// A connector for the node `name`, which is also its address.
    pub fn connector(self: &Arc<Self>, name: &str) -> Arc<Memory> {
        Arc::new(Memory {
            hub: self.clone(),
            name: name.into(),
        })
    }
    /// Signals a controllable network transition, without advancing time.
    pub fn network_changed(&self) {
        self.connectivity.send_replace(());
    }
    pub fn listens(&self, name: &str) -> bool {
        self.listeners
            .lock()
            .is_ok_and(|listeners| listeners.contains_key(name))
    }
    /// Starts, dials and closes so far.
    pub fn counts(&self) -> [usize; 3] {
        [&self.starts, &self.dials, &self.closes].map(|count| count.load(Ordering::SeqCst))
    }
    /// Breaks every stream once `bytes` more have been written across the
    /// hub; `None` heals it for later writes.
    pub fn cut_after(&self, bytes: Option<usize>) {
        *self.cut.lock().unwrap_or_else(PoisonError::into_inner) = bytes;
    }
    /// How many of `wanted` bytes may still cross; `None` once cut.
    fn allow(&self, wanted: usize) -> Option<usize> {
        match *self.cut.lock().unwrap_or_else(PoisonError::into_inner) {
            None => Some(wanted),
            Some(0) => None,
            Some(left) => Some(wanted.min(left)),
        }
    }
    fn spend(&self, written: usize) {
        if let Some(left) = &mut *self.cut.lock().unwrap_or_else(PoisonError::into_inner) {
            *left = left.saturating_sub(written);
        }
    }
}

/// One end of a pipe, subject to the hub's cut.
struct Pipe {
    io: DuplexStream,
    hub: Arc<Hub>,
}
fn cut() -> std::io::Error {
    std::io::ErrorKind::ConnectionReset.into()
}
impl AsyncRead for Pipe {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.hub.allow(1).is_none() {
            return Poll::Ready(Err(cut()));
        }
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for Pipe {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let Some(allowed) = self.hub.allow(buf.len()) else {
            return Poll::Ready(Err(cut()));
        };
        let written = Pin::new(&mut self.io).poll_write(cx, &buf[..allowed]);
        if let Poll::Ready(Ok(written)) = &written {
            self.hub.spend(*written);
        }
        written
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// Starts the node `name` on `hub`.
pub struct Memory {
    pub hub: Arc<Hub>,
    pub name: String,
}
struct Node {
    hub: Arc<Hub>,
    name: String,
    inbound: tokio::sync::Mutex<mpsc::Receiver<Inbound>>,
    listening: Arc<AtomicBool>,
}
impl Connector for Memory {
    fn start(
        &self,
        _accepts: bool,
        _secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Arc<dyn Transport>, Failure>> {
        Box::pin(async move {
            self.hub.starts.fetch_add(1, Ordering::SeqCst);
            let (sender, receiver) = mpsc::channel(PENDING);
            let listening = Arc::new(AtomicBool::new(false));
            self.hub
                .listeners
                .lock()
                .map_err(|_| "hub poisoned")?
                .insert(self.name.clone(), (sender, listening.clone()));
            Ok(Arc::new(Node {
                hub: self.hub.clone(),
                name: self.name.clone(),
                inbound: tokio::sync::Mutex::new(receiver),
                listening,
            }) as Arc<dyn Transport>)
        })
    }
}
impl Transport for Node {
    fn connectivity(&self) -> Option<tokio::sync::watch::Receiver<()>> {
        Some(self.hub.connectivity.subscribe())
    }
    fn kind(&self) -> &str {
        "memory"
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async { Ok(self.name.clone()) })
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            self.hub.dials.fetch_add(1, Ordering::SeqCst);
            if self.hub.stalled.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            let (listener, listening) = self
                .hub
                .listeners
                .lock()
                .map_err(|_| "hub poisoned")?
                .get(address)
                .cloned()
                .ok_or("unreachable")?;
            if !listening.load(Ordering::SeqCst) {
                return Err("no service listens there".into());
            }
            let (near, far) = tokio::io::duplex(self.hub.capacity.load(Ordering::SeqCst).max(1));
            let pipe = |io| Pipe {
                io,
                hub: self.hub.clone(),
            };
            listener
                .try_send((protocol, Box::new(pipe(far))))
                .map_err(|_| "unreachable or busy")?;
            Ok(Box::new(pipe(near)) as Stream)
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<Inbound>> {
        Box::pin(async { self.inbound.lock().await.recv().await })
    }
    fn listen(&self, listening: bool) {
        self.listening.store(listening, Ordering::SeqCst);
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.hub.closes.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut listeners) = self.hub.listeners.lock() {
                listeners.remove(&self.name);
            }
            self.inbound.lock().await.close();
        })
    }
}
