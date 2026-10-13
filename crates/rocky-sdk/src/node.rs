//! One open secure database: its replica, identity, sessions and optional
//! network service, and the JSON calls of the native ABI. See
//! `docs/runtime/sdk-composition.md` and `docs/runtime/sdk-enrollment.md`.
mod command_mesh;
mod delivery;
mod discovery;
mod horizon;
mod live_links;
mod mesh;
mod partition;
mod registry;
mod shared;
use crate::{
    Failure,
    domain::Domain,
    enrollment::{Contact, Settings, Terms, answer, exchange, now},
    secrets::Vault,
    service::{Network, ServiceError},
    sync::{self, Upstream},
    transport::{Protocol, Stream, Transport},
};
use data_encoding::HEXLOWER;
use futures_util::{StreamExt, stream};
use rocky_core::{Coordinator, ResourceScope, SqlValue, TypedCommands, commitment};
use rocky_credentials::Profile;
use rocky_secure::{
    CredentialChain, Delivery, EncryptedReplica, Error, KeyMaterial, PrivateKey, PublicKey,
    RevocationSet, SecureSession, SecureSql, SqlError, SqlOptions, TicketState, secure_node_id,
    secure_principal_id,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
pub(crate) use shared::Shared;
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

const MAX_ROWS: usize = 1024;
const MAX_SQL: usize = 64 * 1024;
const MAX_SUBSCRIPTIONS: usize = 64;
const MAX_SESSIONS: usize = 64;
/// Concurrent inbound streams a node serves; more are dropped.
const INBOUND: usize = 32;
/// Peers met without a contact that `peers` lists.
const MET: usize = 64;
/// How long a closing network service lets its work finish before it
/// cancels what remains.
const DRAIN: Duration = Duration::from_secs(5);
/// How long a staged checkpoint import waits for its linked signer without
/// a new page before another peer may restart it from its own checkpoint.
const IMPORT_SOURCE: Duration = Duration::from_secs(180);

/// A definite refusal: retrying the same call cannot succeed.
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
/// A refusal that keeps a named error, such as a full registry.
fn named(error: Failure) -> Failure {
    if error.is::<crate::registry::RegistryFull>() {
        error
    } else {
        refused(error)
    }
}

/// An executor's refusal that a later run of the same ticket may not meet.
#[derive(Debug)]
struct Deferred(String);
impl std::fmt::Display for Deferred {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Deferred {}

/// A bound SQL value as the JSON contract spells it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum Cell {
    Null,
    Integer(String),
    /// The 16 hexadecimal digits of the double's bits.
    Real(String),
    Text(String),
    Blob(Vec<u8>),
}
impl Cell {
    pub(crate) fn sql(self) -> Result<SqlValue, Failure> {
        Ok(match self {
            Self::Null => SqlValue::Null,
            Self::Integer(value) => SqlValue::Integer(value.parse()?),
            Self::Real(bits) => SqlValue::Real(
                u64::from_str_radix(&bits, 16)
                    .ok()
                    .filter(|_| bits.len() == 16)
                    .and_then(rocky_core::Real::from_bits)
                    .ok_or("invalid REAL bits")?,
            ),
            Self::Text(value) => SqlValue::Text(value),
            Self::Blob(value) => SqlValue::Blob(value),
        })
    }
}
impl From<SqlValue> for Cell {
    fn from(value: SqlValue) -> Self {
        match value {
            SqlValue::Null => Self::Null,
            SqlValue::Integer(value) => Self::Integer(value.to_string()),
            SqlValue::Real(value) => Self::Real(format!("{:016x}", value.bits())),
            SqlValue::Text(value) => Self::Text(value),
            SqlValue::Blob(value) => Self::Blob(value),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    sql: String,
    #[serde(default)]
    values: Vec<Cell>,
    max_rows: usize,
    /// Encoded page budget, capped at the native frame limit.
    #[serde(default)]
    max_bytes: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Clone)]
struct Reader {
    session: SecureSession<TypedCommands>,
    sql: Arc<SecureSql<TypedCommands>>,
}
struct Subscription {
    query: Query,
    delivered: Option<String>,
}

/// One session's reader and subscriptions. Sessions share the database.
pub(crate) struct Client {
    reader: Mutex<Option<Reader>>,
    cursor_key: [u8; 32],
    subscriptions: std::sync::Mutex<BTreeMap<u64, Subscription>>,
    next: AtomicU64,
    closing: watch::Sender<bool>,
    subscriptions_changed: watch::Sender<()>,
}
impl Client {
    fn subscriptions(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<u64, Subscription>>, Failure> {
        self.subscriptions
            .lock()
            .map_err(|_| "subscriptions poisoned".into())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Open,
    /// Accepts nothing new; admitted work finishes.
    Draining,
    Cancelled,
}
/// The attached network service.
struct Link {
    id: u64,
    transport: Arc<dyn Transport>,
    owned: bool,
    stage: watch::Sender<Stage>,
    server: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    synchronizer: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    publisher: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// What the transport finds peers with, and the worker announcing on it.
    discovery: Option<Arc<dyn crate::transport::Discovery>>,
    discoverer: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Inbound streams being served, served so far and dropped over budget.
    active: Arc<Semaphore>,
    live_budget: Arc<Semaphore>,
    live_handshakes: Arc<Semaphore>,
    live: live_links::Registry,
    served: AtomicU64,
    refused: AtomicU64,
}
/// Signals when an automatic pass has been dropped.
struct WorkerActivity(watch::Sender<bool>);
impl Drop for WorkerActivity {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}
struct SyncActivity(watch::Sender<&'static str>);
impl Drop for SyncActivity {
    fn drop(&mut self) {
        self.0.send_replace("idle");
    }
}
#[derive(Default)]
struct PeerRetry {
    short: bool,
    retry: crate::scheduling::Retry,
    pending: bool,
    link: bool,
    link_retry: crate::scheduling::Retry,
    generation: u64,
}
struct Reached {
    at: u64,
    error: Option<String>,
    /// Scopes the last successful sync with the peer did not exchange.
    refused: Vec<ResourceScope>,
    /// Scopes the peer restores from a checkpoint instead of this device's history.
    waiting: Vec<ResourceScope>,
    held: Vec<ResourceScope>,
    /// Why the last successful sync's results pass failed, if it did.
    results: Option<String>,
}

/// How long the sync worker lets a committed change settle before its pass:
/// local observers read first on the shared reactor, and a burst of commits
/// becomes one pass. The crate's own tests freeze the clock and run without it.
const LOCAL_SETTLE: std::time::Duration =
    std::time::Duration::from_millis(if cfg!(test) { 0 } else { 20 });
/// Waits `LOCAL_SETTLE`; with none there is no timer at all.
async fn settle() {
    if !LOCAL_SETTLE.is_zero() {
        tokio::time::sleep(LOCAL_SETTLE).await;
    }
}

pub(crate) struct Node {
    owner: std::sync::OnceLock<std::sync::Weak<Node>>,
    pub replica: EncryptedReplica<TypedCommands>,
    pub domain: Domain,
    /// The scopes this device's credentials name. A renewal keeps them.
    granted: std::sync::Mutex<Vec<ResourceScope>>,
    pub vault: Vault,
    pub settings: Mutex<Shared<Settings>>,
    pub path: PathBuf,
    pub online: AtomicBool,
    /// Set by logout; `purge` also removes local data at close.
    pub ended: Mutex<Option<bool>>,
    /// Calls that use the network hold it shared; attach and detach replace it.
    network: RwLock<Option<Arc<Link>>>,
    /// The attached link for status calls, which never wait for the network.
    current: std::sync::Mutex<Option<Arc<Link>>>,
    detaching: Mutex<()>,
    links: AtomicU64,
    clients: std::sync::Mutex<BTreeMap<u64, Arc<Client>>>,
    syncing: Mutex<()>,
    bootstrap: Mutex<()>,
    retries: std::sync::Mutex<BTreeMap<PublicKey, PeerRetry>>,
    resolution_retries:
        std::sync::Mutex<BTreeMap<rocky_credentials::PeerId, crate::scheduling::Retry>>,
    forwarding: command_mesh::Slots,
    sync_control: watch::Sender<u64>,
    worker_running: watch::Sender<bool>,
    sync_state: watch::Sender<&'static str>,
    automatic: AtomicBool,
    foreground: AtomicBool,
    sync_events: watch::Sender<u64>,
    link_events: watch::Sender<()>,
    sync_status: watch::Sender<Value>,
    link_revision: AtomicU64,
    /// What the node did in the background, for applications to show.
    pub(crate) activity: crate::activity::Activity,
    backups: Mutex<()>,
    reached: std::sync::Mutex<BTreeMap<PublicKey, Reached>>,
    /// Peers whose sessions this device served, with the last time; at
    /// most `MET` are kept.
    met: std::sync::Mutex<BTreeMap<PublicKey, u64>>,
    /// A session brought statements since the last renewal. Rotated keys
    /// still arrive by renewal, so the next sync pass renews once.
    learned: AtomicBool,
    handover: std::sync::Mutex<std::collections::BTreeSet<PublicKey>>,
    /// By scope, the last staged row of an import another peer would
    /// restart and when it was last seen to move.
    import_sources: std::sync::Mutex<
        BTreeMap<ResourceScope, (Option<rocky_core::Commitment>, tokio::time::Instant)>,
    >,
    choices: std::sync::Mutex<BTreeMap<PublicKey, &'static str>>,
    /// Signals that the discovery topics may have changed.
    topics: watch::Sender<u64>,
    /// Whether this device announces under a topic now.
    announcing: AtomicBool,
    /// Moves the discovery clock forward, for the topic window.
    #[cfg(test)]
    pub(crate) discovery_skew: AtomicU64,
    /// The application's coordinator, when it composed strong commands.
    coordinator: Option<Arc<dyn Coordinator>>,
    /// Takes an unassigned domain of a coordination service for this
    /// executing device, once per fencing epoch.
    acquire: Option<Acquire>,
    /// The coordination service commands are composed with, for its owner.
    service: Option<crate::owner::Service>,
    /// Deposits held as a mailbox, loaded at first use.
    mailbox: Mutex<Option<crate::delivery::Mailbox>>,
}
/// Acquires `scope` at fencing `epoch` with this device's node key.
pub(crate) type Acquire = Arc<
    dyn Fn(rocky_core::Scope, u64) -> rocky_core::CoordinationFuture<'static, ()> + Send + Sync,
>;

impl Node {
    pub fn new(
        replica: EncryptedReplica<TypedCommands>,
        domain: Domain,
        vault: Vault,
        settings: Settings,
        path: PathBuf,
        coordinator: Option<Arc<dyn Coordinator>>,
        acquire: Option<Acquire>,
    ) -> Self {
        Self {
            owner: Default::default(),
            replica,
            granted: std::sync::Mutex::new(settings.granted(&domain)),
            domain,
            vault,
            settings: Mutex::new(Shared::new(settings)),
            path,
            online: AtomicBool::new(true),
            ended: Mutex::new(None),
            network: RwLock::new(None),
            current: std::sync::Mutex::new(None),
            detaching: Mutex::new(()),
            links: AtomicU64::new(1),
            clients: std::sync::Mutex::new(BTreeMap::new()),
            syncing: Mutex::new(()),
            bootstrap: Mutex::new(()),
            retries: std::sync::Mutex::new(BTreeMap::new()),
            resolution_retries: std::sync::Mutex::new(BTreeMap::new()),
            forwarding: command_mesh::Slots::default(),
            sync_control: watch::channel(0).0,
            worker_running: watch::channel(false).0,
            sync_state: watch::channel("idle").0,
            automatic: AtomicBool::new(false),
            foreground: AtomicBool::new(true),
            sync_events: watch::channel(0).0,
            link_events: watch::channel(()).0,
            link_revision: AtomicU64::new(0),
            sync_status: watch::channel(json!({"token": "0", "state": "idle", "retry": false})).0,
            activity: crate::activity::Activity::new(),
            backups: Mutex::new(()),
            reached: std::sync::Mutex::new(BTreeMap::new()),
            met: std::sync::Mutex::new(BTreeMap::new()),
            learned: AtomicBool::new(false),
            handover: std::sync::Mutex::new(Default::default()),
            import_sources: std::sync::Mutex::new(Default::default()),
            choices: std::sync::Mutex::new(BTreeMap::new()),
            topics: watch::channel(0).0,
            announcing: AtomicBool::new(false),
            #[cfg(test)]
            discovery_skew: AtomicU64::new(0),
            coordinator,
            acquire,
            service: None,
            mailbox: Mutex::new(None),
        }
    }
    /// Lets the owner control its domain on this coordination service.
    pub fn with_service(mut self, service: Option<crate::owner::Service>) -> Self {
        self.service = service;
        self
    }
    /// Every granted scope, the registry's included: what sync covers.
    fn scopes(&self) -> Vec<ResourceScope> {
        self.granted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    /// The granted scopes the application's sessions open: the registry is
    /// the SDK's own.
    fn app_scopes(&self) -> Vec<ResourceScope> {
        let registry = self.domain.registry();
        self.scopes()
            .into_iter()
            .filter(|scope| *scope != registry)
            .collect()
    }

    fn clients(&self) -> Vec<Arc<Client>> {
        self.clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }
    /// The state of session `id`, created by its first call. Resolved as
    /// a call arrives, so a session's end never precedes its earlier calls.
    pub fn client(&self, id: u64) -> Result<Arc<Client>, Failure> {
        let mut clients = self.clients.lock().map_err(|_| "sessions poisoned")?;
        if let Some(client) = clients.get(&id) {
            return Ok(client.clone());
        }
        if clients.len() >= MAX_SESSIONS {
            return Err("session quota".into());
        }
        let client = Arc::new(Client {
            reader: Mutex::new(None),
            cursor_key: rand::random(),
            subscriptions: std::sync::Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            closing: watch::channel(false).0,
            subscriptions_changed: watch::channel(()).0,
        });
        clients.insert(id, client.clone());
        Ok(client)
    }
    /// Ends session `id`, or every session: pending waits stop, admitted
    /// calls finish. Other sessions and the database stay open.
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
    /// A session over every collection, opened again after an authority
    /// change ended the previous one.
    async fn reader(&self, client: &Client) -> Result<Reader, Failure> {
        let mut cached = client.reader.lock().await;
        if let Some(reader) = &*cached
            && reader.session.status().await.is_ok()
        {
            return Ok(reader.clone());
        }
        *cached = None;
        let session = self
            .replica
            .session(self.app_scopes().into_iter().collect())
            .await?;
        let sql = Arc::new(session.sql(SqlOptions::default()).await?);
        let reader = Reader { session, sql };
        *cached = Some(reader.clone());
        Ok(reader)
    }
    /// A SQL session over every granted table, for a driver connection.
    pub(crate) async fn sql_session(
        &self,
        options: SqlOptions,
    ) -> Result<(SecureSession<TypedCommands>, SecureSql<TypedCommands>), Failure> {
        if self.ended.lock().await.is_some() {
            return Err(refused("this device logged out of the domain"));
        }
        let session = self
            .replica
            .session(self.app_scopes().into_iter().collect())
            .await?;
        let sql = session.sql(options).await?;
        Ok((session, sql))
    }
    async fn forget_readers(&self) {
        for client in self.clients() {
            *client.reader.lock().await = None;
        }
    }

    /// Serves `transport` as the network service in the empty `slot`.
    fn install(
        self: &Arc<Self>,
        slot: &mut Option<Arc<Link>>,
        transport: Arc<dyn Transport>,
        owned: bool,
    ) -> u64 {
        let _ = self.owner.set(Arc::downgrade(self));
        let link = Arc::new(Link {
            id: self.links.fetch_add(1, Ordering::Relaxed),
            owned,
            stage: watch::channel(Stage::Open).0,
            server: std::sync::Mutex::new(None),
            synchronizer: std::sync::Mutex::new(None),
            publisher: std::sync::Mutex::new(None),
            discovery: transport.discovery(),
            transport,
            discoverer: std::sync::Mutex::new(None),
            active: Arc::new(Semaphore::new(INBOUND)),
            live_budget: Arc::new(Semaphore::new(live_links::BUDGET)),
            live_handshakes: Arc::new(Semaphore::new(live_links::BUDGET)),
            live: live_links::Registry::default(),
            served: AtomicU64::new(0),
            refused: AtomicU64::new(0),
        });
        link.transport.listen(true);
        let publisher = tokio::task::spawn_local(self.clone().publish_links(link.clone()));
        *link
            .publisher
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(publisher);
        let synchronizer = tokio::task::spawn_local(self.clone().synchronize(link.clone()));
        *link
            .synchronizer
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(synchronizer);
        let server = tokio::task::spawn_local(self.clone().accept(link.clone()));
        *link.server.lock().unwrap_or_else(PoisonError::into_inner) = Some(server);
        if let Some(discovery) = link.discovery.clone() {
            let discoverer =
                tokio::task::spawn_local(self.clone().discover(link.clone(), discovery));
            *link
                .discoverer
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(discoverer);
        }
        let id = link.id;
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(link.clone());
        *slot = Some(link);
        self.wake_sync();
        id
    }
    /// Installs the transport the database opened with.
    pub async fn link(self: &Arc<Self>, transport: Arc<dyn Transport>, owned: bool) -> u64 {
        self.install(&mut *self.network.write().await, transport, owned)
    }
    /// Attaches a network service of any transport kind: routes of that
    /// kind are dialed, and the domain's identity does not depend on it.
    pub async fn attach(self: &Arc<Self>, network: Network) -> Result<u64, Failure> {
        let mut slot = self.network.write().await;
        if slot.is_some() {
            return Err(ServiceError::NetworkAttached.into());
        }
        if self.ended.lock().await.is_some() {
            return Err(refused("this device logged out of the domain"));
        }
        let issuer = self.settings.lock().await.issuer();
        let key = Settings::transport_key(&self.vault)?;
        let (transport, owned) = network.start(issuer, key).await?;
        Ok(self.install(&mut slot, transport, owned))
    }
    /// Detaches network service `id`, or whichever is attached. Admitted
    /// work has `DRAIN` to finish; an owned transport is then closed, a
    /// borrowed one is left as it was. Does nothing once detached.
    pub async fn detach(&self, id: Option<u64>) {
        let _detaching = self.detaching.lock().await;
        let attached = self.network.read().await.clone();
        let Some(link) = attached.filter(|link| id.is_none_or(|id| link.id == id)) else {
            return;
        };
        link.stage.send_replace(Stage::Draining);
        link.transport.listen(false);
        let mut slot = match tokio::time::timeout(DRAIN, self.network.write()).await {
            Ok(slot) => slot,
            Err(_) => {
                link.stage.send_replace(Stage::Cancelled);
                self.network.write().await
            }
        };
        *slot = None;
        self.synced(json!({"state": "idle", "retry": false}));
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = None;
        drop(slot);
        // The accept loop holds the transport's inbound queue until it ends.
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
    /// Serves inbound streams until the service drains or the transport ends.
    async fn accept(self: Arc<Self>, link: Arc<Link>) {
        let mut tasks = tokio::task::JoinSet::new();
        let mut stage = link.stage.subscribe();
        loop {
            let inbound = tokio::select! {
                inbound = link.transport.accept() => inbound,
                _ = stage.wait_for(|stage| *stage != Stage::Open) => None,
            };
            let Some((protocol, stream)) = inbound else {
                break;
            };
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
            tasks.spawn_local(async move {
                if protocol == Protocol::Link {
                    let settings = node.settings.lock().await.clone();
                    if let Err(error) = node
                        .admit_link(service, stream, None, settings, Some(permit))
                        .await
                    {
                        let _ = node.absorb_revocations(Some(&error)).await;
                        node.activity.record_changed(
                            "link_admission_failed",
                            json!({"error": error.to_string()}),
                        );
                    }
                } else {
                    let _permit = permit;
                    let _ = node.serve(protocol, stream).await;
                }
            });
            while tasks.try_join_next().is_some() {}
        }
        let _ =
            tokio::time::timeout(DRAIN, async { while tasks.join_next().await.is_some() {} }).await;
        tasks.shutdown().await;
    }
    /// Runs a call that uses the network. A closing service cancels it once
    /// its drain bound passes.
    async fn networked(
        &self,
        client: &Client,
        kind: &str,
        request: &Value,
    ) -> Result<Value, Failure> {
        let attached = self.network.read().await;
        let transport = attached.as_ref().map(|link| &*link.transport);
        let work = async {
            match kind {
                "pairing" => {
                    // There is no default grant: the issuer names a profile.
                    let profile: Profile = serde_json::from_value(request["profile"].clone())
                        .map_err(|_| {
                            refused("pairing needs a grant profile: reader, writer or issuer")
                        })?;
                    let transport = transport.ok_or(ServiceError::NetworkRequired("pairing"))?;
                    let registry = self.registry().await?;
                    let mut settings = self.settings.lock().await;
                    let (uri, expires) = settings
                        .invite(
                            &self.path,
                            &self.vault,
                            transport,
                            profile.clone(),
                            &registry,
                        )
                        .await
                        .map_err(named)?;
                    let sequence = self.activity.record(
                        "invitation_created",
                        json!({"profile": profile, "expires": expires.to_string()}),
                    );
                    Ok(
                        json!({"uri": uri, "expires": expires.to_string(), "profile": profile, "sequence": sequence.to_string()}),
                    )
                }
                "renew" => {
                    self.renew(transport, true, None, false).await?;
                    self.status(client).await
                }
                "authors" => self.authors(transport).await,
                "contact" => {
                    let transport = transport.ok_or(ServiceError::NetworkRequired("contact"))?;
                    let settings = self.settings.lock().await.clone();
                    let (uri, node, routes) = settings.contact(&self.vault, transport).await?;
                    Ok(json!({"uri": uri, "node": node.to_hex(), "routes": routes}))
                }
                _ => self
                    .sync(transport.ok_or(ServiceError::NetworkRequired("sync"))?)
                    .await
                    .map(|(report, _)| report),
            }
        };
        let Some(link) = attached.as_ref() else {
            return work.await;
        };
        let mut stage = link.stage.subscribe();
        tokio::select! {
            result = work => result,
            _ = stage.wait_for(|stage| *stage == Stage::Cancelled) => {
                Err(ServiceError::Closed("network service").into())
            }
        }
    }

    /// Handles one call of the native ABI for a session.
    pub async fn dispatch(&self, client: &Client, request: &Value) -> Result<Value, Failure> {
        let kind = request["type"].as_str().unwrap_or_default();
        if self.ended.lock().await.is_some()
            && !matches!(
                kind,
                "domain" | "network" | "status" | "status_next" | "activity" | "activity_next"
            )
        {
            return Err(refused("this device logged out of the domain"));
        }
        match kind {
            "domain" => {
                let settings = self.settings.lock().await;
                Ok(json!({
                    "scope": self.domain.scope,
                    "issuer": settings.issuer(),
                    "profile": settings.profile,
                    "collections": settings.schema.names(),
                    "durable_mutations": true,
                    "transport": self.kind(),
                }))
            }
            "session" => self.status(client).await,
            "invitation_open" => {
                let uri = request["uri"]
                    .as_str()
                    .ok_or_else(|| refused("uri must be an invitation"))?;
                Ok(json!({"open": self.settings.lock().await.invitation_open(uri)?}))
            }
            "generation" => self.generation().await,
            "retention" => self.retention(request).await,
            "horizon" => self.horizon(request).await,
            "retention_limits" => {
                if let Some(limits) = request.get("limits") {
                    self.replica
                        .set_retention_limits(serde_json::from_value(limits.clone())?)
                        .await?;
                }
                Ok(serde_json::to_value(self.replica.retention_limits().await)?)
            }
            "status" => self.readiness().await,
            "activity" => Ok(self.activity.after(token(request)?)),
            "activity_next" => {
                let mut closing = client.closing.subscribe();
                self.activity
                    .next(token(request)?, &mut closing)
                    .await
                    .ok_or_else(|| ServiceError::Closed("session").into())
            }
            "status_next" => {
                let after = request["after"]
                    .as_str()
                    .ok_or_else(|| refused("after must be a status token"))?;
                self.readiness_after(client, after).await
            }
            "backup" => {
                let target = request["path"]
                    .as_str()
                    .ok_or_else(|| refused("path must be a string"))?;
                self.backup(std::path::Path::new(target)).await
            }
            "sync_start" => {
                let enabled = request
                    .get("enabled")
                    .map(|enabled| {
                        enabled
                            .as_bool()
                            .ok_or_else(|| refused("enabled must be boolean"))
                    })
                    .transpose()?
                    .unwrap_or(true);
                self.automatic.store(enabled, Ordering::SeqCst);
                if enabled {
                    self.reset_retry(None);
                }
                self.control_sync();
                if !enabled {
                    if let Some(service) = self.current() {
                        service
                            .live
                            .close(rocky_secure::network::link::CloseReason::Offline);
                    }
                    // Acknowledge only after the worker has dropped its pass.
                    let mut running = self.worker_running.subscribe();
                    running.wait_for(|running| !running).await?;
                    if let Some(service) = self.current() {
                        service.live.wait_empty().await;
                    }
                    self.synced(json!({"state": "idle", "retry": false}));
                }
                Ok(self.sync_status.borrow().clone())
            }
            "sync_next" => {
                let after = request["after"]
                    .as_str()
                    .ok_or_else(|| refused("after must be a sync token"))?;
                let mut status = self.sync_status.subscribe();
                let mut closing = client.closing.subscribe();
                loop {
                    let current = status.borrow_and_update().clone();
                    if current["token"].as_str() != Some(after) {
                        return Ok(current);
                    }
                    tokio::select! {
                        _ = status.changed() => {},
                        _ = closing.wait_for(|closing| *closing) => return Err(ServiceError::Closed("session").into()),
                    }
                }
            }
            #[cfg(feature = "qualification")]
            "network_fault_close" => {
                let attached = self.network.read().await;
                let link = attached
                    .as_ref()
                    .ok_or(ServiceError::NetworkRequired("fault injection"))?;
                link.transport.fault_close_sockets().await?;
                Ok(json!({}))
            }
            "network_foreground" => {
                if let Some(link) = self.network.read().await.as_ref() {
                    link.transport
                        .foreground()
                        .await
                        .map_err(|error| ServiceError::Unreachable(error.to_string()))?;
                }
                self.foreground.store(true, Ordering::SeqCst);
                self.discovery_foreground(true).await;
                self.reset_retry(None);
                self.wake_sync();
                Ok(json!({}))
            }
            "network_background" => {
                self.foreground.store(false, Ordering::SeqCst);
                if let Some(service) = self.current() {
                    service
                        .live
                        .close(rocky_secure::network::link::CloseReason::Offline);
                    service.live.wait_empty().await;
                }
                self.discovery_foreground(false).await;
                Ok(json!({}))
            }
            "network" => Ok(self.network_status().await),
            "pairing" | "renew" | "sync" | "contact" | "authors" => {
                self.networked(client, kind, request).await
            }
            "sql" | "next_id" | "move_row" => self.sql_call(client, kind, request).await,
            kind if kind.starts_with("partition_") => self.partition_call(kind, request).await,
            "devices" => self.devices().await,
            "device_label" => self.label(request).await,
            "grant" | "delegate" => self.grant(request).await,
            kind if kind.starts_with("pseudonym_") => self.pseudonym(kind, request).await,
            "install" => self.install_supplied(client, request).await,
            "contact_request" => {
                let encoded = request["id"]
                    .as_str()
                    .ok_or_else(|| refused("id must be a 16-byte node ID"))?;
                let id: [u8; 16] = HEXLOWER
                    .decode(encoded.as_bytes())?
                    .try_into()
                    .map_err(|_| refused("id must be a 16-byte node ID"))?;
                let network = self.network.read().await;
                self.request_contact(
                    &*network
                        .as_ref()
                        .ok_or_else(|| refused("no network attached"))?
                        .transport,
                    rocky_core::NodeId(id),
                )
                .await
            }
            "discovery_topics" => {
                let settings = self.settings.lock().await;
                let (current, previous) = settings.discovery_topics(&self.vault, now()?)?;
                Ok(
                    json!({"current": HEXLOWER.encode(&current), "previous": previous.map(|topic| HEXLOWER.encode(&topic))}),
                )
            }
            "peers" => {
                if let Some(link) = self.current() {
                    Box::pin(self.resolve_saved_contacts(&*link.transport)).await;
                }
                Ok(json!({
                    "peers": self.peers().await,
                    "discovery": self.discovery_status(),
                    "issuer_contact": self.settings.lock().await.issuer_contact(),
                }))
            }
            "enrollment_route" => Ok(json!({"uri": crate::route_enrollment(
                request["uri"].as_str().ok_or_else(|| refused("uri must be an invitation or issuer contact"))?,
                request["contact"].as_str().ok_or_else(|| refused("contact must name the custody peer"))?,
            )?})),
            "enrollment_upstream" => {
                let contact = request["contact"].as_str();
                if !request["contact"].is_null() && contact.is_none() {
                    return Err(refused("contact must be an issuer contact or null"));
                }
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
                Ok(json!({}))
            }
            "peer_add" => {
                let contact = request["contact"]
                    .as_str()
                    .ok_or("contact must be a contact URI")?;
                let registry = self.registry().await?;
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
                self.activity
                    .record("peer_added", json!({"peer": node.to_hex()}));
                self.wake_sync();
                Ok(json!({"node": node.to_hex()}))
            }
            "peer_remove" => {
                let node: PublicKey = request["node"]
                    .as_str()
                    .ok_or("node must be a public key")?
                    .parse()?;
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
                self.activity
                    .record("peer_removed", json!({"peer": node.to_hex()}));
                self.wake_sync();
                Ok(json!({}))
            }
            "revoke" => {
                let node: PublicKey = request["node"]
                    .as_str()
                    .ok_or("node must be a public key")?
                    .parse()?;
                let revoked = self.revoke(node).await?;
                self.activity
                    .record("device_revoked", json!({"node": node.to_hex()}));
                Ok(revoked)
            }
            "logout" => {
                self.logout(request["purge"].as_bool().unwrap_or(false))
                    .await
            }
            "connection" => {
                let enabled = request["online"]
                    .as_bool()
                    .ok_or("online must be boolean")?;
                if self.online.swap(enabled, Ordering::SeqCst) != enabled {
                    self.activity
                        .record(if enabled { "online" } else { "offline" }, json!({}));
                }
                if enabled {
                    self.reset_retry(None);
                    // A pass with nothing to dial republishes the current status.
                    if self.sync_status.borrow()["state"] == "offline" {
                        self.synced(json!({"state": "idle", "retry": false}));
                    }
                }
                if !enabled && let Some(service) = self.current() {
                    service
                        .live
                        .close(rocky_secure::network::link::CloseReason::Offline);
                }
                self.control_sync();
                if !enabled && let Some(service) = self.current() {
                    service.live.wait_empty().await;
                }
                Ok(json!({"online": enabled}))
            }

            "mutation_progress" => {
                let handle: Vec<u8> = serde_json::from_value(request["handle"].clone())?;
                let handle: [u8; 32] = handle.try_into().map_err(|_| refused("handle"))?;
                Ok(
                    json!({"committed": self.replica.handle_committed(secure_handle(&handle)).await?}),
                )
            }
            "read" => {
                self.read(client, serde_json::from_value(request["query"].clone())?)
                    .await
            }
            "subscribe" => {
                let query: Query = serde_json::from_value(request["query"].clone())?;
                let mut subscriptions = client.subscriptions()?;
                if subscriptions.len() >= MAX_SUBSCRIPTIONS {
                    return Err("subscription quota".into());
                }
                let id = client.next.fetch_add(1, Ordering::Relaxed);
                subscriptions.insert(
                    id,
                    Subscription {
                        query,
                        delivered: None,
                    },
                );
                Ok(json!({"subscription": id.to_string()}))
            }
            kind if kind.starts_with("command_") => {
                let result = self.command(kind, request).await;
                self.wake_sync();
                result
            }
            "next" => self.next(client, subscription(request)?).await,
            "unsubscribe" => {
                client.subscriptions()?.remove(&subscription(request)?);
                client.subscriptions_changed.send_replace(());
                Ok(json!({}))
            }
            other => Err(refused(format!("unsupported request {other}"))),
        }
    }

    fn reached(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<PublicKey, Reached>>, Failure> {
        self.reached
            .lock()
            .map_err(|_| "peer status poisoned".into())
    }
    /// Applies a change of the configured peers, saving it when what is
    /// persisted changed.
    async fn configure<T>(
        &self,
        change: impl FnOnce(&mut Settings) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let mut settings = self.settings.lock().await;
        let mut draft = settings.clone();
        let result = change(&mut draft).map_err(named)?;
        if !draft.persisted_eq(&settings) {
            draft.save(&self.path)?;
        }
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = draft.granted(&self.domain);
        *settings = draft;
        Ok(result)
    }
    /// Issues raw grants or a profile to a prepared device, as the
    /// credentials it installs: signed by the publisher on an issuer, or
    /// delegated from this device's own chain.
    async fn grant(&self, request: &Value) -> Result<Value, Failure> {
        let device: crate::DeviceKeys = serde_json::from_value(request["device"].clone())
            .map_err(|_| refused("device must be the keys a prepared device reports"))?;
        let profile = match (&request["grants"], &request["profile"]) {
            (Value::Null, profile) => serde_json::from_value(profile.clone()),
            (grants, Value::Null) => {
                serde_json::from_value(grants.clone()).map(|grants| Profile::Grants { grants })
            }
            _ => return Err(refused("name grants or a profile, not both")),
        }
        .map_err(|error| refused(format!("invalid grants: {error}")))?;
        let terms: Terms = serde_json::from_value(json!({
            "ttl": request["ttl"],
            "delegations": request["delegations"],
            "authors": request["authors"],
        }))
        .map_err(|_| refused("ttl must be seconds, delegations 1 to 7 and authors a count"))?;
        let delegate = request["type"] == "delegate";
        let contact = match self.current() {
            Some(link) if !delegate => {
                let settings = self.settings.lock().await.clone();
                Some(settings.contact(&self.vault, &*link.transport).await?.0)
            }
            _ => None,
        };
        let supplied =
            Box::pin(self.answer_grant(device, (profile, terms), delegate, contact)).await?;
        Ok(json!({"supplied": supplied}))
    }
    /// Author pseudonyms: request one, assign one, keep one, find whose it
    /// is, or claim a range. See `docs/runtime/credential-primitives.md`.
    async fn pseudonym(&self, kind: &str, request: &Value) -> Result<Value, Failure> {
        let chain = |name: &str| -> Result<CredentialChain, Failure> {
            serde_json::from_value(request[name].clone())
                .map_err(|_| refused(format!("{name} must be a pseudonym chain")))
        };
        match kind {
            "pseudonym_request" => {
                let collection = request["collection"]
                    .as_str()
                    .ok_or_else(|| refused("collection must be a string"))?;
                let settings = self.settings.lock().await.clone();
                let made = settings
                    .pseudonym_request(&self.vault, collection)
                    .map_err(refused)?;
                Ok(json!({"request": HEXLOWER.encode(&made.to_bytes()?)}))
            }
            "pseudonym_assign" => {
                let bytes = HEXLOWER
                    .decode(request["request"].as_str().unwrap_or_default().as_bytes())
                    .map_err(|_| refused("request must be hexadecimal"))?;
                let asked = rocky_credentials::PseudonymRequest::from_bytes(&bytes)?;
                let assigned = Box::pin(self.answer_author(&asked)).await?;
                Ok(json!({"pseudonym": assigned}))
            }
            "pseudonym_install" => {
                let pseudonym = chain("pseudonym")?;
                let (collection, author) = self
                    .configure(|settings| settings.hold_pseudonym(&self.vault, pseudonym.clone()))
                    .await?;
                let key =
                    PrivateKey::from_bytes(&self.vault.get(&format!("pseudonym.{collection}"))?);
                self.replica.register_row_author(key, pseudonym).await?;
                Ok(json!({"collection": collection, "author": author}))
            }
            "pseudonym_owner" => {
                let settings = self.settings.lock().await.clone();
                let (node, collection, author) = settings
                    .pseudonym_owner(&self.vault, &chain("pseudonym")?)
                    .map_err(refused)?;
                Ok(json!({"node": node.to_hex(), "collection": collection, "author": author}))
            }
            "pseudonym_range" => {
                let number = |name: &str| {
                    request[name]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or_else(|| refused(format!("{name} must be an author number")))
                };
                let range = rocky_credentials::AuthorRange::new(number("first")?, number("last")?)?;
                self.configure(|settings| settings.claim_authors(range))
                    .await?;
                // The issuer's own capability carries the range from now on.
                self.renew(None, false, None, false).await?;
                Ok(json!({"first": range.first, "last": range.last}))
            }
            other => Err(refused(format!("unsupported request {other}"))),
        }
    }
    /// Replaces this device's credentials with supplied ones and teaches
    /// the replica, as a renewal does.
    async fn install_supplied(&self, client: &Client, request: &Value) -> Result<Value, Failure> {
        let supplied: crate::Supplied = serde_json::from_value(request["supplied"].clone())
            .map_err(|error| refused(format!("invalid supplied credentials: {error}")))?;
        let current = self.settings.lock().await.clone();
        let renewed = current
            .supplied_renewal(&self.vault, &supplied)
            .map_err(refused)?;
        let registry = self.registry().await?;
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
        self.apply(&settings.revocations, &settings.credentials, keys)
            .await?;
        self.activity.record(
            "renewed",
            json!({
                "revocations": settings.revocation_status()?,
                "expires": settings.expires().to_string(),
                "keys": fresh,
            }),
        );
        // New grants may make more members of a held group readable.
        for partition in settings.partitions(&self.domain)? {
            self.hold_keys(&settings, &partition).await?;
        }
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? =
            settings.granted(&self.domain);
        drop(settings);
        self.forget_readers().await;
        self.status(client).await
    }
    /// Every peer met or kept as a contact: its routes, whether it is a
    /// configured contact, and the last attempt to sync with it. A peer met
    /// only through its own session has no routes.
    async fn peers(&self) -> Vec<Value> {
        let settings = self.settings.lock().await;
        let reached = self.reached.lock().unwrap_or_else(PoisonError::into_inner);
        let met = self.met.lock().unwrap_or_else(PoisonError::into_inner);
        let nodes: std::collections::BTreeSet<PublicKey> = settings
            .peers
            .keys()
            .chain(met.keys())
            .filter(|node| !settings.revocations.revokes(node))
            .copied()
            .collect();
        nodes
            .into_iter()
            .map(|node| {
                let contact = settings.peers.get(&node);
                let last = reached.get(&node);
                let state = match (last, met.get(&node)) {
                    (Some(Reached { error: None, .. }), _) | (None, Some(_)) => "connected",
                    (Some(_), _) => "unreachable",
                    (None, None) => "unknown",
                };
                let checked = last.map(|last| last.at).or_else(|| met.get(&node).copied());
                json!({
                    "node": node.to_hex(),
                    "issuer": settings.issuers.contains(&node),
                    "configured": contact.is_some(),
                    "local": settings.contact_meta.get(&node).is_some_and(|meta| meta.local),
                    "source": settings.contact_meta.get(&node).map_or(if contact.is_some() { "enrollment" } else { "session" }, |meta| meta.source.as_str()),
                    "choice": self.choices.lock().unwrap_or_else(PoisonError::into_inner).get(&node).copied().unwrap_or("not_considered"),
                    "routes": contact.map(|contact| contact.routes.clone()).unwrap_or_default(),
                    "link": self.current().map_or_else(|| json!({"state":"closed","opened":null,"round_trip_ms":null,"paths":[]}), |service| service.live.status(node)),
                    "state": state,
                    "checked": checked.map(|at| at.to_string()),
                    "error": last.and_then(|last| last.error.clone()),
                })
            })
            .collect()
    }
    /// The network service, whatever the state of the database's sessions.
    async fn network_status(&self) -> Value {
        let peers = self.peers().await;
        let Some(link) = self.current() else {
            return json!({"attached": false, "peers": peers, "discovery": self.discovery_status()});
        };
        json!({
            "attached": true,
            "kind": link.transport.kind(),
            "owned": link.owned,
            "online": self.online.load(Ordering::SeqCst),
            "sync": self.sync_status.borrow().clone(),
            "draining": *link.stage.borrow() != Stage::Open,
            "discovery": self.discovery_status(),
            "inbound": {
                "limit": INBOUND,
                "active": INBOUND - link.active.available_permits(),
                "served": link.served.load(Ordering::Relaxed).to_string(),
                "refused": link.refused.load(Ordering::Relaxed).to_string(),
            },
            "peers": peers,
        })
    }
    fn current(&self) -> Option<Arc<Link>> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    /// The attached network's transport kind, empty without one.
    fn kind(&self) -> String {
        self.current()
            .map(|link| link.transport.kind().to_owned())
            .unwrap_or_default()
    }
    async fn horizon(&self, request: &Value) -> Result<Value, Failure> {
        let collection = request["collection"]
            .as_str()
            .ok_or("collection must be a table name")?;
        let scope = if request.get("values").is_some() {
            let mut address = request.clone();
            address["table"] = collection.into();
            self.partition_address(&address)?.0
        } else {
            self.domain
                .scopes()
                .find(|(name, _)| *name == collection)
                .map(|(_, scope)| scope)
                .filter(|scope| self.scopes().contains(scope))
                .ok_or_else(|| refused("horizon scope is not granted"))?
        };
        let action = request["action"].as_str().unwrap_or("status");
        let mut declare = action == "declare";
        if action == "maintain" {
            let limit = request["max_operations"].as_u64().unwrap_or(1024);
            if limit == 0 {
                return Err(refused("horizon operation cadence must be positive"));
            }
            let current = self.replica.admitted_horizon(scope).await?;
            let finalized = self.replica.finalized_horizon(scope).await?;
            if current.is_some() && current != finalized {
                if !self.replica.horizon_rebootstrap_required(scope).await?
                    && self
                        .replica
                        .horizon_progress(scope)
                        .await?
                        .waiting
                        .is_empty()
                    && !self.replica.horizon_dependents_pending(scope).await?
                {
                    self.replica.finalize_horizon(scope).await?;
                    self.replica.prune(scope).await?;
                }
            } else {
                declare = self
                    .replica
                    .horizon_operations_since_finalization(scope)
                    .await?
                    >= limit;
            }
        }
        match if declare { "declare" } else { action } {
            "declare" => {
                if !self.settings.lock().await.issuer() {
                    return Err(refused("only an issuer declares a horizon"));
                }
                let mut statement = self
                    .replica
                    .horizon_statement_for_participants(scope, &[])
                    .await?;
                let scopes = std::iter::once(scope)
                    .chain(statement.related.iter().map(|cut| cut.scope))
                    .collect::<Vec<_>>();
                statement.peers = self.horizon_participants(request, &scopes).await?;
                let publisher = PrivateKey::from_bytes(&self.vault.get("publisher")?);
                let signed = rocky_credentials::SignedHorizon::sign(statement, &publisher)?;
                let own = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
                let participates = signed
                    .statement
                    .peers
                    .iter()
                    .any(|peer| peer.node == rocky_credentials::PeerId::of(own));
                self.replica.receive_horizon(signed).await?;
                if participates {
                    self.replica.acknowledge_horizon(scope).await?;
                }
                self.wake_sync();
            }
            "admit" => {
                let bytes = HEXLOWER.decode(
                    request["statement"]
                        .as_str()
                        .ok_or("statement must be canonical CBOR in hex")?
                        .as_bytes(),
                )?;
                let signed = rocky_credentials::SignedHorizon::from_bytes(&bytes)?;
                if signed.statement.scope != scope {
                    return Err(refused("declaration differs from requested scope"));
                }
                self.replica.receive_horizon(signed).await?;
            }
            "finalize" => {
                self.replica.finalize_horizon(scope).await?;
                self.replica.prune(scope).await?;
                self.wake_sync();
            }
            "status" | "maintain" => {}
            _ => return Err(refused("unknown horizon action")),
        }
        let signed = self.replica.admitted_horizon(scope).await?;
        let progress = if signed.is_some() {
            Some(self.replica.horizon_progress(scope).await?)
        } else {
            None
        };
        Ok(
            json!({"statement": signed.as_ref().map(|signed| signed.to_bytes().map(|bytes| HEXLOWER.encode(&bytes))).transpose()?,
            "sequence": signed.as_ref().map(|signed| signed.statement.sequence),
            "acknowledged": progress.as_ref().map(|progress| &progress.acknowledged),
            "waiting": progress.as_ref().map(|progress| &progress.waiting),
            "behind": progress.as_ref().map(|progress| &progress.behind),
            "issued": signed.as_ref().map(|signed| signed.statement.issued),
            // When finalization stops waiting, at the latest: each waiting
            // participant becomes behind at its deadline.
            "deadline": signed.as_ref().zip(progress.as_ref()).and_then(|(signed, progress)| signed
                .statement
                .peers
                .iter()
                .filter(|peer| progress.waiting.contains(&peer.node))
                .map(|peer| peer.deadline(signed.statement.issued))
                .max()),
            "rebootstrap_required": self.replica.horizon_rebootstrap_required(scope).await?,
            "finalized": self.replica.finalized_horizon(scope).await?.as_ref() == signed.as_ref() && signed.is_some(),
            "operations_since_finalization": self.replica.horizon_operations_since_finalization(scope).await?}),
        )
    }
    async fn retention(&self, request: &Value) -> Result<Value, Failure> {
        let collection = request["collection"]
            .as_str()
            .ok_or("collection must be a table name")?;
        let scope = self
            .domain
            .scopes()
            .find(|(name, _)| *name == collection)
            .map(|(_, scope)| scope)
            .filter(|scope| self.scopes().contains(scope))
            .ok_or_else(|| refused("retention scope is not granted"))?;
        if let Some(policy) = request.get("policy") {
            self.replica
                .set_retention_policy(scope, serde_json::from_value(policy.clone())?)
                .await?;
        }
        if request["maintain"] == true {
            self.replica.checkpoint(scope).await?;
            self.replica.prune(scope).await?;
        }
        let status = self.replica.checkpoint_status(scope).await?;
        Ok(
            json!({"policy": self.replica.retention_policy(scope).await?,
            "covered": status.covered, "uncovered": status.uncovered,
            "horizon": status.horizon, "maintenance_error": status.maintenance_error}),
        )
    }
    async fn status(&self, client: &Client) -> Result<Value, Failure> {
        let reader = self.reader(client).await?;
        let status = reader.session.status().await?;
        let settings = self.settings.lock().await;
        Ok(json!({
            "principal": status.principal.to_hex(),
            "node": status.node.to_hex(),
            "revocations": {"digest": HEXLOWER.encode(&status.revocations), "statements": status.revoked},
            "expires": status.expires.to_string(),
            "issuer": settings.issuer(),
            "profile": settings.profile,
            "transport": self.kind(),
            "readable": status.readable.len(),
            "writable": status.writable.len(),
            "pseudonyms": settings.pseudonyms.iter().map(|(collection, chain)| {
                let author = chain
                    .0
                    .last()
                    .and_then(|leaf| leaf.capability.authors().ok().flatten())
                    .and_then(|range| range.single());
                (collection.clone(), json!(author))
            }).collect::<Map<String, Value>>(),
            "authors": settings.minting,
        }))
    }
    /// What an application shows of a database besides its rows: whether
    /// it is ready, its generation, authority and unsettled tickets, and
    /// the committed revision. `token` changes whenever any of it does.
    async fn readiness(&self) -> Result<Value, Failure> {
        let generation = self.generation().await?;
        let settings = self.settings.lock().await.clone();
        let logged_out = self.ended.lock().await.is_some() || settings.logged_out;
        let revocation = settings.revocation(&self.path)?;
        let expired = settings.expires() <= now()?;
        let mut tickets = BTreeMap::<String, u64>::new();
        for ticket in self.replica.open_tickets(usize::MAX).await? {
            let state = crate::commands::ticket_json(&self.domain.app, &ticket)?["state"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            *tickets.entry(state).or_default() += 1;
        }
        let revision = *self.replica.observe_revision().borrow();
        let mut bootstrapping = self.path.with_file_name("bootstrap.json").exists()
            || *self.sync_state.borrow() == "bootstrapping";
        for scope in &self.scopes() {
            bootstrapping |= self.replica.rebootstrap_progress(*scope).await?.is_some()
                || self.replica.horizon_rebootstrap_required(*scope).await?;
        }
        let state = if revocation.is_some() {
            "revoked"
        } else if logged_out || expired || !generation["rebuilding"].is_null() {
            "unavailable"
        } else if bootstrapping {
            "bootstrapping"
        } else if *self.sync_state.borrow() == "syncing" {
            "syncing"
        } else {
            "ready"
        };
        let mut status = json!({
            "state": state,
            "ready": state == "ready",
            "bootstrap": self.bootstrap_progress().await?,
            "revision": revision.to_string(),
            "generation": generation,
            "authority": {
                "revocations": settings.revocation_status()?,
                "expires": settings.expires().to_string(),
                "expired": expired,
                "logged_out": logged_out,
                "revoked": revocation.is_some(),
                "profile": settings.profile,
                "issuer_contact": settings.issuer_contact(),
            },
            "key_epochs": settings.keys.iter().map(|(collection, epochs)| json!({"collection":collection,"epochs":epochs,"current":epochs.iter().max()})).collect::<Vec<_>>(),
            "tickets": tickets,
            "network": {"attached": self.current().is_some(), "discovery": self.discovery_status(), "peers": self.peers().await, "link_revision": self.link_revision.load(Ordering::SeqCst).to_string()},
        });
        let mut token_status = status.clone();
        if let Some(peers) = token_status["network"]["peers"].as_array_mut() {
            // Path measurements change continuously and must not move the token.
            for peer in peers {
                peer["link"]["round_trip_ms"] = Value::Null;
                peer["link"]["paths"] = json!([]);
            }
        }
        let token = HEXLOWER
            .encode(&commitment("rocky.sdk.status.v1", token_status.to_string().as_bytes())[..16]);
        status["token"] = json!(token);
        Ok(status)
    }
    /// How far the bootstrap intent has imported, in the stored rows of the
    /// checkpoints its peer offered; `null` until a peer offered one.
    async fn bootstrap_progress(&self) -> Result<Value, Failure> {
        let intent: Value = match std::fs::read(self.path.with_file_name("bootstrap.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
            Err(error) => return Err(error.into()),
        };
        let Some(totals) = intent.get("totals") else {
            return Ok(Value::Null);
        };
        let totals: Vec<sync::Planned> = serde_json::from_value(totals.clone())?;
        if totals.is_empty() {
            return Ok(Value::Null);
        }
        let pending: Vec<ResourceScope> = serde_json::from_value(intent["pending"].clone())?;
        let (mut rows_done, mut scopes_done) = (0, 0);
        for planned in &totals {
            if pending.contains(&planned.scope) {
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
        Ok(json!({
            "rows_done": rows_done,
            "rows_total": totals.iter().map(|planned| planned.rows).sum::<u64>(),
            "scopes_done": scopes_done,
            "scopes_total": totals.len(),
        }))
    }
    /// The status once it differs from the one `after` named.
    async fn readiness_after(&self, client: &Client, after: &str) -> Result<Value, Failure> {
        let mut revisions = self.replica.observe_revision();
        let mut imports = self.replica.observe_imports().await;
        let mut sync_state = self.sync_state.subscribe();
        let mut closing = client.closing.subscribe();
        let mut events = self.sync_events.subscribe();
        let mut network = self.sync_status.subscribe();
        loop {
            let status = self.readiness().await?;
            if status["token"] != after {
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
                changed = imports.changed() => {
                    changed.map_err(|_| ServiceError::Closed("database"))?
                }
                _ = async {
                    let expires = status["authority"]["expires"].as_str().ok_or("missing expiry")?.parse::<u64>()?;
                    let time = now()?;
                    if expires > time {
                        tokio::time::sleep(Duration::from_secs(expires - time)).await;
                    } else {
                        std::future::pending().await
                    }
                    Ok::<_, Failure>(())
                } => {},
            }
        }
    }
    /// Writes a signed backup archive to `target`: a coherent copy of the
    /// file, the retained receipt evidence and the identity, which holds no
    /// key. See `docs/runtime/backup-and-restore.md`.
    async fn backup(&self, target: &std::path::Path) -> Result<Value, Failure> {
        let _backup = self.backups.lock().await;
        if target.exists() {
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
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let result = async {
            let settings = self.settings.lock().await;
            let database = staging.join("rocky.sqlite");
            let revision = self.replica.backup_into(&database).await?;
            let snapshot = settings.clone();
            drop(settings);
            let settings = snapshot;
            let identity = staging.join("identity.json");
            std::fs::write(&identity, serde_json::to_vec(&*settings)?)?;
            let mut staged = vec![("identity.json", identity), ("rocky.sqlite", database)];
            let evidence = directory.join("coordinator.evidence");
            if evidence.exists() {
                let copy = staging.join("coordinator.evidence");
                std::fs::copy(&evidence, &copy)?;
                staged.push(("coordinator.evidence", copy));
            }
            let node = PrivateKey::from_bytes(&self.vault.get("node")?);
            let scope = serde_json::to_value(self.domain.scope)?;
            let target = target.to_path_buf();
            let inventory = tokio::task::spawn_blocking(move || {
                crate::backup::write(&target, &staged, &settings, scope, revision, &node, now()?)
            })
            .await??;
            Ok::<_, Failure>(serde_json::to_value(inventory)?)
        }
        .await;
        let _ = std::fs::remove_dir_all(&staging);
        result
    }
    /// The definition generation, the earlier ones still admitted and a
    /// table rebuild under way.
    async fn generation(&self) -> Result<Value, Failure> {
        let status = self.replica.generations().await?;
        let generation = |generation: rocky_core::Generation| json!({"schema": generation.schema, "handler": generation.handler});
        Ok(json!({
            "manifest": HEXLOWER.encode(&status.manifest),
            "current": generation(status.current),
            "horizon": status.horizon,
            "previous": status.previous.iter().map(|(previous, _)| generation(*previous)).collect::<Vec<_>>(),
            "rebuilding": status.plan.map(|plan| json!({"filled": plan.filled, "tables": plan.tables})),
        }))
    }
    /// Revokes a device and rotates the keys it could read. The intent,
    /// the new keys and the identity are saved before the replica learns
    /// them, so an interruption completes at the next open.
    async fn revoke(&self, node: PublicKey) -> Result<Value, Failure> {
        let registry = self.registry().await?;
        let mut settings = self.settings.lock().await;
        let mut draft = settings.clone();
        draft
            .begin_revocation(node, &registry)
            .map_err(|error| -> Failure {
                if error.is::<crate::enrollment::RevocationSetFull>() {
                    error
                } else {
                    refused(error)
                }
            })?;
        draft.save(&self.path)?;
        *settings = draft.clone();
        let rotation = draft
            .authority
            .as_ref()
            .and_then(|authority| authority.rotation.clone())
            .ok_or("the revocation was not begun")?;
        let keys = draft.rotate(&self.vault)?.unwrap_or_default();
        draft.drop_revoked_deliveries();
        draft.save(&self.path)?;
        *settings = draft;
        self.apply(&settings.revocations, &settings.credentials, keys)
            .await?;
        for (key, credentials) in settings.row_authors(&self.vault)? {
            self.replica.register_row_author(key, credentials).await?;
        }
        for partition in settings.partitions(&self.domain)? {
            let address = crate::groups::record_key(&partition.record.address);
            if rotation.groups.contains_key(&address) {
                self.hold_keys(&settings, &partition).await?;
            }
        }
        let status = settings.revocation_status()?;
        drop(settings);
        self.forget_readers().await;
        // Rotated first: the statement refuses the device even if this fails.
        Box::pin(self.complete_revocation()).await?;
        Ok(json!({
            "statement": HEXLOWER.encode(&rotation.statement.id()?),
            "revocations": status,
        }))
    }
    /// Teaches the replica what the identity already records: the
    /// revocations, this device's chain and new key epochs, earlier ones
    /// first.
    async fn apply(
        &self,
        revocations: &RevocationSet,
        chain: &CredentialChain,
        mut keys: Vec<(String, rocky_core::KeyEpoch, [u8; 32])>,
    ) -> Result<(), Failure> {
        self.replica.learn_revocations(revocations).await?;
        self.replica.renew_credentials(chain.clone()).await?;
        keys.sort_by_key(|(_, epoch, _)| *epoch);
        for (name, generation, key) in keys {
            let scope = self
                .domain
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
    /// Renews this device's credentials: from its issuers on a member,
    /// self-signed on an issuer, which asks the other issuers first when
    /// `learn` is set. New reader keys arrive with the renewal. Returns
    /// whether anything changed.
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
                    current.record_revocation(&self.path, revoked.0.clone())?;
                    let node = current
                        .credentials
                        .0
                        .last()
                        .ok_or("missing device credential")?
                        .capability
                        .subject;
                    self.activity
                        .record("device_revoked", json!({"node": node.to_hex()}));
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
        if !epochs_only && settings.lacks(&renewed.revocations)? {
            rocky_credentials::write_private(&self.path.with_file_name("bootstrap.json"), b"true")?;
        }
        if epochs_only {
            renewed.chain = settings.credentials.clone();
        }
        let (keys, adopted) = settings.adopt(&self.path, &self.vault, &renewed)?;
        let mut changed = !epochs_only || adopted;
        if changed {
            self.apply(&settings.revocations, &settings.credentials, keys)
                .await?;
            for partition in settings.partitions(&self.domain)? {
                self.hold_keys(&settings, &partition).await?;
            }
        }
        if settings.issuer() {
            drop(settings);
            let registry = self.registry().await?;
            settings = self.settings.lock().await;
            if settings.begin_exposure_rotation(&registry)? {
                changed = true;
                settings.save(&self.path)?;
                let keys = settings
                    .rotate(&self.vault)?
                    .ok_or("exposure rotation intent disappeared")?;
                settings.save(&self.path)?;
                self.apply(&settings.revocations, &settings.credentials, keys)
                    .await?;
                for partition in settings.partitions(&self.domain)? {
                    self.hold_keys(&settings, &partition).await?;
                }
            }
        }
        if !changed {
            return Ok(false);
        }
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? =
            settings.granted(&self.domain);
        drop(settings);
        self.refresh_importing().await?;
        self.forget_readers().await;
        if self.settings.lock().await.issuer() {
            Box::pin(self.complete_revocation()).await?;
        }
        // An issuer's own record names its current capability.
        if !epochs_only && let Err(error) = Box::pin(self.record_self()).await {
            self.activity
                .record("registry_failed", json!({"error": error.to_string()}));
        }
        self.wake_sync();
        Ok(true)
    }
    async fn logout(&self, purge: bool) -> Result<Value, Failure> {
        let registry = self.registry().await?;
        let own = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let mut settings = self.settings.lock().await;
        if settings.issuer() && !settings.another_issuer(&registry, own) {
            return Err(refused(
                "this device holds the only known copy of the domain's publisher key and cannot log out",
            ));
        }
        self.replica.logout().await;
        settings.logged_out = true;
        settings.save(&self.path)?;
        *self.ended.lock().await = Some(purge);
        self.forget_readers().await;
        for client in self.clients() {
            client.subscriptions()?.clear();
            client.subscriptions_changed.send_replace(());
        }
        self.automatic.store(false, Ordering::SeqCst);
        self.wake_sync();
        self.vault.forget(settings.roles())?;
        Ok(json!({"logged_out": true, "purge": purge}))
    }

    pub(crate) async fn initial_sync(&self) -> Result<(), Failure> {
        let network = self.network.read().await;
        let link = network
            .as_ref()
            .ok_or(ServiceError::NetworkRequired("bootstrap"))?;
        let mut stage = link.stage.subscribe();
        tokio::select! {
            result = self.sync(&*link.transport) => result.map(|_| ()),
            _ = stage.wait_for(|stage| *stage == Stage::Cancelled) => Err(ServiceError::Closed("network service").into()),
        }
    }

    /// Installs a pseudonym for every collection this device writes and has
    /// none for: an issuer assigns its own, a member asks its issuers.
    async fn authors(&self, transport: Option<&dyn Transport>) -> Result<Value, Failure> {
        let missing = self.settings.lock().await.missing_authors(&self.domain)?;
        let mut installed = Vec::new();
        for collection in missing {
            let settings = self.settings.lock().await.clone();
            let request = settings.pseudonym_request(&self.vault, &collection)?;
            let chain = if settings.issuer() {
                Box::pin(self.answer_author(&request)).await?
            } else {
                let transport = transport.ok_or(ServiceError::NetworkRequired("authors"))?;
                settings.request_author(transport, &request).await?
            };
            let (collection, author) = self
                .configure(|settings| settings.hold_pseudonym(&self.vault, chain.clone()))
                .await?;
            let key = PrivateKey::from_bytes(&self.vault.get(&format!("pseudonym.{collection}"))?);
            self.replica.register_row_author(key, chain).await?;
            self.activity.record(
                "author_installed",
                json!({"collection": collection, "author": author}),
            );
            installed.push(json!({"collection": collection, "author": author}));
        }
        Ok(json!({"installed": installed}))
    }
    /// Records a peer's reachability when it changed and what a sync moved.
    fn record_reach(
        &self,
        peer: PublicKey,
        before: Option<Reached>,
        result: &Result<(bool, bool, sync::Transfer), Failure>,
    ) {
        let peer_hex = peer.to_hex();
        let (before, refused_before, waiting_before, results_before) = match before {
            Some(reached) => (
                Some(reached.error),
                reached.refused,
                reached.waiting,
                reached.results,
            ),
            None => (None, Vec::new(), Vec::new(), None),
        };
        match result {
            Err(error) => {
                let error = error.to_string();
                if before.flatten().as_ref() != Some(&error) {
                    self.activity.record(
                        "peer_unreachable",
                        json!({"peer": peer_hex, "error": error}),
                    );
                }
            }
            Ok((_, _, transfer)) => {
                if !matches!(before, Some(None)) {
                    self.activity
                        .record("peer_reached", json!({"peer": peer_hex}));
                }
                // Recorded when the set changes, not on every sync.
                if transfer.refused != refused_before {
                    let scopes: Vec<Value> = transfer
                        .refused
                        .iter()
                        .map(|scope| self.scope_label(scope))
                        .collect();
                    self.activity.record(
                        "scopes_refused",
                        json!({"peer": peer_hex, "scopes": scopes}),
                    );
                }
                if transfer.waiting != waiting_before {
                    let scopes: Vec<Value> = transfer
                        .waiting
                        .iter()
                        .map(|scope| self.scope_label(scope))
                        .collect();
                    self.activity.record(
                        "scopes_waiting",
                        json!({"peer": peer_hex, "scopes": scopes}),
                    );
                }
                // Rows synced; confirmed results did not. Recorded when the error changes.
                if let Some(error) = &transfer.results
                    && transfer.results != results_before
                {
                    self.activity
                        .record("results_failed", json!({"peer": peer_hex, "error": error}));
                }
                if transfer.scopes.is_empty() {
                    return;
                }
                let scopes: Vec<Value> = transfer
                    .scopes
                    .iter()
                    .map(|(scope, moved)| {
                        let mut label = self.scope_label(scope);
                        label["pulled"] = json!(moved.pulled);
                        label["pushed"] = json!(moved.pushed);
                        label["bytes"] = json!(moved.bytes);
                        label
                    })
                    .collect();
                let (pulled, pushed) = transfer
                    .scopes
                    .iter()
                    .fold((0, 0), |(pulled, pushed), (_, moved)| {
                        (pulled + moved.pulled, pushed + moved.pushed)
                    });
                if pulled + pushed > 0 {
                    self.activity.record(
                        "synced",
                        json!({"peer": peer_hex, "pulled": pulled, "pushed": pushed, "scopes": scopes}),
                    );
                }
                for (scope, moved) in &transfer.scopes {
                    if let Some((checkpoint, superseded)) = moved.checkpoint {
                        let mut event = self.scope_label(scope);
                        event["peer"] = json!(peer_hex);
                        event["checkpoint"] = json!(HEXLOWER.encode(&checkpoint));
                        event["superseded"] = json!(superseded);
                        self.activity.record("checkpoint_imported", event);
                    }
                    if moved.finalized {
                        self.activity
                            .record("horizon_finalized", self.scope_label(scope));
                    }
                }
            }
        }
    }
    /// A scope as events name it: its table, and for a partition its
    /// encryption commitment, which `partition_address` also reports.
    fn scope_label(&self, scope: &ResourceScope) -> Value {
        let own = self
            .domain
            .scopes()
            .find(|(_, own)| own.table == scope.table);
        json!({
            "table": own.map(|(name, _)| name),
            "partition": own.is_some_and(|(_, own)| own != *scope).then(|| HEXLOWER.encode(&scope.encryption)),
        })
    }
    fn wake_sync(&self) {
        self.sync_events
            .send_modify(|event| *event = event.wrapping_add(1));
    }
    fn synced(&self, mut status: Value) {
        self.sync_status.send_modify(|current| {
            let token = current["token"]
                .as_str()
                .and_then(|token| token.parse::<u64>().ok())
                .unwrap_or(0);
            status["token"] = json!(token.wrapping_add(1).to_string());
            *current = status;
        });
    }
    fn control_sync(&self) {
        self.sync_control
            .send_modify(|event| *event = event.wrapping_add(1));
        self.wake_sync();
    }
    fn reset_retry(&self, peer: Option<PublicKey>) {
        let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        for (node, state) in retries.iter_mut() {
            if peer.is_none_or(|peer| peer == *node) {
                state.retry.reset();
                state.link_retry.reset();
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
            state.pending = true;
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
                state.pending = true;
            }
        }
    }
    fn retry_deadline(&self) -> Option<tokio::time::Instant> {
        self.retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .flat_map(|state| {
                [
                    if state.pending {
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
            .min()
    }
    /// One worker per link; only pending work has a retry timer.
    async fn synchronize(self: Arc<Self>, link: Arc<Link>) {
        let mut stage = link.stage.subscribe();
        let mut events = self.sync_events.subscribe();
        let mut control = self.sync_control.subscribe();
        let mut frames = self.replica.observe_frames().await;
        let mut revisions = self.replica.observe_revision();
        let mut connectivity = link.transport.connectivity();
        let mut link_events = self.link_events.subscribe();
        let mut retry_only = false;
        loop {
            if *stage.borrow() != Stage::Open {
                break;
            }
            control.borrow_and_update();
            // Another watch may have won the select while explicit work was queued.
            if events.has_changed().unwrap_or(false) {
                self.pending_sync();
                retry_only = false;
            }
            events.borrow_and_update();
            if self.automatic.load(Ordering::SeqCst) && self.online.load(Ordering::SeqCst) {
                self.worker_running.send_replace(true);
                let running = WorkerActivity(self.worker_running.clone());
                let result = tokio::select! {
                    biased;
                    _ = stage.wait_for(|stage| *stage != Stage::Open) => break,
                    _ = control.changed() => continue,
                    result = async {
                        let attached = self.network.read().await;
                        if attached.as_ref().is_none_or(|current| current.id != link.id) {
                            return Ok((json!({"state": "idle"}), false));
                        }
                        let result = self.sync_pass(&*link.transport, true, retry_only).await;
                        self.open_links(&link).await;
                        result
                    } => result,
                };
                drop(running);
                self.topics_changed();
                let status = match result {
                    Ok((mut status, retry)) => {
                        status["retry"] = json!(retry);
                        status
                    }
                    Err(error) if error.is::<crate::enrollment::Revoked>() => {
                        json!({"state": "revoked", "error": error.to_string(), "retry": false})
                    }
                    Err(error) => {
                        json!({"state": "unavailable", "error": error.to_string(), "retry": self.retry_deadline().is_some()})
                    }
                };
                self.synced(status);
            } else if !self.online.load(Ordering::SeqCst) {
                self.synced(json!({"state": "offline", "retry": false}));
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
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                }, if self.automatic.load(Ordering::SeqCst) && self.online.load(Ordering::SeqCst) => { retry_only = true; }
            }
        }
    }

    /// Syncs with every configured peer. One reachable peer is enough;
    /// what each attempt did is kept for `peers` and `network`.
    async fn sync(&self, transport: &dyn Transport) -> Result<(Value, bool), Failure> {
        let result = self.sync_pass(transport, false, false).await;
        self.topics_changed();
        result
    }
    async fn sync_pass(
        &self,
        transport: &dyn Transport,
        automatic: bool,
        retry_only: bool,
    ) -> Result<(Value, bool), Failure> {
        if let Some(statement) = self.settings.lock().await.revocation(&self.path)? {
            return Err(Box::new(crate::enrollment::Revoked(statement)));
        }
        if !self.online.load(Ordering::SeqCst) {
            return Ok((json!({"state": "offline"}), false));
        }
        if automatic
            && retry_only
            && {
                let targets = self.dialed(&*self.settings.lock().await);
                let retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
                targets.iter().all(|(peer, _)| retries.contains_key(peer))
                    && !retries
                        .values()
                        .any(|state| state.pending && state.retry.ready())
            }
            && self
                .resolution_retries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        {
            return Ok((
                self.sync_status.borrow().clone(),
                self.retry_deadline().is_some(),
            ));
        }
        let _syncing = self.syncing.lock().await;
        let _activity = SyncActivity(self.sync_state.clone());
        self.sync_state.send_replace("syncing");
        Box::pin(self.resolve_saved_contacts(transport)).await;
        let saved = self.settings.lock().await.clone();
        if !automatic && self.dialed(&saved).is_empty() && !saved.unresolved_contacts().is_empty() {
            if saved
                .unresolved_contacts()
                .iter()
                .all(|(_, contact)| contact.addresses(transport.kind()).is_empty())
            {
                return Err(ServiceError::NoRoute {
                    peer: "a saved peer",
                    network: transport.kind().into(),
                }
                .into());
            }
            return Err(
                ServiceError::Unreachable("saved contacts could not be resolved".into()).into(),
            );
        }
        let settings = self.settings.lock().await.clone();
        let member = !settings.issuer();
        if settings.expires() < now()?.saturating_add(settings.ttl / 2) {
            let renewed = self.renew(Some(transport), false, None, false).await;
            if let Err(error) = &renewed {
                self.activity
                    .record("renewal_failed", json!({"error": error.to_string()}));
            }
            if !member {
                renewed?;
            } else if renewed.is_err() && settings.expires() <= now()? {
                return Err("credentials expired; an issuer must be reachable to renew".into());
            }
        }
        // A member that joined while no issuer answered asks again here.
        if member
            && !self
                .settings
                .lock()
                .await
                .missing_authors(&self.domain)?
                .is_empty()
        {
            let _ = self.authors(Some(transport)).await;
        }
        let renewable = member || !settings.issuers.is_empty();
        // A served session brought statements: renew for the rotated keys
        // before moving rows.
        if renewable && self.learned.load(Ordering::SeqCst) {
            let _ = self.renew(Some(transport), true, None, true).await;
            if let Some(statement) = self.settings.lock().await.revocation(&self.path)? {
                return Err(Box::new(crate::enrollment::Revoked(statement)));
            }
        }
        let targets = self.dialed(&*self.settings.lock().await);
        self.replica.configure_peers(settings.kept()).await;
        let needs_initial = {
            let retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
            targets.iter().any(|(peer, _)| !retries.contains_key(peer))
        };
        let mut has_frames = false;
        if needs_initial {
            let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
            let mut after = None;
            loop {
                let (scopes, next) = self
                    .replica
                    .held_scopes(node, &settings.credentials, after)
                    .await?;
                for scope in scopes {
                    has_frames |= !self.replica.frontier(scope).await?.logs.is_empty();
                }
                if has_frames || next.is_none() {
                    break;
                }
                after = next;
            }
        }
        let has_contacts = !targets.is_empty();
        let targets: Vec<_> = {
            let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
            retries.retain(|peer, _| targets.iter().any(|(node, _)| node == peer));
            targets
                .into_iter()
                .filter_map(|(peer, contact)| {
                    let state = retries.entry(peer).or_insert_with(|| PeerRetry {
                        pending: has_frames,
                        ..PeerRetry::default()
                    });
                    (state.retry.ready()
                        && (!automatic || !retry_only || state.pending)
                        && (!automatic
                            || !self
                                .current()
                                .is_some_and(|service| service.live.inbound_short(peer))))
                    .then_some((peer, contact, state.generation))
                })
                .collect()
        };
        if !automatic && has_contacts && targets.is_empty() {
            return Err(
                ServiceError::Unreachable("all selected contacts are in backoff".into()).into(),
            );
        }
        if automatic && targets.is_empty() {
            return Ok((
                self.sync_status.borrow().clone(),
                self.retry_deadline().is_some(),
            ));
        }
        let needed: Vec<_> = self
            .scopes()
            .into_iter()
            .filter(|scope| !self.domain.partitioned_table(scope))
            .collect();
        // A pass that failed after importing checkpoint pages goes on soon.
        let imports = self.replica.observe_imports().await;
        let imported = *imports.borrow();
        let mut covered = std::collections::BTreeSet::new();
        let mut targets = targets;
        let mut results = Vec::new();
        for round in 0..2 {
            let local_first = targets.first().is_some_and(|(peer, _, _)| {
                settings
                    .contact_meta
                    .get(peer)
                    .is_some_and(|meta| meta.local)
            });
            let mut remaining = targets.into_iter();
            let mut first_batch = true;
            loop {
                let width = if local_first && first_batch { 1 } else { 4 };
                first_batch = false;
                let batch: Vec<_> = remaining.by_ref().take(width).collect();
                if batch.is_empty() {
                    break;
                }
                if !needed.is_empty() && needed.iter().all(|scope| covered.contains(scope)) {
                    let mut choices = self.choices.lock().unwrap_or_else(PoisonError::into_inner);
                    for (peer, _, _) in batch.into_iter().chain(remaining) {
                        choices.insert(peer, "covered");
                    }
                    break;
                }
                let attempts = stream::iter(batch)
                    .map(|(peer, contact, generation)| async move {
                        let mut result = self.reconcile(transport, peer, &contact).await;
                        let settings = self.settings.lock().await.clone();
                        if settings.issuer()
                            && settings.issuers.contains(&peer)
                            && !result
                                .as_ref()
                                .is_err_and(|error| error.is::<ServiceError>())
                        {
                            match self.renew(Some(transport), true, Some(peer), true).await {
                                Ok(true) => {
                                    result = self.reconcile(transport, peer, &contact).await;
                                }
                                Ok(false) => {}
                                Err(error) => self.activity.record_changed(
                                    "epoch_delivery_failed",
                                    json!({"peer": peer.to_hex(), "error": error.to_string()}),
                                ),
                            }
                        }
                        (peer, contact, generation, result)
                    })
                    .buffer_unordered(4);
                tokio::pin!(attempts);
                while let Some((peer, contact, generation, result)) = attempts.next().await {
                    let error = result.as_ref().err().map(ToString::to_string);
                    let (refused, waiting, received) = match &result {
                        Ok((_, _, transfer)) => (
                            transfer.refused.clone(),
                            transfer.waiting.clone(),
                            transfer.results.clone(),
                        ),
                        Err(_) => (Vec::new(), Vec::new(), None),
                    };
                    let before = self.reached()?.insert(
                        peer,
                        Reached {
                            at: now()?,
                            error,
                            refused,
                            waiting,
                            held: result
                                .as_ref()
                                .map(|(_, _, transfer)| transfer.held.clone())
                                .unwrap_or_default(),
                            results: received,
                        },
                    );
                    if let Ok((_, _, transfer)) = &result {
                        covered.extend(transfer.held.iter().copied());
                    }
                    self.record_reach(peer, before, &result);
                    results.push((peer, contact, generation, result));
                }
            }
            // A peer told this device of its own revocation.
            if let Some(statement) = self.settings.lock().await.revocation(&self.path)? {
                return Err(Box::new(crate::enrollment::Revoked(statement)));
            }
            let failed = |result: &Result<(bool, bool, sync::Transfer), Failure>| {
                result
                    .as_ref()
                    .is_err_and(|error| !error.is::<ServiceError>())
            };
            // Renewing cannot repair a broken or stalled stream, or a replaced
            // checkpoint, and would cancel other active links.
            let renewal_peer = results
                .iter()
                .filter(|(_, _, _, result)| {
                    failed(result)
                        && !result.as_ref().is_err_and(|error| {
                            matches!(error.downcast_ref::<Error>(), Some(Error::Io(_)))
                                || matches!(error.downcast_ref::<Error>(), Some(Error::Rejected(reason)) if reason.starts_with("link ") || reason.ends_with(" timeout") || reason.starts_with("checkpoint superseded"))
                        })
                })
                .min_by_key(|(peer, _, _, _)| !settings.issuers.contains(peer))
                .map(|(peer, _, _, _)| *peer);
            if round == 0
                && renewable
                && let Some(peer) = renewal_peer
            {
                // Finish old-authority sessions before renewing once. Even a successful
                // old handshake must be checked again against the learned revocations.
                let renewed = self
                    .renew(Some(transport), true, Some(peer), false)
                    .await
                    .is_ok();
                if let Some(statement) = self.settings.lock().await.revocation(&self.path)? {
                    return Err(Box::new(crate::enrollment::Revoked(statement)));
                }
                // Without a renewal, a sync that stopped for learned
                // statements still moves its rows; new keys wait.
                let learned = |result: &Result<(bool, bool, sync::Transfer), Failure>| {
                    result
                        .as_ref()
                        .is_err_and(|error| error.is::<sync::Learned>())
                };
                if renewed || results.iter().any(|(_, _, _, result)| learned(result)) {
                    let admitted: std::collections::BTreeSet<_> = self
                        .settings
                        .lock()
                        .await
                        .targets()
                        .into_iter()
                        .map(|(peer, _)| peer)
                        .collect();
                    results.retain(|(peer, _, _, _)| admitted.contains(peer));
                    // Coverage from the previous authority cannot skip its replacement pass.
                    covered.clear();
                    let again = |result: &Result<_, Failure>| match renewed {
                        true => result.is_ok() || failed(result),
                        false => learned(result),
                    };
                    targets = results
                        .iter()
                        .filter(|(_, _, _, result)| again(result))
                        .map(|(peer, contact, generation, _)| (*peer, contact.clone(), *generation))
                        .collect();
                    results.retain(|(_, _, _, result)| !again(result));
                    continue;
                }
            }
            break;
        }
        let (mut connected, mut first, mut newer, mut retry) = (0, None, false, false);
        for (peer, _, generation, result) in results {
            {
                let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(state) = retries.get_mut(&peer)
                    && state.generation == generation
                {
                    match &result {
                        Ok((_, false, _)) => {
                            state.retry.reset();
                            state.pending = false;
                        }
                        Ok((_, true, _)) => {
                            state.pending = true;
                            state.retry.failed();
                        }
                        Err(_) if *imports.borrow() != imported => {
                            state.pending = true;
                            state.retry.soon();
                        }
                        Err(_) => state.retry.failed(),
                    }
                }
            }
            match result {
                Ok((behind, pending, _)) => {
                    connected += 1;
                    newer |= behind;
                    retry |= pending;
                }
                Err(error) => {
                    first.get_or_insert(error);
                }
            }
        }
        retry |= first.is_some();
        match first {
            Some(error) if connected == 0 => Err(error),
            // A peer runs a newer generation: local changes wait for an upgrade.
            _ if newer => Ok((
                json!({"state": "connected", "peers": connected, "upgrade": true}),
                retry,
            )),
            _ => Ok((json!({"state": "connected", "peers": connected}), retry)),
        }
    }
    /// Local routes, observed holdings and trusted contacts, with backed-off peers last.
    fn dialed(&self, settings: &Settings) -> Vec<(PublicKey, Contact)> {
        let reached = self.reached.lock().unwrap_or_else(PoisonError::into_inner);
        let retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        let needed: Vec<_> = self
            .scopes()
            .into_iter()
            .filter(|scope| !self.domain.partitioned_table(scope))
            .collect();
        let mut targets = settings.targets();
        targets.sort_by_key(|(node, _)| {
            let meta = settings.contact_meta.get(node);
            (
                retries.get(node).is_some_and(|state| !state.retry.ready()),
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
            let reason = if retries.get(node).is_some_and(|state| !state.retry.ready()) {
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
    /// Keeps the statements the replica learned in sessions in this
    /// identity. One that revokes this device, which the replica does not
    /// keep, is recorded as after a refused renewal; rows of a newly revoked
    /// author arrive by checkpoint.
    async fn absorb_revocations(&self, outcome: Option<&Failure>) -> Result<(), Failure> {
        let held = self.replica.revocations().await;
        let revoking = outcome.and_then(revoked_by);
        let mut settings = self.settings.lock().await;
        if !settings.lacks(&held)? && revoking.is_none() {
            return Ok(());
        }
        let mut draft = settings.clone();
        if draft.lacks(&held)? {
            draft.learn_revocations(&held)?;
            draft.forget_revoked();
            draft.drop_revoked_deliveries();
            self.learned.store(true, Ordering::SeqCst);
            // Issuers retain sibling epochs; members restore newly revoked authors by checkpoint.
            if !draft.issuer() {
                rocky_credentials::write_private(
                    &self.path.with_file_name("bootstrap.json"),
                    b"true",
                )?;
            }
            draft.save(&self.path)?;
        }
        if let Some(statement) = revoking
            && draft.revocation(&self.path)?.is_none()
        {
            draft.record_revocation(&self.path, statement)?;
            let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
            self.activity
                .record("device_revoked", json!({"node": node.to_hex()}));
        }
        *settings = draft;
        drop(settings);
        self.refresh_importing().await?;
        self.sweep_deliveries().await
    }
    /// The scopes a bootstrap intent still imports from a checkpoint.
    async fn bootstrap_pending(&self, intent: &Value) -> Result<Vec<ResourceScope>, Failure> {
        let token = intent.get("intent").unwrap_or(intent);
        if token != &json!(true) {
            return Err(refused("invalid bootstrap authority intent"));
        }
        let pending: Vec<ResourceScope> = match intent.get("pending") {
            Some(pending) => serde_json::from_value(pending.clone())?,
            None if intent == &json!(true) => self.scopes(),
            None => return Err(refused("invalid bootstrap intent")),
        };
        if pending.iter().any(|scope| !self.scopes().contains(scope)) {
            return Err(refused("bootstrap intent names an ungranted scope"));
        }
        // Keyless custody scopes cannot import a checkpoint; they sync rows.
        // Nor can a partitioned table's own scope, which holds no rows.
        let settings = self.settings.lock().await;
        Ok(pending
            .into_iter()
            .filter(|scope| {
                settings.reads_whole(&self.domain, scope) && !self.domain.partitioned_table(scope)
            })
            .collect())
    }
    /// Tells the replica which scopes the bootstrap intent imports, so
    /// peers hold back their history for them.
    pub(crate) async fn refresh_importing(&self) -> Result<(), Failure> {
        // An intent the bootstrap itself refuses holds nothing back; the
        // bootstrap reports it.
        let pending = match std::fs::read(self.path.with_file_name("bootstrap.json")) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(intent) => self.bootstrap_pending(&intent).await.unwrap_or_default(),
                Err(_) => Vec::new(),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        self.replica
            .set_importing(pending.into_iter().collect())
            .await;
        Ok(())
    }
    /// Whether a staged import of `signer`'s checkpoint, at `cursor`, goes on
    /// with that signer rather than restarting from another peer's
    /// checkpoint: while the signer is linked and the import moved within
    /// `IMPORT_SOURCE`.
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
        let now = tokio::time::Instant::now();
        let seen = sources.entry(scope).or_insert((cursor, now));
        if seen.0 != cursor {
            *seen = (cursor, now);
        }
        now.duration_since(seen.1) < IMPORT_SOURCE
    }
    /// Syncs with one peer, then keeps the revocations its sessions brought.
    async fn reconcile(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
    ) -> Result<(bool, bool, sync::Transfer), Failure> {
        let mut result = Err("link authority kept changing during reconciliation".into());
        for _ in 0..4 {
            let revocations = self.replica.revocation_digest().await?;
            result = self.exchange(transport, peer, contact).await;
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
        let transfer = result?;
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
        Ok(transfer)
    }
    pub(super) async fn request_link(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
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
            let stream = crate::enrollment::dial(transport, &addresses, Protocol::Link).await?;
            let owner = self
                .owner
                .get()
                .and_then(std::sync::Weak::upgrade)
                .ok_or("network owner ended")?;
            let link_settings = self.settings.lock().await.clone();
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
                    self.absorb_revocations(Some(&error)).await?;
                    continue;
                }
                // A refusal, a full link budget included, backs off; nothing else syncs.
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
    async fn exchange(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
    ) -> Result<(bool, bool, sync::Transfer), Failure> {
        let service = self
            .current()
            .ok_or(ServiceError::NetworkRequired("sync"))?;
        let revocations = self.replica.revocation_digest().await?;
        let link = self.request_link(transport, peer, contact).await?;
        let _short = service.live.short(peer);
        let renewable = {
            let settings = self.settings.lock().await;
            !settings.issuer() || !settings.issuers.is_empty()
        };
        let keep = |scope, signer, cursor| self.keeps_import_source(scope, signer, cursor);
        let upstream = Upstream {
            link,
            peer,
            contacts: Some(self),
            renew_from: match renewable {
                true => Some(revocations),
                false => None,
            },
            keep: Some(&keep),
        };
        let reconcile = upstream.link.begin_reconcile().await?;
        let coordinator = self
            .coordinator
            .as_deref()
            .filter(|_| !self.domain.app.commands().is_empty());
        // Envelopes first, on the exchange's first session: a list a
        // delivery opens syncs in this exchange. A stalled dial must not
        // hold the shared checkpoint import slot.
        let mut first = Some(self.deliver(&upstream).await?);
        let marker = self.path.with_file_name("bootstrap.json");
        let mut bootstrapped = sync::Transfer::default();
        if marker.exists() {
            let _bootstrap = self.bootstrap.lock().await;
            if marker.exists() {
                self.sync_state.send_replace("bootstrapping");
                let expected = std::fs::read(&marker)?;
                let pending = self
                    .bootstrap_pending(&serde_json::from_slice(&expected)?)
                    .await?;
                self.replica
                    .set_importing(pending.iter().copied().collect())
                    .await;
                self.activity.record_changed(
                    "bootstrap_started",
                    json!({"peer": peer.to_hex(), "scopes": pending.len()}),
                );
                // `None`: this peer offers no checkpoint; intent stays for another.
                if let Some(completed) = sync::bootstrap(
                    &self.replica,
                    &upstream,
                    &pending,
                    &marker,
                    expected,
                    &mut bootstrapped,
                    first.take(),
                )
                .await?
                {
                    self.replica.flush().await?;
                    if std::fs::read(&marker)? != completed {
                        return Err("bootstrap authority changed; retry with the new intent".into());
                    }
                    std::fs::remove_file(&marker)?;
                    self.replica
                        .set_importing(std::collections::BTreeSet::new())
                        .await;
                    #[cfg(unix)]
                    std::fs::File::open(
                        marker.parent().ok_or("bootstrap marker has no directory")?,
                    )?
                    .sync_all()?;
                    self.activity
                        .record("bootstrap_finished", json!({"peer": peer.to_hex()}));
                }
            }
        }
        self.sync_state.send_replace("syncing");
        // Table roots remain in the SQL catalog; only concrete holdings sync.
        let scopes: Vec<_> = self
            .scopes()
            .into_iter()
            .filter(|scope| !self.domain.partitioned_table(scope))
            .collect();
        let (newer, retry, transfer) = sync::reconcile(
            &self.replica,
            &upstream,
            &scopes,
            coordinator,
            |bootstrap| {
                self.sync_state.send_replace(if bootstrap {
                    "bootstrapping"
                } else {
                    "syncing"
                });
            },
            first,
        )
        .await?;
        bootstrapped.extend(transfer);
        reconcile.close().await?;
        upstream.link.drain().await?;
        Ok((newer, retry, bootstrapped))
    }
    /// Serves one inbound one-shot stream: pairing and renewal on an issuer,
    /// and commands. Synchronization runs on live links.
    pub async fn serve(&self, protocol: Protocol, stream: Stream) -> Result<(), Failure> {
        if !self.online.load(Ordering::SeqCst) || self.ended.lock().await.is_some() {
            return Err("this node is offline".into());
        }
        match protocol {
            Protocol::Pair => {
                let mut enrolled = false;
                let answered = answer(stream, async |request| {
                    if let Some(reply) = self.forward_enrollment(Protocol::Pair, &request).await? {
                        return Ok(reply);
                    }
                    let reply = Box::pin(self.answer_pair(&request)).await?;
                    enrolled = true;
                    Ok(reply)
                })
                .await;
                // The joining peer must receive enrollment before we dial its return routes.
                if answered.is_ok() && enrolled {
                    self.wake_sync();
                }
                answered
            }
            Protocol::Renew => {
                answer(stream, async |request| {
                    if let Some(reply) = self.forward_enrollment(Protocol::Renew, &request).await? {
                        return Ok(reply);
                    }
                    if let Some(reply) =
                        rocky_secure::network::NativeSession::peer_resolution_reply(
                            &request,
                            &PrivateKey::from_bytes(&self.vault.get("node")?),
                        )?
                    {
                        return Ok(reply);
                    }
                    let settings = self.settings.lock().await.clone();
                    if !settings.issuer() {
                        return Err(refused("only an issuer renews devices"));
                    }
                    if let Some(bytes) = request.strip_prefix(crate::enrollment::EPOCHS) {
                        let request: crate::enrollment::EpochRequest =
                            serde_json::from_slice(bytes)?;
                        let signed = rocky_credentials::RenewRequest::from_bytes(&request.request)?;
                        signed.verify(
                            PrivateKey::from_bytes(&self.vault.get("node")?).public_key(),
                            settings.publisher,
                            self.domain.manifest().hash(),
                            (now()?, 300),
                        )?;
                        let registry = self.registry().await?;
                        let mut current = self.settings.lock().await;
                        let mut draft = current.clone();
                        let added = draft.learn_revocations(&request.revocations)?;
                        if !added.is_empty() {
                            draft.save(&self.path)?;
                            *current = draft;
                            self.apply(&current.revocations, &current.credentials, Vec::new())
                                .await?;
                            for partition in current.partitions(&self.domain)? {
                                self.hold_keys(&current, &partition).await?;
                            }
                        }
                        let reply =
                            current.answer_epochs(&self.vault, &request.request, &registry)?;
                        drop(current);
                        return Ok(reply);
                    }
                    if request == crate::enrollment::POLICY {
                        return Ok(serde_json::to_vec(&settings.revocations)?);
                    }
                    if request == crate::generation::OFFER {
                        return settings.offer();
                    }
                    if let Some(asked) = request.strip_prefix(crate::enrollment::AUTHOR) {
                        let asked = rocky_credentials::PseudonymRequest::from_bytes(asked)?;
                        let chain = Box::pin(self.answer_author(&asked)).await?;
                        let collection = settings.collection_name(asked.table).unwrap_or_default();
                        self.activity.record(
                            "author_assigned",
                            json!({"node": asked.node.to_hex(), "collection": collection}),
                        );
                        return Ok(serde_json::to_vec(&chain)?);
                    }
                    Box::pin(self.answer_renewal(&request)).await
                })
                .await
            }
            // The invocation carries its caller's signature and credentials;
            // whoever forwards it gains nothing by doing so.
            Protocol::Command => {
                answer(stream, async |request| {
                    let reply = self.answer_command_carrier(&request).await?;
                    self.wake_sync();
                    Ok(reply)
                })
                .await
            }
            Protocol::Link => Err("links require network-service ownership".into()),
            Protocol::Coordinate => Err("a database serves no coordinator requests".into()),
        }
    }

    /// Relays bytes only; the issuer and caller still verify the original request/answer.
    async fn forward_enrollment(
        &self,
        protocol: Protocol,
        request: &[u8],
    ) -> Result<Option<Vec<u8>>, Failure> {
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
            exchange(&*link.transport, &addresses, protocol, request).await?,
        ))
    }

    fn coordinator(&self) -> Result<&dyn Coordinator, Failure> {
        self.coordinator
            .as_deref()
            .ok_or_else(|| refused("no coordinator is composed with this database"))
    }
    #[cfg(feature = "iroh")]
    async fn service_call(
        &self,
        service: &crate::owner::Service,
        kind: &str,
        request: &Value,
        publisher: Option<PrivateKey>,
        activation: Option<rocky_core::Activation>,
    ) -> Result<Value, Failure> {
        let user = self
            .settings
            .lock()
            .await
            .credentials
            .0
            .first()
            .ok_or("missing credentials")?
            .capability
            .subject;
        let transport = self
            .network
            .read()
            .await
            .as_ref()
            .map(|link| link.transport.clone());
        let manifest = self.domain.manifest();
        let domain = crate::owner::Domain {
            scope: self.domain.scope,
            application: manifest.name.clone(),
            owner: secure_principal_id(user),
            schema: manifest.schema,
            handler: self.domain.app.commands().handler(),
            anchor: self.settings.lock().await.publisher,
            publisher,
            transport,
            activation,
            request,
        };
        service.call(kind, domain).await
    }
    #[cfg(not(feature = "iroh"))]
    async fn service_call(
        &self,
        service: &crate::owner::Service,
        _: &str,
        _: &Value,
        _: Option<PrivateKey>,
        _: Option<rocky_core::Activation>,
    ) -> Result<Value, Failure> {
        match *service {}
    }
    /// Admits and executes a command another device or a gateway delivered,
    /// through the admission a local ticket passes.
    async fn execute_delivered(&self, envelope: &[u8]) -> Delivery {
        match self.coordinator() {
            Ok(coordinator) => self.replica.deliver_command(envelope, coordinator).await,
            Err(error) => Delivery::NotSubmitted {
                retryable: false,
                message: error.to_string(),
            },
        }
    }
    /// The calls of typed strong commands. Submission, lookup and
    /// cancellation are local; `command_run` is what executes or delivers.
    async fn command(&self, kind: &str, request: &Value) -> Result<Value, Failure> {
        let app = &self.domain.app;
        let handle = || -> Result<[u8; 32], Failure> {
            let bytes: Vec<u8> = serde_json::from_value(request["handle"].clone())
                .map_err(|_| refused("handle must be 32 bytes"))?;
            bytes
                .try_into()
                .map_err(|_| refused("handle must be 32 bytes"))
        };
        let bytes = |name: &str| -> Result<Vec<u8>, Failure> {
            serde_json::from_value(request[name].clone())
                .map_err(|_| refused(format!("{name} must be bytes")))
        };
        let report = |ticket| crate::commands::ticket_json(app, &ticket);
        match kind {
            "command_identity" => {
                let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
                let user = self
                    .settings
                    .lock()
                    .await
                    .credentials
                    .0
                    .first()
                    .ok_or("missing credentials")?
                    .capability
                    .subject;
                Ok(json!({
                    "scope": self.domain.scope,
                    "node": HEXLOWER.encode(&secure_node_id(node).0),
                    "key": HEXLOWER.encode(node.as_bytes()),
                    "application": self.domain.manifest().name,
                    "principal": HEXLOWER.encode(&secure_principal_id(user).0),
                    "handler": app.hosts(),
                    "coordinator": self.coordinator.is_some(),
                    "schema_generation": self.domain.manifest().schema,
                    "handler_generation": app.commands().handler(),
                }))
            }
            // What the domain's owner submits to the coordinator to activate
            // this generation: the coordinator's head, which this device's
            // confirmed results must have reached, and the transition.
            "command_activation" => {
                let coordinator = self.coordinator()?;
                let scope = self.domain.scope;
                let head = coordinator.head(scope).await?;
                let manifest = self.domain.manifest();
                if (head.schema, head.handler) == (manifest.schema, manifest.handler) {
                    return Ok(json!({"active": true, "activation": null}));
                }
                let local = self.replica.confirmed_head(scope).await?;
                if local != Some((head.sequence, head.head))
                    && !(local.is_none() && head.sequence == 0)
                {
                    return Err(refused(
                        "this device has not received every confirmed result; sync first",
                    ));
                }
                let transition = self
                    .settings
                    .lock()
                    .await
                    .transition()?
                    .ok_or_else(|| refused("this domain has no transition to activate"))?;
                let resources: std::collections::BTreeSet<_> = app
                    .commands()
                    .descriptors()
                    .iter()
                    .flat_map(|descriptor| descriptor.resources.iter())
                    .collect();
                let mut state = Vec::new();
                for name in resources {
                    let resource = app
                        .scope(name)
                        .ok_or_else(|| refused("unknown command resource"))?;
                    let snapshot = self.replica.command_snapshot(resource).await?;
                    if !snapshot.complete {
                        return Err(refused(
                            "generation activation needs complete confirmed resource state",
                        ));
                    }
                    let rows: Vec<_> = app
                        .typed_rows(name, &snapshot.rows)?
                        .into_iter()
                        .map(crate::commands::cells)
                        .collect();
                    state.extend(serde_json::to_vec(&(name, rows))?);
                }
                if self.replica.confirmed_head(scope).await? != local {
                    return Err(refused(
                        "confirmed state advanced during generation verification; retry",
                    ));
                }
                let activation = rocky_core::Activation {
                    head,
                    schema: manifest.schema,
                    handler: manifest.handler,
                    transition: transition.id(),
                    state: commitment("rocky.sdk.activation-state.v1", &state),
                };
                if !activation.valid() {
                    return Err(refused(
                        "the coordinator's generation is not the one before this device's",
                    ));
                }
                Ok(json!({
                    "active": false,
                    "commitment": HEXLOWER.encode(&activation.commitment()),
                    "activation": activation,
                }))
            }
            // The owner registers or controls this domain with its tenant
            // key; the tenant secret stays in this process.
            "command_service_register"
            | "command_service_control"
            | "command_service_close"
            | "command_service_usage"
            | "command_service_activate" => {
                let service = self.service.as_ref().ok_or_else(|| {
                    refused("commands are not composed with a coordination service")
                })?;
                let settings = self.settings.lock().await.clone();
                // Whoever may execute registers executors, not only an issuer.
                let executes = if kind == "command_service_register" && !settings.issuer() {
                    let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
                    settings
                        .credentials
                        .verify(
                            settings.publisher,
                            self.domain.manifest().hash(),
                            node,
                            &settings.revocations,
                            now()?,
                        )?
                        .iter()
                        .any(|grant| grant.rights.contains(&rocky_core::Right::Execute))
                } else {
                    settings.issuer()
                };
                if kind == "command_service_register" && !executes && !settings.issuer() {
                    return Err(refused(
                        "registering a domain's executors needs credentials holding Execute",
                    ));
                }
                let publisher = if settings.issuer() {
                    Some(PrivateKey::from_bytes(&self.vault.get("publisher")?))
                } else {
                    None
                };
                // The owner activates exactly what this device verified.
                let activation = if kind == "command_service_activate" {
                    let prepared = Box::pin(self.command("command_activation", request)).await?;
                    if prepared["active"] == true {
                        return Ok(json!({"active": true}));
                    }
                    Some(serde_json::from_value(prepared["activation"].clone())?)
                } else {
                    None
                };
                self.service_call(service, kind, request, publisher, activation)
                    .await
            }
            // The publisher lets a tenant control key register this
            // application's domains on a coordination service. It grants
            // no row, key or command right.
            "command_coordination_grant" => {
                if !self.settings.lock().await.issuer() {
                    return Err(refused("only an issuer holds the publisher key"));
                }
                // The tenant's public key, or its secret in this process.
                let hex32 = |name: &str| -> Option<[u8; 32]> {
                    HEXLOWER
                        .decode(request[name].as_str()?.as_bytes())
                        .ok()?
                        .try_into()
                        .ok()
                };
                let tenant = match (hex32("tenant"), hex32("tenant_secret")) {
                    (Some(public), None) => public,
                    (None, Some(secret)) => {
                        *PrivateKey::from_bytes(&secret).public_key().as_bytes()
                    }
                    _ => return Err(refused("tenant must be a 64-digit hexadecimal key")),
                };
                let lifetime = request["lifetime"]
                    .as_u64()
                    .filter(|seconds| (1..=366 * 86_400).contains(seconds))
                    .ok_or_else(|| refused("lifetime must be 1 s to 366 days"))?;
                let publisher = PrivateKey::from_bytes(&self.vault.get("publisher")?);
                let time = now()?;
                let grant = rocky_credentials::SignedCoordinationGrant::sign(
                    rocky_credentials::CoordinationGrant {
                        publisher: rocky_credentials::Publisher(publisher.public_key()),
                        application: self.domain.manifest().name,
                        tenant: rocky_credentials::CoordinatorControl(
                            PublicKey::from_bytes(&tenant)
                                .map_err(|_| refused("tenant is not a valid key"))?,
                        ),
                        not_before: time.saturating_sub(60),
                        expires: time.saturating_add(lifetime),
                    },
                    &publisher,
                )?;
                Ok(json!({ "grant": grant }))
            }
            "command_submit" => {
                let method = request["method"]
                    .as_str()
                    .ok_or("method must be a string")?;
                let version = request["version"]
                    .as_u64()
                    .and_then(|version| u32::try_from(version).ok())
                    .ok_or("version must be a number")?;
                let descriptor = app.commands().get(method, version).map_err(refused)?;
                let cells: Vec<Cell> = serde_json::from_value(request["arguments"].clone())
                    .map_err(|_| refused("arguments must be tagged values"))?;
                let arguments = descriptor
                    .encode_arguments(&crate::commands::values(cells).map_err(refused)?)
                    .map_err(refused)?;
                let ticket = self
                    .replica
                    .submit_command(handle()?, self.domain.scope, method, version, &arguments)
                    .await?;
                report(ticket)
            }
            "command_ticket" => match self.replica.ticket(handle()?).await? {
                Some(ticket) => report(ticket),
                None => Ok(json!({"state": "unresolved"})),
            },
            "command_tickets" => {
                let tickets = self.replica.open_tickets(usize::MAX).await?;
                let tickets: Vec<Value> =
                    tickets.into_iter().map(report).collect::<Result<_, _>>()?;
                Ok(json!({"tickets": tickets}))
            }
            "command_cancel" => report(self.replica.cancel_ticket(handle()?).await?),
            "command_run" => {
                let handles = match request.get("handle").filter(|handle| !handle.is_null()) {
                    Some(_) => vec![handle()?],
                    None => self
                        .replica
                        .open_tickets(usize::MAX)
                        .await?
                        .into_iter()
                        .map(|ticket| ticket.handle)
                        .collect(),
                };
                let mut tickets = Vec::new();
                for handle in handles {
                    let failure = self.run(handle).await.err();
                    let ticket = self
                        .replica
                        .ticket(handle)
                        .await?
                        .ok_or_else(|| refused("unknown ticket"))?;
                    let mut ticket = report(ticket)?;
                    ticket["error"] = failure.as_ref().map_or(Value::Null, error);
                    tickets.push(ticket);
                }
                Ok(json!({"tickets": tickets}))
            }
            // The application's statement that every settled ticket's result
            // reached whoever needs it: the coordinator may retire those
            // identities, and their receipt evidence is no longer pinned.
            "command_acknowledge" => {
                self.replica
                    .acknowledge_tickets(self.coordinator()?)
                    .await?;
                Ok(json!({}))
            }
            // The three steps an application's own RPC or gateway forwards.
            "command_envelope" => {
                let envelope = self.replica.export_ticket(handle()?).await?;
                Ok(json!({"envelope": envelope.to_bytes()?}))
            }
            "command_execute" => {
                let answer = self.execute_delivered(&bytes("envelope")?).await;
                Ok(json!({"answer": answer.to_bytes()?}))
            }
            "command_settle" => {
                let answer = Delivery::from_bytes(&bytes("answer")?)?;
                report(
                    self.replica
                        .settle_delivery(handle()?, answer, self.coordinator()?)
                        .await?,
                )
            }
            "command_rows" => {
                let table = request["table"].as_str().ok_or("table must be a string")?;
                let scope = app.scope(table).ok_or_else(|| refused("unknown table"))?;
                let snapshot = self.replica.command_snapshot(scope).await?;
                let rows: Vec<Vec<Cell>> = app
                    .typed_rows(table, &snapshot.rows)?
                    .into_iter()
                    .map(crate::commands::cells)
                    .collect();
                let columns: Vec<&str> = self
                    .domain
                    .logical()
                    .table(table)
                    .ok_or_else(|| refused("unknown table"))?
                    .columns
                    .iter()
                    .map(|column| column.sql_name.as_str())
                    .collect();
                // The confirmed sequence this device holds results through.
                let sequence = self
                    .replica
                    .confirmed_head(scope.domain)
                    .await?
                    .map_or(0, |(sequence, _)| sequence);
                // Without `complete`, the rows are withheld, not empty.
                Ok(json!({
                    "complete": snapshot.complete,
                    "sequence": sequence.to_string(),
                    "revision": snapshot.revision.to_string(),
                    "columns": columns,
                    "rows": rows,
                }))
            }
            other => Err(refused(format!("unsupported request {other}"))),
        }
    }
    /// Advances one ticket: executes it here when this device is the
    /// domain's executor, otherwise discovers a direct route or forwards once.
    /// The coordinator's assignment and the peer's credentials decide eligibility.
    async fn run(&self, handle: [u8; 32]) -> Result<(), Failure> {
        let ticket = self
            .replica
            .ticket(handle)
            .await?
            .ok_or_else(|| refused("unknown ticket"))?;
        match ticket.state {
            TicketState::Confirmed | TicketState::Applied => return Ok(()),
            TicketState::Cancelled => return Err(refused("ticket was cancelled")),
            // Settled by the coordinator: never run again or replaced.
            TicketState::Retired | TicketState::Expired => return Ok(()),
            _ => {}
        }
        let coordinator = self.coordinator()?;
        let mut head = coordinator.head(self.domain.scope).await?;
        if head.executor == rocky_core::NodeId([0; 16])
            && self.domain.app.hosts()
            && let Some(acquire) = &self.acquire
        {
            match acquire(self.domain.scope, head.epoch).await {
                Ok(()) => {}
                // Not eligible here: another executor acquires it on its next run.
                Err(rocky_core::CoordinationError::Denied) => {
                    return Err(Box::new(Deferred(
                        "the domain has no executor yet; an eligible executor acquires it on its next run".into(),
                    )));
                }
                Err(error) => return Err(error.into()),
            }
            head = coordinator.head(self.domain.scope).await?;
        }
        if head.executor == rocky_core::NodeId([0; 16]) {
            return Err(Box::new(Deferred(
                "the domain has no executor yet; an eligible executor acquires it on its next run"
                    .into(),
            )));
        }
        let executor = head.executor;
        let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        if executor == secure_node_id(node) {
            self.replica.run_ticket(handle, coordinator).await?;
            return Ok(());
        }
        let attached = self.network.read().await;
        let link = attached
            .as_ref()
            .ok_or(ServiceError::NetworkRequired("command delivery"))?;
        let envelope = self.replica.export_ticket(handle).await?.to_bytes()?;
        let mut stage = link.stage.subscribe();
        let answer = tokio::select! {
            answer = self.route_command(&*link.transport, executor, envelope) => answer?,
            _ = stage.wait_for(|stage| *stage == Stage::Cancelled) => {
                return Err(ServiceError::Closed("network service").into());
            }
        };
        let answer = Delivery::from_bytes(&answer)?;
        let failure = match &answer {
            Delivery::Confirmed(_) => None,
            Delivery::NotSubmitted { retryable, message } => Some((*retryable, message.clone())),
            Delivery::Unknown(message) => Some((true, message.clone())),
            // The ticket records the settlement; it is never run again.
            Delivery::Settled { .. } => None,
        };
        self.replica
            .settle_delivery(handle, answer, coordinator)
            .await?;
        match failure {
            None => Ok(()),
            Some((true, message)) => Err(Box::new(Deferred(message))),
            Some((false, message)) => Err(refused(message)),
        }
    }

    async fn sql_call(
        &self,
        client: &Client,
        kind: &str,
        request: &Value,
    ) -> Result<Value, Failure> {
        // An active SQL lease checks authority itself. Asking session status
        // would wait for the replica lock the lease already owns.
        let cached = client.reader.lock().await.clone();
        let reader = match cached {
            Some(reader) => reader,
            None => self.reader(client).await?,
        };
        if kind == "move_row" {
            let table = request["table"]
                .as_str()
                .ok_or_else(|| refused("table must be a string"))?;
            let id: rocky_core::RowIdentity = serde_json::from_value(request["id"].clone())?;
            let tuple = |name: &str| -> Result<Vec<SqlValue>, Failure> {
                let values: Vec<Cell> = serde_json::from_value(request[name].clone())?;
                values.into_iter().map(Cell::sql).collect()
            };
            let handle: Vec<u8> = serde_json::from_value(request["handle"].clone())?;
            let handle: [u8; 32] = handle
                .try_into()
                .map_err(|_| refused("handle must be 32 bytes"))?;
            let committed = reader
                .sql
                .move_row(
                    secure_handle(&handle),
                    table,
                    id,
                    &tuple("from")?,
                    &tuple("to")?,
                )
                .await?;
            return Ok(
                json!({"committed": {"handle": HEXLOWER.encode(&committed.handle), "frames": committed.frames.len()}}),
            );
        }
        if kind == "next_id" {
            let table = request["table"]
                .as_str()
                .ok_or_else(|| refused("table must be a string"))?;
            let id = if let Some(values) = request.get("partition_values") {
                let values: Vec<Cell> = serde_json::from_value(values.clone())?;
                reader
                    .sql
                    .next_id_partition(
                        table,
                        &values
                            .into_iter()
                            .map(Cell::sql)
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                    .await?
            } else {
                reader.sql.next_id(table).await?
            };
            return Ok(json!({"id":id}));
        }
        let sql = request["sql"]
            .as_str()
            .filter(|sql| sql.len() <= MAX_SQL)
            .ok_or_else(|| refused("sql must be a bounded string"))?;
        let values: Vec<Cell> =
            serde_json::from_value(request.get("values").cloned().unwrap_or_else(|| json!([])))
                .map_err(refused)?;
        if values.len() > 256 {
            return Err(refused("SQL parameter quota"));
        }
        let result = if sql.trim().eq_ignore_ascii_case("COMMIT") && request.get("handle").is_some()
        {
            if !values.is_empty() {
                return Err(refused("COMMIT takes no SQL parameters"));
            }
            let handle: Vec<u8> =
                serde_json::from_value(request["handle"].clone()).map_err(refused)?;
            let handle: [u8; 32] = handle
                .try_into()
                .map_err(|_| refused("handle must be 32 bytes"))?;
            reader.sql.commit(secure_handle(&handle)).await?
        } else {
            reader
                .sql
                .run(
                    sql,
                    values
                        .into_iter()
                        .map(Cell::sql)
                        .collect::<Result<_, _>>()?,
                )
                .await?
        };
        let rows: Vec<Vec<Cell>> = result
            .rows
            .into_iter()
            .map(|row| row.into_iter().map(Cell::from).collect())
            .collect();
        Ok(
            json!({"columns": result.columns, "rows": rows, "changes": result.changes.to_string(), "committed": result.committed.map(|commit| json!({"handle": HEXLOWER.encode(&commit.handle), "frames": commit.frames.len()}))}),
        )
    }
    async fn read(&self, client: &Client, query: Query) -> Result<Value, Failure> {
        match self.read_once(client, query.clone()).await {
            // An authority change ends the cached session; read again once.
            Err(error) if is_authority(&error) => {
                *client.reader.lock().await = None;
                self.read_once(client, query).await
            }
            other => other,
        }
    }
    async fn read_once(&self, client: &Client, query: Query) -> Result<Value, Failure> {
        let max_bytes = query.max_bytes.unwrap_or(rocky_runtime::MAX_FRAME_BYTES);
        if query.max_rows == 0
            || query.max_rows > MAX_ROWS
            || query.sql.len() > MAX_SQL
            || query.values.len() > 256
            || !(256..=rocky_runtime::MAX_FRAME_BYTES).contains(&max_bytes)
        {
            return Err(refused("query quota"));
        }
        let settings = self.settings.lock().await;
        let authority = (settings.revocations.digest(now()?)?, settings.expires());
        drop(settings);
        let mut descriptor = query.clone();
        descriptor.cursor = None;
        let token = |offset: usize| -> Result<String, Failure> {
            let bytes = serde_json::to_vec(&(
                client.cursor_key,
                self.domain.manifest().hash(),
                authority,
                &descriptor,
                offset,
            ))?;
            Ok(format!(
                "page.v1:{offset}:{}",
                HEXLOWER.encode(&commitment("rocky.sdk.cursor.v1", &bytes))
            ))
        };
        let offset: usize = match &query.cursor {
            Some(cursor) => {
                let offset = cursor
                    .strip_prefix("page.v1:")
                    .and_then(|rest| rest.split_once(':'))
                    .and_then(|(offset, _)| offset.parse().ok())
                    .ok_or_else(|| refused("invalid cursor"))?;
                if *cursor != token(offset)? {
                    return Err(refused(
                        "cursor belongs to another session, query or authority generation",
                    ));
                }
                offset
            }
            None => 0,
        };
        let reader = self.reader(client).await?;
        let revision = *self.replica.observe_revision().borrow();
        let mut values = query
            .values
            .into_iter()
            .map(Cell::sql)
            .collect::<Result<Vec<_>, _>>()?;
        values.push(SqlValue::Integer(i64::try_from(query.max_rows + 1)?));
        values.push(SqlValue::Integer(i64::try_from(offset)?));
        let sql = format!(
            "SELECT * FROM ({}) LIMIT ? OFFSET ?",
            query.sql.trim().trim_end_matches(';')
        );
        let mut result = reader.sql.run(&sql, values).await?;
        let more = result.rows.len() > query.max_rows;
        result.rows.truncate(query.max_rows);
        let mut rows = Vec::new();
        let mut used = serde_json::to_vec(&json!({
            "scope": self.domain.scope,
            "revision": revision.to_string(),
            "columns": result.columns,
            "rows": [],
            "next": token(usize::MAX)?,
            "next_kind": "offset",
        }))?
        .len()
            + 64;
        let mut limited = false;
        for row in result.rows {
            let row: Vec<Cell> = row.into_iter().map(Cell::from).collect();
            let size = serde_json::to_vec(&row)?.len() + 1;
            if used + size > max_bytes {
                if rows.is_empty() {
                    return Err(refused("one row exceeds the page byte quota"));
                }
                limited = true;
                break;
            }
            used += size;
            rows.push(row);
        }
        let next = if more || limited {
            Some(token(
                offset
                    .checked_add(rows.len())
                    .ok_or_else(|| refused("cursor quota"))?,
            )?)
        } else {
            None
        };
        Ok(json!({
            "scope": self.domain.scope,
            "revision": revision.to_string(),
            "columns": result.columns,
            "rows": rows,
            "next": next,
            "next_kind": "offset",
        }))
    }
    /// The first page of a subscription's query once the committed
    /// revision differs from the one it last delivered.
    async fn next(&self, client: &Client, id: u64) -> Result<Value, Failure> {
        let mut revisions = self.replica.observe_revision();
        let mut closing = client.closing.subscribe();
        let mut subscriptions = client.subscriptions_changed.subscribe();
        loop {
            let (query, delivered) = {
                let subscriptions = client.subscriptions()?;
                let subscription = subscriptions
                    .get(&id)
                    .ok_or_else(|| refused("unknown subscription"))?;
                (subscription.query.clone(), subscription.delivered.clone())
            };
            let current = revisions.borrow_and_update().to_string();
            if delivered.as_deref() != Some(current.as_str()) {
                let page = self.read(client, query).await?;
                if let Some(subscription) = client.subscriptions()?.get_mut(&id) {
                    subscription.delivered = page["revision"].as_str().map(String::from);
                }
                return Ok(page);
            }
            tokio::select! {
                changed = revisions.changed() => {
                    changed.map_err(|_| ServiceError::Closed("database"))?
                }
                _ = closing.wait_for(|closing| *closing) => {
                    return Err(ServiceError::Closed("session").into());
                }
                _ = subscriptions.changed() => {}
            }
        }
    }
}

/// The secure handle of a mutation handle.
fn secure_handle(handle: &[u8; 32]) -> rocky_core::Commitment {
    commitment("rocky.sdk.mutation.v1", handle)
}
/// The `after` token of an activity call; absent starts at the beginning.
fn token(request: &Value) -> Result<u64, Failure> {
    match &request["after"] {
        Value::Null => Ok(0),
        after => Ok(after
            .as_str()
            .ok_or_else(|| refused("after must be an activity token"))?
            .parse()?),
    }
}
fn subscription(request: &Value) -> Result<u64, Failure> {
    Ok(request["subscription"]
        .as_str()
        .ok_or("subscription must be a string")?
        .parse()?)
}
/// The statement that revokes this device, when a session ended on it.
fn revoked_by(error: &Failure) -> Option<rocky_credentials::SignedRevocation> {
    match error.downcast_ref::<Error>()? {
        Error::Application(error) => error
            .downcast_ref::<rocky_secure::network::Revoked>()
            .map(|revoked| (*revoked.0).clone()),
        _ => None,
    }
}
fn is_authority(error: &Failure) -> bool {
    error.to_string().contains("session authority changed")
}

/// The error object of a failed call: its code and whether retrying the
/// same call may succeed.
pub fn error(error: &Failure) -> Value {
    let (code, retryable) = if error.is::<crate::enrollment::Revoked>() {
        ("revoked", false)
    } else if error.is::<crate::registry::RegistryFull>() {
        ("registry_full", false)
    } else if error.is::<crate::enrollment::RevocationSetFull>() {
        ("revocation_set_full", false)
    } else if error.is::<Refused>() {
        ("rejected", false)
    } else if error.is::<Deferred>() {
        ("executor_unavailable", true)
    } else if let Some(coordination) = error.downcast_ref::<rocky_core::CoordinationError>() {
        coordination_code(coordination)
    } else if let Some(service) = error.downcast_ref::<ServiceError>() {
        (service.code(), service.retryable())
    } else if let Some(credentials) = error.downcast_ref::<rocky_credentials::Error>() {
        match credentials {
            rocky_credentials::Error::Provider(_) => ("storage", false),
            _ => ("denied", false),
        }
    } else {
        match error.downcast_ref::<Error>() {
            Some(Error::Rejected(_) | Error::Protocol(_)) => ("rejected", false),
            Some(Error::Incompatible(_)) => ("unsupported_generation", false),
            Some(Error::Sql(SqlError::Denied)) => ("denied", false),
            Some(Error::Sql(SqlError::Capture(_) | SqlError::Statement { .. })) => {
                ("rejected", false)
            }
            Some(Error::Conflict) => ("conflict", true),
            Some(Error::Backpressure) => ("quota", true),
            Some(Error::Handler(_)) => ("handler_unavailable", true),
            Some(Error::StrongPending(_)) => ("missing_head", true),
            Some(Error::Coordination(coordination)) => coordination_code(coordination),
            _ => ("storage", true),
        }
    };
    json!({"code": code, "retryable": retryable, "message": error.to_string()})
}
/// A lost coordinator answer leaves the confirmation unknown; a stale fence
/// or head is met again by running the same ticket.
fn coordination_code(error: &rocky_core::CoordinationError) -> (&'static str, bool) {
    use rocky_core::CoordinationError::{
        Backpressure, Denied, Generations, StaleEpoch, StaleHead, Unavailable,
    };
    match error {
        Unavailable => ("unknown_confirmation", true),
        // A full response window: acknowledge settled tickets or wait for expiry.
        Backpressure => ("quota", true),
        // Retried once the coordinator activates this device's generation.
        Generations => ("unsupported_generation", true),
        StaleHead | StaleEpoch => ("missing_head", true),
        Denied => ("denied", false),
        _ => ("rejected", false),
    }
}

#[cfg(test)]
mod delivery_tests;
#[cfg(test)]
mod revocation_tests;
#[cfg(test)]
mod sync_tests;
