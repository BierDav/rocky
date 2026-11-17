//! The database owner and the handles composed on it: sessions and an
//! optional network service.
#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
use crate::test_snapshot as snap;
use crate::{
    Failure,
    connection::{self, ConnectionOptions, Page, SqlFailure, TableEvents},
    enrollment::{DEFAULT_TTL, NodeKeys, Settings, Supplied, now},
    node::{self, Node},
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

/// What opening a database takes. Without `secrets`, keys are owner-only
/// files in the data directory. Identity providers are used only during
/// installation. Without `network`, nothing is
/// dialed or listened on.
pub struct Open<'a> {
    pub directory: &'a Path,
    pub schema: &'a str,
    pub secrets: Option<Arc<dyn SecretStore>>,
    /// The identity signer for this installation call; opening needs no signer.
    pub identity: Option<Arc<dyn crate::IdentityProvider>>,
    pub network: Option<Network>,
    /// Application-owned coordinator; local storage works without one.
    pub coordination: Option<Arc<dyn rocky_credentials::Coordinator>>,
}
/// An open database and the network service it opened with, if any.
pub struct Opened {
    pub database: Arc<Database>,
    pub network: Option<NetworkService>,
    /// The root key a create without one made. Show its phrase to the user
    /// once, or keep its secret behind a passkey: Rocky keeps no copy, and
    /// only it adds or revokes trusted nodes and recovers the domain.
    pub root: Option<crate::RootKey>,
    /// A generated linkable identity's phrase, returned once.
    pub identity: Option<String>,
    /// The lost trusted identities a recovery revoked with the root, with
    /// the identities each first enrolled, whose devices enroll again; every
    /// key rotated. All of them, also those an interrupted attempt revoked.
    /// Empty for any other open. A trusted identity the backup never knew
    /// is not named here: the recovery statement ends it, and devices that
    /// know it report `AlertKind::RecoveryEnded`.
    pub revoked: Vec<rocky_credentials::PublicKey>,
}

/// How a database is installed or opened. The blocking constructors make
/// every variant; a browser makes create, join, restore, recover and open
/// through `Configuration`.
#[cfg_attr(
    target_family = "wasm",
    expect(dead_code, reason = "blocking installation variants are native only")
)]
pub(crate) enum Install {
    Create {
        ttl: u64,
        /// Taken once, when the domain is written.
        root: std::sync::Mutex<Option<Arc<dyn crate::IdentityProvider>>>,
        /// The bookmark the creating device makes and follows before the
        /// database is handed out.
        bookmark: Option<&'static str>,
        /// A personal database of the root's owner identity, whose
        /// interrupted create resumes from its install intent.
        personal: Option<Box<crate::personal::Create>>,
    },
    Join {
        invitation: String,
        /// What the device calls itself in its join request.
        name: String,
    },
    Supplied(Box<Supplied>),
    /// Another node of the identity `open.identity` supplies, from a node
    /// of that identity.
    Identity {
        contact: String,
        confirmed: Option<Arc<crate::AwaitingLink>>,
    },
    Installed,
    /// One more owner device of a personal database, signed in with the
    /// passkey; its interrupted install resumes from its intent.
    SignIn(Box<crate::personal::SignIn>),
    /// A backup archive, under a fresh identity the invitation enrolls.
    Restore {
        archive: PathBuf,
        invitation: String,
    },
    /// A trusted node's backup archive, under a fresh trusted identity the
    /// root grants, with the keys the root opens from its escrow.
    Recover {
        archive: PathBuf,
        /// Copied when the installation is written and taken once the
        /// lost trusted identities are revoked.
        root: std::sync::Mutex<Option<Box<crate::RootKey>>>,
        /// The archived identity record, once the archive is opened.
        archived: std::sync::Mutex<Option<Box<Settings>>>,
    },
}
impl Install {
    /// A restore interrupted after it enrolled continues, as does a
    /// recovery interrupted after it saved its identity.
    fn resumes_interrupted(&self, directory: &Path) -> Result<bool, Failure> {
        Ok(match self {
            Self::Restore { .. } => {
                rocky_platform::fs::exists(directory.join(crate::backup::MARKER))
            }
            Self::Recover { .. } => crate::recovery::unfinished(directory)?.is_some(),
            _ => false,
        })
    }
    /// A join or another node of an identity bootstraps from a checkpoint.
    fn bootstraps_from_checkpoint(&self) -> bool {
        matches!(self, Self::Join { .. } | Self::Identity { .. })
    }
    /// A create, sign-in or supplied install takes back what it wrote when
    /// it fails before it is handed out; see `unwind`.
    fn unwinds_on_failure(&self) -> bool {
        matches!(
            self,
            Self::Create { .. } | Self::SignIn(_) | Self::Supplied(_)
        )
    }
    /// A personal install's intent ends once it is handed out; until then a
    /// retry resumes it. An open ends one an interrupted install left beside
    /// its saved identity, so no later sign-in takes that store back.
    fn ends_personal_intent(&self) -> bool {
        matches!(
            self,
            Self::Create {
                personal: Some(_),
                ..
            } | Self::SignIn(_)
                | Self::Installed
        )
    }
    /// A shared create that fails takes its secrets back. A personal
    /// create keeps its device keys and pinned epochs for the retry its
    /// install intent resumes.
    fn discards_secrets(&self) -> bool {
        matches!(self, Self::Create { personal: None, .. })
    }
}
struct Setup {
    directory: PathBuf,
    schema: Schema,
    secrets: Arc<dyn SecretStore>,
    identity: Option<Arc<dyn crate::IdentityProvider>>,
    install: Install,
    coordination: Option<Arc<dyn rocky_credentials::Coordinator>>,
    confirmation: Option<String>,
    /// Where a hosted database hands out its node.
    #[cfg(test)]
    expose: Option<std::sync::mpsc::Sender<std::sync::Weak<Node>>>,
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
    /// A backup archive, as bytes.
    Export(oneshot::Sender<Result<Vec<u8>, Failure>>),
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
    Coordinate(
        Option<Arc<dyn rocky_credentials::Coordinator>>,
        oneshot::Sender<Result<(), Failure>>,
    ),
    #[cfg(feature = "qualification")]
    LockMeasurements(oneshot::Sender<rocky_secure::LockMeasurements>),
    #[cfg(feature = "qualification")]
    ReactorMeasurements(oneshot::Sender<Option<crate::ReactorMeasurements>>),
    Observe(
        Option<Vec<String>>,
        oneshot::Sender<Result<rocky_runtime::TableObserver, Failure>>,
    ),
    Outcome(
        rocky_core::TransactionToken,
        oneshot::Sender<Result<Option<rocky_core::replay::TransactionOutcome>, Failure>>,
    ),
    Flush(oneshot::Sender<Result<(), Failure>>),

    Claim(
        crate::counters::Name,
        crate::counters::Claim,
        oneshot::Sender<Result<u64, Failure>>,
    ),

    Counter(
        crate::counters::Name,
        Option<crate::counters::Claim>,
        oneshot::Sender<Result<crate::counters::Reading, Failure>>,
    ),
}
type Greeting = Result<Ready, Failure>;
/// The scope and the network service a database opened with, and the
/// identities its recovery revoked.
type Ready = (
    rocky_core::Scope,
    Option<u64>,
    Vec<rocky_credentials::PublicKey>,
);
/// Where `run` reports how opening went: to the opener blocked on the
/// database's thread, or to an opener that awaits it.
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

/// Calls a database serves at once; one more is refused.
const ACTIVE_CALL_QUOTA: usize = 128;

