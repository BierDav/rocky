use super::*;
use crate::{
    membership::Membership,
    registry::{self, Changes, Registry},
};

impl Node {
    pub(crate) async fn settings_snapshot(&self) -> Shared<Settings> {
        self.settings.lock().await.clone()
    }
    pub(crate) async fn registry(&self, settings: &Settings) -> Result<Registry, Failure> {
        if !settings.issuer() {
            return Ok(Registry::default());
        }
        Box::pin(registry::read(&self.replica, self.domain().registry())).await
    }
    pub(crate) async fn membership(&self) -> Result<Membership, Failure> {
        let mut known = self.replica.revocations().await;
        let settings = self.settings.lock().await;
        known.merge(
            &settings.revocations,
            settings.publisher,
            self.domain().manifest().app,
            now()?,
        )?;
        if let Some(certificate) = settings.credentials.certificate() {
            known.link(certificate.clone())?;
        }
        Membership::replicated(&known, settings.database_context()?)
    }
    pub(super) async fn see(&self, identity: PublicKey, node: PublicKey) -> Result<(), Failure> {
        let time = now()?;
        self.configure(|settings| {
            if let Some(issuing) = settings.authority.as_mut() {
                crate::membership::see(&mut issuing.seen, identity, node, time);
            }
            Ok(())
        })
        .await
    }

    pub(super) async fn commit(&self, mut changes: Changes) -> Result<(), Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let own = self.settings.lock().await.own_record(&registry)?;
        changes.extend(own);
        Box::pin(registry::write(
            &self.replica,
            self.domain().registry(),
            &changes,
        ))
        .await
    }
    pub(super) async fn record_self(&self) -> Result<(), Failure> {
        if !self.settings.lock().await.issuer() {
            return Ok(());
        }
        self.commit(Changes::default()).await
    }

    pub(super) async fn answer_renewal(&self, request: &[u8]) -> Result<Vec<u8>, Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let settings = self.settings.lock().await.clone();
        let (reply, changes) = settings.renew(&self.vault, request, &registry)?;
        self.commit(changes).await?;
        let renewed = rocky_credentials::RenewRequest::from_bytes(request)?;
        if let Some(identity) = renewed.chain.principal() {
            self.see(identity, renewed.node).await?;
        }
        crate::delivery::fault::pause(
            Settings::node_key(&self.vault)?.public_key(),
            crate::delivery::fault::Stage::Renewing,
        )
        .await;
        let settings = self.settings_snapshot().await;
        let registry = self.registry(&settings).await?;
        if settings.tombstoned(&renewed.node)
            || renewed
                .chain
                .principal()
                .is_some_and(|identity| settings.revoked_identity(&identity, &registry))
        {
            return Err("device was revoked".into());
        }
        Ok(reply)
    }

    pub(super) async fn record_revocation(&self, identity: PublicKey) -> Result<(), Failure> {
        let mut changes = Changes::default();
        changes.revoked(&identity)?;
        self.commit(changes).await
    }

    pub(crate) async fn complete_revocation(&self) -> Result<(), Failure> {
        let pending = self
            .settings
            .lock()
            .await
            .authority
            .as_ref()
            .and_then(|authority| {
                authority
                    .rotation
                    .as_ref()
                    .map(|rotation| rotation.identities.clone())
            });
        let Some(identities) = pending else {
            return Ok(());
        };
        for identity in identities {
            self.record_revocation(identity).await?;
        }
        crate::enrollment::fault::check(crate::enrollment::fault::Stage::Committed)?;
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let membership = self.membership().await?;
        self.configure(|settings| {
            settings.complete_revocation(&self.vault, &self.domain(), (&registry, &membership))
        })
        .await
    }
}
