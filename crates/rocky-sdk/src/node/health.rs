//! Mesh health: standing coordination failures as this device's alerts,
//! and each device's signed report to the domain's trusted nodes over its
//! live links to them. Trusted nodes forward what they receive to each
//! other. Reports stay in memory; a restarted trusted node learns them again
//! within `REFRESH`.
use super::*;
use crate::status::{
    Alert, AlertKind, BlockerCount, CoordinationFailure, HealthReport, MeshDevice, SILENT_AFTER,
    STANDING_FAILURE,
};
use live_links::io::Writer;
use rocky_core::CoordinationError;
use rocky_credentials::{Signature, SignedCapability};
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
const VERSION: u32 = 1;

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

/// One health message on a live link.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u32,
    message: HealthMessage,
}
#[derive(Serialize, Deserialize)]
enum HealthMessage {
    /// The sender collects reports: a trusted node shows the owner's
    /// trusted grant to its identity; the owner's own chain shows it.
    Collector {
        trust: Option<Box<SignedCapability>>,
    },
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
    /// This device's alerts: clocks its peers disagree with and standing
    /// coordination failures.
    pub(super) fn alerts(&self) -> Vec<Alert> {
        let mut alerts: Vec<Alert> = self
            .clocks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .chain(self.coordination.alerts())
            .collect();
        alerts.sort();
        alerts
    }
    /// Whether this device collects health reports: the owner, or a node of
    /// an identity holding the owner's trusted grant, which it shows.
    async fn collects(&self) -> Option<Option<SignedCapability>> {
        let settings = self.settings.lock().await;
        if settings.owner() {
            return Some(None);
        }
        settings
            .trusted_links()
            .map(|links| links.0.into_iter().next())
    }
    /// Whether `peer`, admitted with `chain`, is the owner's node or one of
    /// an identity `trust` shows the owner trusts.
    fn proves_collector(
        &self,
        settings: &Settings,
        peer: PublicKey,
        chain: &CredentialChain,
        trust: Option<SignedCapability>,
    ) -> Result<bool, Failure> {
        let identity = chain.provenance(settings.publisher, peer)?.subject;
        if identity == settings.publisher {
            return Ok(true);
        }
        let Some(trust) = trust else {
            return Ok(false);
        };
        Ok(CredentialChain(vec![trust])
            .trusted_identity(
                settings.publisher,
                self.domain().scope,
                &settings.revocations,
                now()?,
            )
            .is_ok_and(|trusted| trusted == identity))
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
        if let Some(trust) = self.collects().await
            && let Err(error) = send(HealthMessage::Collector {
                trust: trust.map(Box::new),
            })
            .await
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
            let trusted = self.collects().await.is_some();
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
            HealthMessage::Collector { trust } => {
                let settings = self.settings.lock().await;
                if !self.proves_collector(&settings, peer, chain, trust.map(|trust| *trust))? {
                    return Err("the peer is no trusted node".into());
                }
                collector.send_replace(true);
                return Ok(());
            }
            HealthMessage::Report { report, received } => (*report, received),
        };
        #[cfg(test)]
        self.reports_received.fetch_add(1, Ordering::Relaxed);
        if self.collects().await.is_none() {
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
        if self.collects().await.is_none() {
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
