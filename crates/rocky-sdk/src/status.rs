use crate::Timestamp;
pub use rocky_core::{BlockerKind, ClearsBy, FrameReplay, WaitReason};

/// Data that cannot be read yet, and what clears it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Blocker {
    /// The table or partition it holds back.
    pub scope: String,
    /// What it waits for.
    pub kind: BlockerKind,
    /// Who or what can clear it.
    pub clears_by: ClearsBy,
    /// When it was first seen, or `None` when not recorded.
    pub since: Option<Timestamp>,
}
impl From<&rocky_core::Blocker> for Blocker {
    fn from(blocker: &rocky_core::Blocker) -> Self {
        Self {
            scope: blocker.scope.clone(),
            kind: blocker.kind.clone(),
            clears_by: blocker.clears_by.clone(),
            since: blocker.since.map(Timestamp::from_unix_seconds),
        }
    }
}
impl From<&Blocker> for rocky_core::Blocker {
    fn from(blocker: &Blocker) -> Self {
        Self {
            scope: blocker.scope.clone(),
            kind: blocker.kind.clone(),
            clears_by: blocker.clears_by.clone(),
            since: blocker
                .since
                .and_then(|since| u64::try_from(since.unix_micros() / 1_000_000).ok()),
        }
    }
}
impl std::fmt::Display for Blocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        rocky_core::Blocker::from(self).fmt(f)
    }
}
use serde::{Deserialize, Serialize};

mod attempts;
pub(crate) use attempts::{Attempt, Observations, epoch_millis, failure};

/// Whether a network is attached to the database.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentState {
    /// No network; the database works offline.
    Detached,
    /// A network is attached and serving.
    Attached,
    /// A network is detaching and finishing open exchanges.
    Draining,
}
/// The state of the replication link to one peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    /// The link is open.
    Open,
    /// The link is being opened.
    Opening,
    /// The link is closing.
    Closing,
    /// No link.
    Closed,
}
/// Whether one discovery path runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryPathState {
    /// It runs.
    On,
    /// It is turned off.
    Off,
    /// It was asked for but cannot run; `DiscoveryPath::reason` says why.
    Unavailable,
}
/// What a network attempt tried to do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOperation {
    /// Resolve a contact to routes.
    ResolveContact,
    /// Open a replication link.
    OpenLink,
    /// Reconcile data with a peer.
    Reconcile,
}
/// How far a network attempt got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStage {
    /// Finding a route.
    ResolveRoute,
    /// Dialing the route.
    Dial,
    /// Being admitted by the peer.
    Admission,
    /// Exchanging data.
    Reconciliation,
}
/// How a network attempt ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// It finished.
    Completed,
    /// It ended without an answer.
    NoResult,
    /// It was cancelled.
    Cancelled,
    /// It failed; `AttemptObservation::failure` says why.
    Failed,
}
/// The class of a failed network attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// It timed out.
    Timeout,
    /// The transport failed.
    Transport,
    /// The peer refused.
    RemoteRefusal,
    /// This device lacked the credentials.
    LocalAuthorization,
    /// The peer broke the protocol.
    Protocol,
    /// Any other failure.
    Other,
}
/// Why a sync pass skipped a peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The peer failed recently and waits out a backoff.
    Backoff,
    /// The peer is already reconciling with this device.
    InboundReconciliation,
    /// Another peer in the pass already supplied what this one would.
    Covered,
    /// Nothing needs exchanging with the peer.
    NoPendingWork,
}

