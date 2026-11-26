use super::*;
use crate::status::{Alert, AlertKind, BlockerCount, HealthReport, MeshDevice, SILENT_AFTER};
use live_links::io::Writer;
use rocky_credentials::Signature;
use rocky_secure::network::link::{self, Frame, HEALTH_BYTES};

const MAX_DEVICES: usize = 256;
pub(super) const REFRESH: Duration = Duration::from_secs(60);
const MAX_ITEMS: usize = 32;
const SETTLE: Duration = Duration::from_secs(1);
const VERSION: u32 = 9;

#[derive(Default)]
pub(super) struct AdminWatch {
    granted: BTreeMap<PublicKey, u64>,
    nodes: BTreeMap<PublicKey, PublicKey>,
    heard: BTreeMap<PublicKey, u64>,
    stale_after: u64,
    membership_alerts: Vec<Alert>,
}
const HEARD_SAVE: u64 = 3600;
impl AdminWatch {
    fn hear(&mut self, node: &PublicKey, at: u64) {
        if self.nodes.contains_key(node) {
            let heard = self.heard.entry(*node).or_default();
            *heard = (*heard).max(at);
        }
    }
    fn last(&self) -> impl Iterator<Item = (PublicKey, u64)> + '_ {
        self.granted.iter().map(|(identity, granted)| {
            let heard = self
                .nodes
                .iter()
                .filter(|(_, owner)| *owner == identity)
                .filter_map(|(node, _)| self.heard.get(node))
                .copied()
                .max()
                .unwrap_or_default();
            (*identity, heard.max(*granted))
        })
    }
    fn alerts(&self, time: u64) -> impl Iterator<Item = Alert> + '_ {
        self.last()
            .filter(move |(_, last)| time.saturating_sub(*last) >= self.stale_after)
            .map(|(identity, last)| Alert {
                kind: AlertKind::AdminStale {
                    identity: crate::PersonId::new(identity),
                    last_heard: crate::Timestamp::from_unix_seconds(last),
                },
                since: Some(crate::Timestamp::from_unix_seconds(
                    last.saturating_add(self.stale_after),
                )),
            })
    }
    fn deadline(&self, time: u64) -> Option<u64> {
        self.last()
            .map(|(_, last)| last.saturating_add(self.stale_after))
            .filter(|deadline| *deadline > time)
            .min()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u32,
    message: HealthMessage,
}
#[derive(Serialize, Deserialize)]
enum HealthMessage {
    Collector,
    Report {
        report: Box<SignedReport>,
        received: Option<u64>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedReport {
    node: PublicKey,
    #[serde(with = "serde_bytes")]
    body: Vec<u8>,
    signature: Signature,
}
pub(super) struct Held {
    signed: SignedReport,
    report: HealthReport,
    received: u64,
    via: Option<PublicKey>,
}
fn content(report: &HealthReport) -> HealthReport {
    HealthReport {
        produced_at_epoch_seconds: 0,
        ..report.clone()
    }
}
fn signed_bytes(
    database: rocky_credentials::DatabaseContext,
    node: &PublicKey,
    body: &[u8],
) -> Result<Vec<u8>, Failure> {
    Ok(rocky_credentials::signing_bytes(
        "rocky.sdk.health-report.v2",
        &(database, node, serde_bytes::Bytes::new(body)),
    )?)
}
fn label(node: &PublicKey) -> String {
    node.to_hex().get(..12).unwrap_or_default().to_owned()
}

impl Node {
    pub(super) fn alerts(&self) -> Vec<Alert> {
        let time = self.status_time().unwrap_or_default();
        let stale: Vec<Alert> = self
            .admin_watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .alerts(time)
            .collect();
        let membership = self
            .admin_watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .membership_alerts
            .clone();
        let mut alerts: Vec<Alert> = self
            .clocks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .chain(stale)
            .chain(membership)
            .chain(
                self.seal_skipped
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            )
            .collect();
        alerts.sort();
        alerts
    }
    pub(super) async fn replica_alerts(&self) -> Result<Vec<Alert>, Failure> {
        let mut alerts = self.tip_chain_alerts().await?;
        let lanes = self.replica.lane_alerts().await?;
        let superseded = lanes
            .superseded
            .into_iter()
            .map(|store| AlertKind::StoreSuperseded {
                node: crate::DeviceId::new(store.node),
                held: store.held,
                reported: store.reported,
            });
        let regressed = lanes
            .regressions
            .into_iter()
            .map(|peer| AlertKind::PeerStoreRegressed {
                node: crate::DeviceId::new(peer.node),
                scope: rocky_core::scope_label(&peer.scope),
                held: peer.held,
                reported: peer.reported,
            });
        alerts.extend(
            superseded
                .chain(regressed)
                .map(|kind| Alert { kind, since: None }),
        );
        alerts.extend(self.write_dropped_alerts().await?);
        Ok(alerts)
    }
    async fn write_dropped_alerts(&self) -> Result<Vec<Alert>, Failure> {
        let admin = self.settings.lock().await.issuer();
        let own = self.replica.node_key();
        Ok(self
            .replica
            .schema_drops()
            .await?
            .into_iter()
            .filter(|drop| admin || drop.node == own)
            .map(|drop| Alert {
                kind: AlertKind::WriteDropped {
                    token: crate::TransactionToken::new(drop.transaction),
                    node: crate::DeviceId::new(drop.node),
                    reason: crate::DropReason::Schema,
                },
                since: Some(crate::Timestamp::from_unix_micros(
                    i64::try_from(drop.written).unwrap_or(i64::MAX),
                )),
            })
            .collect())
    }
    async fn tip_chain_alerts(&self) -> Result<Vec<Alert>, Failure> {
        let graph = match self.replica.tip_graph(self.domain().scope).await {
            Ok(graph) => graph,
            Err(_graph_not_installed) => return Ok(Vec::new()),
        };
        let mut past: BTreeMap<(PublicKey, PublicKey), u64> = BTreeMap::new();
        for (identity, lane, statements) in self.replica.past_cut_records(graph.scope).await? {
            *past.entry((identity, lane)).or_default() += statements;
        }
        for item in &graph.past_cut {
            *past.entry((item.identity, item.link.lane)).or_default() += 1;
        }
        let past =
            past.into_iter().map(
                |((signer, node), statements)| AlertKind::TipStatementsPastCut {
                    signer: crate::PersonId::new(signer),
                    node: crate::DeviceId::new(node),
                    statements,
                },
            );
        let forks = graph
            .equivocations
            .iter()
            .map(|fork| AlertKind::TipChainEquivocation {
                signer: crate::PersonId::new(fork.identity),
                node: crate::DeviceId::new(fork.lane),
                sequence: fork.sequence,
            });
        Ok(past
            .chain(forks)
            .map(|kind| Alert { kind, since: None })
            .collect())
    }
    pub(super) async fn refresh_admin_watch(&self) -> Result<(), Failure> {
        let registry = if self.collects().await {
            let Some(registry) = self
                .readable_registry(&*self.settings_snapshot().await)
                .await?
            else {
                return Ok(());
            };
            Some(registry)
        } else {
            None
        };
        let mut known = self.replica.revocations().await;
        let snapshot = self.settings.lock().await.clone();
        #[cfg(test)]
        self.probe_settings_lock();
        let own = snapshot.identity;
        let database = snapshot.database_context()?;
        known.merge_in(&snapshot.revocations, database, now()?)?;
        if let Some(certificate) = snapshot.credentials.certificate() {
            known.link(certificate.clone())?;
        }
        known.verify_in(database)?;
        let saved = snapshot
            .authority
            .as_ref()
            .map(|issuing| issuing.heard.clone())
            .unwrap_or_default();
        let time = self.status_time()?;
        let admins = registry
            .as_ref()
            .map(|registry| registry.admin_identities(database, &known, time))
            .transpose()?
            .unwrap_or_default();
        let (granted, nodes) = match &registry {
            Some(registry) => {
                let membership = crate::membership::Membership::replicated(&known, database)?;
                let granted: BTreeMap<PublicKey, u64> = admins
                    .iter()
                    .copied()
                    .filter(|identity| *identity != own)
                    .filter_map(|identity| {
                        let record = registry.admins.get(&identity)?;
                        Some((identity, record.granted))
                    })
                    .collect();
                let nodes: BTreeMap<PublicKey, PublicKey> = granted
                    .keys()
                    .flat_map(|identity| {
                        membership
                            .nodes(identity)
                            .map(move |linked| (linked.node, *identity))
                    })
                    .collect();
                (granted, nodes)
            }
            None => Default::default(),
        };
        let base = snapshot
            .authority
            .as_ref()
            .map(|issuing| issuing.trusted_nodes.clone())
            .unwrap_or_default();
        let mut observed = base.clone();
        if registry.is_some() {
            crate::membership::observe_admins(&known, &admins, time, &mut observed)?;
        }
        let save = {
            let mut watch = self
                .admin_watch
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            watch.stale_after = self.admin_stale_after;
            for (node, at) in &saved {
                let heard = watch.heard.entry(*node).or_default();
                *heard = (*heard).max(*at);
            }
            watch.heard.retain(|node, _| nodes.contains_key(node));
            watch.granted = granted;
            watch.nodes = nodes;
            let live: Vec<_> = watch
                .nodes
                .keys()
                .filter(|node| self.linked(**node))
                .copied()
                .collect();
            for node in live {
                watch.hear(&node, time);
            }
            let granularity = HEARD_SAVE.min(watch.stale_after / 8);
            let moved = watch.heard.iter().any(|(node, at)| {
                saved
                    .get(node)
                    .is_none_or(|saved| at.saturating_sub(*saved) >= granularity)
            });
            (moved || watch.heard.len() != saved.len()).then(|| watch.heard.clone())
        };
        let mut settings = self.settings.lock().await;
        let snapshot_still_current = settings
            .authority
            .as_ref()
            .is_some_and(|issuing| issuing.heard == saved && issuing.trusted_nodes == base);
        let observed_changed = snapshot_still_current && base != observed;
        if snapshot_still_current && (save.is_some() || observed_changed) {
            let mut draft = settings.clone();
            if let Some(issuing) = draft.authority.as_mut() {
                if let Some(heard) = save {
                    issuing.heard = heard;
                }
                issuing.trusted_nodes = observed;
            }
            settings.commit(draft, &self.replica).await?;
        }
        let membership_alerts = if registry.is_some() {
            settings
                .authority
                .as_ref()
                .into_iter()
                .flat_map(|issuing| &issuing.trusted_nodes)
                .flat_map(|(identity, nodes)| {
                    nodes
                        .iter()
                        .flat_map(move |(node, observation)| observation.alerts(*identity, *node))
                })
                .collect()
        } else {
            Vec::new()
        };
        self.admin_watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .membership_alerts = membership_alerts;
        drop(settings);
        if observed_changed {
            self.observations.touch();
        }
        Ok(())
    }
    #[cfg(test)]
    fn probe_settings_lock(&self) {
        if self.settings.try_lock().is_err() {
            self.verified_under_settings.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn hear_admin(&self, node: &PublicKey, at: u64) {
        let mut watch = self
            .admin_watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let time = self.status_time().unwrap_or_default();
        let stale = watch.alerts(time).count();
        watch.hear(node, at);
        let cleared = watch.alerts(time).count() < stale;
        drop(watch);
        if cleared {
            self.observations.touch();
        }
    }
    pub(super) fn stale_deadline(&self, time: u64) -> Option<u64> {
        self.admin_watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .deadline(time)
    }
    pub(super) async fn collects(&self) -> bool {
        self.settings.lock().await.admin_links().is_some()
    }
    fn proves_collector(
        &self,
        settings: &Settings,
        peer: PublicKey,
        chain: &CredentialChain,
    ) -> Result<bool, Failure> {
        let compact = if chain.0.len() == 2 {
            chain.clone()
        } else {
            let grant = chain
                .0
                .first()
                .cloned()
                .ok_or("collector credentials are empty")?;
            let certificate = chain
                .certificate()
                .cloned()
                .ok_or("collector credentials have no node certificate")?;
            CredentialChain(vec![grant, certificate])
        };
        Ok(compact
            .trusted_node(
                settings.publisher,
                self.domain().scope,
                peer,
                Some((&settings.revocations, now()?)),
            )
            .is_ok())
    }
    pub(super) async fn health_report(&self) -> Result<HealthReport, Failure> {
        self.refresh_admin_watch().await?;
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for blocker in self.blockers().await? {
            let kind = serde_json::to_value(&blocker.kind)?["type"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned();
            *counts.entry(kind).or_default() += 1;
        }
        let mut alerts = self.alerts();
        let known = self.replica.revocations().await;
        let membership = known.membership()?;
        let identity = self.settings.lock().await.identity;
        alerts.extend(membership.forks(identity).map(|(sequence, _)| Alert {
            kind: crate::AlertKind::MembershipFork {
                identity: crate::PersonId::new(identity),
                sequence,
            },
            since: None,
        }));
        alerts.extend(self.replica_alerts().await?);
        alerts.sort();
        alerts.truncate(MAX_ITEMS);
        Ok(HealthReport {
            produced_at_epoch_seconds: self.status_time()?,
            schema: self.declared.version,
            alerts,
            blockers: counts
                .into_iter()
                .take(MAX_ITEMS)
                .map(|(kind, count)| BlockerCount { kind, count })
                .collect(),
        })
    }
    async fn sign_report(&self, report: &HealthReport) -> Result<SignedReport, Failure> {
        let key = PrivateKey::from_bytes(&self.vault.get("node")?);
        let node = key.public_key();
        let body = serde_json::to_vec(report)?;
        let database = self.settings.lock().await.database_context()?;
        let signature = key.sign(&signed_bytes(database, &node, &body)?);
        Ok(SignedReport {
            node,
            body,
            signature,
        })
    }
    fn reported(&self, result: Result<(), &Failure>) {
        let failing = result.is_err();
        if self.report_failing.swap(failing, Ordering::Relaxed) != failing {
            match result {
                Err(error) => tracing::warn!(%error, "this device could not report its health"),
                Ok(()) => tracing::info!("this device reports its health again"),
            }
        }
    }

    pub(super) async fn link_health(
        &self,
        peer: PublicKey,
        writer: Writer,
        direction: bool,
        mut collector: watch::Receiver<bool>,
    ) {
        let send = |message| self.send_health(&writer, direction, message);
        if self.collects().await
            && let Err(error) = send(HealthMessage::Collector).await
        {
            self.reported(Err(&error));
            return;
        }
        if collector.wait_for(|collects| *collects).await.is_err() {
            return;
        }
        let mut observations = self.observations.subscribe();
        let mut revisions = self.replica.observe_revision();
        let mut frames = self.replica.observe_frames().await;
        let mut mesh = self.mesh_revision.subscribe();
        let mut last: Option<HealthReport> = None;
        let mut refreshed = rocky_platform::time::Instant::now();
        let mut forwarded: BTreeMap<PublicKey, u64> = BTreeMap::new();
        loop {
            observations.borrow_and_update();
            revisions.borrow_and_update();
            frames.borrow_and_update();
            mesh.borrow_and_update();
            let collects = self.collects().await;
            let due = collects && refreshed.elapsed() >= REFRESH;
            let result = async {
                let report = self.health_report().await?;
                if due || last.as_ref().map(content) != Some(content(&report)) {
                    send(HealthMessage::Report {
                        report: Box::new(self.sign_report(&report).await?),
                        received: None,
                    })
                    .await?;
                    last = Some(report);
                }
                if collects {
                    let time = self.status_time()?;
                    for (node, report, received) in
                        self.forwards(peer, due.then_some(time), &forwarded)
                    {
                        send(HealthMessage::Report {
                            report: Box::new(report),
                            received: Some(received),
                        })
                        .await?;
                        forwarded.insert(node, received);
                    }
                }
                if due {
                    refreshed = rocky_platform::time::Instant::now();
                }
                Ok::<_, Failure>(())
            }
            .await;
            self.reported(result.as_ref().map(drop));
            let tick = async {
                if collects {
                    rocky_platform::time::sleep_until(refreshed + REFRESH).await
                } else {
                    std::future::pending().await
                }
            };
            tokio::select! {
                changed = observations.changed() => if changed.is_err() { return },
                changed = revisions.changed() => if changed.is_err() { return },
                changed = frames.changed() => if changed.is_err() { return },
                changed = mesh.changed() => if changed.is_err() { return },
                () = tick => {},
            }
            rocky_platform::time::sleep(SETTLE).await;
        }
    }
    fn forwards(
        &self,
        peer: PublicKey,
        now: Option<u64>,
        forwarded: &BTreeMap<PublicKey, u64>,
    ) -> Vec<(PublicKey, SignedReport, u64)> {
        self.mesh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(node, held)| **node != peer && held.via != Some(peer))
            .map(|(node, held)| {
                let heard = match now {
                    Some(now) if held.via.is_none() && self.linked(*node) => now,
                    _ => held.received,
                };
                (*node, held.signed.clone(), heard)
            })
            .filter(|(node, _, heard)| forwarded.get(node).is_none_or(|sent| sent < heard))
            .collect()
    }
    pub(super) fn reported_schema(&self, node: PublicKey, time: u64) -> Option<(u32, u64)> {
        if node == self.replica.node_key() {
            return Some((self.declared.version, time));
        }
        self.mesh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&node)
            .map(|held| (held.report.schema, held.received))
    }
    pub(super) fn last_seen(&self, node: PublicKey, time: u64) -> Option<u64> {
        if self.linked(node) {
            return Some(time);
        }
        self.mesh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&node)
            .map(|held| held.received)
    }
    fn linked(&self, node: PublicKey) -> bool {
        self.current()
            .is_some_and(|service| service.live.contains(node))
    }
    pub(super) fn link_ended(&self, node: PublicKey) {
        let Ok(time) = self.status_time() else {
            return;
        };
        if let Some(held) = self
            .mesh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&node)
            .filter(|held| held.via.is_none())
        {
            held.received = held.received.max(time);
        }
        self.hear_admin(&node, time);
        self.observations.touch();
    }
    async fn send_health(
        &self,
        writer: &Writer,
        direction: bool,
        message: HealthMessage,
    ) -> Result<(), Failure> {
        let bytes = rocky_credentials::encode(&Wire {
            version: VERSION,
            message,
        })?;
        if bytes.len() > HEALTH_BYTES {
            return Err("the health message exceeds its bound".into());
        }
        writer
            .send(Frame::new(direction, link::Message::Health(bytes)))
            .await
    }
    pub(super) async fn final_report(&self, writer: &Writer, direction: bool) {
        let result = async {
            let report = self.sign_report(&self.health_report().await?).await?;
            self.send_health(
                writer,
                direction,
                HealthMessage::Report {
                    report: Box::new(report),
                    received: None,
                },
            )
            .await
        }
        .await;
        self.reported(result.as_ref().map(drop));
    }
    pub(super) async fn receive_health(
        &self,
        peer: PublicKey,
        chain: &CredentialChain,
        collector: &watch::Sender<bool>,
        bytes: Vec<u8>,
    ) {
        if let Err(error) = self.absorb_health(peer, chain, collector, &bytes).await {
            tracing::debug!(peer = label(&peer), %error, "a health message was refused");
        }
    }
    async fn absorb_health(
        &self,
        peer: PublicKey,
        chain: &CredentialChain,
        collector: &watch::Sender<bool>,
        bytes: &[u8],
    ) -> Result<(), Failure> {
        let wire: Wire = rocky_credentials::decode_bounded(bytes, HEALTH_BYTES)?;
        if wire.version != VERSION {
            return Err("unsupported health message version".into());
        }
        let (signed, received) = match wire.message {
            HealthMessage::Collector => {
                let settings = self.settings.lock().await.clone();
                if !self.proves_collector(&settings, peer, chain)? {
                    return Err("the peer is no admin node".into());
                }
                self.hear_admin(&peer, self.status_time()?);
                collector.send_replace(true);
                return Ok(());
            }
            HealthMessage::Report { report, received } => (*report, received),
        };
        #[cfg(test)]
        self.reports_received.fetch_add(1, Ordering::Relaxed);
        if !self.collects().await {
            return Err("this device collects no reports".into());
        }
        if self.settings.lock().await.revocations.revokes(&signed.node) {
            return Err("the reporting device is revoked".into());
        }
        let time = self.status_time()?;
        let (received, via) = match received {
            None if signed.node == peer => (time, None),
            None => return Err("a device reports only its own health".into()),
            Some(_) if !*collector.borrow() => {
                return Err("only an admin node forwards reports".into());
            }
            Some(at) => (at.min(time), Some(peer)),
        };
        if self.node_identity.as_ref().ok() == Some(&signed.node) {
            return Ok(());
        }
        let database = self.settings.lock().await.database_context()?;
        let bytes = signed_bytes(database, &signed.node, &signed.body)?;
        if !signed.node.verify(&bytes, &signed.signature) {
            return Err("the report's signature does not verify".into());
        }
        let report: HealthReport = serde_json::from_slice(&signed.body)?;
        if report.alerts.len() > MAX_ITEMS || report.blockers.len() > MAX_ITEMS {
            return Err("the report exceeds its bounds".into());
        }
        self.hear_admin(&signed.node, received);
        let mut mesh = self.mesh.lock().unwrap_or_else(PoisonError::into_inner);
        let prior = mesh.get(&signed.node);
        if prior.is_some_and(|held| held.received > received) {
            return Ok(());
        }
        let known = prior.is_some();
        let changed = prior.is_none_or(|held| {
            content(&held.report) != content(&report) || self.silent(held.received, time)
        });
        if !known
            && mesh.len() >= MAX_DEVICES
            && let Some(oldest) = mesh
                .iter()
                .min_by_key(|(_, held)| held.received)
                .map(|(node, _)| *node)
        {
            tracing::warn!(
                dropped = label(&oldest),
                "health reports of {MAX_DEVICES} devices are held: the oldest is dropped"
            );
            mesh.remove(&oldest);
        }
        if !known {
            tracing::info!(device = label(&signed.node), "a device reports its health");
        }
        mesh.insert(
            signed.node,
            Held {
                signed,
                report,
                received,
                via,
            },
        );
        drop(mesh);
        self.mesh_revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        if changed {
            self.observations.touch();
        }
        Ok(())
    }
    fn silent(&self, received: u64, time: u64) -> bool {
        time.saturating_sub(received) >= SILENT_AFTER.as_secs()
    }
    pub(super) async fn mesh_status(&self) -> Result<Option<Vec<MeshDevice>>, Failure> {
        if !self.collects().await {
            return Ok(None);
        }
        let time = self.status_time()?;
        Ok(Some(
            self.mesh
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .map(|(node, held)| MeshDevice {
                    node: crate::DeviceId::new(*node),
                    report: held.report.clone(),
                    received_at_epoch_seconds: held.received,
                    via: held.via.map(crate::DeviceId::new),
                    silent: !self.linked(*node) && self.silent(held.received, time),
                })
                .collect(),
        ))
    }
    pub(super) fn health_deadline(&self, time: u64) -> Option<u64> {
        self.mesh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(node, _)| !self.linked(**node))
            .map(|(_, held)| held.received.saturating_add(SILENT_AFTER.as_secs()))
            .filter(|deadline| *deadline > time)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn health_evidence_is_bound_to_the_pinned_root_and_stable_scope() {
        let key = PrivateKey::new();
        let node = key.public_key();
        let database = rocky_credentials::DatabaseContext {
            publisher: PrivateKey::new().public_key(),
            scope: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
        };
        let body = b"health";
        let signature = key.sign(&signed_bytes(database, &node, body).unwrap());
        assert!(node.verify(&signed_bytes(database, &node, body).unwrap(), &signature));
        for foreign in [
            rocky_credentials::DatabaseContext {
                publisher: PrivateKey::new().public_key(),
                ..database
            },
            rocky_credentials::DatabaseContext {
                scope: rocky_core::Scope {
                    domain: rocky_core::DomainId([9; 16]),
                    ..database.scope
                },
                ..database
            },
        ] {
            assert!(!node.verify(&signed_bytes(foreign, &node, body).unwrap(), &signature));
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn either_linked_sibling_keeps_an_identity_live_but_an_unlinked_one_does_not() {
        let identity = rocky_credentials::PrivateKey::new().public_key();
        let nodes = [
            rocky_credentials::PrivateKey::new().public_key(),
            rocky_credentials::PrivateKey::new().public_key(),
        ];
        let mut watch = AdminWatch {
            granted: [(identity, 100)].into(),
            nodes: nodes.into_iter().map(|node| (node, identity)).collect(),
            stale_after: 60,
            ..Default::default()
        };
        watch.hear(&nodes[0], 150);
        assert_eq!(watch.alerts(209).count(), 0);
        watch.hear(&nodes[1], 200);
        assert_eq!(watch.alerts(259).count(), 0);
        assert_eq!(watch.deadline(201), Some(260));
        watch.nodes.remove(&nodes[1]);
        assert!(matches!(
            watch.alerts(210).next().map(|alert| alert.kind),
            Some(AlertKind::AdminStale { last_heard, .. })
                if last_heard == crate::Timestamp::from_unix_seconds(150)
        ));
        watch.hear(&nodes[1], 300);
        assert_eq!(watch.last().collect::<Vec<_>>(), [(identity, 150)]);
        let restored = AdminWatch {
            heard: watch.heard.clone(),
            granted: watch.granted.clone(),
            nodes: watch.nodes.clone(),
            stale_after: 60,
            ..Default::default()
        };
        assert_eq!(
            restored.alerts(210).collect::<Vec<_>>(),
            watch.alerts(210).collect::<Vec<_>>()
        );
        let ordinary_database = AdminWatch::default();
        assert_eq!(ordinary_database.alerts(1000).count(), 0);
    }
}
