//! Mesh health: standing coordination failures as this device's alerts,
//! and each device's signed report to the domain's trusted nodes over its
//! live links to them. Trusted nodes forward what they receive to each
//! other. Reports stay in memory; a restarted trusted node learns them again
//! within `REFRESH`.
use super::*;
use crate::status::{
    Alert, AlertKind, BlockerCount, CoordinationFailure, HealthReport, MeshDevice, SILENT_AFTER,
    STANDING_FAILURE, TRUSTED_STALE_AFTER,
};
use live_links::io::Writer;
use rocky_core::CoordinationError;
use rocky_credentials::Signature;
use rocky_secure::network::link::{self, Frame, HEALTH_BYTES};

/// Devices whose reports a trusted node holds; the oldest is dropped first.
const MAX_DEVICES: usize = 256;
/// Trusted nodes refresh each other this often on what they hold, so one
/// that links to a device keeps it heard on the others.
pub(super) const REFRESH: Duration = Duration::from_secs(60);
/// Alerts and blocker kinds one report carries.
const MAX_ITEMS: usize = 32;
/// Changes that arrive together become one report.
const SETTLE: Duration = Duration::from_secs(1);
const VERSION: u32 = 2;

/// Coordination failures that stand, from every call to the service.
/// Without iroh no service is composed, so none arises.
#[derive(Default)]
#[cfg_attr(not(feature = "iroh"), allow(dead_code))]
struct Standing {
    skew: Option<Alert>,
    /// When the current run of unanswered calls began.
    unanswered: Option<u64>,
    unreachable: Option<Alert>,
    /// When the current run of failed revocation pushes began.
    failing_push: Option<u64>,
    unpushed: Option<Alert>,
    failure: Option<CoordinationFailure>,
}
/// What the coordination service's answers show; shared with its client.
#[cfg_attr(not(feature = "iroh"), allow(dead_code))]
pub(crate) struct Coordination {
    state: std::sync::Mutex<Standing>,
    observations: crate::status::Observations,
}
impl Coordination {
    pub(crate) fn new(observations: crate::status::Observations) -> Self {
        Self {
            state: Default::default(),
            observations,
        }
    }
    /// The client's observer of every call's outcome.
    #[cfg(feature = "iroh")]
    pub(crate) fn observer(self: &Arc<Self>) -> rocky_coordinator::service::Observer {
        let coordination = Arc::downgrade(self);
        Arc::new(move |result| {
            if let (Some(coordination), Ok(time)) = (coordination.upgrade(), now()) {
                coordination.observe(result, time);
            }
        })
    }
    /// A clock skew refusal raises its alert; an answer past the voter's
    /// clock check clears it.
    /// Unanswered calls raise one once they last `STANDING_FAILURE`; any
    /// answer clears it. Logs only transitions.
    #[cfg_attr(not(feature = "iroh"), allow(dead_code))]
    pub(crate) fn observe(&self, result: Result<(), &CoordinationError>, time: u64) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut changed = false;
        if let Err(error) = result {
            state.failure = Some(CoordinationFailure {
                code: error.category().into(),
                at_epoch_seconds: time,
            });
            changed = true;
        }
        if let Err(CoordinationError::Unavailable) = result {
            let start = *state.unanswered.get_or_insert(time);
            if state.unreachable.is_none()
                && time.saturating_sub(start) >= STANDING_FAILURE.as_secs()
            {
                tracing::warn!(since = start, "the coordination service has not answered");
                state.unreachable = Some(Alert {
                    kind: AlertKind::CoordinatorUnreachable,
                    since: Some(start),
                });
            }
        } else {
            state.unanswered = None;
            if state.unreachable.take().is_some() {
                tracing::info!("the coordination service answers again");
                changed = true;
            }
            match result {
                Err(CoordinationError::ClockSkew { coordinator_time }) => {
                    let kind = AlertKind::CoordinatorClockSkew {
                        seconds: time.abs_diff(*coordinator_time),
                        ahead: time > *coordinator_time,
                    };
                    let since = match &state.skew {
                        Some(alert) => alert.since,
                        None => {
                            tracing::warn!(
                                seconds = time.abs_diff(*coordinator_time),
                                "the coordination service refuses this device's clock"
                            );
                            Some(time)
                        }
                    };
                    state.skew = Some(Alert { kind, since });
                }
                // The voter authenticates before it checks the clock.
                Err(CoordinationError::Denied) => {}
                _ if state.skew.take().is_some() => {
                    tracing::info!("the coordination service accepts this device's clock again");
                    changed = true;
                }
                _ => {}
            }
        }
        drop(state);
        if changed {
            self.observations.touch();
        }
    }
    /// A failed push of held revocations raises its alert once failures
    /// last `STANDING_FAILURE`; an acknowledged one, or none left to push,
    /// clears it.
    #[cfg_attr(not(feature = "iroh"), allow(dead_code))]
    pub(crate) fn observe_push(&self, pushed: bool, time: u64) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut changed = false;
        if pushed {
            state.failing_push = None;
            if state.unpushed.take().is_some() {
                tracing::info!("the coordination service acknowledged the held revocations");
                changed = true;
            }
        } else {
            let start = *state.failing_push.get_or_insert(time);
            if state.unpushed.is_none() && time.saturating_sub(start) >= STANDING_FAILURE.as_secs()
            {
                tracing::warn!(
                    since = start,
                    "held revocations have not reached the coordination service"
                );
                state.unpushed = Some(Alert {
                    kind: AlertKind::RevocationsUnpushed,
                    since: Some(start),
                });
                changed = true;
            }
        }
        drop(state);
        if changed {
            self.observations.touch();
        }
    }
    fn alerts(&self) -> Vec<Alert> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        [&state.skew, &state.unreachable, &state.unpushed]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
    fn failure(&self) -> Option<CoordinationFailure> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failure
            .clone()
    }
}

