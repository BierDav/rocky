#[cfg(any(test, feature = "test-support"))]
mod faults;
use super::{
    BoxFuture, Connected, Connector, Discovery, Failure, Found, Incoming, LOCAL_PATH, PathState,
    Protocol, Stream, TOPIC_PATH, Transport,
};
#[cfg(any(test, feature = "test-support"))]
pub use faults::{Draws, LinkFaults};
#[cfg(any(test, feature = "test-support"))]
use std::collections::HashSet;
#[cfg(any(test, feature = "test-support"))]
use std::task::ready;
use std::{
    collections::{BTreeMap, HashMap},
    pin::Pin,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::mpsc,
};

/// Incoming streams a node queues before new ones are refused.
pub const PENDING: usize = 8;

type Inbound = Incoming;
#[derive(Clone)]
struct Listener {
    sender: mpsc::Sender<Inbound>,
    listening: Arc<AtomicBool>,
    scope: Arc<Mutex<Option<rocky_core::Scope>>>,
    rpc: Arc<tokio::sync::Semaphore>,
    refused: Arc<AtomicUsize>,
    closed: tokio::sync::watch::Sender<bool>,
}
const RPC_INBOUND: usize = 32;
/// How many requests a node serves and has refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RpcAdmission {
    /// Requests served now.
    pub active: usize,
    /// The most served at once.
    pub limit: usize,
    /// Requests refused so far.
    pub refused: usize,
}
struct RpcSlot {
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    changed: tokio::sync::watch::Sender<()>,
}
impl Drop for RpcSlot {
    fn drop(&mut self) {
        drop(self.permit.take());
        self.changed.send_replace(());
    }
}
/// An in-process network for running several devices in one process, as tests and the todo desktop
/// demo do.
pub struct Hub {
    listeners: Mutex<HashMap<String, Listener>>,
    /// Transports started.
    pub starts: AtomicUsize,
    /// Dials made.
    pub dials: AtomicUsize,
    /// Transports closed.
    pub closes: AtomicUsize,
    /// Holds every dial while set.
    #[cfg(any(test, feature = "test-support"))]
    pub stalled: AtomicBool,
    /// The buffer of each in-process stream in bytes; 64 KiB by default.
    pub capacity: AtomicUsize,
    #[cfg(any(test, feature = "test-support"))]
    cut: Mutex<CutGate>,
    #[cfg(any(test, feature = "test-support"))]
    stalled_peers: Mutex<HashSet<String>>,
    peer_dials: Mutex<HashMap<String, usize>>,
    protocol_dials: Mutex<HashMap<(String, String, Protocol), usize>>,
    link_messages: Mutex<HashMap<String, usize>>,
    links: Mutex<Vec<OpenPipe>>,
    dial_events: tokio::sync::watch::Sender<()>,
    written: tokio::sync::watch::Sender<usize>,
    /// Makes a cut stream wait instead of failing while set.
    #[cfg(any(test, feature = "test-support"))]
    pub hold_cut: AtomicBool,
    connectivity: tokio::sync::watch::Sender<()>,
    admissions: tokio::sync::watch::Sender<()>,
    transcripts: Mutex<HashMap<String, Tap>>,
    /// Whether nodes find each other by topic.
    pub discovery: AtomicBool,
    announced: Mutex<BTreeMap<String, Announcement>>,
    announcements: tokio::sync::watch::Sender<()>,
    #[cfg(any(test, feature = "test-support"))]
    seed: u64,
    #[cfg(any(test, feature = "test-support"))]
    faults: Mutex<HashMap<[String; 2], LinkFaults>>,
    #[cfg(any(test, feature = "test-support"))]
    down: Mutex<HashSet<[String; 2]>>,
    #[cfg(any(test, feature = "test-support"))]
    ordinals: Mutex<HashMap<(String, String, Protocol), u64>>,
    #[cfg(any(test, feature = "test-support"))]
    events: Mutex<Vec<String>>,
}
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
struct CutGate {
    budget: Option<usize>,
    waiters: Vec<Waker>,
}
struct Announcement {
    node: [u8; 32],
    topics: Vec<[u8; 32]>,
    found: mpsc::Sender<Found>,
}
type Tap = Arc<Mutex<Vec<u8>>>;
type OpenPipe = ([String; 2], Protocol, std::sync::Weak<Severed>);
impl Default for Hub {
    fn default() -> Self {
        Self {
            listeners: Mutex::default(),
            starts: AtomicUsize::new(0),
            dials: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            #[cfg(any(test, feature = "test-support"))]
            stalled: AtomicBool::new(false),
            capacity: AtomicUsize::new(64 * 1024),
            #[cfg(any(test, feature = "test-support"))]
            cut: Mutex::default(),
            #[cfg(any(test, feature = "test-support"))]
            stalled_peers: Mutex::default(),
            peer_dials: Mutex::default(),
            protocol_dials: Mutex::default(),
            link_messages: Mutex::default(),
            links: Mutex::default(),
            dial_events: tokio::sync::watch::channel(()).0,
            written: tokio::sync::watch::channel(0).0,
            #[cfg(any(test, feature = "test-support"))]
            hold_cut: AtomicBool::new(false),
            connectivity: tokio::sync::watch::channel(()).0,
            admissions: tokio::sync::watch::channel(()).0,
            transcripts: Mutex::default(),
            discovery: AtomicBool::new(false),
            announced: Mutex::default(),
            announcements: tokio::sync::watch::channel(()).0,
            #[cfg(any(test, feature = "test-support"))]
            seed: 0,
            #[cfg(any(test, feature = "test-support"))]
            faults: Mutex::default(),
            #[cfg(any(test, feature = "test-support"))]
            down: Mutex::default(),
            #[cfg(any(test, feature = "test-support"))]
            ordinals: Mutex::default(),
            #[cfg(any(test, feature = "test-support"))]
            events: Mutex::default(),
        }
    }
}
fn pair(a: &str, b: &str) -> [String; 2] {
    let mut pair = [a.to_owned(), b.to_owned()];
    pair.sort();
    pair
}
impl Hub {
    /// A hub whose injected faults draw from `seed`, so a run repeats.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seeded(seed: u64) -> Self {
        Self {
            seed,
            ..Self::default()
        }
    }
    /// Injects faults on the link between two nodes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_faults(&self, a: &str, b: &str, faults: LinkFaults) {
        self.faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(pair(a, b), faults);
    }
    /// Takes the link between two nodes down or back up; taking it down severs open streams.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_down(&self, a: &str, b: &str, down: bool) {
        let link = pair(a, b);
        let changed = {
            let mut links = self.down.lock().unwrap_or_else(PoisonError::into_inner);
            if down {
                links.insert(link.clone())
            } else {
                links.remove(&link)
            }
        };
        if changed {
            self.event(format!(
                "{} {} {}",
                if down { "down" } else { "up" },
                link[0],
                link[1]
            ));
        }
        if down {
            self.sever(|nodes, _| *nodes == link);
        }
    }
    /// Whether the link between two nodes is down.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_down(&self, a: &str, b: &str) -> bool {
        self.down
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&pair(a, b))
    }
    /// Every link up and down event so far.
    #[cfg(any(test, feature = "test-support"))]
    pub fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    #[cfg(any(test, feature = "test-support"))]
    fn event(&self, line: String) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line);
    }
    fn sever(&self, matches: impl Fn(&[String; 2], Protocol) -> bool) {
        let mut links = self.links.lock().unwrap_or_else(PoisonError::into_inner);
        links.retain(|(nodes, protocol, severed)| {
            let Some(severed) = severed.upgrade() else {
                return false;
            };
            if matches(nodes, *protocol) {
                severed.sever();
                return false;
            }
            true
        });
    }
    /// Link messages a node sent.
    pub fn link_messages(&self, node: &str) -> usize {
        self.link_messages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(node)
            .copied()
            .unwrap_or_default()
    }

    /// A connector for a node of this name, for `Network::Owned`.
    pub fn connector(self: &Arc<Self>, name: &str) -> Arc<Memory> {
        Arc::new(Memory {
            hub: self.clone(),
            name: name.into(),
        })
    }
    /// Routes another transport through the hub so its traffic is counted and recorded.
    pub fn wrap(self: &Arc<Self>, name: &str, transport: Connected) -> Connected {
        Connected {
            discovery: transport.discovery,
            changes: transport.changes,
            transport: Arc::new(Wrapped {
                hub: self.clone(),
                name: name.into(),
                transport: transport.transport,
            }),
        }
    }
    /// Starts recording the bytes a node sends and receives.
    pub fn record(&self, name: &str) {
        self.transcripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(name.into())
            .or_default();
    }
    /// The bytes recorded for a node.
    pub fn transcript(&self, name: &str) -> Vec<u8> {
        self.transcripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .map(|tap| tap.lock().unwrap_or_else(PoisonError::into_inner).clone())
            .unwrap_or_default()
    }
    fn taps(&self, ends: [&str; 2]) -> Vec<Tap> {
        let transcripts = self
            .transcripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        ends.iter()
            .filter_map(|end| transcripts.get(*end).cloned())
            .collect()
    }
    /// The discovery topics a node announces.
    pub fn topics(&self, name: &str) -> Vec<[u8; 32]> {
        self.announced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .map(|announcement| announcement.topics.clone())
            .unwrap_or_default()
    }
    /// Waits until `ready` holds for the announced topics.
    pub async fn wait_announced(&self, ready: impl Fn(&Self) -> bool) {
        let mut announcements = self.announcements.subscribe();
        while !ready(self) {
            if announcements.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
    /// The nodes that share a topic with `name`.
    pub fn discoverable(&self, name: &str) -> Vec<String> {
        let announced = self
            .announced
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(own) = announced.get(name) else {
            return Vec::new();
        };
        let mut names: Vec<String> = announced
            .iter()
            .filter(|(other, announcement)| {
                *other != name
                    && announcement
                        .topics
                        .iter()
                        .any(|topic| own.topics.contains(topic))
            })
            .map(|(other, _)| other.clone())
            .collect();
        names.sort();
        names
    }
    /// Holds or releases every dial to one node.
    #[cfg(any(test, feature = "test-support"))]
    pub fn stall_peer(&self, address: &str, stalled: bool) {
        let mut peers = self
            .stalled_peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if stalled {
            peers.insert(address.into());
        } else {
            peers.remove(address);
        }
    }
    /// Dials from one node to another on a protocol.
    pub fn protocol_dials(&self, from: &str, to: &str, protocol: Protocol) -> usize {
        self.protocol_dials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(from.into(), to.into(), protocol))
            .copied()
            .unwrap_or(0)
    }
    /// Dials to a node.
    pub fn dials_to(&self, address: &str) -> usize {
        self.peer_dials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(address)
            .copied()
            .unwrap_or(0)
    }
    /// Waits until a node was dialed `count` times.
    pub async fn wait_dials(&self, address: &str, count: usize) {
        let mut events = self.dial_events.subscribe();
        loop {
            events.borrow_and_update();
            if self.dials_to(address) >= count {
                return;
            }
            if events.changed().await.is_err() {
                return;
            }
        }
    }
    /// Bytes written so far, as a watch.
    pub fn written(&self) -> tokio::sync::watch::Receiver<usize> {
        self.written.subscribe()
    }
    /// Signals a network change to every node.
    pub fn network_changed(&self) {
        self.connectivity.send_replace(());
    }
    /// Whether a node listens.
    pub fn listens(&self, name: &str) -> bool {
        self.listeners
            .lock()
            .is_ok_and(|listeners| listeners.contains_key(name))
    }
    /// A node's request admission, or `None` when it does not listen.
    pub fn rpc_admission(&self, name: &str) -> Option<RpcAdmission> {
        let listeners = self.listeners.lock().ok()?;
        let listener = listeners.get(name)?;
        Some(RpcAdmission {
            active: RPC_INBOUND - listener.rpc.available_permits(),
            limit: RPC_INBOUND,
            refused: listener.refused.load(Ordering::SeqCst),
        })
    }
    /// Fires when any node's admission changes.
    pub fn admission_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.admissions.subscribe()
    }
    /// Starts, dials and closes so far.
    pub fn counts(&self) -> [usize; 3] {
        [&self.starts, &self.dials, &self.closes].map(|count| count.load(Ordering::SeqCst))
    }
    /// Severs every link stream of a node.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sever_links(&self, node: &str) {
        self.sever(|nodes, protocol| {
            protocol == Protocol::Link && nodes.iter().any(|name| name == node)
        });
    }
    /// Cuts every stream once `bytes` more are written, or never for `None`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn cut_after(&self, bytes: Option<usize>) {
        let waiters: Vec<_> = {
            let mut gate = self.cut.lock().unwrap_or_else(PoisonError::into_inner);
            gate.budget = bytes;
            std::mem::take(&mut gate.waiters)
        };
        for waker in waiters {
            waker.wake();
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    fn allow_or_block(&self, wanted: usize, cx: &Context<'_>) -> Poll<std::io::Result<usize>> {
        let mut gate = self.cut.lock().unwrap_or_else(PoisonError::into_inner);
        match gate.budget {
            None => Poll::Ready(Ok(wanted)),
            Some(0) => {
                if !self.hold_cut.load(Ordering::SeqCst) {
                    return Poll::Ready(Err(cut()));
                }
                if !gate.waiters.iter().any(|waker| waker.will_wake(cx.waker())) {
                    gate.waiters.push(cx.waker().clone());
                }
                Poll::Pending
            }
            Some(left) => Poll::Ready(Ok(wanted.min(left))),
        }
    }
    fn spend(&self, written: usize) {
        #[cfg(any(test, feature = "test-support"))]
        {
            let mut gate = self.cut.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(left) = &mut gate.budget {
                *left = left.saturating_sub(written);
            }
        }
        self.written.send_modify(|total| *total += written);
    }
}

