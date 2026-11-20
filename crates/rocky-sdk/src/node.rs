mod repair;
mod requests;
use repair::RepairWork;
mod claims;
mod coordinated;
mod delivery;
mod discovery;
mod exchange;
use exchange::{Exchanged, PeerWork};
mod health;
mod held;
mod linking;
mod live_links;
mod membership;
mod mesh;
mod partition;
mod personal;
#[cfg(test)]
pub(crate) use personal::FORGED_SCHEMA;
mod recovery;
mod registry;
mod reset;
mod schema_publication;
mod schemas;
mod shared;
mod status;
mod sync_pass;
use sync_pass::PassOutcome;
mod tip_nodes;
mod tips;
use crate::{
    Failure,
    bootstrap_intent::{BootstrapIntent, BootstrapIntentFile, Finished, Imports},
    domain::Domain,
    enrollment::{Contact, Settings, Terms, exchange, now},
    peer::{PeerReply, PeerRequest},
    request::{Kind as RequestKind, Lifecycle},
    secrets::Vault,
    service::{Network, ServiceError},
    sync::{self, Upstream},
    transport::{Protocol, Stream, Transport},
};
use data_encoding::HEXLOWER;
use futures_util::{StreamExt, stream};
use rocky_core::{ResourceScope, SqlValue, TypedRows, commitment};
use rocky_credentials::{IdentityEnrollment, IdentityReply, LinkEnrollment, Profile};
use rocky_secure::{
    CredentialChain, EncryptedReplica, Error, KeyMaterial, PrivateKey, PublicKey, RevocationSet,
    SecureSql, SqlError, SqlOptions, Unavailable, secure_node_id,
};
use serde::{Deserialize, Serialize};
pub(crate) use shared::{SettingsCell, SettingsGuard, Shared};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, RwLock, Semaphore, watch};

const MAX_SESSIONS: usize = 64;
const INBOUND: usize = 32;
const MET: usize = 64;
const DRAIN: Duration = Duration::from_secs(5);
const IMPORT_SOURCE: Duration = Duration::from_secs(180);