/// A trusted node's view of the other trusted identities: when each was
/// granted and last heard, through any of its nodes.
#[derive(Default)]
pub(super) struct TrustedWatch {
    /// The other live trusted identities and their grant times; empty
    /// unless this device is a trusted node.
    granted: BTreeMap<PublicKey, u64>,
    /// Node keys of those identities.
    nodes: BTreeMap<PublicKey, PublicKey>,
    heard: BTreeMap<PublicKey, u64>,
    stale_after: u64,
}
/// A last-heard time is saved once it moved this far, or an eighth of
/// the threshold, so reports and links do not write the settings each time.
const HEARD_SAVE: u64 = 3600;
impl TrustedWatch {
    fn hear(&mut self, node: &PublicKey, at: u64) {
        if let Some(identity) = self.nodes.get(node) {
            let heard = self.heard.entry(*identity).or_default();
            *heard = (*heard).max(at);
        }
    }
    /// Each identity's last sign of life: heard, or else granted.
    fn last(&self) -> impl Iterator<Item = (PublicKey, u64)> + '_ {
        self.granted.iter().map(|(identity, granted)| {
            let heard = self.heard.get(identity).copied().unwrap_or_default();
            (*identity, heard.max(*granted))
        })
    }
    fn alerts(&self, time: u64) -> impl Iterator<Item = Alert> + '_ {
        self.last()
            .filter(move |(_, last)| time.saturating_sub(*last) >= self.stale_after)
            .map(|(identity, last)| Alert {
                kind: AlertKind::TrustedNodeStale {
                    identity: identity.to_hex(),
                    last_heard_epoch_seconds: last,
                },
                since: Some(last.saturating_add(self.stale_after)),
            })
    }
    fn deadline(&self, time: u64) -> Option<u64> {
        self.last()
            .map(|(_, last)| last.saturating_add(self.stale_after))
            .filter(|deadline| *deadline > time)
            .min()
    }
}

/// One health message on a live link.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u32,
    message: HealthMessage,
}
#[derive(Serialize, Deserialize)]
enum HealthMessage {
    /// The sender collects reports: its admitted chain shows a trusted node.
    Collector,
    /// A device's report; `received` is when a trusted node got it from the
    /// device, present only when one forwards it.
    Report {
        report: Box<SignedReport>,
        received: Option<u64>,
    },
}
/// A report as its device signed it, for this domain.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedReport {
    node: PublicKey,
    #[serde(with = "serde_bytes")]
    body: Vec<u8>,
    signature: Signature,
}
/// A device's latest report on a trusted node.
pub(super) struct Held {
    signed: SignedReport,
    report: HealthReport,
    received: u64,
    via: Option<PublicKey>,
}
/// A report without its production time, to tell a change from a refresh.
fn content(report: &HealthReport) -> HealthReport {
    HealthReport {
        produced_at_epoch_seconds: 0,
        ..report.clone()
    }
}
fn signed_bytes(
    scope: &rocky_core::Scope,
    node: &PublicKey,
    body: &[u8],
) -> Result<Vec<u8>, Failure> {
    Ok(rocky_credentials::signing_bytes(
        "rocky.sdk.health-report.v1",
        &(scope, node, serde_bytes::Bytes::new(body)),
    )?)
}
fn label(node: &PublicKey) -> String {
    node.to_hex().get(..12).unwrap_or_default().to_owned()
}

