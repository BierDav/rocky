//! An identity's own key, held like the root's: a 24-word phrase or a
//! passkey's PRF output, in memory for one call and never stored by Rocky.
use crate::{Error, IdentityProvider, PublicKey, Purpose, Signature, phrase_key::PhraseKey};

/// An identity's secret in memory, for one call. It shares the root key's
/// phrase and passkey encodings but is its own type: no root-only call
/// takes it, and it converts into no `RootKey`.
///
/// ```compile_fail
/// let identity = rocky_credentials::IdentityKey::generate();
/// let root: &rocky_credentials::RootKey = &identity;
/// ```
pub struct IdentityKey(PhraseKey);
impl IdentityKey {
    /// A fresh identity key.
    pub fn generate() -> Self {
        Self(PhraseKey::generate())
    }
    /// The identity key with this 32-byte secret, such as a passkey's PRF
    /// output.
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self(PhraseKey::from_secret(secret))
    }
    /// The identity key a 24-word English BIP 39 phrase names.
    pub fn from_phrase(phrase: &str) -> Result<Self, Error> {
        PhraseKey::from_phrase(phrase).map(Self)
    }
    /// The 24 words that restore this key.
    pub fn phrase(&self) -> String {
        self.0.phrase()
    }
    /// The 32-byte secret, for an application that keeps it elsewhere.
    pub fn secret(&self) -> [u8; 32] {
        self.0.secret()
    }
    pub fn public_key(&self) -> PublicKey {
        self.0.public_key()
    }
}
impl IdentityProvider for IdentityKey {
    fn public_key(&self) -> Result<PublicKey, Error> {
        Ok(self.0.public_key())
    }
    fn sign(&self, _: Purpose, message: &[u8]) -> Result<Signature, Error> {
        Ok(self.0.private().sign(message))
    }
}
impl std::fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IdentityKey({})", self.public_key())
    }
}
