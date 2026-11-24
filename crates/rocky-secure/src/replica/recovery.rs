use super::*;

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn recover_locally(&self) -> Result<(), Error> {
        let mut first = None;
        let mut keep = |result: Result<(), Error>| {
            if let Err(error) = result {
                first.get_or_insert(error);
            }
        };
        keep(self.settle().await);
        keep(self.recover_base_cleanup().await);
        if let Some((domain, _)) = self.followed_bookmark().await {
            let needs = self.seal_rebootstraps(domain).await?;
            for need in &needs {
                if need.replay == FrameReplay::Local {
                    keep(self.replay_at_seals(need.scope, false).await);
                }
            }
            if !needs.is_empty() || self.compaction_status(domain).await?.rebootstrap.is_some() {
                keep(self.finish_seal_rebootstrap(domain).await.map(drop));
            }
        }
        for scope in self.restoring().await? {
            keep(self.restored(scope).await);
        }
        keep(self.rederive().await);
        first.map_or(Ok(()), Err)
    }
    async fn recover_base_cleanup(&self) -> Result<(), Error> {
        loop {
            let mut inner = self.inner.lock(crate::replica::Class::Background).await;
            let more = inner.base_cleanup_step().await?;
            drop(inner);
            if !more {
                return Ok(());
            }
            rocky_platform::task::yield_now().await;
        }
    }
    pub async fn rederive(&self) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        if let Some(domain) = inner.workspace.typed.as_ref().map(|typed| typed.domain) {
            inner.retry_rederive(domain).await?;
        }
        Ok(())
    }
    pub async fn holds_revoked_frames(&self, scope: ResourceScope) -> Result<bool, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        if !inner.config.revocations.has_revocations() {
            return Ok(false);
        }
        for record in collect_rows(
            inner
                .conn
                .query(
                    "SELECT header,body FROM secure_operations WHERE scope=? AND status='applied'",
                    [encode(&scope)?],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else {
                return Err(Error::Corrupt("applied operation".into()));
            };
            let frame = inner.stored_frame(header.clone(), body.clone()).await?;
            if inner
                .config
                .revocations
                .revoking(frame.chain()?.credentials())
                .map_err(credential)?
                .is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    async fn restoring(&self) -> Result<Vec<ResourceScope>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        rows(&inner.conn, "SELECT scope FROM secure_restoration_guards")
            .await?
            .iter()
            .map(|record| match record.as_slice() {
                [SqlValue::Blob(scope)] => decode(scope),
                _ => Err(Error::Corrupt("restoration guard".into())),
            })
            .collect()
    }
}
