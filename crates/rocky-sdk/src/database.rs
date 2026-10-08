//! The database owner and the handles composed on it: sessions and an
//! optional network service. See `docs/runtime/sdk-composition.md`.
use crate::{
    Failure,
    commands::{Commands, HANDLER_DEADLINE, Hosted},
    connection::{self, ConnectionOptions, Page, SqlFailure, TableEvents},
    enrollment::{DEFAULT_TTL, DeviceKeys, Settings, Supplied, now},
    node::{self, Client, Node},
    schema::Schema,
    secrets::{FileSecretStore, SecretStore},
    service::{Network, ServiceError},
    transport::Transport,
};
use rocky_secure::{EncryptedReplica, PrivateKey};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot};

/// What opening a database takes. Without `secrets`, keys are owner-only
/// files in the data directory. Without `network`, nothing is dialed or
/// listened on.
pub struct Open<'a> {
    pub directory: &'a Path,
    pub schema: &'a str,
    pub secrets: Option<Arc<dyn SecretStore>>,
    pub network: Option<Network>,
    /// The coordinator and handlers of the schema's strong commands.
    /// Without them commands are submitted and read, never run or settled.
    pub commands: Option<Commands>,
}
/// An open database and the network service it opened with, if any.
pub struct Opened {
    pub database: Arc<Database>,
    pub network: Option<NetworkService>,
}

enum Install {
    Create {
        ttl: u64,
    },
    Join {
        invitation: String,
    },
    Supplied(Box<Supplied>),
    Installed,
    /// A backup archive, under a fresh identity the invitation enrolls.
    Restore {
        archive: PathBuf,
        invitation: String,
    },
}
struct Setup {
    directory: PathBuf,
    schema: Schema,
    secrets: Arc<dyn SecretStore>,
    install: Install,
    commands: Option<Commands>,
}
enum Command {
    Call {
        session: u64,
        bytes: Vec<u8>,
        ending: Arc<AtomicBool>,
        reply: oneshot::Sender<Vec<u8>>,
    },
    End(u64, oneshot::Sender<()>),
    Attach(Network, oneshot::Sender<Result<u64, Failure>>),
    Detach(u64, oneshot::Sender<()>),
    Close(oneshot::Sender<Result<(), String>>),
    Connect(ConnectionOptions, oneshot::Sender<Result<u64, Failure>>),
    Sql {
        connection: u64,
        call: connection::Call,
        reply: oneshot::Sender<Result<connection::Reply, SqlFailure>>,
    },
    Disconnect(u64, oneshot::Sender<()>),
    Observe(
        Option<Vec<String>>,
        oneshot::Sender<Result<rocky_runtime::TableObserver, Failure>>,
    ),
}
/// The scope and the network service a database opened with.
type Greeting = Result<(String, Option<u64>), Failure>;

struct Shared {
    input: mpsc::Sender<Command>,
    budget: Semaphore,
    closed: AtomicBool,
    sessions: AtomicU64,
}
impl Shared {
    async fn request<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, ServiceError> {
        let (reply, result) = oneshot::channel();
        self.input
            .send(command(reply))
            .await
            .map_err(|_| ServiceError::Closed("database"))?;
        result.await.map_err(|_| ServiceError::Closed("database"))
    }
}

