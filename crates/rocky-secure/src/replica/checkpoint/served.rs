use super::*;

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn seal_checkpoint_offer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        seal: Commitment,
    ) -> Result<Option<SignedCheckpoint>, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner = flushed(inner, &self.inner).await?;
        inner.authorize_seal_peer(peer, credentials, scope)?;
        if !inner.importing.contains(&scope)
            && !inner.importing(scope).await?
            && let Some(signed) = inner.no_frames_base(scope).await?
            && signed.statement.seal == Some(seal)
        {
            return Ok(inner
                .served_checkpoint(scope)
                .await?
                .map(|(_, signed)| signed)
                .filter(|signed| signed.signer() != peer));
        }
        if inner.scope_key(scope).is_err()
            || inner.importing.contains(&scope)
            || inner.importing(scope).await?
            || (inner.partitioned(scope) && !inner.partitions.contains_key(&scope))
        {
            return Ok(None);
        }
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok(None);
        };
        Ok(inner
            .seal_served(scope, id, seal)
            .await?
            .map(|(_, signed)| signed)
            .filter(|signed| signed.signer() != peer))
    }
}

impl<A: RowApplication> Inner<A> {
    async fn seal_served(
        &self,
        scope: ResourceScope,
        id: i64,
        seal: Commitment,
    ) -> Result<Option<(i64, SignedCheckpoint)>, Error> {
        if let Some(record) = self.checkpoint_record(id, true).await?
            && let Ok(signed) = self.verify_record(scope, &record)
            && signed.statement.seal == Some(seal)
            && (signed.statement.no_frames()
                || signed.statement.key_epoch() == Some(self.scope_key(scope)?.0))
            && signed.verify_authority(self, now()?).is_ok()
        {
            return Ok(Some((id, signed)));
        }
        Ok(None)
    }
    pub(super) async fn served_seal_checkpoint(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<Option<(i64, SignedCheckpoint)>, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(None);
        };
        let Some(signed) = self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed)
        else {
            return Ok(None);
        };
        if signed.id()? == checkpoint
            && let Some(seal) = signed.statement.seal
            && let Some(served) = self.seal_served(scope, id, seal).await?
            && served.1.id()? == checkpoint
        {
            return Ok(Some(served));
        }
        Ok(None)
    }
}
