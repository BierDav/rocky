//! Transport fixtures retain keys across reopen without testing file-key fsync.
use rocky_sdk::{SecretStore, credentials::ProviderError};
use std::{collections::HashMap, sync::Mutex};

#[derive(Default)]
pub struct Keys(Mutex<HashMap<String, Vec<u8>>>);
impl SecretStore for Keys {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, ProviderError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "poisoned keys")?
            .get(name)
            .cloned())
    }
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), ProviderError> {
        self.0
            .lock()
            .map_err(|_| "poisoned keys")?
            .insert(name.into(), secret.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), ProviderError> {
        self.0.lock().map_err(|_| "poisoned keys")?.remove(name);
        Ok(())
    }
}
