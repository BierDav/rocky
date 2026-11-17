use crate::{Database, Failure, IdentityProvider, Network, Open, Opened, Schema, SecretStore};
use std::{path::PathBuf, sync::Arc};

/// Reusable installation settings. Networking is opt-in; lifecycle operations are explicit.
#[derive(Clone)]
pub struct Configuration {
    directory: PathBuf,
    schema: String,
    secrets: Option<Arc<dyn SecretStore>>,
    identity: Option<Arc<dyn IdentityProvider>>,
    network: Option<Network>,
    coordination: Option<crate::CoordinatorSlot>,
    name: String,
    sign_in_wait: std::time::Duration,
}
impl Configuration {
    /// The bookmark `create` makes and follows on the genesis tip.
    pub const BOOKMARK: &'static str = "main";
    /// Validates the schema before touching the data directory.
    pub fn new(directory: impl Into<PathBuf>, schema: &Schema) -> Result<Self, Failure> {
        let schema = serde_json::to_string(schema)?;
        Schema::parse(&schema)?;
        Ok(Self {
            directory: directory.into(),
            schema,
            secrets: None,
            identity: None,
            network: None,
            coordination: None,
            name: String::new(),
            sign_in_wait: std::time::Duration::from_secs(15),
        })
    }
    #[must_use]
    pub fn secrets(mut self, store: Arc<dyn SecretStore>) -> Self {
        self.secrets = Some(store);
        self
    }
    #[must_use]
    pub fn identity(mut self, provider: Arc<dyn IdentityProvider>) -> Self {
        self.identity = Some(provider);
        self
    }
    /// Reused for create, open and join. Borrowed transports remain application-owned.
    #[must_use]
    pub fn network(mut self, network: Network) -> Self {
        self.network = Some(network);
        self
    }
    /// What this device calls itself in a join request, for the issuer
    /// that admits it; at most 256 bytes.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
    /// How long a discovering [`Self::sign_in`] waits for a first source
    /// before it reports `SourceUnavailable`; 15 seconds by default. It
    /// only bounds the wait: sources answer as soon as they are found.
    #[must_use]
    pub fn sign_in_wait(mut self, wait: std::time::Duration) -> Self {
        self.sign_in_wait = wait;
        self
    }
    /// An explicit coordinator client. Creating with one names it in the
    /// database's owner-signed settings, so the database is coordinated from
    /// its first tip; a database whose settings name another refuses it.
    #[must_use]
    pub fn coordination(mut self, coordination: Arc<dyn rocky_credentials::Coordinator>) -> Self {
        self.coordination = Some(crate::CoordinatorSlot::Client(coordination));
        self
    }
    /// Reaches whichever coordinator the database's owner-signed settings
    /// name, once a sync brings them, instead of an explicit client.
    #[must_use]
    pub fn coordinator_connector(
        mut self,
        connector: Arc<dyn rocky_credentials::CoordinatorConnector>,
    ) -> Self {
        self.coordination = Some(crate::CoordinatorSlot::Connector(connector));
        self
    }
    fn options(&self) -> Open<'_> {
        Open {
            directory: &self.directory,
            schema: &self.schema,
            secrets: self.secrets.clone(),
            identity: self.identity.clone(),
            network: self.network.clone(),
            coordination: self.coordination.clone(),
        }
    }
    /// Creates an installation under a fresh root key, which
    /// `Opened::root` hands back once: show its phrase to the user or keep
    /// its secret behind a passkey. See [`Self::create_with_root`].
    pub async fn create(&self) -> Result<Opened, Failure> {
        let root = crate::RootKey::generate();
        let created = crate::RootKey::from_secret(root.secret());
        let mut opened = self.create_with_root(Arc::new(root)).await?;
        opened.root = Some(created);
        Ok(opened)
    }
    /// Creates an installation; refuses an existing one. Startup runs off the
    /// caller's async executor: natively on the database's own thread. A
    /// create that fails leaves nothing behind, and can be retried.
    ///
    /// `root`, the domain's offline root, signs the genesis and a trusted
    /// grant to this device's own identity, and is dropped: the device never
    /// runs as the root. A passkey or a phrase-backed [`crate::RootKey`]
    /// supplies it. The creating device follows a new bookmark,
    /// [`Self::BOOKMARK`], so its sync passes seal routinely: only seals make
    /// replay drops final and bound the undo history. `Database::create`
    /// leaves bookmarks to the caller.
    pub async fn create_with_root(
        &self,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<Opened, Failure> {
        Database::launch_create(self.options(), None, root, (Self::BOOKMARK, None)).await
    }
    /// Creates the personal database of the account `prf` names in
    /// `namespace`: the root is `RootKey::from_secret(prf)`, the owner its
    /// derived identity, and the namespace also locates the mesh and
    /// domain, so a device that knows only the passkey finds it. Data keys
    /// stay random. Root, identity and PRF are erased when this returns;
    /// the device keeps its node keys, reader keys and the discovery
    /// capability, and reopens with [`Self::open`] alone.
    ///
    /// Refuses a directory holding any state, including a sign-in's
    /// install intent, and another account's as `WrongLocalAccount`,
    /// without touching it. An interrupted create keeps its install intent:
    /// calling this again with the same passkey resumes under the same node. An explicit [`Self::identity`] is
    /// refused: the owner is the root's own. Only the owner's devices and
    /// explicitly granted custody principals join the database.
    pub async fn create_personal(
        &self,
        prf: crate::VerifiedPasskeyPrf,
        namespace: crate::PersonalNamespace,
    ) -> Result<crate::PersonalOpened, crate::PersonalSignInError> {
        use crate::PersonalSignInError as Refused;
        let account = rocky_credentials::PersonalAccount::derive(prf, &namespace)
            .map_err(|_| Refused::PrfUnsupported)?;
        if self.identity.is_some() {
            return Err(Refused::Failed(crate::enrollment::personal_refusal(
                rocky_credentials::PersonalError::WrongOwner,
            )));
        }
        crate::personal::check_empty(&self.directory, &account)?;
        let (root, discovery) = account.into_parts();
        let coverage = Arc::new(std::sync::Mutex::new(None));
        let personal = crate::personal::Create {
            namespace,
            discovery: std::sync::Mutex::new(Some(discovery)),
            coverage: coverage.clone(),
        };
        let opened = Database::launch_create(
            self.options(),
            None,
            Arc::new(root),
            (Self::BOOKMARK, Some(Box::new(personal))),
        )
        .await
        .map_err(Refused::from_failure)?;
        let restored = coverage
            .lock()
            .map_err(|_| Refused::Failed("coverage poisoned".into()))?
            .take()
            .unwrap_or_default();
        Ok(crate::PersonalOpened {
            opened,
            restored,
            freshness: crate::Freshness::LocallyAnchored,
        })
    }
    /// Signs this empty device in to the personal database the account
    /// `prf` names in `namespace`, as one more owner device: from a sibling
    /// or custodian announcing under the passkey's discovery topic on the
    /// configured network, or from a verified backup without one. The root
    /// opens the escrow the source holds and the owner certifies a fresh
    /// node; nothing is revoked, rotated or granted to anyone else, and no
    /// code or other device confirms it. Root, identity and PRF are erased
    /// when this returns.
    ///
    /// No reachable source is `SourceUnavailable`, never an empty
    /// database, and leaves the directory as it was. Sources whose genesis
    /// differs are `GenesisConflict`, and both stay. An interrupted sign-in
    /// resumes under the same node, certificate and publication ID when
    /// called again with the same passkey; another account's state is
    /// `WrongLocalAccount`, and an installed database or a create's intent
    /// `ExistingState`, untouched. `restored` names what the source's
    /// escrow covered; `freshness` is `Unwitnessed` unless another owner
    /// device answered, and [`crate::Session::freshness`] reports the same
    /// until another owner device reconciles with this one. A sign-in that
    /// saved its database but stopped before handing it out returns it.
    pub async fn sign_in(
        &self,
        prf: crate::VerifiedPasskeyPrf,
        namespace: crate::PersonalNamespace,
        source: crate::SignInSource,
    ) -> Result<crate::PersonalOpened, crate::PersonalSignInError> {
        use crate::PersonalSignInError as Refused;
        let account = rocky_credentials::PersonalAccount::derive(prf, &namespace)
            .map_err(|_| Refused::PrfUnsupported)?;
        if self.identity.is_some() {
            return Err(Refused::Failed(crate::enrollment::personal_refusal(
                rocky_credentials::PersonalError::WrongOwner,
            )));
        }
        let secrets = self.secrets.clone().unwrap_or_else(|| {
            Arc::new(crate::secrets::FileSecretStore::new(
                self.directory.join("secrets"),
            ))
        });
        if let Some(saved_but_never_handed_out) =
            crate::personal::prepare_sign_in(&self.directory, &account, &secrets)?
        {
            drop(account);
            let opened = Database::launch_open(self.options())
                .await
                .map_err(Refused::from_failure)?;
            return Ok(crate::PersonalOpened {
                opened,
                restored: saved_but_never_handed_out.restored,
                freshness: saved_but_never_handed_out.freshness,
            });
        }
        match &source {
            crate::SignInSource::Discover if self.network.is_none() => {
                return Err(Refused::SourceUnavailable);
            }
            crate::SignInSource::Backup(backup) => {
                let locator = account.locator();
                let database = rocky_credentials::DatabaseContext {
                    publisher: locator.publisher,
                    scope: locator.scope(
                        &Schema::parse(&self.schema)
                            .map_err(Refused::Failed)?
                            .application,
                    ),
                };
                backup.check(&account, &database)?;
            }
            crate::SignInSource::Discover => {}
        }
        let before = rocky_platform::fs::exists(&self.directory)
            .then(|| rocky_platform::fs::read_dir(&self.directory).unwrap_or_default());
        let outcome = Arc::new(std::sync::Mutex::new(None));
        let sign_in = crate::personal::SignIn {
            account: std::sync::Mutex::new(Some(account)),
            source: std::sync::Mutex::new(Some(source)),
            wait: self.sign_in_wait,
            outcome: outcome.clone(),
        };
        let opened = match Database::launch_sign_in(self.options(), sign_in).await {
            Ok(opened) => opened,
            Err(error) => {
                crate::personal::tidy(&self.directory, before.as_deref());
                return Err(Refused::from_failure(error));
            }
        };
        let (restored, freshness) = outcome
            .lock()
            .map_err(|_| Refused::Failed("outcome poisoned".into()))?
            .take()
            .ok_or_else(|| Refused::Failed("the sign-in reported no outcome".into()))?;
        Ok(crate::PersonalOpened {
            opened,
            restored,
            freshness,
        })
    }
    /// The root recovers the domain from `archive`, a backup of one of its
    /// trusted nodes, with every trusted node and device lost: a fresh
    /// trusted node holding every key the archived node held, opened from
    /// its root escrow. The root then revokes every other trusted identity
    /// the archive knows, with the identities each first enrolled, and every
    /// key rotates; `Opened::revoked` names them. In a browser `archive`
    /// names a file in Rocky's storage (`rocky_platform::fs`); see
    /// [`Self::recover_archive`] for an archive's bytes. A recovery
    /// interrupted at any step is finished by calling this again with the
    /// same archive and root; opening the directory meanwhile is refused.
    pub async fn recover(
        &self,
        archive: &std::path::Path,
        root: &crate::RootKey,
    ) -> Result<Opened, Failure> {
        Database::launch_recover(self.options(), archive, root).await
    }
    /// As [`Self::recover`], from the archive's bytes, such as a browser
    /// upload of [`Database::export_backup`]. They are staged in the
    /// directory and removed once the recovery finishes or stops; an
    /// interrupted one is finished by calling this again with the same
    /// bytes.
    pub async fn recover_archive(
        &self,
        archive: &[u8],
        root: &crate::RootKey,
    ) -> Result<Opened, Failure> {
        self.staged(archive, "recovery", |staged| async move {
            self.recover(&staged, root).await
        })
        .await
    }
    /// Restores `archive`, a backup of a member or device, into an empty
    /// directory under a fresh identity that `invitation` enrolls over the
    /// configured network. The archive's operations keep their identities;
    /// nothing is sent again as new work. In a browser `archive` names a
    /// file in Rocky's storage; see [`Self::restore_archive`] for bytes. An
    /// interrupted restore continues when called again with the same
    /// archive, under the identity it enrolled.
    pub async fn restore(
        &self,
        archive: &std::path::Path,
        invitation: &str,
    ) -> Result<Opened, Failure> {
        Database::launch_restore(self.options(), archive, invitation).await
    }
    /// As [`Self::restore`], from the archive's bytes, staged and removed
    /// as for [`Self::recover_archive`].
    pub async fn restore_archive(
        &self,
        archive: &[u8],
        invitation: &str,
    ) -> Result<Opened, Failure> {
        self.staged(archive, "restore", |staged| async move {
            self.restore(&staged, invitation).await
        })
        .await
    }
    /// Runs `install` on `archive` staged in the directory as
    /// `{install}.backup`, which is removed whether it finishes or stops. An
    /// interrupted personal create or sign-in has no database yet.
    async fn staged<F, Installing>(
        &self,
        archive: &[u8],
        install: &str,
        run: F,
    ) -> Result<Opened, Failure>
    where
        F: FnOnce(PathBuf) -> Installing,
        Installing: std::future::Future<Output = Result<Opened, Failure>>,
    {
        rocky_platform::fs::create_dir_all(&self.directory)?;
        let staged = self.directory.join(format!("{install}.backup"));
        rocky_platform::fs::write(&staged, archive)?;
        let installed = run(staged.clone()).await;
        if let Err(error) = rocky_platform::fs::remove_file(&staged) {
            if let Ok(opened) = &installed {
                opened.database.close().await?;
            }
            return Err(format!(
                "could not remove the staged archive {}: {error}{}",
                staged.display(),
                if installed.is_ok() {
                    format!("; the {install} finished: open the directory")
                } else {
                    String::new()
                }
            )
            .into());
        }
        installed
    }
    /// Opens an existing installation. Startup runs off the caller's async executor.
    pub async fn open(&self) -> Result<Opened, Failure> {
        let interrupted_personal_install = crate::personal::unfinished(&self.directory);
        if interrupted_personal_install {
            return Err(
                "a personal install is unfinished in this directory: call the same \
                 create_personal or sign_in again"
                    .into(),
            );
        }
        Database::launch_open(self.options()).await
    }
    /// Resumes an approval join, retaining its single-use key only in this continuation.
    /// After a process restart a supplied identity provider is required to resume.
    pub async fn resume_join(
        &self,
        invitation: &str,
        pending: &crate::AwaitingAdmission,
    ) -> Result<Opened, Failure> {
        pending.check_resume(&self.directory, invitation)?;
        let mut options = self.options();
        options.identity = Some(pending.identity.clone());
        let mut opened = match Database::launch_join(options, invitation, &self.name).await {
            Ok(opened) => opened,
            Err(mut error) => {
                if let Some(awaiting) = error.downcast_mut::<crate::AwaitingAdmission>() {
                    awaiting.phrase = pending.phrase.clone();
                }
                return Err(error);
            }
        };
        opened.identity = pending.phrase.clone();
        Ok(opened)
    }
    /// Starts linking and returns `AwaitingLink` with the code to compare on both devices.
    pub async fn link(
        &self,
        contact: &str,
        identity: Arc<dyn IdentityProvider>,
    ) -> Result<Opened, Failure> {
        let mut options = self.options();
        options.identity = Some(identity);
        Database::launch_link(options, contact, None).await
    }
    /// Completes the same request, using the identity only for this call.
    pub async fn confirm_link(
        &self,
        contact: &str,
        identity: Arc<dyn IdentityProvider>,
        pending: Arc<crate::AwaitingLink>,
        code: &str,
    ) -> Result<Opened, Failure> {
        if pending.code != code {
            return Err("link code does not match".into());
        }
        let mut options = self.options();
        options.identity = Some(identity);
        Database::launch_link(options, contact, Some(pending)).await
    }
    /// Creates a linkable identity for this join and returns its phrase once.
    pub async fn join_linkable(&self, invitation: &str) -> Result<Opened, Failure> {
        let identity = crate::IdentityKey::generate();
        let phrase = identity.phrase();
        let mut options = self.options();
        options.identity = Some(Arc::new(identity));
        let mut opened = match Database::launch_join(options, invitation, &self.name).await {
            Ok(opened) => opened,
            Err(mut error) => {
                if let Some(pending) = error.downcast_mut::<crate::AwaitingAdmission>() {
                    pending.phrase = Some(phrase);
                }
                return Err(error);
            }
        };
        opened.identity = Some(phrase);
        Ok(opened)
    }
    /// Enrolls using the configured network. Enrollment runs off the caller's async executor.
    /// Through an approval invitation this fails with
    /// [`crate::AwaitingAdmission`] until an issuer admits the device:
    /// resume with [`Self::resume_join`], keeping the continuation in memory.
    pub async fn join(&self, invitation: &str) -> Result<Opened, Failure> {
        Database::launch_join(self.options(), invitation, &self.name).await
    }
}
