use crate::{IdentityKey, RootKey};

pub fn identity_key(secret: [u8; 32]) -> IdentityKey {
    IdentityKey::from_secret(secret)
}
pub fn identity_secret(key: &IdentityKey) -> [u8; 32] {
    key.secret()
}
pub fn root_key(secret: [u8; 32]) -> RootKey {
    RootKey::from_secret(secret)
}
pub fn root_secret(key: &RootKey) -> [u8; 32] {
    key.secret()
}
