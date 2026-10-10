//! Finding admitted peers on iroh: an mDNS announcement on the local
//! network and a rendezvous record on the address-lookup service, both
//! under names and keys derived from a topic only members hold. Neither
//! touches the endpoint's own lookup services. See
//! `docs/decisions/0057-discovery.md`.
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use data_encoding::{BASE32_NOPAD, BASE64URL_NOPAD};
use futures_util::StreamExt;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey, TransportAddr, Watcher,
    address_lookup::{AddressLookup, EndpointData, PkarrError, PkarrRelayClient, UserData},
};
use iroh_dns::pkarr::SignedPacket;
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use rand::RngCore;
use rocky_core::{
    commitment,
    net::{BoxFuture, Discovery, Failure, Found, LOCAL_PATH, PathState, TOPIC_PATH},
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, mpsc, watch};

/// A member merges its contact into the rendezvous record this often.
pub const RENDEZVOUS_INTERVAL: Duration = Duration::from_secs(300);
/// Contacts the rendezvous record holds at most, newest first.
pub const RENDEZVOUS_CONTACTS: usize = 6;
/// Plaintext bytes of the record; sealed and encoded it fits the lookup
/// service's 1,000-byte packet.
pub const RENDEZVOUS_BYTES: usize = 560;
/// A contact nobody republished for this long leaves the record.
pub const RENDEZVOUS_AGE: u64 = 3600;
/// The record's layout version, its first plaintext byte.
const RECORD_VERSION: u8 = 1;
const RECORD_NAME: &str = "_rocky";
const RECORD_TTL: u32 = 30;
const TXT_CHUNK: usize = 200;
const REQUEST: Duration = Duration::from_secs(15);
const FOUND: usize = 64;

/// The public address-lookup service whose pkarr relay holds the record.
pub const DEFAULT_RENDEZVOUS: &str = iroh::address_lookup::N0_DNS_PKARR_RELAY_PROD;

/// Whether a path was asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Topic {
    #[default]
    Off,
    /// The pkarr relay that holds the rendezvous record.
    On(String),
    /// Asked for but impossible with these options.
    Unavailable(String),
}

/// What an attachment is asked to find peers with. The default asks for
/// nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discover {
    pub topic: Topic,
    /// mDNS on the local network.
    pub local: bool,
}

/// Why local discovery cannot run on this target, if it cannot. A physical
/// iOS device needs the multicast entitlement, which no build carries yet.
pub fn local_unavailable() -> Option<&'static str> {
    if cfg!(all(target_os = "ios", not(target_abi = "sim"))) {
        Some(
            "this build lacks the iOS multicast entitlement; peers are found by topic and contacts",
        )
    } else {
        None
    }
}

fn derive(label: &str, topic: &[u8; 32]) -> [u8; 32] {
    commitment(label, topic)
}
/// The mDNS service members of `topic` announce: 15 characters, as a
/// service name allows.
pub fn service_name(topic: &[u8; 32]) -> String {
    let tag = BASE32_NOPAD.encode(&derive("rocky.discovery-mdns-service.v1", topic));
    format!("rk{}", tag[..13].to_ascii_lowercase())
}
/// The key whose public half names the rendezvous record of `topic`.
pub fn rendezvous_key(topic: &[u8; 32]) -> SecretKey {
    SecretKey::from_bytes(&derive("rocky.discovery-rendezvous-key.v1", topic))
}
fn cipher(label: &str, topic: &[u8; 32]) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new((&derive(label, topic)).into())
}
fn seal(cipher: &ChaCha20Poly1305, plain: &[u8], aad: &[u8]) -> Result<Vec<u8>, Failure> {
    let mut nonce = [0; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let sealed = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
        .map_err(|_| "sealing failed")?;
    Ok([nonce.as_slice(), &sealed].concat())
}
fn open(cipher: &ChaCha20Poly1305, sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    let (nonce, msg) = sealed.split_at_checked(12)?;
    cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg, aad })
        .ok()
}

