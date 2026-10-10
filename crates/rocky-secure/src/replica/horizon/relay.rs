//! Signature-checked evidence retained by readers and keyless custody alike.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct HorizonFinalization {
    pub checkpoint: SignedCheckpoint,
    pub acks: Vec<HorizonAck>,
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Whether this holder can reproduce a row cut locally.
    pub async fn can_read_scope(&self, scope: ResourceScope) -> bool {
        self.inner.lock().await.scope_key(scope).is_ok()
    }
    pub(crate) async fn relay_finalized_horizon(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<Box<HorizonFinalization>>, Error> {
        let inner = self.inner.lock().await;
        if let Some(id) = inner.find_scope_id(scope).await?
            && let Some(signed) = inner
                .checkpoint_record(id, true)
                .await?
                .and_then(|r| r.signed)
            && signed.statement.finalized.is_some()
            && signed.statement.finalized.as_ref() == inner.mergeable_horizon(scope).await?.as_ref()
        {
            let acks = inner.horizon_acknowledgments(scope).await?;
            return Ok(Some(Box::new(HorizonFinalization {
                checkpoint: signed,
                acks,
            })));
        }
        let mut rows = inner
            .conn
            .query(
                "SELECT checkpoint FROM secure_horizon_relay_finalized WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| decode(&row.get::<Vec<u8>>(0)?).map(Box::new))
            .transpose()
    }
    /// Verifies a finalized checkpoint without importing or decrypting its rows.
    pub(crate) async fn receive_relay_finalized_horizon(
        &self,
        evidence: HorizonFinalization,
    ) -> Result<(), Error> {
        let checkpoint = &evidence.checkpoint;
        let mut inner = self.inner.lock().await;
        let time = now()?;
        checkpoint.verify_authority(&inner, time)?;
        inner.verify_checkpoint_horizon(checkpoint)?;
        let signed = checkpoint
            .statement
            .finalized
            .as_ref()
            .ok_or_else(|| denied("relay checkpoint is not finalized"))?;
        inner.verify_horizon(signed).await?;
        if evidence.acks.len() > 64 {
            return Err(denied("finalized horizon acknowledgment bound"));
        }
        for ack in &evidence.acks {
            ack.verify().map_err(credential)?;
            if ack.horizon != signed.id().map_err(credential)?
                || !signed
                    .statement
                    .peers
                    .iter()
                    .any(|peer| peer.node == ack.node)
            {
                return Err(denied(
                    "acknowledgment is outside the finalized horizon participant set",
                ));
            }
        }
        let scope = signed.statement.scope;
        if let Some(held) = inner.mergeable_horizon(scope).await?
            && (signed.statement.sequence < held.statement.sequence
                || (signed.statement.sequence == held.statement.sequence
                    && signed != &held
                    && !(signed.statement.previous == held.statement.previous
                        && signed.id().map_err(credential)? < held.id().map_err(credential)?)))
        {
            return Ok(());
        }
        // Readers import through the normal checkpoint path. Custody clears
        // the pending cut but keeps signed finalization evidence for delivery.
        if inner.scope_key(scope).is_ok() {
            if inner.mergeable_horizon(scope).await?.as_ref() != Some(signed) {
                drop(inner);
                match Box::pin(self.receive_horizon((**signed).clone())).await {
                    Ok(()) => {}
                    Err(Error::Rejected(_)) => {
                        Box::pin(self.require_horizon_rebootstrap((**signed).clone())).await?;
                    }
                    Err(error) => return Err(error),
                }
                inner = self.inner.lock().await;
            }
            if inner.mergeable_horizon(scope).await?.as_ref() == Some(signed) {
                let finalized = inner.finalized_basis(scope).await?
                    == Some((signed.statement.sequence, signed.id().map_err(credential)?));
                let bootstrap = if finalized {
                    false
                } else {
                    match inner.check_horizon_cut(&signed.statement).await {
                        Ok(()) => false,
                        Err(Error::Rejected(_)) => true,
                        Err(error) => return Err(error),
                    }
                };
                transaction!(inner, |inner| {
                    if bootstrap {
                        inner.conn.execute("INSERT INTO secure_horizon_bootstrap VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET statement=excluded.statement", [encode(&scope)?, signed.to_bytes().map_err(credential)?]).await?;
                    }
                    for ack in &evidence.acks {
                        if signed.statement.peers.iter().any(|peer| {
                            peer.node == ack.node && time < peer.deadline(signed.statement.issued)
                        }) {
                            inner.store_horizon_ack(signed, ack).await?;
                        }
                    }
                    Ok(())
                })?;
                inner.flush()?;
            }
            return Ok(());
        }
        transaction!(inner, |inner| {
            inner.conn.execute("INSERT INTO secure_horizon_relay_finalized VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET checkpoint=excluded.checkpoint", [encode(&scope)?,encode(&evidence)?]).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_horizon_ack WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            inner.index_pending_horizon(signed, false).await?;
            inner.conn.execute("INSERT INTO secure_horizon_rejoined VALUES(?,?,?) ON CONFLICT(scope,node) DO UPDATE SET horizon=excluded.horizon", [encode(&scope)?, inner.config.node.public_key().as_bytes().to_vec(), signed.id().map_err(credential)?.to_vec()]).await?;
            inner.conn.execute("INSERT INTO secure_mergeable_horizon VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET statement=excluded.statement", [encode(&scope)?,signed.to_bytes().map_err(credential)?]).await?;
            Ok(())
        })?;
        inner.flush()?;
        Ok(())
    }
    pub(crate) async fn receive_horizon_relay(
        &self,
        scope: ResourceScope,
        signed: Option<Box<SignedHorizon>>,
        acks: Vec<HorizonAck>,
        finalized: Option<Box<HorizonFinalization>>,
    ) -> Result<(), Error> {
        if acks.len() > 64
            || signed.as_ref().is_some_and(|s| s.statement.scope != scope)
            || finalized
                .as_ref()
                .is_some_and(|s| s.checkpoint.statement.scope != scope)
        {
            return Err(denied("horizon relay outside requested scope or bound"));
        }
        let expected = match &signed {
            Some(signed) => Some(signed.clone()),
            None => self.admitted_horizon(scope).await?,
        };
        for ack in &acks {
            ack.verify().map_err(credential)?;
            if expected.as_ref().is_none_or(|s| {
                s.id().ok() != Some(ack.horizon)
                    || !s.statement.peers.iter().any(|peer| peer.node == ack.node)
            }) {
                return Err(denied(
                    "acknowledgment is outside the horizon participant set",
                ));
            }
        }
        if let Some(signed) = signed {
            self.receive_horizon(*signed).await?;
        }
        // A valid losing sibling's evidence cannot enter the winning set.
        if self.admitted_horizon(scope).await? == expected {
            for ack in acks {
                self.receive_horizon_ack(scope, ack).await?;
            }
        }
        if let Some(finalized) = finalized {
            Box::pin(self.receive_relay_finalized_horizon(*finalized)).await?;
        }
        Ok(())
    }
}
