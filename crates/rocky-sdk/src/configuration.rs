use crate::{
    Created, Database, IdentityProvider, Network, Schema, SecretStore, SyncMode, database::Install,
};
use std::{path::PathBuf, sync::Arc};

const ADMIN_STALE_AFTER_RANGE: std::ops::RangeInclusive<std::time::Duration> =
    std::time::Duration::from_secs(60)..=std::time::Duration::from_secs(10 * 366 * 24 * 60 * 60);

/// Where a database lives, its schema, keys and network; `create`, `open`, `join` or `link` starts
/// it.
#[derive(Clone)]
pub struct Configuration {
    pub(crate) directory: PathBuf,
    pub(crate) schema: Option<Schema>,
    pub(crate) secrets: Option<Arc<dyn SecretStore>>,
    pub(crate) identity: Option<Arc<dyn IdentityProvider>>,
    pub(crate) network: Option<Network>,
    pub(crate) sync: SyncMode,
    pub(crate) admin_stale_after: std::time::Duration,
}
impl Configuration {
    /// A database in `directory` with the application's schema, syncing `Live` and with no network
    /// until one is set.
    pub fn new(directory: impl Into<PathBuf>, schema: &Schema) -> Result<Self, crate::Error> {
        let schema = schema.validated()?;
        Ok(Self {
            schema: Some(schema),
            ..Self::store_only(directory)
        })
    }
    /// A relay database without the application's schema, which holds and forwards encrypted frames
    /// but never reads them.
    pub fn store_only(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            schema: None,
            secrets: None,
            identity: None,
            network: None,
            sync: SyncMode::Live,
            admin_stale_after: crate::status::ADMIN_STALE_AFTER,
        }
    }
    /// Keeps this device's keys in `store` instead of files under `directory/secrets`.
    #[must_use]
    pub fn secrets(mut self, store: Arc<dyn SecretStore>) -> Self {
        self.secrets = Some(store);
        self
    }
    /// The person key this session signs with; Rocky never stores it, so the application supplies
    /// it on every open.
    #[must_use]
    pub fn identity(mut self, provider: Arc<dyn IdentityProvider>) -> Self {
        self.identity = Some(provider);
        self
    }
    /// The network the database attaches when it starts.
    #[must_use]
    pub fn network(mut self, network: Network) -> Self {
        self.network = Some(network);
        self
    }
    /// The sync mode the database starts in; `Live` by default.
    #[must_use]
    pub fn sync(mut self, mode: SyncMode) -> Self {
        self.sync = mode;
        self
    }
    /// How long an admin device waits to hear from another admin before raising `AdminStale`;
    /// 30 days by default, and a minute to ten years.
    pub fn admin_stale_after(mut self, after: std::time::Duration) -> Result<Self, crate::Error> {
        if !ADMIN_STALE_AFTER_RANGE.contains(&after) {
            return Err(crate::Error::new(
                crate::ErrorKind::Rejected,
                "admin_stale_after must be a minute to ten years",
            ));
        }
        self.admin_stale_after = after;
        Ok(self)
    }
    /// Creates a new database owned by a new root key and returns both keys once; Rocky never
    /// stores them.
    pub async fn create(&self) -> Result<Created, crate::Error> {
        let root = crate::RootKey::generate();
        let owner = root.identity();
        let database = self
            .create_with_root(
                Arc::new(rocky_credentials::key_bytes::root_key(
                    rocky_credentials::key_bytes::root_secret(&root),
                )),
                Arc::new(rocky_credentials::key_bytes::identity_key(
                    rocky_credentials::key_bytes::identity_secret(&owner),
                )),
            )
            .await?;
        Ok(Created {
            database,
            root,
            owner,
        })
    }
    /// Creates a new database with a root signer and owner person signer the application supplies,
    /// such as passkey-backed ones.
    pub async fn create_with_root(
        &self,
        root: Arc<dyn crate::IdentityProvider>,
        owner: Arc<dyn crate::IdentityProvider>,
    ) -> Result<Database, crate::Error> {
        Database::launch(self, Install::create(root, owner), None).await
    }
    /// Opens an existing database; a schema that drops tables or fields or narrows a role is
    /// refused with `RolloutConfirmation` on an admin device.
    pub async fn open(&self) -> Result<Database, crate::Error> {
        Database::launch(self, Install::Installed, None).await
    }
    /// Opens and publishes a schema that removes data, given the `RolloutImpact::digest` its
    /// refusal carried.
    pub async fn open_confirmed(&self, removals: &str) -> Result<Database, crate::Error> {
        Database::launch(self, Install::Installed, Some(removals.into())).await
    }
    /// Starts linking this device to a person who holds another device, reached at `contact`; needs
    /// that person's key in `identity`.
    pub async fn link(&self, contact: &str) -> Result<crate::PendingLink, crate::Error> {
        self.require_identity()?;
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
                Err(error) => Err(crate::Error::failed(error)),
            },
        }
    }
    /// Redeems an invitation as the person in `identity` and returns after the first pull from the
    /// issuing device, so the first read sees the issuer's rows.
    pub async fn join(&self, code: &crate::InvitationCode) -> Result<Database, crate::Error> {
        self.require_identity()?;
        let install = Install::Invited { code: code.clone() };
        let database = Database::launch(self, install, None).await?;
        database.send(crate::request::Request::FirstPull).await?;
        Ok(database)
    }
    fn require_identity(&self) -> Result<(), crate::Error> {
        match self.identity {
            Some(_) => Ok(()),
            None => Err(crate::Error::refused(
                crate::Right::Sign,
                crate::DenyReason::IdentityRequired,
            )),
        }
    }
}
