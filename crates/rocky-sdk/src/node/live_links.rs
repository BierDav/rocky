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
    persistent: bool,
    handle: link::channels::Handle,
    authority: watch::Receiver<u64>,
    close: watch::Sender<Option<CloseReason>>,
    changed: watch::Sender<Option<Vec<ResourceScope>>>,
    frontiers: BTreeMap<ResourceScope, [u8; 32]>,
    cursors: BTreeMap<ResourceScope, rocky_secure::ScopeFrontier>,
    auxiliary: BTreeMap<ResourceScope, [u8; 32]>,
}
pub(super) struct Registry {
    pub(super) publishing: Mutex<()>,
    entries: std::sync::Mutex<BTreeMap<PublicKey, Entry>>,
    next: AtomicU64,
    events: watch::Sender<()>,
    #[cfg(test)]
    pub pongs: AtomicU64,
    #[cfg(test)]
    pub closing: AtomicU64,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            publishing: Mutex::new(()),
            entries: Default::default(),
            next: AtomicU64::new(0),
            events: watch::channel(()).0,
            #[cfg(test)]
            pongs: AtomicU64::new(0),
            #[cfg(test)]
            closing: AtomicU64::new(0),
        }
    }
}
impl Registry {
    pub async fn wait_inbound_short(&self, peer: PublicKey) -> Result<(), Failure> {
        let mut events = self.events.subscribe();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                events.borrow_and_update();
                if !self.inbound_short(peer) {
                    return Ok(());
                }
                events.changed().await.map_err(|_| "link registry ended")?;
            }
        })
        .await
        .map_err(|_| "incoming short reconcile did not finish")?
    }

    pub fn inbound_short(&self, peer: PublicKey) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .is_some_and(|entry| !entry.persistent && !entry.dialed)
    }

    pub fn close_peer(&self, peer: PublicKey) {
        if let Some(entry) = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
        {
            entry.close.send_replace(Some(CloseReason::Detached));
        }
    }

    pub fn close_short(&self, peer: PublicKey) {
        if let Some(entry) = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            && !entry.persistent
        {
            entry.close.send_replace(Some(CloseReason::Detached));
        }
    }

    pub fn handle(&self, peer: PublicKey) -> Option<link::channels::Handle> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .filter(|entry| {
                entry.close.borrow().is_none()
                    && !entry.handle.is_closed()
                    && !entry.authority.has_changed().unwrap_or(true)
            })
            .map(|entry| entry.handle.clone())
    }
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
            .get(&peer)
            .is_some_and(|entry| {
                entry.close.borrow().is_none()
                    && !entry.handle.is_closed()
                    && !entry.authority.has_changed().unwrap_or(true)
            })
    }
    #[cfg(test)]
    pub fn dialed(&self, peer: PublicKey) -> Option<bool> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .map(|entry| entry.dialed)
    }
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
                                !retry.short
                                    && retry.link_retry.ready()
                                    && (retry.link || retry.retry.ready())
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
                        // Reconciliation retries the link; only busy refusal permits native sync.
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
        if !self.online.load(Ordering::SeqCst) || *service.stage.borrow() != Stage::Open {
            return Err("live links are disabled".into());
        }
        let mut authority = self.replica.observe_authority().await;
        let local = PrivateKey::from_bytes(&self.vault.get("node")?);
        let own = local.public_key();
        let opened = match expected {
            Some(_) => Box::pin(link::Link::open(
                stream,
                &self.replica,
                local,
                settings.credentials,
                expected,
            ))
            .await
            .map(Some),
            // An inbound stream may only resolve a pinned contact reference.
            None => {
                Box::pin(link::Link::accept_or_resolve(
                    stream,
                    &self.replica,
                    local,
                    settings.credentials,
                ))
                .await
            }
        };
        let mut opened = match opened {
            Err(_) if expected.is_some_and(|peer| service.live.contains(peer)) => return Ok(()),
            other => match other? {
                Some(opened) => opened,
                None => return Ok(()),
            },
        };
        if !self.online.load(Ordering::SeqCst)
            || *service.stage.borrow() != Stage::Open
            || authority.has_changed()?
        {
            let _ = opened
                .writer
                .send(Frame::new(
                    opened.direction,
                    Message::Close {
                        reason: CloseReason::Authority,
                    },
                ))
                .await;
            let _ = opened.writer.finish().await;
            return Err(Box::new(rocky_secure::Error::Application(Box::new(
                link::AuthorityChanged,
            ))));
        }
        authority.borrow_and_update();
        self.absorb_revocations(None).await?;
        if authority.has_changed()? {
            let _ = opened
                .writer
                .send(Frame::new(
                    opened.direction,
                    Message::Close {
                        reason: CloseReason::Authority,
                    },
                ))
                .await;
            let _ = opened.writer.finish().await;
            return Err(Box::new(rocky_secure::Error::Application(Box::new(
                link::AuthorityChanged,
            ))));
        }
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
        let persistent = match opened.ready(self.automatic.load(Ordering::SeqCst)).await {
            Err(_) if expected.is_some_and(|peer| service.live.contains(peer)) => return Ok(()),
            other => other?,
        };
        let peer = opened.peer;
        let dialed = expected.is_some();
        let preferred = own < peer;
        let id = service.live.next.fetch_add(1, Ordering::Relaxed);
        let close = watch::channel(None).0;
        let changed = watch::channel(None).0;
        let mut frontiers = BTreeMap::new();
        let mut cursors = BTreeMap::new();
        let mut auxiliary = BTreeMap::new();
        for scope in &opened.held {
            cursors.insert(*scope, self.replica.frontier(*scope).await?);
            auxiliary.insert(*scope, self.link_auxiliary(*scope).await?);
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
            if entries.get(&peer).is_some_and(|old| {
                (old.dialed == preferred || old.dialed == dialed)
                    && !old.handle.is_closed()
                    && old.close.borrow().is_none()
                    && !old.authority.has_changed().unwrap_or(true)
            }) {
                false
            } else {
                if let Some(old) = entries.insert(
                    peer,
                    Entry {
                        id,
                        dialed,
                        persistent,
                        handle: opened.channels.handle.clone(),
                        authority: authority.clone(),
                        close: close.clone(),
                        changed: changed.clone(),
                        frontiers,
                        cursors,
                        auxiliary,
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
        if persistent || dialed {
            self.inbound_sync(peer);
        } else {
            self.reset_retry(Some(peer));
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
            state.short = !persistent;
            state.link_retry.reset();
            state.retry.reset();
            state.pending |= persistent || dialed;
            if state.pending && (persistent || dialed) {
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
                if result.is_ok() && !persistent && !dialed && node.automatic.load(Ordering::SeqCst)
                {
                    node.link_pending(peer);
                }
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
                    state.link = persistent;
                    if persistent {
                        state.link_retry.failed();
                    } else {
                        state.pending = true;
                        state.retry.failed();
                    }
                    drop(retries);
                    node.link_events.send_replace(());
                }
            }
        });
        Ok(())
    }
    pub(super) async fn signal_links(&self, service: &Link) -> Result<(), Failure> {
        let _publishing = service.live.publishing.lock().await;
        let holdings: Vec<_> = service
            .live
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(peer, entry)| {
                (
                    *peer,
                    entry.id,
                    entry.handle.clone(),
                    entry.frontiers.clone(),
                    entry.cursors.clone(),
                    entry.auxiliary.clone(),
                )
            })
            .collect();
        for (peer, id, handle, old, cursors, auxiliary) in holdings {
            let mut frontiers = old.clone();
            let mut next = cursors.clone();
            let mut aux = auxiliary.clone();
            let mut scopes = BTreeSet::new();
            for (scope, cursor) in &cursors {
                let token = self.link_scope_state(*scope).await?;
                if old.get(scope) == Some(&token) {
                    continue;
                }
                // Capture the cursor before export; a later commit must remain pending.
                let frontier = self.replica.frontier(*scope).await?;
                let metadata = self.link_auxiliary(*scope).await?;
                let rows_changed = frontier != *cursor;
                let pushed = rows_changed && handle.push_committed(&self.replica, cursor).await?;
                if !pushed || auxiliary.get(scope) != Some(&metadata) {
                    scopes.insert(*scope);
                }
                frontiers.insert(*scope, token);
                next.insert(*scope, frontier);
                aux.insert(*scope, metadata);
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
            entry.frontiers = frontiers;
            entry.cursors = next;
            entry.auxiliary = aux;
            if !scopes.is_empty() {
                if let Some(pending) = entry.changed.borrow().as_ref() {
                    scopes.extend(pending);
                }
                // Unsent scopes keep their old token so the next publication can send them.
                for scope in scopes.iter().skip(link::SIGNAL_SCOPES) {
                    if let Some(token) = old.get(scope) {
                        entry.frontiers.insert(*scope, *token);
                    }
                }
                entry
                    .changed
                    .send_replace(Some(scopes.into_iter().take(link::SIGNAL_SCOPES).collect()));
            }
        }
        Ok(())
    }
    async fn received_push(
        &self,
        service: &Link,
        peer: PublicKey,
        scope: ResourceScope,
        received: Vec<(PublicKey, u64)>,
    ) -> Result<(), Failure> {
        let frontier = self.replica.frontier(scope).await?;
        let token = self.link_scope_state(scope).await?;
        let metadata = self.link_auxiliary(scope).await?;
        let mut entries = service
            .live
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = entries.get_mut(&peer) else {
            return Ok(());
        };
        let Some(cursor) = entry.cursors.get_mut(&scope) else {
            return Ok(());
        };
        for (author, sequence) in received {
            if let Some(log) = frontier.log(author) {
                let mut shared = *log;
                shared.held = shared.held.min(sequence);
                shared.applied = shared.applied.min(sequence);
                if let Some(held) = cursor.logs.iter_mut().find(|held| held.author == author) {
                    held.held = held.held.max(shared.held);
                    held.applied = held.applied.max(shared.applied);
                } else {
                    cursor.logs.push(shared);
                }
            }
        }
        cursor.logs.sort_by_key(|log| log.author);
        if *cursor == frontier && entry.auxiliary.get(&scope) == Some(&metadata) {
            entry.frontiers.insert(scope, token);
        }
        Ok(())
    }
    async fn link_auxiliary(&self, scope: ResourceScope) -> Result<[u8; 32], Failure> {
        Ok(commitment(
            "rocky.link-auxiliary.v1",
            &rocky_credentials::encode(&(
                self.replica.known_result_head(scope.domain).await?,
                self.replica.admitted_horizon(scope).await?,
                self.replica.finalized_horizon(scope).await?,
            ))?,
        ))
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
