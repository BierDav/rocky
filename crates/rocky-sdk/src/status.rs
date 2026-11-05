//! Observable native SDK contract. Bindings are generated from these declarations.
pub use rocky_core::{Blocker, BlockerKind, ClearsBy, FrameReplay, WaitReason};
use serde::{Deserialize, Serialize};

mod attempts;
pub(crate) use attempts::{Attempt, Observations, epoch_millis, failure};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentState {
    Detached,
    Attached,
    Draining,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    Open,
    Opening,
    Closing,
    Closed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryPathState {
    On,
    Off,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinationMode {
    NotConfigured,
    Service,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOperation {
    ResolveContact,
    OpenLink,
    Reconcile,
    CoordinatorRegister,
    CoordinatorControl,
    CoordinatorClose,
    CoordinatorUsage,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStage {
    ResolveRoute,
    Dial,
    Admission,
    Reconciliation,
    CoordinatorRequest,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Completed,
    NoResult,
    Cancelled,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Timeout,
    Transport,
    RemoteRefusal,
    LocalAuthorization,
    Protocol,
    Coordination,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    NetworkingDisabled,
    Suspended,
    Backoff,
    InboundReconciliation,
    Covered,
    NoPendingWork,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptFailure {
    pub kind: FailureKind,
    pub code: Option<String>,
    pub detail: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptObservation {
    pub id: u64,
    pub target: String,
    pub operation: AttemptOperation,
    pub stage: AttemptStage,
    pub started_at_epoch_millis: Option<u64>,
    pub ended_at_epoch_millis: Option<u64>,
    pub elapsed_millis: Option<u64>,
    pub outcome: Option<AttemptOutcome>,
    pub failure: Option<AttemptFailure>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub transport: String,
    pub address: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationStatus {
    pub digest: String,
    pub statements: u64,
    /// Held statements the coordination service has not acknowledged; a
    /// node pushes them at its next sync or claim. Only meaningful on a
    /// database composed with a coordination service.
    pub unpushed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusGrant {
    pub scope: String,
    pub rights: Vec<String>,
    pub columns: Option<Vec<String>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityProfile {
    pub kind: String,
    pub scopes: Vec<String>,
    pub grants: Vec<StatusGrant>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedRevocation {
    pub id: String,
    pub statement: Vec<u8>,
    pub reason: RevocationReason,
}
/// Why a verified revocation ends this device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RevocationReason {
    /// This device's node.
    Node,
    /// This device's identity.
    Identity,
    /// The root revoked the trusted identity `trusted` that first enrolled
    /// this identity: this device did nothing wrong and may enroll again
    /// through another trusted node.
    Origin { trusted: String },
    /// A root recovery that kept the trusted identity `recovered` ended
    /// every trusted grant before its own: this device's, or that of the
    /// trusted identity that first enrolled it. It may enroll again
    /// through a trusted node the recovery kept or the root added since.
    Recovery { recovered: String },
    /// A capability or an issuer above it.
    Capability,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenewalRoute {
    pub target: String,
    pub routes: Vec<Route>,
    pub enrollment_routes: Vec<Route>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityStatus {
    pub principal: String,
    pub node: String,
    pub is_issuer: bool,
    pub expires_at_epoch_seconds: u64,
    pub expired: bool,
    pub logged_out: bool,
    pub verified_revocation: Option<VerifiedRevocation>,
    pub revocations: RevocationStatus,
    pub profile: AuthorityProfile,
    pub renewal_routes: Vec<RenewalRoute>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    pub schema: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebuildProgress {
    pub schema: String,
    pub tables: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingGeneration {
    pub tip: String,
    pub manifest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationStatus {
    pub manifest: String,
    pub current: Generation,
    pub pending: Option<PendingGeneration>,
    pub rebuilding: Option<RebuildProgress>,
    pub application: DeclaredSchema,
    /// A migration this device waits for or publishes.
    pub migration: Option<MigrationStatus>,
    /// An ahead member needs an issuer's signed generation, possibly offline.
    pub waiting_for_issuer: bool,
    /// Whether this Rocky build may replay the followed tip.
    pub engine: EngineStatus,
}
/// How this Rocky build's replay engine (its SQLite and replay semantics)
/// stands to the tips this device follows. Replicas on different engines
/// could replay the same transaction differently, so a tip names one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    Current,
    /// The owner moved on to a tip on another engine: this device stays
    /// read-only at the old tip until the application ships a Rocky upgrade.
    UpgradeRequired,
    /// The followed tip names another engine: this device reads but does
    /// not write or replay until an owner device on this build opens a tip.
    /// Engines are not ordered, so an older build that enrolled at a newer
    /// tip reports this too; upgrading also clears it.
    AwaitingOwner,
}
impl From<rocky_secure::EngineStatus> for EngineStatus {
    fn from(status: rocky_secure::EngineStatus) -> Self {
        match status {
            rocky_secure::EngineStatus::Current => Self::Current,
            rocky_secure::EngineStatus::UpgradeRequired => Self::UpgradeRequired,
            rocky_secure::EngineStatus::AwaitingOwner => Self::AwaitingOwner,
        }
    }
}
/// A schema migration as this device sees it. See
/// `docs/runtime/generations.md#migrations`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MigrationStatus {
    /// The application's schema has migrations this device may not run:
    /// an issuer that reads and writes every partition must migrate first.
    FullAccessRequired,
    /// Bundles of migrations this device ran, and how many of them, in
    /// order, a peer holds.
    Publishing { bundles: u64, published: u64 },
}
/// How the schema the application declares stands to the installed
/// generation's. An application behind or ahead of its database still
/// opens it; its statements on what differs fail. See
/// `docs/runtime/adopting-generations.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredSchema {
    Current,
    /// Older than the database, which adopted a newer generation.
    Behind,
    /// Newer than the database: no issuer has signed it, or this device
    /// could not reach one yet.
    Ahead,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapProgress {
    pub rows_done: u64,
    pub rows_total: u64,
    pub scopes_done: u64,
    pub scopes_total: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeStatus {
    pub table: Option<String>,
    pub table_id: String,
    pub encryption: String,
    pub partition: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeImportStatus {
    pub scope: ScopeStatus,
    pub checkpoint: Option<String>,
    pub cursor: Option<String>,
    pub pages_complete: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptionEpoch {
    pub generation: u64,
    pub secret_hash: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEpochStatus {
    pub collection: String,
    pub epochs: Vec<EncryptionEpoch>,
    pub current: Option<EncryptionEpoch>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataStatus {
    pub revision: u64,
    pub generation: GenerationStatus,
    /// Every reason a scope cannot read, write or import yet.
    pub blockers: Vec<Blocker>,
    /// A summary: the bootstrap intent or a staged import remains.
    pub bootstrap_pending: bool,
    pub bootstrap: Option<BootstrapProgress>,
    pub imports: Vec<ScopeImportStatus>,
    pub key_epochs: Vec<KeyEpochStatus>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryPath {
    pub state: DiscoveryPathState,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryStatus {
    pub announcing: bool,
    pub local: DiscoveryPath,
    pub topic: DiscoveryPath,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPath {
    pub selected: bool,
    pub kind: String,
    pub remote: String,
    pub local: Option<String>,
    pub round_trip_micros: u64,
    pub mtu: u32,
    pub window: u64,
    pub sent_bytes: u64,
    pub sent_datagrams: u64,
    pub received_bytes: u64,
    pub received_datagrams: u64,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub congestion_events: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerLink {
    pub state: LinkState,
    pub opened_at_epoch_seconds: Option<u64>,
    pub round_trip_millis: Option<u64>,
    pub paths: Vec<LinkPath>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryStatus {
    pub pending: bool,
    pub reconciliation_retry_at_epoch_millis: Option<u64>,
    pub link_retry_at_epoch_millis: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub node: String,
    pub is_issuer: bool,
    pub is_configured: bool,
    pub source: String,
    pub choice: String,
    pub routes: Vec<Route>,
    pub is_local: bool,
    pub link: PeerLink,
    pub attempts: Vec<AttemptObservation>,
    pub last_authenticated_exchange_at_epoch_seconds: Option<u64>,
    pub retry: Option<RetryStatus>,
    /// The last exchange with this peer that was not skipped.
    pub last_sync: Option<PeerReconciliation>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayStatus {
    pub url: String,
    pub connected: bool,
    pub last_error: Option<String>,
    pub auth_denied_reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayObservation {
    pub initial_configuration: Option<String>,
    /// None when the attached transport cannot expose relay observations.
    pub relays: Option<Vec<RelayStatus>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkOwnership {
    Rocky,
    Application,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundStatus {
    pub active: u64,
    pub limit: u64,
    pub served: u64,
    pub refused: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub attachment: AttachmentState,
    pub kind: Option<String>,
    pub ownership: Option<NetworkOwnership>,
    pub networking_enabled: bool,
    pub foreground: bool,
    pub worker_enabled: bool,
    pub worker_running: bool,
    pub reconciling: bool,
    pub inbound: Option<InboundStatus>,
    pub peers: Vec<Peer>,
    pub connected_peer_count: u64,
    pub contact_resolutions: Vec<AttemptObservation>,
    pub discovery: DiscoveryStatus,
    pub relay: RelayObservation,
    pub last_reconciliation: Option<ReconciliationReport>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinationStatus {
    pub mode: CoordinationMode,
    pub attempts: Vec<AttemptObservation>,
}
/// A condition that delays or degrades replication but blocks no scope;
/// blockers stay for scopes. Later conditions add kinds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Alert {
    pub kind: AlertKind,
    /// Epoch seconds since the condition holds, when known.
    pub since: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertKind {
    /// This device's clock is `seconds` ahead of `node`'s, more than
    /// `MAX_CLOCK_AHEAD`: this device's changes wait on that device until
    /// its clock catches up.
    ClockAheadOfPeer { node: String, seconds: u64 },
    /// `node`'s clock is `seconds` ahead of this device's, more than
    /// `MAX_CLOCK_AHEAD`: its changes wait here until this clock catches up
    /// (a `ClockAhead` blocker once they arrive).
    PeerClockAhead { node: String, seconds: u64 },
    /// The coordination service refused this device's last call because
    /// its clock is `seconds` off the service's (`ahead` when this one
    /// runs ahead). Clears with the next call the service accepts, or
    /// refuses for a reason it checks after the clock.
    CoordinatorClockSkew { seconds: u64, ahead: bool },
    /// No voter of the coordination service answered this device's calls
    /// for `STANDING_FAILURE` or longer. Clears with the next answer.
    CoordinatorUnreachable,
    /// The coordination service has not acknowledged the revocations this
    /// device holds for `STANDING_FAILURE` or longer: claim rights they end
    /// still pass there. Clears when a push is acknowledged.
    RevocationsUnpushed,
    /// On a trusted node: no node of the trusted identity `identity` was
    /// heard from, directly or through another trusted node, for the
    /// configured threshold (`TRUSTED_STALE_AFTER` by default) since
    /// `last_heard_epoch_seconds`. Ask the user whether the root should
    /// revoke it; clears once it is heard again or revoked.
    TrustedNodeStale {
        identity: String,
        last_heard_epoch_seconds: u64,
    },
    /// On a trusted node: a seal is due and was not made, for `reason`:
    /// `no_followed_bookmark`, `no_tip_authority: …` or `failed: …`.
    /// Clears with the next seal pass that has nothing left undone.
    SealSkipped { reason: String },
    /// A root revocation's cut left out `statements` tip statements and
    /// `acknowledgments` terminal acknowledgments that the identity
    /// `signer` signed through its node `node` past the kept head; this
    /// device holds them and resolves without them.
    TipStatementsPastCut {
        signer: String,
        node: String,
        statements: u64,
        acknowledgments: u64,
    },
    /// The identity `signer`'s node `node` signed two statements (or
    /// acknowledgments) at `sequence` of its chain: its key signed a fork.
    /// Ask the user whether the root should revoke it.
    TipChainEquivocation {
        signer: String,
        node: String,
        sequence: u64,
        acknowledgment: bool,
    },
    /// The peer `node` carried `records` root escrow records this device
    /// refused, the latest because `reason`: no key holder of their scope
    /// vouched for them, or a later revocation refused the one that did.
    /// A genuine record is never displaced by one.
    EscrowRefused {
        node: String,
        records: u64,
        reason: String,
    },
    /// Two root recoveries met: the later, which kept the identity `kept`,
    /// ended every trusted grant before it, the earlier recovered identity
    /// `ended` too, whose writes every holder refuses from then on. With
    /// `shared_slot` both took the same author numbers. Recover once; a
    /// device of `ended` enrolls again through `kept`.
    RecoveriesMet {
        kept: String,
        ended: String,
        shared_slot: bool,
    },
    /// The root recovery that kept the trusted identity `recovered` ended
    /// the trusted identity `ended`, whose grant came before it, and with
    /// it every identity `ended` first enrolled: their devices are refused
    /// (each reports `RevocationReason::Recovery`) and enroll again through
    /// `recovered` or a trusted node the root added since. Reported for
    /// every trusted identity this device knows, from the issuer registry
    /// or the tip statements it holds, whether it learns of it before the
    /// statement or after.
    RecoveryEnded { recovered: String, ended: String },
}
/// How long a trusted node may stay unheard before the others flag it.
pub const TRUSTED_STALE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(30 * 24 * 60 * 60);
/// How long a coordination failure lasts before it is an alert.
pub const STANDING_FAILURE: std::time::Duration = std::time::Duration::from_secs(300);
/// What a device reports of its own health to the domain's trusted nodes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthReport {
    /// The reporting device's clock when it produced the report.
    pub produced_at_epoch_seconds: u64,
    pub alerts: Vec<Alert>,
    /// Its blockers by kind, without their scopes.
    pub blockers: Vec<BlockerCount>,
    pub coordination_failure: Option<CoordinationFailure>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockerCount {
    /// The blocker kind's `type`, as `BlockerKind` names it.
    pub kind: String,
    pub count: u64,
}
/// The last refusal or failure of a call to the coordination service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinationFailure {
    /// `CoordinationError::category`, such as `unavailable` or `clock_skew`.
    pub code: String,
    pub at_epoch_seconds: u64,
}
/// One device's latest report, as a trusted node holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshDevice {
    pub node: String,
    pub report: HealthReport,
    /// When a trusted node last heard from the device, on its own clock:
    /// a report, or its link to the device open or closing.
    pub received_at_epoch_seconds: u64,
    /// The trusted node that forwarded it; `None` when the device sent it here.
    pub via: Option<String>,
    /// No trusted node has heard from this device for `SILENT_AFTER`: no
    /// link to it is open and none was since `received_at_epoch_seconds`.
    pub silent: bool,
}
/// How long trusted nodes may not hear from a device before it shows silent.
pub const SILENT_AFTER: std::time::Duration = std::time::Duration::from_secs(300);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseStatus {
    pub token: String,
    pub observed_at_epoch_millis: Option<u64>,
    pub authority: AuthorityStatus,
    pub data: DataStatus,
    pub network: NetworkStatus,
    pub coordination: CoordinationStatus,
    /// Each condition the application should know of that no blocker shows.
    pub alerts: Vec<Alert>,
    /// On a trusted node, each other device's latest health report; `None`
    /// on any other device, which sees only its own status.
    pub mesh: Option<Vec<MeshDevice>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeReconciliationTransfer {
    pub scope: ScopeStatus,
    /// Frames received.
    pub pulled: u64,
    /// Frames sent.
    pub pushed: u64,
    pub received_bytes: u64,
    pub sent_bytes: u64,
    pub checkpoint: Option<String>,
    pub superseded: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerReconciliation {
    pub node: String,
    pub attempts: Vec<AttemptObservation>,
    pub skip: Option<SkipReason>,
    pub transfers: Vec<ScopeReconciliationTransfer>,
    pub refused: Vec<ScopeStatus>,
    pub waiting: Vec<ScopeStatus>,
    /// Always false: peer discovery never upgrades the installed workspace.
    pub upgrade_required: bool,
    pub retry_pending: bool,
    /// How each scope this exchange reached ended.
    pub scopes: Vec<ScopeOutcome>,
    /// Failed attempts of this exchange that timed out, by kind.
    pub timeouts: Vec<TimeoutCount>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeSyncOutcome {
    Synced,
    /// One side's credentials grant no whole-scope access to the other.
    Refused,
    /// The peer restores the scope from a checkpoint.
    PeerImporting,
    /// The exchange failed while this scope was syncing.
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeOutcome {
    pub scope: ScopeStatus,
    pub outcome: ScopeSyncOutcome,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeoutCount {
    pub kind: String,
    pub count: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationReport {
    pub contact_resolutions: Vec<AttemptObservation>,
    pub started_at_epoch_millis: Option<u64>,
    pub ended_at_epoch_millis: Option<u64>,
    pub skip: Option<SkipReason>,
    pub peers: Vec<PeerReconciliation>,
}

/// The native request boundary's stable error, without losing its code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallFailure {
    pub code: String,
    pub retryable: bool,
    pub detail: String,
}
impl std::fmt::Display for CallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for CallFailure {}