#[derive(Default)]
struct Severed {
    set: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
    #[cfg(any(test, feature = "test-support"))]
    budget: Mutex<Option<usize>>,
}
impl Severed {
    #[cfg(any(test, feature = "test-support"))]
    fn with_budget(budget: Option<usize>) -> Self {
        Self {
            budget: Mutex::new(budget),
            ..Self::default()
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    fn allow(&self, wanted: usize) -> usize {
        self.budget
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map_or(wanted, |left| wanted.min(left))
    }
    #[cfg(any(test, feature = "test-support"))]
    fn carry(&self, carried: usize) -> bool {
        let mut budget = self.budget.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut *budget {
            Some(left) => {
                *left = left.saturating_sub(carried);
                *left == 0
            }
            None => false,
        }
    }
    fn sever(&self) {
        self.set.store(true, Ordering::SeqCst);
        for waker in self
            .wakers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            waker.wake();
        }
    }
    fn check(&self, cx: &Context<'_>) -> std::io::Result<()> {
        let mut wakers = self.wakers.lock().unwrap_or_else(PoisonError::into_inner);
        if self.set.load(Ordering::SeqCst) {
            return Err(cut());
        }
        if !wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Ok(())
    }
}

#[derive(Default)]
struct MessageCounter {
    header: [u8; 4],
    filled: usize,
    remaining: usize,
}
impl MessageCounter {
    fn record(&mut self, mut bytes: &[u8]) -> usize {
        let mut complete = 0;
        while !bytes.is_empty() {
            if self.remaining > 0 {
                let count = self.remaining.min(bytes.len());
                self.remaining -= count;
                bytes = &bytes[count..];
                if self.remaining == 0 {
                    complete += 1;
                }
            } else {
                let count = (4 - self.filled).min(bytes.len());
                self.header[self.filled..self.filled + count].copy_from_slice(&bytes[..count]);
                self.filled += count;
                bytes = &bytes[count..];
                if self.filled == 4 {
                    self.remaining = u32::from_be_bytes(self.header) as usize;
                    self.filled = 0;
                    if self.remaining == 0 {
                        complete += 1;
                    }
                }
            }
        }
        complete
    }
}
struct Pipe {
    messages: MessageCounter,
    severed: Arc<Severed>,
    io: Stream,
    hub: Arc<Hub>,
    taps: Vec<Tap>,
    from: String,
    protocol: Protocol,
    #[cfg(any(test, feature = "test-support"))]
    faults: LinkFaults,
    #[cfg(any(test, feature = "test-support"))]
    draws: Draws,
    #[cfg(any(test, feature = "test-support"))]
    label: String,
    #[cfg(any(test, feature = "test-support"))]
    delay: Option<Pin<Box<rocky_platform::time::Sleep>>>,
    #[cfg(any(test, feature = "test-support"))]
    paced: bool,
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
        #[cfg(any(test, feature = "test-support"))]
        let _ = ready!(self.hub.allow_or_block(1, cx))?;
        self.severed.check(cx)?;
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for Pipe {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.severed.check(cx)?;
        #[cfg(any(test, feature = "test-support"))]
        {
            if !self.paced && self.faults.delays() {
                let faults = self.faults;
                let (delay, stalled) = self.draws.delay(&faults, buf.len());
                if stalled {
                    self.hub
                        .event(format!("stall {} {}us", self.label, delay.as_micros()));
                }
                self.delay =
                    (!delay.is_zero()).then(|| Box::pin(rocky_platform::time::sleep(delay)));
                self.paced = true;
            }
            if let Some(delay) = &mut self.delay {
                ready!(delay.as_mut().poll(cx));
                self.delay = None;
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        let allowed = {
            let allowed = ready!(self.hub.allow_or_block(buf.len(), cx))?;
            let allowed = self.severed.allow(allowed);
            if allowed == 0 && !buf.is_empty() {
                return Poll::Ready(Err(cut()));
            }
            allowed
        };
        #[cfg(not(any(test, feature = "test-support")))]
        let allowed = buf.len();
        let written = Pin::new(&mut self.io).poll_write(cx, &buf[..allowed]);
        if let Poll::Ready(Ok(written)) = &written {
            #[cfg(any(test, feature = "test-support"))]
            {
                self.paced = false;
                if self.severed.carry(*written) {
                    self.hub.event(format!("cut {}", self.label));
                    self.severed.sever();
                }
            }
            self.hub.spend(*written);
            if self.protocol == Protocol::Link {
                let complete = self.messages.record(&buf[..*written]);
                *self
                    .hub
                    .link_messages
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .entry(self.from.clone())
                    .or_default() += complete;
            }
            for tap in &self.taps {
                tap.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(&buf[..*written]);
            }
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

/// One named node on a `Hub`.
pub struct Memory {
    /// The hub it is on.
    pub hub: Arc<Hub>,
    /// Its name, which is also its route.
    pub name: String,
}
struct Node {
    hub: Arc<Hub>,
    name: String,
    inbound: tokio::sync::Mutex<mpsc::Receiver<Inbound>>,
    listening: Arc<AtomicBool>,
    scope: Arc<Mutex<Option<rocky_core::Scope>>>,
    closed: tokio::sync::watch::Sender<bool>,
}
struct Finder {
    hub: Arc<Hub>,
    name: String,
    sender: mpsc::Sender<Found>,
    found: tokio::sync::Mutex<mpsc::Receiver<Found>>,
}
impl Discovery for Finder {
    fn announce<'a>(
        &'a self,
        node: [u8; 32],
        topics: &'a [[u8; 32]],
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let mut announced = self.hub.announced.lock().map_err(|_| "hub poisoned")?;
            if announced
                .get(&self.name)
                .is_some_and(|held| held.node == node && held.topics == topics)
            {
                return Ok(());
            }
            announced.remove(&self.name);
            if topics.is_empty() {
                self.hub.announcements.send_replace(());
                return Ok(());
            }
            let hint = |claimed_node, name: &str| Found {
                claimed_node,
                address: name.to_owned(),
                path: LOCAL_PATH.into(),
                local: true,
            };
            for (name, other) in announced.iter() {
                if other.topics.iter().any(|topic| topics.contains(topic)) {
                    let _ = self.sender.try_send(hint(other.node, name));
                    let _ = other.found.try_send(hint(node, &self.name));
                }
            }
            announced.insert(
                self.name.clone(),
                Announcement {
                    node,
                    topics: topics.to_vec(),
                    found: self.sender.clone(),
                },
            );
            self.hub.announcements.send_replace(());
            Ok(())
        })
    }
    fn found(&self) -> BoxFuture<'_, Option<Found>> {
        Box::pin(async { self.found.lock().await.recv().await })
    }
    fn paths(&self) -> Vec<(String, PathState)> {
        vec![
            (LOCAL_PATH.into(), PathState::On),
            (
                TOPIC_PATH.into(),
                PathState::Unavailable("the hub has no lookup service".into()),
            ),
        ]
    }
}
impl Connector for Memory {
    fn start(
        &self,
        _accepts: bool,
        _route_secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Connected, Failure>> {
        Box::pin(async move {
            self.hub.starts.fetch_add(1, Ordering::SeqCst);
            let (sender, receiver) = mpsc::channel(PENDING);
            let listening = Arc::new(AtomicBool::new(false));
            let scope = Arc::new(Mutex::new(None));
            let closed = tokio::sync::watch::channel(false).0;
            self.hub
                .listeners
                .lock()
                .map_err(|_| "hub poisoned")?
                .insert(
                    self.name.clone(),
                    Listener {
                        sender,
                        listening: listening.clone(),
                        scope: scope.clone(),
                        rpc: Arc::new(tokio::sync::Semaphore::new(RPC_INBOUND)),
                        refused: Arc::new(AtomicUsize::new(0)),
                        closed: closed.clone(),
                    },
                );
            let finder = self.hub.discovery.load(Ordering::SeqCst).then(|| {
                let (sender, found) = mpsc::channel(64);
                Arc::new(Finder {
                    hub: self.hub.clone(),
                    name: self.name.clone(),
                    sender,
                    found: tokio::sync::Mutex::new(found),
                })
            });
            let discovery = finder.clone().map(|finder| finder as Arc<dyn Discovery>);
            Ok(Connected {
                transport: Arc::new(Node {
                    hub: self.hub.clone(),
                    name: self.name.clone(),
                    inbound: tokio::sync::Mutex::new(receiver),
                    listening,
                    scope,
                    closed,
                }),
                discovery,
                changes: Some(self.hub.connectivity.subscribe()),
            })
        })
    }
}
impl Transport for Node {
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        Box::pin(request_routes(self, addresses, scope, request))
    }
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(dial_routes(self, addresses, Protocol::Link, scope))
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
        scope: Option<rocky_core::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            if *self.closed.borrow() {
                return Err("memory transport is closed".into());
            }
            self.hub.dials.fetch_add(1, Ordering::SeqCst);
            *self
                .hub
                .peer_dials
                .lock()
                .map_err(|_| "hub poisoned")?
                .entry(address.into())
                .or_default() += 1;
            self.hub.dial_events.send_replace(());
            #[cfg(any(test, feature = "test-support"))]
            if self.hub.stalled.load(Ordering::SeqCst)
                || self
                    .hub
                    .stalled_peers
                    .lock()
                    .map_err(|_| "hub poisoned")?
                    .contains(address)
            {
                std::future::pending::<()>().await;
            }
            #[cfg(any(test, feature = "test-support"))]
            let (label, faults, mut draws, budget) = {
                let ordinal = {
                    let mut ordinals = self.hub.ordinals.lock().map_err(|_| "hub poisoned")?;
                    let next = ordinals
                        .entry((self.name.clone(), address.into(), protocol))
                        .or_default();
                    *next += 1;
                    *next - 1
                };
                let label = format!("{}>{} {protocol:?} #{ordinal}", self.name, address);
                if self.hub.is_down(&self.name, address) {
                    self.hub.event(format!("unreachable {label}"));
                    return Err("unreachable".into());
                }
                let faults = self
                    .hub
                    .faults
                    .lock()
                    .map_err(|_| "hub poisoned")?
                    .get(&pair(&self.name, address))
                    .copied()
                    .unwrap_or_default();
                let mut draws =
                    Draws::stream(self.hub.seed, &self.name, address, protocol, ordinal);
                if draws.chance(faults.refusals) {
                    self.hub.event(format!("refuse {label}"));
                    return Err("refused".into());
                }
                let budget = draws
                    .chance(faults.cuts)
                    .then(|| draws.below(faults.cut_within as u64) as usize + 1);
                (label, faults, draws, budget)
            };
            let listener = self
                .hub
                .listeners
                .lock()
                .map_err(|_| "hub poisoned")?
                .get(address)
                .cloned()
                .ok_or("unreachable")?;
            if scope.is_some() && *listener.scope.lock().map_err(|_| "scope poisoned")? != scope {
                return Err(rocky_core::net::ScopeRefusal::UnknownScope.into());
            }
            if !listener.listening.load(Ordering::SeqCst) {
                return Err("no service listens there".into());
            }
            let rpc_slot = if protocol == Protocol::Rpc {
                let permit = listener.rpc.clone().try_acquire_owned().map_err(|_| {
                    listener.refused.fetch_add(1, Ordering::SeqCst);
                    self.hub.admissions.send_replace(());
                    "RPC admission budget"
                })?;
                self.hub.admissions.send_replace(());
                Some(RpcSlot {
                    permit: Some(permit),
                    changed: self.hub.admissions.clone(),
                })
            } else {
                None
            };
            let (near, far) = tokio::io::duplex(self.hub.capacity.load(Ordering::SeqCst).max(1));
            let taps = self.hub.taps([&self.name, address]);
            *self
                .hub
                .protocol_dials
                .lock()
                .map_err(|_| "hub poisoned")?
                .entry((self.name.clone(), address.into(), protocol))
                .or_default() += 1;
            #[cfg(any(test, feature = "test-support"))]
            let severed = Arc::new(Severed::with_budget(budget));
            #[cfg(not(any(test, feature = "test-support")))]
            let severed = Arc::new(Severed::default());
            self.hub.links.lock().map_err(|_| "hub poisoned")?.push((
                pair(&self.name, address),
                protocol,
                Arc::downgrade(&severed),
            ));
            #[cfg_attr(
                not(any(test, feature = "test-support")),
                expect(unused_mut, reason = "only mutates draws when test-support is enabled")
            )]
            let mut pipe = |io, from: &str| Pipe {
                messages: Default::default(),
                severed: severed.clone(),
                io: Stream::new(io),
                hub: self.hub.clone(),
                taps: taps.clone(),
                from: from.into(),
                protocol,
                #[cfg(any(test, feature = "test-support"))]
                faults,
                #[cfg(any(test, feature = "test-support"))]
                draws: draws.fork(),
                #[cfg(any(test, feature = "test-support"))]
                label: format!("{label} {from}"),
                #[cfg(any(test, feature = "test-support"))]
                delay: None,
                #[cfg(any(test, feature = "test-support"))]
                paced: false,
            };
            let (near, far) = (pipe(near, &self.name), pipe(far, address));
            if protocol == Protocol::Rpc {
                rocky_platform::task::spawn(async move {
                    let _slot = rpc_slot;
                    let mut closed = listener.closed.subscribe();
                    tokio::select! {
                        _ = rocky_platform::time::timeout(std::time::Duration::from_secs(30), rocky_core::net::framing::serve(Stream::new(far), &listener.sender)) => {},
                        _ = closed.wait_for(|closed| *closed) => {},
                    }
                });
            } else {
                listener
                    .sender
                    .try_send(Incoming::Stream(protocol, Stream::new(far)))
                    .map_err(|_| "unreachable or busy")?;
            }
            #[cfg(any(test, feature = "test-support"))]
            self.hub.event(match budget {
                Some(bytes) => format!("open {label} cut at {bytes}"),
                None => format!("open {label}"),
            });
            Ok(Stream::new(near))
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<Inbound>> {
        Box::pin(async {
            let mut closed = self.closed.subscribe();
            let mut inbound = tokio::select! {
                biased;
                _ = closed.wait_for(|closed| *closed) => return None,
                inbound = self.inbound.lock() => inbound,
            };
            tokio::select! {
                biased;
                _ = closed.wait_for(|closed| *closed) => None,
                incoming = inbound.recv() => incoming,
            }
        })
    }
    fn listen(&self, scope: Option<rocky_core::Scope>, listening: bool) -> Result<(), Failure> {
        if listening && *self.closed.borrow() {
            return Err("memory transport is closed".into());
        }
        *self.scope.lock().map_err(|_| "scope poisoned")? = scope;
        self.listening.store(listening, Ordering::SeqCst);
        Ok(())
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.closed.send_replace(true);
            self.hub
                .sever(|nodes, _| nodes.iter().any(|node| node == &self.name));
            self.hub.closes.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut listeners) = self.hub.listeners.lock() {
                listeners.remove(&self.name);
            }
            if let Ok(mut announced) = self.hub.announced.lock() {
                announced.remove(&self.name);
            }
            self.hub.announcements.send_replace(());
            self.inbound.lock().await.close();
        })
    }
}

