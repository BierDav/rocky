mod repair;
mod requests;
use repair::RepairWork;
mod delivery;
mod discovery;
mod exchange;
use exchange::{Exchanged, PeerWork};
mod health;
#[cfg(any(test, feature = "test-support"))]
mod held;
mod invitations;
mod linking;
mod live_links;
mod membership;
mod mesh;
mod partition;
mod people;
mod recovery;
mod registry;
mod schema_publication;
mod schemas;
mod shared;
mod status;
mod sync_pass;
use sync_pass::PassOutcome;
mod tip_nodes;
#[cfg(any(test, feature = "test-support"))]
mod tip_probes;
mod tips;
use crate::{
    Failure,
    bootstrap_intent::{BootstrapIntent, BootstrapIntentFile, Finished, Imports},
    domain::Domain,
    enrollment::{Contact, Settings, exchange, now},
    peer::{PeerReply, PeerRequest},
    request::Lifecycle,
    secrets::Vault,
    service::{Network, ServiceError},
    sync::{self, Upstream},
    transport::{Protocol, Stream, Transport},
};
use data_encoding::HEXLOWER;
use futures_util::{StreamExt, stream};
#[cfg(test)]
use rocky_core::SqlValue;
use rocky_core::{ResourceScope, TypedRows, commitment};
use rocky_credentials::{IdentityEnrollment, IdentityReply};
use rocky_secure::{
    CredentialChain, EncryptedReplica, Error, KeyMaterial, PrivateKey, PublicKey, RevocationSet,
    SecureSql, SqlOptions, secure_node_id,
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
    if error.is::<crate::Error>()
        || error.is::<crate::registry::RegistryFull>()
        || error.is::<crate::JoinError>()
        || error.is::<crate::InviteError>()
        || error.is::<crate::AccessError>()
    {
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
    closing: watch::Sender<bool>,
    syncing: Mutex<()>,
    bootstrap: Mutex<()>,
    redeeming: Mutex<()>,
    #[cfg(test)]
    redemption_hook: std::sync::Mutex<Option<invitations::RedemptionHook>>,
    pub(crate) bootstrap_intent: BootstrapIntentFile,
    retry_seed: u64,
    retries: std::sync::Mutex<BTreeMap<PublicKey, PeerRetry>>,
    backoff_tip: std::sync::Mutex<Option<rocky_core::TipId>>,
    resolution_retries:
        std::sync::Mutex<BTreeMap<rocky_credentials::PeerId, crate::scheduling::Retry>>,
    unlink_retries: std::sync::Mutex<BTreeMap<rocky_credentials::PeerId, crate::scheduling::Retry>>,
    unlink_delivery: Mutex<()>,
    sync_control: watch::Sender<u64>,
    pub(crate) first_pull: watch::Sender<bool>,
    worker_running: watch::Sender<bool>,
    sync_state: watch::Sender<SyncActivityState>,
    automatic: AtomicBool,
    sync_events: watch::Sender<u64>,
    link_events: watch::Sender<()>,
    sync_status: watch::Sender<SyncStatus>,
    link_revision: AtomicU64,
    pub(crate) activity: crate::activity::Activity,
    reached: std::sync::Mutex<BTreeMap<PublicKey, Reached>>,
    met: std::sync::Mutex<BTreeMap<PublicKey, u64>>,
    link_requests: std::sync::Mutex<BTreeMap<PublicKey, crate::self_enrollment::HeldLink>>,
    clocks: std::sync::Mutex<BTreeMap<PublicKey, crate::Alert>>,
    mesh: std::sync::Mutex<BTreeMap<PublicKey, health::Held>>,
    admin_watch: std::sync::Mutex<health::AdminWatch>,
    admin_stale_after: u64,
    tip_chain: Arc<tokio::sync::Mutex<()>>,
    seal_skipped: std::sync::Mutex<Option<crate::Alert>>,
    mesh_revision: watch::Sender<u64>,
    report_failing: AtomicBool,
    #[cfg(test)]
    pub(crate) reports_received: AtomicU64,
    #[cfg(test)]
    pub(crate) verified_under_settings: AtomicU64,
    learned: AtomicBool,
    rights_notice: std::sync::Mutex<Option<PublicKey>>,
    granted_devices: std::sync::Mutex<std::collections::BTreeSet<PublicKey>>,
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
    choices: std::sync::Mutex<BTreeMap<PublicKey, crate::PeerChoice>>,
    topics: watch::Sender<u64>,
    announcing: AtomicBool,
    #[cfg(test)]
    pub(crate) discovery_skew: AtomicU64,
    observations: crate::status::Observations,
    node_identity: Result<PublicKey, String>,
    #[cfg(test)]
    status_clock: std::sync::Mutex<Option<(u64, rocky_platform::time::Instant)>>,
    last_reconciliation: std::sync::Mutex<Option<crate::SyncReport>>,
    last_syncs: std::sync::Mutex<BTreeMap<PublicKey, crate::PeerReconciliation>>,
    mailbox: Mutex<Option<crate::delivery::Mailbox>>,
    declared: crate::schema::Schema,
    widened: std::sync::Mutex<std::collections::BTreeSet<String>>,
    signer: Option<Arc<dyn rocky_credentials::IdentityProvider>>,
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
        bootstrap_intent.attach_replica(replica.clone());
        let revocation_notice = crate::enrollment::RevocationNotice::open(replica.clone());
        let observations = crate::status::Observations::default();
        Self {
            #[cfg(test)]
            status_clock: Default::default(),
            node_identity: vault
                .get("node")
                .map(|key| PrivateKey::from_bytes(&key).public_key())
                .map_err(|error| error.to_string()),
            last_reconciliation: Default::default(),
            last_syncs: Default::default(),
            mesh: Default::default(),
            link_requests: Default::default(),
            mesh_revision: watch::channel(0).0,
            admin_watch: Default::default(),
            admin_stale_after: crate::status::ADMIN_STALE_AFTER.as_secs(),
            seal_skipped: Default::default(),
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
            closing: watch::channel(false).0,
            syncing: Mutex::new(()),
            bootstrap: Mutex::new(()),
            redeeming: Mutex::new(()),
            #[cfg(test)]
            redemption_hook: std::sync::Mutex::new(None),
            bootstrap_intent,
            retry_seed: rocky_platform::entropy::random_u64(),
            retries: std::sync::Mutex::new(BTreeMap::new()),
            backoff_tip: std::sync::Mutex::new(None),
            resolution_retries: std::sync::Mutex::new(BTreeMap::new()),
            unlink_retries: std::sync::Mutex::new(BTreeMap::new()),
            unlink_delivery: Mutex::new(()),
            sync_control: watch::channel(0).0,
            first_pull: watch::channel(false).0,
            worker_running: watch::channel(false).0,
            sync_state: watch::channel(SyncActivityState::Idle).0,
            automatic: AtomicBool::new(false),
            sync_events: watch::channel(0).0,
            link_events: watch::channel(()).0,
            link_revision: AtomicU64::new(0),
            sync_status: watch::channel(SyncStatus::new(SyncState::Idle)).0,
            activity: crate::activity::Activity::new(),
            reached: std::sync::Mutex::new(BTreeMap::new()),
            met: std::sync::Mutex::new(BTreeMap::new()),
            clocks: std::sync::Mutex::new(BTreeMap::new()),
            learned: AtomicBool::new(false),
            rights_notice: std::sync::Mutex::new(None),
            granted_devices: std::sync::Mutex::new(Default::default()),
            handover: std::sync::Mutex::new(Default::default()),
            import_sources: std::sync::Mutex::new(Default::default()),
            sync_failures: Default::default(),
            choices: std::sync::Mutex::new(BTreeMap::new()),
            topics: watch::channel(0).0,
            announcing: AtomicBool::new(false),
            #[cfg(test)]
            discovery_skew: AtomicU64::new(0),
            mailbox: Mutex::new(None),
            declared: settings_schema,
            signer: None,
            schema_fetch: Mutex::new(()),
            schema_acquisition: Default::default(),
            widened: Default::default(),
        }
    }
    pub fn with_declared(mut self, schema: crate::schema::Schema) -> Self {
        self.declared = schema;
        self
    }
    pub(crate) fn with_admin_stale_after(mut self, after: std::time::Duration) -> Self {
        self.admin_stale_after = after.as_secs();
        self
    }
    pub(crate) fn with_signer(
        mut self,
        signer: Option<Arc<dyn rocky_credentials::IdentityProvider>>,
    ) -> Self {
        self.signer = signer;
        self
    }
    pub(crate) fn signed<F: std::future::Future>(
        &self,
        call: F,
    ) -> impl std::future::Future<Output = F::Output> + use<F> {
        crate::identities::supplying(self.signer.clone(), call)
    }
    pub async fn application(&self, settings: &Settings) -> Result<crate::DeclaredSchema, Failure> {
        if self.replica.followed_bookmark().await.is_none() {
            return Ok(crate::DeclaredSchema::Ahead);
        }
        settings.application(&self.declared)
    }
    pub(crate) async fn upgrade_required(&self) -> Result<Option<(u32, u32)>, Failure> {
        if self.replica.followed_bookmark().await.is_none() {
            return Ok(None);
        }
        let settings = self.settings.lock().await.clone();
        settings.upgrade_required(&self.declared)
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

    pub fn end_waits(&self) {
        self.closing.send_replace(true);
    }
    pub(crate) async fn admit_sql(&self) -> Result<(), Failure> {
        if self.lifecycle.lock().await.is_logged_out() {
            return Err(refused("this device logged out of the domain"));
        }
        if self.own_revocation().await?.is_some() {
            return Err(Box::new(crate::Error::removed()));
        }
        Ok(())
    }
    pub(crate) async fn sql_session(
        &self,
        options: SqlOptions,
    ) -> Result<SecureSql<TypedRows>, Failure> {
        self.admit_sql().await?;
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
    ) -> Result<(), Failure> {
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
        let accepts = !link.transport.dial_only();
        if accepts {
            link.transport.listen(Some(self.domain().scope), true)?;
        }
        let publisher = crate::reactor::spawn_local(
            "publish_links",
            self.signed(self.clone().publish_links(link.clone())),
        );
        *link
            .publisher
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(publisher);
        let synchronizer = crate::reactor::spawn_local(
            "synchronize",
            self.signed(self.clone().synchronize(link.clone())),
        );
        *link
            .synchronizer
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(synchronizer);
        if accepts {
            let server = crate::reactor::spawn_local(
                "accept",
                self.signed(self.clone().accept(link.clone())),
            );
            *link.server.lock().unwrap_or_else(PoisonError::into_inner) = Some(server);
        }
        if let Some(discovery) = link.discovery.clone() {
            let discoverer = crate::reactor::spawn_local(
                "discover",
                self.signed(self.clone().discover(link.clone(), discovery)),
            );
            *link
                .discoverer
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(discoverer);
        }
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(link.clone());
        *slot = Some(link);
        self.wake_sync();
        Ok(())
    }
    pub async fn link(
        self: &Arc<Self>,
        transport: impl Into<crate::transport::Connected>,
        owned: bool,
    ) -> Result<(), Failure> {
        self.install(&mut *self.network.write().await, transport.into(), owned)
    }
    pub async fn attach(self: &Arc<Self>, network: Network) -> Result<(), Failure> {
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
    pub async fn detach(&self) {
        let _detaching = self.detaching.lock().await;
        let attached = self.network.read().await.clone();
        let Some(link) = attached else {
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
                        node.activity.record_changed(
                            crate::activity::ActivityKind::LinkAdmissionFailed {
                                error: (error.to_string()).to_owned(),
                            },
                        );
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

    pub(super) async fn own_routes(&self) -> Vec<crate::Route> {
        let attached = self.network.read().await;
        match attached.as_deref() {
            Some(link) => match link.transport.address().await {
                Ok(Some(address)) => crate::Route::try_from(address).into_iter().collect(),
                _ => Vec::new(),
            },
            None => Vec::new(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
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
            Ok(crate::ContactInfo { uri, node, routes })
        };
        Self::networked(attached.as_deref(), work).await
    }

    async fn add_peer(&self, contact: &str) -> Result<PublicKey, Failure> {
        let registry = match self.registry(&*self.settings_snapshot().await).await {
            Ok(registry) => Some(registry),
            Err(error) if waits_on_blockers(error.as_ref()) => None,
            Err(error) => return Err(error),
        };
        let node = self
            .configure(|settings| {
                let node = settings.add_peer(&self.vault, contact)?;
                if registry
                    .as_ref()
                    .is_some_and(|registry| registry.revoked(&node))
                {
                    return Err("the contact names a revoked device".into());
                }
                Ok(node)
            })
            .await?;
        self.reset_retry(Some(node));
        self.activity
            .record(crate::activity::ActivityKind::PeerAdded {
                peer: (node).to_owned(),
            });
        self.wake_sync();
        Ok(node)
    }

    async fn remove_peer(&self, node: PublicKey) -> Result<(), Failure> {
        self.configure(|settings| {
            settings.remove_peer(node);
            Ok(())
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
        self.activity
            .record(crate::activity::ActivityKind::PeerRemoved {
                peer: (node).to_owned(),
            });
        self.wake_sync();
        Ok(())
    }

    pub(crate) async fn admit_request(
        &self,
        request: &crate::request::Request,
    ) -> Result<(), Failure> {
        if self.lifecycle.lock().await.is_logged_out() && !request.allowed_after_logout() {
            return Err(refused("this device logged out of the domain"));
        }
        Ok(())
    }
    async fn own_revocation(&self) -> Result<Option<rocky_credentials::SignedRevocation>, Failure> {
        let settings = self.settings_snapshot().await;
        self.revocation_notice.revoked(&settings).await
    }
    pub(crate) async fn purge_if_removed(&self) {
        if matches!(self.own_revocation().await, Ok(Some(_))) {
            self.replica.logout().await;
            self.observations.touch();
        }
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
            settings.commit(draft, &self.replica).await?;
        }
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? =
            settings.granted(&self.domain());
        self.observations.touch();
        Ok(result)
    }
    fn current(&self) -> Option<Arc<Link>> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    #[cfg(any(test, feature = "test-support"))]
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
    async fn bootstrap_progress(&self) -> Result<Option<crate::BootstrapProgress>, Failure> {
        let Some(intent) = self.bootstrap_intent.load().await? else {
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
    async fn readiness_after(&self, after: &str) -> Result<crate::DatabaseStatus, Failure> {
        let mut revisions = self.replica.observe_revision();
        let mut imports = self.replica.observe_imports().await;
        let mut authority = self.replica.observe_authority().await;
        let mut sync_state = self.sync_state.subscribe();
        let mut closing = self.closing.subscribe();
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
                    return Err(ServiceError::Closed("database").into());
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
    #[cfg(test)]
    async fn revoke(&self, target: crate::enrollment::Revoke) -> Result<(), Failure> {
        self.revoke_with(target, &Root::default()).await
    }
    async fn record_ended(
        &self,
        signed: &rocky_credentials::SignedRevocation,
        identity: PublicKey,
    ) -> Result<(), Failure> {
        self.replica.logout().await;
        self.observations.touch();
        let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let statement = &signed.statement;
        if !statement.identities.contains(&identity)
            && statement
                .nodes
                .contains(&rocky_credentials::PeerId::of(node))
        {
            self.activity
                .record(crate::activity::ActivityKind::NodeUnlinked {
                    identity: (identity).to_owned(),
                    node: (node).to_owned(),
                });
        } else {
            self.activity
                .record(crate::activity::ActivityKind::IdentityRevoked {
                    identity: (identity).to_owned(),
                });
        }
        Ok(())
    }
    async fn revocation_cut(
        &self,
        target: &crate::enrollment::Revoke,
        membership: &crate::membership::Membership,
    ) -> Result<RevocationCut, Failure> {
        let graph = self.replica.tip_graph(self.domain().scope).await?;
        let (identity, lanes): (PublicKey, std::collections::BTreeSet<PublicKey>) = match *target {
            crate::enrollment::Revoke::Identity(identity)
            | crate::enrollment::Revoke::Admin(identity, _) => (
                identity,
                membership
                    .nodes(&identity)
                    .map(|linked| linked.node)
                    .collect(),
            ),
            crate::enrollment::Revoke::Node { owner, node } => (owner, [node].into()),
            crate::enrollment::Revoke::Rights { .. } => return Ok(Default::default()),
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
    ) -> Result<(), Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let membership = self.membership().await?;
        let (cut, dropped) = if root.signer().is_some() {
            self.revocation_cut(&target, &membership).await?
        } else {
            Default::default()
        };
        if !dropped.is_empty() {
            self.activity
                .record(crate::activity::ActivityKind::RevocationCutDroppedStages {
                    identity: None,
                    count: dropped.len(),
                });
        }
        self.rotate_revoked(|draft| {
            draft.begin_revocation_cut(target, (&registry, &membership), root.signer(), cut)?;
            Ok(true)
        })
        .await
        .map(drop)
    }
    pub(super) async fn rotate_revoked(
        &self,
        begin: impl FnOnce(&mut Settings) -> Result<bool, Failure>,
    ) -> Result<bool, Failure> {
        let mut settings = self.settings.lock().await;
        let mut draft = settings.clone();
        let begun = begin(&mut draft).map_err(|error| -> Failure {
            if error.is::<crate::enrollment::RevocationSetFull>()
                || error.is::<crate::enrollment::PublisherRequired>()
            {
                error
            } else {
                refused(error)
            }
        })?;
        if !begun {
            if !draft.persisted_eq(&settings) {
                settings.commit(draft, &self.replica).await?;
            }
            return Ok(false);
        }
        settings.commit(draft.clone(), &self.replica).await?;
        self.check_fault(crate::database::fault::Stage::RevocationBegun)?;
        let rotation = draft
            .authority
            .as_ref()
            .and_then(|authority| authority.rotation.clone())
            .ok_or("the revocation was not begun")?;
        let keys = draft.rotate(&self.vault)?.unwrap_or_default();
        draft.drop_revoked_deliveries();
        settings.commit(draft, &self.replica).await?;
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
        drop(settings);
        Box::pin(self.complete_revocation()).await?;
        if let Err(error) = self.seal_for_members().await {
            self.activity
                .record(crate::activity::ActivityKind::RevocationSealFailed {
                    error: (error.to_string()).to_owned(),
                });
        }
        Ok(true)
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
        if let Err(error) = self.refresh_line_node(settings).await {
            self.activity
                .record_changed(crate::activity::ActivityKind::LineNodeFailed {
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
        Ok(())
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
                        .record_unless_revoked(&current, revoked.0.clone())
                        .await?
                    {
                        self.record_ended(&revoked.0, current.identity).await?;
                    }
                    self.wake_sync();
                }
                return Err(error);
            }
        };
        if let Some(peer) = renewed.contacted {
            self.inbound_sync(peer);
        }
        let admin_grant = renewed.admin_grant.take();
        self.learned.store(false, Ordering::SeqCst);
        let mut settings = self.settings.lock().await;
        if !epochs_only && settings.issuer() && !current.issuer() {
            return Ok(false);
        }
        let restores_revoked_authors =
            (!epochs_only || !settings.issuer()) && settings.lacks(&renewed.revocations)?;
        if restores_revoked_authors {
            self.bootstrap_intent.request_checkpoint_import().await?;
        }
        if epochs_only {
            renewed.chain = settings.credentials.clone();
        }
        let (keys, adopted) = settings.adopt(&self.replica, &self.vault, &renewed).await?;
        let promoted = match admin_grant {
            Some(link) if settings.admin_links().is_none() => {
                let mut draft = (**settings).clone();
                draft
                    .install_admin_grant(&self.vault, link)
                    .map_err(refused)?;
                settings.commit(Shared::new(draft), &self.replica).await?;
                true
            }
            _ => false,
        };
        let mut changed = !epochs_only || adopted || promoted;
        if changed {
            let readable = self
                .granted
                .lock()
                .map_err(|_| "scope catalog poisoned")?
                .len();
            self.apply(&mut settings, keys).await?;
            self.hold_keys(&settings, &settings.partitions(&self.domain())?)
                .await?;
            if let Some(partial) =
                crate::database::align_role(&self.replica, &settings, &self.domain()).await?
            {
                self.activity
                    .record(crate::activity::ActivityKind::RoleChanged { partial });
            }
            let grants_widened = settings.granted(&self.domain()).len() > readable;
            if grants_widened {
                let domain = self.domain();
                let mut widened = (**settings).clone();
                if crate::database::attach(&self.replica, &mut widened, &self.vault, &domain)
                    .await?
                {
                    settings.commit(Shared::new(widened), &self.replica).await?;
                }
            }
        }
        if settings.issuer() && !promoted {
            drop(settings);
            let registry = self.registry(&*self.settings_snapshot().await).await?;
            settings = self.settings.lock().await;
            if settings.begin_exposure_rotation(&registry)? {
                changed = true;
                settings.save(&self.replica).await?;
                let keys = settings
                    .rotate(&self.vault)?
                    .ok_or("exposure rotation intent disappeared")?;
                settings.save(&self.replica).await?;
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
        if self.settings.lock().await.issuer() && !promoted {
            Box::pin(self.complete_revocation()).await?;
        }
        if !epochs_only
            && !promoted
            && let Err(error) = Box::pin(self.record_self()).await
        {
            self.activity
                .record(crate::activity::ActivityKind::RegistryFailed {
                    error: (error.to_string()).to_owned(),
                });
        }
        if promoted {
            let _ = Box::pin(self.renew(None, true, None, true)).await;
            self.observations.touch();
        }
        self.wake_sync();
        Ok(true)
    }
    async fn logout(&self, logout: crate::Logout) -> Result<(), Failure> {
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
        settings.save(&self.replica).await?;
        *self.lifecycle.lock().await = Lifecycle::LoggedOut {
            purge_on_close: logout == crate::Logout::PurgeOnClose,
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
                        .record(crate::activity::ActivityKind::PeerAttemptFailed {
                            peer: (peer).to_owned(),
                            error: (error).to_owned(),
                        });
                }
            }
            Ok(Exchanged { transfer, .. }) => {
                if !matches!(before, Some(None)) {
                    self.activity.record(
                        crate::activity::ActivityKind::PeerReconciliationCompleted {
                            peer: (peer).to_owned(),
                        },
                    );
                }
                let refusals_changed = transfer.refused != refused_before;
                if refusals_changed {
                    let scopes = transfer
                        .refused
                        .iter()
                        .map(|scope| self.scope_label(scope))
                        .collect();
                    self.activity
                        .record(crate::activity::ActivityKind::ScopesRefused {
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
                    self.activity
                        .record(crate::activity::ActivityKind::ScopesWaiting {
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
                    .map(|(scope, moved)| crate::activity::ActivityTransfer {
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
                    self.activity.record(crate::activity::ActivityKind::Synced {
                        peer: (peer).to_owned(),
                        pulled: (pulled) as u64,
                        pushed: (pushed) as u64,
                        scopes,
                    });
                }
                for (scope, moved) in &transfer.scopes {
                    if let Some((checkpoint, superseded)) = moved.checkpoint {
                        self.activity
                            .record(crate::activity::ActivityKind::CheckpointImported {
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
    fn scope_label(&self, scope: &ResourceScope) -> crate::activity::ActivityScope {
        let domain = self.domain();
        let own = domain.scopes().find(|(_, own)| own.table == scope.table);
        crate::activity::ActivityScope {
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
        let identity = match &self.node_identity {
            Ok(node) => format!("{}:{peer}", node.to_hex()),
            Err(_) => format!("unavailable:{peer}"),
        };
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
            if self.sync_mode() == crate::SyncMode::Live {
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
                }, if self.sync_mode() == crate::SyncMode::Live => { retry_only = true; }
            }
        }
    }

    async fn sync_optional(
        &self,
        transport: Option<&dyn Transport>,
    ) -> Result<(crate::SyncReport, bool), Failure> {
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
                crate::PeerChoice::Backoff
            } else if index >= crate::enrollment::MAX_DIALED {
                crate::PeerChoice::PassLimit
            } else if meta.is_some_and(|meta| meta.local) {
                crate::PeerChoice::SelectedLocal
            } else if reached.get(node).is_some_and(|last| {
                last.error.is_none() && needed.iter().any(|scope| last.held.contains(scope))
            }) {
                crate::PeerChoice::SelectedHolding
            } else if settings.issuers.contains(node) || meta.is_some_and(|meta| meta.trusted) {
                crate::PeerChoice::SelectedAdmin
            } else {
                crate::PeerChoice::SelectedProbe
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
                self.bootstrap_intent.request_checkpoint_import().await?;
            }
        }
        if let Some(statement) = revoking
            && self
                .revocation_notice
                .record_unless_revoked(&draft, statement.clone())
                .await?
        {
            self.record_ended(&statement, draft.identity).await?;
        }
        let rotated = match registry {
            Some(registry) => resumes || draft.begin_exposure_rotation(&registry)?,
            None => false,
        };
        settings.commit(draft, &self.replica).await?;
        if rotated {
            let mut rotation = (*settings).clone();
            let keys = rotation
                .rotate(&self.vault)?
                .ok_or("missing exposure rotation")?;
            settings.commit(rotation, &self.replica).await?;
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
        match self
            .revocation_notice
            .revoked(&self.settings.published())
            .await?
        {
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
    async fn awaited_inviter(&self) -> Result<Option<PublicKey>, Failure> {
        Ok(self
            .bootstrap_intent
            .load()
            .await?
            .and_then(|intent| intent.inviter()))
    }
    pub(crate) async fn refresh_importing(&self) -> Result<(), Failure> {
        let pending = match self.bootstrap_intent.load().await? {
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
                ClearsBy::AnyPeer | ClearsBy::Admin => true,
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
        let addresses = contact.addresses(transport);
        let service = self
            .current()
            .ok_or(ServiceError::NetworkRequired("sync"))?;
        if addresses.is_empty() && !service.live.contains(peer) {
            return Err(ServiceError::no_route("the peer", transport).into());
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
            let reply = self.answer_peer(frame.request).await?;
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
        Ok(())
    }

    async fn answer_peer(&self, request: PeerRequest) -> Result<PeerReply, Failure> {
        if matches!(
            request,
            PeerRequest::Renew(_) | PeerRequest::Epochs(_) | PeerRequest::Policy
        ) && !self.settings.lock().await.issuer()
        {
            return Err(refused("only an issuer renews devices or supplies policy"));
        }
        match request {
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
                let mut current = self.settings.lock().await;
                if let Some(statement) = current.revoking(&request.revocations)? {
                    if self
                        .revocation_notice
                        .record_unless_revoked(&current, statement.clone())
                        .await?
                    {
                        self.record_ended(&statement, current.identity).await?;
                    }
                    return Err(Box::new(crate::enrollment::Revoked(statement)));
                }
                let mut draft = current.clone();
                let added = draft.learn_revocations(&request.revocations)?;
                if !added.is_empty() {
                    current.commit(draft, &self.replica).await?;
                    self.apply(&mut current, Vec::new()).await?;
                    self.hold_keys(&current, &current.partitions(&self.domain())?)
                        .await?;
                }
                let reply = current.answer_epochs(&self.vault, &request.request.to_bytes()?)?;
                drop(current);
                Ok(PeerReply::Epochs(reply))
            }
            PeerRequest::Policy => {
                let mut settings = self.settings.lock().await.clone();
                settings.learn_revocations(&self.replica.revocations().await)?;
                let mut known = settings.revocations.clone();
                if let Some(ended) = self.revocation_notice.revoked(&settings).await? {
                    known.merge_in(
                        &RevocationSet::try_from(vec![ended])?,
                        settings.database_context()?,
                        now()?,
                    )?;
                }
                Ok(PeerReply::Policy(known))
            }
            PeerRequest::Identity(enrollment) => match *enrollment {
                IdentityEnrollment::Shared(request) => {
                    let mut settings = self.settings.lock().await.clone();
                    settings.bootstrap = Some(self.enrollment_bootstrap().await?);
                    settings.learn_revocations(&self.replica.revocations().await)?;
                    Ok(PeerReply::Identity(self.answer_link(&settings, request)?))
                }
                IdentityEnrollment::Invited(request) => Ok(PeerReply::Identity(
                    Box::pin(self.answer_invitation(request)).await?,
                )),
            },
            PeerRequest::LinkEnroll(certificate) => {
                Ok(PeerReply::LinkEnroll(self.accept_link(*certificate).await?))
            }
            PeerRequest::Unlink(known) => {
                self.accept_unlink(*known).await?;
                let settings = self.settings.lock().await;
                let database = settings.database_context()?;
                let mut committed = settings.revocations.clone();
                if let Some(ended) = self.revocation_notice.revoked(&settings).await? {
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
        }
    }
}

#[derive(Default)]
pub(crate) struct Root(Option<crate::identities::Loan>);
impl Root {
    pub(crate) fn lent(loan: Option<crate::identities::Loan>) -> Self {
        Self(loan)
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
fn renewal_cannot_repair(error: &Failure) -> bool {
    match error.downcast_ref::<Error>() {
        Some(Error::Io(_)) => true,
        Some(Error::Application(cause))
            if cause.is::<rocky_secure::network::CheckpointSuperseded>() =>
        {
            true
        }
        Some(Error::Rejected(reason)) => {
            reason.starts_with("link ") || reason.ends_with(" timeout")
        }
        _ => false,
    }
}
pub(crate) fn typed_error(error: &Failure) -> crate::Error {
    use crate::{DenyReason, Right};
    if let Some(peer) = error.downcast_ref::<crate::peer::PeerError>() {
        return crate::peer::remote(peer);
    }
    if error.is::<crate::enrollment::Revoked>() || revoked_by(error).is_some() {
        return crate::Error::removed();
    }
    if error.is::<crate::enrollment::PublisherRequired>() {
        return crate::Error::refused(Right::Administer, DenyReason::OwnerRequired);
    }
    typed_cause(error.as_ref(), error.to_string())
}
fn typed_cause(
    error: &(dyn std::error::Error + Send + Sync + 'static),
    detail: String,
) -> crate::Error {
    use crate::ErrorKind as Kind;
    if let Some(error) = error.downcast_ref::<crate::Error>() {
        return error.clone();
    }
    let kind = if error.is::<crate::registry::RegistryFull>() {
        Kind::RegistryFull
    } else if let Some(error) = error.downcast_ref::<crate::InviteError>() {
        Kind::Invite(*error)
    } else if let Some(error) = error.downcast_ref::<crate::JoinError>() {
        Kind::Join(*error)
    } else if let Some(error) = error.downcast_ref::<crate::LinkError>() {
        Kind::Link(*error)
    } else if let Some(error) = error.downcast_ref::<crate::AccessError>() {
        Kind::Access(error.clone())
    } else if error.is::<crate::enrollment::RevocationSetFull>() {
        Kind::RevocationSetFull
    } else if error.is::<Refused>() {
        Kind::Rejected
    } else if let Some(service) = error.downcast_ref::<ServiceError>() {
        Kind::Service(service.clone())
    } else if let Some(schema) = error.downcast_ref::<crate::SchemaError>() {
        Kind::Schema(Box::new(schema.clone()))
    } else if let Some(decode) = error.downcast_ref::<crate::DecodeError>() {
        Kind::Decode(decode.clone())
    } else if let Some(refusal) = error.downcast_ref::<crate::StoreOnlyRefusal>() {
        Kind::StoreOnly(*refusal)
    } else if let Some(required) = error.downcast_ref::<crate::RolloutConfirmationRequired>() {
        Kind::RolloutConfirmation(Box::new(required.clone()))
    } else if let Some(in_use) = error.downcast_ref::<crate::RoleInUse>() {
        Kind::RoleInUse(in_use.clone())
    } else if let Some(credentials) = error.downcast_ref::<rocky_credentials::Error>() {
        match credentials {
            rocky_credentials::Error::Provider(_) => Kind::Storage,
            _ => Kind::Rejected,
        }
    } else if error.is::<std::io::Error>() {
        Kind::Storage
    } else if let Some(error) = error.downcast_ref::<Error>() {
        return storage_error(error, detail);
    } else {
        Kind::Internal
    };
    crate::Error::new(kind, detail)
}
pub(crate) fn waits_on_blockers(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(error.downcast_ref::<Error>(), Some(Error::Blocked(_)))
        || error
            .downcast_ref::<crate::Error>()
            .is_some_and(|error| matches!(error.kind(), crate::ErrorKind::Blocked { .. }))
}
fn storage_error(error: &Error, detail: String) -> crate::Error {
    use crate::ErrorKind as Kind;
    let kind = match error {
        Error::Application(cause) if cause.is::<rocky_secure::MigrationPending>() => {
            Kind::MigrationPending
        }
        Error::Application(cause) if cause.is::<rocky_secure::StoreSuperseded>() => {
            Kind::StoreSuperseded
        }
        Error::Application(cause) => return typed_cause(cause.as_ref(), detail),
        Error::Rejected(_) | Error::Protocol(_) => Kind::Rejected,
        Error::Blocked(blockers) => match crate::Blockers::new(blockers.clone()) {
            Some(blockers) => Kind::Blocked {
                table: None,
                blockers,
            },
            None => Kind::Internal,
        },
        Error::Incompatible(_) => Kind::UnsupportedGeneration,
        Error::Conflict => Kind::Conflict,
        Error::Backpressure | Error::RetryExhausted => Kind::Busy,
        Error::Storage(_) if error.is_retryable() => Kind::Busy,
        Error::Quota { .. } => Kind::Quota,
        Error::TooLarge { limit, size, max } => Kind::TooLarge {
            limit: match limit {
                rocky_storage::Limit::Intent => crate::Limit::Intent,
                rocky_storage::Limit::Frame => crate::Limit::Frame,
            },
            size: *size,
            max: *max,
        },
        Error::Closed => Kind::Closed,
        Error::Sql(sql) => return crate::connection::sql_error(sql, detail),
        Error::Io(_)
        | Error::Storage(_)
        | Error::Corrupt(_)
        | Error::Durability(_)
        | Error::Rollback { .. } => Kind::Storage,
        Error::InjectedFailure => Kind::Internal,
    };
    crate::Error::new(kind, detail)
}
#[cfg(test)]
pub(crate) fn error(error: &Failure) -> serde_json::Value {
    let failure = typed_error(error);
    let mut value = serde_json::json!({"code": failure.code(), "retryable": failure.retryable(), "message": failure.to_string()});
    if !failure.blockers().is_empty() {
        value["blockers"] = serde_json::json!(failure.blockers());
    }
    value
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
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_pending_migration_refusal_is_typed() {
        let pending: Failure =
            Box::new(Error::Application(Box::new(rocky_secure::MigrationPending)));
        let typed = crate::Error::from_failure(&pending);
        assert_eq!(typed.kind(), &crate::ErrorKind::MigrationPending);
        assert_eq!(code(pending), ("migration_pending".into(), false));
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_superseded_store_refusal_is_typed_and_final() {
        let superseded: Failure = Box::new(Error::Application(Box::new(
            rocky_secure::StoreSuperseded {
                node: rocky_secure::PrivateKey::new().public_key(),
                scope: rocky_core::ResourceScope {
                    domain: rocky_core::Scope {
                        mesh: rocky_core::MeshId([1; 16]),
                        app: rocky_core::AppId([2; 16]),
                        domain: rocky_core::DomainId([3; 16]),
                    },
                    encryption: [4; 32],
                    table: [5; 32],
                },
                held: 2,
                reported: 4,
            },
        )));
        let typed = crate::Error::from_failure(&superseded);
        assert_eq!(typed.kind(), &crate::ErrorKind::StoreSuperseded);
        assert!(typed.to_string().contains("re-invite it as a new device"));
        assert_eq!(code(superseded), ("store_superseded".into(), false));
    }
}
#[cfg(all(test, not(target_family = "wasm")))]
mod delivery_tests;

#[cfg(all(test, not(target_family = "wasm")))]
mod revocation_tests;
#[cfg(all(test, not(target_family = "wasm")))]
mod rights_change_tests;
#[cfg(all(test, not(target_family = "wasm")))]
mod sync_tests;
#[cfg(all(test, not(target_family = "wasm")))]
mod wide_schema_tests;
