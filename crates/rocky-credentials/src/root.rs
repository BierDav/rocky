use crate::{
    Error, IdentityKey, IdentityProvider, PrivateKey, PublicKey, Purpose, Signature,
    phrase_key::PhraseKey,
};

const OWNER_IDENTITY_DERIVATION: &[u8] = b"rocky identity v1";

pub struct RootKey(PhraseKey);
impl RootKey {
    pub fn generate() -> Self {
        Self(PhraseKey::generate())
    }
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self(PhraseKey::from_secret(secret))
    }
    pub fn from_phrase(phrase: &str) -> Result<Self, Error> {
        PhraseKey::from_phrase(phrase).map(Self)
    }
    pub fn phrase(&self) -> String {
        self.0.phrase()
    }
    pub fn secret(&self) -> [u8; 32] {
        self.0.secret()
    }
    pub fn public_key(&self) -> PublicKey {
        self.0.public_key()
    }
    pub fn identity(&self) -> IdentityKey {
        let secret = zeroize::Zeroizing::new(self.secret());
        IdentityKey::from_secret(crate::group::hkdf(&[], &*secret, OWNER_IDENTITY_DERIVATION))
    }
    pub(crate) fn private(&self) -> &PrivateKey {
        self.0.private()
    }
}
impl IdentityProvider for RootKey {
    fn owner_identity(&self) -> Option<std::sync::Arc<dyn IdentityProvider>> {
        Some(std::sync::Arc::new(self.identity()))
    }
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
        let expected = "9c390ed6e479fa7ac5add3e9107652cd7cf143dac2b4466a3a1f7178be565a91";
        let hex: String = identity
            .secret()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            hex, expected,
            "HKDF-SHA256 with empty salt and info rocky identity v1 is part of the key format"
        );
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
