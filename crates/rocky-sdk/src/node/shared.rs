use crate::{
    Failure,
    enrollment::{NewKey, Renewed, Settings},
    join_requests::Note,
    registry::Registry,
    secrets::Vault,
    transport::Transport,
};
use rocky_credentials::Profile;
use std::{
    ops::{Deref, DerefMut},
    path::Path,
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
    pub fn save(&mut self, path: &Path) -> Result<(), Failure> {
        self.held.save(path)?;
        self.publish();
        Ok(())
    }
    pub fn commit(&mut self, draft: Shared<Settings>, path: &Path) -> Result<(), Failure> {
        draft.save(path)?;
        *self.held = draft;
        self.publish();
        Ok(())
    }
    pub fn adopt(
        &mut self,
        path: &Path,
        vault: &Vault,
        renewed: &Renewed,
    ) -> Result<(Vec<NewKey>, bool), Failure> {
        let adopted = self.held.adopt(path, vault, renewed)?;
        if adopted.1 {
            self.publish();
        }
        Ok(adopted)
    }
    pub async fn invite(
        &mut self,
        path: &Path,
        vault: &Vault,
        transport: &dyn Transport,
        offer: (Option<Profile>, Option<Vec<u8>>),
        registry: &Registry,
    ) -> Result<(String, u64, Vec<Note>), Failure> {
        let invited = self
            .held
            .invite(path, vault, transport, offer, registry)
            .await?;
        self.publish();
        Ok(invited)
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