impl Node {
    /// Feeds one coordination outcome to this device, as its client does.
    #[cfg(test)]
    pub(crate) fn observe_coordination(&self, result: Result<(), &CoordinationError>) {
        if let Ok(time) = self.status_time() {
            self.coordination.observe(result, time);
        }
    }
    /// This device's alerts: clocks its peers disagree with, standing
    /// coordination failures and, on a trusted node, stale trusted nodes
    /// and a skipped seal.
    pub(super) fn alerts(&self) -> Vec<Alert> {
        let time = self.status_time().unwrap_or_default();
        let stale: Vec<Alert> = self
            .trusted_watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .alerts(time)
            .collect();
        let mut alerts: Vec<Alert> = self
            .clocks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .chain(self.coordination.alerts())
            .chain(stale)
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
    /// What a root revocation's cut left out of the domain's tip graph,
    /// held, refused or pruned, per signing node, and each fork of a node's
    /// chain the graph holds.
    pub(super) async fn tip_chain_alerts(&self) -> Result<Vec<Alert>, Failure> {
        let graph = match self.replica.tip_graph(self.domain().scope).await {
            Ok(graph) => graph,
            // Before the domain's graph is installed there is nothing to judge.
            Err(_) => return Ok(Vec::new()),
        };
        let mut past: BTreeMap<(PublicKey, PublicKey), (u64, u64)> = BTreeMap::new();
        // Refused on arrival or pruned: no longer held, still counted.
        for (identity, lane, statements, acknowledgments) in
            self.replica.past_cut_records(graph.scope).await?
        {
            let counts = past.entry((identity, lane)).or_default();
            counts.0 += statements;
            counts.1 += acknowledgments;
        }
        for item in &graph.past_cut {
            let counts = past.entry((item.identity, item.link.lane)).or_default();
            if item.acknowledgment {
                counts.1 += 1;
            } else {
                counts.0 += 1;
            }
        }
        let past = past
            .into_iter()
            .map(|((signer, node), (statements, acknowledgments))| {
                AlertKind::TipStatementsPastCut {
                    signer: signer.to_hex(),
                    node: node.to_hex(),
                    statements,
                    acknowledgments,
                }
            });
        let forks = graph
            .equivocations
            .iter()
            .map(|fork| AlertKind::TipChainEquivocation {
                signer: fork.identity.to_hex(),
                node: fork.lane.to_hex(),
                sequence: fork.sequence,
                acknowledgment: fork.acknowledgment,
            });
        Ok(past
            .chain(forks)
            .map(|kind| Alert { kind, since: None })
            .collect())
    }
    /// The carried root escrow records this replica refused, by peer.
    pub(super) async fn escrow_alerts(&self) -> Result<Vec<Alert>, Failure> {
        Ok(self
            .replica
            .escrow_refusals()
            .await?
            .into_iter()
            .map(|(peer, records, reason)| Alert {
                kind: AlertKind::EscrowRefused {
                    node: peer.to_hex(),
                    records,
                    reason,
                },
                since: None,
            })
            .collect())
    }
    /// Root recoveries that met: the latest ended the others. And every
    /// other trusted identity known here, from the registry or the tip
    /// graph, whose grant the latest recovery ended.
    pub(super) async fn recovery_alerts(&self) -> Result<Vec<Alert>, Failure> {
        if self.settings.lock().await.revocations.recovery().is_none() {
            return Ok(Vec::new());
        }
        // Read before the settings are held: reading takes their lock.
        let registry = self.registry().await?;
        // The trusted identities known here, with their grants' time: the
        // registry's, and those whose tip statements the graph holds.
        let mut known: BTreeMap<PublicKey, u64> =
            match self.replica.tip_graph(self.domain().scope).await {
                Ok(graph) => graph.trusted_grants().map_err(refused)?,
                // Before the domain's graph is installed there is nothing to judge.
                Err(_) => BTreeMap::new(),
            };
        for (identity, record) in &registry.trusted {
            let since = known
                .entry(*identity)
                .or_insert(record.grant.capability.not_before);
            *since = (*since).max(record.grant.capability.not_before);
        }
        let settings = self.settings.lock().await;
        let revocations = &settings.revocations;
        let Some(latest) = revocations.recovery().copied() else {
            return Ok(Vec::new());
        };
        let recoveries = revocations.recoveries();
        let met = recoveries
            .iter()
            .filter(|recovery| **recovery != latest)
            .map(|ended| AlertKind::RecoveriesMet {
                kept: latest.keep.to_hex(),
                ended: ended.keep.to_hex(),
                shared_slot: ended.slot == latest.slot,
            });
        // Recovered identities are reported as recoveries that met; this
        // device's own end as its revocation reason.
        let ended = known
            .iter()
            .filter(|(identity, since)| {
                **identity != settings.identity
                    && !recoveries
                        .iter()
                        .any(|recovery| recovery.keep == **identity)
                    && latest.ends(**since, identity)
            })
            .map(|(identity, _)| AlertKind::RecoveryEnded {
                recovered: latest.keep.to_hex(),
                ended: identity.to_hex(),
            });
        Ok(met
            .chain(ended)
            .map(|kind| Alert { kind, since: None })
            .collect())
    }
    /// Reloads the other trusted identities from the registry, merges the
    /// saved last-heard times and saves those that moved `HEARD_SAVE`.
    pub(super) async fn refresh_trusted_watch(&self) -> Result<(), Failure> {
        let registry = if self.collects().await {
            Some(self.registry().await?)
        } else {
            None
        };
        let mut settings = self.settings.lock().await;
        let own = settings.identity;
        let saved = settings
            .authority
            .as_ref()
            .map(|issuing| (issuing.heard.clone(), issuing.stale_after))
            .unwrap_or_default();
        let (granted, nodes) = match &registry {
            Some(registry) => {
                let granted: BTreeMap<PublicKey, u64> = registry
                    .trusted_identities(&settings.revocations)
                    .into_iter()
                    .filter(|identity| *identity != own)
                    .filter_map(|identity| {
                        let record = registry.trusted.get(&identity)?;
                        Some((identity, record.granted))
                    })
                    .collect();
                let nodes = registry
                    .records
                    .iter()
                    .filter(|(_, record)| !record.revoked)
                    .filter_map(|(node, record)| {
                        let user = record.device.as_ref()?.user;
                        granted.contains_key(&user).then_some((*node, user))
                    })
                    .collect();
                (granted, nodes)
            }
            None => Default::default(),
        };
        let save = {
            let mut watch = self
                .trusted_watch
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            watch.stale_after = saved.1.unwrap_or(TRUSTED_STALE_AFTER.as_secs());
            for (identity, at) in &saved.0 {
                let heard = watch.heard.entry(*identity).or_default();
                *heard = (*heard).max(*at);
            }
            watch
                .heard
                .retain(|identity, _| granted.contains_key(identity));
            watch.granted = granted;
            watch.nodes = nodes;
            let granularity = HEARD_SAVE.min(watch.stale_after / 8);
            let moved = watch.heard.iter().any(|(identity, at)| {
                saved
                    .0
                    .get(identity)
                    .is_none_or(|saved| at.saturating_sub(*saved) >= granularity)
            });
            (moved || watch.heard.len() != saved.0.len()).then(|| watch.heard.clone())
        };
        if let (Some(heard), Some(_)) = (save, &settings.authority) {
            let mut draft = settings.clone();
            if let Some(issuing) = draft.authority.as_mut() {
                issuing.heard = heard;
            }
            draft.save(&self.path)?;
            *settings = draft;
        }
        Ok(())
    }
    /// A sign of life from `node` at `at`, if it is another trusted node's.
    fn hear_trusted(&self, node: &PublicKey, at: u64) {
        let mut watch = self
            .trusted_watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = watch.heard.clone();
        watch.hear(node, at);
        let time = self.status_time().unwrap_or_default();
        // A stale node heard again clears its alert.
        let cleared = watch.granted.keys().any(|identity| {
            let last = |heard: &BTreeMap<PublicKey, u64>| {
                heard
                    .get(identity)
                    .copied()
                    .unwrap_or_default()
                    .max(watch.granted[identity])
            };
            time.saturating_sub(last(&before)) >= watch.stale_after
                && time.saturating_sub(last(&watch.heard)) < watch.stale_after
        });
        drop(watch);
        if cleared {
            self.observations.touch();
        }
    }
    /// Sets how long another trusted identity may stay unheard before this
    /// trusted node flags it.
    pub(super) async fn set_trusted_stale_after(&self, seconds: u64) -> Result<Value, Failure> {
        if !(60..=10 * 366 * 24 * 60 * 60).contains(&seconds) {
            return Err(Box::new(crate::node::Refused(
                "stale_after must be a minute to ten years".into(),
            )));
        }
        if !self.collects().await {
            return Err(Box::new(crate::node::Refused(
                "only a trusted node watches other trusted nodes".into(),
            )));
        }
        {
            let mut settings = self.settings.lock().await;
            let mut draft = settings.clone();
            draft
                .authority
                .as_mut()
                .ok_or("a trusted node issues")?
                .stale_after = Some(seconds);
            draft.save(&self.path)?;
            *settings = draft;
        }
        self.refresh_trusted_watch().await?;
        self.observations.touch();
        Ok(json!({"stale_after": seconds}))
    }
    /// The next time, after `time`, another trusted node turns stale.
    pub(super) fn stale_deadline(&self, time: u64) -> Option<u64> {
        self.trusted_watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .deadline(time)
    }
    /// Whether this device collects health reports: a trusted node, whose
    /// identity holds the root's trusted grant.
    async fn collects(&self) -> bool {
        self.settings.lock().await.trusted_links().is_some()
    }
    /// Whether `peer`, admitted with `chain`, is a trusted node: a node of
    /// an identity the root's unrevoked trusted grant names.
    fn proves_collector(
        &self,
        settings: &Settings,
        peer: PublicKey,
        chain: &CredentialChain,
    ) -> Result<bool, Failure> {
        Ok(chain
            .trusted_node(
                settings.publisher,
                self.domain().scope,
                peer,
                Some((&settings.revocations, now()?)),
            )
            .is_ok())
    }
    pub(super) async fn health_report(&self) -> Result<HealthReport, Failure> {
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for blocker in self.blockers().await? {
            let kind = serde_json::to_value(&blocker.kind)?["type"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned();
            *counts.entry(kind).or_default() += 1;
        }
        let mut alerts = self.alerts();
        alerts.extend(self.tip_chain_alerts().await?);
        alerts.extend(self.escrow_alerts().await?);
        alerts.extend(self.recovery_alerts().await?);
        alerts.sort();
        alerts.truncate(MAX_ITEMS);
        Ok(HealthReport {
            produced_at_epoch_seconds: self.status_time()?,
            alerts,
            blockers: counts
                .into_iter()
                .take(MAX_ITEMS)
                .map(|(kind, count)| BlockerCount { kind, count })
                .collect(),
            coordination_failure: self.coordination.failure(),
        })
    }
    fn sign_report(&self, report: &HealthReport) -> Result<SignedReport, Failure> {
        let key = PrivateKey::from_bytes(&self.vault.get("node")?);
        let node = key.public_key();
        let body = serde_json::to_vec(report)?;
        let signature = key.sign(&signed_bytes(&self.domain().scope, &node, &body)?);
        Ok(SignedReport {
            node,
            body,
            signature,
        })
    }
    /// Logs the first failure to report after a success, and the recovery.
    fn reported(&self, result: Result<(), &Failure>) {
        let failing = result.is_err();
        if self.report_failing.swap(failing, Ordering::Relaxed) != failing {
            match result {
                Err(error) => tracing::warn!(%error, "this device could not report its health"),
                Ok(()) => tracing::info!("this device reports its health again"),
            }
        }
    }

    /// Sends this device's health over a link to `peer`: first that this
    /// device collects, if it does; then, once `collector` shows the peer
    /// does, its own report on each change and every `REFRESH`, and on a
    /// trusted node what it holds of other devices. Never ends with an
    /// error: replication goes on whatever becomes of reports.
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
            // Between trusted nodes alone: a device's idle link writes nothing.
            let trusted = self.collects().await;
            let due = trusted && refreshed.elapsed() >= REFRESH;
            let result = async {
                let report = self.health_report().await?;
                if due || last.as_ref().map(content) != Some(content(&report)) {
                    send(HealthMessage::Report {
                        report: Box::new(self.sign_report(&report)?),
                        received: None,
                    })
                    .await?;
                    last = Some(report);
                }
                if trusted {
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
                if trusted {
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
    /// What a trusted node forwards to the trusted node `peer`: each
    /// report newer than the one it sent; at a refresh, `now`, a device it
    /// links to as heard then.
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
    /// Whether a live link to `node` is open here.
    fn linked(&self, node: PublicKey) -> bool {
        self.current()
            .is_some_and(|service| service.live.contains(node))
    }
    /// A device was heard until its link here closed.
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
        self.hear_trusted(&node, time);
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
    /// A last report before a link to a trusted node closes, so a device
    /// that syncs on short links reports what its exchange left.
    pub(super) async fn final_report(&self, writer: &Writer, direction: bool) {
        let result = async {
            let report = self.sign_report(&self.health_report().await?)?;
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
    /// Takes one health message from `peer`, admitted with `chain`. A
    /// refused one is logged and changes nothing; it never ends the link.
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
                let settings = self.settings.lock().await;
                if !self.proves_collector(&settings, peer, chain)? {
                    return Err("the peer is no trusted node".into());
                }
                drop(settings);
                self.hear_trusted(&peer, self.status_time()?);
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
                return Err("only a trusted node forwards reports".into());
            }
            Some(at) => (at.min(time), Some(peer)),
        };
        if Some(signed.node.to_hex()) == self.node_identity.as_ref().ok().cloned() {
            return Ok(());
        }
        let bytes = signed_bytes(&self.domain().scope, &signed.node, &signed.body)?;
        if !signed.node.verify(&bytes, &signed.signature) {
            return Err("the report's signature does not verify".into());
        }
        let report: HealthReport = serde_json::from_slice(&signed.body)?;
        if report.alerts.len() > MAX_ITEMS || report.blockers.len() > MAX_ITEMS {
            return Err("the report exceeds its bounds".into());
        }
        self.hear_trusted(&signed.node, received);
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
    /// Each device's latest report on a trusted node; `None` elsewhere.
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
                    node: node.to_hex(),
                    report: held.report.clone(),
                    received_at_epoch_seconds: held.received,
                    via: held.via.map(|via| via.to_hex()),
                    silent: !self.linked(*node) && self.silent(held.received, time),
                })
                .collect(),
        ))
    }
    /// The next time, in epoch seconds after `time`, a device turns silent.
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

