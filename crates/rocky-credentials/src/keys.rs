use crate::Error;
use rocky_platform::fs;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub type ProviderError = Box<dyn std::error::Error + Send + Sync>;

pub trait KeyProvider: Send + Sync {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, ProviderError>;
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), ProviderError>;
    fn delete(&self, name: &str) -> Result<(), ProviderError>;
}

pub struct FileKeyProvider {
    directory: PathBuf,
}
impl FileKeyProvider {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
    fn path(&self, name: &str) -> Result<PathBuf, ProviderError> {
        if name.is_empty()
            || name.len() > 255
            || name.starts_with('.')
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        {
            return Err("invalid secret name".into());
        }
        Ok(self.directory.join(name))
    }
}
impl KeyProvider for FileKeyProvider {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, ProviderError> {
        match fs::read(self.path(name)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), ProviderError> {
        let path = self.path(name)?;
        fs::create_private_dir_all(&self.directory)?;
        write_private(&path, secret)
    }
    fn delete(&self, name: &str) -> Result<(), ProviderError> {
        match fs::remove_file(self.path(name)?) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), ProviderError> {
    Ok(fs::write_private(path, bytes)?)
}

#[derive(Clone)]
pub struct Vault {
    provider: Arc<dyn KeyProvider>,
    installation: String,
}
impl Vault {
    pub fn new(provider: Arc<dyn KeyProvider>, installation: &str) -> Self {
        Self {
            provider,
            installation: installation.to_owned(),
        }
    }
    fn name(&self, role: &str) -> String {
        format!("rocky.{}.{role}", self.installation)
    }
    pub fn load_bytes(&self, role: &str) -> Result<Option<Vec<u8>>, Error> {
        self.provider
            .load(&self.name(role))
            .map_err(Error::Provider)
    }
    pub fn store_bytes(&self, role: &str, bytes: &[u8]) -> Result<(), Error> {
        self.provider
            .store(&self.name(role), bytes)
            .map_err(Error::Provider)
    }
    pub fn find(&self, role: &str) -> Result<Option<[u8; 32]>, Error> {
        match self
            .provider
            .load(&self.name(role))
            .map_err(Error::Provider)?
        {
            Some(bytes) => bytes
                .try_into()
                .map(Some)
                .map_err(|_| Error::Provider(format!("secret {role} is corrupt").into())),
            None => Ok(None),
        }
    }
    pub fn get(&self, role: &str) -> Result<[u8; 32], Error> {
        self.find(role)?.ok_or_else(|| {
            Error::Provider(
                format!("secret {role} is missing; the device logged out or lost its keys").into(),
            )
        })
    }
    pub fn put(&self, role: &str, secret: &[u8; 32]) -> Result<(), Error> {
        self.provider
            .store(&self.name(role), secret)
            .map_err(Error::Provider)
    }
    pub fn create(&self, role: &str) -> Result<[u8; 32], Error> {
        if self.find(role)?.is_some() {
            return Err(Error::Provider(
                format!("secret {role} exists and is not replaced").into(),
            ));
        }
        self.replace(role)
    }
    pub fn replace(&self, role: &str) -> Result<[u8; 32], Error> {
        let secret: [u8; 32] = rocky_platform::entropy::random();
        self.put(role, &secret)?;
        Ok(secret)
    }
    pub fn forget(&self, roles: impl IntoIterator<Item = String>) -> Result<(), Error> {
        let mut first = None;
        for role in roles {
            if let Err(error) = self.provider.delete(&self.name(&role)) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), |error| Err(Error::Provider(error)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn files_are_private_and_names_are_checked() -> Result<(), ProviderError> {
        let directory = rocky_platform::fs::Scratch::new().await;
        let store = FileKeyProvider::new(directory.path().join("secrets"));
        assert!(store.load("rocky.a.node")?.is_none());
        store.store("rocky.a.node", &[1; 32])?;
        assert_eq!(store.load("rocky.a.node")?, Some(vec![1; 32]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(directory.path().join("secrets/rocky.a.node"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        for name in ["../x", "A", "", ".hidden", "a/b"] {
            assert!(store.store(name, &[0]).is_err());
        }
        store.delete("rocky.a.node")?;
        store.delete("rocky.a.node")?;
        assert!(store.load("rocky.a.node")?.is_none());
        Ok(())
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn the_file_provider_meets_the_key_provider_contract() {
        let directory = rocky_platform::fs::Scratch::new().await;
        crate::testing::key_provider_contract(&FileKeyProvider::new(directory.path()));
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn the_memory_keys_meet_the_key_provider_contract() {
        crate::testing::key_provider_contract(&crate::testing::MemoryKeys::default());
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_missing_secret_is_an_error_and_an_existing_one_is_not_replaced() -> Result<(), Error>
    {
        let directory = rocky_platform::fs::Scratch::new().await;
        let provider: Arc<dyn KeyProvider> = Arc::new(FileKeyProvider::new(directory.path()));
        let vault = Vault::new(provider.clone(), "a");
        assert!(vault.get("node").is_err());
        assert_eq!(vault.find("node")?, None);
        let node = vault.create("node")?;
        assert_eq!(vault.get("node")?, node);
        assert!(vault.create("node").is_err());
        assert_eq!(vault.get("node")?, node);
        assert!(
            Vault::new(provider.clone(), "b").get("node").is_err(),
            "another installation's roles are other secrets"
        );
        vault.forget(["node".to_owned()])?;
        assert!(vault.get("node").is_err(), "a deleted secret stays missing");
        provider
            .store("rocky.a.user", &[1; 5])
            .map_err(Error::Provider)?;
        assert!(vault.get("user").is_err());
        Ok(())
    }
}
