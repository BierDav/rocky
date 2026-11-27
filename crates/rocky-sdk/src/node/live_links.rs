use super::*;
use rocky_secure::network::link::{self, CloseReason, Frame, Message};
use std::collections::BTreeSet;
pub(super) mod io;
mod liveness;
mod scope_reading;
#[cfg(test)]
pub(super) use scope_reading::ScopeReadGate;
pub(super) use scope_reading::{ScopeReadSite, ScopeReading, ScopeToken};

pub(super) const BUDGET: usize = 32;

#[derive(Clone)]
struct ChangeHint {
    scopes: Vec<ResourceScope>,
    control: bool,
}
struct Entry {
    opened_at: u64,
    id: u64,
    dialed: bool,
    persistent: bool,
    shorts: usize,
    receiving: usize,
    publishing: bool,
    republish: bool,
    cursor_generation: u64,
    control_generation: Option<u64>,
    handle: link::channels::Handle,
    authority: watch::Receiver<u64>,
    close: watch::Sender<Option<CloseReason>>,
    changed: watch::Sender<Option<ChangeHint>>,
    tokens: BTreeMap<ResourceScope, ScopeToken>,
    cursors: BTreeMap<ResourceScope, rocky_secure::ScopeFrontier>,
}
impl Entry {
    fn usable(&self) -> bool {
        self.close.borrow().is_none()
            && !self.handle.is_closed()
            && !self.authority.has_changed().unwrap_or(true)
    }
}
pub(super) struct Registry {
    #[cfg(test)]
    pub(super) publishing: Mutex<()>,
    entries: std::sync::Mutex<BTreeMap<PublicKey, Entry>>,
    opening: std::sync::Mutex<BTreeMap<PublicKey, usize>>,
    next: AtomicU64,
    events: watch::Sender<()>,
    #[cfg(test)]
    pub closing: AtomicU64,
    #[cfg(test)]
    pub(super) authority_close_gate: std::sync::Mutex<Option<Arc<Semaphore>>>,
    #[cfg(test)]
    pub(super) push_gate: std::sync::Mutex<Option<Arc<Semaphore>>>,
    #[cfg(test)]
    scope_read_gate: std::sync::Mutex<Option<Arc<ScopeReadGate>>>,
    #[cfg(test)]
    pub(super) dropped_pushes: AtomicU64,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            #[cfg(test)]
            publishing: Mutex::new(()),
            entries: Default::default(),
            opening: Default::default(),
            next: AtomicU64::new(0),
            events: watch::channel(()).0,
            #[cfg(test)]
            closing: AtomicU64::new(0),
            #[cfg(test)]
            authority_close_gate: Default::default(),
            #[cfg(test)]
            push_gate: Default::default(),
            #[cfg(test)]
            scope_read_gate: Default::default(),
            #[cfg(test)]
            dropped_pushes: AtomicU64::new(0),
        }
    }
}
pub(super) struct Opening<'a> {
    node: &'a Node,
    service: &'a Link,
    peer: PublicKey,
}
impl Drop for Opening<'_> {
    fn drop(&mut self) {
        let mut opening = self
            .service
            .live
            .opening
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = opening.get_mut(&self.peer) {
            *count -= 1;
            if *count == 0 {
                opening.remove(&self.peer);
            }
        }
        drop(opening);
        self.node.link_status_changed();
    }
}
pub(super) struct ShortLink<'a> {
    registry: &'a Registry,
    peer: PublicKey,
    id: Option<u64>,
}
impl Drop for ShortLink<'_> {
    fn drop(&mut self) {
        self.registry.close_short(self.peer, self.id);
    }
}
struct Claim<'a> {
    registry: &'a Registry,
    peer: PublicKey,
    id: u64,
    held: bool,
}
impl Claim<'_> {
    fn rerun(&mut self) -> bool {
        let mut entries = self
            .registry
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = entries
            .get_mut(&self.peer)
            .filter(|entry| entry.id == self.id)
        else {
            self.held = false;
            return false;
        };
        if entry.republish && entry.receiving == 0 {
            entry.republish = false;
            return true;
        }
        entry.publishing = false;
        entry.republish = false;
        self.held = false;
        false
    }
}
impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if !self.held {
            return;
        }
        let mut entries = self
            .registry
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = entries
            .get_mut(&self.peer)
            .filter(|entry| entry.id == self.id)
        {
            entry.publishing = false;
            entry.republish = false;
        }
    }
}
struct Receiving<'a> {
    registry: &'a Registry,
    peer: PublicKey,
    id: Option<u64>,
}
impl Drop for Receiving<'_> {
    fn drop(&mut self) {
        let mut entries = self
            .registry
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = entries
            .get_mut(&self.peer)
            .filter(|entry| Some(entry.id) == self.id)
        {
            entry.receiving = entry.receiving.saturating_sub(1);
        }
        drop(entries);
        self.registry.events.send_replace(());
    }
}
impl Registry {
    #[cfg(test)]
    pub(super) fn traffic(&self) -> Vec<link::channels::Traffic> {
        let handles: Vec<_> = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|entry| entry.handle.clone())
            .collect();
        handles.into_iter().map(|handle| handle.traffic()).collect()
    }
    #[cfg(test)]
    pub(super) fn publishing_or_receiving(&self) -> bool {
        self.publishing.try_lock().is_err()
            || self
                .entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .values()
                .any(|entry| entry.publishing || entry.receiving != 0)
    }
    fn claim_publications(&self) -> Vec<(PublicKey, u64)> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .iter_mut()
            .filter(|(_, entry)| entry.receiving == 0)
            .filter_map(|(peer, entry)| {
                if entry.publishing {
                    entry.republish = true;
                    return None;
                }
                entry.publishing = true;
                Some((*peer, entry.id))
            })
            .collect()
    }

    fn receiving(&self, peer: PublicKey) -> Receiving<'_> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let id = entries.get_mut(&peer).map(|entry| {
            entry.receiving += 1;
            entry.id
        });
        Receiving {
            registry: self,
            peer,
            id,
        }
    }
    pub(super) fn begin_control(
        &self,
        peer: PublicKey,
        generation: u64,
        scoped: bool,
    ) -> (Option<u64>, bool) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match entries.get_mut(&peer) {
            Some(entry) => {
                let previous = entry.control_generation.take();
                (Some(entry.id), !scoped || previous != Some(generation))
            }
            None => (None, true),
        }
    }
    pub(super) fn finish_control(&self, peer: PublicKey, id: Option<u64>, generation: u64) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = entries.get_mut(&peer).filter(|entry| Some(entry.id) == id) {
            entry.control_generation = Some(generation);
        }
    }
    pub fn nodes(&self) -> Vec<PublicKey> {
        let mut nodes: Vec<_> = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .copied()
            .collect();
        nodes.extend(
            self.opening
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .keys()
                .copied(),
        );
        nodes
    }
    pub fn status(&self, peer: PublicKey) -> crate::PeerLink {
        let source = {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            entries.get(&peer).map(|entry| {
                (
                    entry.opened_at,
                    entry.close.borrow().is_some()
                        || entry.handle.is_closed()
                        || entry.authority.has_changed().unwrap_or(true),
                )
            })
        };
        if let Some((opened, closing)) = source {
            return crate::PeerLink {
                state: if closing {
                    crate::LinkState::Closing
                } else {
                    crate::LinkState::Open
                },
                opened_at_epoch_seconds: Some(opened),
            };
        }
        let opening = self.opening.lock().unwrap_or_else(PoisonError::into_inner);
        crate::PeerLink {
            state: if opening.contains_key(&peer) {
                crate::LinkState::Opening
            } else {
                crate::LinkState::Closed
            },
            opened_at_epoch_seconds: None,
        }
    }

    pub async fn wait_for_dial_slot(
        &self,
        peer: PublicKey,
        automatic: bool,
    ) -> Result<(), Failure> {
        let mut events = self.events.subscribe();
        rocky_platform::time::timeout(Duration::from_secs(10), async {
            loop {
                events.borrow_and_update();
                let occupied = self
                    .entries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&peer)
                    .is_some_and(|entry| {
                        (!entry.persistent && !entry.dialed) || (automatic && !entry.usable())
                    });
                if !occupied {
                    return Ok(());
                }
                events.changed().await.map_err(|_| "link registry ended")?;
            }
        })
        .await
        .map_err(|_| "previous link did not finish")?
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

    pub fn short(&self, peer: PublicKey) -> ShortLink<'_> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let id = entries.get_mut(&peer).map(|entry| {
            entry.shorts += 1;
            entry.id
        });
        ShortLink {
            registry: self,
            peer,
            id,
        }
    }

    fn close_short(&self, peer: PublicKey, id: Option<u64>) {
        if let Some(entry) = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&peer)
            && Some(entry.id) == id
        {
            entry.shorts = entry.shorts.saturating_sub(1);
            if entry.shorts == 0 && !entry.persistent {
                entry.close.send_replace(Some(CloseReason::Detached));
            }
        }
    }

    pub fn handle(&self, peer: PublicKey) -> Option<link::channels::Handle> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .filter(|entry| entry.usable())
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
            .is_some_and(Entry::usable)
    }
    pub(super) fn registered(&self, peer: PublicKey) -> bool {
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
    pub(super) fn opening<'a>(&'a self, service: &'a Link, peer: PublicKey) -> Opening<'a> {
        *service
            .live
            .opening
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(peer)
            .or_default() += 1;
        self.link_status_changed();
        Opening {
            node: self,
            service,
            peer,
        }
    }
    fn link_status_changed(&self) {
        self.link_revision.fetch_add(1, Ordering::SeqCst);
        let status = self.sync_status.borrow().clone();
        self.synced(status);
    }

    pub(super) async fn publish_links(self: Arc<Self>, service: Arc<Link>) {
        let mut stage = service.stage.subscribe();
        let mut frames = self.replica.observe_frames().await;
        let mut membership = self.replica.observe_membership().await;
        let mut revisions = self.replica.observe_revision();
        let mut ready = service.live.events.subscribe();
        let mut passes = stream::FuturesUnordered::new();
        let mut pace = PublicationPace::new(LOCAL_SETTLE);
        loop {
            tokio::select! {
                _ = stage.wait_for(|stage| *stage != Stage::Open) => break,
                changed = membership.changed() => { if changed.is_err() { break; } },
                changed = frames.changed() => { if changed.is_err() { break; } },
                changed = ready.changed() => { if changed.is_err() { break; } },
                changed = revisions.changed() => { if changed.is_err() { break; } },
                Some(()) = passes.next(), if !passes.is_empty() => continue,
            }
            pace.start().await;
            membership.borrow_and_update();
            frames.borrow_and_update();
            ready.borrow_and_update();
            revisions.borrow_and_update();
            if self.sync_mode() == crate::SyncMode::Live {
                for (peer, id) in service.live.claim_publications() {
                    passes.push(self.publish_peer(&service, peer, id));
                }
            }
        }
    }
    async fn publish_peer(&self, service: &Link, peer: PublicKey, id: u64) {
        let mut claim = Claim {
            registry: &service.live,
            peer,
            id,
            held: true,
        };
        loop {
            if let Err(error) = self.signal_link(service, peer, id).await {
                self.activity
                    .record_changed(crate::activity::ActivityKind::LinkSignalFailed {
                        peer: (peer).to_owned(),
                        error: (error.to_string()).to_owned(),
                    });
            }
            if !claim.rerun() {
                return;
            }
        }
    }
    pub(super) async fn open_links(self: &Arc<Self>, service: &Arc<Link>) {
        if self.sync_mode() != crate::SyncMode::Live {
            return;
        }
        let settings = self.settings.lock().await.clone();
        let targets = self.dialed(&settings, true);
        let attempts = stream::iter(targets)
            .map(|(peer, contact)| {
                let node = self.clone();
                let service = service.clone();
                let settings = settings.clone();
                async move {
                    if service.live.registered(peer)
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
                    let _opening = node.opening(&service, peer);
                    let mut attempt = node.observations.begin(
                        peer.to_hex(),
                        crate::AttemptOperation::OpenLink,
                        crate::AttemptStage::Dial,
                    );
                    let result = async {
                        let stream = service
                            .transport
                            .session(&contact.addresses(&*service.transport), node.domain().scope)
                            .await?;
                        attempt.stage(crate::AttemptStage::Admission);
                        node.admit_link(service.clone(), stream, Some(peer), settings, None)
                            .await
                    }
                    .await;
                    let failure = result
                        .as_ref()
                        .err()
                        .map(|error| crate::status::failure(error, &attempt.current_stage()));
                    attempt.finish(failure);
                    if let Err(error) = result {
                        {
                            let mut retries =
                                node.retries.lock().unwrap_or_else(PoisonError::into_inner);
                            let state =
                                retries.entry(peer).or_insert_with(|| node.peer_retry(peer));
                            state.link = true;
                            state.link_retry.failed();
                        }
                        let _ = node.absorb_revocations(Some(&error)).await;
                        if node.learned.load(Ordering::SeqCst) {
                            node.wake_sync();
                        }
                        node.activity.record_changed(
                            crate::activity::ActivityKind::LinkAttemptFailed {
                                peer: (peer).to_owned(),
                                error: (error.to_string()).to_owned(),
                            },
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
        mut stream: Stream,
        expected: Option<PublicKey>,
        settings: Shared<Settings>,
        handshake_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<(), Failure> {
        if !self.online.load(Ordering::SeqCst) || *service.stage.borrow() != Stage::Open {
            return Err("live links are disabled".into());
        }
        if let Some(statement) = self.revocation_notice.revoked(&settings).await? {
            return Err(Box::new(crate::enrollment::Revoked(statement)));
        }
        let mut authority = self.replica.observe_authority().await;
        let local = PrivateKey::from_bytes(&self.vault.get("node")?);
        let own = local.public_key();
        let lanes = stream.take_lanes();
        let opened = Box::pin(link::Link::open(
            stream,
            &self.replica,
            local,
            settings.credentials.clone(),
            expected,
        ))
        .await;
        let mut opened = match opened {
            Err(_) if expected.is_some_and(|peer| service.live.contains(peer)) => return Ok(()),
            other => other?,
        };
        opened.attach_lanes(lanes)?;
        self.observe_peer_clock(opened.peer, opened.clock_offset);
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
        let mut tokens = BTreeMap::new();
        let mut cursors = BTreeMap::new();
        for scope in &opened.held {
            let token = match opened.local_states.get(scope) {
                Some(token) => ScopeToken::from_handshake(*token),
                None => ScopeToken::read(self, *scope).await?,
            };
            let reading = ScopeReading::racing_commit_stays_dirty(
                self,
                &service.live,
                ScopeReadSite::LinkOpen,
                *scope,
                token,
            )
            .await?;
            tokens.insert(*scope, reading.token());
            cursors.insert(*scope, reading.into_frontier());
        }
        let peer_tokens: BTreeMap<_, _> = opened
            .states
            .iter()
            .map(|(scope, token)| (*scope, ScopeToken::from_handshake(*token)))
            .collect();
        let initial: Vec<_> = tokens
            .iter()
            .filter(|(scope, token)| peer_tokens.get(scope) != Some(token))
            .map(|(scope, _)| *scope)
            .take(link::SIGNAL_SCOPES)
            .collect();
        if !initial.is_empty() {
            changed.send_replace(Some(ChangeHint {
                scopes: initial,
                control: false,
            }));
        }
        let accepted = {
            self.replica.flush().await?;
            let admission_excludes_revocation_receipt = self.settings.lock().await;
            if let Some(statement) = self
                .revocation_notice
                .revoked(&admission_excludes_revocation_receipt)
                .await?
            {
                return Err(Box::new(crate::enrollment::Revoked(statement)));
            }
            let mut entries = service
                .live
                .entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let existing_link_wins = entries.get(&peer).is_some_and(|old| {
                let crossed_dial = old.dialed != dialed;
                let restarted_peer_relinks = persistent;
                let old_direction_kept = match crossed_dial {
                    true => old.dialed == preferred,
                    false => !restarted_peer_relinks,
                };
                old_direction_kept && old.usable()
            });
            if existing_link_wins {
                false
            } else {
                if let Some(old) = entries.insert(
                    peer,
                    Entry {
                        opened_at: now()?,
                        id,
                        dialed,
                        persistent,
                        shorts: 0,
                        receiving: 0,
                        publishing: false,
                        republish: false,
                        cursor_generation: 0,
                        control_generation: None,
                        handle: opened.channels.handle.clone(),
                        authority: authority.clone(),
                        close: close.clone(),
                        changed: changed.clone(),
                        tokens,
                        cursors,
                    },
                ) {
                    old.close.send_replace(Some(CloseReason::Duplicate));
                    self.activity
                        .record(crate::activity::ActivityKind::LinkClosed {
                            peer: (peer).to_owned(),
                            reason: ("duplicate").to_owned(),
                            error: None,
                        });
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
            let state = retries.entry(peer).or_insert_with(|| self.peer_retry(peer));
            state.link = false;
            state.short = !persistent;
            state.link_retry.wake();
            state.retry.wake();
            if persistent || dialed {
                state.repair_all();
            }
            if state.pending.pending() && (persistent || dialed) {
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
            .record(crate::activity::ActivityKind::LinkOpened {
                peer: (peer).to_owned(),
                reason: (if dialed { "dialed" } else { "accepted" }).to_owned(),
            });
        self.link_status_changed();
        let node = self.clone();
        let close_rx = close.subscribe();
        let changed_tx = changed.clone();
        let running = self.signed(async move {
            let started = rocky_platform::time::Instant::now();
            let _permit = permit;
            let result = node
                .run_link(&service, opened, close_rx, changed_tx, authority)
                .await;
            node.link_ended(peer);
            let still_open = *service.stage.borrow() == Stage::Open;
            let authority_close = matches!(result, Ok(CloseReason::Authority));
            let redial_authority = authority_close
                && still_open
                && (persistent || dialed)
                && node.sync_mode() == crate::SyncMode::Live;
            let removed = {
                let mut redial = redial_authority
                    .then(|| node.retries.lock().unwrap_or_else(PoisonError::into_inner));
                let mut entries = service
                    .live
                    .entries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if entries.get(&peer).is_some_and(|entry| entry.id == id) {
                    if let Some(state) = redial.as_mut().and_then(|retries| retries.get_mut(&peer))
                    {
                        state.redial_after_authority_change(started.elapsed());
                    }
                    entries.remove(&peer);
                    true
                } else {
                    false
                }
            };
            if removed {
                service.live.events.send_replace(());
                node.activity
                    .record(crate::activity::ActivityKind::LinkClosed {
                        peer: (peer).to_owned(),
                        reason: result
                            .as_ref()
                            .map_or("error", |reason| liveness::close_reason(*reason))
                            .to_owned(),
                        error: result.as_ref().err().map(ToString::to_string),
                    });
                node.link_status_changed();
                if result.is_ok() && !persistent && !dialed && node.automatic.load(Ordering::SeqCst)
                {
                    node.link_pending(peer);
                }
                if redial_authority {
                    node.link_events.send_replace(());
                } else if still_open && result.is_ok() && !authority_close && persistent && dialed {
                    node.redial_closed_link(peer, started.elapsed());
                }
                if let Err(error) = &result
                    && still_open
                {
                    if let Ok(mut reached) = node.reached() {
                        let last = reached.entry(peer).or_insert_with(|| Reached {
                            at: now().unwrap_or_default(),
                            error: None,
                            refused: vec![],
                            waiting: vec![],
                            held: vec![],
                        });
                        last.error = Some(error.to_string());
                    }
                    let mut retries = node.retries.lock().unwrap_or_else(PoisonError::into_inner);
                    let state = retries.entry(peer).or_insert_with(|| node.peer_retry(peer));
                    state.link_retry.survived(started.elapsed());
                    state.retry.survived(started.elapsed());
                    state.link = persistent;
                    if persistent {
                        state.link_retry.failed();
                    } else {
                        state.repair_all();
                        state.retry.failed();
                    }
                    drop(retries);
                    node.link_events.send_replace(());
                }
            }
        });
        self.background.spawn("live_link", running);
        Ok(())
    }
    async fn signal_link(&self, service: &Link, peer: PublicKey, id: u64) -> Result<(), Failure> {
        #[cfg(test)]
        drop(service.live.publishing.lock().await);
        let Some((cursor_generation, handle, old, cursors)) = service
            .live
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .filter(|entry| entry.id == id && entry.receiving == 0)
            .map(|entry| {
                (
                    entry.cursor_generation,
                    entry.handle.clone(),
                    entry.tokens.clone(),
                    entry.cursors.clone(),
                )
            })
        else {
            return Ok(());
        };
        let mut tokens = old.clone();
        let mut next = cursors.clone();
        let mut scopes = BTreeSet::new();
        let mut control = false;
        let mut changed = Vec::new();
        for scope in cursors.keys() {
            let token = ScopeToken::read(self, *scope).await?;
            if old.get(scope) != Some(&token) {
                changed.push(*scope);
                tokens.insert(*scope, token);
            }
        }
        if !changed.is_empty() {
            let selected: Vec<_> = cursors
                .values()
                .filter(|cursor| {
                    cursors.len() <= link::SIGNAL_SCOPES || changed.contains(&cursor.scope)
                })
                .cloned()
                .collect();
            let outcome = match handle.publish_committed(&self.replica, &selected).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.link_pending_scopes(peer, &changed);
                    return Err(error.into());
                }
            };
            match outcome {
                rocky_secure::network::PublicationOutcome::Published(receipt) => {
                    for frontier in &receipt.frontiers {
                        match next.get_mut(&frontier.scope) {
                            Some(cursor) => acknowledge(cursor, frontier),
                            None => {
                                next.insert(frontier.scope, frontier.clone());
                            }
                        }
                    }
                    if receipt.repair_all {
                        control = true;
                        self.link_pending(peer);
                    }
                    tracing::debug!(?peer, reasons = ?receipt.reasons, repair_all = receipt.repair_all, "publication receipt");
                    scopes.extend(receipt.repair);
                    for scope in changed {
                        let metadata_only = next.get(&scope) == cursors.get(&scope);
                        if metadata_only {
                            control = true;
                            scopes.insert(scope);
                        }
                    }
                }
                rocky_secure::network::PublicationOutcome::Repair {
                    scopes: failed,
                    reason,
                } => {
                    tracing::debug!(?peer, %reason, "publication preflight requires repair");
                    for scope in failed {
                        let frontier = self.replica.frontier(scope).await?;
                        if next.get(&scope) != Some(&frontier) {
                            let reading = ScopeReading::racing_commit_stays_in_repair(
                                self,
                                &service.live,
                                ScopeReadSite::FailedPushRepair,
                                scope,
                                frontier,
                            )
                            .await?;
                            tokens.insert(scope, reading.token());
                            next.insert(scope, reading.into_frontier());
                            scopes.insert(scope);
                        } else if changed.contains(&scope) {
                            scopes.insert(scope);
                        }
                    }
                }
            }
        }
        self.replica.flush().await?;
        let mut entries = service
            .live
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = entries.get_mut(&peer).filter(|entry| {
            entry.id == id && entry.receiving == 0 && entry.cursor_generation == cursor_generation
        }) else {
            return Ok::<_, Failure>(());
        };
        entry.tokens = tokens;
        entry.cursors = next;
        entry.cursor_generation = entry.cursor_generation.wrapping_add(1);
        if !scopes.is_empty() {
            if let Some(pending) = entry.changed.borrow().as_ref() {
                scopes.extend(&pending.scopes);
                control |= pending.control;
            }
            control |= scopes.len() > link::SIGNAL_SCOPES;
            let unsent = scopes.iter().skip(link::SIGNAL_SCOPES);
            for scope in unsent {
                if let Some(old_token) = old.get(scope) {
                    entry.tokens.insert(*scope, *old_token);
                }
            }
            entry.changed.send_replace(Some(ChangeHint {
                scopes: scopes.into_iter().take(link::SIGNAL_SCOPES).collect(),
                control,
            }));
        }
        Ok::<_, Failure>(())
    }
    async fn received_publication(
        &self,
        service: &Link,
        peer: PublicKey,
        id: Option<u64>,
        receipt: &rocky_secure::network::PublicationReceipt,
    ) -> Result<(), Failure> {
        for shared in &receipt.frontiers {
            let scope = shared.scope;
            let token = ScopeToken::read(self, scope).await?;
            let reading = ScopeReading::racing_commit_stays_dirty(
                self,
                &service.live,
                ScopeReadSite::Receipt,
                scope,
                token,
            )
            .await?;
            let mut entries = service
                .live
                .entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let Some(entry) = entries.get_mut(&peer).filter(|e| Some(e.id) == id) else {
                return Ok(());
            };
            let Some(cursor) = entry.cursors.get_mut(&scope) else {
                continue;
            };
            acknowledge(cursor, shared);
            entry.cursor_generation = entry.cursor_generation.wrapping_add(1);
            if cursor == reading.frontier() {
                entry.tokens.insert(scope, reading.token());
            }
        }
        Ok(())
    }
    pub(in crate::node) async fn signal_token(
        &self,
        scopes: &[ResourceScope],
    ) -> Result<[u8; 32], Failure> {
        let mut state = Vec::new();
        for scope in scopes {
            state.push((*scope, link::scope_token(&self.replica, *scope).await?));
        }
        self.replica.flush().await?;
        Ok(commitment(
            "rocky.link-batch.v1",
            &serde_json::to_vec(&state)?,
        ))
    }
    fn redial_closed_link(&self, peer: PublicKey, lived: Duration) {
        if self.sync_mode() != crate::SyncMode::Live {
            return;
        }
        let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(kept_peer) = retries.get_mut(&peer) else {
            return;
        };
        kept_peer.link_retry.survived(lived);
        kept_peer.link_retry.failed();
        kept_peer.link = true;
        drop(retries);
        self.link_events.send_replace(());
    }
    fn link_pending_scopes(&self, peer: PublicKey, scopes: &[ResourceScope]) {
        let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        retries
            .entry(peer)
            .or_insert_with(|| self.peer_retry(peer))
            .repair_scopes(scopes);
        drop(retries);
        self.link_events.send_replace(());
    }
    fn link_pending(&self, peer: PublicKey) {
        let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        retries
            .entry(peer)
            .or_insert_with(|| self.peer_retry(peer))
            .repair_all();
        drop(retries);
        self.link_events.send_replace(());
    }
}

fn acknowledge(
    cursor: &mut rocky_secure::ScopeFrontier,
    acknowledged: &rocky_secure::ScopeFrontier,
) {
    for log in &acknowledged.logs {
        if let Some(held) = cursor.logs.iter_mut().find(|h| h.author == log.author) {
            held.held = held.held.max(log.held);
            held.applied = held.applied.max(log.applied);
        } else {
            cursor.logs.push(*log);
        }
    }
    cursor.logs.sort_by_key(|l| l.author);
}

#[cfg(test)]
pub(super) fn control_probe(
    near: link::Link,
    far: link::Link,
) -> (
    std::sync::mpsc::Receiver<Result<(), Failure>>,
    rocky_platform::task::JoinHandle<()>,
) {
    let (near_direction, far_direction) = (near.direction, far.direction);
    let mut near = io::Io::start(near, None);
    let mut far = io::Io::start(far, None);
    let (done, answer) = std::sync::mpsc::sync_channel(1);
    let task = rocky_platform::task::spawn(async move {
        let result: Result<(), Failure> = async {
            near.send(Frame::new(
                near_direction,
                Message::Close {
                    reason: CloseReason::Duplicate,
                },
            ))
            .await?;
            let _ = far.receive().await?;
            far.send(Frame::new(
                far_direction,
                Message::Close {
                    reason: CloseReason::Duplicate,
                },
            ))
            .await?;
            let _ = near.receive().await?;
            Ok(())
        }
        .await;
        let _ = done.send(result);
    });
    (answer, task)
}