struct Wrapped {
    hub: Arc<Hub>,
    name: String,
    transport: Arc<dyn Transport>,
}
impl Wrapped {
    fn pipe(&self, mut stream: Stream, protocol: Protocol) -> Stream {
        let lanes = stream.take_lanes();
        let pipe = Pipe {
            messages: Default::default(),
            severed: Default::default(),
            io: stream,
            hub: self.hub.clone(),
            taps: vec![],
            from: self.name.clone(),
            protocol,
            #[cfg(any(test, feature = "test-support"))]
            faults: LinkFaults::default(),
            #[cfg(any(test, feature = "test-support"))]
            draws: Draws::new(0),
            #[cfg(any(test, feature = "test-support"))]
            label: self.name.clone(),
            #[cfg(any(test, feature = "test-support"))]
            delay: None,
            #[cfg(any(test, feature = "test-support"))]
            paced: false,
        };
        Stream::new(pipe).with_lanes(lanes)
    }
}
impl Transport for Wrapped {
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        Box::pin(request_routes(self, addresses, scope, request))
    }
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(dial_routes(self, addresses, Protocol::Link, scope))
    }

    fn kind(&self) -> &str {
        self.transport.kind()
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        self.transport.address()
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
        scope: Option<rocky_core::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            self.hub.dials.fetch_add(1, Ordering::SeqCst);
            *self
                .hub
                .peer_dials
                .lock()
                .map_err(|_| "hub poisoned")?
                .entry(address.into())
                .or_default() += 1;
            self.hub.dial_events.send_replace(());
            #[cfg(any(test, feature = "test-support"))]
            if self.hub.stalled.load(Ordering::SeqCst)
                || self
                    .hub
                    .stalled_peers
                    .lock()
                    .map_err(|_| "hub poisoned")?
                    .contains(address)
            {
                std::future::pending::<()>().await;
            }
            let stream = self.transport.connect(address, protocol, scope).await?;
            *self
                .hub
                .protocol_dials
                .lock()
                .map_err(|_| "hub poisoned")?
                .entry((self.name.clone(), address.into(), protocol))
                .or_default() += 1;
            Ok(self.pipe(stream, protocol))
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<Inbound>> {
        Box::pin(async {
            self.transport
                .accept()
                .await
                .map(|incoming| match incoming {
                    Incoming::Stream(protocol, stream) => {
                        Incoming::Stream(protocol, self.pipe(stream, protocol))
                    }
                    other => other,
                })
        })
    }
    fn listen(&self, scope: Option<rocky_core::Scope>, listening: bool) -> Result<(), Failure> {
        self.transport.listen(scope, listening)
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.hub.closes.fetch_add(1, Ordering::SeqCst);
            self.transport.close().await;
        })
    }
}

