//! An in-process transport for tests and deterministic faults: named
//! nodes on a hub joined by bounded pipes. The hub counts what is asked of
//! it and injects stalls, disconnects and backpressure, and per link the
//! seeded faults of [`LinkFaults`].
mod faults;
use super::{
    BoxFuture, Connector, Discovery, Failure, Found, LOCAL_PATH, PathState, Protocol, Stream,
    TOPIC_PATH, Transport,
};
pub use faults::{Draws, LinkFaults};
use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker, ready},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
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
    cut_waiters: Mutex<Vec<Waker>>,
    stalled_peers: Mutex<HashSet<String>>,
    peer_dials: Mutex<HashMap<String, usize>>,
    protocol_dials: Mutex<HashMap<(String, String, Protocol), usize>>,
    link_messages: Mutex<HashMap<String, usize>>,
    /// Open pipes by their two nodes, for `sever_links` and `set_down`.
    links: Mutex<Vec<OpenPipe>>,
    dial_events: tokio::sync::watch::Sender<()>,
    written: tokio::sync::watch::Sender<usize>,
    /// At the cut, hold the stream instead of disconnecting it.
    pub hold_cut: AtomicBool,
    connectivity: tokio::sync::watch::Sender<()>,
    /// What every stream of a recorded node carried, both ways.
    transcripts: Mutex<HashMap<String, Tap>>,
    /// Whether nodes started from now on can discover.
    pub discovery: AtomicBool,
    /// Who announces under which topics, and where each node hears of it.
    announced: Mutex<HashMap<String, Announcement>>,
    /// Where every fault draw starts.
    seed: u64,
    /// Faults by the sorted names of a link's two nodes.
    faults: Mutex<HashMap<[String; 2], LinkFaults>>,
    /// Links that refuse dials until healed.
    down: Mutex<HashSet<[String; 2]>>,
    /// Streams dialed so far per direction and protocol.
    ordinals: Mutex<HashMap<(String, String, Protocol), u64>>,
    /// One line per stream opened or refused and per fault injected.
    events: Mutex<Vec<String>>,
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
            stalled: AtomicBool::new(false),
            capacity: AtomicUsize::new(64 * 1024),
            cut: Mutex::new(None),
            cut_waiters: Mutex::default(),
            stalled_peers: Mutex::default(),
            peer_dials: Mutex::default(),
            protocol_dials: Mutex::default(),
            link_messages: Mutex::default(),
            links: Mutex::default(),
            dial_events: tokio::sync::watch::channel(()).0,
            written: tokio::sync::watch::channel(0).0,
            hold_cut: AtomicBool::new(false),
            connectivity: tokio::sync::watch::channel(()).0,
            transcripts: Mutex::default(),
            discovery: AtomicBool::new(false),
            announced: Mutex::default(),
            seed: 0,
            faults: Mutex::default(),
            down: Mutex::default(),
            ordinals: Mutex::default(),
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
    /// A hub whose link faults draw from `seed`.
    pub fn seeded(seed: u64) -> Self {
        Self {
            seed,
            ..Self::default()
        }
    }
    /// Faults of later streams between `a` and `b`, both ways.
    pub fn set_faults(&self, a: &str, b: &str, faults: LinkFaults) {
        self.faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(pair(a, b), faults);
    }
    /// While down, dials between `a` and `b` are refused; going down
    /// fails their open streams.
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
    pub fn is_down(&self, a: &str, b: &str) -> bool {
        self.down
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&pair(a, b))
    }
    /// What the hub opened, refused and injected so far, one line each.
    pub fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
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
    /// Complete length-prefixed link messages written by this node, including its handshake.
    pub fn link_messages(&self, node: &str) -> usize {
        self.link_messages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(node)
            .copied()
            .unwrap_or_default()
    }

    /// A connector for the node `name`, which is also its address.
    pub fn connector(self: &Arc<Self>, name: &str) -> Arc<Memory> {
        Arc::new(Memory {
            hub: self.clone(),
            name: name.into(),
        })
    }
    /// Applies hub faults and counters to a real transport for qualification.
    pub fn wrap(self: &Arc<Self>, name: &str, transport: Arc<dyn Transport>) -> Arc<dyn Transport> {
        Arc::new(Wrapped {
            hub: self.clone(),
            name: name.into(),
            transport,
        })
    }
    /// Records the bytes of every later stream `name` dials or accepts.
    pub fn record(&self, name: &str) {
        self.transcripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(name.into())
            .or_default();
    }
    /// What the streams of a recorded node carried so far.
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
    /// The topics `name` announces under, the current one first.
    pub fn topics(&self, name: &str) -> Vec<[u8; 32]> {
        self.announced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .map(|announcement| announcement.topics.clone())
            .unwrap_or_default()
    }
    /// The nodes that announce under a topic `name` announces under: whom
    /// it finds, and who finds it.
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
    /// Holds dials to just this address; does not signal connectivity.
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
    pub fn protocol_dials(&self, from: &str, to: &str, protocol: Protocol) -> usize {
        self.protocol_dials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(from.into(), to.into(), protocol))
            .copied()
            .unwrap_or(0)
    }
    pub fn dials_to(&self, address: &str) -> usize {
        self.peer_dials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(address)
            .copied()
            .unwrap_or(0)
    }
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
    /// Total bytes written, and an event for deterministic stream cuts.
    pub fn written(&self) -> tokio::sync::watch::Receiver<usize> {
        self.written.subscribe()
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
    /// Fails the open link streams of `node`, as a transport's idle timeout
    /// ends a connection to a vanished peer. Later links are unaffected.
    pub fn sever_links(&self, node: &str) {
        self.sever(|nodes, protocol| {
            protocol == Protocol::Link && nodes.iter().any(|name| name == node)
        });
    }
    /// Breaks every stream once `bytes` more have been written across the
    /// hub; `None` heals it for later writes.
    pub fn cut_after(&self, bytes: Option<usize>) {
        *self.cut.lock().unwrap_or_else(PoisonError::into_inner) = bytes;
        for waker in self
            .cut_waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            waker.wake();
        }
    }
    fn blocked(&self, cx: &Context<'_>) -> Poll<std::io::Result<()>> {
        if !self.hold_cut.load(Ordering::SeqCst) {
            return Poll::Ready(Err(cut()));
        }
        let mut waiters = self
            .cut_waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !waiters.iter().any(|waker| waker.will_wake(cx.waker())) {
            waiters.push(cx.waker().clone());
        }
        Poll::Pending
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
        self.written.send_modify(|total| *total += written);
        if let Some(left) = &mut *self.cut.lock().unwrap_or_else(PoisonError::into_inner) {
            *left = left.saturating_sub(written);
        }
    }
}