/// Why a network attempt failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptFailure {
    /// The class of failure.
    pub kind: FailureKind,
    /// A stable error code, when the failure has one.
    pub code: Option<String>,
    /// A human-readable explanation.
    pub detail: String,
}
/// One network attempt and how it went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptObservation {
    /// Orders attempts within a status.
    pub id: u64,
    /// The peer or contact it targeted.
    pub target: String,
    /// What it tried.
    pub operation: AttemptOperation,
    /// How far it got.
    pub stage: AttemptStage,
    /// When it started, if known.
    pub started_at_epoch_millis: Option<u64>,
    /// When it ended, or `None` while it runs.
    pub ended_at_epoch_millis: Option<u64>,
    /// How long it took.
    pub elapsed_millis: Option<u64>,
    /// How it ended, or `None` while it runs.
    pub outcome: Option<AttemptOutcome>,
    /// Why it failed, if it did.
    pub failure: Option<AttemptFailure>,
}
/// A URL a peer is dialed at, such as `tcp://10.0.0.2:4000`, `wss://sync.example/rocky` or an
/// `iroh:` route; the transport that claims its scheme dials it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Route(String);
impl Route {
    pub(crate) const MAX_LEN: usize = 2048;
    /// The URL.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Its scheme, which names the transport that dials it.
    pub fn scheme(&self) -> &str {
        rocky_core::net::scheme(&self.0).unwrap_or_default()
    }
}
impl TryFrom<String> for Route {
    type Error = crate::Failure;
    fn try_from(url: String) -> Result<Self, Self::Error> {
        if url.len() > Self::MAX_LEN || rocky_core::net::scheme(&url).is_none() {
            return Err(format!("a route is a URL of at most {} bytes", Self::MAX_LEN).into());
        }
        Ok(Self(url))
    }
}
impl std::str::FromStr for Route {
    type Err = crate::Failure;
    fn from_str(url: &str) -> Result<Self, Self::Err> {
        url.to_owned().try_into()
    }
}
impl From<Route> for String {
    fn from(route: Route) -> Self {
        route.0
    }
}
impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
/// The revocations this device holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationStatus {
    /// A digest of the set, equal on devices that hold the same revocations.
    pub digest: String,
    /// Revocation statements held.
    pub statements: u64,
}
/// The revocation that ended this device's access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedRevocation {
    /// The revocation's id.
    pub id: String,
    /// The signed statement.
    #[serde(with = "rocky_core::byte_arrays::one")]
    pub statement: Vec<u8>,
    /// What it revoked.
    pub reason: RevocationReason,
}
/// What a revocation revoked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RevocationReason {
    /// This device was unlinked.
    Node,
    /// This device's person was removed.
    Identity,
    /// An admin whose credentials this device's chain depends on was revoked.
    Cascade {
        /// The revoked admin.
        admin: crate::PersonId,
    },
    /// This device's credential was revoked, as when access narrows.
    Capability,
}
/// Where this device can renew its credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenewalRoute {
    /// An admin device's key, or the hash a saved contact keeps of it until the device is met.
    pub target: String,
    /// Its routes.
    pub routes: Vec<Route>,
}
/// Who this device is and how long its credentials last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityStatus {
    /// The person.
    pub principal: crate::PersonId,
    /// The device.
    pub node: crate::DeviceId,
    /// Whether this is an admin device that issues credentials.
    pub is_issuer: bool,
    /// When the current credentials expire; syncing with an admin device renews them.
    pub expires_at_epoch_seconds: u64,
    /// Whether they have expired.
    pub expired: bool,
    /// Whether the device logged out.
    pub logged_out: bool,
    /// The revocation that removed this device, or `None` while it holds access.
    pub verified_revocation: Option<VerifiedRevocation>,
    /// The revocations held.
    pub revocations: RevocationStatus,
    /// The person's access.
    pub access: crate::Access,
    /// Admin devices that can renew the credentials.
    pub renewal_routes: Vec<RenewalRoute>,
}
/// The schema version the data is at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    /// The version number.
    pub schema: u32,
}
/// A rebuild of tables after a schema change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebuildProgress {
    /// The digest of the schema being built.
    pub schema: String,
    /// Tables to rebuild.
    pub tables: u64,
}
/// A published schema version this device has not switched to yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingGeneration {
    /// The statement that published it.
    pub tip: String,
    /// Its manifest digest.
    pub manifest: String,
}
/// Where the database's schema stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationStatus {
    /// Whether the device has not yet followed an admin's schema publications and needs a sync with
    /// one.
    pub follow_required: bool,
    /// Changes held back because they do not apply yet, such as writes for a schema version not
    /// installed.
    pub held: usize,
    /// The installed manifest digest.
    pub manifest: String,
    /// The installed version.
    pub current: Generation,
    /// A published version not yet switched to.
    pub pending: Option<PendingGeneration>,
    /// A rebuild in progress.
    pub rebuilding: Option<RebuildProgress>,
    /// How the application's declared version compares with the installed one.
    pub application: DeclaredSchema,
    /// The state of a migration to the declared version.
    pub migration: Option<MigrationStatus>,
    /// Whether a newer declared version waits for an admin device to publish it.
    pub waiting_for_issuer: bool,
    /// Whether this build of Rocky can run the installed version.
    pub engine: EngineStatus,
}
/// Whether this build of Rocky can run the database's schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    /// It can.
    Current,
    /// A pending schema needs a newer Rocky; upgrade the application.
    UpgradeRequired,
    /// The engine changed and waits for the owner to publish the matching schema.
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
/// The state of a migration to the declared schema version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MigrationStatus {
    /// Migrating needs a device with full access, so this one waits for an admin.
    FullAccessRequired,
    /// The migrated data is being published.
    Publishing {
        /// Bundles to publish.
        bundles: u64,
        /// Bundles published so far.
        published: u64,
    },
}
/// How the application's declared schema version compares with the installed one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredSchema {
    /// They match.
    Current,
    /// The installed version is newer; the application keeps working unless the newer one removed
    /// what it uses.
    Behind,
    /// The application declares a newer version, which an admin device publishes.
    Ahead,
}
/// Progress of the first download of the data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapProgress {
    /// Rows imported.
    pub rows_done: u64,
    /// Rows to import.
    pub rows_total: u64,
    /// Tables or partitions imported.
    pub scopes_done: u64,
    /// Tables or partitions to import.
    pub scopes_total: u64,
}
/// One synced unit: a table, or one partition of a table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeStatus {
    /// The table name, or `None` for internal data.
    pub table: Option<String>,
    /// The table's internal id.
    pub table_id: String,
    /// The id of the key that encrypts it.
    pub encryption: String,
    /// Whether it is one partition of a table.
    pub partition: bool,
}
/// An import of one scope from a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeImportStatus {
    /// The scope.
    pub scope: ScopeStatus,
    /// The checkpoint being imported.
    pub checkpoint: Option<String>,
    /// How far the import got.
    pub cursor: Option<String>,
    /// Whether every page arrived.
    pub pages_complete: Option<bool>,
}
/// One key of a collection's key history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptionEpoch {
    /// Rises with each rotation.
    pub generation: u64,
    /// Identifies the key without revealing it.
    pub secret_hash: String,
}
/// The keys this device holds for one collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEpochStatus {
    /// The table or partition group.
    pub collection: String,
    /// Every key held.
    pub epochs: Vec<EncryptionEpoch>,
    /// The key new writes use.
    pub current: Option<EncryptionEpoch>,
}
/// Where the local data stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataStatus {
    /// Rises with every local change.
    pub revision: u64,
    /// The schema.
    pub generation: GenerationStatus,
    /// Why some data cannot be read yet and what clears each; empty when everything is readable.
    pub blockers: Vec<rocky_core::Blocker>,
    /// Whether the first download has not finished.
    pub bootstrap_pending: bool,
    /// Progress of the first download.
    pub bootstrap: Option<BootstrapProgress>,
    /// Checkpoint imports in progress.
    pub imports: Vec<ScopeImportStatus>,
    /// Keys held per collection.
    pub key_epochs: Vec<KeyEpochStatus>,
    /// Whether each partition the device should hold is held and complete.
    pub partitions: Vec<crate::PartitionProgress>,
}
/// One discovery path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryPath {
    /// Whether it runs.
    pub state: DiscoveryPathState,
    /// Why it is unavailable.
    pub reason: Option<String>,
}
/// How this device finds peers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryStatus {
    /// Whether it announces itself.
    pub announcing: bool,
    /// Local-network discovery.
    pub local: DiscoveryPath,
    /// Discovery through the database's rendezvous record.
    pub topic: DiscoveryPath,
}
/// The replication link to one peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerLink {
    /// Its state.
    pub state: LinkState,
    /// When it opened.
    pub opened_at_epoch_seconds: Option<u64>,
}
/// When a failed peer is tried again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryStatus {
    /// Whether a retry is scheduled.
    pub pending: bool,
    /// When reconciliation is retried.
    pub reconciliation_retry_at_epoch_millis: Option<u64>,
    /// When the link is reopened.
    pub link_retry_at_epoch_millis: Option<u64>,
}
/// How this device learned of a peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerSource {
    /// The application added it with `add_peer`.
    Application,
    /// It took part in this device's join or link.
    Enrollment,
    /// It connected to this device.
    Session,
    /// It linked as another device of a person.
    Link,
    /// Discovery found it.
    Discovery,
    /// Another peer handed it over.
    Handover,
    /// It asked this device for something.
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
/// Whether the last sync pass dialed a peer and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerChoice {
    /// The pass did not consider it.
    NotConsidered,
    /// It waits out a backoff.
    Backoff,
    /// The pass reached its peer limit first.
    PassLimit,
    /// Chosen because it is on the local network.
    SelectedLocal,
    /// Chosen because it held data this device needs.
    SelectedHolding,
    /// Chosen because it is an admin device.
    SelectedAdmin,
    /// Chosen to probe what it holds.
    SelectedProbe,
    /// Skipped because another peer covered it.
    Covered,
}
/// One known peer device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    /// The device.
    pub node: crate::DeviceId,
    /// Whether it is an admin device.
    pub is_issuer: bool,
    /// Whether the application added it.
    pub is_configured: bool,
    /// How this device learned of it.
    pub source: PeerSource,
    /// Whether the last pass dialed it.
    pub choice: PeerChoice,
    /// Its routes.
    pub routes: Vec<Route>,
    /// Whether it is on the local network.
    pub is_local: bool,
    /// The replication link to it.
    pub link: PeerLink,
    /// Recent network attempts.
    pub attempts: Vec<AttemptObservation>,
    /// When it last proved its identity in an exchange.
    pub last_authenticated_exchange_at_epoch_seconds: Option<u64>,
    /// When it is tried again.
    pub retry: Option<RetryStatus>,
    /// The last reconciliation with it.
    pub last_sync: Option<PeerReconciliation>,
}
/// Who owns the attached transport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkOwnership {
    /// Rocky started it from `Network::Owned` and closes it.
    Rocky,
    /// The application lent it and closes it.
    Application,
}
/// Connections other devices opened to this one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundStatus {
    /// Open now.
    pub active: u64,
    /// The most open at once.
    pub limit: u64,
    /// Served since open.
    pub served: u64,
    /// Refused since open.
    pub refused: u64,
}
/// The network side of the database.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkStatus {
    /// Whether a network is attached.
    pub attachment: AttachmentState,
    /// The route schemes the attached transport dials, such as `iroh` or `tcp`, `ws` and `wss`;
    /// empty when detached.
    pub schemes: Vec<String>,
    /// Whether the attached transport only dials out and names no route of its own.
    pub dial_only: bool,
    /// Who owns the transport.
    pub ownership: Option<NetworkOwnership>,
    /// The sync mode.
    pub sync: crate::SyncMode,
    /// Whether the background sync worker runs.
    pub worker_running: bool,
    /// Whether a sync pass runs now.
    pub reconciling: bool,
    /// Inbound connections.
    pub inbound: Option<InboundStatus>,
    /// Known peers.
    pub peers: Vec<Peer>,
    /// Peers with an open link.
    pub connected_peer_count: u64,
    /// Recent contact resolutions.
    pub contact_resolutions: Vec<AttemptObservation>,
    /// Peer discovery.
    pub discovery: DiscoveryStatus,
    /// The last sync pass.
    pub last_reconciliation: Option<SyncReport>,
}
/// A condition that needs attention; alerts persist until their cause clears.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Alert {
    /// What happened.
    pub kind: AlertKind,
    /// When it was raised, or `None` when not recorded.
    pub since: Option<Timestamp>,
}
/// Why a routine seal was skipped.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum SealSkip {
    /// The device follows no admin's history yet.
    NoFollowedBookmark,
    /// The device may not seal.
    NoTipAuthority(String),
    /// Sealing failed.
    Failed(String),
}
impl SealSkip {
    /// A stable snake-case label.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoFollowedBookmark => "no_followed_bookmark",
            Self::NoTipAuthority(_) => "no_tip_authority",
            Self::Failed(_) => "failed",
        }
    }
}
impl std::fmt::Display for SealSkip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFollowedBookmark => f.write_str(self.code()),
            Self::NoTipAuthority(detail) | Self::Failed(detail) => {
                write!(f, "{}: {detail}", self.code())
            }
        }
    }
}
/// What an alert reports.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertKind {
    /// An admin signed two conflicting membership changes at the same position.
    MembershipFork {
        /// The admin.
        identity: crate::PersonId,
        /// The position.
        sequence: u64,
    },
    /// This device's clock runs ahead of a peer's.
    ClockAheadOfPeer {
        /// The peer.
        node: crate::DeviceId,
        /// By how much.
        seconds: u64,
    },
    /// A peer's clock runs ahead of this device's.
    PeerClockAhead {
        /// The peer.
        node: crate::DeviceId,
        /// By how much.
        seconds: u64,
    },
    /// An admin linked a device.
    AdminDeviceLinked {
        /// The admin.
        identity: crate::PersonId,
        /// The device.
        node: crate::DeviceId,
        /// When.
        linked: Timestamp,
    },
    /// An admin unlinked a device.
    AdminDeviceUnlinked {
        /// The admin.
        identity: crate::PersonId,
        /// The device.
        node: crate::DeviceId,
        /// When.
        unlinked: Timestamp,
    },
    /// No device of an admin has been heard from within `ADMIN_STALE_AFTER`.
    AdminStale {
        /// The admin.
        identity: crate::PersonId,
        /// When one of its devices was last heard from.
        last_heard: Timestamp,
    },
    /// A routine seal was skipped.
    SealSkipped {
        /// Why.
        reason: SealSkip,
    },
    /// A revoked admin device kept signing after its revocation's cut; those statements are
    /// ignored.
    TipStatementsPastCut {
        /// The admin.
        signer: crate::PersonId,
        /// The device.
        node: crate::DeviceId,
        /// Statements ignored.
        statements: u64,
    },
    /// An admin device signed two histories at the same position.
    TipChainEquivocation {
        /// The admin.
        signer: crate::PersonId,
        /// The device.
        node: crate::DeviceId,
        /// The position.
        sequence: u64,
    },
    /// This device's data is older than peers have seen from it, as after restoring a backup, so it
    /// refuses to write.
    StoreSuperseded {
        /// The peer that reported it.
        node: crate::DeviceId,
        /// The position this device holds.
        held: u64,
        /// The position the peer saw.
        reported: u64,
    },
    /// A peer's data went back to an older position.
    PeerStoreRegressed {
        /// The peer.
        node: crate::DeviceId,
        /// The scope.
        scope: String,
        /// The position the peer holds now.
        held: u64,
        /// The position it reported before.
        reported: u64,
    },
    /// A committed transaction was dropped on replay; cleared by `Database::acknowledge_drop`.
    WriteDropped {
        /// The dropped transaction.
        token: crate::TransactionToken,
        /// The device that wrote it.
        node: crate::DeviceId,
        /// Why it was dropped.
        reason: crate::DropReason,
    },
}
/// How long an admin may stay unheard before `AdminStale`; 30 days unless
/// `Configuration::admin_stale_after` changes it.
pub const ADMIN_STALE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(30 * 24 * 60 * 60);
/// A device's signed health summary, shared with admin devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthReport {
    /// When it was produced.
    pub produced_at_epoch_seconds: u64,
    /// The application schema version the device runs.
    pub schema: u32,
    /// Its alerts.
    pub alerts: Vec<Alert>,
    /// Its blockers by kind.
    pub blockers: Vec<BlockerCount>,
}
/// How many blockers of one kind a device has.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockerCount {
    /// The blocker kind.
    pub kind: String,
    /// How many.
    pub count: u64,
}
/// The latest health report of one device, as an admin device sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshDevice {
    /// The device.
    pub node: crate::DeviceId,
    /// Its report.
    pub report: HealthReport,
    /// When the report arrived.
    pub received_at_epoch_seconds: u64,
    /// The device that relayed it, or `None` when it came directly.
    pub via: Option<crate::DeviceId>,
    /// Whether no report arrived within `SILENT_AFTER`.
    pub silent: bool,
}
/// How long a device may go without a health report before it counts as silent.
pub const SILENT_AFTER: std::time::Duration = std::time::Duration::from_secs(300);
/// A snapshot of what an application acts on: blockers, alerts, credentials, schema and sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DatabaseStatus {
    #[serde(skip)]
    pub(crate) token: String,
    pub(crate) observed_at_epoch_millis: Option<u64>,
    pub(crate) authority: AuthorityStatus,
    pub(crate) data: DataStatus,
    pub(crate) network: NetworkStatus,
    pub(crate) alerts: Vec<Alert>,
    pub(crate) mesh: Option<Vec<MeshDevice>>,
}
impl DatabaseStatus {
    /// When it was taken.
    pub fn observed_at(&self) -> Option<Timestamp> {
        self.observed_at_epoch_millis
            .map(Timestamp::from_unix_millis)
    }
    /// Data that cannot be read yet and what clears each; empty when everything is readable.
    pub fn blockers(&self) -> Vec<Blocker> {
        self.data.blockers.iter().map(Blocker::from).collect()
    }
    /// Whether the first download of the data has not finished.
    pub fn bootstrapping(&self) -> bool {
        self.data.bootstrap_pending
    }
    /// Conditions that need attention, until their cause clears.
    pub fn alerts(&self) -> &[Alert] {
        &self.alerts
    }
    /// Whether this is an admin device, which issues credentials and publishes schemas.
    pub fn admin(&self) -> bool {
        self.authority.is_issuer
    }
    /// When this device's credentials expire; a sync with an admin device renews them.
    pub fn credentials_expire_at(&self) -> Timestamp {
        Timestamp::from_unix_seconds(self.authority.expires_at_epoch_seconds)
    }
    /// Whether this device's credentials expired, which holds back sync until an admin renews them.
    pub fn credentials_expired(&self) -> bool {
        self.authority.expired
    }
    /// The schema version installed on this device, which can differ from the one the application
    /// declares.
    pub fn schema_version(&self) -> u32 {
        self.data.generation.current.schema
    }
    /// How the application's declared schema version compares with the installed one.
    pub fn declared_schema(&self) -> DeclaredSchema {
        self.data.generation.application
    }
    /// The state of a migration to the declared schema version, or `None` when none runs.
    pub fn migration(&self) -> Option<MigrationStatus> {
        self.data.generation.migration
    }
    /// Whether this build of Rocky can run the database's schema.
    pub fn engine(&self) -> EngineStatus {
        self.data.generation.engine
    }
    /// The sync mode.
    pub fn sync_mode(&self) -> crate::SyncMode {
        self.network.sync
    }
    /// Whether a sync pass runs now.
    pub fn syncing(&self) -> bool {
        self.network.reconciling
    }
    /// Whether the attached transport only dials out, so no peer can dial this device.
    pub fn dial_only(&self) -> bool {
        self.network.dial_only
    }
    /// The peer devices this device knows.
    pub fn peers(&self) -> Vec<PeerStatus> {
        self.network
            .peers
            .iter()
            .map(|peer| PeerStatus {
                device: peer.node,
                connected: peer.link.state == LinkState::Open,
                last_exchange: peer
                    .last_authenticated_exchange_at_epoch_seconds
                    .map(Timestamp::from_unix_seconds),
            })
            .collect()
    }
    /// The last sync pass, or `None` before the first.
    pub fn last_sync(&self) -> Option<&SyncReport> {
        self.network.last_reconciliation.as_ref()
    }
}
/// One known peer device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerStatus {
    /// The device.
    pub device: crate::DeviceId,
    /// Whether a replication link to it is open.
    pub connected: bool,
    /// When it last proved its identity in an exchange, or `None` if never.
    pub last_exchange: Option<Timestamp>,
}
/// What one scope exchanged with a peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeReconciliationTransfer {
    /// The scope.
    pub scope: ScopeStatus,
    /// Encrypted frames received, not rows.
    pub pulled: u64,
    /// Encrypted frames sent, not rows.
    pub pushed: u64,
    /// Bytes received.
    pub received_bytes: u64,
    /// Bytes sent.
    pub sent_bytes: u64,
    /// A checkpoint the scope started from, if one was imported.
    pub checkpoint: Option<String>,
    /// Frames skipped because a checkpoint covers them.
    pub superseded: Option<u64>,
}
/// One peer's part of a sync pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerReconciliation {
    /// The peer.
    pub node: crate::DeviceId,
    /// The attempts made.
    pub attempts: Vec<AttemptObservation>,
    /// Why the pass skipped the peer, if it did.
    pub skip: Option<SkipReason>,
    /// What each scope exchanged.
    pub transfers: Vec<ScopeReconciliationTransfer>,
    /// Scopes the peer refused.
    pub refused: Vec<ScopeStatus>,
    /// Scopes that wait for something before they sync.
    pub waiting: Vec<ScopeStatus>,
    /// Whether the peer needs a newer Rocky.
    pub upgrade_required: bool,
    /// Whether a retry is scheduled.
    pub retry_pending: bool,
    /// The outcome per scope.
    pub scopes: Vec<ScopeOutcome>,
    /// Timeouts by kind.
    pub timeouts: Vec<TimeoutCount>,
}
impl PeerReconciliation {
    /// Why the latest failed attempt with the peer failed, if one did.
    pub fn failure(&self) -> Option<&AttemptFailure> {
        self.attempts
            .iter()
            .rev()
            .find_map(|attempt| attempt.failure.as_ref())
    }
    /// Whether a later pass may reconcile with the peer: it is not refused for good by a newer
    /// Rocky, a refusal, missing credentials or a protocol fault.
    pub fn retryable(&self) -> bool {
        !self.upgrade_required
            && self.failure().is_none_or(|failure| {
                matches!(
                    failure.kind,
                    FailureKind::Timeout | FailureKind::Transport | FailureKind::Other
                )
            })
    }
    /// Whether a reconciliation with the peer completed.
    pub fn reconciled(&self) -> bool {
        self.attempts.iter().any(|attempt| {
            attempt.operation == AttemptOperation::Reconcile
                && attempt.outcome == Some(AttemptOutcome::Completed)
        })
    }
}
/// How one scope's sync ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeSyncOutcome {
    /// It synced.
    Synced,
    /// The peer refused it.
    Refused,
    /// The peer is still importing it.
    PeerImporting,
    /// It failed.
    Failed,
    /// It was interrupted.
    Interrupted,
}
/// One scope's sync outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeOutcome {
    /// The scope.
    pub scope: ScopeStatus,
    /// How it ended.
    pub outcome: ScopeSyncOutcome,
}
/// How many timeouts of one kind occurred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeoutCount {
    /// The kind.
    pub kind: String,
    /// How many.
    pub count: u64,
}
/// What one `Database::sync_now` pass did; it prints as one line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub(crate) contact_resolutions: Vec<AttemptObservation>,
    pub(crate) started_at_epoch_millis: Option<u64>,
    pub(crate) ended_at_epoch_millis: Option<u64>,
    pub(crate) peers: Vec<PeerReconciliation>,
}
impl SyncReport {
    /// When the pass started.
    pub fn started_at(&self) -> Option<Timestamp> {
        self.started_at_epoch_millis
            .map(Timestamp::from_unix_millis)
    }
    /// When the pass ended.
    pub fn ended_at(&self) -> Option<Timestamp> {
        self.ended_at_epoch_millis.map(Timestamp::from_unix_millis)
    }
    /// Peers the pass knew.
    pub fn known(&self) -> usize {
        self.peers.len()
    }
    /// Peers that reconciled.
    pub fn reconciled(&self) -> usize {
        self.peers.iter().filter(|peer| peer.reconciled()).count()
    }
    /// Whether every known peer reconciled or was covered by another; a pass that knew no peer is
    /// not complete.
    pub fn complete(&self) -> bool {
        !self.peers.is_empty()
            && self
                .peers
                .iter()
                .all(|peer| peer.reconciled() || peer.skip == Some(SkipReason::Covered))
    }
    /// Encrypted frames received from all peers, not rows.
    pub fn pulled(&self) -> u64 {
        self.transfers().map(|transfer| transfer.pulled).sum()
    }
    /// Encrypted frames sent to all peers, not rows.
    pub fn pushed(&self) -> u64 {
        self.transfers().map(|transfer| transfer.pushed).sum()
    }
    fn transfers(&self) -> impl Iterator<Item = &ScopeReconciliationTransfer> {
        self.peers.iter().flat_map(|peer| &peer.transfers)
    }
}
impl std::fmt::Display for SyncReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let verdict = if self.complete() {
            "complete"
        } else {
            "incomplete"
        };
        write!(
            f,
            "{verdict}: reconciled {} of {} peers, pulled {}, pushed {}",
            self.reconciled(),
            self.peers.len(),
            self.pulled(),
            self.pushed()
        )
    }
}

