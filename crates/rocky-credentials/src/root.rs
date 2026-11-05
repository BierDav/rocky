//! The domain's root key: the publisher key, held offline. A passkey (its
//! PRF output) or a 24-word phrase supplies its 32-byte secret; Rocky never
//! stores it. It signs the genesis, trusted grants and their revocations,
//! and opens the key escrow for recovery.
use crate::{
    Error, IdentityKey, IdentityProvider, PrivateKey, PublicKey, Purpose, Signature,
    phrase_key::PhraseKey,
};

/// Derives the owner's identity key from the root secret.
const IDENTITY_DERIVATION: &[u8] = b"rocky identity v1";

/// The root's secret in memory, for one call. Implements `IdentityProvider`;
/// an application with its own signer supplies that instead.
pub struct RootKey(PhraseKey);
impl RootKey {
    /// A fresh root key.
    pub fn generate() -> Self {
        Self(PhraseKey::generate())
    }
    /// The root key with this 32-byte secret, such as a passkey's PRF output.
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self(PhraseKey::from_secret(secret))
    }
    /// The root key a 24-word English BIP 39 phrase names.
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
    /// The owner's identity key, derived from the root secret (HKDF-SHA256,
    /// `"rocky identity v1"`), so one phrase or passkey restores both. It
    /// is a different key: nothing it signs verifies as the root's.
    pub fn identity(&self) -> IdentityKey {
        IdentityKey::from_secret(crate::group::hkdf(&[], &self.secret(), IDENTITY_DERIVATION))
    }
    pub(crate) fn private(&self) -> &PrivateKey {
        self.0.private()
    }
}
impl IdentityProvider for RootKey {
    fn public_key(&self) -> Result<PublicKey, Error> {
        Ok(self.0.public_key())
    }
    fn sign(&self, _: Purpose, message: &[u8]) -> Result<Signature, Error> {
        Ok(self.0.private().sign(message))
    }
}
impl std::fmt::Debug for RootKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RootKey({})", self.public_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn the_owner_identity_is_derived_from_the_root_and_differs_from_it() {
        let root = RootKey::from_secret([7; 32]);
        let identity = root.identity();
        // HKDF-SHA256 (RFC 5869), empty salt, info "rocky identity v1":
        // the derivation and its label are part of the key format.
        let expected = "9c390ed6e479fa7ac5add3e9107652cd7cf143dac2b4466a3a1f7178be565a91";
        let hex: String = identity
            .secret()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(hex, expected);
        let restored = RootKey::from_phrase(&root.phrase()).unwrap().identity();
        assert_eq!(restored.secret(), identity.secret());
        assert_ne!(identity.secret(), root.secret());
        assert_ne!(identity.public_key(), root.public_key());
        assert_ne!(
            RootKey::from_secret([8; 32]).identity().public_key(),
            identity.public_key()
        );
    }
}
