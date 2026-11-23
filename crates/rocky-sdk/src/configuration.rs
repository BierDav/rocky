use crate::{Database, Failure, IdentityProvider, Network, Open, Opened, Schema, SecretStore};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct Configuration {
    directory: PathBuf,
    schema: Schema,
    secrets: Option<Arc<dyn SecretStore>>,
    identity: Option<Arc<dyn IdentityProvider>>,
    network: Option<Network>,
    coordination: Option<crate::CoordinatorSlot>,
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
        Database::launch_create(self.options(), None, root, Self::BOOKMARK).await
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
        Database::launch_open(self.options()).await
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
    pub async fn accept_invitation(&self, invitation: &str) -> Result<Opened, Failure> {
        Database::launch_invited(self.options(), invitation).await
    }
    pub async fn join_linkable(&self, provisioning: &str) -> Result<Opened, Failure> {
        Database::launch_join(self.options(), provisioning, false).await
    }
    pub async fn join(&self, provisioning: &str) -> Result<Opened, Failure> {
        Database::launch_join(self.options(), provisioning, true).await
    }
}