/// Owns one secure database: its file, its installed credentials and the
/// reactor thread that serves its sessions. It owns no network.
pub struct Database {
    shared: Arc<Shared>,
    scope: String,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl Database {
    /// Creates a domain in an empty directory. This device becomes its
    /// first issuer and holds its publisher key. Credentials last
    /// `credential_ttl` seconds, 30 days by default.
    pub fn create(open: Open<'_>, credential_ttl: Option<u64>) -> Result<Opened, Failure> {
        let ttl = credential_ttl.unwrap_or(DEFAULT_TTL);
        if !(60..=366 * 24 * 60 * 60).contains(&ttl) {
            return Err("credential lifetime must be between a minute and a year".into());
        }
        Self::start(open, Install::Create { ttl })
    }
    /// Enrolls this device with the issuer an invitation names, as the
    /// grant profile it names, over `open.network`, which stays attached.
    pub fn join(open: Open<'_>, invitation: &str) -> Result<Opened, Failure> {
        if open.network.is_none() {
            return Err(ServiceError::NetworkRequired("join").into());
        }
        Self::start(
            open,
            Install::Join {
                invitation: invitation.into(),
            },
        )
    }
    /// Makes the keys of a device that will receive credentials from
    /// outside the SDK, and returns their public halves for whoever issues
    /// them. Asking again returns the same keys. Nothing is contacted.
    pub fn device(
        directory: &Path,
        secrets: Option<Arc<dyn SecretStore>>,
    ) -> Result<DeviceKeys, Failure> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join("identity.json");
        if path.exists() {
            return Err("a domain exists in this directory: open it instead".into());
        }
        let secrets =
            secrets.unwrap_or_else(|| Arc::new(FileSecretStore::new(directory.join("secrets"))));
        Settings::prepare_install(&path, &secrets)
    }
    /// Installs credentials and reader keys supplied for the device that
    /// `device` prepared. They are verified like any others; no issuer is
    /// contacted and no network is needed.
    pub fn install(open: Open<'_>, supplied: Supplied) -> Result<Opened, Failure> {
        Self::start(open, Install::Supplied(Box::new(supplied)))
    }
    /// Opens the domain installed in the directory. A member whose
    /// credentials expired renews them over `open.network` first.
    pub fn open(open: Open<'_>) -> Result<Opened, Failure> {
        Self::start(open, Install::Installed)
    }
    /// Restores a backup archive into an empty directory under a fresh
    /// signing identity, which `invitation` enrolls over `open.network`.
    /// The archive's operations, handles, tickets, results and effects keep
    /// their original identities; nothing is sent again as new work. An
    /// interrupted restore continues when called again. See
    /// `docs/runtime/backup-and-restore.md`.
    pub fn restore(open: Open<'_>, archive: &Path, invitation: &str) -> Result<Opened, Failure> {
        if open.network.is_none() {
            return Err(ServiceError::NetworkRequired("restore").into());
        }
        Self::start(
            open,
            Install::Restore {
                archive: archive.to_path_buf(),
                invitation: invitation.into(),
            },
        )
    }
    /// Writes a signed backup archive of this database to `target`; see
    /// `restore`. It returns the archive's inventory as JSON.
    pub async fn backup(&self, target: &Path) -> Result<String, Failure> {
        let session = self.session();
        let call = json!({
            "version": 1,
            "scope": serde_json::from_str::<Value>(&self.scope)?,
            "request": {"type": "backup", "path": target.to_str().ok_or("the backup path is not UTF-8")?},
        });
        let reply: Value = serde_json::from_str(&session.call(call.to_string()).await?)?;
        session.close().await;
        if reply["status"] != "ok" {
            return Err(reply["error"]["message"]
                .as_str()
                .unwrap_or("backup failed")
                .to_string()
                .into());
        }
        Ok(reply["value"].to_string())
    }
    fn start(open: Open<'_>, install: Install) -> Result<Opened, Failure> {
        let directory = open.directory.to_path_buf();
        let setup = Setup {
            schema: Schema::parse(open.schema)?,
            secrets: open
                .secrets
                .unwrap_or_else(|| Arc::new(FileSecretStore::new(directory.join("secrets")))),
            directory,
            install,
            commands: open.commands,
        };
        let network = open.network;
        let (input, receiver) = mpsc::channel(32);
        let (hello, greeting) = std::sync::mpsc::sync_channel::<Greeting>(1);
        let thread = std::thread::Builder::new()
            .name("rocky-sdk".into())
            .spawn(move || {
                let result = (|| -> Result<(), Failure> {
                    // Transports run their tasks on the workers; the
                    // database's tasks stay on this thread.
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .thread_name("rocky-sdk-io")
                        .enable_all()
                        .build()?;
                    let local = tokio::task::LocalSet::new();
                    runtime.block_on(local.run_until(run(setup, network, receiver, &hello)))
                })();
                if let Err(error) = result {
                    let _ = hello.send(Err(error));
                }
            })?;
        match greeting.recv().map_err(|_| "database failed to start")? {
            Ok((scope, network)) => {
                let shared = Arc::new(Shared {
                    input,
                    budget: Semaphore::new(128),
                    closed: AtomicBool::new(false),
                    sessions: AtomicU64::new(1),
                });
                Ok(Opened {
                    network: network.map(|id| NetworkService::new(&shared, id)),
                    database: Arc::new(Self {
                        shared,
                        scope,
                        thread: Mutex::new(Some(thread)),
                    }),
                })
            }
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }
    /// The domain's scope as the call contract spells it.
    pub fn scope(&self) -> String {
        self.scope.clone()
    }
    /// A session on this database. Sessions are independent: each has its
    /// own subscriptions, and closing one leaves the others open.
    pub fn session(&self) -> Session {
        Session {
            shared: self.shared.clone(),
            id: self.shared.sessions.fetch_add(1, Ordering::Relaxed),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }
    /// Attaches a network service: enrollment, renewal and sync use it, and
    /// the creator serves its peers on it. One can be attached at a time.
    pub async fn attach(&self, network: Network) -> Result<NetworkService, Failure> {
        let id = self
            .shared
            .request(|reply| Command::Attach(network, reply))
            .await??;
        Ok(NetworkService::new(&self.shared, id))
    }
    /// A SQL connection for a database driver: the writer is a secure SQL
    /// session, a reader steps snapshot cursors. See
    /// `docs/runtime/sql-driver.md`.
    pub async fn connect(&self, options: ConnectionOptions) -> Result<Connection, Failure> {
        let id = self
            .shared
            .request(|reply| Command::Connect(options, reply))
            .await??;
        Ok(Connection {
            shared: self.shared.clone(),
            id,
            statements: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        })
    }
    /// Committed changes of `tables`, or of every table. Register before
    /// reading what the changes invalidate.
    pub async fn observe_tables(
        &self,
        tables: Option<Vec<String>>,
    ) -> Result<TableEvents, Failure> {
        let observer = self
            .shared
            .request(|reply| Command::Observe(tables, reply))
            .await??;
        Ok(TableEvents::new(observer))
    }
    /// Ends every session, detaches the network service, flushes and
    /// closes the file. Admitted calls finish first. Closing again does
    /// nothing.
    pub async fn close(&self) -> Result<(), Failure> {
        let mut thread = self.thread.lock().await;
        if let Some(worker) = thread.take() {
            self.shared.closed.store(true, Ordering::SeqCst);
            let result = self.shared.request(Command::Close).await;
            worker.join().map_err(|_| "database thread failed")?;
            if let Ok(result) = result {
                result.map_err(|message| -> Failure { message.into() })?;
            }
        }
        Ok(())
    }
}

/// One SQL connection of a database. Statements are numbered by the
/// caller's handles; each keeps the undelivered rest of its result.
pub struct Connection {
    shared: Arc<Shared>,
    id: u64,
    statements: AtomicU64,
    closed: AtomicBool,
}
impl Connection {
    /// A number for a new statement handle of this connection.
    pub fn statement(&self) -> u64 {
        self.statements.fetch_add(1, Ordering::Relaxed)
    }
    async fn call(&self, call: connection::Call) -> Result<connection::Reply, SqlFailure> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(connection::failure(
                connection::code::MISUSE,
                "the connection is closed",
            ));
        }
        let connection = self.id;
        self.shared
            .request(|reply| Command::Sql {
                connection,
                call,
                reply,
            })
            .await
            .map_err(|error| connection::failure(connection::code::MISUSE, error.to_string()))?
    }
    /// Runs `sql` as statement `statement` and returns its first page; a
    /// result `statement` still held is dropped first.
    pub async fn execute(
        &self,
        statement: u64,
        sql: String,
        params: Vec<rocky_core::SqlValue>,
    ) -> Result<Page, SqlFailure> {
        match self
            .call(connection::Call::Execute {
                statement,
                sql,
                params,
            })
            .await?
        {
            connection::Reply::Page(page) => Ok(page),
            _ => Err(connection::failure(connection::code::MISUSE, "no page")),
        }
    }
    /// The next page of `statement`'s result.
    pub async fn next(&self, statement: u64) -> Result<Page, SqlFailure> {
        match self.call(connection::Call::Next { statement }).await? {
            connection::Reply::Page(page) => Ok(page),
            _ => Err(connection::failure(connection::code::MISUSE, "no page")),
        }
    }
    /// Drops the rest of `statement`'s result.
    pub async fn finish(&self, statement: u64) -> Result<(), SqlFailure> {
        self.call(connection::Call::Finish { statement })
            .await
            .map(drop)
    }
    pub async fn in_transaction(&self) -> Result<bool, SqlFailure> {
        match self.call(connection::Call::InTransaction).await? {
            connection::Reply::Flag(flag) => Ok(flag),
            _ => Err(connection::failure(connection::code::MISUSE, "no state")),
        }
    }
    /// Closes the connection: an open transaction rolls back, its cursors
    /// end. Closing again does nothing.
    pub async fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self
                .shared
                .request(|reply| Command::Disconnect(self.id, reply))
                .await;
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self
                .shared
                .input
                .try_send(Command::Disconnect(self.id, oneshot::channel().0));
        }
    }
}