/// The node key a local announcement carries, sealed to the topic and
/// bound to the announcing endpoint.
fn local_claim(
    topic: &[u8; 32],
    endpoint: EndpointId,
    node: &[u8; 32],
) -> Result<UserData, Failure> {
    let sealed = seal(
        &cipher("rocky.discovery-local-key.v1", topic),
        node,
        endpoint.as_bytes(),
    )?;
    Ok(BASE64URL_NOPAD.encode(&sealed).parse()?)
}
fn local_node(topic: &[u8; 32], endpoint: EndpointId, claim: &str) -> Option<[u8; 32]> {
    let sealed = BASE64URL_NOPAD.decode(claim.as_bytes()).ok()?;
    open(
        &cipher("rocky.discovery-local-key.v1", topic),
        &sealed,
        endpoint.as_bytes(),
    )?
    .try_into()
    .ok()
}

/// One contact of the rendezvous record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub node: [u8; 32],
    pub address: EndpointAddr,
    /// When its owner last merged it, in seconds.
    pub seen: u64,
}
impl Entry {
    /// At most 176 bytes: one relay URL of up to 64 bytes and two sockets.
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend(self.node);
        out.extend(self.address.id.as_bytes());
        out.extend(self.seen.to_le_bytes());
        let relay = self
            .address
            .addrs
            .iter()
            .find_map(|addr| match addr {
                TransportAddr::Relay(url) => Some(url.to_string()),
                _ => None,
            })
            .filter(|url| url.len() <= 64)
            .unwrap_or_default();
        out.push(relay.len() as u8);
        out.extend(relay.as_bytes());
        let sockets: Vec<&SocketAddr> = self
            .address
            .addrs
            .iter()
            .filter_map(|addr| match addr {
                TransportAddr::Ip(socket) => Some(socket),
                _ => None,
            })
            .take(2)
            .collect();
        out.push(sockets.len() as u8);
        for socket in sockets {
            match socket.ip() {
                IpAddr::V4(ip) => {
                    out.push(4);
                    out.extend(ip.octets());
                }
                IpAddr::V6(ip) => {
                    out.push(6);
                    out.extend(ip.octets());
                }
            }
            out.extend(socket.port().to_le_bytes());
        }
    }
    fn decode(bytes: &mut &[u8]) -> Option<Self> {
        fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
            let (head, rest) = bytes.split_at_checked(count)?;
            *bytes = rest;
            Some(head)
        }
        let node = take(bytes, 32)?.try_into().ok()?;
        let id = EndpointId::from_bytes(take(bytes, 32)?.try_into().ok()?).ok()?;
        let seen = u64::from_le_bytes(take(bytes, 8)?.try_into().ok()?);
        let mut address = EndpointAddr::new(id);
        let relay = take(bytes, 1)?[0] as usize;
        if relay > 64 {
            return None;
        }
        let relay = std::str::from_utf8(take(bytes, relay)?).ok()?;
        if !relay.is_empty() {
            address
                .addrs
                .insert(TransportAddr::Relay(relay.parse::<RelayUrl>().ok()?));
        }
        let sockets = take(bytes, 1)?[0];
        if sockets > 2 {
            return None;
        }
        for _ in 0..sockets {
            let ip = match take(bytes, 1)?[0] {
                4 => IpAddr::from(<[u8; 4]>::try_from(take(bytes, 4)?).ok()?),
                6 => IpAddr::from(<[u8; 16]>::try_from(take(bytes, 16)?).ok()?),
                _ => return None,
            };
            let port = u16::from_le_bytes(take(bytes, 2)?.try_into().ok()?);
            address
                .addrs
                .insert(TransportAddr::Ip(SocketAddr::new(ip, port)));
        }
        Some(Self {
            node,
            address,
            seen,
        })
    }
}

