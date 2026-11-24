use crate::{
    Created, Database, IdentityProvider, Network, Opened, Schema, SecretStore, database::Install,
};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct Configuration {
    pub(crate) directory: PathBuf,
    pub(crate) schema: Option<Schema>,
    pub(crate) secrets: Option<Arc<dyn SecretStore>>,
    pub(crate) identity: Option<Arc<dyn IdentityProvider>>,
    pub(crate) network: Option<Network>,
    pub(crate) page_cache: rocky_secure::PageCache,
}
impl Configuration {
    pub fn new(directory: impl Into<PathBuf>, schema: &Schema) -> Result<Self, crate::Error> {
        let schema = schema.validated()?;
        Ok(Self {
            schema: Some(schema),
            ..Self::store_only(directory)
        })
    }
    pub fn store_only(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            schema: None,
            secrets: None,
            identity: None,
            network: None,
            page_cache: rocky_secure::PageCache::default(),
        }
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
    pub fn page_cache(mut self, page_cache: rocky_secure::PageCache) -> Self {
        self.page_cache = page_cache;
        self
    }
    pub async fn create(&self) -> Result<Created, crate::Error> {
        let root = crate::RootKey::generate();
        let created = crate::RootKey::from_secret(root.secret());
        let database = self.create_with_root(Arc::new(root)).await?;
        Ok(Created {
            database,
            root: created,
        })
    }
    pub async fn create_with_root(
        &self,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<Database, crate::Error> {
        Database::launch(self, Install::create(root), None).await
    }
    pub async fn open(&self) -> Result<Database, crate::Error> {
        Database::launch(self, Install::Installed, None).await
    }
    pub async fn open_confirmed(&self, removals: &str) -> Result<Database, crate::Error> {
        Database::launch(self, Install::Installed, Some(removals.into())).await
    }
    pub async fn link(&self, contact: &str) -> Result<crate::PendingLink, crate::Error> {
        if self.identity.is_none() {
            return Err(crate::LinkError::IdentityRequired.into());
        }
        let install = Install::Identity {
            contact: contact.into(),
            confirmed: None,
        };
        match Database::launch_raw(self, install).await {
            Ok(database) => {
                database.close().await?;
                Err(crate::Error::new(
                    crate::ErrorKind::Storage,
                    "the other device enrolled this one without asking for a link code",
                ))
            }
            Err(error) => match error.downcast::<crate::self_enrollment::AwaitingLink>() {
                Ok(awaiting) => Ok(crate::PendingLink {
                    configuration: self.clone(),
                    contact: contact.into(),
                    awaiting: Arc::new(*awaiting),
                }),
                Err(error) => Err(error.into()),
            },
        }
    }
    pub async fn join(&self, code: &crate::InvitationCode) -> Result<Opened, crate::Error> {
        let (install, minted) = Install::invited(code);
        let database = Database::launch(self, install, None).await?;
        let identity = minted.lock().ok().and_then(|mut minted| minted.take());
        Ok(Opened { database, identity })
    }
}
