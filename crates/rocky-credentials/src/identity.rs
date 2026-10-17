//! The distinct identities of a domain. Each is its own key or name; none
//! implies another, and only a publisher-rooted capability chain grants
//! anything. An `IdentityProvider` supplies the identity that holds
//! permissions; see `docs/runtime/credentials.md#identities`.
use crate::{AuthorRange, Error, PrivateKey, PublicKey, Signature, denied};
use rocky_core::ResourceScope;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The domain's trust anchor: signs its manifest, policies and root
/// capabilities. Devices pin it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Publisher(pub PublicKey);

/// Who holds permissions: the key of the link a node certificate names as
/// its parent. Its nodes sign under certificates it signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Identity(pub PublicKey);

/// The node key that signs operations, session frames and renewal
/// requests, certified by its identity. It owns the node's author logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SigningDevice(pub PublicKey);

/// The stable key of a device's network routes. It reaches a device and
/// authorizes nothing: whoever answers must still prove a signing device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransportEndpoint(pub [u8; 32]);

/// A reader set: an encryption scope at one key epoch. Holding its key
/// reveals every row encrypted under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReaderScope {
    pub scope: ResourceScope,
    pub epoch: u64,
}

/// The key that signs a tenant's coordinator control requests. It manages
/// coordination metadata and reads or writes no rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CoordinatorControl(pub PublicKey);

/// Who an external login provider says is asking. It is a name an issuer
/// may consider when it decides to issue a capability. It carries no
/// grant, no scope key and no executor eligibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalLogin {
    pub provider: String,
    pub subject: String,
}

/// An adapter for a login provider. No provider ships with Rocky.
pub trait LoginProvider: Send + Sync {
    /// Verifies a provider assertion at `time`.
    fn verify(&self, assertion: &[u8], time: u64) -> Result<ExternalLogin, Error>;
}

/// What an identity signs. A provider may show it to its user or refuse it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// A certificate for one of the identity's nodes; it adds no rights.
    NodeCertificate,
    /// A capability the identity's authority grants another identity.
    Capability,
    /// A revocation statement.
    Revocation,
    /// Another statement its authority requires: a manifest, a tip statement,
    /// a renewal answer or a request proving the identity.
    Statement,
}

/// Supplies an identity: its public key and signatures. It never returns
/// the private key; how the key is stored or derived is the provider's.
pub trait IdentityProvider: Send + Sync {
    fn public_key(&self) -> Result<PublicKey, Error>;
    /// Signs `message`, the domain-separated bytes of a statement.
    fn sign(&self, purpose: Purpose, message: &[u8]) -> Result<Signature, Error>;
    /// An author number of `range` that this identity never handed out
    /// before, for a new node certificate.
    fn reserve_author(&self, range: AuthorRange) -> Result<u32, Error> {
        let _ = range;
        Err(denied("this identity provider keeps no author numbers"))
    }
}

/// A key held in memory signs as itself; it keeps no author numbers.
impl IdentityProvider for PrivateKey {
    fn public_key(&self) -> Result<PublicKey, Error> {
        Ok(PrivateKey::public_key(self))
    }
    fn sign(&self, _: Purpose, message: &[u8]) -> Result<Signature, Error> {
        Ok(PrivateKey::sign(self, message))
    }
}

/// An identity in one owner-only directory, for tests and servers: the
/// key in `identity` and one file per reserved author number. Several
/// processes may share it. Plaintext at rest, like `FileKeyProvider`.
pub struct FileIdentityProvider {
    directory: PathBuf,
    key: PrivateKey,
}
impl FileIdentityProvider {
    /// Opens the identity in `directory`, creating a fresh key when it holds
    /// none.
    pub fn open(directory: impl Into<PathBuf>) -> Result<Self, Error> {
        let directory = directory.into();
        let path = directory.join("identity");
        let provider = |error: std::io::Error| Error::Provider(error.into());
        let key = match std::fs::read(&path) {
            Ok(bytes) => {
                let bytes: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| Error::Provider("the identity key is corrupt".into()))?;
                PrivateKey::from_bytes(&bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_private_directory(&directory.join("authors")).map_err(provider)?;
                let key = PrivateKey::new();
                // Two processes creating at once: the first file wins.
                match create_new(&path, key.as_bytes()) {
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
    fn reserve_author(&self, range: AuthorRange) -> Result<u32, Error> {
        let authors = self.directory.join("authors");
        create_private_directory(&authors).map_err(|error| Error::Provider(error.into()))?;
        // An exclusive create is the reservation, across processes.
        for author in range.first..=range.last {
            match create_new(&authors.join(author.to_string()), &[]) {
                Ok(()) => return Ok(author),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(Error::Provider(error.into())),
            }
        }
        Err(denied(
            "every author number of the identity's range is taken",
        ))
    }
}
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}
/// Writes a new owner-only file, durable on return; an existing one fails.
fn create_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        create_private_directory(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