/// Shared by both ends of one pipe: once set, both fail.
#[derive(Default)]
struct Severed {
    set: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
    /// The seeded cut: bytes both ends may still carry together.
    budget: Mutex<Option<usize>>,
}
impl Severed {
    fn with_budget(budget: Option<usize>) -> Self {
        Self {
            budget: Mutex::new(budget),
            ..Self::default()
        }
    }
    /// How many of `wanted` bytes may cross before the seeded cut.
    fn allow(&self, wanted: usize) -> usize {
        self.budget
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map_or(wanted, |left| wanted.min(left))
    }
    /// Spends `carried` bytes; true once that reaches the cut.
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
    /// Fails once severed; otherwise wakes `cx` when it is.
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

/// One end of a pipe, subject to the hub's cut.
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
    faults: LinkFaults,
    draws: Draws,
    /// The stream in the hub's events.
    label: String,
    /// The drawn wait before the current write crosses.
    delay: Option<Pin<Box<tokio::time::Sleep>>>,
    /// The current write already waited.
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
        if self.hub.allow(1).is_none() {
            return self.hub.blocked(cx);
        }
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
        if !self.paced && self.faults.delays() {
            let faults = self.faults;
            let (delay, stalled) = self.draws.delay(&faults, buf.len());
            if stalled {
                self.hub
                    .event(format!("stall {} {}us", self.label, delay.as_micros()));
            }
            self.delay = (!delay.is_zero()).then(|| Box::pin(tokio::time::sleep(delay)));
            self.paced = true;
        }
        if let Some(delay) = &mut self.delay {
            ready!(delay.as_mut().poll(cx));
            self.delay = None;
        }
        let Some(allowed) = self.hub.allow(buf.len()) else {
            return self.hub.blocked(cx).map_ok(|()| 0);
        };
        let allowed = self.severed.allow(allowed);
        if allowed == 0 && !buf.is_empty() {
            return Poll::Ready(Err(cut()));
        }
        let written = Pin::new(&mut self.io).poll_write(cx, &buf[..allowed]);
        if let Poll::Ready(Ok(written)) = &written {
            self.paced = false;
            if self.severed.carry(*written) {
                self.hub.event(format!("cut {}", self.label));
                self.severed.sever();
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
    finder: Option<Arc<Finder>>,
}
/// The hub's discovery: every node announcing under a shared topic is
/// found at once, as on a local network.
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
                return Ok(());
            }
            let hint = |node, name: &str| Found {
                node,
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
            let finder = self.hub.discovery.load(Ordering::SeqCst).then(|| {
                let (sender, found) = mpsc::channel(64);
                Arc::new(Finder {
                    hub: self.hub.clone(),
                    name: self.name.clone(),
                    sender,
                    found: tokio::sync::Mutex::new(found),
                })
            });
            Ok(Arc::new(Node {
                hub: self.hub.clone(),
                name: self.name.clone(),
                inbound: tokio::sync::Mutex::new(receiver),
                listening,
                finder,
            }) as Arc<dyn Transport>)
        })
    }
}
impl Transport for Node {
    fn connectivity(&self) -> Option<tokio::sync::watch::Receiver<()>> {
        Some(self.hub.connectivity.subscribe())
    }
    fn discovery(&self) -> Option<Arc<dyn Discovery>> {
        let finder: Arc<dyn Discovery> = self.finder.clone()?;
        Some(finder)
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
            *self
                .hub
                .peer_dials
                .lock()
                .map_err(|_| "hub poisoned")?
                .entry(address.into())
                .or_default() += 1;
            self.hub.dial_events.send_replace(());
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
            let mut draws = Draws::stream(self.hub.seed, &self.name, address, protocol, ordinal);
            if draws.chance(faults.refusals) {
                self.hub.event(format!("refuse {label}"));
                return Err("refused".into());
            }
            let budget = draws
                .chance(faults.cuts)
                .then(|| draws.below(faults.cut_within as u64) as usize + 1);
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
            let taps = self.hub.taps([&self.name, address]);
            *self
                .hub
                .protocol_dials
                .lock()
                .map_err(|_| "hub poisoned")?
                .entry((self.name.clone(), address.into(), protocol))
                .or_default() += 1;
            let severed = Arc::new(Severed::with_budget(budget));
            self.hub.links.lock().map_err(|_| "hub poisoned")?.push((
                pair(&self.name, address),
                protocol,
                Arc::downgrade(&severed),
            ));
            let mut pipe = |io, from: &str| Pipe {
                messages: Default::default(),
                severed: severed.clone(),
                io: Stream::new(io),
                hub: self.hub.clone(),
                taps: taps.clone(),
                from: from.into(),
                protocol,
                faults,
                draws: draws.fork(),
                label: format!("{label} {from}"),
                delay: None,
                paced: false,
            };
            let (near, far) = (pipe(near, &self.name), pipe(far, address));
            listener
                .try_send((protocol, Stream::new(far)))
                .map_err(|_| "unreachable or busy")?;
            self.hub.event(match budget {
                Some(bytes) => format!("open {label} cut at {bytes}"),
                None => format!("open {label}"),
            });
            Ok(Stream::new(near))
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
            if let Ok(mut announced) = self.hub.announced.lock() {
                announced.remove(&self.name);
            }
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
    fn pipe(&self, stream: Stream, protocol: Protocol) -> Stream {
        let paths = stream.paths();
        let pipe = Pipe {
            messages: Default::default(),
            severed: Default::default(),
            io: stream,
            hub: self.hub.clone(),
            taps: vec![],
            from: self.name.clone(),
            protocol,
            faults: LinkFaults::default(),
            draws: Draws::new(0),
            label: self.name.clone(),
            delay: None,
            paced: false,
        };
        match paths {
            Some(paths) => Stream::with_paths(pipe, paths),
            None => Stream::new(pipe),
        }
    }
}
impl Transport for Wrapped {
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
            let stream = self.transport.connect(address, protocol).await?;
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
                .map(|(protocol, stream)| (protocol, self.pipe(stream, protocol)))
        })
    }
    fn listen(&self, listening: bool) {
        self.transport.listen(listening);
    }
    fn connectivity(&self) -> Option<tokio::sync::watch::Receiver<()>> {
        Some(self.hub.connectivity.subscribe())
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.hub.closes.fetch_add(1, Ordering::SeqCst);
            self.transport.close().await;
        })
    }
    fn forget(&self, address: &str) {
        self.transport.forget(address);
    }
    fn foreground(&self) -> BoxFuture<'_, Result<(), Failure>> {
        self.transport.foreground()
    }
}