#[cfg(test)]
mod sync_report_tests {
    use super::*;

    fn peer(node: &str, outcome: AttemptOutcome, skip: Option<SkipReason>) -> PeerReconciliation {
        PeerReconciliation {
            node: crate::DeviceId::new(
                rocky_credentials::PrivateKey::from_bytes(&[node.as_bytes()[0]; 32]).public_key(),
            ),
            attempts: vec![AttemptObservation {
                id: 1,
                target: node.into(),
                operation: AttemptOperation::Reconcile,
                stage: AttemptStage::Reconciliation,
                started_at_epoch_millis: None,
                ended_at_epoch_millis: None,
                elapsed_millis: None,
                outcome: Some(outcome),
                failure: None,
            }],
            skip,
            transfers: vec![ScopeReconciliationTransfer {
                scope: ScopeStatus {
                    table: Some("items".into()),
                    table_id: String::new(),
                    encryption: String::new(),
                    partition: false,
                },
                pulled: 3,
                pushed: 1,
                received_bytes: 0,
                sent_bytes: 0,
                checkpoint: None,
                superseded: None,
            }],
            refused: vec![],
            waiting: vec![],
            upgrade_required: false,
            retry_pending: false,
            scopes: vec![],
            timeouts: vec![],
        }
    }
    fn report(peers: Vec<PeerReconciliation>) -> SyncReport {
        SyncReport {
            contact_resolutions: vec![],
            started_at_epoch_millis: None,
            ended_at_epoch_millis: None,
            peers,
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_pass_is_complete_when_every_peer_reconciled_or_another_covered_it() {
        let covered = PeerReconciliation {
            attempts: vec![],
            ..peer("c", AttemptOutcome::Completed, Some(SkipReason::Covered))
        };
        let complete = report(vec![peer("a", AttemptOutcome::Completed, None), covered]);
        assert!(complete.complete());
        assert_eq!(complete.reconciled(), 1);
        let failed = report(vec![
            peer("a", AttemptOutcome::Completed, None),
            peer("b", AttemptOutcome::Failed, None),
        ]);
        assert!(!failed.complete());
        assert_eq!(failed.reconciled(), 1);
        assert!(
            !report(vec![]).complete(),
            "a pass that knew no peer is not complete"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_report_prints_its_verdict_and_counts_on_one_line() {
        let failed = report(vec![
            peer("a", AttemptOutcome::Completed, None),
            peer("b", AttemptOutcome::Failed, None),
        ]);
        assert_eq!((failed.pulled(), failed.pushed()), (6, 2));
        assert_eq!(
            failed.to_string(),
            "incomplete: reconciled 1 of 2 peers, pulled 6, pushed 2"
        );
    }
}
