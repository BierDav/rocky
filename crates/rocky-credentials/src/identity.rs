//! The distinct identities of a domain. Each is its own key or name; none
//! implies another, and only a publisher-rooted capability chain grants
//! anything.
use crate::{Error, PublicKey};
use rocky_core::ResourceScope;
use serde::{Deserialize, Serialize};

/// The domain's trust anchor: signs its manifest, policies and root
/// capabilities. Devices pin it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Publisher(pub PublicKey);

/// The user a credential chain delegates from: the subject of its root
/// capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LoginPrincipal(pub PublicKey);

/// The device key that signs operations, session frames and renewal
/// requests: the subject of a chain's leaf. It owns the device's author logs.
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
