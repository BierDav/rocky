use crate::{
    Error, IdentityProvider, PublicKey, Purpose, RootKey, Signature, phrase_key::PhraseKey,
};

pub struct IdentityKey(PhraseKey);
never_implements!(IdentityKey: std::ops::Deref<Target = RootKey>);
never_implements!(IdentityKey: AsRef<RootKey>);
never_implements!(IdentityKey: std::borrow::Borrow<RootKey>);
never_implements!(IdentityKey: Into<RootKey>);
never_implements!(RootKey: std::ops::Deref<Target = IdentityKey>);
never_implements!(RootKey: AsRef<IdentityKey>);
never_implements!(RootKey: std::borrow::Borrow<IdentityKey>);
never_implements!(RootKey: Into<IdentityKey>);
impl IdentityKey {
    pub fn generate() -> Self {
        Self(PhraseKey::generate())
    }
    pub(crate) fn from_secret(secret: [u8; 32]) -> Self {
        Self(PhraseKey::from_secret(secret))
    }
    pub fn from_phrase(phrase: &str) -> Result<Self, Error> {
        PhraseKey::from_phrase(phrase).map(Self)
    }
    pub fn phrase(&self) -> String {
        self.0.phrase()
    }
    pub(crate) fn secret(&self) -> [u8; 32] {
        self.0.secret()
    }
    pub fn person_key(&self) -> PublicKey {
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
        write!(f, "IdentityKey({})", self.person_key())
    }
}
