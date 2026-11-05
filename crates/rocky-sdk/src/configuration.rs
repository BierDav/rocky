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
    /// `{install}.backup`, which is removed whether it finishes or stops.
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
        Database::launch_open(self.options()).await
    }
    /// Enrolls using the configured network. Enrollment runs off the caller's async executor.
    pub async fn join(&self, invitation: &str) -> Result<Opened, Failure> {
        Database::launch_join(self.options(), invitation).await
    }
}
