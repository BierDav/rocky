use crate::{Database, Failure, IdentityProvider, Network, Open, Opened, Schema, SecretStore};
use std::{path::PathBuf, sync::Arc};

const DEFAULT_SIGN_IN_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Clone)]
pub struct Configuration {
    directory: PathBuf,
    schema: Schema,
    secrets: Option<Arc<dyn SecretStore>>,
    identity: Option<Arc<dyn IdentityProvider>>,
    network: Option<Network>,
    coordination: Option<crate::CoordinatorSlot>,
    name: String,
    sign_in_wait: std::time::Duration,
}
impl Configuration {
    pub const BOOKMARK: &'static str = "main";
    pub fn new(directory: impl Into<PathBuf>, schema: &Schema) -> Result<Self, Failure> {
        let schema = schema.validated()?;
        Ok(Self {
            directory: directory.into(),
            schema,
            secrets: None,
            identity: None,
            network: None,
            coordination: None,
            name: String::new(),
            sign_in_wait: DEFAULT_SIGN_IN_WAIT,
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
    #[must_use]
    pub fn network(mut self, network: Network) -> Self {
        self.network = Some(network);
        self
    }
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
    #[must_use]
    pub fn sign_in_wait(mut self, wait: std::time::Duration) -> Self {
        self.sign_in_wait = wait;
        self
    }
    #[must_use]
    pub fn coordination(mut self, coordination: Arc<dyn rocky_credentials::Coordinator>) -> Self {
        self.coordination = Some(crate::CoordinatorSlot::Client(coordination));
        self
    }
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
    pub async fn create(&self) -> Result<Opened, Failure> {
        let root = crate::RootKey::generate();
        let created = crate::RootKey::from_secret(root.secret());
        let mut opened = self.create_with_root(Arc::new(root)).await?;
        opened.root = Some(created);
        Ok(opened)
    }
    pub async fn create_with_root(
        &self,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<Opened, Failure> {
        Database::launch_create(self.options(), None, root, (Self::BOOKMARK, None)).await
    }
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
                    scope: locator.scope(&self.schema.application),
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
    pub async fn recover(
        &self,
        archive: &std::path::Path,
        root: &crate::RootKey,
    ) -> Result<Opened, Failure> {
        Database::launch_recover(self.options(), archive, root).await
    }
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
    pub async fn restore(
        &self,
        archive: &std::path::Path,
        invitation: &str,
    ) -> Result<Opened, Failure> {
        Database::launch_restore(self.options(), archive, invitation).await
    }
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
    pub async fn link(
        &self,
        contact: &str,
        identity: Arc<dyn IdentityProvider>,
    ) -> Result<Opened, Failure> {
        let mut options = self.options();
        options.identity = Some(identity);
        Database::launch_link(options, contact, None).await
    }
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
    pub async fn join(&self, invitation: &str) -> Result<Opened, Failure> {
        Database::launch_join(self.options(), invitation, &self.name).await
    }
}
