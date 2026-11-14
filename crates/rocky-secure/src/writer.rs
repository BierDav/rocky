//! Writer identities: a node's full public key names its writes, stamps and
//! created rows, so no replica allocates or maps author numbers.
use crate::{Error, PublicKey};
use rocky_core::SqlValue;
pub(crate) use rocky_core::WriterId;

pub(crate) fn writer(key: &PublicKey) -> WriterId {
    WriterId::from_bytes(*key.as_bytes())
}
/// The verifying key of `writer`; a value that is no key is refused.
pub(crate) fn writer_key(writer: WriterId) -> Result<PublicKey, Error> {
    PublicKey::from_bytes(writer.as_bytes()).map_err(|_| Error::Corrupt("writer key".into()))
}
/// A stored writer column.
pub(crate) fn stored_writer(bytes: &[u8]) -> Result<WriterId, Error> {
    WriterId::from_slice(bytes).map_err(|_| Error::Corrupt("stored writer".into()))
}
pub(crate) fn writer_value(writer: WriterId) -> SqlValue {
    SqlValue::Blob(writer.as_bytes().to_vec())
}