/// One caller's view of a shared database: the JSON calls of the native ABI.
pub struct Session {
    shared: Arc<Shared>,
    id: u64,
    closed: Arc<AtomicBool>,
}
impl Session {
    pub async fn call(&self, request: String) -> Result<String, Failure> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ServiceError::Closed("session").into());
        }
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(ServiceError::Closed("database").into());
        }
        if request.len() > rocky_runtime::MAX_FRAME_BYTES {
            return Err("call frame quota".into());
        }
        let _permit = self
            .shared
            .budget
            .try_acquire()
            .map_err(|_| "pending call quota")?;
        let session = self.id;
        let bytes = request.into_bytes();
        let reply = self
            .shared
            .request(|reply| Command::Call {
                session,
                bytes,
                ending: self.closed.clone(),
                reply,
            })
            .await?;
        Ok(String::from_utf8(reply)?)
    }
    /// Ends this session: its waits stop and its subscriptions are
    /// released. The database and its other sessions stay open.
    pub async fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self
                .shared
                .request(|reply| Command::End(self.id, reply))
                .await;
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self
                .shared
                .input
                .try_send(Command::End(self.id, oneshot::channel().0));
        }
    }
}

/// The network service attached to a database. Dropping the handle leaves
/// it attached until the database closes.
pub struct NetworkService {
    shared: Arc<Shared>,
    id: u64,
    closed: AtomicBool,
}
impl NetworkService {
    fn new(shared: &Arc<Shared>, id: u64) -> Self {
        Self {
            shared: shared.clone(),
            id,
            closed: AtomicBool::new(false),
        }
    }
    /// Stops serving and syncing. Admitted work has a few seconds to
    /// finish; what remains is cancelled and is safe to repeat. A transport
    /// the service started is closed, a borrowed one is not. The database
    /// stays open. Closing again does nothing.
    pub async fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self
                .shared
                .request(|reply| Command::Detach(self.id, reply))
                .await;
        }
    }
}

