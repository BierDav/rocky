//! Where an installation's secrets live. See `docs/runtime/credentials.md`
//! for the threat model of the file provider.
use crate::Error;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub type ProviderError = Box<dyn std::error::Error + Send + Sync>;

/// Where an installation keeps its signing, transport and reader keys.
/// Platform implementations wrap a keychain or keystore. Names are unique
/// per installation and use only `[a-z0-9._-]`. A provider never invents a
/// secret: `load` of an absent name is `None`.
pub trait KeyProvider: Send + Sync {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, ProviderError>;
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), ProviderError>;
    fn delete(&self, name: &str) -> Result<(), ProviderError>;
}

/// The fallback provider: owner-only files in one directory, plaintext at
/// rest. They protect keys from other users of the machine, not from its
/// owner, a backup or a copy of the disk.
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
        match std::fs::read(self.path(name)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), ProviderError> {
        let path = self.path(name)?;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&self.directory)?;
        write_private(&path, secret)
    }
    fn delete(&self, name: &str) -> Result<(), ProviderError> {
        match std::fs::remove_file(self.path(name)?) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }
}

/// Replaces `path` atomically with owner-only `bytes`, durable on return.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), ProviderError> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().ok_or("missing parent")?)?.sync_all()?;
    Ok(())
}

/// One installation's secrets, by role, as `rocky.<installation>.<role>`.
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
    /// The secret of `role`, or `None` when the provider holds none.
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
    /// The secret of `role`. A missing secret is an error: nothing is
    /// generated in its place.
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
    /// A fresh random secret for a role that holds none. An existing
    /// secret is never replaced.
    pub fn create(&self, role: &str) -> Result<[u8; 32], Error> {
        if self.find(role)?.is_some() {
            return Err(Error::Provider(
                format!("secret {role} exists and is not replaced").into(),
            ));
        }
        self.replace(role)
    }
    /// A fresh random secret that replaces whatever `role` held. Only for
    /// secrets nothing else was sealed or signed under.
    pub fn replace(&self, role: &str) -> Result<[u8; 32], Error> {
        let mut secret = [0; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut secret);
        self.put(role, &secret)?;
        Ok(secret)
    }
    /// Deletes every role, continuing past failures; returns the first.
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
    #[test]
    fn files_are_private_and_names_are_checked() -> Result<(), ProviderError> {
        let directory = tempfile::tempdir()?;
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
    #[test]
    fn a_missing_secret_is_an_error_and_an_existing_one_is_not_replaced() -> Result<(), Error> {
        let directory = tempfile::tempdir().map_err(|error| Error::Provider(error.into()))?;
        let provider: Arc<dyn KeyProvider> = Arc::new(FileKeyProvider::new(directory.path()));
        let vault = Vault::new(provider.clone(), "a");
        assert!(vault.get("node").is_err());
        assert_eq!(vault.find("node")?, None);
        let node = vault.create("node")?;
        assert_eq!(vault.get("node")?, node);
        assert!(vault.create("node").is_err());
        assert_eq!(vault.get("node")?, node);
        // Another installation's roles are other secrets.
        assert!(Vault::new(provider.clone(), "b").get("node").is_err());
        // A deleted secret stays missing; a truncated one is corrupt.
        vault.forget(["node".to_owned()])?;
        assert!(vault.get("node").is_err());
        provider
            .store("rocky.a.user", &[1; 5])
            .map_err(Error::Provider)?;
        assert!(vault.get("user").is_err());
        Ok(())
    }
}