struct Shared {
    input: mpsc::Sender<Command>,
    cleanup: mpsc::UnboundedSender<Command>,
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
    scope: rocky_core::Scope,
    worker: Mutex<Option<Worker>>,
}
/// What serves a database: its own thread natively, or a task on the
/// caller's thread in a browser Worker and in a simulation.
enum Worker {
    #[cfg(not(target_family = "wasm"))]
    Thread {
        thread: std::thread::JoinHandle<()>,
        /// Fires once the thread's runtime, and the replica with it, dropped.
        ended: oneshot::Receiver<()>,
    },
    #[cfg(any(test, target_family = "wasm"))]
    Task(rocky_platform::task::JoinHandle<()>),
}
/// Before a database thread signals its end, it reports reaching it and
/// waits for the release, so a test sees whether a closer still yields.
#[cfg(all(test, not(target_family = "wasm")))]
type HeldEnd = (oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);
/// Bounds a test's hang only: a closer blocking its executor never releases.
#[cfg(all(test, not(target_family = "wasm")))]
const HELD_END_HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(all(test, not(target_family = "wasm")))]
thread_local! {
    /// Taken by the next database thread this thread starts.
    static HOLD_END: std::cell::RefCell<Option<HeldEnd>> = const { std::cell::RefCell::new(None) };
}
impl Worker {
    #[cfg(not(target_family = "wasm"))]
    fn thread(serve: impl FnOnce() + Send + 'static) -> std::io::Result<Self> {
        let (end, ended) = oneshot::channel();
        #[cfg(test)]
        let held = HOLD_END.with(|hold| hold.borrow_mut().take());
        let thread = std::thread::Builder::new()
            .name("rocky-sdk".into())
            .stack_size(rocky_platform::task::THREAD_STACK)
            .spawn(move || {
                serve();
                #[cfg(test)]
                if let Some((reached, release)) = held {
                    let _ = reached.send(());
                    let _ = release.recv_timeout(HELD_END_HANG_BOUND);
                }
                let _ = end.send(());
            })?;
        Ok(Self::Thread { thread, ended })
    }
    /// Waits for the worker to end without blocking the caller's executor.
    /// A thread that signalled its end has only its exit left, so it is
    /// detached rather than joined.
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
    /// Waits for the worker's end from a synchronous opener. A task needs
    /// no wait: it ends with the error it sent.
    #[cfg(not(target_family = "wasm"))]
    fn release(self) {
        match self {
            Self::Thread { thread, .. } => drop(thread.join()),
            #[cfg(test)]
            Self::Task(_) => {}
        }
    }
}
/// Blocking installs and opens, native only: each blocks the caller until
/// the database is open, which a browser cannot; `Configuration` opens
/// without blocking.
#[cfg(not(target_family = "wasm"))]
impl Database {
    /// Creates a domain in an empty directory under a fresh root key, which
    /// `Opened::root` hands back once; see `create_with_root`. Credentials
    /// last `credential_ttl` seconds, 30 days by default.
    pub fn create(open: Open<'_>, credential_ttl: Option<u64>) -> Result<Opened, Failure> {
        let root = crate::RootKey::generate();
        let created = crate::RootKey::from_secret(root.secret());
        let mut opened = Self::start(
            open,
            Self::create_install(credential_ttl, Arc::new(root), None, None)?,
        )?;
        opened.root = Some(created);
        Ok(opened)
    }
    /// Creates a domain in an empty directory whose root `root` signs it:
    /// the genesis and a trusted grant to this device's own identity, which
    /// it runs on from then on. The root is dropped when the domain is
    /// written and never stored; renewing and sealing never need it.
    pub fn create_with_root(
        open: Open<'_>,
        credential_ttl: Option<u64>,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<Opened, Failure> {
        Self::start(
            open,
            Self::create_install(credential_ttl, root, None, None)?,
        )
    }
    /// Enrolls this device with the issuer an invitation names, as the
    /// grant profile it names, over `open.network`, which stays attached.
    pub fn join(open: Open<'_>, invitation: &str) -> Result<Opened, Failure> {
        let install = Self::join_install(&open, invitation, "")?;
        Self::start(open, install)
    }
    /// Installs another node of the identity `open.identity` supplies: a
    /// node of that identity, which `contact` names, hands it what the
    /// identity holds over `open.network`, and its provider certifies this
    /// node. No issuer and no invitation is involved.
    pub fn enroll_identity(open: Open<'_>, contact: &str) -> Result<Opened, Failure> {
        if open.network.is_none() {
            return Err(ServiceError::NetworkRequired("enroll an identity's node").into());
        }
        if open.identity.is_none() {
            return Err("another node of an identity needs that identity's provider".into());
        }
        Self::start(
            open,
            Install::Identity {
                contact: contact.into(),
                confirmed: None,
            },
        )
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
    /// Opens after the application confirms the exact removal impact report.
    pub fn open_confirmed(open: Open<'_>, digest: &str) -> Result<Opened, Failure> {
        Self::start_confirmed(open, Install::Installed, Some(digest.into()))
    }
    /// Restores a backup archive into an empty directory under a fresh
    /// signing identity, which `invitation` enrolls over `open.network`.
    /// The archive's operations, handles, tickets, results and effects keep
    /// their original identities; nothing is sent again as new work. An
    /// interrupted restore continues when called again.
    /// `Configuration::restore` does the same without blocking.
    pub fn restore(open: Open<'_>, archive: &Path, invitation: &str) -> Result<Opened, Failure> {
        let install = Self::restore_install(&open, archive, invitation)?;
        Self::start(open, install)
    }
    /// The root recovers a domain from a backup archive of one of its
    /// trusted nodes with every trusted node and device lost: into an empty
    /// directory, under a fresh trusted identity the root grants, with every
    /// key the archived node held opened from its root escrow. A key the
    /// escrow lacks fails the recovery and is named. The root revokes the
    /// lost trusted identities (`Opened::revoked`), then is dropped. See
    /// `Configuration::recover`.
    pub fn recover(
        open: Open<'_>,
        archive: &Path,
        root: &crate::RootKey,
    ) -> Result<Opened, Failure> {
        Self::start(open, Self::recover_install(archive, root))
    }
    fn start(open: Open<'_>, install: Install) -> Result<Opened, Failure> {
        Self::start_confirmed(open, install, None)
    }
    fn start_confirmed(
        open: Open<'_>,
        install: Install,
        confirmation: Option<String>,
    ) -> Result<Opened, Failure> {
        let (setup, network) = Self::setup(open, install, confirmation)?;
        let (input, receiver) = mpsc::channel(32);
        let (cleanup, cleanups) = mpsc::unbounded_channel();
        let (hello, greeting) = std::sync::mpsc::sync_channel::<Greeting>(1);
        let hello = Hello::Thread(hello);
        let worker =
            Worker::thread(move || serve_on_thread(setup, network, receiver, cleanups, &hello))?;
        match greeting.recv().map_err(|_| "database failed to start")? {
            Ok(ready) => Ok(Self::opened(ready, input, cleanup, worker)),
            Err(error) => {
                worker.release();
                Err(error)
            }
        }
    }
}
impl Database {
    /// Replica-local measurement support for release qualification probes.
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
        bookmark: Option<&'static str>,
        personal: Option<Box<crate::personal::Create>>,
    ) -> Result<Install, Failure> {
        let ttl = credential_ttl.unwrap_or(DEFAULT_TTL);
        if !(60..=366 * 24 * 60 * 60).contains(&ttl) {
            return Err("credential lifetime must be between a minute and a year".into());
        }
        Ok(Install::Create {
            ttl,
            root: std::sync::Mutex::new(Some(root)),
            bookmark,
            personal,
        })
    }
    fn join_install(open: &Open<'_>, invitation: &str, name: &str) -> Result<Install, Failure> {
        if open.network.is_none() {
            return Err(ServiceError::NetworkRequired("join").into());
        }
        Ok(Install::Join {
            invitation: invitation.into(),
            name: name.into(),
        })
    }
    /// Prepares public node keys for external enrollment under an explicit identity.
    /// Keep the provider outside Rocky and supply it again when installing the answer.
    pub fn node_keys_of(
        directory: &Path,
        secrets: Option<Arc<dyn SecretStore>>,
        identity: Option<Arc<dyn crate::IdentityProvider>>,
    ) -> Result<NodeKeys, Failure> {
        rocky_platform::fs::create_dir_all(directory)?;
        let path = directory.join("identity.json");
        if rocky_platform::fs::exists(&path) {
            return Err("a domain exists in this directory: open it instead".into());
        }
        let secrets =
            secrets.unwrap_or_else(|| Arc::new(FileSecretStore::new(directory.join("secrets"))));
        Settings::prepare_install(&path, &secrets, identity)
    }
    pub(crate) async fn launch_restore(
        open: Open<'_>,
        archive: &Path,
        invitation: &str,
    ) -> Result<Opened, Failure> {
        let install = Self::restore_install(&open, archive, invitation)?;
        Self::launch(open, install, None).await
    }
    fn restore_install(
        open: &Open<'_>,
        archive: &Path,
        invitation: &str,
    ) -> Result<Install, Failure> {
        if open.network.is_none() {
            return Err(ServiceError::NetworkRequired("restore").into());
        }
        Ok(Install::Restore {
            archive: archive.to_path_buf(),
            invitation: invitation.into(),
        })
    }
    pub(crate) async fn launch_recover(
        open: Open<'_>,
        archive: &Path,
        root: &crate::RootKey,
    ) -> Result<Opened, Failure> {
        Self::launch(open, Self::recover_install(archive, root), None).await
    }
    fn recover_install(archive: &Path, root: &crate::RootKey) -> Install {
        Install::Recover {
            archive: archive.to_path_buf(),
            root: std::sync::Mutex::new(Some(Box::new(crate::RootKey::from_secret(root.secret())))),
            archived: std::sync::Mutex::new(None),
        }
    }
    /// Writes a signed, consistent archive and returns its inventory.
    pub async fn backup(&self, target: &Path) -> Result<crate::BackupInventory, Failure> {
        let session = self.session();
        let result = session
            .send(|reply| crate::request::Request::Backup(target.to_path_buf(), reply))
            .await;
        session.close().await;
        result
    }
    /// A signed backup archive of this database as bytes, for the app to
    /// save or a browser to download; `backup` writes the same archive to
    /// a file. `Configuration::recover_archive` takes the bytes back.
    pub async fn export_backup(&self) -> Result<Vec<u8>, Failure> {
        self.shared.request(Command::Export).await?
    }
    /// Opens without blocking the caller: natively the database gets its
    /// own thread, as `start` gives it; in a browser it is a task on the
    /// Worker's event loop.
    async fn launch(
        open: Open<'_>,
        install: Install,
        confirmation: Option<String>,
    ) -> Result<Opened, Failure> {
        let (setup, network) = Self::setup(open, install, confirmation)?;
        let (input, receiver) = mpsc::channel(32);
        let (cleanup, cleanups) = mpsc::unbounded_channel();
        let (hello, greeting) = oneshot::channel();
        let hello = Hello::Task(std::sync::Mutex::new(Some(hello)));
        #[cfg(not(target_family = "wasm"))]
        let worker =
            Worker::thread(move || serve_on_thread(setup, network, receiver, cleanups, &hello))?;
        #[cfg(target_family = "wasm")]
        let worker = {
            rocky_platform::fs::prepare(&setup.directory.join("rocky.sqlite")).await?;
            Worker::Task(crate::reactor::spawn_local("database", async move {
                if let Err(error) = run(setup, network, receiver, cleanups, &hello).await {
                    let _ = hello.send(Err(error));
                }
            }))
        };
        match greeting
            .await
            .unwrap_or_else(|_| Err("database failed to start".into()))
        {
            Ok(ready) => Ok(Self::opened(ready, input, cleanup, worker)),
            Err(error) => {
                let _ = worker.join().await;
                Err(error)
            }
        }
    }
    pub(crate) async fn launch_create(
        open: Open<'_>,
        credential_ttl: Option<u64>,
        root: Arc<dyn crate::IdentityProvider>,
        (bookmark, personal): (&'static str, Option<Box<crate::personal::Create>>),
    ) -> Result<Opened, Failure> {
        let install = Self::create_install(credential_ttl, root, Some(bookmark), personal)?;
        Self::launch(open, install, None).await
    }
    pub(crate) async fn launch_join(
        open: Open<'_>,
        invitation: &str,
        name: &str,
    ) -> Result<Opened, Failure> {
        let install = Self::join_install(&open, invitation, name)?;
        Self::launch(open, install, None).await
    }
    pub(crate) async fn launch_link(
        open: Open<'_>,
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
    /// Completes linking after both devices confirm the displayed code.
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
            open,
            Install::Identity {
                contact: contact.into(),
                confirmed: Some(pending),
            },
        )
    }
    pub(crate) async fn launch_sign_in(
        open: Open<'_>,
        sign_in: crate::personal::SignIn,
    ) -> Result<Opened, Failure> {
        Self::launch(open, Install::SignIn(Box::new(sign_in)), None).await
    }
    pub(crate) async fn launch_open(open: Open<'_>) -> Result<Opened, Failure> {
        Self::launch(open, Install::Installed, None).await
    }
    fn setup(
        open: Open<'_>,
        install: Install,
        confirmation: Option<String>,
    ) -> Result<(Setup, Option<Network>), Failure> {
        let directory = open.directory.to_path_buf();
        if let Install::Join { invitation, .. } = &install {
            crate::enrollment::Invite::parse(invitation)?;
        }
        let setup = Setup {
            schema: Schema::parse(open.schema)?,
            secrets: open
                .secrets
                .unwrap_or_else(|| Arc::new(FileSecretStore::new(directory.join("secrets")))),
            identity: open.identity,
            directory,
            install,
            coordination: open.coordination,
            confirmation,
            #[cfg(test)]
            expose: None,
        };
        Ok((setup, open.network))
    }
    fn opened(
        (scope, network, revoked): Ready,
        input: mpsc::Sender<Command>,
        cleanup: mpsc::UnboundedSender<Command>,
        worker: Worker,
    ) -> Opened {
        let shared = Arc::new(Shared {
            input,
            cleanup,
            budget: Semaphore::new(ACTIVE_CALL_QUOTA),
            closed: AtomicBool::new(false),
            sessions: AtomicU64::new(1),
        });
        Opened {
            root: None,
            identity: None,
            revoked,
            network: network.map(|id| NetworkService::new(&shared, id)),
            database: Arc::new(Self {
                shared,
                scope,
                worker: Mutex::new(Some(worker)),
            }),
        }
    }
    /// The database domain.
    pub fn scope(&self) -> rocky_core::Scope {
        self.scope
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
    /// Makes this device a trusted node of its domain's coordinated logs,
    /// which it ends with seals and whose terminal entries it acknowledges;
    /// `None` ends that. The log is the application's and
    /// is never closed by Rocky. The coordinator also handles claims and
    /// revocation push.
    pub async fn coordinate(
        &self,
        log: Option<Arc<dyn rocky_credentials::Coordinator>>,
    ) -> Result<(), Failure> {
        self.shared
            .request(|reply| Command::Coordinate(log, reply))
            .await??;
        Ok(())
    }
    /// The value `claim` takes from the domain's `counter` on the composed
    /// coordination service: the next one, or the one it already took.
    /// Values are unique and strictly increasing across every trusted node,
    /// and a value is consumed once claimed: an abandoned one is a gap.
    ///
    /// A claim is a coordinator decision, never part of a transaction. Store
    /// the [`counters::Claim`](crate::counters::Claim) in the application's
    /// own pending row before the first attempt, then use the value in an
    /// ordinary transaction that also settles that row. After an uncertain
    /// result (`CoordinationError::Unavailable`) or a restart, claim again
    /// with the stored claim: it answers the same value within
    /// `counters::CLAIM_WINDOW` of its issue time and the counter's latest
    /// `CLAIM_RECORDS` claims; beyond that it is refused with `Expired` and
    /// never takes a second value. The domain's trusted nodes claim, and so
    /// do holders of a claim right (`grant_claims`) for that counter.
    pub async fn claim(
        &self,
        counter: crate::counters::Name,
        claim: crate::counters::Claim,
    ) -> Result<u64, Failure> {
        self.shared
            .request(|reply| Command::Claim(counter, claim, reply))
            .await?
    }
    /// `counter`'s last claimed value and, for `claim`, what it took: a
    /// linearizable read for reconciliation after a restart.
    pub async fn counter(
        &self,
        counter: crate::counters::Name,
        claim: Option<crate::counters::Claim>,
    ) -> Result<crate::counters::Reading, Failure> {
        self.shared
            .request(|reply| Command::Counter(counter, claim, reply))
            .await?
    }
    /// A SQL connection for a database driver: the writer is a secure SQL
    /// session, a reader steps snapshot cursors.
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
    /// How a committed transaction with replayed writes stands here:
    /// `None` until one of its writes replayed. A local commit stays
    /// visible until replay drops it. `Applied` and `Dropped` are final
    /// (`sealed`) once a seal covers the transaction.
    pub async fn transaction_outcome(
        &self,
        token: rocky_core::TransactionToken,
    ) -> Result<Option<rocky_core::replay::TransactionOutcome>, Failure> {
        self.shared
            .request(|reply| Command::Outcome(token, reply))
            .await?
    }
    /// Returns once every transaction committed before the call is durable
    /// against power loss, as `Durability::Flushed` acknowledges at commit.
    /// Durability is a prefix of the local log, so a buffered transaction
    /// whose `commit` returned (its token included) is covered. Use it
    /// before an irreversible external effect of buffered work.
    pub async fn flush(&self) -> Result<(), Failure> {
        self.shared.request(Command::Flush).await?
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

/// A database a simulation serves on its own thread, and its node.
#[cfg(test)]
pub(crate) struct Hosted {
    pub(crate) opened: Opened,
    /// Upgrade only for a check; a held node outlives a crash.
    pub(crate) node: std::sync::Weak<Node>,
}
#[cfg(test)]
impl Database {
    /// Serves the database on a `LocalSet` of the calling task's thread
    /// instead of its own thread, so a paused clock and one thread drive
    /// every node of a simulation. Call it inside a `LocalSet`. While
    /// `ahead` holds seconds, the database's local tasks see this thread's
    /// clock that far ahead, as a device whose wall clock runs ahead does.
    pub(crate) async fn host(
        open: Open<'_>,
        install: Install,
        ahead: Arc<AtomicU64>,
    ) -> Result<Hosted, Failure> {
        let (mut setup, network) = Self::setup(open, install, None)?;
        let (expose, exposed) = std::sync::mpsc::channel();
        setup.expose = Some(expose);
        let (input, receiver) = mpsc::channel(32);
        let (cleanup, cleanups) = mpsc::unbounded_channel();
        let (hello, greeting) = oneshot::channel();
        let hello = Hello::Task(std::sync::Mutex::new(Some(hello)));
        let task = crate::reactor::spawn_local("host", async move {
            let local = tokio::task::LocalSet::new();
            let mut served =
                std::pin::pin!(local.run_until(run(setup, network, receiver, cleanups, &hello)));
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
        let opened = Self::opened(greeting?, input, cleanup, Worker::Task(task));
        let node = exposed
            .try_recv()
            .map_err(|_| "the database exposed no node")?;
        Ok(Hosted { opened, node })
    }
    /// Ends a hosted database at once: its tasks are dropped mid-step, as
    /// a process death would stop them; nothing flushes or closes first.
    pub(crate) async fn crash(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some(Worker::Task(task)) = self.worker.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
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
    #[cfg(feature = "qualification")]
    pub fn measurement_id(&self) -> u64 {
        self.id
    }
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
                #[cfg(feature = "qualification")]
                queued: web_time::Instant::now(),
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
    pub(crate) fn discard(&self, statement: u64) {
        let _ = self.shared.cleanup.send(Command::Sql {
            connection: self.id,
            call: connection::Call::Finish { statement },
            #[cfg(feature = "qualification")]
            queued: web_time::Instant::now(),
            reply: oneshot::channel().0,
        });
    }
    /// Drops the rest of `statement`'s result.
    pub async fn finish(&self, statement: u64) -> Result<(), SqlFailure> {
        self.call(connection::Call::Finish { statement })
            .await
            .map(drop)
    }
    /// The transaction this connection's last `COMMIT` published, as
    /// `Database::transaction_outcome` and table changes name it; `None`
    /// when it published nothing.
    pub async fn committed(&self) -> Result<Option<rocky_core::TransactionToken>, SqlFailure> {
        match self.call(connection::Call::Committed).await? {
            connection::Reply::Token(token) => Ok(token),
            _ => Err(connection::failure(connection::code::MISUSE, "no token")),
        }
    }
    /// `BEGIN` under a handle chosen now: the token its commit publishes.
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
    /// Closes the connection: an open transaction rolls back, its cursors
    /// end. Closing again does nothing.
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

/// One caller's independent waits and requests on a shared database.
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
        self.send_signed(None, request).await
    }
    pub(crate) async fn send_signed<T>(
        &self,
        identity: Option<Arc<dyn crate::IdentityProvider>>,
        request: impl FnOnce(oneshot::Sender<Result<T, Failure>>) -> crate::request::Request,
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
        self.shared
            .request(|reply| Command::Request {
                session: self.id,
                ending: self.closed.clone(),
                request: Box::new(match identity {
                    Some(identity) => {
                        crate::request::Request::WithIdentity(identity, Box::new(request(reply)))
                    }
                    None => request(reply),
                }),
            })
            .await?
            .map_err(crate::CallFailure::boxed)
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
    /// Observes owned runtime facts without dialing or starting a worker.
    pub async fn status(&self) -> Result<crate::DatabaseStatus, Failure> {
        self.send(crate::request::Request::Status).await
    }
    /// Waits for a changed snapshot; closing this session ends only its waits.
    pub async fn status_after(&self, token: String) -> Result<crate::DatabaseStatus, Failure> {
        self.send(|reply| crate::request::Request::StatusAfter(token, reply))
            .await
    }
    /// Reconciles selected peers and preserves each attempt's outcome.
    pub async fn sync_once(&self) -> Result<crate::ReconciliationReport, Failure> {
        self.send(crate::request::Request::Sync).await
    }
    /// Supplies the identity only for this sync, including certification after a tip change.
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
    /// Resumes networking after the application enters the foreground.
    pub async fn enter_foreground(&self) -> Result<(), Failure> {
        self.send(crate::request::Request::Foreground).await
    }
    /// Suspends background networking and drains its live links.
    pub async fn enter_background(&self) -> Result<(), Failure> {
        self.send(crate::request::Request::Background).await
    }
    /// Ends this device's authority, optionally removing its local data at close.
    pub async fn logout(&self, purge_on_close: bool) -> Result<(), Failure> {
        self.send(|reply| crate::request::Request::Logout(purge_on_close, reply))
            .await
    }
    async fn change_tip(
        &self,
        change: crate::request::TipChange,
        root: Option<Arc<dyn crate::IdentityProvider>>,
    ) -> Result<crate::TipStatus, Failure> {
        self.send(|reply| crate::request::Request::ChangeTip(change, node::Root(root), reply))
            .await
    }
    /// The followed bookmark, tip, and peer progress.
    pub async fn tip_status(&self) -> Result<crate::TipStatus, Failure> {
        self.change_tip(crate::request::TipChange::Status, None)
            .await
    }
    pub async fn inspect_tips(&self) -> Result<crate::TipInspection, Failure> {
        self.send(crate::request::Request::InspectTips).await
    }
    pub async fn seal(&self, close: bool) -> Result<crate::TipStatus, Failure> {
        self.change_tip(crate::request::TipChange::Seal { close }, None)
            .await
    }
    /// The root recovers the followed tip's stuck coordinated log.
    pub async fn recover(
        &self,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::TipStatus, Failure> {
        self.change_tip(crate::request::TipChange::Recover, Some(root))
            .await
    }
    pub async fn set_genesis_minimum(
        &self,
        minimum_in_sync: u32,
    ) -> Result<crate::TipStatus, Failure> {
        self.change_tip(
            crate::request::TipChange::GenesisMinimum { minimum_in_sync },
            None,
        )
        .await
    }
    pub async fn set_root_genesis_minimum(
        &self,
        root: Arc<dyn crate::IdentityProvider>,
        minimum_in_sync: u32,
    ) -> Result<crate::TipStatus, Failure> {
        self.change_tip(
            crate::request::TipChange::GenesisMinimum { minimum_in_sync },
            Some(root),
        )
        .await
    }
    pub async fn compact(&self, keep: usize) -> Result<crate::TipCompaction, Failure> {
        self.send(|reply| crate::request::Request::Compact(keep, reply))
            .await
    }
    pub async fn compaction(&self) -> Result<crate::TipCompaction, Failure> {
        self.send(crate::request::Request::Compaction).await
    }
    pub async fn kept_seals(&self) -> Result<crate::KeptSeals, Failure> {
        self.send(crate::request::Request::KeptSeals).await
    }
    pub async fn set_kept_seals(
        &self,
        kept: crate::KeptSeals,
    ) -> Result<crate::KeptSeals, Failure> {
        self.send(|reply| crate::request::Request::SetKeptSeals(kept, reply))
            .await
    }
    pub async fn pull_seal(&self, seal: &str) -> Result<crate::TipStatus, Failure> {
        self.change_tip(crate::request::TipChange::Pull { seal: seal.into() }, None)
            .await
    }
    pub async fn open_tip(&self, seal: &str, minimum_in_sync: u32) -> Result<String, Failure> {
        self.open_tip_signed(seal, minimum_in_sync, None).await
    }
    /// Supplies the identity when opening starts another local writer chain.
    pub async fn open_tip_with_identity(
        &self,
        seal: &str,
        minimum_in_sync: u32,
        identity: Arc<dyn crate::IdentityProvider>,
    ) -> Result<String, Failure> {
        self.open_tip_signed(seal, minimum_in_sync, Some(identity))
            .await
    }
    async fn open_tip_signed(
        &self,
        seal: &str,
        minimum_in_sync: u32,
        identity: Option<Arc<dyn crate::IdentityProvider>>,
    ) -> Result<String, Failure> {
        let seal = data_encoding::HEXLOWER
            .decode(seal.as_bytes())?
            .try_into()
            .map_err(|_| "expected a seal digest")?;
        self.send_signed(identity, |reply| {
            crate::request::Request::OpenTip(seal, minimum_in_sync, reply)
        })
        .await
    }
    pub async fn create_bookmark(
        &self,
        name: &str,
        tip: Option<&str>,
    ) -> Result<crate::TipStatus, Failure> {
        self.change_tip(
            crate::request::TipChange::Create {
                name: name.into(),
                tip: tip.map(str::to_owned),
            },
            None,
        )
        .await
    }
    pub async fn move_bookmark(&self, name: &str, tip: &str) -> Result<crate::TipStatus, Failure> {
        self.change_tip(
            crate::request::TipChange::Move {
                name: name.into(),
                tip: tip.into(),
            },
            None,
        )
        .await
    }
    pub async fn delete_bookmark(&self, name: &str) -> Result<crate::TipStatus, Failure> {
        self.change_tip(
            crate::request::TipChange::Delete { name: name.into() },
            None,
        )
        .await
    }
    pub async fn follow(&self, name: &str) -> Result<crate::TipStatus, Failure> {
        self.change_tip(
            crate::request::TipChange::Follow { name: name.into() },
            None,
        )
        .await
    }
    pub async fn follow_when_ready(&self, name: &str) -> Result<crate::TipStatus, Failure> {
        self.change_tip(
            crate::request::TipChange::FollowWhenReady { name: name.into() },
            None,
        )
        .await
    }
    /// The root grants trust to an issuer identity.
    pub async fn trust(
        &self,
        identity: &str,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::TrustGrant, Failure> {
        let identity = identity.parse()?;
        self.send(|reply| crate::request::Request::Trust(identity, node::Root(Some(root)), reply))
            .await
    }
    /// Ends a trusted identity. Lost lets its first enrollees renew elsewhere;
    /// Compromised ends them too. A merged compromise cannot be undone by loss.
    pub async fn revoke_trusted(
        &self,
        identity: &str,
        mode: rocky_credentials::RevocationMode,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::RevocationResult, Failure> {
        self.revoke_identity_with_root(identity, mode, Some(root), true)
            .await
    }
    /// "This is everything: sign out everywhere else." The root ends every
    /// node except this one and those `keep` names, including nodes this
    /// store never saw, and keeps their operations only up to what this
    /// replica applied: anything not on this device now is discarded on
    /// every replica. A later reset follows this one; resets signed
    /// concurrently end each other's devices. Devices a kept device admits
    /// afterwards stay. Fails on a device a reset already ended.
    pub async fn sign_out_everywhere_else(
        &self,
        root: Arc<dyn crate::IdentityProvider>,
        keep: crate::ResetKeep,
    ) -> Result<crate::ResetReceipt, Failure> {
        self.send(|reply| {
            crate::request::Request::SignOutEverywhereElse(node::Root(Some(root)), keep, reply)
        })
        .await
    }
    /// Whether an owner reset fenced this device's view.
    pub async fn freshness(&self) -> Result<crate::Freshness, Failure> {
        self.send(crate::request::Request::Freshness).await
    }
    pub async fn revoke_identity(
        &self,
        identity: &str,
    ) -> Result<crate::RevocationResult, Failure> {
        self.revoke_identity_with_root(
            identity,
            rocky_credentials::RevocationMode::Lost,
            None,
            true,
        )
        .await
    }
    /// Root revocations optionally retain tip statements through the local heads.
    pub async fn revoke_identity_with_root(
        &self,
        identity: &str,
        mode: rocky_credentials::RevocationMode,
        root: Option<Arc<dyn crate::IdentityProvider>>,
        keep: bool,
    ) -> Result<crate::RevocationResult, Failure> {
        let identity = identity.parse()?;
        self.send(|reply| {
            crate::request::Request::RevokeIdentity(identity, mode, node::Root(root), keep, reply)
        })
        .await
    }
    pub async fn install_trust(&self, grant: &str) -> Result<crate::TipStatus, Failure> {
        self.change_tip(
            crate::request::TipChange::InstallTrust {
                grant: grant.into(),
            },
            None,
        )
        .await
    }
    /// Grants counter claim rights; `None` covers every counter.
    pub async fn grant_claims(
        &self,
        identity: &str,
        counters: Option<&[&[u8]]>,
        delegations: u8,
    ) -> Result<crate::ClaimGrant, Failure> {
        let identity = identity.parse()?;
        let counters = counters.map(|names| names.iter().map(|name| name.to_vec()).collect());
        self.send(|reply| {
            crate::request::Request::GrantClaims(identity, counters, delegations, reply)
        })
        .await
    }
    pub async fn install_claims(&self, grant: &crate::ClaimGrant) -> Result<(), Failure> {
        self.send(|reply| crate::request::Request::InstallClaims(grant.clone(), reply))
            .await
    }
    pub async fn held_claims(&self) -> Result<Option<crate::ClaimGrant>, Failure> {
        self.send(crate::request::Request::HeldClaims).await
    }
    pub async fn revoke_claims(&self, identity: &str) -> Result<(), Failure> {
        let identity = identity.parse()?;
        self.send(|reply| crate::request::Request::RevokeClaims(identity, reply))
            .await
    }
    pub async fn drop_tip(&self, tip: &str) -> Result<crate::TipStatus, Failure> {
        self.change_tip(crate::request::TipChange::Drop { tip: tip.into() }, None)
            .await
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
                .cleanup
                .send(Command::End(self.id, oneshot::channel().0));
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

type Link = (crate::transport::Connected, bool);

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
    let exists = rocky_platform::fs::exists(path);
    let resuming = install.resumes_interrupted(directory)?;
    if exists && !matches!(install, Install::Installed) && !resuming {
        return Err("a domain exists in this directory: open it instead".into());
    }
    if !exists && matches!(install, Install::Installed) {
        return Err("no domain in this directory: create or join one".into());
    }
    if exists && !resuming && !rocky_platform::fs::exists(directory.join("rocky.sqlite")) {
        return Err(
            if rocky_platform::fs::exists(directory.join(crate::backup::MARKER)) {
                "a restore is unfinished in this directory: restore again with its backup"
            } else {
                "existing identity has no runtime database; recovery is required"
            }
            .into(),
        );
    }
    match install {
        Install::Create {
            ttl,
            root,
            personal,
            ..
        } => {
            let root = root
                .lock()
                .map_err(|_| "root poisoned")?
                .take()
                .ok_or(crate::enrollment::PublisherRequired("creating a domain"))?;
            let settings = match personal {
                Some(personal) => {
                    if setup.identity.is_some() {
                        return Err(crate::enrollment::personal_refusal(
                            rocky_credentials::PersonalError::WrongOwner,
                        ));
                    }
                    let discovery = personal
                        .discovery
                        .lock()
                        .map_err(|_| "discovery poisoned")?
                        .take()
                        .ok_or("the personal create was already attempted")?;
                    let installing = crate::personal::Installing::resume(
                        directory,
                        secrets,
                        &*root,
                        (&personal.namespace, discovery),
                    )?;
                    if installing.resumed() {
                        remove_installation(directory)?;
                    }
                    let settings = Settings::create_personal(
                        schema,
                        secrets,
                        *ttl,
                        (personal.namespace.clone(), installing),
                        &*root,
                    )?;
                    *personal.coverage.lock().map_err(|_| "coverage poisoned")? =
                        Some(crate::personal::RestoreCoverage::held(
                            &settings,
                            &settings.vault(secrets),
                        )?);
                    settings
                }
                None => Settings::create(schema, secrets, *ttl, setup.identity.clone(), &*root)?,
            };
            drop(root);
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
                    if install.discards_secrets() {
                        let _ = vault.forget(settings.roles());
                    }
                    Err(error)
                }
            }
        }
        Install::Join { invitation, name } => {
            let network = network.ok_or(ServiceError::NetworkRequired("join"))?;
            let joining =
                Settings::prepare_join(path, (invitation, name), secrets, setup.identity.clone())?;
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
        Install::Identity { contact, confirmed } => {
            let network =
                network.ok_or(ServiceError::NetworkRequired("enroll an identity's node"))?;
            let provider = setup
                .identity
                .clone()
                .ok_or("another node of an identity needs that identity's provider")?;
            let prepared = Settings::prepare_identity(path, secrets, &provider, contact)?;
            let key = Settings::transport_key(&prepared.1)?;
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
        Install::SignIn(sign_in) => {
            if setup.identity.is_some() {
                return Err(crate::enrollment::personal_refusal(
                    rocky_credentials::PersonalError::WrongOwner,
                ));
            }
            crate::personal::sign_in::install(path, directory, secrets, schema, sign_in, network)
                .await
        }
        Install::Supplied(supplied) => {
            let settings =
                Settings::install(path, schema, secrets, supplied, setup.identity.clone())?;
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
        Install::Recover {
            archive,
            root,
            archived,
        } => {
            let root = root
                .lock()
                .map_err(|_| "root poisoned")?
                .as_deref()
                .map(|root| crate::RootKey::from_secret(root.secret()))
                .ok_or(crate::enrollment::PublisherRequired("recovering a domain"))?;
            let (settings, record) = crate::recovery::recover(
                path,
                directory,
                secrets,
                archive,
                &root,
                setup.identity.clone(),
            )
            .await?;
            drop(root);
            *archived.lock().map_err(|_| "archive poisoned")? = Some(Box::new(record));
            let Some(network) = network else {
                return Ok((settings, None));
            };
            let key = Settings::transport_key(&settings.vault(secrets))?;
            Ok((settings, Some(network.start(true, key).await?)))
        }
        Install::Installed => {
            let mut settings = Settings::load(path)?;
            if settings.logged_out {
                return Err("this device logged out; join the domain again".into());
            }
            let vault = settings.vault(secrets);
            let link = match network {
                Some(network) => {
                    let key = Settings::transport_key(&vault)?;
                    Some(network.start(settings.issuer(), key).await?)
                }
                None => None,
            };
            if settings.application(schema)? == crate::DeclaredSchema::Current
                && settings.schema != *schema
            {
                settings.schema = schema.clone();
                settings.save(path)?;
            }
            Ok((settings, link))
        }
    }
}

/// Readies an opened replica for this identity: its readable tables,
/// group keys and the node credential.
pub(crate) async fn attach(
    replica: &EncryptedReplica<rocky_core::TypedRows>,
    settings: &mut Settings,
    vault: &crate::secrets::Vault,
    domain: &crate::domain::Domain,
    path: &Path,
) -> Result<(), Failure> {
    replica
        .install_readable_tables(domain.logical().clone(), domain.scope, domain.readers())
        .await?;
    if settings.recover_groups(vault, domain, &replica.retained_partitions().await?)? {
        settings.save(path)?;
    }
    install_group_keys(replica, settings, vault, domain).await?;
    if replica.role().await?.role == rocky_secure::ReplicaRole::Partial {
        hold_late_group_members(replica, settings, domain).await?;
    }
    replica
        .register_credential_signer(rocky_credentials::PrivateKey::from_bytes(
            &vault.get("node")?,
        ))
        .await?;
    crate::escrow::keep(replica, settings, vault, domain).await
}

/// Registers the granted members of every group partition and installs
/// each readable member's keys, which derive from its group secret, also
/// for a table that joined the group in a later generation.
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

/// On a partial replica, holds a table that joined a group in a later
/// generation wherever its group partition is held.
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

/// Gives every granted scope without one the default retention policy.
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

/// Rows one statement inserts into a replayed table: its intent carries
/// them as bound values, and a partition's intent holds at most
/// `MAX_INTENT_STATEMENTS` statements.
const REPLAYED_INSERT_ROWS: usize = 1024;

/// Inserts a row of `table` for each row of `select`, with the next id of
/// its partition, inside the open transaction.
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
    if rocky_platform::fs::exists(&marker) {
        let saved: Marker = serde_json::from_slice(&rocky_platform::fs::read(&marker)?)?;
        if saved.recovery.is_some() {
            return Err("a recovery is unfinished in this directory".into());
        }
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
                recovery: None,
            })?,
        )?;
    }
    let (settings, link) = if rocky_platform::fs::exists(path) {
        let settings = Settings::load(path)?;
        let vault = settings.vault(secrets);
        let key = Settings::transport_key(&vault)?;
        let link = network.start(settings.issuer(), key).await?;
        (settings, link)
    } else {
        let fresh_single_use_identity = None;
        let joining =
            Settings::prepare_join(path, (invitation, ""), secrets, fresh_single_use_identity)?;
        let key = Settings::transport_key(&joining.vault)?;
        let (transport, owned) = network.start(false, key).await?;
        match joining.enroll(path, schema, &*transport).await {
            Ok(mut settings) => {
                let node =
                    rocky_secure::PrivateKey::from_bytes(&settings.vault(secrets).get("node")?);
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
    let placed = (|| -> Result<(), Failure> {
        if (settings.mesh, settings.domain) != (opened.identity.mesh, opened.identity.domain) {
            return Err("the enrolled domain differs from the backup's".into());
        }
        crate::recovery::fault::check(directory, crate::recovery::fault::Stage::Enrolled)?;
        crate::backup::place(&opened, directory)?;
        rocky_platform::fs::remove_dir_all(&staging)?;
        Ok(())
    })();
    if let Err(error) = placed {
        if link.1 {
            link.0.close().await;
        }
        return Err(error);
    }
    Ok((settings, Some(link)))
}

/// Records the durable intent to bootstrap from a checkpoint, as before
/// enrollment can persist the identity or when rows of a newly revoked
/// author arrive by checkpoint only.
fn record_checkpoint_bootstrap(directory: &Path) -> Result<(), Failure> {
    rocky_credentials::write_private(&directory.join("bootstrap.json"), b"true")
}

/// Envelopes shared after enrollment wait at the inviter: the bootstrap
/// intent stays until one exchange with it.
fn record_inviter_bootstrap(
    directory: &Path,
    inviter: &rocky_credentials::PublicKey,
) -> Result<(), Failure> {
    rocky_credentials::write_private(
        &directory.join("bootstrap.json"),
        &serde_json::to_vec(&serde_json::json!({"intent": true, "inviter": inviter.to_hex()}))?,
    )
}

/// Confirms a removal the installed schema implies before the network
/// starts or anything changes.
async fn confirm_removals(setup: &Setup, directory: &Path, path: &Path) -> Result<(), Failure> {
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
        crate::rollout::confirm(
            &settings,
            path,
            &setup.schema,
            setup.confirmation.as_deref(),
        )
        .await?;
    }
    Ok(())
}

/// Adopts a pending schema the replica already committed. The commit may
/// also have advanced the bookmark past the pending tip on the same schema.
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
    settings.save(path)?;
    Ok(())
}

/// The marker of a restore whose open steps are unfinished. A restored
/// file is rebound to this device's identity first; the marker goes once
/// the file holds its tables and keys, a recovery's once the root revoked
/// the lost identities too, and until then each open repeats these steps.
/// A recovery whose store took its keys opens as installed.
fn unfinished_restore(marker: &Path) -> Result<Option<crate::backup::Marker>, Failure> {
    if !rocky_platform::fs::exists(marker) {
        return Ok(None);
    }
    let saved: crate::backup::Marker = serde_json::from_slice(&rocky_platform::fs::read(marker)?)?;
    Ok((saved.recovery != Some(crate::backup::Recovery::Keyed)).then_some(saved))
}

/// Drains the work of migration commits, which precede the revision
/// subscription, before enrollment or the first reconciliation can
/// trigger it instead.
async fn drain_migration_work(
    replica: &EncryptedReplica<rocky_core::TypedRows>,
    scope: rocky_core::Scope,
) -> Result<(), Failure> {
    replica.flush().await?;
    replica.maintain().await;
    replica.finalize_path(scope).await?;
    Ok(())
}

/// Recovery is for lost trusted nodes: the root revokes them before anyone
/// writes, and is dropped. The marker stays until the revocations are
/// applied, so until then the directory opens only by recovering again.
async fn revoke_lost_trusted(
    node: &Node,
    root: &std::sync::Mutex<Option<Box<crate::RootKey>>>,
    archived: &std::sync::Mutex<Option<Box<Settings>>>,
    directory: &Path,
) -> Result<Vec<rocky_credentials::PublicKey>, Failure> {
    let archived = archived
        .lock()
        .map_err(|_| "archive poisoned")?
        .take()
        .ok_or("the recovered archive was not opened")?;
    let root: Arc<crate::RootKey> = root
        .lock()
        .map_err(|_| "root poisoned")?
        .take()
        .ok_or(crate::enrollment::PublisherRequired("recovering a domain"))?
        .into();
    let unfinished = |error: Failure| -> Failure {
        format!("recovered, but {error}; recover again with its backup and the root").into()
    };
    crate::recovery::fault::check(directory, crate::recovery::fault::Stage::Revoking)
        .map_err(unfinished)?;
    let revoked = node
        .revoke_lost(root, &archived)
        .await
        .map_err(unfinished)?;
    rocky_platform::fs::remove_file(directory.join(crate::backup::MARKER))?;
    Ok(revoked)
}

/// Serves a database on the calling thread: its tasks stay on it, and
/// transports run theirs on two workers.
#[cfg(not(target_family = "wasm"))]
fn serve_on_thread(
    setup: Setup,
    network: Option<Network>,
    receiver: mpsc::Receiver<Command>,
    cleanups: mpsc::UnboundedReceiver<Command>,
    hello: &Hello,
) {
    let result = (|| -> Result<(), Failure> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rocky-sdk-io")
            .thread_stack_size(rocky_platform::task::THREAD_STACK)
            .enable_all()
            .build()?;
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(run(setup, network, receiver, cleanups, hello)))
    })();
    if let Err(error) = result {
        let _ = hello.send(Err(error));
    }
}

async fn run(
    setup: Setup,
    network: Option<Network>,
    input: mpsc::Receiver<Command>,
    cleanups: mpsc::UnboundedReceiver<Command>,
    hello: &Hello,
) -> Result<(), Failure> {
    let serving = run_inner(setup, network, input, cleanups, hello);
    #[cfg(feature = "qualification")]
    let serving = crate::reactor::scope(crate::ReactorMeasurements::default(), serving);
    serving.await
}

async fn run_inner(
    mut setup: Setup,
    network: Option<Network>,
    mut input: mpsc::Receiver<Command>,
    mut cleanups: mpsc::UnboundedReceiver<Command>,
    hello: &Hello,
) -> Result<(), Failure> {
    let directory = setup.directory.clone();
    if matches!(setup.install, Install::Installed)
        && rocky_platform::fs::exists(directory.join("identity.json"))
    {
        Settings::load(&directory.join("identity.json"))?;
    }
    rocky_secure::check_replica_format(&directory.join("rocky.sqlite")).await?;
    if matches!(setup.install, Install::Installed) {
        if !rocky_platform::fs::exists(directory.join("identity.json")) {
            return Err(if rocky_platform::fs::exists(directory.join("identity.join.json")) {
                "a join in this directory is unfinished: join again with its or a newer invitation"
            } else {
                "no domain in this directory: create or join one"
            }
            .into());
        }
        Settings::load(&directory.join("identity.json"))?;
    }
    rocky_platform::fs::create_dir_all(&directory)?;
    let _lock = rocky_platform::fs::Owner::acquire(&directory.join("sdk"))
        .await
        .map_err(|_| "data directory is already in use")?;
    rocky_secure::check_replica_format(&directory.join("rocky.sqlite")).await?;
    let path = directory.join("identity.json");
    if setup.install.bootstraps_from_checkpoint() && !rocky_platform::fs::exists(&path) {
        record_checkpoint_bootstrap(&directory)?;
    }
    if !matches!(setup.install, Install::Recover { .. }) {
        crate::recovery::refuse_unfinished(&directory)?;
    }
    if matches!(setup.install, Install::Installed) {
        confirm_removals(&setup, &directory, &path).await?;
    }
    let fresh = setup.install.unwinds_on_failure() && !rocky_platform::fs::exists(&path);
    let (mut settings, link) = match identity(&path, &setup, network).await {
        Ok(installed) => installed,
        Err(error) if fresh => return Err(unwind(&directory, None, error)),
        Err(error) => return Err(error),
    };
    if let (Install::Join { .. }, Some(inviter)) = (&setup.install, settings.issuers.first()) {
        record_inviter_bootstrap(&directory, inviter)?;
    }
    let startup_identity = setup.identity.take();
    let vault = settings.vault(&setup.secrets);
    let opened = async {
        let completed_interrupted_revocation = settings.rotate(&vault)?.is_some();
        if completed_interrupted_revocation {
            settings.save(&path)?;
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
            if rocky_platform::fs::exists(&path) {
                let revokes_new_authors = settings.lacks(&renewed.revocations)?;
                if revokes_new_authors {
                    record_checkpoint_bootstrap(&directory)?;
                }
                settings.adopt(&path, &vault, &renewed)?;
            } else {
                settings.learn_revocations(&renewed.revocations)?;
                settings.credentials = renewed.chain;
            }
        }
        adopt_committed_pending_schema(&mut settings, &vault, &directory, &path).await?;
        let domain = settings.domain()?;
        let marker = directory.join(crate::backup::MARKER);
        let restored = unfinished_restore(&marker)?;
        let mut keys_awaiting_tables = Vec::new();
        let replica = if let Some(restored) = &restored {
            let mut config = settings.config(&vault, &domain)?;
            let store_may_lack_frames = restored.recovery.is_some();
            if store_may_lack_frames {
                keys_awaiting_tables = std::mem::take(&mut config.keys);
            }
            let replica = EncryptedReplica::open_restored(
                directory.join("rocky.sqlite"),
                domain.app.clone(),
                config.with_table_scopes(domain.scopes().map(|(_, scope)| scope).collect()),
                restored.predecessor,
            )
            .await?;
            crate::recovery::fault::check(&directory, crate::recovery::fault::Stage::Rebound)?;
            replica
        } else if let Some(bootstrap) = &settings.bootstrap {
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
        attach(&replica, &mut settings, &vault, &domain, &path).await?;
        crate::recovery::fault::check(&directory, crate::recovery::fault::Stage::Attached)?;
        let mut newest_first = keys_awaiting_tables;
        newest_first.sort_by_key(|key| std::cmp::Reverse(key.generation));
        for key in newest_first {
            replica.install_key_epoch(key).await?;
            crate::recovery::fault::check(&directory, crate::recovery::fault::Stage::Keyed)?;
        }
        match restored {
            Some(saved) if saved.recovery.is_some() => crate::recovery::mark(
                &directory,
                &crate::backup::Marker {
                    recovery: Some(crate::backup::Recovery::Keyed),
                    ..saved
                },
            )?,
            Some(_) => rocky_platform::fs::remove_file(&marker)?,
            None => {}
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
        let coordinator = setup
            .coordination
            .as_ref()
            .map(|client| {
                let key = rocky_credentials::PrivateKey::from_bytes(&vault.get("node")?);
                let certificate = settings
                    .credentials
                    .certificate()
                    .ok_or("missing node certificate")?
                    .clone();
                client.bind(key, certificate).map_err(Failure::from)
            })
            .transpose()?;
        settings.save(&path)?;
        Ok::<_, Failure>((domain, replica, coordinator))
    }
    .await;
    let (domain, replica, coordinator) = match opened {
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
    let node = Arc::new(
        Node::new(replica.clone(), domain, vault, settings, path.clone())
            .with_declared(setup.schema.clone()),
    );
    #[cfg(test)]
    if let Some(expose) = &setup.expose {
        let _ = expose.send(Arc::downgrade(&node));
    }
    node.coordinate_bound(coordinator);
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
        if let Install::Create {
            bookmark: Some(name),
            ..
        } = &setup.install
        {
            node.follow_new_bookmark(name).await?;
        }
        drain_migration_work(&replica, node.domain().scope).await?;
        crate::recovery::fault::check(&directory, crate::recovery::fault::Stage::Started)?;
        if matches!(setup.install, Install::SignIn(_)) {
            crate::personal::fault::check(&directory, crate::personal::fault::Stage::Saved)?;
        }
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
    let revoked = match &setup.install {
        Install::Recover { root, archived, .. } => {
            revoke_lost_trusted(&node, root, archived, &directory).await?
        }
        _ => Vec::new(),
    };
    let personal = setup.install.ends_personal_intent();
    if hello.send(Ok((scope, network, revoked))).is_err() {
        let abandoned: Failure = "database opener abandoned".into();
        let created_root_undelivered = fresh && matches!(setup.install, Install::Create { .. });
        return Err(if created_root_undelivered {
            discard(node, replica, &directory, &setup.install, abandoned).await
        } else {
            abandoned
        });
    }
    if personal && let Err(error) = crate::personal::finish(&directory) {
        tracing::warn!(%error, "the finished personal install intent could not be removed");
    }
    if rocky_platform::fs::exists(directory.join("bootstrap.json")) && network.is_some() {
        let joining = node.clone();
        crate::reactor::spawn_local("bootstrap", async move {
            if let Err(error) = joining.initial_sync().await {
                tracing::debug!(%error, "initial sync failed; its bootstrap intent stays for the next sync or open");
            }
        });
    }
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
            Command::Export(reply) => {
                crate::reactor::spawn_in(&mut tasks, "export_backup", async move {
                    let _ = reply.send(node.export_backup().await);
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
            Command::Coordinate(log, reply) => {
                let _ = reply.send(node.coordinate(log).await);
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

            Command::Claim(counter, claim, reply) => {
                crate::reactor::spawn_in(&mut tasks, "claim", async move {
                    let _ = reply.send(node.claim(counter, claim).await);
                });
            }

            Command::Counter(counter, claim, reply) => {
                crate::reactor::spawn_in(&mut tasks, "counter", async move {
                    let _ = reply.send(node.counter(counter, claim).await);
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
    recovering.abort();
    let _ = recovering.await;
    end_users_of_file(&node, &connections, &mut tasks).await;
    let purge = node.lifecycle.lock().await.purge_on_close();
    drop(node);
    settling.abort();
    let _ = settling.await;
    let mut result = close_file(replica).await;
    if purge && result.is_ok() {
        result = purge_directory(&directory).map_err(|e| e.to_string());
    }
    if let Some(reply) = shutdown {
        let _ = reply.send(result);
    }
    Ok(())
}
/// Ends everything that uses the file before it closes: sessions end, open
/// transactions roll back and cursors end, and calls that use the network
/// end within its drain bound. An attach still pending once the tasks are
/// joined is detached by the second pass.
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
}
/// Flushes and optimizes the replica, then drops this last handle, which
/// closes the file.
async fn close_file(replica: EncryptedReplica<rocky_core::TypedRows>) -> Result<(), String> {
    let flushed = replica.flush().await.map(drop).map_err(|e| e.to_string());
    replica.optimize().await;
    drop(replica);
    flushed
}
/// Flushes buffered SQL commits and runs their queued maintenance after
/// they answer. The flush runs on a blocking thread and covers every
/// commit that arrived during the previous flush. A flush failure surfaces
/// at the next call that exposes data, which flushes itself. Failed background
/// flushes retain maintenance and retry with backoff. Maintenance left over
/// retries soon: a concurrent commit leaves its revision pending too, and
/// the timer also covers work present before this driver's subscription.
async fn settle_commits(replica: EncryptedReplica<rocky_core::TypedRows>) {
    let mut revisions = replica.observe_revision();
    revisions.mark_changed();
    let mut retry = crate::scheduling::Retry::default();
    loop {
        let deadline = retry.deadline();
        tokio::select! {
            changed = revisions.changed() => if changed.is_err() { break },
            () = async {
                match deadline {
                    Some(deadline) => rocky_platform::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => {},
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
/// Removes the installation's files and secrets; a later join in
/// `directory` starts clean.
fn purge_directory(directory: &Path) -> Result<(), Failure> {
    remove_installation(directory)?;
    match rocky_platform::fs::remove_dir_all(directory.join("secrets")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}
/// Removes the installation's files, the identity first: what a crash
/// leaves is no domain, which a later create or install replaces.
fn remove_installation(directory: &Path) -> Result<(), Failure> {
    for name in [
        "identity.json",
        "rocky.sqlite",
        "rocky.sqlite-wal",
        "rocky.sqlite-shm",
        "revoked.json",
        "bootstrap.json",
        "mailbox.json",
    ] {
        match rocky_platform::fs::remove_file(directory.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}
/// Takes back a create, sign-in or supplied install that failed before it
/// was handed out, so the directory takes a retry: its files go, and a
/// created domain's secrets, whose root was never handed out either. A
/// supplied or personal install keeps the device keys it is retried with.
fn unwind(
    directory: &Path,
    created: Option<(&crate::secrets::Vault, Vec<String>)>,
    error: Failure,
) -> Failure {
    let next_attempt_finds_what_it_wrote = crate::personal::fault::crashed(&error);
    if next_attempt_finds_what_it_wrote {
        return error;
    }
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
/// As `unwind`, once the node started: it lets go of its network and file
/// first.
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
#[cfg(test)]
mod tests {
    use super::*;

    /// Close awaits the database thread's end instead of joining it, so
    /// the closer's single-threaded executor runs other tasks meanwhile.
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
        let schema = serde_json::to_string(&schema)?;
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
                coordination: None,
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
                coordination: None,
            },
            None,
        )?
        .database;
        let connection = database
            .connect(ConnectionOptions {
                read_only: true,
                durability: connection::Durability::Buffered,
                profile: None,
                transaction_timeout: None,
                binding: None,
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
            "identity.json",
            "rocky.sqlite",
            "rocky.sqlite-wal",
            "rocky.sqlite-shm",
            "revoked.json",
            "bootstrap.json",
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
        crate::recovery::fault::STOPS
            .lock()
            .map_err(|_| "poisoned")?
            .push((
                directory.to_path_buf(),
                crate::recovery::fault::Stage::Started,
            ));
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

    /// The root of a create that fails is never handed out: the domain it
    /// signed goes with it, and creating again succeeds.
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

    /// A supplied install that fails keeps the prepared device keys, so the
    /// same credentials install on a retry.
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
        let schema = serde_json::to_string(&schema)?;
        let install = || {
            Database::install(
                Open {
                    directory: &directory,
                    schema: &schema,
                    secrets: None,
                    identity: Some(identity.clone()),
                    network: None,
                    coordination: None,
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
        const SCHEMA: &str = r#"{"format":6,"application":"refusal","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"]}]}"#;
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
                    schema: SCHEMA,
                    secrets: None,
                    network: None,
                    coordination: None,
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
        const SCHEMA: &str = r#"{"format":6,"application":"reopen_numeric","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"]}]}"#;
        let directory = rocky_platform::fs::Scratch::new().await;
        let options = || Open {
            identity: None,
            directory: directory.path(),
            schema: SCHEMA,
            secrets: None,
            network: None,
            coordination: None,
        };
        let database = Database::create(options(), None)?.database;
        let session = database.session();
        snap::value(session.execute("BEGIN", Vec::new())).await?;
        let first = snap::value(session.next_id("items")).await?["id"]
            .as_str()
            .ok_or("missing id")?
            .to_owned();
        snap::value(session.execute(
            "INSERT INTO items(id,title) VALUES(?,'first')",
            vec![snap::row_id(&(first))],
        ))
        .await?;
        snap::value(session.execute("COMMIT", Vec::new())).await?;
        session.close().await;
        database.close().await?;

        let database = Database::open(options())?.database;
        let session = database.session();
        let rows = snap::value(session.read(rocky_sdk::ReadQuery {
            sql: "SELECT id FROM items".to_owned(),
            values: Vec::new(),
            max_rows: 8,
            max_bytes: None,
            cursor: None,
        }))
        .await?;
        assert_eq!(snap::cell_id(&rows["rows"][0][0]).to_string(), first);
        snap::value(session.execute("BEGIN", Vec::new())).await?;
        let second = snap::value(session.next_id("items")).await?["id"]
            .as_str()
            .ok_or("missing id")?
            .to_owned();
        let (first_id, second_id): (rocky_sdk::RowIdentity, rocky_sdk::RowIdentity) =
            (first.parse()?, second.parse()?);
        assert_eq!(second_id.writer(), first_id.writer());
        assert_eq!(second_id.sequence(), first_id.sequence() + 1);
        snap::value(session.execute(
            "INSERT INTO items(id,title) VALUES(?,'second')",
            vec![snap::row_id(&(second))],
        ))
        .await?;
        snap::value(session.execute("COMMIT", Vec::new())).await?;
        let rows = snap::value(session.read(rocky_sdk::ReadQuery {
            sql: "SELECT id FROM items ORDER BY id".to_owned(),
            values: Vec::new(),
            max_rows: 8,
            max_bytes: None,
            cursor: None,
        }))
        .await?;
        assert_eq!(rows["rows"].as_array().ok_or("missing rows")?.len(), 2);
        assert_eq!(snap::cell_id(&rows["rows"][1][0]).to_string(), second);
        session.close().await;
        database.close().await?;
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn saturated_waits_cannot_block_session_shutdown() -> Result<(), Failure> {
        let directory = rocky_platform::fs::Scratch::new().await;
        let opened = Database::create(
            Open {
                identity: None,
                directory: directory.path(),
                schema: r#"{"application":"saturation","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#,
                secrets: None,
                network: None,
                coordination: None,
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
}
