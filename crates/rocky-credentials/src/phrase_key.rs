//! A key held in memory for one call, restored from a 24-word phrase or a
//! passkey's 32-byte PRF output. `RootKey` and `IdentityKey` share it; each
//! wraps it in its own type so neither stands in for the other.
use crate::{Error, PrivateKey, PublicKey, denied};

pub(crate) struct PhraseKey(PrivateKey);
impl PhraseKey {
    pub(crate) fn generate() -> Self {
        Self(PrivateKey::from_bytes(&rocky_platform::entropy::random()))
    }
    pub(crate) fn from_secret(secret: [u8; 32]) -> Self {
        Self(PrivateKey::from_bytes(&secret))
    }
    /// The key a 24-word English BIP 39 phrase names; whitespace is free.
    pub(crate) fn from_phrase(phrase: &str) -> Result<Self, Error> {
        let words = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
        let mnemonic = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &words)
            .map_err(|_| denied("not a valid 24-word phrase"))?;
        let (entropy, length) = mnemonic.to_entropy_array();
        if length != 32 {
            return Err(denied("a key phrase has 24 words"));
        }
        let mut secret = [0; 32];
        secret.copy_from_slice(&entropy[..32]);
        Ok(Self::from_secret(secret))
    }
    pub(crate) fn phrase(&self) -> String {
        bip39::Mnemonic::from_entropy_in(bip39::Language::English, self.0.as_bytes())
            .map(|mnemonic| mnemonic.words().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }
    pub(crate) fn secret(&self) -> [u8; 32] {
        *self.0.as_bytes()
    }
    pub(crate) fn public_key(&self) -> PublicKey {
        self.0.public_key()
    }
    pub(crate) fn private(&self) -> &PrivateKey {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_phrase_restores_the_same_key_and_a_wrong_phrase_is_refused() {
        let key = PhraseKey::generate();
        let phrase = key.phrase();
        assert_eq!(phrase.split(' ').count(), 24);
        let restored =
            PhraseKey::from_phrase(&format!("  {}\n", phrase.replace(' ', "  "))).unwrap();
        assert_eq!(restored.public_key(), key.public_key());
        let mut words: Vec<&str> = phrase.split(' ').collect();
        // Flip a checksum bit; swapping words can accidentally make another valid phrase.
        let dictionary = bip39::Language::English.word_list();
        let last = dictionary
            .iter()
            .position(|word| *word == words[23])
            .unwrap();
        words[23] = dictionary[last ^ 1];
        assert!(PhraseKey::from_phrase(&words.join(" ")).is_err());
        let twelve = bip39::Mnemonic::from_entropy(&[7; 16]).unwrap().to_string();
        assert!(PhraseKey::from_phrase(&twelve).is_err());
    }
}
