use crate::{Error, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Publisher(pub PublicKey);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Identity(pub PublicKey);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    NodeCertificate,
    Capability,
    Revocation,
    Statement,
}

pub trait IdentityProvider: Send + Sync {
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
