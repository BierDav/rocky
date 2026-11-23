use crate::{
    Failure,
    enrollment::{AUTHORITY_IDENTITY_KEY, NewKey, Renewed, Settings, now},
    secrets::Vault,
};
use rocky_core::TypedRows;
use rocky_credentials::{RevocationSet, SignedRevocation};
use rocky_secure::EncryptedReplica;
use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, PoisonError},
};

#[derive(Clone)]
pub struct Shared<T: Clone>(Arc<T>);
impl<T: Clone> Shared<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
}
impl<T: Clone> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: Clone> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}

pub struct SettingsCell {
    current: tokio::sync::Mutex<Shared<Settings>>,
    published: std::sync::RwLock<Shared<Settings>>,
}
impl SettingsCell {
    pub fn new(settings: Settings) -> Self {
        let settings = Shared::new(settings);
        Self {
            published: std::sync::RwLock::new(settings.clone()),
            current: tokio::sync::Mutex::new(settings),
        }
    }
    pub async fn lock(&self) -> SettingsGuard<'_> {
        SettingsGuard {
            held: self.current.lock().await,
            published: &self.published,
        }
    }
    #[cfg(test)]
    pub fn try_lock(&self) -> Result<SettingsGuard<'_>, tokio::sync::TryLockError> {
        Ok(SettingsGuard {
            held: self.current.try_lock()?,
            published: &self.published,
        })
    }
    pub fn published(&self) -> Shared<Settings> {
        self.published
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

pub struct SettingsGuard<'a> {
    held: tokio::sync::MutexGuard<'a, Shared<Settings>>,
    published: &'a std::sync::RwLock<Shared<Settings>>,
}
impl SettingsGuard<'_> {
    fn publish(&self) {
        *self
            .published
            .write()
            .unwrap_or_else(PoisonError::into_inner) = self.held.clone();
    }
    pub async fn save(&mut self, replica: &EncryptedReplica<TypedRows>) -> Result<(), Failure> {
        let bytes = self.held.to_bytes()?;
        replica
            .put_authority_state(AUTHORITY_IDENTITY_KEY, &bytes, now()?)
            .await?;
        self.publish();
        Ok(())
    }
    pub async fn commit(
        &mut self,
        draft: Shared<Settings>,
        replica: &EncryptedReplica<TypedRows>,
    ) -> Result<(), Failure> {
        let bytes = draft.to_bytes()?;
        replica
            .put_authority_state(AUTHORITY_IDENTITY_KEY, &bytes, now()?)
            .await?;
        *self.held = draft;
        self.publish();
        Ok(())
    }
    pub async fn commit_with_revocations(
        &mut self,
        draft: Shared<Settings>,
        replica: &EncryptedReplica<TypedRows>,
        revocations: &RevocationSet,
    ) -> Result<Vec<SignedRevocation>, Failure> {
        let bytes = draft.to_bytes()?;
        let added = replica
            .commit_authority_state(
                &[(AUTHORITY_IDENTITY_KEY, Some(bytes.as_slice()))],
                now()?,
                Some(revocations),
            )
            .await?;
        *self.held = draft;
        self.publish();
        Ok(added)
    }
    pub async fn commit_with_entries(
        &mut self,
        draft: Shared<Settings>,
        replica: &EncryptedReplica<TypedRows>,
        extra: &[(&str, Option<&[u8]>)],
        revocations: Option<&RevocationSet>,
    ) -> Result<Vec<SignedRevocation>, Failure> {
        let bytes = draft.to_bytes()?;
        let mut entries = Vec::with_capacity(extra.len() + 1);
        entries.push((AUTHORITY_IDENTITY_KEY, Some(bytes.as_slice())));
        entries.extend_from_slice(extra);
        let added = replica
            .commit_authority_state(&entries, now()?, revocations)
            .await?;
        *self.held = draft;
        self.publish();
        Ok(added)
    }
    pub async fn adopt(
        &mut self,
        replica: &EncryptedReplica<TypedRows>,
        vault: &Vault,
        renewed: &Renewed,
    ) -> Result<(Vec<NewKey>, bool), Failure> {
        let (draft, held, changed) = self.held.adopt_into(vault, renewed)?;
        if changed {
            self.commit(Shared::new(draft), replica).await?;
        } else {
            *self.held = Shared::new(draft);
        }
        Ok((held, changed))
    }
}
impl Deref for SettingsGuard<'_> {
    type Target = Shared<Settings>;
    fn deref(&self) -> &Shared<Settings> {
        &self.held
    }
}
impl DerefMut for SettingsGuard<'_> {
    fn deref_mut(&mut self) -> &mut Shared<Settings> {
        &mut self.held
    }
}
