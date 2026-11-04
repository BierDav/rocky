//! The domain's root key: the publisher key, held offline. A passkey (its
//! PRF output) or a 24-word phrase supplies its 32-byte secret; Rocky never
//! stores it. It signs the genesis, trusted grants and their revocations,
//! and opens the key escrow for recovery.
use crate::{Error, IdentityProvider, PrivateKey, PublicKey, Purpose, Signature, denied};

/// The root's secret in memory, for one call. Implements `IdentityProvider`;
/// an application with its own signer supplies that instead.
pub struct RootKey(PrivateKey);
impl RootKey {
    /// A fresh root key.
    pub fn generate() -> Self {
        Self(PrivateKey::from_bytes(&rocky_platform::entropy::random()))
    }
    /// The root key with this 32-byte secret, such as a passkey's PRF output.
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self(PrivateKey::from_bytes(&secret))
    }
    /// The root key a 24-word English BIP 39 phrase names.
    pub fn from_phrase(phrase: &str) -> Result<Self, Error> {
        let words = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
        let mnemonic = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &words)
            .map_err(|_| denied("not a valid 24-word root phrase"))?;
        let (entropy, length) = mnemonic.to_entropy_array();
        if length != 32 {
            return Err(denied("a root phrase has 24 words"));
        }
        let mut secret = [0; 32];
        secret.copy_from_slice(&entropy[..32]);
        Ok(Self::from_secret(secret))
    }
    /// The 24 words that restore this key.
    pub fn phrase(&self) -> String {
        bip39::Mnemonic::from_entropy_in(bip39::Language::English, self.0.as_bytes())
            .map(|mnemonic| mnemonic.words().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }
    /// The 32-byte secret, for an application that keeps it elsewhere.
    pub fn secret(&self) -> [u8; 32] {
        *self.0.as_bytes()
    }
    pub fn public_key(&self) -> PublicKey {
        self.0.public_key()
    }
}
impl IdentityProvider for RootKey {
    fn public_key(&self) -> Result<PublicKey, Error> {
        Ok(self.0.public_key())
    }
    fn sign(&self, _: Purpose, message: &[u8]) -> Result<Signature, Error> {
        Ok(self.0.sign(message))
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
    fn a_phrase_restores_the_same_root_and_a_wrong_phrase_is_refused() {
        let root = RootKey::generate();
        let phrase = root.phrase();
        assert_eq!(phrase.split(' ').count(), 24);
        let restored = RootKey::from_phrase(&format!("  {}\n", phrase.replace(' ', "  "))).unwrap();
        assert_eq!(restored.public_key(), root.public_key());
        let mut words: Vec<&str> = phrase.split(' ').collect();
        words.swap(0, 1);
        if words[0] != words[1] {
            assert!(RootKey::from_phrase(&words.join(" ")).is_err());
        }
        let twelve = bip39::Mnemonic::from_entropy(&[7; 16]).unwrap().to_string();
        assert!(RootKey::from_phrase(&twelve).is_err());
    }
}
