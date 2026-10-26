use crate::{
    Coordination, Database, Failure, IdentityProvider, Network, Open, Opened, Schema, SecretStore,
};
use std::{path::PathBuf, sync::Arc};

/// Reusable installation settings. Networking is opt-in; lifecycle operations are explicit.
#[derive(Clone)]
pub struct Configuration {
    directory: PathBuf,
    schema: String,
    secrets: Option<Arc<dyn SecretStore>>,
    identity: Option<Arc<dyn IdentityProvider>>,
    network: Option<Network>,
    coordination: Option<Coordination>,
}
impl Configuration {
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
    #[must_use]
    pub fn coordination(mut self, coordination: Coordination) -> Self {
        self.coordination = Some(coordination);
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
    /// Creates an installation; refuses an existing one. Startup runs off the caller’s async executor.
    pub async fn create(&self) -> Result<Opened, Failure> {
        let configuration = self.clone();
        tokio::runtime::Handle::try_current()?
            .spawn_blocking(move || Database::create(configuration.options(), None))
            .await?
    }
    /// Opens an existing installation. Startup runs off the caller’s async executor.
    pub async fn open(&self) -> Result<Opened, Failure> {
        let configuration = self.clone();
        tokio::runtime::Handle::try_current()?
            .spawn_blocking(move || Database::open(configuration.options()))
            .await?
    }
    /// Enrolls using the configured network. Enrollment runs off the caller’s async executor.
    pub async fn join(&self, invitation: &str) -> Result<Opened, Failure> {
        let configuration = self.clone();
        let invitation = invitation.to_owned();
        tokio::runtime::Handle::try_current()?
            .spawn_blocking(move || Database::join(configuration.options(), &invitation))
            .await?
    }
}
