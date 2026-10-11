//! Network-service-owned links, signals and liveness.
use super::*;
use rocky_secure::network::link::{self, CloseReason, Frame, Message};
use std::collections::BTreeSet;
mod liveness;

pub(super) const BUDGET: usize = 32;
const PING: Duration = Duration::from_secs(15);

struct Entry {
    id: u64,
    dialed: bool,
    held: Vec<ResourceScope>,
    close: watch::Sender<Option<CloseReason>>,
    changed: watch::Sender<Option<Vec<ResourceScope>>>,
    frontiers: BTreeMap<ResourceScope, [u8; 32]>,
}
pub(super) struct Registry {
    entries: std::sync::Mutex<BTreeMap<PublicKey, Entry>>,
    next: AtomicU64,
    events: watch::Sender<()>,
    #[cfg(test)]
    pub pongs: AtomicU64,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            entries: Default::default(),
            next: AtomicU64::new(0),
            events: watch::channel(()).0,
            #[cfg(test)]
            pongs: AtomicU64::new(0),
        }
    }
}
impl Registry {
    pub async fn wait_empty(&self) {
        let mut events = self.events.subscribe();
        loop {
            events.borrow_and_update();
            if self
                .entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
            {
                return;
            }
            if events.changed().await.is_err() {
                return;
            }
        }
    }
    pub fn close(&self, reason: CloseReason) {
        for entry in self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
        {
            entry.close.send_replace(Some(reason));
        }
    }
    pub fn contains(&self, peer: PublicKey) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&peer)
    }
    #[cfg(test)]
    pub fn dialed(&self, peer: PublicKey) -> Option<bool> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .map(|entry| entry.dialed)
    }
    #[cfg(test)]
    pub fn id(&self, peer: PublicKey) -> Option<u64> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .map(|entry| entry.id)
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}
impl Node {
    pub(super) async fn publish_links(self: Arc<Self>, service: Arc<Link>) {
        let mut stage = service.stage.subscribe();
        let mut frames = self.replica.observe_frames().await;
        let mut revisions = self.replica.observe_revision();
        loop {
            tokio::select! {
                _ = stage.wait_for(|stage| *stage != Stage::Open) => break,
                changed = frames.changed() => { if changed.is_err() { break; } },
                changed = revisions.changed() => { if changed.is_err() { break; } },
            }
            settle().await;
            if self.automatic.load(Ordering::SeqCst)
                && self.online.load(Ordering::SeqCst)
                && let Err(error) = self.signal_links(&service).await
            {
                self.activity
                    .record_changed("link_signal_failed", json!({"error":error.to_string()}));
            }
        }
    }
    pub(super) async fn open_links(self: &Arc<Self>, service: &Arc<Link>) {
        if !self.automatic.load(Ordering::SeqCst) || !self.online.load(Ordering::SeqCst) {
            return;
        }
        let settings = self.settings.lock().await.clone();
        let targets = self.dialed(&settings);
        let attempts = stream::iter(targets)
            .map(|(peer, contact)| {
                let node = self.clone();
                let service = service.clone();
                let settings = settings.clone();
                async move {
                    if service.live.contains(peer)
                        || !node
                            .retries
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get(&peer)
                            .is_none_or(|retry| {
                                retry.link_retry.ready() && (retry.link || retry.retry.ready())
                            })
                    {
                        return;
                    }
                    let result = async {
                        let stream = crate::enrollment::dial(
                            &*service.transport,
                            &contact.addresses(service.transport.kind()),
                            Protocol::Link,
                        )
                        .await?;
                        node.admit_link(service.clone(), stream, Some(peer), settings, None)
                            .await
                    }
                    .await;
                    if let Err(error) = result {
                        {
                            let mut retries =
                                node.retries.lock().unwrap_or_else(PoisonError::into_inner);
                            let state = retries.entry(peer).or_default();
                            state.link = true;
                            state.link_retry.failed();
                        }
                        let _ = node.absorb_revocations(Some(&error)).await;
                        if node.learned.load(Ordering::SeqCst) {
                            node.wake_sync();
                        }
                        // Busy or an unavailable link retains ordinary sync for this pair.
                        node.activity.record_changed(
                            "link_unavailable",
                            json!({"peer":peer.to_hex(),"error":error.to_string()}),
                        );
                    }
                }
            })
            .buffer_unordered(4);
        tokio::pin!(attempts);
        while attempts.next().await.is_some() {}
    }
    pub(super) async fn admit_link(
        self: &Arc<Self>,
        service: Arc<Link>,
        stream: Stream,
        expected: Option<PublicKey>,
        settings: Settings,
        handshake_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<(), Failure> {
        if !self.automatic.load(Ordering::SeqCst)
            || !self.online.load(Ordering::SeqCst)
            || *service.stage.borrow() != Stage::Open
        {
            return Err("live links are disabled".into());
        }
        let mut authority = self.replica.observe_authority().await;
        let local = PrivateKey::from_bytes(&self.vault.get("node")?);
        let own = local.public_key();
        let mut opened =
            link::Link::open(stream, &self.replica, local, settings.credentials, expected).await?;
        if !self.automatic.load(Ordering::SeqCst)
            || !self.online.load(Ordering::SeqCst)
            || *service.stage.borrow() != Stage::Open
            || authority.has_changed()?
        {
            return Err("link authority changed during handshake".into());
        }
        authority.borrow_and_update();
        drop(handshake_permit);
        let permit = if expected.is_none() {
            match service.live_budget.clone().try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    service.refused.fetch_add(1, Ordering::Relaxed);
                    let _ = opened
                        .writer
                        .send(Frame::new(
                            opened.direction,
                            Message::Close {
                                reason: CloseReason::Busy,
                            },
                        ))
                        .await;
                    let _ = opened.writer.finish().await;
                    return Err("live link budget is busy".into());
                }
            }
        } else {
            None
        };
        let peer = opened.peer;
        let dialed = expected.is_some();
        let preferred = own < peer;
        let id = service.live.next.fetch_add(1, Ordering::Relaxed);
        let close = watch::channel(None).0;
        let changed = watch::channel(None).0;
        let mut frontiers = BTreeMap::new();
        for scope in &opened.held {
            frontiers.insert(*scope, self.link_scope_state(*scope).await?);
        }
        let initial: Vec<_> = frontiers
            .iter()
            .filter(|(scope, token)| opened.states.get(scope) != Some(token))
            .map(|(scope, _)| *scope)
            .take(link::SIGNAL_SCOPES)
            .collect();
        if !initial.is_empty() {
            changed.send_replace(Some(initial));
        }
        let accepted = {
            self.replica.flush().await?;
            let mut entries = service
                .live
                .entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if entries
                .get(&peer)
                .is_some_and(|old| old.dialed == preferred || old.dialed == dialed)
            {
                false
            } else {
                if let Some(old) = entries.insert(
                    peer,
                    Entry {
                        id,
                        dialed,
                        held: opened.held.clone(),
                        close: close.clone(),
                        changed: changed.clone(),
                        frontiers,
                    },
                ) {
                    old.close.send_replace(Some(CloseReason::Duplicate));
                }
                true
            }
        };
        if !accepted {
            let _ = opened
                .writer
                .send(Frame::new(
                    opened.direction,
                    Message::Close {
                        reason: CloseReason::Duplicate,
                    },
                ))
                .await;
            let _ = opened.writer.finish().await;
            return Ok(());
        }
        {
            let mut met = self.met.lock().unwrap_or_else(PoisonError::into_inner);
            met.insert(peer, now()?);
            if met.len() > MET
                && let Some(oldest) = met.iter().min_by_key(|(_, at)| **at).map(|(peer, _)| *peer)
            {
                met.remove(&oldest);
            }
            let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
            let state = retries.entry(peer).or_default();
            state.link = false;
            state.link_retry.reset();
            state.retry.reset();
            if state.pending {
                self.link_events.send_replace(());
            }
        }
        if let Ok(mut reached) = self.reached()
            && let Some(last) = reached.get_mut(&peer)
        {
            last.error = None;
        }
        service.live.events.send_replace(());
        self.activity
            .record("link_opened", json!({"peer":peer.to_hex()}));
        let node = self.clone();
        // The stage watch owns cancellation even for outbound links.
        let close_rx = close.subscribe();
        let changed_tx = changed.clone();
        tokio::task::spawn_local(async move {
            let _permit = permit;
            let result = node
                .run_link(&service, opened, close_rx, changed_tx, authority)
                .await;
            let removed = {
                let mut entries = service
                    .live
                    .entries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if entries.get(&peer).is_some_and(|entry| entry.id == id) {
                    entries.remove(&peer);
                    true
                } else {
                    false
                }
            };
            if removed {
                service.live.events.send_replace(());
                node.activity.record("link_closed", json!({"peer":peer.to_hex(),"error":result.as_ref().err().map(ToString::to_string)}));
                if let Err(error) = &result
                    && *service.stage.borrow() == Stage::Open
                {
                    if let Ok(mut reached) = node.reached() {
                        let last = reached.entry(peer).or_insert_with(|| Reached {
                            at: now().unwrap_or_default(),
                            error: None,
                            refused: vec![],
                            held: vec![],
                            results: None,
                        });
                        last.error = Some(error.to_string());
                    }
                    let mut retries = node.retries.lock().unwrap_or_else(PoisonError::into_inner);
                    let state = retries.entry(peer).or_default();
                    state.link = true;
                    state.link_retry.failed();
                    drop(retries);
                    node.link_events.send_replace(());
                }
            }
        });
        Ok(())
    }
    pub(super) async fn signal_links(&self, service: &Link) -> Result<(), Failure> {
        let holdings: Vec<_> = service
            .live
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(peer, entry)| (*peer, entry.id, entry.held.clone()))
            .collect();
        for (peer, id, held) in holdings {
            let mut frontiers = BTreeMap::new();
            for scope in held {
                frontiers.insert(scope, self.link_scope_state(scope).await?);
            }
            self.replica.flush().await?;
            let mut entries = service
                .live
                .entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let Some(entry) = entries.get_mut(&peer).filter(|entry| entry.id == id) else {
                continue;
            };
            let scopes: Vec<_> = frontiers
                .iter()
                .filter(|(scope, bytes)| entry.frontiers.get(scope) != Some(bytes))
                .map(|(scope, _)| *scope)
                .collect();
            if !scopes.is_empty() {
                // Retain all changed scopes until the writer takes its coalesced value.
                let mut combined: BTreeSet<_> = scopes.into_iter().collect();
                if let Some(pending) = entry.changed.borrow().as_ref() {
                    combined.extend(pending);
                }
                entry.changed.send_replace(Some(
                    combined.into_iter().take(link::SIGNAL_SCOPES).collect(),
                ));
                entry.frontiers = frontiers;
            }
        }
        Ok(())
    }
    async fn link_scope_state(&self, scope: ResourceScope) -> Result<[u8; 32], Failure> {
        Ok(link::scope_token(&self.replica, scope).await?)
    }
    async fn signal_token(&self, scopes: &[ResourceScope]) -> Result<[u8; 32], Failure> {
        let mut state = Vec::new();
        for scope in scopes {
            state.push((*scope, self.link_scope_state(*scope).await?));
        }
        self.replica.flush().await?;
        Ok(commitment(
            "rocky.link-batch.v1",
            &serde_json::to_vec(&state)?,
        ))
    }
    fn reopen_link(&self, peer: PublicKey) {
        self.reset_retry(Some(peer));
        self.retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(peer)
            .or_default()
            .link = true;
        self.link_events.send_replace(());
    }
    fn link_pending(&self, peer: PublicKey) {
        self.reset_retry(Some(peer));
        let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        retries.entry(peer).or_default().pending = true;
        drop(retries);
        self.link_events.send_replace(());
    }
}
