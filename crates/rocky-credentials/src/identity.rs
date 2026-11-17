use crate::{Error, PrivateKey, PublicKey, Signature};
use rocky_core::ResourceScope;
use rocky_platform::fs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Publisher(pub PublicKey);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Identity(pub PublicKey);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SigningDevice(pub PublicKey);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransportEndpoint(pub [u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReaderScope {
    pub scope: ResourceScope,
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CoordinatorControl(pub PublicKey);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalLogin {
    pub provider: String,
    pub subject: String,
}

pub trait LoginProvider: Send + Sync {
    fn verify(&self, assertion: &[u8], time: u64) -> Result<ExternalLogin, Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    NodeCertificate,
    Capability,
    Revocation,
    Statement,
}

pub trait IdentityProvider: Send + Sync {
    fn owner_identity(&self) -> Option<std::sync::Arc<dyn IdentityProvider>> {
        None
    }
    fn public_key(&self) -> Result<PublicKey, Error>;
    fn sign(&self, purpose: Purpose, message: &[u8]) -> Result<Signature, Error>;
}

impl IdentityProvider for PrivateKey {
    fn public_key(&self) -> Result<PublicKey, Error> {
        Ok(PrivateKey::public_key(self))
    }
    fn sign(&self, _: Purpose, message: &[u8]) -> Result<Signature, Error> {
        Ok(PrivateKey::sign(self, message))
    }
}

pub struct FileIdentityProvider {
    directory: PathBuf,
    key: PrivateKey,
}
impl FileIdentityProvider {
    pub fn open(directory: impl Into<PathBuf>) -> Result<Self, Error> {
        let directory = directory.into();
        let path = directory.join("identity");
        let provider = |error: std::io::Error| Error::Provider(error.into());
        let key = match fs::read(&path) {
            Ok(bytes) => {
                let bytes: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| Error::Provider("the identity key is corrupt".into()))?;
                PrivateKey::from_bytes(&bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_private_dir_all(&directory).map_err(provider)?;
                let key = PrivateKey::from_bytes(&rocky_platform::entropy::random());
                match fs::create_new(&path, key.as_bytes()) {
                    Ok(()) => key,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Self::open(directory);
                    }
                    Err(error) => return Err(provider(error)),
                }
            }
            Err(error) => return Err(provider(error)),
        };
        Ok(Self { directory, key })
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}
impl IdentityProvider for FileIdentityProvider {
    fn public_key(&self) -> Result<PublicKey, Error> {
        Ok(self.key.public_key())
    }
    fn sign(&self, _: Purpose, message: &[u8]) -> Result<Signature, Error> {
        Ok(self.key.sign(message))
    }
}
