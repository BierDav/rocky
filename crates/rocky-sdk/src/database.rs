use crate::{
    Failure,
    bootstrap_intent::BootstrapIntentFile,
    connection::{self, ConnectionOptions, Page, SqlFailure, TableEvents},
    enrollment::{DEFAULT_TTL, NodeKeys, Settings, Supplied, now},
    node::{self, Node},
    pause,
    schema::Schema,
    secrets::{FileSecretStore, SecretStore},
    service::{Network, ServiceError},
};
use rocky_secure::EncryptedReplica;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot};

mod fault;
mod residence;

pub struct Open<'a> {
    pub directory: &'a Path,
    pub schema: &'a Schema,
    pub secrets: Option<Arc<dyn SecretStore>>,
    pub identity: Option<Arc<dyn crate::IdentityProvider>>,
    pub network: Option<Network>,
}
pub(crate) struct Opening<'a> {
    pub(crate) directory: &'a Path,
    pub(crate) schema: Option<&'a Schema>,
    pub(crate) secrets: Option<Arc<dyn SecretStore>>,
    pub(crate) identity: Option<Arc<dyn crate::IdentityProvider>>,
    pub(crate) network: Option<Network>,
}
impl<'a> From<Open<'a>> for Opening<'a> {
    fn from(open: Open<'a>) -> Self {
        Self {
            directory: open.directory,
            schema: Some(open.schema),
            secrets: open.secrets,
            identity: open.identity,
            network: open.network,
        }
    }
}
pub struct Opened {
    pub database: Arc<Database>,
    pub network: Option<NetworkService>,
    pub root: Option<crate::RootKey>,
    pub identity: Option<String>,
}

#[cfg_attr(
    target_family = "wasm",
    expect(dead_code, reason = "blocking installation variants are native only")
)]
pub(crate) enum Install {
    Create {
        ttl: u64,
        root: std::sync::Mutex<Option<Arc<dyn crate::IdentityProvider>>>,
    },
    Join {
        provisioning: String,
        single_use: bool,
        phrase: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    },
    Supplied(Box<Supplied>),
    Identity {
        contact: String,
        confirmed: Option<Arc<crate::AwaitingLink>>,
    },
    Invited {
        invitation: String,
    },
    Installed,
}
impl Install {
    fn bootstraps_from_checkpoint(&self) -> bool {
        matches!(
            self,
            Self::Join { .. } | Self::Identity { .. } | Self::Invited { .. }
        )
    }
    fn unwinds_on_failure(&self) -> bool {
        matches!(
            self,
            Self::Create { .. } | Self::Join { .. } | Self::Supplied(_)
        )
    }
    fn discards_secrets(&self) -> bool {
        matches!(self, Self::Create { .. } | Self::Join { .. })
    }
}
struct Setup {
    directory: PathBuf,
    schema: Option<Schema>,
    secrets: Arc<dyn SecretStore>,
    identity: Option<Arc<dyn crate::IdentityProvider>>,
    install: Install,
    confirmation: Option<String>,
    #[cfg(test)]
    expose: Option<std::sync::mpsc::Sender<std::sync::Weak<Node>>>,
}
struct Serving {
    setup: Setup,
    network: Option<Network>,
    input: mpsc::Receiver<Command>,
    cleanups: mpsc::UnboundedReceiver<Command>,
    predecessor: residence::Predecessor,
}
struct Senders {
    input: mpsc::Sender<Command>,
    cleanup: mpsc::UnboundedSender<Command>,
    handles: residence::Handles,
}
enum Command {
    Request {
        session: u64,
        ending: Arc<AtomicBool>,
        request: Box<crate::request::Request>,
    },
    #[cfg(all(test, feature = "qualification"))]
    BlockReactor(
        oneshot::Sender<()>,
        std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    ),
    End(u64, oneshot::Sender<()>),
    Attach(Network, oneshot::Sender<Result<u64, Failure>>),
    Detach(u64, oneshot::Sender<()>),
    Close(oneshot::Sender<Result<(), String>>),
    Connect {
        options: ConnectionOptions,
        shared: Arc<Shared>,
        #[cfg(feature = "qualification")]
        queued: web_time::Instant,
        reply: oneshot::Sender<Result<Connection, Failure>>,
    },
    Query {
        sql: String,
        params: Vec<crate::SqlValue>,
        shared: Arc<Shared>,
        reply: oneshot::Sender<Result<(Page, Option<Connection>), Failure>>,
    },
    Sql {
        connection: u64,
        call: connection::Call,
        #[cfg(feature = "qualification")]
        queued: web_time::Instant,
        reply: oneshot::Sender<Result<connection::Reply, SqlFailure>>,
    },
    Disconnect {
        connection: u64,
        #[cfg(feature = "qualification")]
        queued: web_time::Instant,
        reply: oneshot::Sender<()>,
    },
    #[cfg(feature = "qualification")]
    LockMeasurements(oneshot::Sender<rocky_secure::LockMeasurements>),
    #[cfg(feature = "qualification")]
    ReactorMeasurements(oneshot::Sender<Option<crate::ReactorMeasurements>>),
    Observe(
        Option<Vec<String>>,
        oneshot::Sender<Result<rocky_storage::observe::TableObserver, Failure>>,
    ),
    Outcome(
        rocky_core::TransactionToken,
        oneshot::Sender<Result<Option<rocky_core::replay::TransactionOutcome>, Failure>>,
    ),
    Flush(oneshot::Sender<Result<(), Failure>>),
}
type Greeting = Result<Ready, Failure>;
type Ready = (rocky_core::Scope, Option<u64>);
enum Hello {
    #[cfg(not(target_family = "wasm"))]
    Thread(std::sync::mpsc::SyncSender<Greeting>),
    Task(std::sync::Mutex<Option<oneshot::Sender<Greeting>>>),
}
impl Hello {
    fn send(&self, greeting: Greeting) -> Result<(), Failure> {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Thread(sender) => sender
                .send(greeting)
                .map_err(|_| "database opener abandoned")?,
            Self::Task(sender) => sender
                .lock()
                .map_err(|_| "greeting poisoned")?
                .take()
                .ok_or("greeted twice")?
                .send(greeting)
                .map_err(|_| "database opener abandoned")?,
        }
        Ok(())
    }
}

const ACTIVE_CALL_QUOTA: usize = 128;