    /// Unanswered calls and failed pushes become alerts only once they
    /// last `STANDING_FAILURE`, keep their start as `since`, and clear with
    /// an answer or an acknowledged push; a skew keeps its first `since`.
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn standing_failures_raise_alerts_after_their_bound_and_clear_by_events() {
        let observations = crate::status::Observations::default();
        let coordination = Coordination::new(observations.clone());
        let revision = || *observations.subscribe().borrow();
        let standing = STANDING_FAILURE.as_secs();
        let kinds = |coordination: &Coordination| -> Vec<(AlertKind, Option<u64>)> {
            coordination
                .alerts()
                .into_iter()
                .map(|alert| (alert.kind, alert.since))
                .collect()
        };
        coordination.observe(Err(&CoordinationError::Unavailable), 1000);
        coordination.observe(Err(&CoordinationError::Unavailable), 1000 + standing - 1);
        assert!(kinds(&coordination).is_empty());
        coordination.observe(Err(&CoordinationError::Unavailable), 1000 + standing);
        assert_eq!(
            kinds(&coordination),
            [(AlertKind::CoordinatorUnreachable, Some(1000))]
        );
        let before = revision();
        coordination.observe(
            Err(&CoordinationError::ClockSkew {
                coordinator_time: 5000,
            }),
            2000,
        );
        coordination.observe(
            Err(&CoordinationError::ClockSkew {
                coordinator_time: 5000,
            }),
            2010,
        );
        assert_ne!(revision(), before);
        assert_eq!(
            kinds(&coordination),
            [(
                AlertKind::CoordinatorClockSkew {
                    seconds: 2990,
                    ahead: false
                },
                Some(2000)
            )]
        );
        assert_eq!(
            coordination.failure(),
            Some(CoordinationFailure {
                code: "clock_skew".into(),
                at_epoch_seconds: 2010
            })
        );
        coordination.observe(Ok(()), 2020);
        assert!(kinds(&coordination).is_empty());
        coordination.observe_push(false, 3000);
        assert!(kinds(&coordination).is_empty());
        coordination.observe_push(false, 3000 + standing);
        assert_eq!(
            kinds(&coordination),
            [(AlertKind::RevocationsUnpushed, Some(3000))]
        );
        coordination.observe_push(true, 3000 + standing + 1);
        assert!(kinds(&coordination).is_empty());
    }
}