/// Puts `own` first, then the other live contacts newest first, as many
/// as the record's bounds hold.
pub(crate) fn merge(own: Entry, held: Vec<Entry>, time: u64) -> Vec<Entry> {
    let mut others: Vec<Entry> = held
        .into_iter()
        .filter(|entry| {
            entry.node != own.node
                && entry.address.id != own.address.id
                && entry.seen.saturating_add(RENDEZVOUS_AGE) >= time
                // A clock ahead of ours must not pin a contact forever.
                && entry.seen <= time.saturating_add(RENDEZVOUS_AGE)
        })
        .collect();
    others.sort_by(|a, b| b.seen.cmp(&a.seen).then(a.node.cmp(&b.node)));
    others.dedup_by(|a, b| a.node == b.node);
    let mut kept = Vec::new();
    let mut size = 1;
    for entry in std::iter::once(own).chain(others) {
        let mut bytes = Vec::new();
        entry.encode(&mut bytes);
        if kept.len() == RENDEZVOUS_CONTACTS || size + bytes.len() > RENDEZVOUS_BYTES {
            break;
        }
        size += bytes.len();
        kept.push(entry);
    }
    kept
}
/// The signed packet holding `entries`, sealed to `topic`.
pub(crate) fn record(topic: &[u8; 32], entries: &[Entry]) -> Result<SignedPacket, Failure> {
    let mut plain = vec![RECORD_VERSION];
    for entry in entries {
        entry.encode(&mut plain);
    }
    if plain.len() > RENDEZVOUS_BYTES {
        return Err("the rendezvous record exceeds its bound".into());
    }
    let key = rendezvous_key(topic);
    let sealed = seal(
        &cipher("rocky.discovery-rendezvous-seal.v1", topic),
        &plain,
        key.public().as_bytes(),
    )?;
    let text = BASE64URL_NOPAD.encode(&sealed);
    let chunks: Vec<String> = text
        .as_bytes()
        .chunks(TXT_CHUNK)
        .enumerate()
        // TXT answers are a set: the index restores their order.
        .map(|(index, chunk)| format!("{index}={}", String::from_utf8_lossy(chunk)))
        .collect();
    Ok(SignedPacket::from_txt_strings(
        &key,
        RECORD_NAME,
        chunks,
        RECORD_TTL,
    )?)
}
/// The contacts of a record of `topic`. A record of another version, or
/// one this topic does not open, holds none.
pub(crate) fn entries(topic: &[u8; 32], packet: &SignedPacket) -> Vec<Entry> {
    let key = rendezvous_key(topic).public();
    if packet.public_key() != key {
        return Vec::new();
    }
    let mut chunks: Vec<(usize, String)> = packet
        .txt_records(RECORD_NAME)
        .into_iter()
        .filter_map(|chunk| {
            let (index, text) = chunk.split_once('=')?;
            Some((index.parse().ok()?, text.to_owned()))
        })
        .collect();
    chunks.sort();
    let text: String = chunks.into_iter().map(|(_, text)| text).collect();
    let Some(plain) = BASE64URL_NOPAD
        .decode(text.as_bytes())
        .ok()
        .and_then(|sealed| {
            open(
                &cipher("rocky.discovery-rendezvous-seal.v1", topic),
                &sealed,
                key.as_bytes(),
            )
        })
    else {
        return Vec::new();
    };
    let Some((&RECORD_VERSION, mut rest)) = plain.split_first() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    while !rest.is_empty() && entries.len() < RENDEZVOUS_CONTACTS {
        match Entry::decode(&mut rest) {
            Some(entry) => entries.push(entry),
            None => return Vec::new(),
        }
    }
    entries
}