struct Shared {
    input: mpsc::Sender<Command>,
    cleanup: mpsc::UnboundedSender<Command>,
    budget: Semaphore,
    closed: AtomicBool,
    sessions: AtomicU64,
    _handles: residence::Handles,
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

pub struct Database {
    shared: Arc<Shared>,
    scope: rocky_core::Scope,
    worker: Mutex<Option<Worker>>,
}
enum Worker {
    #[cfg(not(target_family = "wasm"))]
    Thread {
        thread: std::thread::JoinHandle<()>,
        ended: oneshot::Receiver<()>,
    },
    #[cfg(any(test, target_family = "wasm"))]
    Task(rocky_platform::task::JoinHandle<()>),
}
#[cfg(all(test, not(target_family = "wasm")))]
type HeldEnd = (oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);
#[cfg(all(test, not(target_family = "wasm")))]
const HELD_END_HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(all(test, not(target_family = "wasm")))]
thread_local! {
    static HOLD_END: std::cell::RefCell<Option<HeldEnd>> = const { std::cell::RefCell::new(None) };
}
impl Worker {
    #[cfg(not(target_family = "wasm"))]
    fn serving_thread(
        serving: Serving,
        claim: residence::Claim,
        hello: Hello,
    ) -> std::io::Result<Self> {
        let (end, ended) = oneshot::channel();
        #[cfg(test)]
        let held = HOLD_END.with(|hold| hold.borrow_mut().take());
        let thread = std::thread::Builder::new()
            .name("rocky-sdk".into())
            .stack_size(rocky_platform::task::THREAD_STACK)
            .spawn(move || {
                serve_on_thread(serving, &hello);
                drop(claim);
                #[cfg(test)]
                if let Some((reached, release)) = held {
                    let _ = reached.send(());
                    let _ = release.recv_timeout(HELD_END_HANG_BOUND);
                }
                let _ = end.send(());
            })?;
        Ok(Self::Thread { thread, ended })
    }
    #[cfg(any(test, target_family = "wasm"))]
    fn local(serving: Serving, claim: residence::Claim, hello: Hello) -> std::io::Result<Self> {
        Ok(Self::Task(crate::reactor::spawn_local(
            "database",
            async move {
                if let Err(error) = run(serving, &hello).await {
                    let _ = hello.send(Err(error));
                }
                drop(claim);
            },
        )))
    }
    async fn join(self) -> Result<(), Failure> {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Thread { ended, .. } => {
                ended.await.map_err(|_| "database thread failed")?;
            }
            #[cfg(any(test, target_family = "wasm"))]
            Self::Task(task) => task.await.map_err(|_| "database task failed")?,
        }
        Ok(())
    }
    #[cfg(not(target_family = "wasm"))]
    fn release(self) {
        match self {
            Self::Thread { thread, .. } => drop(thread.join()),
            #[cfg(test)]
            Self::Task(_) => {}
        }
    }
}
#[cfg(not(target_family = "wasm"))]
impl Database {
    pub fn create(open: Open<'_>, credential_ttl: Option<u64>) -> Result<Opened, Failure> {
        let root = crate::RootKey::generate();
        let created = crate::RootKey::from_secret(root.secret());
        let mut opened = Self::start(
            open.into(),
            Self::create_install(credential_ttl, Arc::new(root))?,
        )?;
        opened.root = Some(created);
        Ok(opened)
    }
    pub fn create_with_root(
        open: Open<'_>,
        credential_ttl: Option<u64>,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<Opened, Failure> {
        Self::start(open.into(), Self::create_install(credential_ttl, root)?)
    }
    pub fn join(open: Open<'_>, provisioning: &str) -> Result<Opened, Failure> {
        let single_use = open.identity.is_none();
        let (install, phrase) = Self::join_install(provisioning, single_use);
        let mut opened = Self::start(open.into(), install)?;
        if let Ok(mut guard) = phrase.lock() {
            opened.identity = guard.take();
        }
        Ok(opened)
    }
    pub fn enroll_identity(open: Open<'_>, contact: &str) -> Result<Opened, Failure> {
        if open.network.is_none() {
            return Err(ServiceError::NetworkRequired("enroll an identity's node").into());
        }
        if open.identity.is_none() {
            return Err("another node of an identity needs that identity's provider".into());
        }
        Self::start(
            open.into(),
            Install::Identity {
                contact: contact.into(),
                confirmed: None,
            },
        )
    }
    pub fn accept_invitation(open: Open<'_>, invitation: &str) -> Result<Opened, Failure> {
        Self::start(open.into(), Self::invited_install(invitation))
    }
    pub fn install(open: Open<'_>, supplied: Supplied) -> Result<Opened, Failure> {
        Self::start(open.into(), Install::Supplied(Box::new(supplied)))
    }
    pub fn open(open: Open<'_>) -> Result<Opened, Failure> {
        Self::start(open.into(), Install::Installed)
    }
    pub fn open_confirmed(open: Open<'_>, digest: &str) -> Result<Opened, Failure> {
        Self::start_confirmed(open.into(), Install::Installed, Some(digest.into()))
    }
    fn start(open: Opening<'_>, install: Install) -> Result<Opened, Failure> {
        Self::start_confirmed(open, install, None)
    }
    fn start_confirmed(
        open: Opening<'_>,
        install: Install,
        confirmation: Option<String>,
    ) -> Result<Opened, Failure> {
        let (serving, claim, senders) = Self::serving(open, install, confirmation)?;
        let (hello, greeting) = std::sync::mpsc::sync_channel::<Greeting>(1);
        let worker = Worker::serving_thread(serving, claim, Hello::Thread(hello))?;
        match greeting.recv().map_err(|_| "database failed to start")? {
            Ok(ready) => Ok(Self::opened(ready, senders, worker)),
            Err(error) => {
                worker.release();
                Err(error)
            }
        }
    }
}
impl Database {
    #[cfg(feature = "qualification")]
    pub async fn lock_measurements(&self) -> Result<rocky_secure::LockMeasurements, Failure> {
        Ok(self.shared.request(Command::LockMeasurements).await?)
    }
    #[cfg(feature = "qualification")]
    pub async fn reactor_measurements(&self) -> Result<crate::ReactorMeasurements, Failure> {
        self.shared
            .request(Command::ReactorMeasurements)
            .await?
            .ok_or_else(|| "reactor measurement context missing".into())
    }
    fn create_install(
        credential_ttl: Option<u64>,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<Install, Failure> {
        let ttl = credential_ttl.unwrap_or(DEFAULT_TTL);
        if !CREDENTIAL_TTL_SECONDS.contains(&ttl) {
            return Err("credential lifetime must be between a minute and a year".into());
        }
        Ok(Install::Create {
            ttl,
            root: std::sync::Mutex::new(Some(root)),
        })
    }
    fn join_install(
        provisioning: &str,
        single_use: bool,
    ) -> (Install, Arc<std::sync::Mutex<Option<String>>>) {
        let phrase = Arc::new(std::sync::Mutex::new(None));
        (
            Install::Join {
                provisioning: provisioning.into(),
                single_use,
                phrase: phrase.clone(),
            },
            phrase,
        )
    }
    pub fn node_keys_of(
        directory: &Path,
        secrets: Option<Arc<dyn SecretStore>>,
        identity: Option<Arc<dyn crate::IdentityProvider>>,
    ) -> Result<NodeKeys, Failure> {
        rocky_platform::fs::create_dir_all(directory)?;
        let path = directory.join(crate::enrollment::REPLICA_FILE);
        if Settings::exists(&path)? {
            return Err("a domain exists in this directory: open it instead".into());
        }
        let secrets =
            secrets.unwrap_or_else(|| Arc::new(FileSecretStore::new(directory.join("secrets"))));
        Settings::prepare_install(&path, &secrets, identity)
    }
    async fn launch(
        open: Opening<'_>,
        install: Install,
        confirmation: Option<String>,
    ) -> Result<Opened, Failure> {
        #[cfg(not(target_family = "wasm"))]
        let place = Worker::serving_thread;
        #[cfg(target_family = "wasm")]
        let place = Worker::local;
        Self::launch_in(open, install, confirmation, place).await
    }
    async fn launch_in(
        open: Opening<'_>,
        install: Install,
        confirmation: Option<String>,
        place: fn(Serving, residence::Claim, Hello) -> std::io::Result<Worker>,
    ) -> Result<Opened, Failure> {
        let (serving, claim, senders) = Self::serving(open, install, confirmation)?;
        #[cfg(target_family = "wasm")]
        rocky_platform::fs::prepare(&serving.setup.directory.join("rocky.sqlite")).await?;
        let (hello, greeting) = oneshot::channel();
        let worker = place(
            serving,
            claim,
            Hello::Task(std::sync::Mutex::new(Some(hello))),
        )?;
        match greeting
            .await
            .unwrap_or_else(|_| Err("database failed to start".into()))
        {
            Ok(ready) => Ok(Self::opened(ready, senders, worker)),
            Err(error) => {
                let _ = worker.join().await;
                Err(error)
            }
        }
    }
    pub(crate) async fn launch_create(
        open: Opening<'_>,
        credential_ttl: Option<u64>,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<Opened, Failure> {
        let install = Self::create_install(credential_ttl, root)?;
        Self::launch(open, install, None).await
    }
    pub(crate) async fn launch_join(
        open: Opening<'_>,
        provisioning: &str,
        single_use: bool,
    ) -> Result<Opened, Failure> {
        let (install, phrase) = Self::join_install(provisioning, single_use);
        let mut opened = Self::launch(open, install, None).await?;
        opened.identity = phrase.lock().ok().and_then(|mut guard| guard.take());
        Ok(opened)
    }
    fn invited_install(invitation: &str) -> Install {
        Install::Invited {
            invitation: invitation.into(),
        }
    }
    pub(crate) async fn launch_invited(
        open: Opening<'_>,
        invitation: &str,
    ) -> Result<Opened, Failure> {
        Self::launch(open, Self::invited_install(invitation), None).await
    }
    pub(crate) async fn launch_link(
        open: Opening<'_>,
        contact: &str,
        confirmed: Option<Arc<crate::AwaitingLink>>,
    ) -> Result<Opened, Failure> {
        Self::launch(
            open,
            Install::Identity {
                contact: contact.into(),
                confirmed,
            },
            None,
        )
        .await
    }
    #[cfg(not(target_family = "wasm"))]
    pub fn confirm_link(
        open: Open<'_>,
        contact: &str,
        pending: Arc<crate::AwaitingLink>,
        code: &str,
    ) -> Result<Opened, Failure> {
        if pending.code != code {
            return Err("link code does not match".into());
        }
        Self::start(
            open.into(),
            Install::Identity {
                contact: contact.into(),
                confirmed: Some(pending),
            },
        )
    }
    pub(crate) async fn launch_open(open: Opening<'_>) -> Result<Opened, Failure> {
        Self::launch(open, Install::Installed, None).await
    }
    fn setup(
        open: Opening<'_>,
        install: Install,
        confirmation: Option<String>,
    ) -> Result<(Setup, Option<Network>), Failure> {
        let directory = open.directory.to_path_buf();
        if let Install::Join { provisioning, .. } = &install {
            crate::enrollment::ProvisioningBundle::parse(provisioning)?;
        }
        if let Install::Invited { invitation } = &install {
            crate::invitation::check_uri(invitation)?;
            if open.network.is_none() {
                return Err(ServiceError::NetworkRequired("accept an invitation").into());
            }
            if open.identity.is_none() {
                return Err("accepting an invitation needs the invited identity's provider".into());
            }
        }
        if open.schema.is_none() && matches!(install, Install::Create { .. } | Install::Supplied(_))
        {
            return Err(crate::StoreOnlyRefusal::NeedsSchema.into());
        }
        let setup = Setup {
            schema: open.schema.map(Schema::validated).transpose()?,
            secrets: open
                .secrets
                .unwrap_or_else(|| Arc::new(FileSecretStore::new(directory.join("secrets")))),
            identity: open.identity,
            directory,
            install,
            confirmation,
            #[cfg(test)]
            expose: None,
        };
        Ok((setup, open.network))
    }
    fn serving(
        open: Opening<'_>,
        install: Install,
        confirmation: Option<String>,
    ) -> Result<(Serving, residence::Claim, Senders), Failure> {
        let (setup, network) = Self::setup(open, install, confirmation)?;
        let claimed = residence::claim(&setup.directory)?;
        let (input, receiver) = mpsc::channel(32);
        let (cleanup, cleanups) = mpsc::unbounded_channel();
        Ok((
            Serving {
                setup,
                network,
                input: receiver,
                cleanups,
                predecessor: claimed.predecessor,
            },
            claimed.claim,
            Senders {
                input,
                cleanup,
                handles: claimed.handles,
            },
        ))
    }
    fn opened((scope, network): Ready, senders: Senders, worker: Worker) -> Opened {
        let shared = Arc::new(Shared {
            input: senders.input,
            cleanup: senders.cleanup,
            budget: Semaphore::new(ACTIVE_CALL_QUOTA),
            closed: AtomicBool::new(false),
            sessions: AtomicU64::new(1),
            _handles: senders.handles,
        });
        Opened {
            root: None,
            identity: None,
            network: network.map(|id| NetworkService::new(&shared, id)),
            database: Arc::new(Self {
                shared,
                scope,
                worker: Mutex::new(Some(worker)),
            }),
        }
    }
    pub fn scope(&self) -> rocky_core::Scope {
        self.scope
    }
    pub fn session(&self) -> Session {
        Session {
            shared: self.shared.clone(),
            id: self.shared.sessions.fetch_add(1, Ordering::Relaxed),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }
    pub async fn attach(&self, network: Network) -> Result<NetworkService, Failure> {
        let id = self
            .shared
            .request(|reply| Command::Attach(network, reply))
            .await??;
        Ok(NetworkService::new(&self.shared, id))
    }
    pub async fn connect(&self, options: ConnectionOptions) -> Result<Connection, Failure> {
        self.shared
            .request(|reply| Command::Connect {
                options,
                shared: self.shared.clone(),
                #[cfg(feature = "qualification")]
                queued: web_time::Instant::now(),
                reply,
            })
            .await?
    }
    pub(crate) async fn query_once(
        &self,
        sql: String,
        params: Vec<crate::SqlValue>,
    ) -> Result<(Page, Option<Connection>), Failure> {
        self.shared
            .request(|reply| Command::Query {
                sql,
                params,
                shared: self.shared.clone(),
                reply,
            })
            .await?
    }
    pub async fn transaction_outcome(
        &self,
        token: rocky_core::TransactionToken,
    ) -> Result<Option<rocky_core::replay::TransactionOutcome>, Failure> {
        self.shared
            .request(|reply| Command::Outcome(token, reply))
            .await?
    }
    pub async fn flush(&self) -> Result<(), Failure> {
        self.shared.request(Command::Flush).await?
    }
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
    pub async fn close(&self) -> Result<(), Failure> {
        let mut worker = self.worker.lock().await;
        if let Some(worker) = worker.take() {
            self.shared.closed.store(true, Ordering::SeqCst);
            let result = self.shared.request(Command::Close).await;
            worker.join().await?;
            if let Ok(result) = result {
                result.map_err(|message| -> Failure { message.into() })?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) struct Hosted {
    pub(crate) opened: Opened,
    pub(crate) node: std::sync::Weak<Node>,
}
#[cfg(test)]
impl Database {
    pub(crate) async fn host(
        open: Open<'_>,
        install: Install,
        ahead: Arc<AtomicU64>,
    ) -> Result<Hosted, Failure> {
        let (mut serving, claim, senders) = Self::serving(open.into(), install, None)?;
        let (expose, exposed) = std::sync::mpsc::channel();
        serving.setup.expose = Some(expose);
        let (hello, greeting) = oneshot::channel();
        let hello = Hello::Task(std::sync::Mutex::new(Some(hello)));
        let task = crate::reactor::spawn_local("host", async move {
            let _claim = claim;
            let local = tokio::task::LocalSet::new();
            let mut served = std::pin::pin!(local.run_until(run(serving, &hello)));
            let result = std::future::poll_fn(|cx| {
                let seconds = ahead.load(Ordering::Relaxed);
                if seconds == 0 {
                    return served.as_mut().poll(cx);
                }
                let prior = rocky_secure::skew_clock(seconds);
                let polled = served.as_mut().poll(cx);
                rocky_secure::skew_clock(prior);
                polled
            })
            .await;
            if let Err(error) = result {
                let _ = hello.send(Err(error));
            }
        });
        let greeting = greeting
            .await
            .unwrap_or_else(|_| Err("database failed to start".into()));
        let opened = Self::opened(greeting?, senders, Worker::Task(task));
        let node = exposed
            .try_recv()
            .map_err(|_| "the database exposed no node")?;
        Ok(Hosted { opened, node })
    }
    async fn launch_local(open: Open<'_>, install: Install) -> Result<Opened, Failure> {
        Self::launch_in(open.into(), install, None, Worker::local).await
    }
    pub(crate) async fn crash(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some(Worker::Task(task)) = self.worker.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

pub struct Connection {
    shared: Arc<Shared>,
    id: u64,
    statements: AtomicU64,
    closed: AtomicBool,
}
impl Connection {
    #[cfg(feature = "qualification")]
    pub fn measurement_id(&self) -> u64 {
        self.id
    }
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
                #[cfg(feature = "qualification")]
                queued: web_time::Instant::now(),
                reply,
            })
            .await
            .map_err(|error| connection::failure(connection::code::MISUSE, error.to_string()))?
    }
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
    pub async fn next(&self, statement: u64) -> Result<Page, SqlFailure> {
        match self.call(connection::Call::Next { statement }).await? {
            connection::Reply::Page(page) => Ok(page),
            _ => Err(connection::failure(connection::code::MISUSE, "no page")),
        }
    }
    pub(crate) fn discard(&self, statement: u64) {
        let _ = self.shared.cleanup.send(Command::Sql {
            connection: self.id,
            call: connection::Call::Finish { statement },
            #[cfg(feature = "qualification")]
            queued: web_time::Instant::now(),
            reply: oneshot::channel().0,
        });
    }
    pub async fn finish(&self, statement: u64) -> Result<(), SqlFailure> {
        self.call(connection::Call::Finish { statement })
            .await
            .map(drop)
    }
    pub async fn committed(&self) -> Result<Option<rocky_core::TransactionToken>, SqlFailure> {
        match self.call(connection::Call::Committed).await? {
            connection::Reply::Token(token) => Ok(token),
            _ => Err(connection::failure(connection::code::MISUSE, "no token")),
        }
    }
    pub(crate) async fn begin_identified(
        &self,
    ) -> Result<rocky_core::TransactionToken, SqlFailure> {
        match self.call(connection::Call::Begin).await? {
            connection::Reply::Token(Some(token)) => Ok(token),
            _ => Err(connection::failure(connection::code::MISUSE, "no token")),
        }
    }
    pub async fn in_transaction(&self) -> Result<bool, SqlFailure> {
        match self.call(connection::Call::InTransaction).await? {
            connection::Reply::Flag(flag) => Ok(flag),
            _ => Err(connection::failure(connection::code::MISUSE, "no state")),
        }
    }
    pub async fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self
                .shared
                .request(|reply| Command::Disconnect {
                    connection: self.id,
                    #[cfg(feature = "qualification")]
                    queued: web_time::Instant::now(),
                    reply,
                })
                .await;
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.shared.cleanup.send(Command::Disconnect {
                connection: self.id,
                #[cfg(feature = "qualification")]
                queued: web_time::Instant::now(),
                reply: oneshot::channel().0,
            });
        }
    }
}

pub struct Session {
    shared: Arc<Shared>,
    id: u64,
    closed: Arc<AtomicBool>,
}
impl Session {
    pub(crate) async fn send<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<Result<T, Failure>>) -> crate::request::Request,
    ) -> Result<T, Failure> {
        self.send_lending(None, |_, reply| request(reply)).await
    }
    pub(crate) async fn send_signed<T>(
        &self,
        identity: Option<Arc<dyn crate::IdentityProvider>>,
        request: impl FnOnce(oneshot::Sender<Result<T, Failure>>) -> crate::request::Request,
    ) -> Result<T, Failure> {
        self.send_lending(identity, |loan, reply| match loan {
            Some(loan) => crate::request::Request::WithIdentity(loan, Box::new(request(reply))),
            None => request(reply),
        })
        .await
    }
    pub(crate) async fn send_rooted<T>(
        &self,
        root: Option<Arc<dyn crate::IdentityProvider>>,
        request: impl FnOnce(node::Root, oneshot::Sender<Result<T, Failure>>) -> crate::request::Request,
    ) -> Result<T, Failure> {
        self.send_lending(root, |loan, reply| request(node::Root::lent(loan), reply))
            .await
    }
    async fn send_lending<T>(
        &self,
        provider: Option<Arc<dyn crate::IdentityProvider>>,
        request: impl FnOnce(
            Option<crate::identities::Loan>,
            oneshot::Sender<Result<T, Failure>>,
        ) -> crate::request::Request,
    ) -> Result<T, Failure> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ServiceError::Closed("session").into());
        }
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(ServiceError::Closed("database").into());
        }
        let _permit = self
            .shared
            .budget
            .try_acquire()
            .map_err(|_| "pending call quota")?;
        let (loan, returned) = provider.map(crate::identities::lend).unzip();
        let result = self
            .shared
            .request(|reply| Command::Request {
                session: self.id,
                ending: self.closed.clone(),
                request: Box::new(request(loan, reply)),
            })
            .await;
        if let Some(returned) = returned {
            returned.released().await;
        }
        result?.map_err(crate::CallFailure::boxed)
    }
    pub async fn network(&self) -> Result<crate::NetworkStatus, Failure> {
        self.send(crate::request::Request::Network).await
    }
    pub async fn peers(&self) -> Result<Vec<crate::Peer>, Failure> {
        self.send(crate::request::Request::Peers).await
    }
    pub async fn discovery(&self) -> Result<crate::DiscoveryStatus, Failure> {
        Ok(self.network().await?.discovery)
    }
    pub async fn status(&self) -> Result<crate::DatabaseStatus, Failure> {
        self.send(crate::request::Request::Status).await
    }
    pub async fn status_after(&self, token: String) -> Result<crate::DatabaseStatus, Failure> {
        self.send(|reply| crate::request::Request::StatusAfter(token, reply))
            .await
    }
    pub async fn sync_once(&self) -> Result<crate::ReconciliationReport, Failure> {
        self.send(crate::request::Request::Sync).await
    }
    pub async fn sync_once_with_identity(
        &self,
        identity: Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::ReconciliationReport, Failure> {
        self.send_signed(Some(identity), crate::request::Request::Sync)
            .await
    }
    pub async fn set_networking_enabled(&self, enabled: bool) -> Result<(), Failure> {
        self.send(|reply| crate::request::Request::NetworkingEnabled(enabled, reply))
            .await
    }
    pub async fn set_automatic_sync(&self, enabled: bool) -> Result<(), Failure> {
        self.send(|reply| crate::request::Request::AutomaticSync(enabled, reply))
            .await
    }
    pub async fn enter_foreground(&self) -> Result<(), Failure> {
        self.send(crate::request::Request::Foreground).await
    }
    pub async fn enter_background(&self) -> Result<(), Failure> {
        self.send(crate::request::Request::Background).await
    }
    pub async fn logout(&self, purge_on_close: bool) -> Result<(), Failure> {
        self.send(|reply| crate::request::Request::Logout(purge_on_close, reply))
            .await
    }
    pub async fn trust(
        &self,
        identity: rocky_credentials::PublicKey,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::TrustGrant, Failure> {
        self.send_rooted(Some(root), |root, reply| {
            crate::request::Request::Trust(identity, root, reply)
        })
        .await
    }
    pub async fn revoke_trusted(
        &self,
        identity: rocky_credentials::PublicKey,
        mode: rocky_credentials::RevocationMode,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::RevocationResult, Failure> {
        self.revoke_identity_with_root(identity, mode, Some(root), true)
            .await
    }
    pub async fn revoke_identity(
        &self,
        identity: rocky_credentials::PublicKey,
    ) -> Result<crate::RevocationResult, Failure> {
        self.revoke_identity_with_root(
            identity,
            rocky_credentials::RevocationMode::Lost,
            None,
            true,
        )
        .await
    }
    pub async fn revoke_identity_with_root(
        &self,
        identity: rocky_credentials::PublicKey,
        mode: rocky_credentials::RevocationMode,
        root: Option<Arc<dyn crate::IdentityProvider>>,
        keep: bool,
    ) -> Result<crate::RevocationResult, Failure> {
        self.send_rooted(root, |root, reply| {
            crate::request::Request::RevokeIdentity(identity, mode, root, keep, reply)
        })
        .await
    }
    pub async fn install_trust(&self, grant: &str) -> Result<(), Failure> {
        self.send(|reply| crate::request::Request::InstallTrust(grant.into(), reply))
            .await
    }
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
                .cleanup
                .send(Command::End(self.id, oneshot::channel().0));
        }
    }
}

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
    pub async fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self
                .shared
                .request(|reply| Command::Detach(self.id, reply))
                .await;
        }
    }
}

