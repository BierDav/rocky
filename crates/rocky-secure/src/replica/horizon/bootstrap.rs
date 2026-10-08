//! A verified remote cut blocks writes throughout interrupted rebootstrap.
use super::*;
impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn horizon_bootstrap(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<SignedHorizon>, Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT statement FROM secure_horizon_bootstrap WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| SignedHorizon::from_bytes(&row.get::<Vec<u8>>(0)?).map_err(credential))
            .transpose()
    }
    pub(in crate::replica) async fn check_horizon_bootstrap(
        &self,
        checkpoint: &SignedCheckpoint,
    ) -> Result<(), Error> {
        let Some(needed) = self.horizon_bootstrap(checkpoint.statement.scope).await? else {
            return Ok(());
        };
        let incoming = checkpoint
            .statement
            .finalized
            .as_ref()
            .ok_or_else(|| denied("behind device requires a finalized checkpoint"))?;
        if incoming.statement.sequence < needed.statement.sequence
            || (incoming.statement.sequence == needed.statement.sequence
                && incoming.as_ref() != &needed)
            || needed.statement.logs.iter().any(|log| {
                !checkpoint.statement.frontier.iter().any(|position| {
                    position.author == log.signer
                        && position.sequence >= log.sequence
                        && (position.sequence != log.sequence
                            || position.hash.as_bytes() == &log.head)
                })
            })
        {
            return Err(denied(
                "checkpoint does not cover the required horizon bootstrap",
            ));
        }
        Ok(())
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    /// Persists the issuer cut learned from sync before downloading its replay base.
    pub async fn require_horizon_rebootstrap(&self, signed: SignedHorizon) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        inner.verify_horizon(&signed)?;
        if signed.statement.issued > now()? {
            return Err(denied("horizon issue time is in the future"));
        }
        for held in [
            inner
                .mergeable_horizon(signed.statement.scope)
                .await?
                .map(|held| *held),
            inner.horizon_bootstrap(signed.statement.scope).await?,
        ]
        .into_iter()
        .flatten()
        {
            if signed.statement.sequence < held.statement.sequence
                || (signed.statement.sequence == held.statement.sequence && signed != held)
            {
                return Err(denied(
                    "bootstrap requirement moves the issuer cut backwards",
                ));
            }
        }
        transaction!(inner, |inner| {
            inner.conn.execute("INSERT INTO secure_horizon_bootstrap VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET statement=excluded.statement", [encode(&signed.statement.scope)?, signed.to_bytes().map_err(credential)?]).await?;
            Ok(())
        })?;
        inner.flush()?;
        Ok(())
    }
}