async fn request_routes(
    transport: &dyn Transport,
    addresses: &[String],
    scope: rocky_core::Scope,
    request: &[u8],
) -> Result<Vec<u8>, Failure> {
    let mut failure: Failure = "no route".into();
    for address in addresses {
        let result = rocky_platform::time::timeout(std::time::Duration::from_secs(30), async {
            let mut stream = transport
                .connect(address, Protocol::Rpc, Some(scope))
                .await?;
            rocky_core::net::framing::write(&mut stream, request).await?;
            let reply = rocky_core::net::framing::read(&mut stream).await?;
            let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
            Ok::<_, Failure>(reply)
        })
        .await;
        match result {
            Ok(Ok(reply)) => return Ok(reply),
            Ok(Err(error)) => failure = error,
            Err(error) => failure = error.into(),
        }
    }
    Err(failure)
}

async fn dial_routes(
    transport: &dyn Transport,
    addresses: &[String],
    protocol: Protocol,
    scope: rocky_core::Scope,
) -> Result<Stream, Failure> {
    let mut failure: Failure = "no route".into();
    for address in addresses {
        match rocky_platform::time::timeout(
            std::time::Duration::from_secs(15),
            transport.connect(address, protocol, Some(scope)),
        )
        .await
        {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => failure = error,
            Err(error) => failure = error.into(),
        }
    }
    Err(failure)
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn close_cancels_a_partial_rpc_and_pending_accept() -> Result<(), Failure> {
        let hub = Arc::new(Hub::default());
        let server = hub.connector("server").start(true, [1; 32]).await?;
        let caller = hub.connector("caller").start(false, [2; 32]).await?;
        server.listen(None, true)?;
        let _partial = caller.connect("server", Protocol::Rpc, None).await?;
        let accept = server.accept();
        tokio::pin!(accept);
        tokio::select! {
            biased;
            _ = &mut accept => return Err("partial RPC reached dispatch".into()),
            () = tokio::task::yield_now() => {},
        }
        let ((), incoming) =
            rocky_platform::time::timeout(std::time::Duration::from_secs(30), async {
                tokio::join!(server.close(), &mut accept)
            })
            .await?;
        assert!(incoming.is_none());
        assert!(server.listen(None, true).is_err());
        caller.close().await;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn rpc_falls_back_after_each_stalled_route_deadline() -> Result<(), Failure> {
        let hub = Arc::new(Hub::default());
        let server = hub.connector("third").start(true, [1; 32]).await?;
        let caller = hub.connector("caller").start(false, [2; 32]).await?;
        let scope = rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        server.listen(Some(scope), true)?;
        hub.stall_peer("first", true);
        hub.stall_peer("second", true);
        let routes = ["first".into(), "second".into(), "third".into()];
        let answer = async {
            let Some(Incoming::Request(request)) = server.accept().await else {
                return Err("request expected".into());
            };
            assert_eq!(request.bytes, b"request");
            request.respond(b"answer".to_vec()).await
        };
        let (reply, ()) = tokio::try_join!(caller.request(&routes, scope, b"request"), answer)?;
        assert_eq!(reply, b"answer");
        assert_eq!(hub.counts()[1], 3);
        tokio::join!(caller.close(), server.close());
        Ok(())
    }
    #[tokio::test(start_paused = true)]
    async fn session_falls_back_after_a_stalled_route() -> Result<(), Failure> {
        let hub = Arc::new(Hub::default());
        let server = hub.connector("server").start(true, [1; 32]).await?;
        let caller = hub.connector("caller").start(false, [2; 32]).await?;
        let scope = rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        server.listen(Some(scope), true)?;
        hub.stall_peer("stalled", true);
        let _stream = caller
            .session(&["stalled".into(), "server".into()], scope)
            .await?;
        assert!(matches!(
            server.accept().await,
            Some(Incoming::Stream(Protocol::Link, _))
        ));
        tokio::join!(caller.close(), server.close());
        Ok(())
    }

    #[tokio::test]
    async fn scoped_requests_and_sessions_refuse_another_database() -> Result<(), Failure> {
        let hub = Arc::new(Hub::default());
        let server = hub.connector("server").start(true, [1; 32]).await?;
        let caller = hub.connector("caller").start(false, [2; 32]).await?;
        let caller = hub.wrap("wrapped", caller);
        let scope = rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        server.listen(Some(scope), true)?;
        let wrong = rocky_core::Scope {
            domain: rocky_core::DomainId([4; 16]),
            ..scope
        };
        let routes = ["server".into()];
        let error = caller
            .request(&routes, wrong, b"request")
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<rocky_core::net::ScopeRefusal>(),
            Some(&rocky_core::net::ScopeRefusal::UnknownScope)
        );
        let error = caller
            .session(&routes, wrong)
            .await
            .err()
            .ok_or("wrong scope accepted")?;
        assert_eq!(
            error.downcast_ref::<rocky_core::net::ScopeRefusal>(),
            Some(&rocky_core::net::ScopeRefusal::UnknownScope)
        );
        let answer = async {
            let Some(Incoming::Request(request)) = server.accept().await else {
                return Err("request expected".into());
            };
            assert_eq!(request.bytes, b"request");
            request.respond(b"answer".to_vec()).await
        };
        let (reply, ()) = tokio::try_join!(caller.request(&routes, scope, b"request"), answer)?;
        assert_eq!(reply, b"answer");
        let _stream = caller.session(&routes, scope).await?;
        assert!(matches!(
            server.accept().await,
            Some(Incoming::Stream(Protocol::Link, _))
        ));
        tokio::join!(caller.close(), server.close());
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn rpc_falls_back_when_the_first_route_never_replies() -> Result<(), Failure> {
        let hub = Arc::new(Hub::default());
        let silent = hub.connector("silent").start(true, [1; 32]).await?;
        let server = hub.connector("server").start(true, [2; 32]).await?;
        let caller = hub.connector("caller").start(false, [3; 32]).await?;
        let scope = rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        silent.listen(Some(scope), true)?;
        server.listen(Some(scope), true)?;
        let answer = async {
            let Some(Incoming::Request(held)) = silent.accept().await else {
                return Err("first request expected".into());
            };
            let Some(Incoming::Request(request)) = server.accept().await else {
                return Err("second request expected".into());
            };
            assert_eq!(request.bytes, held.bytes);
            let result = request.respond(b"answer".to_vec()).await;
            drop(held);
            result
        };
        let routes = ["silent".into(), "server".into()];
        let (reply, ()) = tokio::try_join!(caller.request(&routes, scope, b"request"), answer)?;
        assert_eq!(reply, b"answer");
        tokio::join!(caller.close(), silent.close(), server.close());
        Ok(())
    }
}