type Link = (crate::transport::Connected, bool);

async fn identity(
    path: &Path,
    setup: &Setup,
    network: Option<Network>,
) -> Result<(Settings, Option<Link>), Failure> {
    let Setup {
        schema,
        secrets,
        install,
        ..
    } = setup;
    let exists = Settings::exists(path)?;
    if exists && !matches!(install, Install::Installed) {
        return Err("a domain exists in this directory: open it instead".into());
    }
    if !exists && matches!(install, Install::Installed) {
        return Err("no domain in this directory: create or join one".into());
    }
    match install {
        Install::Create { ttl, root, .. } => {
            let root = root
                .lock()
                .map_err(|_| "root poisoned")?
                .take()
                .ok_or(crate::enrollment::PublisherRequired("creating a domain"))?;
            let schema = schema
                .as_ref()
                .ok_or(crate::StoreOnlyRefusal::NeedsSchema)?;
            let settings = Settings::create(schema, secrets, *ttl, setup.identity.clone(), &*root)?;
            drop(root);
            let vault = settings.vault(secrets);
            let started = async {
                let Some(network) = network else {
                    return Ok(None);
                };
                let key = Settings::route_secret(&vault)?;
                Ok::<_, Failure>(Some(network.start(true, key).await?))
            }
            .await;
            match started {
                Ok(link) => Ok((settings, link)),
                Err(error) => {
                    if install.discards_secrets() {
                        let _ = vault.forget(settings.roles());
                    }
                    Err(error)
                }
            }
        }
        Install::Join {
            provisioning,
            single_use,
            phrase,
        } => {
            let (settings, vault, identity_phrase) =
                Settings::install_provisioned(secrets, schema.as_ref(), provisioning, *single_use)
                    .await?;
            let started = async {
                let link = match network {
                    Some(network) => {
                        let key = Settings::route_secret(&vault)?;
                        Some(network.start(settings.issuer(), key).await?)
                    }
                    None => None,
                };
                if let Err(error) = settings.save(path).await {
                    if let Some((transport, true)) = link {
                        transport.close().await;
                    }
                    return Err(error);
                }
                Ok::<_, Failure>(link)
            }
            .await;
            match started {
                Ok(link) => {
                    if let Ok(mut guard) = phrase.lock() {
                        *guard = identity_phrase;
                    }
                    Ok((settings, link))
                }
                Err(error) => {
                    let _ = vault.forget(settings.roles());
                    Err(error)
                }
            }
        }
        Install::Identity { contact, confirmed } => {
            let network =
                network.ok_or(ServiceError::NetworkRequired("enroll an identity's node"))?;
            let provider = setup
                .identity
                .clone()
                .ok_or("another node of an identity needs that identity's provider")?;
            let prepared = Settings::prepare_identity(path, secrets, &provider, contact)?;
            let key = Settings::route_secret(&prepared.1)?;
            let (transport, owned) = network.start(false, key).await?;
            match Settings::enroll_identity(
                path,
                prepared,
                provider,
                contact,
                &*transport,
                confirmed.clone(),
            )
            .await
            {
                Ok(settings) => Ok((settings, Some((transport, owned)))),
                Err(error) => {
                    if owned {
                        transport.close().await;
                    }
                    Err(error)
                }
            }
        }
        Install::Invited { invitation } => {
            let network = network.ok_or(ServiceError::NetworkRequired("accept an invitation"))?;
            let provider = setup
                .identity
                .clone()
                .ok_or("accepting an invitation needs the invited identity's provider")?;
            let prepared = Settings::prepare_invited(path, secrets, &provider, invitation)?;
            let key = Settings::route_secret(&prepared.1)?;
            let (transport, owned) = network.start(false, key).await?;
            match Settings::enroll_invited(
                (path, schema.as_ref()),
                prepared,
                provider,
                invitation,
                &*transport,
            )
            .await
            {
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
            let schema = schema
                .as_ref()
                .ok_or(crate::StoreOnlyRefusal::NeedsSchema)?;
            let settings =
                Settings::install(path, schema, secrets, supplied, setup.identity.clone()).await?;
            let Some(network) = network else {
                return Ok((settings, None));
            };
            let key = Settings::route_secret(&settings.vault(secrets))?;
            Ok((settings, Some(network.start(false, key).await?)))
        }
        Install::Installed => {
            let mut settings = Settings::load(path)?;
            if settings.logged_out {
                return Err("this device logged out; join the domain again".into());
            }
            let vault = settings.vault(secrets);
            let link = match network {
                Some(network) => {
                    let key = Settings::route_secret(&vault)?;
                    Some(network.start(settings.issuer(), key).await?)
                }
                None => None,
            };
            if let Some(schema) = schema
                && settings.application(schema)? == crate::DeclaredSchema::Current
                && settings.schema != *schema
            {
                settings.schema = schema.clone();
                settings.save(path).await?;
            }
            Ok((settings, link))
        }
    }
}

pub(crate) async fn attach(
    replica: &EncryptedReplica<rocky_core::TypedRows>,
    settings: &mut Settings,
    vault: &crate::secrets::Vault,
    domain: &crate::domain::Domain,
) -> Result<bool, Failure> {
    replica
        .install_readable_tables(domain.logical().clone(), domain.scope, domain.readers())
        .await?;
    let recovered =
        settings.recover_groups(vault, domain, &replica.retained_partitions().await?)?;
    install_group_keys(replica, settings, vault, domain).await?;
    if replica.role().await?.role == rocky_secure::ReplicaRole::Partial {
        hold_late_group_members(replica, settings, domain).await?;
    }
    replica
        .register_credential_signer(rocky_credentials::PrivateKey::from_bytes(
            &vault.get("node")?,
        ))
        .await?;
    Ok(recovered)
}

async fn install_group_keys(
    replica: &EncryptedReplica<rocky_core::TypedRows>,
    settings: &Settings,
    vault: &crate::secrets::Vault,
    domain: &crate::domain::Domain,
) -> Result<(), Failure> {
    let granted = settings.granted(domain);
    for partition in settings.partitions(domain)? {
        for member in &partition.members {
            if granted.contains(&member.scope) {
                replica
                    .register_partition(member.scope, member.tuple.clone(), partition.record.name)
                    .await?;
            }
            if settings.reads_partition(domain, member) {
                let newest_first = partition.record.epochs.iter().rev();
                for epoch in newest_first {
                    replica
                        .install_key_epoch(partition.key(vault, member, *epoch)?)
                        .await?;
                }
            }
        }
    }
    Ok(())
}

async fn hold_late_group_members(
    replica: &EncryptedReplica<rocky_core::TypedRows>,
    settings: &Settings,
    domain: &crate::domain::Domain,
) -> Result<(), Failure> {
    for partition in settings.partitions(domain)? {
        let mut held = false;
        let mut unrecorded = Vec::new();
        for member in &partition.members {
            match replica.subscription(member.scope).await? {
                Some(rocky_secure::Holding::Held) => held = true,
                None if settings.reads_partition(domain, member) => unrecorded.push(member.scope),
                _ => {}
            }
        }
        for scope in unrecorded.into_iter().filter(|_| held) {
            replica.subscribe(scope).await?;
        }
    }
    Ok(())
}

pub(crate) async fn retain(
    replica: &EncryptedReplica<rocky_core::TypedRows>,
    settings: &Settings,
    domain: &crate::domain::Domain,
) -> Result<(), Failure> {
    for scope in settings.granted(domain) {
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
    Ok(())
}

const REPLAYED_INSERT_ROWS: usize = 1024;

pub(crate) async fn insert_rows(
    sql: &rocky_secure::SecureSql<rocky_core::TypedRows>,
    schema: &rocky_core::LogicalSchema,
    table: &str,
    select: &str,
) -> Result<(), rocky_secure::Error> {
    let refused = |reason: String| rocky_secure::Error::Incompatible(reason);
    let logical = schema
        .table(table)
        .ok_or_else(|| refused(format!("{table} is not a table")))?;
    let rows = sql.run(select, Vec::new()).await?;
    let partition = logical
        .partition_by
        .iter()
        .map(|name| {
            rows.columns
                .iter()
                .position(|column| column.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    refused(format!(
                        "the rows for {table} lack its partition column {name}"
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let quoted = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    let width = rows.columns.len() + 1;
    let row = format!("({})", vec!["?"; width].join(","));
    let insert = |count: usize| {
        format!(
            "INSERT INTO {}({},\"id\") VALUES{}",
            quoted(table),
            rows.columns
                .iter()
                .map(|column| quoted(column))
                .collect::<Vec<_>>()
                .join(","),
            vec![row.as_str(); count].join(",")
        )
    };
    let batch = REPLAYED_INSERT_ROWS.min(32_766 / width).max(1);
    let mut values = Vec::new();
    let mut pending = 0;
    let total = rows.rows.len();
    for (index, row) in rows.rows.into_iter().enumerate() {
        let id = if partition.is_empty() {
            sql.next_id(table).await?
        } else {
            let values: Vec<_> = partition.iter().map(|index| row[*index].clone()).collect();
            sql.next_id_partition(table, &values).await?
        };
        values.extend(row);
        values.push(id.sql());
        pending += 1;
        if pending == batch || index + 1 == total {
            sql.run(&insert(pending), std::mem::take(&mut values))
                .await?;
            pending = 0;
        }
    }
    Ok(())
}
async fn confirm_removals(setup: &Setup, directory: &Path, path: &Path) -> Result<(), Failure> {
    let Some(schema) = &setup.schema else {
        return Ok(());
    };
    let settings = Settings::load(path)?;
    let has_tip_schema = rocky_secure::installed_tip_schema(
        &directory.join("rocky.sqlite"),
        settings.domain()?.scope,
        settings.publisher,
        None,
    )
    .await?
    .is_some();
    if has_tip_schema {
        crate::rollout::confirm(&settings, path, schema, setup.confirmation.as_deref()).await?;
    }
    Ok(())
}

async fn adopt_committed_pending_schema(
    settings: &mut Settings,
    vault: &crate::secrets::Vault,
    directory: &Path,
    path: &Path,
) -> Result<(), Failure> {
    let Some(pending) = settings.pending.clone() else {
        return Ok(());
    };
    let Some(installed) = rocky_secure::installed_tip_schema(
        &directory.join("rocky.sqlite"),
        settings.domain()?.scope,
        settings.publisher,
        Some(pending.tip),
    )
    .await?
    else {
        return Ok(());
    };
    let committed = installed.tip == pending.tip
        || (installed.continues && installed.manifest == pending.manifest);
    if !committed {
        return Ok(());
    }
    let manifest = installed.manifest;
    if manifest != pending.manifest {
        return Err("pending schema differs from committed workspace".into());
    }
    let record = crate::tip_schemas::Record::parse(&installed.record)?;
    let domain = record.domain(settings, manifest)?;
    settings.schema = record.schema;
    settings.generation = record.generation;
    settings.engine = record.engine;
    settings.prepare_schema(vault, &domain)?;
    settings.pending = None;
    settings.save(path).await?;
    Ok(())
}

async fn drain_migration_work(
    replica: &EncryptedReplica<rocky_core::TypedRows>,
    scope: rocky_core::Scope,
) -> Result<(), Failure> {
    replica.flush().await?;
    replica.maintain().await;
    replica.finalize_path(scope).await?;
    Ok(())
}

#[cfg(not(target_family = "wasm"))]
fn serve_on_thread(serving: Serving, hello: &Hello) {
    let result = (|| -> Result<(), Failure> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rocky-sdk-io")
            .thread_stack_size(rocky_platform::task::THREAD_STACK)
            .enable_all()
            .build()?;
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(run(serving, hello)))
    })();
    if let Err(error) = result {
        let _ = hello.send(Err(error));
    }
}

async fn run(serving: Serving, hello: &Hello) -> Result<(), Failure> {
    let serving = run_inner(serving, hello);
    #[cfg(feature = "qualification")]
    let serving = crate::reactor::scope(crate::ReactorMeasurements::default(), serving);
    serving.await
}

async fn run_inner(serving: Serving, hello: &Hello) -> Result<(), Failure> {
    let Serving {
        mut setup,
        network,
        mut input,
        mut cleanups,
        predecessor,
    } = serving;
    let directory = setup.directory.clone();
    predecessor.departed(&directory).await?;
    let replica_path = directory.join(crate::enrollment::REPLICA_FILE);
    let wal_index = rocky_storage::database::WalIndex::hold(&replica_path)?;
    if matches!(setup.install, Install::Installed) && Settings::exists(&replica_path)? {
        Settings::load(&replica_path)?;
    }
    rocky_secure::check_replica_format(&replica_path).await?;
    if matches!(setup.install, Install::Installed) {
        if !Settings::exists(&replica_path)? {
            return Err(
                if rocky_platform::fs::exists(directory.join("identity.join.json")) {
                    "a join in this directory is unfinished: join again with its or a newer invitation"
                } else {
                    "no domain in this directory: create or join one"
                }
                .into(),
            );
        }
        Settings::load(&replica_path)?;
    }
    rocky_platform::fs::create_dir_all(&directory)?;
    let _lock = rocky_platform::fs::Owner::acquire(&directory.join("sdk"))
        .await
        .map_err(|_| "data directory is already in use")?;
    rocky_secure::check_replica_format(&replica_path).await?;
    let bootstrap_intent = BootstrapIntentFile::in_directory(&directory);
    let path = replica_path;
    if setup.install.bootstraps_from_checkpoint() && !Settings::exists(&path)? {
        bootstrap_intent.request_checkpoint_import().await?;
    }
    if matches!(setup.install, Install::Installed) {
        confirm_removals(&setup, &directory, &path).await?;
    }
    let fresh = setup.install.unwinds_on_failure() && !Settings::exists(&path)?;
    let (mut settings, link) = match identity(&path, &setup, network).await {
        Ok(installed) => installed,
        Err(error) if fresh => {
            drop(wal_index);
            return Err(unwind(&directory, None, error));
        }
        Err(error) => return Err(error),
    };
    let startup_identity = setup.identity.take();
    let vault = settings.vault(&setup.secrets);
    let opened = async {
        if setup.schema.is_none() && !settings.stores_only() {
            return Err(crate::StoreOnlyRefusal::ReadableAccess.into());
        }
        if let (Install::Join { .. } | Install::Invited { .. }, Some(inviter)) =
            (&setup.install, settings.issuers.first())
        {
            bootstrap_intent.join(*inviter).await?;
        }
        let completed_interrupted_revocation = settings.rotate(&vault)?.is_some();
        if completed_interrupted_revocation {
            settings.save(&path).await?;
        }
        let credentials_expired = settings.expires() <= now()?;
        if credentials_expired {
            let transport = link.as_ref().map(|(transport, _)| &**transport);
            if transport.is_none() && !settings.issuer() {
                return Err(ServiceError::CredentialsExpired.into());
            }
            let renewed = settings
                .request_renewal(&vault, transport, false, None)
                .await?;
            if Settings::exists(&path)? {
                let revokes_new_authors = settings.lacks(&renewed.revocations)?;
                if revokes_new_authors {
                    bootstrap_intent.request_checkpoint_import().await?;
                }
                settings.adopt(&path, &vault, &renewed).await?;
            } else {
                settings.learn_revocations(&renewed.revocations)?;
                settings.credentials = renewed.chain;
            }
        }
        adopt_committed_pending_schema(&mut settings, &vault, &directory, &path).await?;
        let domain = settings.domain()?;
        let replica = if let Some(bootstrap) = &settings.bootstrap {
            bootstrap.verify(&settings)?;
            EncryptedReplica::open_following(
                directory.join("rocky.sqlite"),
                domain.app.clone(),
                settings
                    .config(&vault, &domain)?
                    .with_table_scopes(domain.scopes().map(|(_, scope)| scope).collect()),
                bootstrap.graph.clone(),
                bootstrap.followed.clone(),
            )
            .await?
        } else {
            EncryptedReplica::open(
                directory.join("rocky.sqlite"),
                domain.app.clone(),
                settings
                    .config(&vault, &domain)?
                    .with_table_scopes(domain.scopes().map(|(_, scope)| scope).collect()),
            )
            .await?
        };
        if attach(&replica, &mut settings, &vault, &domain).await? {
            replica
                .put_authority_state(
                    crate::enrollment::AUTHORITY_IDENTITY_KEY,
                    &settings.to_bytes()?,
                    now()?,
                )
                .await?;
        }
        replica
            .record_current_schema(
                crate::tip_schemas::Record::new(
                    &settings.schema,
                    settings.generation,
                    settings.engine,
                )
                .bytes()?,
            )
            .await?;
        settings.bootstrap = None;
        retain(&replica, &settings, &domain).await?;
        replica
            .put_authority_state(
                crate::enrollment::AUTHORITY_IDENTITY_KEY,
                &settings.to_bytes()?,
                now()?,
            )
            .await?;
        Ok::<_, Failure>((domain, replica))
    }
    .await;
    drop(wal_index);
    let (domain, replica) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            if let Some((transport, true)) = &link {
                transport.close().await;
            }
            if fresh {
                let created = setup
                    .install
                    .discards_secrets()
                    .then(|| (&vault, settings.roles()));
                return Err(unwind(&directory, created, error));
            }
            return Err(error);
        }
    };
    let scope = domain.scope;
    let node = Node::new(
        replica.clone(),
        domain,
        vault,
        settings,
        path.clone(),
        bootstrap_intent,
    );
    let node = Arc::new(match &setup.schema {
        Some(schema) => node.with_declared(schema.clone()),
        None => node,
    });
    #[cfg(test)]
    if let Some(expose) = &setup.expose {
        let _ = expose.send(Arc::downgrade(&node));
    }
    let started = async {
        crate::identities::supplying(startup_identity, async {
            node.restore_line_signers().await?;
            node.complete_revocation().await?;
            node.refresh_importing().await?;
            node.try_ensure_tip_node().await;
            if matches!(setup.install, Install::Installed) {
                node.publish_schema_at_open(setup.confirmation.as_deref())
                    .await?;
            }
            Ok::<_, Failure>(())
        })
        .await?;
        if matches!(setup.install, Install::Create { .. }) {
            node.follow_main().await?;
        }
        drain_migration_work(&replica, node.domain().scope).await?;
        fault::check(&directory, fault::Stage::Started)?;
        if matches!(setup.install, Install::Supplied(_)) {
            rocky_platform::fs::remove_file(crate::enrollment::Intent::path(&path, true))?;
        }
        Ok::<_, Failure>(())
    }
    .await;
    if let Err(error) = started {
        if let Some((transport, true)) = &link {
            transport.close().await;
        }
        return Err(if fresh {
            discard(node, replica, &directory, &setup.install, error).await
        } else {
            error
        });
    }
    let network = match link {
        Some((transport, owned)) => Some(node.link(transport, owned).await?),
        None => None,
    };
    if hello.send(Ok((scope, network))).is_err() {
        let abandoned: Failure = "database opener abandoned".into();
        let created_root_undelivered =
            fresh && matches!(setup.install, Install::Create { .. } | Install::Join { .. });
        return Err(if created_root_undelivered {
            discard(node, replica, &directory, &setup.install, abandoned).await
        } else {
            abandoned
        });
    }
    let bootstrapping = (node.bootstrap_intent.exists().await && network.is_some())
    .then(|| {
        let joining = node.clone();
        let held = directory.clone();
        crate::reactor::spawn_local("bootstrap", async move {
            pause::at(&held, pause::Point::Bootstrap).await;
            if let Err(error) = joining.initial_sync().await {
                tracing::debug!(%error, "initial sync failed; its bootstrap intent stays for the next sync or open");
            }
        })
    });
    let budget = Arc::new(Semaphore::new(ACTIVE_CALL_QUOTA));
    let connections = Arc::new(connection::Registry::default());
    let mut tasks = rocky_platform::task::JoinSet::new();
    let mut shutdown = None;
    let settling = crate::reactor::spawn_local("settle_commits", settle_commits(replica.clone()));
    let recovering = crate::reactor::spawn_local("recovery", node.clone().drive_recovery());
    loop {
        let command = tokio::select! {
            command = input.recv() => match command { Some(command) => command, None => break },
            Some(command) = cleanups.recv() => command,
        };
        let node = node.clone();
        match command {
            #[cfg(all(test, feature = "qualification"))]
            Command::BlockReactor(entered, resume) => {
                crate::reactor::spawn_in(&mut tasks, "test_blocking", async move {
                    let _ = entered.send(());
                    let _ = resume.lock().unwrap().recv();
                });
            }
            Command::Request {
                session,
                ending,
                request,
            } => {
                let Ok(permit) = budget.clone().try_acquire_owned() else {
                    request.fail("active call quota".into());
                    continue;
                };
                let client = if request.waits() && ending.load(Ordering::SeqCst) {
                    Err(ServiceError::Closed("session").into())
                } else {
                    node.client(session)
                };
                crate::reactor::spawn_in(&mut tasks, "request", async move {
                    let _permit = permit;
                    node.request(*request, client).await;
                    if ending.load(Ordering::SeqCst) {
                        node.end_sessions(Some(session));
                    }
                });
            }
            Command::End(session, reply) => {
                node.end_sessions(Some(session));
                let _ = reply.send(());
            }
            Command::Attach(network, reply) => {
                crate::reactor::spawn_in(&mut tasks, "attach", async move {
                    let _ = reply.send(node.attach(network).await);
                });
            }
            Command::Detach(id, reply) => {
                crate::reactor::spawn_in(&mut tasks, "detach", async move {
                    node.detach(Some(id)).await;
                    let _ = reply.send(());
                });
            }
            Command::Close(reply) => {
                shutdown = Some(reply);
                break;
            }
            Command::Query {
                sql,
                params,
                shared,
                reply,
            } => {
                let connections = connections.clone();
                crate::reactor::spawn_in(&mut tasks, "query", async move {
                    let result = connections.query_once(node, sql, params).await.map(
                        |(page, connection)| {
                            let connection = connection.map(|id| Connection {
                                shared,
                                id,
                                statements: AtomicU64::new(2),
                                closed: AtomicBool::new(false),
                            });
                            (page, connection)
                        },
                    );
                    if let Err(Ok((_, Some(unread_connection)))) = reply.send(result) {
                        drop(unread_connection);
                    }
                });
            }
            Command::Connect {
                options,
                shared,
                #[cfg(feature = "qualification")]
                queued,
                reply,
            } => {
                let connections = connections.clone();
                crate::reactor::spawn_in(&mut tasks, "connect", async move {
                    #[cfg(feature = "qualification")]
                    let timing = crate::reactor::CallTimer::start(queued);
                    let result = connections
                        .connect(node, options)
                        .await
                        .map(|id| Connection {
                            shared,
                            id,
                            statements: AtomicU64::new(1),
                            closed: AtomicBool::new(false),
                        });
                    #[cfg(feature = "qualification")]
                    if let Ok(connection) = &result {
                        timing.finish(connection.id);
                    }
                    if let Err(Ok(unread_connection)) = reply.send(result) {
                        drop(unread_connection);
                    }
                });
            }
            Command::Sql {
                connection,
                call,
                #[cfg(feature = "qualification")]
                queued,
                reply,
            } => match connections.get(connection) {
                Ok(state) => {
                    crate::reactor::spawn_in(&mut tasks, "sql", async move {
                        #[cfg(feature = "qualification")]
                        let timing = crate::reactor::CallTimer::start(queued);
                        let statement = match &call {
                            connection::Call::Execute { statement, .. }
                            | connection::Call::Next { statement } => Some(*statement),
                            _ => None,
                        };
                        let serving = connection::serve(state.clone(), call);
                        #[cfg(feature = "qualification")]
                        let serving =
                            rocky_secure::with_measurement_connection(connection, serving);
                        let result = serving.await;
                        #[cfg(feature = "qualification")]
                        timing.finish(connection);
                        if reply.send(result).is_err()
                            && let Some(statement) = statement
                        {
                            let _ =
                                connection::serve(state, connection::Call::Finish { statement })
                                    .await;
                        }
                    });
                }
                Err(error) => {
                    #[cfg(feature = "qualification")]
                    crate::reactor::CallTimer::start(queued).finish(connection);
                    drop(reply.send(Err(connection::sql_failure(error))));
                }
            },
            Command::Disconnect {
                connection,
                #[cfg(feature = "qualification")]
                queued,
                reply,
            } => {
                #[cfg(feature = "qualification")]
                let timing = crate::reactor::CallTimer::start(queued);
                connections.close(Some(connection));
                #[cfg(feature = "qualification")]
                timing.finish(connection);
                let _ = reply.send(());
            }
            #[cfg(feature = "qualification")]
            Command::LockMeasurements(reply) => {
                let _ = reply.send(node.replica.lock_measurements());
            }
            #[cfg(feature = "qualification")]
            Command::ReactorMeasurements(reply) => {
                let _ = reply.send(crate::reactor::current());
            }
            Command::Observe(tables, reply) => {
                crate::reactor::spawn_in(&mut tasks, "observe", async move {
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
            Command::Flush(reply) => {
                crate::reactor::spawn_in(&mut tasks, "flush", async move {
                    let flushed = node.replica.flush().await.map(drop).map_err(Failure::from);
                    let _ = reply.send(flushed);
                });
            }
            Command::Outcome(token, reply) => {
                crate::reactor::spawn_in(&mut tasks, "outcome", async move {
                    let outcome = node
                        .replica
                        .transaction_outcome(token.0)
                        .await
                        .map_err(Failure::from);
                    let _ = reply.send(outcome);
                });
            }
        }
        while tasks.try_join_next().is_some() {}
    }
    pause::at(&directory, pause::Point::Teardown).await;
    for background in std::iter::once(recovering).chain(bootstrapping) {
        background.abort();
        let _ = background.await;
    }
    end_users_of_file(&node, &connections, &mut tasks).await;
    let purge = node.lifecycle.lock().await.purge_on_close();
    let released = Arc::into_inner(node).is_some();
    settling.abort();
    let _ = settling.await;
    let mut result = close_file(replica).await;
    if !released {
        result = Err(
            "a task still held the database at close, so its directory was not purged; open and close it again"
                .into(),
        );
    }
    if purge && result.is_ok() {
        result = purge_directory(&directory).map_err(|e| e.to_string());
    }
    if let Some(reply) = shutdown {
        let _ = reply.send(result);
    }
    Ok(())
}
async fn end_users_of_file(
    node: &Node,
    connections: &connection::Registry,
    tasks: &mut rocky_platform::task::JoinSet<()>,
) {
    node.end_sessions(None);
    connections.close(None);
    node.detach(None).await;
    while tasks.join_next().await.is_some() {}
    node.detach(None).await;
    node.background.finish().await;
}
async fn close_file(replica: EncryptedReplica<rocky_core::TypedRows>) -> Result<(), String> {
    let flushed = replica.flush().await.map(drop).map_err(|e| e.to_string());
    replica.optimize().await;
    replica.close().await;
    flushed
}
const SETTLE_QUIET: web_time::Duration = web_time::Duration::from_millis(5);
const SETTLE_MAX_WAIT: web_time::Duration = web_time::Duration::from_millis(25);

async fn settle_commits(replica: EncryptedReplica<rocky_core::TypedRows>) {
    let mut revisions = replica.observe_revision();
    revisions.mark_changed();
    let mut retry = crate::scheduling::Retry::default();
    'settle: loop {
        let deadline = retry.deadline();
        let triggered_by_change = tokio::select! {
            changed = revisions.changed() => {
                if changed.is_err() {
                    break 'settle;
                }
                true
            }
            () = async {
                match deadline {
                    Some(deadline) => rocky_platform::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => false,
        };
        if triggered_by_change {
            let max_deadline = rocky_platform::time::Instant::now() + SETTLE_MAX_WAIT;
            loop {
                let quiet_deadline =
                    (rocky_platform::time::Instant::now() + SETTLE_QUIET).min(max_deadline);
                tokio::select! {
                    changed = revisions.changed() => {
                        if changed.is_err() {
                            break 'settle;
                        }
                        if rocky_platform::time::Instant::now() >= max_deadline {
                            break;
                        }
                    }
                    () = rocky_platform::time::sleep_until(quiet_deadline) => break,
                }
            }
        }
        match replica.flush().await {
            Ok(_) if replica.maintain().await => retry.reset(),
            Ok(_) => retry.soon(),
            Err(error) => {
                tracing::warn!(%error, "background flush failed; maintenance remains queued");
                retry.failed();
            }
        }
    }
}
fn purge_directory(directory: &Path) -> Result<(), Failure> {
    remove_installation(directory)?;
    match rocky_platform::fs::remove_dir_all(directory.join("secrets")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}
const INSTALLATION_IDENTITY_FILE: &str = "rocky.sqlite";
const INSTALLATION_DATA_FILES: [&str; 3] = ["rocky.sqlite-wal", "rocky.sqlite-shm", "mailbox.json"];
fn remove_if_present(path: &Path) -> Result<(), Failure> {
    match rocky_platform::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}
fn remove_installation(directory: &Path) -> Result<(), Failure> {
    remove_if_present(&directory.join(INSTALLATION_IDENTITY_FILE))?;
    for name in INSTALLATION_DATA_FILES {
        remove_if_present(&directory.join(name))?;
    }
    Ok(())
}
fn unwind(
    directory: &Path,
    created: Option<(&crate::secrets::Vault, Vec<String>)>,
    error: Failure,
) -> Failure {
    if let Some((vault, roles)) = created {
        let _ = vault.forget(roles);
    }
    match remove_installation(directory) {
        Ok(()) => error,
        Err(removal) => {
            format!("{error}; the failed installation could not be removed: {removal}").into()
        }
    }
}
async fn discard(
    node: Arc<Node>,
    replica: EncryptedReplica<rocky_core::TypedRows>,
    directory: &Path,
    install: &Install,
    error: Failure,
) -> Failure {
    node.detach(None).await;
    let vault = node.vault.clone();
    let roles = node.settings.lock().await.roles();
    drop(node);
    drop(replica);
    let created = install.discards_secrets().then_some((&vault, roles));
    unwind(directory, created, error)
}
const CREDENTIAL_TTL_SECONDS: std::ops::RangeInclusive<u64> = 60..=366 * 24 * 60 * 60;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn a_removal_that_fails_on_a_data_file_already_removed_the_identity() -> Result<(), Failure> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join(INSTALLATION_IDENTITY_FILE), b"{}")?;
        let unremovable = directory.path().join(INSTALLATION_DATA_FILES[0]);
        std::fs::create_dir(&unremovable)?;
        std::fs::write(unremovable.join("held"), b"")?;
        assert!(
            remove_installation(directory.path()).is_err(),
            "a data file that cannot be removed fails the removal"
        );
        assert!(
            !directory.path().join(INSTALLATION_IDENTITY_FILE).exists(),
            "the identity goes before any data file, so a crash leaves no domain"
        );
        Ok(())
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_create_without_a_credential_ttl_issues_thirty_days_and_refuses_one_outside_a_minute_to_a_year()
    -> Result<(), Failure> {
        let ttl_of = |credential_ttl| match Database::create_install(
            credential_ttl,
            Arc::new(crate::RootKey::generate()),
        )? {
            Install::Create { ttl, .. } => Ok::<_, Failure>(ttl),
            _ => Err("a create installs as a create".into()),
        };
        assert_eq!(ttl_of(None)?, 30 * 24 * 60 * 60);
        assert_eq!(ttl_of(None)?, DEFAULT_TTL);
        assert_eq!(ttl_of(Some(60))?, 60);
        assert_eq!(ttl_of(Some(366 * 24 * 60 * 60))?, 366 * 24 * 60 * 60);
        for refused in [0, 59, 366 * 24 * 60 * 60 + 1] {
            assert!(
                ttl_of(Some(refused)).is_err(),
                "a credential ttl of {refused} s is refused"
            );
        }
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn close_yields_while_the_database_thread_ends() -> Result<(), Failure> {
        let directory = rocky_platform::fs::Scratch::new().await;
        let schema = crate::Schema::builder("closing", 1)
            .table(
                crate::Table::replayed("items")
                    .column(crate::Column::new("title", crate::Affinity::Text)),
            )
            .build()?;
        let (reached, reaching) = oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        HOLD_END.with(|hold| *hold.borrow_mut() = Some((reached, released)));
        let database = Database::create(
            Open {
                directory: directory.path(),
                schema: &schema,
                secrets: None,
                identity: None,
                network: None,
            },
            None,
        )?
        .database;
        let releasing = async {
            reaching
                .await
                .map_err(|_| -> Failure { "the database thread never ended".into() })?;
            release
                .send(())
                .map_err(|_| -> Failure { "the close blocked its executor".into() })
        };
        let (closed, released) = tokio::join!(database.close(), releasing);
        closed?;
        released?;
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    fn held_schema() -> &'static Schema {
        static PARSED: std::sync::LazyLock<Schema> = std::sync::LazyLock::new(|| {
            Schema::parse(r#"{"application":"held","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"]}]}"#).unwrap()
        });
        &PARSED
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn close_succeeds_only_once_a_running_bootstrap_let_go_of_the_file() -> Result<(), Failure>
    {
        let directory = rocky_platform::fs::Scratch::new().await;
        let hub = Arc::new(crate::transport::memory::Hub::default());
        let open = || Open {
            directory: directory.path(),
            schema: held_schema(),
            secrets: None,
            identity: None,
            network: Some(Network::Owned(hub.connector("held"))),
        };
        tokio::task::LocalSet::new()
            .run_until(async {
                let create = Database::create_install(None, Arc::new(crate::RootKey::generate()))?;
                Database::launch_local(open(), create)
                    .await?
                    .database
                    .close()
                    .await?;
                BootstrapIntentFile::in_directory(directory.path())
                    .request_checkpoint_import()
                    .await?;
                let (bootstrapping, _never_released) =
                    pause::arm(directory.path(), pause::Point::Bootstrap);
                let database = Database::launch_local(open(), Install::Installed)
                    .await?
                    .database;
                bootstrapping
                    .await
                    .map_err(|_| -> Failure { "the bootstrap never started".into() })?;
                database.close().await?;
                let reopened = Database::launch_local(open(), Install::Installed)
                    .await
                    .map_err(|error| -> Failure {
                        format!("closed, yet the file is still held: {error}").into()
                    })?;
                reopened.database.close().await
            })
            .await
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reopen_after_a_dropped_database_waits_for_its_teardown() -> Result<(), Failure> {
        let directory = rocky_platform::fs::Scratch::new().await;
        let path = directory.path().to_path_buf();
        fn open(directory: &Path) -> Open<'_> {
            Open {
                directory,
                schema: held_schema(),
                secrets: None,
                identity: None,
                network: None,
            }
        }
        let created = Database::create(open(&path), None)?;
        let (tearing_down, finish_teardown) = pause::arm(&path, pause::Point::Teardown);
        let (succeeding, let_successor_wait) = pause::arm(&path, pause::Point::Succeeding);
        drop(created);
        tearing_down
            .await
            .map_err(|_| -> Failure { "the dropped database never tore down".into() })?;
        let reopening = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || {
                Database::open(open(&path)).map(|opened| opened.database)
            })
        };
        tokio::pin!(reopening);
        let reopened = tokio::select! {
            reopened = &mut reopening => reopened?,
            waiting = succeeding => {
                waiting.map_err(|_| -> Failure { "the reopen stopped waiting".into() })?;
                let _ = let_successor_wait.send(());
                let _ = finish_teardown.send(());
                reopening.await?
            }
        };
        let database = reopened.map_err(|error| -> Failure {
            format!("a reopen racing the dropped database's teardown failed: {error}").into()
        })?;
        database.close().await
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn reopening_a_crashed_directory_recovers_its_wal_once() -> Result<(), Failure> {
        rocky_storage::wal_recoveries::watch();
        let root = rocky_platform::fs::Scratch::new().await;
        let (live, crashed) = (root.path().join("live"), root.path().join("crashed"));
        let open = |directory| Open {
            directory,
            schema: held_schema(),
            secrets: None,
            identity: None,
            network: None,
        };
        let created = Database::create(open(&live), None)?.database;
        fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
            std::fs::create_dir_all(to)?;
            for entry in std::fs::read_dir(from)? {
                let entry = entry?;
                let target = to.join(entry.file_name());
                if entry.file_type()?.is_dir() {
                    copy(&entry.path(), &target)?;
                } else if entry.file_name() != "rocky.sqlite-shm" {
                    std::fs::copy(entry.path(), target)?;
                }
            }
            Ok(())
        }
        copy(&live, &crashed)?;
        created.close().await?;
        let replica = crashed.join("rocky.sqlite");
        assert!(
            rocky_platform::fs::len(crashed.join("rocky.sqlite-wal"))? > 0,
            "the crash left frames to recover"
        );
        let reopened = Database::open(open(&crashed))?.database;
        assert_eq!(
            rocky_storage::wal_recoveries::of(&replica),
            1,
            "settings, format checks and the replica share one recovery"
        );
        reopened.close().await
    }

    #[cfg(feature = "qualification")]
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn blocking_reactor_task_and_queued_sql_call_are_attributed() -> Result<(), Failure> {
        let directory = rocky_platform::fs::Scratch::new().await;
        let schema = crate::Schema::builder("reactor_timing", 1)
            .table(
                crate::Table::replayed("items")
                    .column(crate::Column::new("title", crate::Affinity::Text)),
            )
            .build()?;
        let schema = serde_json::to_string(&schema)?;
        let database = Database::create(
            Open {
                directory: directory.path(),
                schema: &schema,
                secrets: None,
                identity: None,
                network: None,
            },
            None,
        )?
        .database;
        let connection = database
            .connect(ConnectionOptions {
                read_only: true,
                durability: connection::Durability::Buffered,
                transaction_timeout: None,
            })
            .await?;
        let measurements = database.reactor_measurements().await?;
        measurements.start();
        let (entered, waiting) = oneshot::channel();
        let (resume, resumed) = std::sync::mpsc::channel();
        database
            .shared
            .input
            .send(Command::BlockReactor(
                entered,
                std::sync::Mutex::new(resumed),
            ))
            .await?;
        waiting.await?;
        let queued = web_time::Instant::now();
        let (reply, result) = oneshot::channel();
        database
            .shared
            .input
            .send(Command::Sql {
                connection: connection.id,
                call: connection::Call::InTransaction,
                queued,
                reply,
            })
            .await?;
        let deliberate_reactor_block = std::time::Duration::from_millis(2);
        while queued.elapsed() < deliberate_reactor_block {
            std::hint::spin_loop();
        }
        resume.send(())?;
        assert!(matches!(result.await??, connection::Reply::Flag(false)));
        let snapshot = measurements.stop();
        assert!(snapshot.polls["test_blocking"].iter().any(|us| *us >= 2000));
        assert!(
            snapshot.calls[&connection.id]
                .iter()
                .any(|call| call.queue_us >= 2000)
        );
        connection.close().await;
        database.close().await?;
        Ok(())
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn unread_connection_reply_cleans_up_with_a_full_request_queue() -> Result<(), Failure> {
        let (input, _requests) = mpsc::channel(1);
        input.try_send(Command::Disconnect {
            connection: 99,
            #[cfg(feature = "qualification")]
            queued: web_time::Instant::now(),
            reply: oneshot::channel().0,
        })?;
        let (cleanup, mut cleanups) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            input,
            cleanup,
            budget: Semaphore::new(1),
            closed: AtomicBool::new(false),
            sessions: AtomicU64::new(1),
            _handles: residence::claim(Path::new("unread-connection-reply"))?.handles,
        });
        let connection = Connection {
            shared,
            id: 7,
            statements: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        };
        let (reply, unread) = oneshot::channel::<Result<Connection, Failure>>();
        assert!(reply.send(Ok(connection)).is_ok());
        drop(unread);
        assert!(matches!(
            cleanups.try_recv()?,
            Command::Disconnect { connection: 7, .. }
        ));
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn purge_leaves_no_installation_state_for_a_later_join() -> Result<(), Failure> {
        let directory = tempfile::tempdir()?;
        for name in [
            "rocky.sqlite",
            "rocky.sqlite-wal",
            "rocky.sqlite-shm",
            "mailbox.json",
        ] {
            std::fs::write(directory.path().join(name), b"{}")?;
        }
        std::fs::create_dir(directory.path().join("secrets"))?;
        purge_directory(directory.path())?;
        assert_eq!(
            std::fs::read_dir(directory.path())?.count(),
            0,
            "a stale revocation notice left here fails every status of the next installation"
        );
        Ok(())
    }

    fn installed_files(directory: &Path) -> Vec<&'static str> {
        ["identity.json", "rocky.sqlite"]
            .into_iter()
            .filter(|name| rocky_platform::fs::exists(directory.join(name)))
            .collect()
    }
    fn stop_once_started(directory: &Path) -> Result<(), Failure> {
        fault::STOPS
            .lock()
            .map_err(|_| "poisoned")?
            .push((directory.to_path_buf(), fault::Stage::Started));
        Ok(())
    }
    fn notes() -> Result<crate::Schema, Failure> {
        crate::Schema::builder("unwound", 1)
            .table(
                crate::Table::replayed("notes")
                    .column(crate::Column::new("body", crate::Affinity::Text)),
            )
            .build()
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_create_failing_before_it_is_handed_out_leaves_the_directory_for_a_retry()
    -> Result<(), Failure> {
        let directory = rocky_platform::fs::Scratch::new().await;
        let config = crate::Configuration::new(directory.path(), &notes()?)?;
        stop_once_started(directory.path())?;
        let stopped = config.create().await.err().ok_or("created past the stop")?;
        assert!(stopped.to_string().contains("injected stop"), "{stopped}");
        assert_eq!(installed_files(directory.path()), Vec::<&str>::new());
        let created = config.create().await?;
        assert!(created.root.is_some());
        created.database.close().await
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_supplied_install_failing_before_it_is_handed_out_installs_on_a_retry()
    -> Result<(), Failure> {
        let root = rocky_platform::fs::Scratch::new().await;
        let schema = notes()?;
        let issuer = crate::Configuration::new(root.path().join("issuer"), &schema)?
            .create()
            .await?
            .database;
        let directory = root.path().join("device");
        let identity: Arc<dyn crate::IdentityProvider> = Arc::new(crate::IdentityKey::generate());
        let device = Database::node_keys_of(&directory, None, Some(identity.clone()))?;
        let session = issuer.session();
        let supplied = session
            .grant(
                device,
                crate::GrantProfile::Reader {
                    scopes: ["notes".into()].into(),
                },
                crate::GrantTerms::default(),
            )
            .await?;
        session.close().await;
        let install = || {
            Database::install(
                Open {
                    directory: &directory,
                    schema: &schema,
                    secrets: None,
                    identity: Some(identity.clone()),
                    network: None,
                },
                supplied.clone(),
            )
        };
        stop_once_started(&directory)?;
        let stopped = install().err().ok_or("installed past the stop")?;
        assert!(stopped.to_string().contains("injected stop"), "{stopped}");
        assert_eq!(installed_files(&directory), Vec::<&str>::new());
        install()?.database.close().await?;
        issuer.close().await
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn old_database_is_refused_before_enrollment_or_database_writes() -> Result<(), Failure> {
        let schema = Schema::parse(
            r#"{"format":6,"application":"refusal","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"]}]}"#,
        )?;
        for format in [0, 1, 2, 3, 5] {
            let directory = rocky_platform::fs::Scratch::new().await;
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
            let before = rocky_platform::fs::read(&path)?;
            for create in [false, true] {
                let options = Open {
                    identity: None,
                    directory: directory.path(),
                    schema: &schema,
                    secrets: None,
                    network: None,
                };
                let error = if create {
                    Database::create(options, None)
                } else {
                    Database::open(options)
                }
                .err()
                .ok_or("old database admitted")?
                .to_string();
                assert!(error.contains("unsupported Rocky file format"), "{error}");
                assert_eq!(rocky_platform::fs::read(&path)?, before);
                for name in [
                    "sdk.lock",
                    "identity.json",
                    "bootstrap.json",
                    "secrets",
                    "rocky.sqlite-wal",
                ] {
                    assert!(
                        !rocky_platform::fs::exists(directory.path().join(name)),
                        "{name}"
                    );
                }
            }
        }
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn populated_consumer_continues_its_writer_sequence_after_reopen() -> Result<(), Failure>
    {
        let schema = Schema::parse(
            r#"{"format":6,"application":"reopen_numeric","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"]}]}"#,
        )?;
        let directory = rocky_platform::fs::Scratch::new().await;
        let options = || Open {
            identity: None,
            directory: directory.path(),
            schema: &schema,
            secrets: None,
            network: None,
        };
        let database = Database::create(options(), None)?.database;
        let mut tx = database.transaction().await?;
        let first_id = tx.next_id("items", Vec::new()).await?;
        tx.execute(
            "INSERT INTO items(id,title) VALUES(?,'first')",
            vec![first_id.sql()],
        )
        .await?;
        tx.commit().await?;
        database.close().await?;

        let database = Database::open(options())?.database;
        let rows = database
            .query("SELECT id FROM items", Vec::new())
            .await?
            .collect()
            .await?;
        let stored: rocky_sdk::RowIdentity = rows[0].get_at(0)?;
        assert_eq!(stored, first_id);
        let mut tx = database.transaction().await?;
        let second_id = tx.next_id("items", Vec::new()).await?;
        assert_eq!(second_id.writer(), first_id.writer());
        assert_eq!(second_id.sequence(), first_id.sequence() + 1);
        tx.execute(
            "INSERT INTO items(id,title) VALUES(?,'second')",
            vec![second_id.sql()],
        )
        .await?;
        tx.commit().await?;
        let rows = database
            .query("SELECT id FROM items ORDER BY id", Vec::new())
            .await?
            .collect()
            .await?;
        assert_eq!(rows.len(), 2);
        let stored_second: rocky_sdk::RowIdentity = rows[1].get_at(0)?;
        assert_eq!(stored_second, second_id);
        database.close().await?;
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn saturated_waits_cannot_block_session_shutdown() -> Result<(), Failure> {
        let directory = rocky_platform::fs::Scratch::new().await;
        let schema = Schema::parse(
            r#"{"application":"saturation","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#,
        )?;
        let opened = Database::create(
            Open {
                identity: None,
                directory: directory.path(),
                schema: &schema,
                secrets: None,
                network: None,
            },
            None,
        )?;
        let database = opened.database;
        let session = database.session();
        let status = session.status().await?;
        let mut replies = Vec::new();
        let saturating_waits_queued_before_end = ACTIVE_CALL_QUOTA + 1;
        for _ in 0..saturating_waits_queued_before_end {
            let (reply, receiver) = oneshot::channel();
            database
                .shared
                .input
                .send(Command::Request {
                    session: session.id,
                    ending: session.closed.clone(),
                    request: Box::new(crate::request::Request::StatusAfter(
                        status.token.clone(),
                        reply,
                    )),
                })
                .await?;
            replies.push(receiver);
        }
        rocky_platform::time::timeout(std::time::Duration::from_secs(5), session.close()).await?;
        drop(replies);
        database.close().await?;
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn pooled_queries_preserve_sql_errors_and_reset_query_only() -> Result<(), Failure> {
        use crate::{Durability, SqlFailure, SqlValue};
        let directory = rocky_platform::fs::Scratch::new().await;
        let schema = Schema::parse(
            r#"{"application":"poolcheck","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#,
        )?;
        let opened = Database::create(
            Open {
                identity: None,
                directory: directory.path(),
                schema: &schema,
                secrets: None,
                network: None,
            },
            None,
        )?;
        let database = opened.database;
        let mut tx = database.transaction().await?;
        let first_id = tx.next_id("items", Vec::new()).await?;
        tx.execute(
            "INSERT INTO items(id, key, title) VALUES (?, ?, ?)",
            vec![
                first_id.sql(),
                SqlValue::Text("a".into()),
                SqlValue::Text("first".into()),
            ],
        )
        .await?;
        tx.commit().await?;

        let rows = database
            .query(
                "SELECT title FROM items WHERE key = ?",
                vec![SqlValue::Text("a".into())],
            )
            .await?
            .collect()
            .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_at::<String>(0)?, "first");

        let err = database
            .query("SELECT * FROM no_such_table", Vec::new())
            .await
            .err()
            .expect("invalid table should fail");
        assert!(err.downcast_ref::<SqlFailure>().is_some());

        let rows = database
            .query(
                "SELECT title FROM items WHERE key = ?",
                vec![SqlValue::Text("a".into())],
            )
            .await?
            .collect()
            .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_at::<String>(0)?, "first");

        let conn = database
            .connect(ConnectionOptions {
                read_only: false,
                durability: Durability::Buffered,
                transaction_timeout: None,
            })
            .await?;
        conn.execute(
            conn.statement(),
            "PRAGMA query_only = ON".into(),
            Vec::new(),
        )
        .await?;
        conn.close().await;

        let mut tx = database.transaction().await?;
        let second_id = tx.next_id("items", Vec::new()).await?;
        tx.execute(
            "INSERT INTO items(id, key, title) VALUES (?, ?, ?)",
            vec![
                second_id.sql(),
                SqlValue::Text("b".into()),
                SqlValue::Text("second".into()),
            ],
        )
        .await?;
        tx.commit().await?;

        let fresh = database
            .connect(ConnectionOptions {
                read_only: false,
                durability: Durability::Buffered,
                transaction_timeout: None,
            })
            .await?;
        let counters = fresh
            .execute(
                fresh.statement(),
                "SELECT changes(), total_changes(), last_insert_rowid()".into(),
                Vec::new(),
            )
            .await?;
        assert_eq!(
            counters.rows,
            vec![vec![
                SqlValue::Integer(0),
                SqlValue::Integer(0),
                SqlValue::Integer(0),
            ]]
        );
        fresh.close().await;

        database.close().await?;
        Ok(())
    }
}
