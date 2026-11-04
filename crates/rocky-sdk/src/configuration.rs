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
    /// caller's async executor: natively on the database's own thread.
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
        let opened = Database::launch_create(self.options(), None, root).await?;
        let session = opened.database.session();
        let followed = async {
            session.create_bookmark(Self::BOOKMARK, None).await?;
            session.follow(Self::BOOKMARK).await
        }
        .await;
        if let Err(error) = followed {
            // Never hand out a trusted node that silently never seals.
            opened.database.close().await?;
            return Err(format!(
                "created, but could not follow the `{}` bookmark: {error}; open the \
                 installation and create and follow a bookmark",
                Self::BOOKMARK
            )
            .into());
        }
        Ok(opened)
    }
    /// Opens an existing installation. Startup runs off the caller's async executor.
    pub async fn open(&self) -> Result<Opened, Failure> {
        Database::launch_open(self.options()).await
    }
    /// Enrolls using the configured network. Enrollment runs off the caller's async executor.
    pub async fn join(&self, invitation: &str) -> Result<Opened, Failure> {
        Database::launch_join(self.options(), invitation).await
    }
}
