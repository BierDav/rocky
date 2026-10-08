//! The key provider of an installation. The contract and the file
//! fallback live in `rocky-credentials`; see `docs/runtime/credentials.md`.
pub use rocky_credentials::{FileKeyProvider as FileSecretStore, KeyProvider as SecretStore};
pub(crate) use rocky_credentials::{Vault, write_private};