type Link = (Arc<dyn Transport>, bool);

/// The installation of the data directory, created, joined or loaded, and
/// the started transport of `network`.
async fn identity(
    path: &Path,
    setup: &Setup,
    network: Option<Network>,
) -> Result<(Settings, Option<Link>), Failure> {
    let Setup {
        directory,
        schema,
        secrets,
        install,
        ..
    } = setup;
    let exists = path.exists();
    // A restore interrupted after it enrolled continues.
    let resuming = matches!(install, Install::Restore { .. })
        && directory.join(crate::backup::MARKER).exists();
    if exists && !matches!(install, Install::Installed) && !resuming {
        return Err("a domain exists in this directory: open it instead".into());
    }
    if !exists && matches!(install, Install::Installed) {
        return Err("no domain in this directory: create or join one".into());
    }
    if exists && !resuming && !directory.join("rocky.sqlite").exists() {
        return Err("existing identity has no runtime database; recovery is required".into());
    }
    match install {
        Install::Create { ttl } => {
            let settings = Settings::create(schema, secrets, *ttl)?;
            let vault = settings.vault(secrets);
            let started = async {
                let Some(network) = network else {
                    return Ok(None);
                };
                let key = Settings::transport_key(&vault)?;
                Ok::<_, Failure>(Some(network.start(true, key).await?))
            }
            .await;
            match started {
                Ok(link) => Ok((settings, link)),
                Err(error) => {
                    let _ = vault.forget(settings.roles());
                    Err(error)
                }
            }
        }
        Install::Join { invitation } => {
            let network = network.ok_or(ServiceError::NetworkRequired("join"))?;
            let joining = Settings::prepare_join(path, invitation, secrets)?;
            let key = Settings::transport_key(&joining.vault)?;
            let (transport, owned) = network.start(false, key).await?;
            match joining.enroll(path, schema, &*transport).await {
                Ok(settings) => Ok((settings, Some((transport, owned)))),
                Err(error) => {
                    if owned {
                        transport.close().await;
                    }
                    Err(error)
                }
            }
        }
        Install::Supplied(supplied) => {
            let settings = Settings::install(path, schema, secrets, supplied)?;
            let Some(network) = network else {
                return Ok((settings, None));
            };
            let key = Settings::transport_key(&settings.vault(secrets))?;
            Ok((settings, Some(network.start(false, key).await?)))
        }
        Install::Restore {
            archive,
            invitation,
        } => {
            let network = network.ok_or(ServiceError::NetworkRequired("restore"))?;
            restore(
                path, directory, secrets, schema, archive, invitation, network,
            )
            .await
        }
        Install::Installed => {
            let (mut settings, migrated) = Settings::load(path)?;
            // Before version 5 a device made its route key on first use.
            // The migration makes it once; afterwards a missing key is an error.
            let vault = settings.vault(secrets);
            if migrated && vault.find("transport")?.is_none() {
                vault.create("transport")?;
            }
            if settings.logged_out {
                return Err("this device logged out; join the domain again".into());
            }
            let link = match network {
                Some(network) => {
                    let key = Settings::transport_key(&vault)?;
                    Some(network.start(settings.issuer(), key).await?)
                }
                None => None,
            };
            // Another application schema: a local rebuild or the domain's
            // next generation, saved before the replica opens.
            if settings.schema != *schema {
                let transport = link.as_ref().map(|(transport, _)| &**transport);
                if let Err(error) = settings.evolve(path, &vault, schema, transport).await {
                    if let Some((transport, true)) = &link {
                        transport.close().await;
                    }
                    return Err(error);
                }
            }
            Ok((settings, link))
        }
    }
}

