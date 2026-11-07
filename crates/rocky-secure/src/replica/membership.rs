//! A relayed author carries the same public membership evidence as a direct peer.
use super::*;

impl<A: RowApplication> EncryptedReplica<A> {
    pub(super) async fn learn_carried_membership(
        &self,
        frames: &[EncryptedFrame],
    ) -> Result<(), Error> {
        let evidence = {
            let inner = self.inner.lock(Class::Background).await;
            let time = now()?;
            let mut evidence = RevocationSet::new();
            let known = inner
                .config
                .revocations
                .certificates()
                .map(rocky_credentials::SignedCapability::id)
                .collect::<Result<BTreeSet<_>, _>>()
                .map_err(credential)?;
            for frame in frames {
                let Ok(operation) = frame.operation_ref() else {
                    continue;
                };
                if operation.header.extensions.scope.domain.app != inner.definition.app {
                    continue;
                }
                let Ok(chain) = frame.chain() else {
                    continue;
                };
                if inner
                    .verified_chain(chain.credentials(), &inner.config.revocations, time)
                    .is_err()
                {
                    continue;
                }
                let Some(certificate) = chain.credentials().certificate() else {
                    continue;
                };
                if !known.contains(&certificate.id().map_err(credential)?) {
                    evidence.link(certificate.clone()).map_err(credential)?;
                }
            }
            evidence
        };
        // Membership is an independently signed declaration, even if the
        // subsequent application payload is refused. Notifications follow commit.
        if !evidence.is_empty() {
            self.learn_revocations(&evidence).await?;
        }
        Ok(())
    }
}
