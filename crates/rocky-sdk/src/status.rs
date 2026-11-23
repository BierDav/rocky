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
    pub unpushed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedRevocation {
    pub id: String,
    pub statement: Vec<u8>,
    pub reason: RevocationReason,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RevocationReason {
    Node,
    Identity,
    Cascade { trusted: String },
    Capability,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenewalRoute {
    pub target: String,
    pub routes: Vec<Route>,
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
    pub profile: crate::GrantProfile,
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
    pub follow_required: bool,
    pub held: usize,
    pub manifest: String,
    pub current: Generation,
    pub pending: Option<PendingGeneration>,
    pub rebuilding: Option<RebuildProgress>,
    pub application: DeclaredSchema,
    pub migration: Option<MigrationStatus>,
    pub waiting_for_issuer: bool,
    pub engine: EngineStatus,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    Current,
    UpgradeRequired,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MigrationStatus {
    FullAccessRequired,
    Publishing { bundles: u64, published: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredSchema {
    Current,
    Behind,
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
    pub blockers: Vec<Blocker>,
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
pub struct PeerLink {
    pub state: LinkState,
    pub opened_at_epoch_seconds: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryStatus {
    pub pending: bool,
    pub reconciliation_retry_at_epoch_millis: Option<u64>,
    pub link_retry_at_epoch_millis: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerSource {
    Application,
    Enrollment,
    Session,
    Link,
    Discovery,
    Handover,
    Request,
}
impl PeerSource {
    pub(crate) fn from_str_or_default(source: &str) -> Self {
        match source {
            "application" => Self::Application,
            "enrollment" => Self::Enrollment,
            "session" => Self::Session,
            "link" => Self::Link,
            "discovery" => Self::Discovery,
            "handover" => Self::Handover,
            "request" => Self::Request,
            _ => Self::Enrollment,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerChoice {
    NotConsidered,
    Backoff,
    PassLimit,
    SelectedLocal,
    SelectedHolding,
    SelectedTrusted,
    SelectedProbe,
    Covered,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub node: String,
    pub is_issuer: bool,
    pub is_configured: bool,
    pub source: PeerSource,
    pub choice: PeerChoice,
    pub routes: Vec<Route>,
    pub is_local: bool,
    pub link: PeerLink,
    pub attempts: Vec<AttemptObservation>,
    pub last_authenticated_exchange_at_epoch_seconds: Option<u64>,
    pub retry: Option<RetryStatus>,
    pub last_sync: Option<PeerReconciliation>,
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
    pub last_reconciliation: Option<ReconciliationReport>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinationStatus {
    pub mode: CoordinationMode,
    pub attempts: Vec<AttemptObservation>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Alert {
    pub kind: AlertKind,
    pub since: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum SealSkip {
    NoFollowedBookmark,
    NoTipAuthority(String),
    CoordinatorRequired(String),
    CoordinatorMismatch,
    SealPending(String),
    Failed(String),
}
impl SealSkip {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoFollowedBookmark => "no_followed_bookmark",
            Self::NoTipAuthority(_) => "no_tip_authority",
            Self::CoordinatorRequired(_) => rocky_secure::COORDINATOR_REQUIRED,
            Self::CoordinatorMismatch => rocky_secure::COORDINATOR_MISMATCH,
            Self::SealPending(_) => "seal_pending",
            Self::Failed(_) => "failed",
        }
    }
}
impl std::fmt::Display for SealSkip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFollowedBookmark | Self::CoordinatorMismatch => f.write_str(self.code()),
            Self::NoTipAuthority(detail)
            | Self::CoordinatorRequired(detail)
            | Self::SealPending(detail)
            | Self::Failed(detail) => write!(f, "{}: {detail}", self.code()),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum FinalityStall {
    LogDropped { tip: String },
    EndRefused { tip: String, error: String },
    EntryPending { tip: String },
    UnsupportedFormat { error: String },
}
impl FinalityStall {
    pub(crate) fn of(stall: rocky_secure::FinalityStall) -> Self {
        let hex = |tip: rocky_core::TipId| data_encoding::HEXLOWER.encode(&tip.0);
        match stall {
            rocky_secure::FinalityStall::LogDropped { tip } => Self::LogDropped { tip: hex(tip) },
            rocky_secure::FinalityStall::EndRefused { tip, error } => Self::EndRefused {
                tip: hex(tip),
                error,
            },
            rocky_secure::FinalityStall::EntryPending { tip } => {
                Self::EntryPending { tip: hex(tip) }
            }
        }
    }
}
impl std::fmt::Display for FinalityStall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LogDropped { tip } => write!(f, "log_dropped at {tip}"),
            Self::EndRefused { tip, error } => write!(f, "end_refused at {tip}: {error}"),
            Self::EntryPending { tip } => write!(f, "entry_pending at {tip}"),
            Self::UnsupportedFormat { error } => write!(f, "unsupported_format: {error}"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertKind {
    MembershipFork {
        identity: String,
        sequence: u64,
    },
    ClockAheadOfPeer {
        node: String,
        seconds: u64,
    },
    PeerClockAhead {
        node: String,
        seconds: u64,
    },
    CoordinatorClockSkew {
        seconds: u64,
        ahead: bool,
    },
    CoordinatorUnreachable,
    RevocationsUnpushed,
    TrustedNodeLinked {
        identity: String,
        node: String,
        linked: u64,
    },
    TrustedNodeUnlinked {
        identity: String,
        node: String,
        unlinked: u64,
    },
    TrustedIdentityStale {
        identity: String,
        last_heard_epoch_seconds: u64,
    },
    SealSkipped {
        reason: SealSkip,
    },
    FinalityStalled {
        reason: FinalityStall,
    },
    TipStatementsPastCut {
        signer: String,
        node: String,
        statements: u64,
        acknowledgments: u64,
    },
    TipChainEquivocation {
        signer: String,
        node: String,
        sequence: u64,
        acknowledgment: bool,
    },
}
pub const TRUSTED_STALE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(30 * 24 * 60 * 60);
pub const STANDING_FAILURE: std::time::Duration = std::time::Duration::from_secs(300);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthReport {
    pub produced_at_epoch_seconds: u64,
    pub alerts: Vec<Alert>,
    pub blockers: Vec<BlockerCount>,
    pub coordination_failure: Option<CoordinationFailure>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockerCount {
    pub kind: String,
    pub count: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinationFailure {
    pub code: String,
    pub at_epoch_seconds: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshDevice {
    pub node: String,
    pub report: HealthReport,
    pub received_at_epoch_seconds: u64,
    pub via: Option<String>,
    pub silent: bool,
}
pub const SILENT_AFTER: std::time::Duration = std::time::Duration::from_secs(300);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseStatus {
    pub token: String,
    pub observed_at_epoch_millis: Option<u64>,
    pub authority: AuthorityStatus,
    pub data: DataStatus,
    pub network: NetworkStatus,
    pub coordination: CoordinationStatus,
    pub alerts: Vec<Alert>,
    pub mesh: Option<Vec<MeshDevice>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeReconciliationTransfer {
    pub scope: ScopeStatus,
    pub pulled: u64,
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
    pub upgrade_required: bool,
    pub retry_pending: bool,
    pub scopes: Vec<ScopeOutcome>,
    pub timeouts: Vec<TimeoutCount>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeSyncOutcome {
    Synced,
    Refused,
    PeerImporting,
    Failed,
    Interrupted,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallFailure {
    pub code: String,
    pub retryable: bool,
    pub detail: String,
    pub coordinator_time: Option<u64>,
    pub blockers: Vec<rocky_core::Blocker>,
}
impl CallFailure {
    pub fn from_failure(error: &crate::Failure) -> Self {
        crate::node::call_failure(error)
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn retryable(&self) -> bool {
        self.retryable
    }
    pub(crate) fn boxed(error: crate::Failure) -> crate::Failure {
        if error.is::<crate::ServiceError>() {
            error
        } else {
            Box::new(Self::from_failure(&error))
        }
    }
}
impl std::fmt::Display for CallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for CallFailure {}
