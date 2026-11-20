use crate::{Failure, sync::Planned};
use rocky_core::ResourceScope;
use rocky_credentials::{PublicKey, RevocationSet};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError, TryLockError},
};

pub(crate) const BOOTSTRAP_INTENT_FILE: &str = "bootstrap.json";
const BOOTSTRAP_INTENT_VERSION: u32 = 1;

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

struct Stored {
    path: PathBuf,
}
impl Stored {
    fn load(&self) -> Result<Option<BootstrapIntent>, Failure> {
        let bytes = match rocky_platform::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let intent: BootstrapIntent = serde_json::from_slice(&bytes)
            .map_err(|_| "unsupported bootstrap intent format; reinstall the domain")?;
        if intent.version != BOOTSTRAP_INTENT_VERSION {
            return Err("unsupported bootstrap intent version; reinstall the domain".into());
        }
        Ok(Some(intent))
    }
    fn save(&self, intent: &BootstrapIntent) -> Result<(), Failure> {
        rocky_credentials::write_private(&self.path, &serde_json::to_vec(intent)?)?;
        Ok(())
    }
    fn remove(&self) -> Result<(), Failure> {
        rocky_platform::fs::remove_file(&self.path)?;
        #[cfg(unix)]
        std::fs::File::open(
            self.path
                .parent()
                .ok_or("bootstrap intent has no directory")?,
        )?
        .sync_all()?;
        Ok(())
    }
}

pub(crate) struct BootstrapIntentFile {
    directory: PathBuf,
    stored: Mutex<Stored>,
}
impl BootstrapIntentFile {
    pub(crate) fn in_directory(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
            stored: Mutex::new(Stored {
                path: directory.join(BOOTSTRAP_INTENT_FILE),
            }),
        }
    }
    pub(crate) fn exists(&self) -> bool {
        rocky_platform::fs::exists(&self.locked().path)
    }
    pub(crate) fn recorded_at_epoch_seconds(&self) -> Option<u64> {
        rocky_platform::fs::modified(&self.locked().path)
            .ok()
            .and_then(|time| time.duration_since(web_time::UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_secs())
    }
    pub(crate) fn load(&self) -> Result<Option<BootstrapIntent>, Failure> {
        self.locked().load()
    }
    pub(crate) fn request_checkpoint_import(&self) -> Result<(), Failure> {
        self.update(BootstrapIntent::regranted)
    }
    pub(crate) fn join(&self, inviter: PublicKey) -> Result<(), Failure> {
        self.update(|intent| intent.invited_by(inviter))
    }
    pub(crate) fn add_scopes(&self, added: &[ResourceScope]) -> Result<(), Failure> {
        self.update(|intent| intent.adding(added))
    }
    pub(crate) fn record_progress(
        &self,
        expected: &BootstrapIntent,
        pending: &[ResourceScope],
        totals: &[Planned],
    ) -> Result<BootstrapIntent, Failure> {
        self.transition(|stored, current| {
            unchanged_since(current.as_ref(), expected)?;
            let next = expected.progressed(pending, totals);
            stored.save(&next)?;
            Ok(next)
        })
    }
    pub(crate) fn finish(
        &self,
        completed: &BootstrapIntent,
        peer: PublicKey,
        revocations: &RevocationSet,
    ) -> Result<Finished, Failure> {
        self.transition(|stored, current| {
            unchanged_since(current.as_ref(), completed)?;
            if completed
                .awaited_inviter(revocations)
                .is_some_and(|inviter| inviter != peer)
            {
                return Ok(Finished::AwaitingInviter);
            }
            stored.remove()?;
            Ok(Finished::Cleared)
        })
    }
    fn update(
        &self,
        change: impl FnOnce(BootstrapIntent) -> BootstrapIntent,
    ) -> Result<(), Failure> {
        self.transition(|stored, current| {
            let next = change(current.clone().unwrap_or_else(BootstrapIntent::empty));
            if current.as_ref() != Some(&next) {
                stored.save(&next)?;
            }
            Ok(())
        })
    }
    fn transition<T>(
        &self,
        change: impl FnOnce(&Stored, Option<BootstrapIntent>) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let stored = self.locked();
        let current = stored.load()?;
        crate::pause::blocking_at(&self.directory, crate::pause::Point::BootstrapIntentLoaded);
        change(&stored, current)
    }
    fn locked(&self) -> MutexGuard<'_, Stored> {
        match self.stored.try_lock() {
            Ok(stored) => stored,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                crate::pause::passing(
                    &self.directory,
                    crate::pause::Point::BootstrapIntentContended,
                );
                self.stored.lock().unwrap_or_else(PoisonError::into_inner)
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