/// Verifies the archive, enrolls a fresh identity with the invitation's
/// issuer and puts the archive's files in place. The marker, written
/// before enrolling, names the predecessor whose file is rebound at open.
async fn restore(
    path: &Path,
    directory: &Path,
    secrets: &Arc<dyn SecretStore>,
    schema: &Schema,
    archive: &Path,
    invitation: &str,
    network: Network,
) -> Result<(Settings, Option<Link>), Failure> {
    use crate::backup::{MARKER, Marker};
    let staging = directory.join("restore");
    let opened = crate::backup::open(archive, &staging)?;
    let marker = directory.join(MARKER);
    if marker.exists() {
        let saved: Marker = serde_json::from_slice(&std::fs::read(&marker)?)?;
        if saved.archive != opened.id {
            return Err("another restore is unfinished in this directory".into());
        }
    } else {
        if Settings::invitation_publisher(invitation)? != opened.inventory.publisher {
            return Err("the invitation is for another domain than the backup".into());
        }
        crate::secrets::write_private(
            &marker,
            &serde_json::to_vec(&Marker {
                predecessor: opened.inventory.node,
                archive: opened.id.clone(),
            })?,
        )?;
    }
    let (settings, link) = if path.exists() {
        let (settings, _) = Settings::load(path)?;
        let vault = settings.vault(secrets);
        let key = Settings::transport_key(&vault)?;
        let link = network.start(settings.issuer(), key).await?;
        (settings, link)
    } else {
        let joining = Settings::prepare_join(path, invitation, secrets)?;
        let key = Settings::transport_key(&joining.vault)?;
        let (transport, owned) = network.start(false, key).await?;
        match joining.enroll(path, schema, &*transport).await {
            Ok(mut settings) => {
                let node = PrivateKey::from_bytes(&settings.vault(secrets).get("node")?);
                settings.learn_contacts(
                    &opened.identity,
                    &[node.public_key(), opened.inventory.node],
                );
                settings.save(path)?;
                (settings, (transport, owned))
            }
            Err(error) => {
                if owned {
                    transport.close().await;
                }
                return Err(error);
            }
        }
    };
    if (settings.mesh, settings.domain) != (opened.identity.mesh, opened.identity.domain) {
        return Err("the enrolled domain differs from the backup's".into());
    }
    for (name, staged) in &opened.files {
        if name != "identity.json" {
            std::fs::rename(staged, directory.join(name))?;
        }
    }
    std::fs::remove_dir_all(&staging)?;
    Ok((settings, Some(link)))
}