#[derive(Debug)]
pub struct Refused(pub String);
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Refused {}
fn refused(error: impl std::fmt::Display) -> Failure {
    Box::new(Refused(error.to_string()))
}
fn named(error: Failure) -> Failure {
    if error.is::<crate::registry::RegistryFull>() {
        error
    } else {
        refused(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncState {
    Idle,
    NetworkingDisabled,
    SchemaPublicationPending,
    Connected,
    Revoked,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct SyncStatus {
    token: u64,
    state: SyncState,
    retry: bool,
    peers: usize,
    error: Option<String>,
}
impl SyncStatus {
    fn new(state: SyncState) -> Self {
        Self {
            token: 0,
            state,
            retry: false,
            peers: 0,
            error: None,
        }
    }
    fn failed(state: SyncState, error: String, retry: bool) -> Self {
        Self {
            error: Some(error),
            retry,
            ..Self::new(state)
        }
    }
}

pub(crate) struct Client {
    closing: watch::Sender<bool>,
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Open,
    Draining,
    Cancelled,
}
struct Link {
    id: u64,
    transport: Arc<dyn Transport>,
    owned: bool,
    stage: watch::Sender<Stage>,
    server: std::sync::Mutex<Option<rocky_platform::task::JoinHandle<()>>>,
    synchronizer: std::sync::Mutex<Option<rocky_platform::task::JoinHandle<()>>>,
    publisher: std::sync::Mutex<Option<rocky_platform::task::JoinHandle<()>>>,
    discovery: Option<Arc<dyn crate::transport::Discovery>>,
    changes: Option<watch::Receiver<()>>,
    discoverer: std::sync::Mutex<Option<rocky_platform::task::JoinHandle<()>>>,
    active: Arc<Semaphore>,
    peer_admission: [Semaphore; 3],
    live_budget: Arc<Semaphore>,
    live_handshakes: Arc<Semaphore>,
    live: live_links::Registry,
    served: AtomicU64,
    refused: AtomicU64,
}
struct WorkerActivity(watch::Sender<bool>);
impl Drop for WorkerActivity {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncActivityState {
    Idle,
    Syncing,
    Bootstrapping,
}
struct SyncActivity(watch::Sender<SyncActivityState>);
impl Drop for SyncActivity {
    fn drop(&mut self) {
        self.0.send_replace(SyncActivityState::Idle);
    }
}
#[derive(Default)]
struct PeerRetry {
    short: bool,
    retry: crate::scheduling::Retry,
    pending: RepairWork,
    link: bool,
    link_retry: crate::scheduling::Retry,
    generation: u64,
    work_generation: u64,
}
struct SyncFailure {
    refused: bool,
    since: Option<u64>,
}
struct Reached {
    at: u64,
    error: Option<String>,
    refused: Vec<ResourceScope>,
    waiting: Vec<ResourceScope>,
    held: Vec<ResourceScope>,
}
impl Reached {
    fn after(result: &Result<Exchanged, Failure>, at: u64) -> Self {
        let (refused, waiting, held) = match result {
            Ok(Exchanged { transfer, .. }) => (
                transfer.refused.clone(),
                transfer.waiting.clone(),
                transfer.held.clone(),
            ),
            Err(_) => (Vec::new(), Vec::new(), Vec::new()),
        };
        Self {
            at,
            error: result.as_ref().err().map(ToString::to_string),
            refused,
            waiting,
            held,
        }
    }
}

const LOCAL_SETTLE: std::time::Duration =
    std::time::Duration::from_millis(if cfg!(test) { 0 } else { 20 });
async fn settle() {
    if !LOCAL_SETTLE.is_zero() {
        rocky_platform::time::sleep(LOCAL_SETTLE).await;
    }
}

pub(crate) struct Node {
    owner: std::sync::OnceLock<std::sync::Weak<Node>>,
    pub(crate) background: crate::reactor::Background,
    pub replica: EncryptedReplica<TypedRows>,
    domain: std::sync::RwLock<Arc<Domain>>,
    granted: std::sync::Mutex<Vec<ResourceScope>>,
    pub vault: Vault,
    pub settings: SettingsCell,
    revocation_notice: crate::enrollment::RevocationNotice,
    schema_fetch: Mutex<()>,
    schema_acquisition: std::sync::Mutex<schemas::Acquisition>,
    pub path: PathBuf,
    pub online: AtomicBool,
    pub lifecycle: Mutex<Lifecycle>,
    network: RwLock<Option<Arc<Link>>>,
    current: std::sync::Mutex<Option<Arc<Link>>>,
    detaching: Mutex<()>,
    links: AtomicU64,
    clients: std::sync::Mutex<BTreeMap<u64, Arc<Client>>>,
    syncing: Mutex<()>,
    bootstrap: Mutex<()>,
    pub(crate) bootstrap_intent: BootstrapIntentFile,
    retry_seed: u64,
    retries: std::sync::Mutex<BTreeMap<PublicKey, PeerRetry>>,
    backoff_tip: std::sync::Mutex<Option<rocky_core::TipId>>,
    resolution_retries:
        std::sync::Mutex<BTreeMap<rocky_credentials::PeerId, crate::scheduling::Retry>>,
    unlink_retries: std::sync::Mutex<BTreeMap<rocky_credentials::PeerId, crate::scheduling::Retry>>,
    unlink_delivery: Mutex<()>,
    sync_control: watch::Sender<u64>,
    worker_running: watch::Sender<bool>,
    sync_state: watch::Sender<SyncActivityState>,
    automatic: AtomicBool,
    foreground: AtomicBool,
    sync_events: watch::Sender<u64>,
    link_events: watch::Sender<()>,
    sync_status: watch::Sender<SyncStatus>,
    link_revision: AtomicU64,
    pub(crate) activity: crate::activity::Activity,
    backups: Mutex<()>,
    reached: std::sync::Mutex<BTreeMap<PublicKey, Reached>>,
    met: std::sync::Mutex<BTreeMap<PublicKey, u64>>,
    link_requests: std::sync::Mutex<BTreeMap<PublicKey, crate::self_enrollment::PendingLink>>,
    personal_nonces: std::sync::Mutex<personal::Nonces>,
    clocks: std::sync::Mutex<BTreeMap<PublicKey, crate::Alert>>,
    coordination: Arc<health::Coordination>,
    mesh: std::sync::Mutex<BTreeMap<PublicKey, health::Held>>,
    trusted_watch: std::sync::Mutex<health::TrustedWatch>,
    tip_chain: Arc<tokio::sync::Mutex<()>>,
    seal_skipped: std::sync::Mutex<Option<crate::Alert>>,
    finality_stalled: std::sync::Mutex<Option<crate::Alert>>,
    mesh_revision: watch::Sender<u64>,
    report_failing: AtomicBool,
    #[cfg(test)]
    pub(crate) reports_received: AtomicU64,
    #[cfg(test)]
    pub(crate) verified_under_settings: AtomicU64,
    learned: AtomicBool,
    handover: std::sync::Mutex<std::collections::BTreeSet<PublicKey>>,
    import_sources: std::sync::Mutex<
        BTreeMap<
            ResourceScope,
            (
                Option<rocky_core::Commitment>,
                rocky_platform::time::Instant,
            ),
        >,
    >,
    sync_failures: std::sync::Mutex<BTreeMap<(PublicKey, ResourceScope), SyncFailure>>,
    choices: std::sync::Mutex<BTreeMap<PublicKey, &'static str>>,
    topics: watch::Sender<u64>,
    announcing: AtomicBool,
    #[cfg(test)]
    pub(crate) discovery_skew: AtomicU64,
    observations: crate::status::Observations,
    node_identity: Result<String, String>,
    #[cfg(test)]
    status_clock: std::sync::Mutex<Option<(u64, rocky_platform::time::Instant)>>,
    last_reconciliation: std::sync::Mutex<Option<crate::ReconciliationReport>>,
    last_syncs: std::sync::Mutex<BTreeMap<String, crate::PeerReconciliation>>,
    coordinator: std::sync::RwLock<Option<Arc<dyn rocky_credentials::Coordinator>>>,
    connector: std::sync::RwLock<Option<Arc<dyn rocky_credentials::CoordinatorConnector>>>,
    connect_failure: std::sync::Mutex<Option<String>>,
    mailbox: Mutex<Option<crate::delivery::Mailbox>>,
    declared: crate::schema::Schema,
    widened: std::sync::Mutex<std::collections::BTreeSet<String>>,
    claims_asked: std::sync::Mutex<u64>,
    revocations_pushing: Mutex<()>,

    push_retry: std::sync::Mutex<claims::PushRetry>,
}

impl Node {
    pub fn domain(&self) -> Arc<Domain> {
        self.domain
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    pub fn new(
        replica: EncryptedReplica<TypedRows>,
        domain: Domain,
        vault: Vault,
        settings: Settings,
        path: PathBuf,
        bootstrap_intent: BootstrapIntentFile,
    ) -> Self {
        let settings_schema = settings.schema.clone();
        let revocation_notice = crate::enrollment::RevocationNotice::open(&settings, &path);
        let observations = crate::status::Observations::default();
        Self {
            #[cfg(test)]
            status_clock: Default::default(),
            node_identity: vault
                .get("node")
                .map(|key| PrivateKey::from_bytes(&key).public_key().to_hex())
                .map_err(|error| error.to_string()),
            last_reconciliation: Default::default(),
            last_syncs: Default::default(),
            coordination: Arc::new(health::Coordination::new(observations.clone())),
            mesh: Default::default(),
            link_requests: Default::default(),
            personal_nonces: Default::default(),
            mesh_revision: watch::channel(0).0,
            trusted_watch: Default::default(),
            seal_skipped: Default::default(),
            finality_stalled: Default::default(),
            tip_chain: Default::default(),
            report_failing: AtomicBool::new(false),
            #[cfg(test)]
            reports_received: AtomicU64::new(0),
            #[cfg(test)]
            verified_under_settings: AtomicU64::new(0),
            observations,
            owner: Default::default(),
            background: Default::default(),
            replica,
            granted: std::sync::Mutex::new(settings.granted(&domain)),
            domain: std::sync::RwLock::new(Arc::new(domain)),
            vault,
            settings: SettingsCell::new(settings),
            revocation_notice,
            path,
            online: AtomicBool::new(true),
            lifecycle: Mutex::new(Lifecycle::Active),
            network: RwLock::new(None),
            current: std::sync::Mutex::new(None),
            detaching: Mutex::new(()),
            links: AtomicU64::new(1),
            clients: std::sync::Mutex::new(BTreeMap::new()),
            syncing: Mutex::new(()),
            bootstrap: Mutex::new(()),
            bootstrap_intent,
            retry_seed: rocky_platform::entropy::random_u64(),
            retries: std::sync::Mutex::new(BTreeMap::new()),
            backoff_tip: std::sync::Mutex::new(None),
            resolution_retries: std::sync::Mutex::new(BTreeMap::new()),
            unlink_retries: std::sync::Mutex::new(BTreeMap::new()),
            unlink_delivery: Mutex::new(()),
            sync_control: watch::channel(0).0,
            worker_running: watch::channel(false).0,
            sync_state: watch::channel(SyncActivityState::Idle).0,
            automatic: AtomicBool::new(false),
            foreground: AtomicBool::new(true),
            sync_events: watch::channel(0).0,
            link_events: watch::channel(()).0,
            link_revision: AtomicU64::new(0),
            sync_status: watch::channel(SyncStatus::new(SyncState::Idle)).0,
            activity: crate::activity::Activity::new(),
            backups: Mutex::new(()),
            reached: std::sync::Mutex::new(BTreeMap::new()),
            met: std::sync::Mutex::new(BTreeMap::new()),
            clocks: std::sync::Mutex::new(BTreeMap::new()),
            learned: AtomicBool::new(false),
            handover: std::sync::Mutex::new(Default::default()),
            import_sources: std::sync::Mutex::new(Default::default()),
            sync_failures: Default::default(),
            choices: std::sync::Mutex::new(BTreeMap::new()),
            topics: watch::channel(0).0,
            announcing: AtomicBool::new(false),
            #[cfg(test)]
            discovery_skew: AtomicU64::new(0),
            coordinator: Default::default(),
            connector: Default::default(),
            connect_failure: Default::default(),
            mailbox: Mutex::new(None),
            declared: settings_schema,
            schema_fetch: Mutex::new(()),
            schema_acquisition: Default::default(),
            widened: Default::default(),

            claims_asked: Default::default(),

            revocations_pushing: Mutex::new(()),

            push_retry: Default::default(),
        }
    }
    pub fn with_declared(mut self, schema: crate::schema::Schema) -> Self {
        self.declared = schema;
        self
    }
    pub async fn application(&self, settings: &Settings) -> Result<crate::DeclaredSchema, Failure> {
        if self.replica.followed_bookmark().await.is_none() {
            return Ok(crate::DeclaredSchema::Ahead);
        }
        settings.application(&self.declared)
    }
    pub(crate) async fn migration(
        &self,
        settings: &Settings,
    ) -> Result<Option<crate::MigrationStatus>, Failure> {
        let (mut bundles, mut published) = (0, 0);
        let mut pending = false;
        for publication in self.replica.publications().await? {
            if publication.published < publication.bundles
                || !self
                    .replica
                    .migration_checkpoint_listed(publication.id)
                    .await?
            {
                pending = true;
                bundles += publication.bundles;
                published += publication.published;
            }
        }
        if pending {
            return Ok(Some(crate::MigrationStatus::Publishing {
                bundles,
                published,
            }));
        }
        Ok(
            (self.application(settings).await? == crate::DeclaredSchema::Ahead
                && crate::generation::migrates(&self.declared, settings.generation)
                && !settings.may_migrate(&self.domain())?)
            .then_some(crate::MigrationStatus::FullAccessRequired),
        )
    }
    pub(crate) fn scopes(&self) -> Vec<ResourceScope> {
        self.granted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn unpartitioned_scopes(&self) -> Vec<ResourceScope> {
        self.scopes()
            .into_iter()
            .filter(|scope| !self.domain().partitioned_table(scope))
            .collect()
    }
    fn app_scopes(&self) -> Vec<ResourceScope> {
        let registry = self.domain().registry();
        self.scopes()
            .into_iter()
            .filter(|scope| *scope != registry)
            .collect()
    }

    pub fn client(&self, id: u64) -> Result<Arc<Client>, Failure> {
        let mut clients = self.clients.lock().map_err(|_| "sessions poisoned")?;
        if let Some(client) = clients.get(&id) {
            return Ok(client.clone());
        }
        if clients.len() >= MAX_SESSIONS {
            return Err("session quota".into());
        }
        let client = Arc::new(Client {
            closing: watch::channel(false).0,
        });
        clients.insert(id, client.clone());
        Ok(client)
    }
    pub fn end_sessions(&self, id: Option<u64>) {
        let mut clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        let ended: Vec<_> = match id {
            Some(id) => clients.remove(&id).into_iter().collect(),
            None => std::mem::take(&mut *clients).into_values().collect(),
        };
        for client in ended {
            client.closing.send_replace(true);
        }
    }
    pub(crate) async fn sql_session(
        &self,
        options: SqlOptions,
    ) -> Result<SecureSql<TypedRows>, Failure> {
        if self.lifecycle.lock().await.is_logged_out() {
            return Err(refused("this device logged out of the domain"));
        }
        self.revocation_notice
            .admit_personal(&self.settings.published())?;
        let session = self
            .replica
            .session(self.app_scopes().into_iter().collect())
            .await?;
        Ok(session.sql(options).await?)
    }

    fn install(
        self: &Arc<Self>,
        slot: &mut Option<Arc<Link>>,
        transport: crate::transport::Connected,
        owned: bool,
    ) -> Result<u64, Failure> {
        let _ = self.owner.set(Arc::downgrade(self));
        let link = Arc::new(Link {
            id: self.links.fetch_add(1, Ordering::Relaxed),
            owned,
            stage: watch::channel(Stage::Open).0,
            server: std::sync::Mutex::new(None),
            synchronizer: std::sync::Mutex::new(None),
            publisher: std::sync::Mutex::new(None),
            discovery: transport.discovery,
            changes: transport.changes,
            transport: transport.transport,
            discoverer: std::sync::Mutex::new(None),
            active: Arc::new(Semaphore::new(INBOUND)),
            peer_admission: std::array::from_fn(|_| Semaphore::new(INBOUND)),
            live_budget: Arc::new(Semaphore::new(live_links::BUDGET)),
            live_handshakes: Arc::new(Semaphore::new(live_links::BUDGET)),
            live: live_links::Registry::default(),
            served: AtomicU64::new(0),
            refused: AtomicU64::new(0),
        });
        link.transport.listen(Some(self.domain().scope), true)?;
        let publisher =
            crate::reactor::spawn_local("publish_links", self.clone().publish_links(link.clone()));
        *link
            .publisher
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(publisher);
        let synchronizer =
            crate::reactor::spawn_local("synchronize", self.clone().synchronize(link.clone()));
        *link
            .synchronizer
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(synchronizer);
        let server = crate::reactor::spawn_local("accept", self.clone().accept(link.clone()));
        *link.server.lock().unwrap_or_else(PoisonError::into_inner) = Some(server);
        if let Some(discovery) = link.discovery.clone() {
            let discoverer = crate::reactor::spawn_local(
                "discover",
                self.clone().discover(link.clone(), discovery),
            );
            *link
                .discoverer
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(discoverer);
        }
        let id = link.id;
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(link.clone());
        *slot = Some(link);
        self.wake_sync();
        Ok(id)
    }
    pub async fn link(
        self: &Arc<Self>,
        transport: impl Into<crate::transport::Connected>,
        owned: bool,
    ) -> Result<u64, Failure> {
        self.install(&mut *self.network.write().await, transport.into(), owned)
    }
    pub async fn attach(self: &Arc<Self>, network: Network) -> Result<u64, Failure> {
        let mut slot = self.network.write().await;
        if slot.is_some() {
            return Err(ServiceError::NetworkAttached.into());
        }
        if self.lifecycle.lock().await.is_logged_out() {
            return Err(refused("this device logged out of the domain"));
        }
        let issuer = self.settings.lock().await.issuer();
        let key = Settings::route_secret(&self.vault)?;
        let (transport, owned) = network.start(issuer, key).await?;
        self.install(&mut slot, transport, owned)
    }
    pub async fn detach(&self, id: Option<u64>) {
        let _detaching = self.detaching.lock().await;
        let attached = self.network.read().await.clone();
        let Some(link) = attached.filter(|link| id.is_none_or(|id| link.id == id)) else {
            return;
        };
        link.stage.send_replace(Stage::Draining);
        let _ = link.transport.listen(Some(self.domain().scope), false);
        let mut slot = match rocky_platform::time::timeout(DRAIN, self.network.write()).await {
            Ok(slot) => slot,
            Err(_) => {
                link.stage.send_replace(Stage::Cancelled);
                self.network.write().await
            }
        };
        *slot = None;
        self.synced(SyncStatus::new(SyncState::Idle));
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = None;
        drop(slot);
        let server = link
            .server
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(server) = server {
            let _ = server.await;
        }
        let synchronizer = link
            .synchronizer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(synchronizer) = synchronizer {
            let _ = synchronizer.await;
        }
        let publisher = link
            .publisher
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(publisher) = publisher {
            let _ = publisher.await;
        }
        let discoverer = link
            .discoverer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(discoverer) = discoverer {
            let _ = discoverer.await;
        }
        link.live.wait_empty().await;
        if link.owned {
            link.transport.close().await;
        }
    }
    async fn accept(self: Arc<Self>, link: Arc<Link>) {
        let mut tasks = rocky_platform::task::JoinSet::new();
        let mut stage = link.stage.subscribe();
        loop {
            let inbound = tokio::select! {
                inbound = link.transport.accept() => inbound,
                _ = stage.wait_for(|stage| *stage != Stage::Open) => None,
            };
            let Some(incoming) = inbound else {
                break;
            };
            let protocol = incoming.protocol();
            let budget = if protocol == Protocol::Link {
                &link.live_handshakes
            } else {
                &link.active
            };
            let Ok(permit) = budget.clone().try_acquire_owned() else {
                link.refused.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            link.served.fetch_add(1, Ordering::Relaxed);
            let node = self.clone();
            let service = link.clone();
            crate::reactor::spawn_in(&mut tasks, "inbound", async move {
                if let rocky_core::net::Incoming::Stream(Protocol::Link, stream) = incoming {
                    let settings = node.settings.lock().await.clone();
                    if let Err(error) = node
                        .admit_link(service, stream, None, settings, Some(permit))
                        .await
                    {
                        let _ = node.absorb_revocations(Some(&error)).await;
                        node.activity
                            .record_changed(crate::ActivityKind::LinkAdmissionFailed {
                                error: (error.to_string()).to_owned(),
                            });
                    }
                } else {
                    let _permit = permit;
                    if let rocky_core::net::Incoming::Request(request) = incoming {
                        let _ = node.serve(request).await;
                    }
                }
            });
            while tasks.try_join_next().is_some() {}
        }
        let _ = rocky_platform::time::timeout(DRAIN, async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        tasks.shutdown().await;
    }
    async fn networked<T>(
        attached: Option<&Link>,
        work: impl std::future::Future<Output = Result<T, Failure>>,
    ) -> Result<T, Failure> {
        let Some(link) = attached else {
            return work.await;
        };
        let mut stage = link.stage.subscribe();
        tokio::select! {
            result = work => result,
            _ = stage.wait_for(|stage| *stage == Stage::Cancelled) => Err(ServiceError::Closed("network service").into()),
        }
    }

    async fn invitation(
        &self,
        profile: Option<Profile>,
        label: Option<Vec<u8>>,
    ) -> Result<crate::Invitation, Failure> {
        if label
            .as_ref()
            .is_some_and(|label| label.len() > crate::registry::MAX_LABEL)
        {
            return Err(refused("invitation label exceeds 256 bytes"));
        }
        self.settings.lock().await.admits(profile.as_ref())?;
        let attached = self.network.read().await;
        let work = async {
            let transport = &*attached
                .as_ref()
                .ok_or(ServiceError::NetworkRequired("an invitation"))?
                .transport;
            let registry = self.registry(&*self.settings_snapshot().await).await?;
            let (uri, expires, notes) = self
                .settings
                .lock()
                .await
                .invite(
                    &self.path,
                    &self.vault,
                    transport,
                    (profile.clone(), label),
                    &registry,
                )
                .await
                .map_err(named)?;
            self.note(&notes).await;
            let sequence = self
                .activity
                .record(crate::ActivityKind::InvitationCreated {
                    profile: profile.clone(),
                    expires: (expires),
                });
            Ok(crate::Invitation {
                uri,
                expires_at_epoch_seconds: expires,
                profile,
                sequence,
                id: HEXLOWER.encode(&rocky_core::commitment(
                    "rocky.sdk.invitation.v1",
                    &self.vault.get("invitation")?,
                )),
            })
        };
        Self::networked(attached.as_deref(), work).await
    }

    async fn renew_now(&self) -> Result<(), Failure> {
        let attached = self.network.read().await;
        let transport = attached.as_ref().map(|link| &*link.transport);
        Self::networked(attached.as_deref(), async {
            self.renew(transport, true, None, false).await.map(drop)
        })
        .await
    }

    async fn contact_info(&self) -> Result<crate::ContactInfo, Failure> {
        let attached = self.network.read().await;
        let work = async {
            let transport = &*attached
                .as_ref()
                .ok_or(ServiceError::NetworkRequired("contact"))?
                .transport;
            let settings = self.settings.lock().await.clone();
            let (uri, node, routes) = settings.contact(&self.vault, transport).await?;
            Ok(crate::ContactInfo {
                uri,
                node: node.to_hex(),
                routes: routes
                    .into_iter()
                    .map(|route| crate::Route {
                        transport: route.transport,
                        address: route.address,
                    })
                    .collect(),
            })
        };
        Self::networked(attached.as_deref(), work).await
    }

    async fn invitation_state(&self, uri: &str) -> Result<crate::InvitationState, Failure> {
        let (id, expires) = Settings::invitation_identity(uri)?;
        let open = self.settings.lock().await.invitation_open(uri)?;
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let enrolled = registry
            .records
            .iter()
            .find(|(_, record)| {
                record
                    .admitted
                    .as_ref()
                    .is_some_and(|admitted| admitted.invitation.as_deref() == Some(&id))
            })
            .map(|(identity, _)| identity.to_hex());
        Ok(crate::InvitationState {
            id,
            expires_at_epoch_seconds: expires,
            open,
            enrolled,
        })
    }

    async fn enrollment_upstream(&self, contact: Option<&str>) -> Result<(), Failure> {
        self.configure(|settings| {
            if !matches!(settings.profile, Profile::Custody { .. }) || settings.issuer() {
                return Err("enrollment forwarding requires a custody-only node".into());
            }
            settings.enrollment_upstream = match contact {
                Some(contact) => {
                    let node = settings.add_peer(&self.vault, contact)?;
                    if !settings.issuers.contains(&node) {
                        return Err("upstream must name an issuer".into());
                    }
                    Some(node)
                }
                None => None,
            };
            Ok(())
        })
        .await?;
        self.wake_sync();
        Ok(())
    }

    async fn add_peer(&self, contact: &str) -> Result<String, Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let node = self
            .configure(|settings| {
                let node = settings.add_peer(&self.vault, contact)?;
                if registry.revoked(&node) {
                    return Err("the contact names a revoked device".into());
                }
                Ok(node)
            })
            .await?;
        self.reset_retry(Some(node));
        self.activity.record(crate::ActivityKind::PeerAdded {
            peer: (node).to_owned(),
        });
        self.wake_sync();
        Ok(node.to_hex())
    }

    async fn remove_peer(&self, node: PublicKey) -> Result<(), Failure> {
        self.configure(|settings| {
            if settings.enrollment_upstream == Some(node) {
                return Err("clear enrollment forwarding before removing its issuer".into());
            }
            settings.remove_peer(node)
        })
        .await?;
        self.reached()?.remove(&node);
        self.met
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&node);
        if let Some(service) = self.current() {
            service.live.close_peer(node);
        }
        self.retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&node);
        self.activity.record(crate::ActivityKind::PeerRemoved {
            peer: (node).to_owned(),
        });
        self.wake_sync();
        Ok(())
    }

    pub(crate) async fn admit_request(&self, kind: RequestKind) -> Result<(), Failure> {
        if self.lifecycle.lock().await.is_logged_out() && !kind.allowed_after_logout() {
            return Err(refused("this device logged out of the domain"));
        }
        if matches!(kind, RequestKind::Tips) {
            self.revocation_notice
                .admit_personal(&self.settings.published())?;
        }
        Ok(())
    }

    fn reached(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<PublicKey, Reached>>, Failure> {
        self.reached
            .lock()
            .map_err(|_| "peer status poisoned".into())
    }
    async fn configure<T>(
        &self,
        change: impl FnOnce(&mut Settings) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let mut settings = self.settings.lock().await;
        let mut draft = settings.clone();
        let result = change(&mut draft).map_err(named)?;
        if draft.persisted_eq(&settings) {
            *settings = draft;
        } else {
            settings.commit(draft, &self.path)?;
        }
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? =
            settings.granted(&self.domain());
        self.observations.touch();
        Ok(result)
    }
    async fn grant(
        &self,
        device: crate::NodeKeys,
        profile: Profile,
        terms: Terms,
        delegate: bool,
    ) -> Result<crate::Supplied, Failure> {
        self.settings.lock().await.admits(Some(&profile))?;
        let contact = match self.current() {
            Some(link) if !delegate => {
                let settings = self.settings.lock().await.clone();
                Some(settings.contact(&self.vault, &*link.transport).await?.0)
            }
            _ => None,
        };
        Box::pin(self.answer_grant(device, (profile, terms), delegate, contact)).await
    }

    async fn install_supplied(&self, supplied: crate::Supplied) -> Result<(), Failure> {
        let current = self.settings.lock().await.clone();
        let renewed = current
            .supplied_renewal(&self.vault, &supplied)
            .map_err(refused)?;
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let mut settings = self.settings.lock().await;
        let (mut keys, _) = settings.adopt(&self.path, &self.vault, &renewed)?;
        if settings.begin_exposure_rotation(&registry)? {
            settings.save(&self.path)?;
            keys.extend(
                settings
                    .rotate(&self.vault)?
                    .ok_or("exposure rotation intent disappeared")?,
            );
            settings.save(&self.path)?;
        }
        let fresh = keys.len();
        self.apply(&mut settings, keys).await?;
        self.activity.record(crate::ActivityKind::Renewed {
            revocations: settings.revocation_status()?,
            expires: (settings.expires()) as u64,
            keys: fresh,
        });
        self.hold_keys(&settings, &settings.partitions(&self.domain())?)
            .await?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? =
            settings.granted(&self.domain());
        drop(settings);
        Ok(())
    }
    fn current(&self) -> Option<Arc<Link>> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn kind(&self) -> String {
        self.current()
            .map(|link| link.transport.kind().to_owned())
            .unwrap_or_default()
    }
    async fn retention(
        &self,
        collection: &str,
        policy: Option<crate::RetentionPolicy>,
        maintain: bool,
    ) -> Result<crate::RetentionStatus, Failure> {
        let scope = self
            .domain()
            .scopes()
            .find(|(name, _)| *name == collection)
            .map(|(_, scope)| scope)
            .filter(|scope| self.scopes().contains(scope))
            .ok_or_else(|| refused("retention scope is not granted"))?;
        if let Some(policy) = policy {
            self.replica.set_retention_policy(scope, policy).await?;
        }
        if maintain {
            self.replica.prune(scope).await?;
        }
        let status = self.replica.checkpoint_status(scope).await?;
        Ok(crate::RetentionStatus {
            policy: self.replica.retention_policy(scope).await?,
            checkpoint: status,
        })
    }
    async fn session_info(&self) -> Result<crate::SessionInfo, Failure> {
        let session = self
            .replica
            .session(self.app_scopes().into_iter().collect())
            .await?;
        let status = session.status().await?;
        let settings = self.settings.published();
        Ok(crate::SessionInfo {
            principal: status.principal,
            node: status.node,
            revocations: settings.revocation_status()?,
            expires: status.expires,
            issuer: settings.issuer(),
            profile: settings.profile.clone(),
            transport: self.kind(),
            readable: status.readable.len(),
            writable: status.writable.len(),
            writer: crate::WriterId::from_bytes(*status.node.as_bytes()),
        })
    }
    async fn bootstrap_progress(&self) -> Result<Option<crate::BootstrapProgress>, Failure> {
        let Some(intent) = self.bootstrap_intent.load()? else {
            return Ok(None);
        };
        let totals = intent.totals();
        if totals.is_empty() {
            return Ok(None);
        }
        let (mut rows_done, mut scopes_done) = (0, 0);
        for planned in totals {
            if intent.imports().includes(&planned.scope) {
                rows_done += self
                    .replica
                    .imported_rows(planned.scope, planned.checkpoint)
                    .await?
                    .min(planned.rows);
            } else {
                rows_done += planned.rows;
                scopes_done += 1;
            }
        }
        Ok(Some(crate::BootstrapProgress {
            rows_done,
            rows_total: totals.iter().map(|planned| planned.rows).sum::<u64>(),
            scopes_done,
            scopes_total: totals.len() as u64,
        }))
    }
    async fn readiness_after(
        &self,
        client: &Client,
        after: &str,
    ) -> Result<crate::DatabaseStatus, Failure> {
        let mut revisions = self.replica.observe_revision();
        let mut imports = self.replica.observe_imports().await;
        let mut authority = self.replica.observe_authority().await;
        let mut sync_state = self.sync_state.subscribe();
        let mut closing = client.closing.subscribe();
        let mut events = self.sync_events.subscribe();
        let mut network = self.sync_status.subscribe();
        let mut observations = self.observations.subscribe();
        let mut worker = self.worker_running.subscribe();
        let mut control = self.sync_control.subscribe();
        loop {
            let mut transport = self.current().and_then(|service| service.changes.clone());
            let (time_before_status, instant_before_status) =
                (self.status_time()?, rocky_platform::time::Instant::now());
            let status = self.observed_status().await?;
            if status.token != after {
                return Ok(status);
            }
            tokio::select! {
                changed = revisions.changed() => {
                    changed.map_err(|_| ServiceError::Closed("database"))?
                }
                _ = closing.wait_for(|closing| *closing) => {
                    return Err(ServiceError::Closed("session").into());
                }
                changed = sync_state.changed() => {
                    changed.map_err(|_| ServiceError::Closed("database"))?
                }
                _ = events.changed() => {},
                _ = network.changed() => {},
                _ = observations.changed() => {},
                _ = worker.changed() => {},
                _ = authority.changed() => {},
                _ = control.changed() => {},
                _ = async { match transport.as_mut() { Some(events) => { let _ = events.changed().await; }, None => std::future::pending().await } } => {},
                changed = imports.changed() => {
                    changed.map_err(|_| ServiceError::Closed("database"))?
                }
                _ = async {
                    let settings = self.settings.lock().await;
                    let expires = std::iter::once(settings.expires()).chain(settings.revocations.statements().map(|notice| notice.expiry_day().saturating_mul(rocky_credentials::DAY))).chain(self.health_deadline(time_before_status)).chain(self.stale_deadline(time_before_status)).filter(|expires| *expires > time_before_status && *expires < rocky_credentials::NEVER / 2).min();
                    drop(settings);
                    if let Some(expires) = expires {
                        rocky_platform::time::sleep_until(instant_before_status + Duration::from_secs(expires - time_before_status)).await;
                    } else {
                        std::future::pending().await
                    }
                    Ok::<_, Failure>(())
                } => {},
            }
        }
    }
    async fn backup(&self, target: &std::path::Path) -> Result<crate::BackupInventory, Failure> {
        let _backup = self.backups.lock().await;
        if rocky_platform::fs::exists(target) {
            return Err(refused("the backup target exists"));
        }
        let directory = self
            .path
            .parent()
            .ok_or("the identity has no data directory")?
            .to_path_buf();
        let staging = directory.join(format!(
            "backup-{}",
            HEXLOWER.encode(
                &commitment(
                    "rocky.sdk.backup-staging.v1",
                    target.as_os_str().as_encoded_bytes()
                )[..8]
            )
        ));
        let _ = rocky_platform::fs::remove_dir_all(&staging);
        rocky_platform::fs::create_dir_all(&staging)?;
        let result = async {
            let settings = self.settings.lock().await;
            let database = staging.join("rocky.sqlite");
            let revision = self.replica.backup_into(&database).await?;
            let mut snapshot = settings.clone();
            if let Some(naming) = self.replica.custody_naming_escrow().await? {
                snapshot.naming_escrow = Some(naming);
            }
            drop(settings);
            let settings = snapshot;
            let identity = staging.join("identity.json");
            rocky_platform::fs::write(&identity, serde_json::to_vec(&*settings)?)?;
            let staged = vec![("identity.json", identity), ("rocky.sqlite", database)];
            let node = PrivateKey::from_bytes(&self.vault.get("node")?);
            let scope = self.domain().scope;
            let target = target.to_path_buf();
            let inventory = rocky_platform::task::spawn_blocking(move || {
                crate::backup::write(&target, &staged, &settings, scope, revision, &node, now()?)
            })
            .await??;
            Ok::<_, Failure>(inventory)
        }
        .await;
        let _ = rocky_platform::fs::remove_dir_all(&staging);
        result
    }
    pub(crate) async fn export_backup(&self) -> Result<Vec<u8>, Failure> {
        let directory = self
            .path
            .parent()
            .ok_or("the identity has no data directory")?;
        let target = directory.join(format!(
            "export-{}.backup",
            HEXLOWER.encode(&crate::enrollment::random::<8>())
        ));
        let exported = async {
            self.backup(&target).await?;
            Ok(rocky_platform::fs::read(&target)?)
        }
        .await;
        for leftover in [&target, &target.with_extension("partial")] {
            if rocky_platform::fs::exists(leftover) {
                rocky_platform::fs::remove_file(leftover)?;
            }
        }
        exported
    }
    async fn domain_info(&self) -> Result<crate::DomainInfo, Failure> {
        let settings = self.settings.lock().await;
        Ok(crate::DomainInfo {
            scope: self.domain().scope,
            issuer: settings.issuer(),
            profile: settings.profile.clone(),
            collections: settings
                .schema
                .names()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            transport: self.kind(),
        })
    }
    async fn generation(&self) -> Result<crate::GenerationStatus, Failure> {
        Ok(self.observed_status().await?.data.generation)
    }
    async fn revoke_identity(
        &self,
        identity: PublicKey,
        mode: rocky_credentials::RevocationMode,
        root: &Root,
        keep: bool,
    ) -> Result<crate::RevocationResult, Failure> {
        let target =
            if root.signer().is_some() || mode == rocky_credentials::RevocationMode::Compromised {
                crate::enrollment::Revoke::TrustedIdentity(identity, mode)
            } else {
                crate::enrollment::Revoke::Identity(identity)
            };
        let result = self.revoke_with(target, root, keep).await?;
        self.activity.record(crate::ActivityKind::IdentityRevoked {
            identity: (identity).to_owned(),
        });
        Ok(result)
    }
    async fn revoke(
        &self,
        target: crate::enrollment::Revoke,
    ) -> Result<crate::RevocationResult, Failure> {
        self.revoke_with(target, &Root::default(), false).await
    }
    fn record_ended(
        &self,
        signed: &rocky_credentials::SignedRevocation,
        identity: PublicKey,
    ) -> Result<(), Failure> {
        let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let statement = &signed.statement;
        if statement.reset().is_some() && signed.root() {
            self.activity.record(crate::ActivityKind::NodeReset {
                node,
                reset: signed.id()?,
            });
        } else if !statement.identities.contains(&identity)
            && statement
                .nodes
                .contains(&rocky_credentials::PeerId::of(node))
        {
            self.activity.record(crate::ActivityKind::NodeUnlinked {
                identity: (identity).to_owned(),
                node: (node).to_owned(),
            });
        } else {
            self.activity.record(crate::ActivityKind::IdentityRevoked {
                identity: (identity).to_owned(),
            });
        }
        Ok(())
    }
    pub(crate) async fn revoke_lost(
        &self,
        root: Arc<dyn rocky_credentials::IdentityProvider>,
        archived: &Settings,
    ) -> Result<Vec<PublicKey>, Failure> {
        let root = Root::held(root);
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let own = self.settings.lock().await.identity;
        let lost: Vec<PublicKey> = registry
            .trusted
            .keys()
            .copied()
            .filter(|identity| {
                *identity != own
                    && !archived.revocations.revokes_trusted(identity)
                    && !archived.tombstoned(identity)
            })
            .collect();
        for identity in &lost {
            let revoked_with_cut = {
                let settings = self.settings.lock().await;
                settings.revocations.revokes_trusted(identity) || settings.tombstoned(identity)
            };
            if revoked_with_cut {
                continue;
            }
            let target = crate::enrollment::Revoke::TrustedIdentity(
                *identity,
                rocky_credentials::RevocationMode::Compromised,
            );
            self.revoke_with(target, &root, true)
                .await
                .map_err(|error| format!("revoking lost trusted identity {identity}: {error}"))?;
            self.activity.record(crate::ActivityKind::IdentityRevoked {
                identity: (identity).to_owned(),
            });
        }
        if let Some(directory) = self.path.parent() {
            crate::recovery::fault::check(directory, crate::recovery::fault::Stage::Named)?;
        }
        self.revoke_before_recovery(&root).await?;
        Ok(lost)
    }
    async fn revoke_before_recovery(&self, root: &Root) -> Result<(), Failure> {
        let graph = self.replica.tip_graph(self.domain().scope).await?;
        let mut settings = self.settings.lock().await;
        let mut cut = Vec::new();
        for identity in graph.chain_identities().map_err(refused)? {
            if identity == settings.identity || settings.revocations.revokes_trusted(&identity) {
                continue;
            }
            let lanes = graph
                .lanes_of(&identity)
                .map_err(refused)?
                .into_iter()
                .filter(|lane| !settings.revocations.ends_node_of(identity, *lane, true))
                .collect();
            let (kept, dropped) = graph.proposed_cut(&identity, &lanes).map_err(refused)?;
            if !dropped.is_empty() {
                self.activity
                    .record(crate::ActivityKind::RevocationCutDroppedStages {
                        identity: Some(identity),
                        count: dropped.len(),
                        heads: Vec::new(),
                    });
            }
            cut.extend(kept);
        }
        let mut draft = settings.clone();
        if !draft.revoke_before_recovery(root.signer(), cut)? {
            return Ok(());
        }
        settings.commit(draft, &self.path)?;
        self.apply(&mut settings, Vec::new()).await?;
        drop(settings);

        if let Err(error) = self.push_revocations().await {
            self.activity
                .record(crate::ActivityKind::RevocationsUnpushed {
                    error: (error.to_string()).to_owned(),
                    retry: None,
                });
        }
        Ok(())
    }
    async fn revocation_cut(
        &self,
        target: crate::enrollment::Revoke,
        membership: &crate::membership::Membership,
    ) -> Result<RevocationCut, Failure> {
        let graph = self.replica.tip_graph(self.domain().scope).await?;
        let (identity, lanes): (PublicKey, std::collections::BTreeSet<PublicKey>) = match target {
            crate::enrollment::Revoke::Identity(identity)
            | crate::enrollment::Revoke::TrustedIdentity(identity, _) => (
                identity,
                membership
                    .nodes(&identity)
                    .map(|linked| linked.node)
                    .collect(),
            ),
            crate::enrollment::Revoke::Claims(_) => return Ok(Default::default()),
        };
        let (mut cut, dropped) = graph.proposed_cut(&identity, &lanes).map_err(refused)?;
        for lane in lanes {
            if !cut.iter().any(|cut| cut.lane == lane) {
                cut.push(graph.heads(&identity, &lane)?);
            }
        }
        cut.sort_by_key(|cut| (cut.identity, cut.lane));
        Ok((cut, dropped))
    }
    async fn revoke_with(
        &self,
        target: crate::enrollment::Revoke,
        root: &Root,
        keep: bool,
    ) -> Result<crate::RevocationResult, Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let membership = self.membership().await?;
        let (cut, dropped) = if keep && root.signer().is_some() {
            self.revocation_cut(target, &membership).await?
        } else {
            Default::default()
        };
        let dropped = dropped
            .iter()
            .map(|(lane, head)| crate::DroppedStage {
                node: lane.to_hex(),
                sequence: head.sequence,
                head: HEXLOWER.encode(&head.id),
            })
            .collect::<Vec<_>>();
        if !dropped.is_empty() {
            self.activity
                .record(crate::ActivityKind::RevocationCutDroppedStages {
                    identity: None,
                    count: dropped.len(),
                    heads: dropped.clone(),
                });
        }
        let mut settings = self.settings.lock().await;
        let mut draft = settings.clone();
        draft
            .begin_revocation_cut(target, (&registry, &membership), root.signer(), cut)
            .map_err(|error| -> Failure {
                if error.is::<crate::enrollment::RevocationSetFull>()
                    || error.is::<crate::enrollment::PublisherRequired>()
                {
                    error
                } else {
                    refused(error)
                }
            })?;
        settings.commit(draft.clone(), &self.path)?;
        let rotation = draft
            .authority
            .as_ref()
            .and_then(|authority| authority.rotation.clone())
            .ok_or("the revocation was not begun")?;
        let keys = draft.rotate(&self.vault)?.unwrap_or_default();
        draft.drop_revoked_deliveries();
        settings.commit(draft, &self.path)?;
        self.apply(&mut settings, keys).await?;
        let rotated: Vec<_> = settings
            .partitions(&self.domain())?
            .into_iter()
            .filter(|partition| {
                rotation
                    .groups
                    .contains_key(&crate::groups::record_key(&partition.record.address))
            })
            .collect();
        if !rotated.is_empty() {
            self.hold_keys(&settings, &rotated).await?;
        }
        let status = settings.revocation_status()?;
        drop(settings);
        Box::pin(self.complete_revocation()).await?;
        if let Err(error) = self.push_revocations().await {
            self.activity
                .record(crate::ActivityKind::RevocationsUnpushed {
                    error: (error.to_string()).to_owned(),
                    retry: None,
                });
        }
        if !matches!(target, crate::enrollment::Revoke::Claims(_))
            && let Err(error) = self.seal_for_members().await
        {
            self.activity
                .record(crate::ActivityKind::RevocationSealFailed {
                    error: (error.to_string()).to_owned(),
                });
        }
        Ok(crate::RevocationResult {
            statement: HEXLOWER.encode(&rotation.statement.id()?),
            revocations: status,
            cut_dropped_stages: dropped,
        })
    }
    pub(super) async fn apply(
        &self,
        settings: &mut SettingsGuard<'_>,
        mut keys: Vec<(String, rocky_core::KeyEpoch, [u8; 32])>,
    ) -> Result<(), Failure> {
        let chain = settings.credentials.clone();
        self.replica
            .learn_revocations(&settings.revocations)
            .await?;
        self.replica.renew_credentials(chain.clone()).await?;
        if let Some(client) = self.coordinator()
            && let Some(certificate) = chain.certificate()
        {
            client.certify(certificate.clone());
        }
        if let Err(error) = self.refresh_line_node(settings).await {
            self.activity
                .record_changed(crate::ActivityKind::LineNodeFailed {
                    error: (error.to_string()).to_owned(),
                });
        }
        keys.sort_by_key(|(_, epoch, _)| *epoch);
        for (name, generation, key) in keys {
            let scope = self
                .domain()
                .scopes()
                .find(|(collection, _)| *collection == name)
                .map(|(_, scope)| scope)
                .ok_or("unknown collection")?;
            self.replica
                .install_key_epoch(KeyMaterial::at(scope, generation, key))
                .await?;
        }
        crate::escrow::keep(&self.replica, settings, &self.vault, &self.domain()).await
    }
    async fn renew(
        &self,
        transport: Option<&dyn Transport>,
        learn: bool,
        first: Option<PublicKey>,
        epochs_only: bool,
    ) -> Result<bool, Failure> {
        let attached = self.current();
        let transport = transport.or_else(|| attached.as_ref().map(|link| &*link.transport));
        if let Some(transport) = transport {
            Box::pin(self.resolve_saved_contacts(transport)).await;
        }
        let current = self.settings.lock().await.clone();
        let request = async {
            if epochs_only {
                current
                    .request_epochs(
                        &self.vault,
                        transport.ok_or(ServiceError::NetworkRequired("epoch delivery"))?,
                        first,
                    )
                    .await
            } else {
                current
                    .request_renewal(&self.vault, transport, learn, first)
                    .await
            }
        };
        let mut renewed = match request.await {
            Ok(renewed) => renewed,
            Err(error) => {
                if let Some(revoked) = error.downcast_ref::<crate::enrollment::Revoked>() {
                    if self
                        .revocation_notice
                        .record_unless_revoked(&current, revoked.0.clone())?
                    {
                        self.record_ended(&revoked.0, current.identity)?;
                    }
                    self.wake_sync();
                }
                return Err(error);
            }
        };
        if let Some(peer) = renewed.contacted {
            self.inbound_sync(peer);
        }
        self.learned.store(false, Ordering::SeqCst);
        let mut settings = self.settings.lock().await;
        let restores_revoked_authors =
            (!epochs_only || !settings.issuer()) && settings.lacks(&renewed.revocations)?;
        if restores_revoked_authors {
            self.bootstrap_intent.request_checkpoint_import()?;
        }
        if epochs_only {
            renewed.chain = settings.credentials.clone();
        }
        let (keys, adopted) = settings.adopt(&self.path, &self.vault, &renewed)?;
        let mut changed = !epochs_only || adopted;
        if changed {
            let readable = self
                .granted
                .lock()
                .map_err(|_| "scope catalog poisoned")?
                .len();
            self.apply(&mut settings, keys).await?;
            self.hold_keys(&settings, &settings.partitions(&self.domain())?)
                .await?;
            let grants_widened = settings.granted(&self.domain()).len() > readable;
            if grants_widened {
                let domain = self.domain();
                let mut widened = (**settings).clone();
                if crate::database::attach(&self.replica, &mut widened, &self.vault, &domain)
                    .await?
                {
                    settings.commit(Shared::new(widened), &self.path)?;
                }
            }
        }
        if settings.issuer() {
            drop(settings);
            let registry = self.registry(&*self.settings_snapshot().await).await?;
            settings = self.settings.lock().await;
            if settings.begin_exposure_rotation(&registry)? {
                changed = true;
                settings.save(&self.path)?;
                let keys = settings
                    .rotate(&self.vault)?
                    .ok_or("exposure rotation intent disappeared")?;
                settings.save(&self.path)?;
                self.apply(&mut settings, keys).await?;
                self.hold_keys(&settings, &settings.partitions(&self.domain())?)
                    .await?;
            }
        }
        if !changed {
            return Ok(false);
        }
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? =
            settings.granted(&self.domain());
        drop(settings);
        self.refresh_importing().await?;
        if self.settings.lock().await.issuer() {
            Box::pin(self.complete_revocation()).await?;
        }
        if !epochs_only && let Err(error) = Box::pin(self.record_self()).await {
            self.activity.record(crate::ActivityKind::RegistryFailed {
                error: (error.to_string()).to_owned(),
            });
        }
        self.wake_sync();
        Ok(true)
    }
    async fn logout(&self, purge: bool) -> Result<(), Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let membership = self.membership().await?;
        let mut settings = self.settings.lock().await;
        if settings.issuer() && !settings.another_issuer(&registry, &membership) {
            return Err(refused(
                "this device holds the only known copy of the domain's publisher key and cannot log out",
            ));
        }
        self.replica.logout().await;
        settings.logged_out = true;
        settings.save(&self.path)?;
        *self.lifecycle.lock().await = Lifecycle::LoggedOut {
            purge_on_close: purge,
        };
        self.automatic.store(false, Ordering::SeqCst);
        self.wake_sync();
        self.vault.forget(settings.roles())?;
        Ok(())
    }

    pub(crate) async fn initial_sync(&self) -> Result<(), Failure> {
        let network = self.network.read().await;
        let link = network
            .as_ref()
            .ok_or(ServiceError::NetworkRequired("bootstrap"))?;
        let mut stage = link.stage.subscribe();
        tokio::select! {
            result = self.sync_optional(Some(&*link.transport)) => result.map(|_| ()),
            _ = stage.wait_for(|stage| *stage == Stage::Cancelled) => Err(ServiceError::Closed("network service").into()),
        }
    }

    fn record_reach(
        &self,
        peer: PublicKey,
        before: Option<Reached>,
        result: &Result<Exchanged, Failure>,
    ) {
        let (before, refused_before, waiting_before) = match before {
            Some(reached) => (Some(reached.error), reached.refused, reached.waiting),
            None => (None, Vec::new(), Vec::new()),
        };
        match result {
            Err(error) => {
                let error = error.to_string();
                if before.flatten().as_ref() != Some(&error) {
                    self.activity
                        .record(crate::ActivityKind::PeerAttemptFailed {
                            peer: (peer).to_owned(),
                            error: (error).to_owned(),
                        });
                }
            }
            Ok(Exchanged { transfer, .. }) => {
                if !matches!(before, Some(None)) {
                    self.activity
                        .record(crate::ActivityKind::PeerReconciliationCompleted {
                            peer: (peer).to_owned(),
                        });
                }
                let refusals_changed = transfer.refused != refused_before;
                if refusals_changed {
                    let scopes = transfer
                        .refused
                        .iter()
                        .map(|scope| self.scope_label(scope))
                        .collect();
                    self.activity.record(crate::ActivityKind::ScopesRefused {
                        peer: (peer).to_owned(),
                        scopes,
                    });
                }
                if transfer.waiting != waiting_before {
                    let scopes = transfer
                        .waiting
                        .iter()
                        .map(|scope| self.scope_label(scope))
                        .collect();
                    self.activity.record(crate::ActivityKind::ScopesWaiting {
                        peer: (peer).to_owned(),
                        scopes,
                    });
                }
                if transfer.scopes.is_empty() {
                    return;
                }
                let scopes = transfer
                    .scopes
                    .iter()
                    .map(|(scope, moved)| crate::ActivityTransfer {
                        scope: self.scope_label(scope),
                        pulled: moved.pulled as u64,
                        pushed: moved.pushed as u64,
                        bytes: moved.bytes as u64,
                    })
                    .collect();
                let (pulled, pushed) = transfer
                    .scopes
                    .iter()
                    .fold((0, 0), |(pulled, pushed), (_, moved)| {
                        (pulled + moved.pulled, pushed + moved.pushed)
                    });
                if pulled + pushed > 0 {
                    self.activity.record(crate::ActivityKind::Synced {
                        peer: (peer).to_owned(),
                        pulled: (pulled) as u64,
                        pushed: (pushed) as u64,
                        scopes,
                    });
                }
                for (scope, moved) in &transfer.scopes {
                    if let Some((checkpoint, superseded)) = moved.checkpoint {
                        self.activity
                            .record(crate::ActivityKind::CheckpointImported {
                                scope: self.scope_label(scope),
                                peer,
                                checkpoint,
                                superseded,
                            });
                    }
                }
            }
        }
    }
    fn scope_label(&self, scope: &ResourceScope) -> crate::ActivityScope {
        let domain = self.domain();
        let own = domain.scopes().find(|(_, own)| own.table == scope.table);
        crate::ActivityScope {
            table: own.map(|(name, _)| name.to_owned()),
            partition: own
                .is_some_and(|(_, own)| own != *scope)
                .then_some(scope.encryption),
        }
    }
    fn wake_sync(&self) {
        self.sync_events
            .send_modify(|event| *event = event.wrapping_add(1));
    }
    fn synced(&self, mut status: SyncStatus) {
        self.sync_status.send_modify(|current| {
            status.token = current.token.wrapping_add(1);
            *current = status;
        });
    }
    fn control_sync(&self) {
        self.sync_control
            .send_modify(|event| *event = event.wrapping_add(1));
        self.wake_sync();
    }
    fn peer_retry(&self, peer: PublicKey) -> PeerRetry {
        let identity = format!(
            "{}:{peer}",
            self.node_identity.as_deref().unwrap_or("unavailable")
        );
        PeerRetry {
            retry: crate::scheduling::Retry::seeded(self.retry_seed, identity.as_bytes()),
            link_retry: crate::scheduling::Retry::seeded(self.retry_seed, identity.as_bytes()),
            ..PeerRetry::default()
        }
    }
    pub(super) fn reset_retry(&self, peer: Option<PublicKey>) {
        let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        for (node, state) in retries.iter_mut() {
            if peer.is_none_or(|peer| peer == *node) {
                state.retry.wake();
                state.link_retry.wake();
                state.short = false;
                state.generation = state.generation.wrapping_add(1);
            }
        }
    }
    fn inbound_sync(&self, peer: PublicKey) {
        if let Ok(at) = now() {
            let mut met = self.met.lock().unwrap_or_else(PoisonError::into_inner);
            met.insert(peer, at);
            if met.len() > MET
                && let Some(oldest) = met.iter().min_by_key(|(_, at)| **at).map(|(node, _)| *node)
            {
                met.remove(&oldest);
            }
        }
        let backed_off = self
            .retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .is_some_and(|state| state.retry.deadline().is_some());
        if backed_off {
            self.reset_retry(Some(peer));
            self.wake_sync();
        }
    }
    fn pending_sync(&self) {
        for state in self
            .retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values_mut()
        {
            state.repair_all();
        }
    }
    fn pending_without_links(&self, service: &Link) {
        for (peer, state) in self
            .retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter_mut()
        {
            if !service.live.contains(*peer) {
                state.repair_all();
            }
        }
    }
    fn retry_deadline(&self) -> Option<rocky_platform::time::Instant> {
        self.retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .flat_map(|state| {
                [
                    if state.pending.pending() {
                        state.retry.deadline()
                    } else {
                        None
                    },
                    if state.link {
                        state.link_retry.deadline()
                    } else {
                        None
                    },
                ]
            })
            .flatten()
            .chain(
                self.resolution_retries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .values()
                    .filter_map(crate::scheduling::Retry::deadline),
            )
            .chain(
                self.unlink_retries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .values()
                    .filter_map(crate::scheduling::Retry::deadline),
            )
            .chain(
                self.schema_acquisition
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .retry
                    .deadline(),
            )
            .min()
    }
    async fn synchronize(self: Arc<Self>, link: Arc<Link>) {
        let mut stage = link.stage.subscribe();
        let mut events = self.sync_events.subscribe();
        let mut control = self.sync_control.subscribe();
        let mut frames = self.replica.observe_frames().await;
        let mut revisions = self.replica.observe_revision();
        let mut connectivity = link.changes.clone();
        let mut link_events = self.link_events.subscribe();
        let mut retry_only = false;
        loop {
            if *stage.borrow() != Stage::Open {
                break;
            }
            control.borrow_and_update();
            let missed_queued_work = events.has_changed().unwrap_or(false);
            if missed_queued_work {
                self.pending_sync();
                retry_only = false;
            }
            events.borrow_and_update();
            if self.automatic.load(Ordering::SeqCst)
                && self.online.load(Ordering::SeqCst)
                && self.foreground.load(Ordering::SeqCst)
            {
                self.worker_running.send_replace(true);
                let running = WorkerActivity(self.worker_running.clone());
                let result = tokio::select! {
                    biased;
                    _ = stage.wait_for(|stage| *stage != Stage::Open) => break,
                    _ = control.changed() => continue,
                    result = async {
                        let attached = self.network.read().await;
                        if attached.as_ref().is_none_or(|current| current.id != link.id) {
                            return Ok(PassOutcome::idle());
                        }
                        let result = self.sync_pass(&*link.transport, true, retry_only).await;
                        self.open_links(&link).await;
                        result
                    } => result,
                };
                drop(running);
                self.topics_changed();
                let status = match result {
                    Ok(outcome) => outcome.into_status(),
                    Err(error) if error.is::<crate::enrollment::Revoked>() => {
                        SyncStatus::failed(SyncState::Revoked, error.to_string(), false)
                    }
                    Err(error) => SyncStatus::failed(
                        SyncState::Unavailable,
                        error.to_string(),
                        self.retry_deadline().is_some(),
                    ),
                };
                self.synced(status);
            } else if !self.online.load(Ordering::SeqCst) {
                self.synced(SyncStatus::new(SyncState::NetworkingDisabled));
            }
            retry_only = false;
            let deadline = self.retry_deadline();
            tokio::select! {
                _ = stage.wait_for(|stage| *stage != Stage::Open) => break,
                _ = control.changed() => {},
                changed = events.changed() => { if changed.is_err() { break; } self.pending_sync(); }
                changed = frames.changed() => { if changed.is_err() { break; } settle().await; self.pending_without_links(&link); retry_only = true; }
                changed = revisions.changed() => { if changed.is_err() { break; } settle().await; self.pending_without_links(&link); retry_only = true; }
                changed = link_events.changed() => { if changed.is_err() { break; } retry_only = true; }
                () = async {
                    match &mut connectivity {
                        Some(events) => { if events.changed().await.is_err() { connectivity = None; } },
                        None => std::future::pending().await,
                    }
                } => { self.reset_retry(None); }
                () = async {
                    match deadline {
                        Some(deadline) => rocky_platform::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                }, if self.automatic.load(Ordering::SeqCst) && self.online.load(Ordering::SeqCst) && self.foreground.load(Ordering::SeqCst) => { retry_only = true; }
            }
        }
    }

    async fn sync_optional(
        &self,
        transport: Option<&dyn Transport>,
    ) -> Result<(crate::ReconciliationReport, bool), Failure> {
        let result = self.reconciliation_pass(transport, false, false).await;
        self.topics_changed();
        let (report, outcome) = result?;
        Ok((report, outcome.retry))
    }
    #[cfg(test)]
    async fn sync(&self, transport: &dyn Transport) -> Result<(serde_json::Value, bool), Failure> {
        let (report, retry) = self.sync_optional(Some(transport)).await?;
        Ok((crate::test_snapshot::Snapshot::snapshot(report), retry))
    }
    async fn sync_pass(
        &self,
        transport: &dyn Transport,
        automatic: bool,
        retry_only: bool,
    ) -> Result<PassOutcome, Failure> {
        self.reconciliation_pass(Some(transport), automatic, retry_only)
            .await
            .map(|(_, outcome)| outcome)
    }
    fn dialed(&self, settings: &Settings, backoff: bool) -> Vec<(PublicKey, Contact)> {
        let reached = self.reached.lock().unwrap_or_else(PoisonError::into_inner);
        let retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        let needed = self.unpartitioned_scopes();
        let mut targets = settings.targets();
        let waiting = |node: &PublicKey| {
            backoff && retries.get(node).is_some_and(|state| !state.retry.ready())
        };
        targets.sort_by_key(|(node, _)| {
            let meta = settings.contact_meta.get(node);
            (
                waiting(node),
                !meta.is_some_and(|meta| meta.local),
                !reached.get(node).is_some_and(|last| {
                    last.error.is_none() && needed.iter().any(|scope| last.held.contains(scope))
                }),
                !(settings.issuers.contains(node) || meta.is_some_and(|meta| meta.trusted)),
                reached.get(node).map(|last| last.at),
            )
        });
        let mut choices = self.choices.lock().unwrap_or_else(PoisonError::into_inner);
        choices.clear();
        for (index, (node, _)) in targets.iter().enumerate() {
            let meta = settings.contact_meta.get(node);
            let reason = if waiting(node) {
                "backoff"
            } else if index >= crate::enrollment::MAX_DIALED {
                "pass_limit"
            } else if meta.is_some_and(|meta| meta.local) {
                "selected_local"
            } else if reached.get(node).is_some_and(|last| {
                last.error.is_none() && needed.iter().any(|scope| last.held.contains(scope))
            }) {
                "selected_holding"
            } else if settings.issuers.contains(node) || meta.is_some_and(|meta| meta.trusted) {
                "selected_trusted"
            } else {
                "selected_probe"
            };
            choices.insert(*node, reason);
        }
        targets.truncate(crate::enrollment::MAX_DIALED);
        targets
    }
    async fn absorb_revocations(&self, outcome: Option<&Failure>) -> Result<(), Failure> {
        let rotates_locally = !self.settings.lock().await.renews_from_issuer();
        let registry = if rotates_locally {
            Some(self.registry(&*self.settings_snapshot().await).await?)
        } else {
            None
        };
        let held = self.replica.revocations().await;
        let revoking = outcome.and_then(revoked_by);
        let mut settings = self.settings.lock().await;
        let resumes = registry.is_some()
            && settings
                .authority
                .as_ref()
                .is_some_and(|authority| authority.rotation.is_some());
        if !settings.lacks(&held)? && revoking.is_none() && !resumes {
            return Ok(());
        }
        let mut draft = settings.clone();
        if draft.lacks(&held)? {
            let added = draft.learn_revocations(&held)?;
            if !added.is_empty() {
                draft.forget_revoked();
                draft.drop_revoked_deliveries();
                self.learned.store(true, Ordering::SeqCst);
            }
            if !draft.issuer() && !added.is_empty() {
                self.bootstrap_intent.request_checkpoint_import()?;
            }
        }
        if let Some(statement) = revoking
            && self
                .revocation_notice
                .record_unless_revoked(&draft, statement.clone())?
        {
            self.record_ended(&statement, draft.identity)?;
        }
        let rotated = match registry {
            Some(registry) => resumes || draft.begin_exposure_rotation(&registry)?,
            None => false,
        };
        settings.commit(draft, &self.path)?;
        if rotated {
            let mut rotation = (*settings).clone();
            let keys = rotation
                .rotate(&self.vault)?
                .ok_or("missing exposure rotation")?;
            settings.commit(rotation, &self.path)?;
            self.apply(&mut settings, keys).await?;
            self.hold_keys(&settings, &settings.partitions(&self.domain())?)
                .await?;
        }
        drop(settings);
        if rotated {
            Box::pin(self.complete_revocation()).await?;
        }
        self.refresh_importing().await?;
        self.sweep_deliveries().await
    }
    async fn bootstrap_pending(
        &self,
        intent: &BootstrapIntent,
    ) -> Result<Vec<ResourceScope>, Failure> {
        let pending = match intent.imports() {
            Imports::Granted => self.scopes(),
            Imports::Scopes(scopes) => scopes.clone(),
        };
        if pending.iter().any(|scope| !self.scopes().contains(scope)) {
            return Err(refused("bootstrap intent names an ungranted scope"));
        }
        let settings = self.settings.lock().await;
        let imports_checkpoint = |scope: &ResourceScope| {
            let holds_keys = settings.reads_whole(&self.domain(), scope);
            let holds_rows = !self.domain().partitioned_table(scope);
            holds_keys && holds_rows
        };
        Ok(pending.into_iter().filter(imports_checkpoint).collect())
    }
    async fn ensure_not_revoked(&self) -> Result<(), Failure> {
        match self.revocation_notice.revoked(&self.settings.published())? {
            Some(statement) => Err(Box::new(crate::enrollment::Revoked(statement))),
            None => Ok(()),
        }
    }
    async fn awaits_rotated_keys(&self) -> bool {
        self.learned.load(Ordering::SeqCst) || self.awaits_key_epoch().await
    }
    async fn awaits_key_epoch(&self) -> bool {
        self.replica.blockers().await.is_ok_and(|blockers| {
            blockers.iter().any(|blocker| {
                matches!(
                    blocker.kind,
                    rocky_core::BlockerKind::PendingFrames {
                        reason: rocky_core::WaitReason::OpeningKey,
                        ..
                    }
                )
            })
        })
    }
    fn awaited_inviter(&self) -> Result<Option<PublicKey>, Failure> {
        Ok(self
            .bootstrap_intent
            .load()?
            .and_then(|intent| intent.inviter()))
    }
    pub(crate) async fn refresh_importing(&self) -> Result<(), Failure> {
        let pending = match self.bootstrap_intent.load()? {
            Some(intent) => self.bootstrap_pending(&intent).await.unwrap_or_default(),
            None => Vec::new(),
        };
        self.replica
            .set_importing(pending.into_iter().collect())
            .await;
        Ok(())
    }
    fn keeps_import_source(
        &self,
        scope: ResourceScope,
        signer: PublicKey,
        cursor: Option<rocky_core::Commitment>,
    ) -> bool {
        let linked = self
            .current()
            .is_some_and(|service| service.live.contains(signer));
        let mut sources = self
            .import_sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !linked {
            sources.remove(&scope);
            return false;
        }
        let now = rocky_platform::time::Instant::now();
        let seen = sources.entry(scope).or_insert((cursor, now));
        if seen.0 != cursor {
            *seen = (cursor, now);
        }
        now.duration_since(seen.1) < IMPORT_SOURCE
    }
    async fn reconcile(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
    ) -> Result<Exchanged, Failure> {
        let _ = self.observations.partial_transfer(&peer.to_hex());
        let mut result = self.reconcile_observed(transport, peer, contact).await;
        if let Ok(Exchanged { transfer, .. }) = &mut result
            && let Some(partial) = self.observations.partial_transfer(&peer.to_hex())
        {
            transfer.extend(partial);
        }
        if let Ok(Exchanged { transfer, .. }) = &result {
            let mut failures = self
                .sync_failures
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let unanswered =
                |scope| transfer.failed.contains(scope) || transfer.denied.contains(scope);
            for scope in transfer
                .held
                .iter()
                .chain(transfer.scopes.iter().map(|(scope, _)| scope))
            {
                if !unanswered(scope) {
                    failures.retain(|(_, known), _| known != scope);
                }
            }
            for scope in &transfer.refused {
                if !unanswered(scope) {
                    failures.remove(&(peer, *scope));
                }
            }
            for scope in transfer.failed.iter().chain(&transfer.denied) {
                failures
                    .entry((peer, *scope))
                    .and_modify(|failure| failure.refused = transfer.denied.contains(scope))
                    .or_insert_with(|| SyncFailure {
                        refused: transfer.denied.contains(scope),
                        since: now().ok(),
                    });
            }
        }
        if let Ok(exchanged) = &mut result
            && exchanged.work == PeerWork::Done
            && self.owes_retry(peer, &exchanged.transfer).await
        {
            exchanged.work = PeerWork::Owed;
        }
        if let Ok(Exchanged { transfer, .. }) = &result
            && transfer.failed.is_empty()
            && transfer.denied.is_empty()
            && let Err(error) = self.witnessed_by(peer).await
        {
            tracing::warn!(%error, "the witnessing owner device could not be recorded");
        }
        result
    }
    async fn owes_retry(&self, peer: PublicKey, transfer: &sync::Transfer) -> bool {
        use rocky_core::{ClearsBy, domain_label, scope_label};
        let Ok(blockers) = self.blockers().await else {
            return false;
        };
        let served: std::collections::BTreeSet<String> = transfer
            .held
            .iter()
            .chain(transfer.scopes.iter().map(|(scope, _)| scope))
            .filter(|scope| !transfer.refused.contains(scope))
            .map(scope_label)
            .collect();
        let domain = domain_label(&self.domain().scope);
        let node = peer.to_hex();
        blockers.iter().any(|blocker| {
            let clears = match &blocker.clears_by {
                ClearsBy::AnyPeer => true,
                ClearsBy::Peer { node: by } => *by == node,
                ClearsBy::LocalWork
                | ClearsBy::Application
                | ClearsBy::NoPeer
                | ClearsBy::Clock => false,
            };
            clears
                && (served.contains(&blocker.scope)
                    || (blocker.scope == domain && !served.is_empty()))
        })
    }
    async fn reconcile_observed(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
    ) -> Result<Exchanged, Failure> {
        let mut result = Err("link authority kept changing during reconciliation".into());
        for _ in 0..4 {
            let revocations = self.replica.revocation_digest().await?;
            let first_attempt_id = self.observations.next_id();
            let mut attempt = self.observations.begin(
                peer.to_hex(),
                crate::AttemptOperation::Reconcile,
                crate::AttemptStage::ResolveRoute,
            );
            result = self.exchange(transport, peer, contact, &mut attempt).await;
            if result.is_err()
                && attempt.current_stage() == crate::AttemptStage::ResolveRoute
                && let Some(link) = self
                    .observations
                    .snapshot()
                    .into_iter()
                    .filter(|item| {
                        item.target == peer.to_hex()
                            && item.operation == crate::AttemptOperation::OpenLink
                            && item.id >= first_attempt_id
                    })
                    .max_by_key(|item| item.id)
            {
                attempt.stage(link.stage);
            }
            let failure = result
                .as_ref()
                .err()
                .map(|error| crate::status::failure(error, &attempt.current_stage()));
            attempt.finish(failure);
            self.absorb_revocations(result.as_ref().err()).await?;
            if result.is_err() && self.replica.revocation_digest().await? != revocations {
                result = Err(Box::new(sync::Learned));
                break;
            }
            let interrupted = result.as_ref().err().is_some_and(|error| match error
                .downcast_ref::<rocky_secure::Error>(
            ) {
                Some(rocky_secure::Error::Io(error)) => matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                ),
                Some(rocky_secure::Error::Rejected(reason)) => reason.starts_with("link ended"),
                Some(rocky_secure::Error::Application(cause)) => {
                    cause.is::<rocky_secure::network::link::AuthorityChanged>()
                }
                _ => false,
            });
            if result.is_ok() || !interrupted {
                break;
            }
        }
        let exchanged = result?;
        let met = self.met.lock().unwrap_or_else(PoisonError::into_inner);
        let mut handover = self.handover.lock().unwrap_or_else(PoisonError::into_inner);
        handover.insert(peer);
        if handover.len() > 16
            && let Some(oldest) = handover
                .iter()
                .min_by_key(|node| (met.get(node).copied().unwrap_or_default(), **node))
                .copied()
        {
            handover.remove(&oldest);
        }
        Ok(exchanged)
    }
    pub(super) async fn request_link(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
    ) -> Result<rocky_secure::network::link::channels::Handle, Failure> {
        let mut attempt = None;
        let result = self
            .request_link_observed(transport, peer, contact, &mut attempt)
            .await;
        if let Some(attempt) = attempt {
            let failure = result
                .as_ref()
                .err()
                .map(|error| crate::status::failure(error, &attempt.current_stage()));
            attempt.finish(failure);
        }
        result
    }
    async fn request_link_observed(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
        attempt: &mut Option<crate::status::Attempt>,
    ) -> Result<rocky_secure::network::link::channels::Handle, Failure> {
        if !self.foreground.load(Ordering::SeqCst) {
            return Err("live links are disabled in the background".into());
        }
        let addresses = contact.addresses(transport.kind());
        let service = self
            .current()
            .ok_or(ServiceError::NetworkRequired("sync"))?;
        if addresses.is_empty() && !service.live.contains(peer) {
            return Err(ServiceError::NoRoute {
                peer: "the peer",
                network: transport.kind().into(),
            }
            .into());
        }
        for _ in 0..4 {
            service.live.wait_inbound_short(peer).await?;
            if let Some(open) = service.live.handle(peer) {
                return Ok(open);
            }
            let _opening = self.opening(&service, peer);
            let current_attempt = attempt.insert(self.observations.begin(
                peer.to_hex(),
                crate::AttemptOperation::OpenLink,
                crate::AttemptStage::Dial,
            ));
            let stream = transport
                .session(&addresses, self.domain().scope)
                .await
                .map_err(|error| -> Failure {
                    if error.is::<rocky_core::net::ScopeRefusal>() || error.is::<ServiceError>() {
                        error
                    } else {
                        ServiceError::Unreachable(error.to_string()).into()
                    }
                })?;
            let owner = self
                .owner
                .get()
                .and_then(std::sync::Weak::upgrade)
                .ok_or("network owner ended")?;
            let link_settings = self.settings.lock().await.clone();
            current_attempt.stage(crate::AttemptStage::Admission);
            let admitted = Box::pin(owner.admit_link(
                service.clone(),
                stream,
                Some(peer),
                link_settings,
                None,
            ))
            .await;
            if let Err(error) = admitted {
                let changed = error.downcast_ref::<rocky_secure::Error>().is_some_and(|error| {
                    matches!(error, rocky_secure::Error::Application(cause) if cause.is::<rocky_secure::network::link::AuthorityChanged>())
                });
                if changed {
                    if let Some(attempt) = attempt.take() {
                        let failure = crate::status::failure(&error, &attempt.current_stage());
                        attempt.finish(Some(failure));
                    }
                    self.absorb_revocations(Some(&error)).await?;
                    continue;
                }
                return Err(error);
            }
            if let Some(open) = service.live.handle(peer) {
                return Ok(open);
            }
        }
        Err(Box::new(rocky_secure::Error::Application(Box::new(
            rocky_secure::network::link::AuthorityChanged,
        ))))
    }
    pub async fn serve(&self, request: rocky_core::net::Request) -> Result<(), Failure> {
        let mut enrolled = false;
        let result = async {
            let frame = crate::peer::RequestFrame::from_bytes(&request.bytes).map_err(refused)?;
            if !self.online.load(Ordering::SeqCst) || self.lifecycle.lock().await.is_logged_out() {
                return Err(refused("networking is disabled or this device logged out"));
            }
            let (_, class) = frame.request.requirements();
            let service = self.current();
            let _permit = service
                .as_ref()
                .map(|service| {
                    service.peer_admission[class as usize]
                        .try_acquire()
                        .map_err(|_| -> Failure {
                            ServiceError::Unreachable("peer RPC admission budget".into()).into()
                        })
                })
                .transpose()?;
            let pairing = matches!(frame.request, PeerRequest::Pair(_))
                && !self.settings.lock().await.has_enrollment_upstream();
            let reply = self.answer_peer(frame.request).await?;
            enrolled = pairing;
            Ok(reply)
        }
        .await
        .map_err(|error| crate::peer::failure(&error));
        request
            .respond(
                crate::peer::ReplyFrame {
                    version: crate::peer::VERSION,
                    result,
                }
                .to_bytes()?,
            )
            .await?;
        if enrolled {
            self.wake_sync();
        }
        Ok(())
    }

    async fn answer_peer(&self, request: PeerRequest) -> Result<PeerReply, Failure> {
        if !matches!(
            request,
            PeerRequest::Unlink(_) | PeerRequest::Policy | PeerRequest::Resolve(_)
        ) {
            self.revocation_notice
                .admit_personal(&self.settings.published())?;
        }
        let is_enrollment = matches!(
            request,
            PeerRequest::Pair(_)
                | PeerRequest::Renew(_)
                | PeerRequest::Epochs(_)
                | PeerRequest::Policy
        );
        if is_enrollment && let Some(reply) = self.forward_enrollment(&request).await? {
            return Ok(reply);
        }
        if matches!(
            request,
            PeerRequest::Renew(_) | PeerRequest::Epochs(_) | PeerRequest::Policy
        ) && !self.settings.lock().await.issuer()
        {
            return Err(refused("only an issuer renews devices or supplies policy"));
        }
        match request {
            PeerRequest::Pair(request) => Ok(PeerReply::Pair(
                Box::pin(self.answer_pair(&request.to_bytes()?)).await?,
            )),
            PeerRequest::Renew(request) => Ok(PeerReply::Renew(
                Box::pin(self.answer_renewal(&request.to_bytes()?)).await?,
            )),
            PeerRequest::Epochs(request) => {
                let settings = self.settings.lock().await.clone();
                if !settings.issuer() {
                    return Err(refused("only an issuer supplies epochs"));
                }
                let signed = &request.request;
                signed.chain.verify_database(settings.database_context()?)?;
                signed.verify(
                    PrivateKey::from_bytes(&self.vault.get("node")?).public_key(),
                    settings.publisher,
                    (now()?, 300),
                )?;
                let registry = self.registry(&*self.settings_snapshot().await).await?;
                let mut current = self.settings.lock().await;
                if let Some(statement) = current.revoking(&request.revocations)? {
                    if self
                        .revocation_notice
                        .record_unless_revoked(&current, statement.clone())?
                    {
                        self.record_ended(&statement, current.identity)?;
                    }
                    return Err(Box::new(crate::enrollment::Revoked(statement)));
                }
                let mut draft = current.clone();
                let added = draft.learn_revocations(&request.revocations)?;
                if !added.is_empty() {
                    current.commit(draft, &self.path)?;
                    self.apply(&mut current, Vec::new()).await?;
                    self.hold_keys(&current, &current.partitions(&self.domain())?)
                        .await?;
                }
                let reply =
                    current.answer_epochs(&self.vault, &request.request.to_bytes()?, &registry)?;
                drop(current);
                Ok(PeerReply::Epochs(reply))
            }
            PeerRequest::Policy => {
                let mut settings = self.settings.lock().await.clone();
                settings.learn_revocations(&self.replica.revocations().await)?;
                let mut known = settings.revocations.clone();
                if let Some(ended) = self.revocation_notice.revoked(&settings)? {
                    known.merge_in(
                        &RevocationSet::try_from(vec![ended])?,
                        settings.database_context()?,
                        now()?,
                    )?;
                }
                Ok(PeerReply::Policy(known))
            }
            PeerRequest::Identity(IdentityEnrollment::Shared(request)) => {
                let mut settings = self.settings.lock().await.clone();
                settings.bootstrap = Some(self.enrollment_bootstrap().await?);
                settings.learn_revocations(&self.replica.revocations().await)?;
                Ok(PeerReply::Identity(self.answer_link(&settings, request)?))
            }
            PeerRequest::Identity(IdentityEnrollment::PersonalHello(hello)) => {
                Ok(PeerReply::Identity(IdentityReply::PersonalChallenge(
                    Box::new(self.answer_personal_hello(hello).await?),
                )))
            }
            PeerRequest::Identity(IdentityEnrollment::Personal(authorization)) => {
                Ok(PeerReply::Identity(IdentityReply::Personal(Box::new(
                    self.answer_personal_bootstrap(*authorization).await?,
                ))))
            }
            PeerRequest::LinkEnroll(LinkEnrollment::Shared(certificate)) => {
                Ok(PeerReply::LinkEnroll(self.accept_link(*certificate).await?))
            }
            PeerRequest::LinkEnroll(LinkEnrollment::Personal(publication)) => Ok(
                PeerReply::LinkEnroll(self.accept_personal(*publication).await?),
            ),
            PeerRequest::LinkEnroll(LinkEnrollment::PersonalCustody(renewal)) => Ok(
                PeerReply::LinkEnroll(self.accept_custody_renewal(*renewal).await?),
            ),
            PeerRequest::Unlink(known) => {
                self.accept_unlink(known).await?;
                let settings = self.settings.lock().await;
                let database = settings.database_context()?;
                let mut committed = settings.revocations.clone();
                if let Some(ended) = self.revocation_notice.revoked(&settings)? {
                    committed.merge_in(&RevocationSet::try_from(vec![ended])?, database, now()?)?;
                }
                Ok(PeerReply::Unlink(Box::new(
                    rocky_credentials::UnlinkReply::sign(
                        committed,
                        database,
                        &Settings::node_key(&self.vault)?,
                    )?,
                )))
            }
            PeerRequest::Resolve(request) => Ok(PeerReply::Resolve(
                request.answer(&Settings::node_key(&self.vault)?)?,
            )),
            PeerRequest::TipSchema { manifest } => Ok(PeerReply::TipSchema(
                if self.replica.private_schema().await? {
                    None
                } else {
                    self.replica.tip_schema(manifest).await?
                },
            )),
            PeerRequest::StagedClose(close) => Ok(PeerReply::StagedClose(
                self.replica.relay_staged_close(&close.to_bytes()?).await?,
            )),
        }
    }

    async fn forward_enrollment(
        &self,
        request: &PeerRequest,
    ) -> Result<Option<PeerReply>, Failure> {
        let settings = self.settings.lock().await.clone();
        if !settings.has_enrollment_upstream() {
            return Ok(None);
        }
        if settings.issuer() || !matches!(settings.profile, Profile::Custody { .. }) {
            return Err(refused(
                "enrollment forwarding requires a custody-only node",
            ));
        }
        let attached = self.network.read().await;
        let link = attached
            .as_ref()
            .ok_or(ServiceError::NetworkRequired("enrollment forwarding"))?;
        if settings.enrollment_upstream.is_none()
            && let Some(node) = settings.resolve_saved_upstream(&*link.transport).await?
        {
            self.settings.lock().await.bind_saved_contact(node);
        }
        let settings = self.settings.lock().await.clone();
        let upstream = settings
            .enrollment_upstream
            .ok_or_else(|| refused("enrollment upstream could not be resolved"))?;
        let contact = settings
            .peers
            .get(&upstream)
            .ok_or_else(|| refused("enrollment upstream is not configured"))?;
        let addresses = contact.addresses(link.transport.kind());
        if addresses.is_empty() {
            return Err(ServiceError::NoRoute {
                peer: "enrollment upstream",
                network: link.transport.kind().into(),
            }
            .into());
        }
        Ok(Some(
            exchange(&*link.transport, &addresses, self.domain().scope, request).await?,
        ))
    }

    async fn sign_coordination_grant(
        &self,
        tenant: PublicKey,
        lifetime: u64,
        root: &Root,
    ) -> Result<rocky_credentials::SignedCoordinationGrant, Failure> {
        let publisher = self.settings.lock().await.publisher;
        let owner = crate::enrollment::root_signer(
            root.signer(),
            publisher,
            "signing a coordination grant",
        )?;
        if !(1..=366 * 86_400).contains(&lifetime) {
            return Err(refused("lifetime must be 1 s to 366 days"));
        }
        let time = now()?;
        let grant = rocky_credentials::SignedCoordinationGrant::sign(
            rocky_credentials::CoordinationGrant {
                publisher: rocky_credentials::Publisher(owner.public_key()?),
                application: self.domain().manifest().name,
                tenant: rocky_credentials::CoordinatorControl(tenant),
                not_before: time.saturating_sub(60),
                expires: time.saturating_add(lifetime),
            },
            owner,
        )?;
        if self.coordinator().is_some() {
            self.declare_coordinated(self.domain().scope, root).await?;
        }
        Ok(grant)
    }
}

#[derive(Default)]
pub(crate) struct Root(Option<crate::identities::Loan>);
impl Root {
    pub(crate) fn lent(loan: Option<crate::identities::Loan>) -> Self {
        Self(loan)
    }
    pub(crate) fn held(provider: Arc<dyn rocky_credentials::IdentityProvider>) -> Self {
        Self(Some(crate::identities::Loan::untracked(provider)))
    }
    pub(crate) fn signer(&self) -> Option<&dyn rocky_credentials::IdentityProvider> {
        self.0.as_ref().and_then(crate::identities::Loan::signer)
    }
}
fn revoked_by(error: &Failure) -> Option<rocky_credentials::SignedRevocation> {
    match error.downcast_ref::<Error>()? {
        Error::Application(error) => error
            .downcast_ref::<rocky_secure::network::Revoked>()
            .map(|revoked| (*revoked.0).clone()),
        _ => None,
    }
}
fn replica_refusal(error: &Failure) -> Option<&rocky_secure::Refusal> {
    match error.downcast_ref::<Error>()? {
        Error::Application(error) => error.downcast_ref(),
        _ => None,
    }
}
fn renewal_cannot_repair(error: &Failure) -> bool {
    match error.downcast_ref::<Error>() {
        Some(Error::Io(_)) => true,
        Some(Error::Rejected(reason)) => {
            reason.starts_with("link ")
                || reason.ends_with(" timeout")
                || reason.starts_with("checkpoint superseded")
        }
        _ => false,
    }
}
pub(crate) fn call_failure(error: &Failure) -> crate::CallFailure {
    if let Some(error) = error.downcast_ref::<crate::CallFailure>() {
        return error.clone();
    }
    if let Some(peer) = error.downcast_ref::<crate::peer::PeerError>() {
        return crate::CallFailure {
            code: peer.code.clone(),
            retryable: peer.retryable,
            detail: peer.message.clone(),
            coordinator_time: None,
            blockers: Vec::new(),
        };
    }
    let (code, retryable) = if error.is::<crate::enrollment::Revoked>() {
        ("revoked", false)
    } else if error.is::<crate::enrollment::PublisherRequired>() {
        ("publisher_required", false)
    } else if error.is::<crate::registry::RegistryFull>() {
        ("registry_full", false)
    } else if error.is::<crate::enrollment::RevocationSetFull>() {
        ("revocation_set_full", false)
    } else if error.is::<Refused>() {
        ("rejected", false)
    } else if let Some(reset) = error.downcast_ref::<crate::ResetRefused>() {
        (reset.code(), false)
    } else if let Some(refusal) = replica_refusal(error) {
        (
            refusal.code,
            matches!(
                refusal.code,
                "coordinator_unavailable"
                    | "behind"
                    | "recovery_pending"
                    | "chain_pending"
                    | "seal_pending"
                    | "entry_pending"
            ),
        )
    } else if let Some(coordination) = error.downcast_ref::<rocky_core::CoordinationError>() {
        coordination_code(coordination)
    } else if let Some(service) = error.downcast_ref::<ServiceError>() {
        (service.code(), service.retryable())
    } else if let Some(credentials) = error.downcast_ref::<rocky_credentials::Error>() {
        match credentials {
            rocky_credentials::Error::Provider(_) => ("storage", false),
            rocky_credentials::Error::Personal(personal) => (personal.code(), false),
            _ => ("denied", false),
        }
    } else {
        match error.downcast_ref::<Error>() {
            Some(Error::Rejected(_) | Error::Protocol(_)) => ("rejected", false),
            Some(Error::Blocked(_)) => ("blocked", true),
            Some(Error::Incompatible(_)) => ("unsupported_generation", false),
            Some(Error::Sql(SqlError::Denied)) => ("denied", false),
            Some(Error::Sql(
                SqlError::Capture(_) | SqlError::Statement { .. } | SqlError::Unsupported(_),
            )) => ("rejected", false),
            Some(Error::Sql(SqlError::Unavailable { reason, .. })) => match reason {
                Unavailable::Incomplete | Unavailable::KeyMissing => ("missing_dependencies", true),
                Unavailable::NotHeld => ("rejected", false),
                Unavailable::Revoked => ("authority_changed", true),
                Unavailable::Released => ("rejected", false),
            },
            Some(Error::Conflict) => ("conflict", true),
            Some(Error::Backpressure) => ("quota", true),
            Some(Error::Coordination(coordination)) => coordination_code(coordination),
            _ => ("storage", true),
        }
    };
    let clock = match error
        .downcast_ref::<rocky_core::CoordinationError>()
        .or_else(|| match error.downcast_ref::<Error>() {
            Some(Error::Coordination(error)) => Some(error),
            _ => None,
        }) {
        Some(rocky_core::CoordinationError::ClockSkew { coordinator_time }) => {
            Some(*coordinator_time)
        }
        _ => None,
    };
    let blockers = match error.downcast_ref::<Error>() {
        Some(Error::Blocked(blockers)) => blockers.clone(),
        _ => Vec::new(),
    };
    crate::CallFailure {
        code: code.into(),
        retryable,
        detail: error.to_string(),
        coordinator_time: clock,
        blockers,
    }
}
#[cfg(test)]
pub(crate) fn error(error: &Failure) -> serde_json::Value {
    let failure = call_failure(error);
    let mut value = serde_json::json!({"code": failure.code, "retryable": failure.retryable, "message": failure.detail});
    if let Some(time) = failure.coordinator_time {
        value["coordinator_time"] = serde_json::json!(time);
    }
    if !failure.blockers.is_empty() {
        value["blockers"] = serde_json::json!(failure.blockers);
    }
    value
}
fn coordination_code(error: &rocky_core::CoordinationError) -> (&'static str, bool) {
    use rocky_core::CoordinationError::{Backpressure, Denied, Sealed, Unavailable};
    match error {
        Unavailable => ("coordinator_unavailable", true),
        rocky_core::CoordinationError::ClockSkew { .. } => ("clock_skew", false),
        Backpressure => ("quota", true),
        Denied => ("denied", false),
        Sealed => ("sealed", false),
        _ => ("rejected", false),
    }
}

type RevocationCut = (
    Vec<rocky_credentials::Cut>,
    Vec<(PublicKey, rocky_credentials::Head)>,
);
#[cfg(test)]
mod error_tests {
    use super::*;

    fn code(failure: Failure) -> (String, bool) {
        let value = error(&failure);
        (
            value["code"].as_str().unwrap_or_default().to_owned(),
            value["retryable"].as_bool().unwrap_or_default(),
        )
    }
    fn refusal(code: &'static str) -> Failure {
        Box::new(Error::Application(Box::new(rocky_secure::Refusal {
            code,
            message: "refused".into(),
        })))
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn clock_skew_reports_authority_time_through_every_error_path() {
        let time = 1_900_000_000;
        let errors: [Failure; 2] = [
            Box::new(rocky_core::CoordinationError::ClockSkew {
                coordinator_time: time,
            }),
            Box::new(Error::Coordination(
                rocky_core::CoordinationError::ClockSkew {
                    coordinator_time: time,
                },
            )),
        ];
        for failure in errors {
            let reported = error(&failure);
            assert_eq!(reported["code"], "clock_skew");
            assert_eq!(reported["coordinator_time"], time);
            assert_eq!(reported["retryable"], false);
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn replica_refusals_keep_their_codes() {
        assert_eq!(code(refusal("sealed")), ("sealed".into(), false));
        assert_eq!(
            code(refusal("coordinator_unavailable")),
            ("coordinator_unavailable".into(), true)
        );
        let sealed: Failure = Box::new(rocky_core::CoordinationError::Sealed);
        assert_eq!(code(sealed), ("sealed".into(), false));
    }
}
#[cfg(all(test, not(target_family = "wasm")))]
mod delivery_tests;

#[cfg(all(test, not(target_family = "wasm")))]
mod revocation_tests;
#[cfg(all(test, not(target_family = "wasm")))]
mod sync_tests;
#[cfg(all(test, not(target_family = "wasm")))]
mod wide_schema_tests;
