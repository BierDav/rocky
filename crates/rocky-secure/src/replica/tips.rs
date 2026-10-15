//! Resolves opaque tips to their locally verified schema manifests.
use super::*;

impl<A: RowApplication> Inner<A> {
    pub(crate) fn manifest_of_tip(&self, tip: TipId) -> Result<Commitment, Error> {
        std::iter::once(self.manifest)
            .chain(self.previous.iter().map(|previous| previous.manifest))
            .find(|manifest| rocky_credentials::genesis_tip(*manifest) == tip)
            .ok_or_else(|| Error::Incompatible("unknown tip".into()))
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    /// The tip whose live workspace this replica follows.
    pub async fn followed_tip(&self) -> TipId {
        self.inner.lock().await.tip
    }
}
