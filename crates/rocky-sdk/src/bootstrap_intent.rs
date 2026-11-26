use crate::{
    Failure,
    enrollment::{AUTHORITY_BOOTSTRAP_KEY, REPLICA_FILE, now},
    sync::Planned,
};
use rocky_core::{ResourceScope, TypedRows};
use rocky_credentials::{PublicKey, RevocationSet};
use rocky_secure::EncryptedReplica;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
use tokio::sync::{Mutex, MutexGuard};

pub(crate) const BOOTSTRAP_INTENT_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Imports {
    Granted,
    Scopes(Vec<ResourceScope>),
}
impl Imports {
    pub(crate) fn includes(&self, scope: &ResourceScope) -> bool {
        match self {
            Self::Granted => true,
            Self::Scopes(scopes) => scopes.contains(scope),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootstrapIntent {
    version: u32,
    imports: Imports,
    totals: Vec<Planned>,
    inviter: Option<PublicKey>,
}
impl BootstrapIntent {
    fn empty() -> Self {
        Self {
            version: BOOTSTRAP_INTENT_VERSION,
            imports: Imports::Scopes(Vec::new()),
            totals: Vec::new(),
            inviter: None,
        }
    }
    pub(crate) fn imports(&self) -> &Imports {
        &self.imports
    }
    pub(crate) fn totals(&self) -> &[Planned] {
        &self.totals
    }
    pub(crate) fn inviter(&self) -> Option<PublicKey> {
        self.inviter
    }
    pub(crate) fn awaited_inviter(&self, revocations: &RevocationSet) -> Option<PublicKey> {
        self.inviter.filter(|inviter| !revocations.revokes(inviter))
    }
    fn regranted(self) -> Self {
        Self {
            imports: Imports::Granted,
            totals: Vec::new(),
            ..self
        }
    }
    fn invited_by(self, inviter: PublicKey) -> Self {
        Self {
            inviter: Some(inviter),
            ..self.regranted()
        }
    }
    fn adding(mut self, added: &[ResourceScope]) -> Self {
        if let Imports::Scopes(scopes) = &mut self.imports {
            for scope in added {
                if !scopes.contains(scope) {
                    scopes.push(*scope);
                }
            }
        }
        self
    }
    fn progressed(&self, pending: &[ResourceScope], totals: &[Planned]) -> Self {
        Self {
            imports: Imports::Scopes(pending.to_vec()),
            totals: totals.to_vec(),
            ..self.clone()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finished {
    Cleared,
    AwaitingInviter,
}

pub(crate) struct BootstrapIntentFile {
    directory: PathBuf,
    database: PathBuf,
    replica: OnceLock<EncryptedReplica<TypedRows>>,
    stored: Mutex<()>,
}
impl BootstrapIntentFile {
    pub(crate) fn in_directory(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
            database: directory.join(REPLICA_FILE),
            replica: OnceLock::new(),
            stored: Mutex::new(()),
        }
    }
    pub(crate) fn attach_replica(&self, replica: EncryptedReplica<TypedRows>) {
        let _ = self.replica.set(replica);
    }
    async fn read_raw(&self) -> Result<Option<(Vec<u8>, u64)>, Failure> {
        Ok(match self.replica.get() {
            Some(replica) => replica.get_authority_state(AUTHORITY_BOOTSTRAP_KEY).await?,
            None => rocky_secure::read_authority_entry(&self.database, AUTHORITY_BOOTSTRAP_KEY)?,
        })
    }
    async fn load_unlocked(&self) -> Result<Option<BootstrapIntent>, Failure> {
        let Some((bytes, _)) = self.read_raw().await? else {
            return Ok(None);
        };
        let intent: BootstrapIntent = serde_json::from_slice(&bytes)
            .map_err(|_| "unsupported bootstrap intent format; reinstall the domain")?;
        if intent.version != BOOTSTRAP_INTENT_VERSION {
            return Err("unsupported bootstrap intent version; reinstall the domain".into());
        }
        Ok(Some(intent))
    }
    async fn save_unlocked(&self, intent: &BootstrapIntent) -> Result<(), Failure> {
        let bytes = serde_json::to_vec(intent)?;
        let updated_at = now()?;
        match self.replica.get() {
            Some(replica) => {
                replica
                    .put_authority_state(AUTHORITY_BOOTSTRAP_KEY, &bytes, updated_at)
                    .await?;
            }
            None => {
                #[cfg(not(target_family = "wasm"))]
                rocky_secure::write_authority_entry(
                    &self.database,
                    AUTHORITY_BOOTSTRAP_KEY,
                    &bytes,
                    updated_at,
                )
                .await?;
                #[cfg(target_family = "wasm")]
                {
                    let database = self.database.clone();
                    rocky_platform::task::spawn_local(async move {
                        rocky_secure::write_authority_entry(
                            &database,
                            AUTHORITY_BOOTSTRAP_KEY,
                            &bytes,
                            updated_at,
                        )
                        .await
                    })
                    .await??;
                }
            }
        }
        Ok(())
    }
    async fn remove_unlocked(&self) -> Result<(), Failure> {
        match self.replica.get() {
            Some(replica) => {
                replica
                    .delete_authority_state(AUTHORITY_BOOTSTRAP_KEY)
                    .await?;
            }
            None => {
                #[cfg(not(target_family = "wasm"))]
                rocky_secure::remove_authority_entry(&self.database, AUTHORITY_BOOTSTRAP_KEY)
                    .await?;
                #[cfg(target_family = "wasm")]
                {
                    let database = self.database.clone();
                    rocky_platform::task::spawn_local(async move {
                        rocky_secure::remove_authority_entry(&database, AUTHORITY_BOOTSTRAP_KEY)
                            .await
                    })
                    .await??;
                }
            }
        }
        Ok(())
    }
    pub(crate) async fn exists(&self) -> bool {
        let _guard = self.locked().await;
        self.read_raw().await.ok().flatten().is_some()
    }
    pub(crate) async fn recorded_at_epoch_seconds(&self) -> Option<u64> {
        let _guard = self.locked().await;
        self.read_raw()
            .await
            .ok()
            .flatten()
            .map(|(_, updated_at)| updated_at)
    }
    pub(crate) async fn load(&self) -> Result<Option<BootstrapIntent>, Failure> {
        let _guard = self.locked().await;
        self.load_unlocked().await
    }
    pub(crate) async fn request_checkpoint_import(&self) -> Result<(), Failure> {
        self.update(BootstrapIntent::regranted).await
    }
    pub(crate) async fn join(&self, inviter: PublicKey) -> Result<(), Failure> {
        self.update(|intent| intent.invited_by(inviter)).await
    }
    pub(crate) async fn add_scopes(&self, added: &[ResourceScope]) -> Result<(), Failure> {
        self.update(|intent| intent.adding(added)).await
    }
    pub(crate) async fn record_progress(
        &self,
        expected: &BootstrapIntent,
        pending: &[ResourceScope],
        totals: &[Planned],
    ) -> Result<BootstrapIntent, Failure> {
        let _guard = self.locked().await;
        let current = self.load_unlocked().await?;
        crate::pause::at(&self.directory, crate::pause::Point::BootstrapIntentLoaded).await;
        unchanged_since(current.as_ref(), expected)?;
        let next = expected.progressed(pending, totals);
        self.save_unlocked(&next).await?;
        Ok(next)
    }
    pub(crate) async fn finish(
        &self,
        completed: &BootstrapIntent,
        peer: PublicKey,
        revocations: &RevocationSet,
    ) -> Result<Finished, Failure> {
        let _guard = self.locked().await;
        let current = self.load_unlocked().await?;
        crate::pause::at(&self.directory, crate::pause::Point::BootstrapIntentLoaded).await;
        unchanged_since(current.as_ref(), completed)?;
        if completed
            .awaited_inviter(revocations)
            .is_some_and(|inviter| inviter != peer)
        {
            return Ok(Finished::AwaitingInviter);
        }
        self.remove_unlocked().await?;
        Ok(Finished::Cleared)
    }
    async fn update(
        &self,
        change: impl FnOnce(BootstrapIntent) -> BootstrapIntent,
    ) -> Result<(), Failure> {
        let _guard = self.locked().await;
        let current = self.load_unlocked().await?;
        crate::pause::at(&self.directory, crate::pause::Point::BootstrapIntentLoaded).await;
        let next = change(current.clone().unwrap_or_else(BootstrapIntent::empty));
        if current.as_ref() != Some(&next) {
            self.save_unlocked(&next).await?;
        }
        Ok(())
    }
    async fn locked(&self) -> MutexGuard<'_, ()> {
        match self.stored.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                crate::pause::passing(
                    &self.directory,
                    crate::pause::Point::BootstrapIntentContended,
                );
                self.stored.lock().await
            }
        }
    }
}

fn unchanged_since(
    current: Option<&BootstrapIntent>,
    expected: &BootstrapIntent,
) -> Result<(), Failure> {
    if current != Some(expected) {
        return Err("bootstrap authority changed; retry with the new intent".into());
    }
    Ok(())
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests;