/// Whether a lookup failed because the service holds no record.
fn absent(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut cause = Some(error);
    while let Some(error) = cause {
        let pkarr = error.downcast_ref::<PkarrError>().or_else(|| {
            error
                .downcast_ref::<n0_error::AnyError>()
                .and_then(|any| any.downcast_ref::<PkarrError>())
        });
        if let Some(PkarrError::HttpRequest { status, .. }) = pkarr {
            return status.as_u16() == 404;
        }
        cause = error.source();
    }
    false
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

struct Announced {
    node: [u8; 32],
    topics: Vec<[u8; 32]>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
struct Inner {
    endpoint: watch::Receiver<Endpoint>,
    asked: Discover,
    announced: std::sync::Mutex<Option<Announced>>,
    sender: mpsc::Sender<Found>,
    found: Mutex<mpsc::Receiver<Found>>,
    foreground: AtomicBool,
    closed: watch::Sender<bool>,
    /// Why a path that was asked for does not run, by path.
    failed: std::sync::Mutex<[Option<String>; 2]>,
}
/// The discovery of one attachment.
#[derive(Clone)]
pub(crate) struct Finder(Arc<Inner>);
impl Finder {
    pub(crate) fn new(endpoint: watch::Receiver<Endpoint>, asked: Discover) -> Self {
        let (sender, found) = mpsc::channel(FOUND);
        Self(Arc::new(Inner {
            endpoint,
            asked,
            announced: std::sync::Mutex::new(None),
            sender,
            found: Mutex::new(found),
            foreground: AtomicBool::new(true),
            closed: watch::channel(false).0,
            failed: std::sync::Mutex::default(),
        }))
    }
    /// Stops every path for good; the endpoint is its owner's.
    pub(crate) fn close(&self) {
        self.0.closed.send_replace(true);
        self.start(None);
    }
    fn fail(&self, path: usize, reason: Option<String>) {
        self.0.failed.lock().unwrap_or_else(PoisonError::into_inner)[path] = reason;
    }
    fn local_runs(&self) -> bool {
        self.0.asked.local
            && local_unavailable().is_none()
            && self.0.foreground.load(Ordering::SeqCst)
    }
    /// Replaces what is announced and the tasks announcing it.
    fn start(&self, announce: Option<([u8; 32], Vec<[u8; 32]>)>) {
        let mut announced = self
            .0
            .announced
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = announced.take() {
            for task in old.tasks {
                task.abort();
            }
        }
        let Some((node, topics)) = announce.filter(|_| !*self.0.closed.borrow()) else {
            return;
        };
        let mut tasks = Vec::new();
        if self.local_runs() {
            for topic in &topics {
                tasks.push(tokio::spawn(self.clone().local(node, *topic)));
            }
        }
        if let Topic::On(relay) = &self.0.asked.topic
            && !topics.is_empty()
        {
            tasks.push(tokio::spawn(self.clone().rendezvous(
                node,
                topics.clone(),
                relay.clone(),
            )));
        }
        *announced = Some(Announced {
            node,
            topics,
            tasks,
        });
    }
    fn restart(&self) {
        let current = self
            .0
            .announced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|announced| (announced.node, announced.topics.clone()));
        if current.is_some() {
            self.start(current);
        }
    }

    /// Announces this endpoint under the topic's service and reports the
    /// members that answer. A replaced endpoint is announced anew.
    async fn local(self, node: [u8; 32], topic: [u8; 32]) {
        let mut endpoints = self.0.endpoint.clone();
        loop {
            let endpoint = endpoints.borrow_and_update().clone();
            let result = tokio::select! {
                result = self.local_on(&endpoint, node, topic) => result,
                changed = endpoints.changed() => match changed {
                    Ok(()) => continue,
                    Err(_) => return,
                },
            };
            self.fail(0, result.err().map(|error| error.to_string()));
            // The endpoint closed or mDNS failed: wait for a replacement.
            if endpoints.changed().await.is_err() {
                return;
            }
        }
    }
    async fn local_on(
        &self,
        endpoint: &Endpoint,
        node: [u8; 32],
        topic: [u8; 32],
    ) -> Result<(), Failure> {
        let lookup = MdnsAddressLookup::builder()
            .service_name(service_name(&topic))
            .build(endpoint.id())
            .map_err(|error| format!("mDNS could not start: {error}"))?;
        self.fail(0, None);
        let claim = local_claim(&topic, endpoint.id(), &node)?;
        let mut events = lookup.subscribe().await;
        let mut addresses = endpoint.watch_addr();
        loop {
            // Relay URLs stay off the local network: a LAN peer needs none.
            let mut data = EndpointData::from(addresses.get());
            data.clear_relay_urls();
            lookup.publish(&data.with_user_data(claim.clone()));
            tokio::select! {
                changed = addresses.updated() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                event = events.next() => {
                    let Some(event) = event else {
                        return Err("mDNS stopped".into());
                    };
                    let DiscoveryEvent::Discovered { endpoint_info, .. } = event else {
                        continue;
                    };
                    let Some(node) = endpoint_info
                        .user_data()
                        .and_then(|claim| local_node(&topic, endpoint_info.endpoint_id, claim.as_ref()))
                    else {
                        continue;
                    };
                    let address = EndpointAddr::from(endpoint_info.with_user_data(None));
                    self.report(node, &address, LOCAL_PATH, true);
                }
            }
        }
    }
    fn report(&self, node: [u8; 32], address: &EndpointAddr, path: &str, local: bool) {
        if let Ok(address) = serde_json::to_string(address) {
            // A full queue drops the hint; the path reports it again.
            let _ = self.0.sender.try_send(Found {
                node,
                address,
                path: path.into(),
                local,
            });
        }
    }

    /// Reads the record of every topic, reports its contacts and merges
    /// this endpoint's own into it, now and at every interval.
    async fn rendezvous(self, node: [u8; 32], topics: Vec<[u8; 32]>, relay: String) {
        let mut endpoints = self.0.endpoint.clone();
        loop {
            let endpoint = endpoints.borrow_and_update().clone();
            let mut failure = None;
            for topic in &topics {
                let cycle = tokio::time::timeout(
                    REQUEST,
                    self.rendezvous_once(&endpoint, node, topic, &relay),
                );
                match cycle.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => failure = Some(error.to_string()),
                    Err(_) => failure = Some("the lookup service did not answer".into()),
                }
            }
            self.fail(1, failure);
            // Members spread over the interval instead of writing together.
            let jitter = Duration::from_millis(rand::thread_rng().next_u64() % 30_000);
            tokio::select! {
                () = tokio::time::sleep(RENDEZVOUS_INTERVAL + jitter) => {}
                changed = endpoints.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    }
    async fn rendezvous_once(
        &self,
        endpoint: &Endpoint,
        node: [u8; 32],
        topic: &[u8; 32],
        relay: &str,
    ) -> Result<(), Failure> {
        let client = PkarrRelayClient::new(
            relay.parse()?,
            endpoint.tls_config().clone(),
            endpoint.dns_resolver()?.clone(),
        );
        let held = match client.resolve(rendezvous_key(topic).public()).await {
            Ok(packet) => entries(topic, &packet),
            // No member wrote the record yet.
            Err(error) if absent(&error) => Vec::new(),
            // An unread record is not overwritten: the last writer wins.
            Err(error) => {
                return Err(format!("the rendezvous record was not read: {error:#}").into());
            }
        };
        for entry in &held {
            if entry.node != node && entry.address.id != endpoint.id() {
                self.report(entry.node, &entry.address, TOPIC_PATH, false);
            }
        }
        let own = Entry {
            node,
            address: endpoint.addr(),
            seen: now(),
        };
        let packet = record(topic, &merge(own, held, now()))?;
        client
            .publish(&packet)
            .await
            .map_err(|error| format!("the rendezvous record was not written: {error}"))?;
        Ok(())
    }
}
impl Discovery for Finder {
    fn announce<'a>(
        &'a self,
        node: [u8; 32],
        topics: &'a [[u8; 32]],
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            if *self.0.closed.borrow() {
                return Err("the iroh attachment is closed".into());
            }
            let unchanged = self
                .0
                .announced
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .is_some_and(|held| held.node == node && held.topics == topics);
            if !unchanged {
                self.start((!topics.is_empty()).then(|| (node, topics.to_vec())));
            }
            Ok(())
        })
    }
    fn found(&self) -> BoxFuture<'_, Option<Found>> {
        Box::pin(async {
            let mut closed = self.0.closed.subscribe();
            let mut found = self.0.found.lock().await;
            tokio::select! {
                found = found.recv() => found,
                _ = closed.wait_for(|closed| *closed) => None,
            }
        })
    }
    fn paths(&self) -> Vec<(String, PathState)> {
        let failed = self
            .0
            .failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let closed = *self.0.closed.borrow();
        let state = |asked: bool, failed: &Option<String>| match failed {
            _ if !asked || closed => PathState::Off,
            Some(reason) => PathState::Unavailable(reason.clone()),
            None => PathState::On,
        };
        let local = match local_unavailable() {
            Some(reason) if self.0.asked.local => PathState::Unavailable(reason.into()),
            _ => state(self.local_runs(), &failed[0]),
        };
        let topic = match &self.0.asked.topic {
            Topic::Unavailable(reason) => PathState::Unavailable(reason.clone()),
            Topic::Off => PathState::Off,
            Topic::On(_) => state(true, &failed[1]),
        };
        vec![(LOCAL_PATH.into(), local), (TOPIC_PATH.into(), topic)]
    }
    fn foreground(&self, foreground: bool) {
        if self.0.foreground.swap(foreground, Ordering::SeqCst) != foreground {
            self.fail(0, None);
            self.restart();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(node: u8, seen: u64) -> Entry {
        let mut address = EndpointAddr::new(SecretKey::from_bytes(&[node; 32]).public());
        address.addrs.insert(TransportAddr::Ip(
            "192.168.1.20:4433"
                .parse()
                .unwrap_or_else(|_| unreachable!()),
        ));
        if let Ok(relay) = "https://relay.example.org./".parse::<RelayUrl>() {
            address.addrs.insert(TransportAddr::Relay(relay));
        }
        Entry {
            node: [node; 32],
            address,
            seen,
        }
    }

    #[test]
    fn a_record_opens_only_under_its_topic_and_keeps_the_newest_few() -> Result<(), Failure> {
        let topic = [7; 32];
        let held: Vec<Entry> = (1..=9)
            .map(|node| entry(node, 1000 + node as u64))
            .collect();
        // The byte bound holds five contacts that name a relay.
        let merged = merge(entry(20, 2000), held.clone(), 2000);
        assert_eq!(
            merged.iter().map(|entry| entry.node[0]).collect::<Vec<_>>(),
            [20, 9, 8, 7, 6]
        );
        // The count bound holds six smaller ones.
        let direct = |mut entry: Entry| {
            entry.address.addrs.retain(|addr| !addr.is_relay());
            entry
        };
        let small = merge(
            direct(entry(20, 2000)),
            held.into_iter().map(direct).collect(),
            2000,
        );
        assert_eq!(small.len(), RENDEZVOUS_CONTACTS);
        let packet = record(&topic, &merged)?;
        assert!(packet.as_bytes().len() <= SignedPacket::MAX_BYTES);
        assert_eq!(entries(&topic, &packet), merged);
        // Another topic neither names nor opens the record.
        assert!(entries(&[8; 32], &packet).is_empty());
        assert_ne!(
            rendezvous_key(&topic).public(),
            rendezvous_key(&[8; 32]).public()
        );
        // The record names no member in the clear.
        let text = packet.txt_records(RECORD_NAME).concat();
        assert!(!text.contains("relay.example.org"));
        Ok(())
    }

    #[test]
    fn a_merge_replaces_the_own_contact_and_drops_the_stale() {
        let held = vec![entry(1, 100), entry(2, 5000), entry(3, 4000)];
        let merged = merge(entry(3, 5000), held, 5000);
        assert_eq!(
            merged
                .iter()
                .map(|entry| (entry.node[0], entry.seen))
                .collect::<Vec<_>>(),
            [(3, 5000), (2, 5000)]
        );
    }

    #[test]
    fn the_largest_contacts_still_fit_the_packet() -> Result<(), Failure> {
        let mut large = entry(1, u64::MAX / 2);
        large.address.addrs.clear();
        let url = format!("https://{}.example.org./", "r".repeat(40));
        assert!(url.len() <= 64);
        large
            .address
            .addrs
            .insert(TransportAddr::Relay(url.parse::<RelayUrl>()?));
        for port in [1, 2] {
            large
                .address
                .addrs
                .insert(TransportAddr::Ip(SocketAddr::new(
                    IpAddr::from([0xfe; 16]),
                    port,
                )));
        }
        let held = (2..9)
            .map(|node| Entry {
                node: [node; 32],
                address: EndpointAddr {
                    id: SecretKey::from_bytes(&[node; 32]).public(),
                    ..large.address.clone()
                },
                seen: large.seen,
            })
            .collect();
        let merged = merge(large.clone(), held, large.seen);
        assert!(merged.len() >= 3);
        let packet = record(&[1; 32], &merged)?;
        assert_eq!(entries(&[1; 32], &packet), merged);
        Ok(())
    }

    #[test]
    fn a_local_claim_opens_only_for_its_topic_and_endpoint() -> Result<(), Failure> {
        let endpoint = SecretKey::from_bytes(&[1; 32]).public();
        let other = SecretKey::from_bytes(&[2; 32]).public();
        let claim = local_claim(&[5; 32], endpoint, &[9; 32])?;
        assert_eq!(
            local_node(&[5; 32], endpoint, claim.as_ref()),
            Some([9; 32])
        );
        assert_eq!(local_node(&[6; 32], endpoint, claim.as_ref()), None);
        assert_eq!(local_node(&[5; 32], other, claim.as_ref()), None);
        let name = service_name(&[5; 32]);
        assert_eq!(name.len(), 15);
        assert_ne!(name, service_name(&[6; 32]));
        Ok(())
    }
}
