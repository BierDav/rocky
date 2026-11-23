use crate::{
    Created, Database, IdentityProvider, Joined, Network, Schema, SecretStore, database::Install,
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
    pub async fn link(
        &self,
        contact: &str,
        identity: Arc<dyn IdentityProvider>,
    ) -> Result<Database, crate::Error> {
        let install = Install::Identity {
            contact: contact.into(),
            confirmed: None,
        };
        Database::launch(&self.clone().identity(identity), install, None).await
    }
    pub async fn confirm_link(
        &self,
        contact: &str,
        identity: Arc<dyn IdentityProvider>,
        pending: Arc<crate::AwaitingLink>,
        code: &str,
    ) -> Result<Database, crate::Error> {
        if pending.code != code {
            return Err(crate::Error::new(
                crate::ErrorKind::Rejected,
                "link code does not match",
            ));
        }
        let install = Install::Identity {
            contact: contact.into(),
            confirmed: Some(pending),
        };
        Database::launch(&self.clone().identity(identity), install, None).await
    }
    pub async fn accept_invitation(&self, invitation: &str) -> Result<Database, crate::Error> {
        let install = Install::Invited {
            invitation: invitation.into(),
        };
        Database::launch(self, install, None).await
    }
    pub async fn join_linkable(&self, provisioning: &str) -> Result<Joined, crate::Error> {
        let (database, identity) = Database::launch_join(self, provisioning, false).await?;
        match identity {
            Some(identity) => Ok(Joined { database, identity }),
            None => {
                database.close().await?;
                Err(crate::Error::new(
                    crate::ErrorKind::Storage,
                    "a linkable join minted no identity phrase",
                ))
            }
        }
    }
    pub async fn join(&self, provisioning: &str) -> Result<Database, crate::Error> {
        Database::launch_join(self, provisioning, true)
            .await
            .map(|(database, _)| database)
    }
}