async fn run(
    setup: Setup,
    network: Option<Network>,
    mut input: mpsc::Receiver<Command>,
    hello: &std::sync::mpsc::SyncSender<Greeting>,
) -> Result<(), Failure> {
    let directory = setup.directory.clone();
    rocky_storage::database::refuse_old_row_identity(&directory.join("rocky.sqlite")).await?;
    std::fs::create_dir_all(&directory)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("sdk.lock"))?;
    lock.try_lock()
        .map_err(|_| "data directory is already in use")?;
    rocky_storage::database::refuse_old_row_identity(&directory.join("rocky.sqlite")).await?;
    let path = directory.join("identity.json");
    // Record checkpoint-first intent before enrollment can persist the identity.
    if matches!(setup.install, Install::Join { .. }) && !path.exists() {
        rocky_credentials::write_private(&directory.join("bootstrap.json"), b"true")?;
    }
    let (mut settings, link) = identity(&path, &setup, network).await?;
    let vault = settings.vault(&setup.secrets);
    let opened = async {
        // A revocation interrupted after its intent was saved completes
        // here; the replica adopts its policy and keys as it opens.
        if !settings.rotate(&vault)?.is_empty() {
            settings.save(&path)?;
        }
        // Expired credentials cannot open the replica: renew them first.
        if settings.expires() <= now()? {
            let transport = link.as_ref().map(|(transport, _)| &**transport);
            if transport.is_none() && !settings.issuer() {
                return Err(ServiceError::CredentialsExpired.into());
            }
            let renewed = settings
                .request_renewal(&vault, transport, false, None)
                .await?;
            if path.exists() {
                if renewed.policy.statement.epoch > settings.policy.statement.epoch {
                    rocky_credentials::write_private(
                        &directory.join("bootstrap.json"),
                        &serde_json::to_vec(&renewed.policy.statement.epoch)?,
                    )?;
                }
                settings.adopt(&path, &vault, &renewed)?;
            } else {
                settings.policy = renewed.policy;
                settings.credentials = renewed.chain;
            }
        }
        // The handler runs on a thread of its own, outside this reactor.
        let host = match setup
            .commands
            .as_ref()
            .and_then(|commands| Some((commands.handler.clone()?, commands.deadline)))
        {
            Some((handler, deadline)) => {
                let deadline = deadline.unwrap_or(HANDLER_DEADLINE);
                Some(Hosted::start(handler, deadline)? as Arc<dyn rocky_core::CommandHost>)
            }
            None => None,
        };
        let domain = settings.domain()?.hosted(host);
        // A restored file is rebound to this device's identity first.
        let marker = directory.join(crate::backup::MARKER);
        let replica = if marker.exists() {
            let restored: crate::backup::Marker = serde_json::from_slice(&std::fs::read(&marker)?)?;
            let replica = EncryptedReplica::open_restored(
                directory.join("rocky.sqlite"),
                domain.app.clone(),
                settings.config(&vault, &domain)?,
                settings.upgrade(&domain)?,
                restored.predecessor,
            )
            .await?;
            std::fs::remove_file(&marker)?;
            replica
        } else {
            EncryptedReplica::open_upgrading(
                directory.join("rocky.sqlite"),
                domain.app.clone(),
                settings.config(&vault, &domain)?,
                settings.upgrade(&domain)?,
            )
            .await?
        };
        if let Err(error) = replica
            .install_tables(domain.logical().clone(), domain.scope, domain.readers())
            .await
        {
            drop(replica);
            return Err(error.into());
        }
        for record in settings
            .partitions
            .values()
            .filter(|record| settings.granted(&domain).contains(&record.scope))
        {
            replica
                .register_partition(record.scope, record.tuple.clone())
                .await?;
        }
        for (key, chain) in settings.row_authors(&vault)? {
            replica.register_row_author(key, chain).await?;
        }
        for scope in settings.granted(&domain) {
            if replica.retention_policy(scope).await?.is_none() {
                replica
                    .set_retention_policy(
                        scope,
                        rocky_secure::RetentionPolicy {
                            copies: 1,
                            window: 256,
                            ..Default::default()
                        },
                    )
                    .await?;
            }
        }
        #[cfg_attr(not(feature = "iroh"), allow(unused_mut))]
        let mut acquire: Option<crate::node::Acquire> = None;
        #[cfg_attr(not(feature = "iroh"), allow(unused_mut))]
        let mut service: Option<crate::owner::Service> = None;
        let coordinator = match setup.commands.as_ref().map(|c| &c.coordinator) {
            None => None,
            Some(crate::Coordination::Composed(coordinator)) => Some(coordinator.clone()),
            #[cfg(feature = "iroh")]
            Some(crate::Coordination::Service(bundle)) => {
                let transport = link
                    .as_ref()
                    .map(|(transport, _)| transport.clone())
                    .ok_or("a coordination service is reached over the database's network")?;
                let key = rocky_credentials::PrivateKey::from_bytes(&vault.get("node")?);
                service = Some(crate::owner::Service::new(
                    (**bundle).clone(),
                    directory.to_path_buf(),
                ));
                let client = Arc::new(rocky_coordinator::service::NetworkCoordinator::connect(
                    bundle,
                    key,
                    directory.join("coordinator.evidence"),
                    transport,
                )?);
                let acquiring = client.clone();
                acquire = Some(Arc::new(move |scope, epoch| {
                    let client = acquiring.clone();
                    Box::pin(async move {
                        let id = rocky_core::commitment(
                            "rocky.sdk-acquire.v1",
                            &[&scope.domain.0[..], &epoch.to_le_bytes()].concat(),
                        );
                        client.acquire(scope, id).await.map(|_| ())
                    })
                }));
                Some(client as Arc<dyn rocky_core::Coordinator>)
            }
        };
        settings.save(&path)?;
        Ok::<_, Failure>((domain, replica, coordinator, acquire, service))
    }
    .await;
    let (domain, replica, coordinator, acquire, service) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            if let Some((transport, true)) = &link {
                transport.close().await;
            }
            if matches!(setup.install, Install::Create { .. }) {
                let _ = vault.forget(settings.roles());
            }
            return Err(error);
        }
    };
    let scope = serde_json::to_string(&domain.scope)?;
    let node = Arc::new(
        Node::new(
            replica.clone(),
            domain,
            vault,
            settings,
            path.clone(),
            coordinator,
            acquire,
        )
        .with_service(service),
    );
    let network = match link {
        Some((transport, owned)) => Some(node.link(transport, owned).await),
        None => None,
    };
    hello
        .send(Ok((scope, network)))
        .map_err(|_| "database opener abandoned")?;
    if directory.join("bootstrap.json").exists() && network.is_some() {
        let joining = node.clone();
        tokio::task::spawn_local(async move {
            // Failure leaves the durable bootstrap intent for the next sync/open.
            let _ = joining.initial_sync().await;
        });
    }
    let budget = Arc::new(Semaphore::new(128));
    let connections = Arc::new(connection::Registry::default());
    let mut tasks = tokio::task::JoinSet::new();
    let mut shutdown = None;
    while let Some(command) = input.recv().await {
        let node = node.clone();
        match command {
            Command::Call {
                session,
                bytes,
                ending,
                mut reply,
            } => {
                let Ok(permit) = budget.clone().try_acquire_owned() else {
                    let _ = reply.send(json!({
                        "version": 1, "status": "error",
                        "error": {"code": "quota", "retryable": true, "message": "active call quota"},
                    }).to_string().into_bytes());
                    continue;
                };
                // Only a subscription's wait stops when its caller goes
                // away; admitted mutations run to completion.
                let waits = serde_json::from_slice::<Value>(&bytes).is_ok_and(|call| {
                    ["next", "status_next", "sync_next"]
                        .contains(&call["request"]["type"].as_str().unwrap_or(""))
                });
                let client = if waits && ending.load(Ordering::SeqCst) {
                    Err(ServiceError::Closed("session").into())
                } else {
                    node.client(session)
                };
                tasks.spawn_local(async move {
                    let _permit = permit;
                    let response = if waits {
                        tokio::select! {
                            response = dispatch(&bytes, client, &node) => response,
                            () = reply.closed() => return,
                        }
                    } else {
                        dispatch(&bytes, client, &node).await
                    };
                    if ending.load(Ordering::SeqCst) {
                        node.end_sessions(Some(session));
                    }
                    let _ = reply.send(response);
                });
            }
            Command::End(session, reply) => {
                node.end_sessions(Some(session));
                let _ = reply.send(());
            }
            Command::Attach(network, reply) => {
                tasks.spawn_local(async move {
                    let _ = reply.send(node.attach(network).await);
                });
            }
            Command::Detach(id, reply) => {
                tasks.spawn_local(async move {
                    node.detach(Some(id)).await;
                    let _ = reply.send(());
                });
            }
            Command::Close(reply) => {
                shutdown = Some(reply);
                break;
            }
            Command::Connect(options, reply) => {
                let connections = connections.clone();
                tasks.spawn_local(async move {
                    let _ = reply.send(connections.connect(node, options).await);
                });
            }
            Command::Sql {
                connection,
                call,
                reply,
            } => match connections.get(connection) {
                Ok(state) => {
                    tasks.spawn_local(async move {
                        let _ = reply.send(connection::serve(state, call).await);
                    });
                }
                Err(error) => drop(reply.send(Err(connection::sql_failure(error)))),
            },
            Command::Disconnect(connection, reply) => {
                connections.close(Some(connection));
                let _ = reply.send(());
            }
            Command::Observe(tables, reply) => {
                tasks.spawn_local(async move {
                    let tables: Option<Vec<&str>> = tables
                        .as_ref()
                        .map(|tables| tables.iter().map(String::as_str).collect());
                    let observed = node
                        .replica
                        .observe_tables(tables.as_deref())
                        .await
                        .map_err(Failure::from);
                    let _ = reply.send(observed);
                });
            }
        }
        while tasks.try_join_next().is_some() {}
    }
    node.end_sessions(None);
    // Open transactions roll back and cursors end before the file closes.
    connections.close(None);
    // Calls that use the network end within its drain bound; an attach
    // still pending is detached by the second pass.
    node.detach(None).await;
    while tasks.join_next().await.is_some() {}
    node.detach(None).await;
    let purge = *node.ended.lock().await == Some(true);
    drop(node);
    // Dropping the last handle closes the file.
    let mut result = replica.flush().await.map(drop).map_err(|e| e.to_string());
    drop(replica);
    if purge && result.is_ok() {
        result = purge_directory(&directory).map_err(|e| e.to_string());
    }
    if let Some(reply) = shutdown {
        let _ = reply.send(result);
    }
    Ok(())
}
/// Removes what a logged-out device stored, keeping only the lock.
fn purge_directory(directory: &Path) -> Result<(), Failure> {
    for name in [
        "identity.json",
        "rocky.sqlite",
        "rocky.sqlite-wal",
        "rocky.sqlite-shm",
    ] {
        match std::fs::remove_file(directory.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    match std::fs::remove_dir_all(directory.join("secrets")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}
async fn dispatch(bytes: &[u8], client: Result<Arc<Client>, Failure>, node: &Node) -> Vec<u8> {
    let result: Result<Value, Failure> = async {
        let call: Value = serde_json::from_slice(bytes)?;
        if call["version"] != 1 || call["scope"] != serde_json::to_value(node.domain.scope)? {
            return Err("foreign scope or SDK version".into());
        }
        node.dispatch(&*client?, &call["request"]).await
    }
    .await;
    let response = match result {
        Ok(value) => json!({"version": 1, "status": "ok", "value": value}),
        Err(error) => json!({"version": 1, "status": "error", "error": node::error(&error)}),
    };
    response.to_string().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn old_database_is_refused_before_enrollment_or_database_writes() -> Result<(), Failure> {
        const SCHEMA: &str = r#"{"format":2,"application":"refusal","version":1,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"]}]}"#;
        for format in [1, 0, 4] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("rocky.sqlite");
            let conn = rocky_storage::sqlite::Connection::open(&path)?;
            conn.execute("CREATE TABLE secure_typed(descriptor BLOB NOT NULL)", ())
                .await?;
            conn.execute("CREATE TABLE secure_metadata(identity BLOB)", ())
                .await?;
            conn.execute(
                "INSERT INTO secure_typed VALUES(?)",
                [format!("{{\"format\":{format}}}").into_bytes()],
            )
            .await?;
            conn.execute("CREATE TABLE sentinel(value TEXT)", ())
                .await?;
            conn.execute("INSERT INTO sentinel VALUES('retained')", ())
                .await?;
            drop(conn);
            let before = std::fs::read(&path)?;
            for create in [false, true] {
                let options = Open {
                    directory: directory.path(),
                    schema: SCHEMA,
                    secrets: None,
                    network: None,
                    commands: None,
                };
                let error = if create {
                    Database::create(options, None)
                } else {
                    Database::open(options)
                }
                .err()
                .ok_or("old database admitted")?
                .to_string();
                assert!(
                    error.contains("row identity")
                        && error.contains("export")
                        && error.contains("reinstall"),
                    "{error}"
                );
                assert_eq!(std::fs::read(&path)?, before);
                for name in [
                    "sdk.lock",
                    "identity.json",
                    "bootstrap.json",
                    "secrets",
                    "rocky.sqlite-wal",
                ] {
                    assert!(!directory.path().join(name).exists(), "{name}");
                }
            }
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn populated_numeric_consumer_restores_pseudonym_after_reopen() -> Result<(), Failure> {
        const SCHEMA: &str = r#"{"application":"reopen_numeric","version":1,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"]}]}"#;
        async fn call(
            database: &Database,
            session: &Session,
            request: Value,
        ) -> Result<Value, Failure> {
            let reply: Value = serde_json::from_str(&session.call(json!({
                "version": 1, "scope": serde_json::from_str::<Value>(&database.scope())?,
                "request": request,
            }).to_string()).await?)?;
            if reply["status"] != "ok" {
                return Err(format!("consumer request refused: {reply}").into());
            }
            Ok(reply["value"].clone())
        }
        let directory = tempfile::tempdir()?;
        let options = || Open {
            directory: directory.path(),
            schema: SCHEMA,
            secrets: None,
            network: None,
            commands: None,
        };
        let database = Database::create(options(), None)?.database;
        let session = database.session();
        let request = call(
            &database,
            &session,
            json!({"type":"pseudonym_request","collection":"items"}),
        )
        .await?;
        let assignment = call(
            &database,
            &session,
            json!({"type":"pseudonym_assign","request":request["request"]}),
        )
        .await?;
        call(
            &database,
            &session,
            json!({"type":"pseudonym_install","pseudonym":assignment["pseudonym"]}),
        )
        .await?;
        call(&database, &session, json!({"type":"sql","sql":"BEGIN"})).await?;
        let first = call(
            &database,
            &session,
            json!({"type":"next_id","table":"items"}),
        )
        .await?["id"]
            .as_str()
            .ok_or("missing id")?
            .to_owned();
        call(&database, &session, json!({"type":"sql","sql":"INSERT INTO items(id,title) VALUES(?,'first')","values":[{"type":"integer","value":first}]})).await?;
        call(&database, &session, json!({"type":"sql","sql":"COMMIT"})).await?;
        session.close().await;
        database.close().await?;

        let database = Database::open(options())?.database;
        let session = database.session();
        let rows = call(
            &database,
            &session,
            json!({"type":"read","query":{"sql":"SELECT id,row_kind FROM items","max_rows":8}}),
        )
        .await?;
        assert_eq!(rows["rows"][0][0]["value"], first);
        assert_eq!(rows["rows"][0][1]["value"], "0");
        call(&database, &session, json!({"type":"sql","sql":"BEGIN"})).await?;
        let second = call(
            &database,
            &session,
            json!({"type":"next_id","table":"items"}),
        )
        .await?["id"]
            .as_str()
            .ok_or("missing id")?
            .to_owned();
        assert_eq!(second.parse::<i64>()?, first.parse::<i64>()? + 1);
        call(&database, &session, json!({"type":"sql","sql":"INSERT INTO items(id,title) VALUES(?,'second')","values":[{"type":"integer","value":second}]})).await?;
        call(&database, &session, json!({"type":"sql","sql":"COMMIT"})).await?;
        let rows = call(
            &database,
            &session,
            json!({"type":"read","query":{"sql":"SELECT id FROM items ORDER BY id","max_rows":8}}),
        )
        .await?;
        assert_eq!(rows["rows"].as_array().ok_or("missing rows")?.len(), 2);
        assert_eq!(rows["rows"][1][0]["value"], second);
        session.close().await;
        database.close().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saturated_waits_cannot_block_session_shutdown() -> Result<(), Failure> {
        let directory = tempfile::tempdir()?;
        let opened = Database::create(
            Open {
                directory: directory.path(),
                schema: r#"{"application":"saturation","version":1,"format":2,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#,
                secrets: None,
                network: None,
                commands: None,
            },
            None,
        )?;
        let database = opened.database;
        let session = database.session();
        let scope: Value = serde_json::from_str(&database.scope())?;
        let status: Value = serde_json::from_str(
            &session
                .call(
                    json!({
                        "version": 1, "scope": scope, "request": {"type": "status"},
                    })
                    .to_string(),
                )
                .await?,
        )?;
        let bytes = json!({
            "version": 1, "scope": scope,
            "request": {"type": "status_next", "after": status["value"]["token"]},
        })
        .to_string()
        .into_bytes();
        let mut replies = Vec::new();
        // Queue every wait before End, without sleeps or scheduler assumptions.
        for _ in 0..129 {
            let (reply, receiver) = oneshot::channel();
            database
                .shared
                .input
                .send(Command::Call {
                    session: session.id,
                    bytes: bytes.clone(),
                    ending: session.closed.clone(),
                    reply,
                })
                .await?;
            replies.push(receiver);
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), session.close()).await?;
        drop(replies);
        database.close().await?;
        Ok(())
    }
}
