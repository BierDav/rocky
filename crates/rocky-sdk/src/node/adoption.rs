//! Adopting the newer generation another device runs, whatever schema the
//! application declares. See `docs/runtime/adopting-generations.md`.
use super::*;

impl Node {
    /// After a sync found `peer` on a newer generation: fetches, verifies
    /// and saves it, then switches the replica to it before any of its
    /// frames apply. Returns whether the database moved.
    pub(super) async fn adopt_newer(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
    ) -> Result<bool, Failure> {
        let addresses = contact.addresses(transport.kind());
        if addresses.is_empty() {
            return Ok(false);
        }
        let mut settings = self.settings.lock().await;
        let mut draft = (**settings).clone();
        if !draft
            .discover(&self.path, &self.vault, transport, peer, &addresses)
            .await?
        {
            return Ok(false);
        }
        // Until the replica switched, this node keeps the settings it ran
        // with; the saved ones reopen at the new generation after a crash,
        // and the next sync tries again after a failure.
        let domain = draft.domain()?.hosted(self.domain().app.host());
        let config = draft.config(&self.vault, &domain)?;
        let upgrade = draft
            .upgrade(&domain)?
            .ok_or("an adopted generation has a lineage")?;
        self.replica
            .adopt_generation(domain.app.clone(), config, upgrade)
            .await?;
        crate::database::attach(&self.replica, &mut draft, &self.vault, &domain, &self.path)
            .await?;
        crate::database::retain(&self.replica, &draft, &domain).await?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = draft.granted(&domain);
        let generation = domain.manifest();
        *self.domain.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(domain);
        *settings = Shared::new(draft);
        drop(settings);
        self.activity.record(
            "generation_adopted",
            json!({"peer": peer.to_hex(), "schema": generation.schema, "handler": generation.handler}),
        );
        self.observations.touch();
        Ok(true)
    }
    /// Once per peer and generation: keeps the earlier transitions a peer
    /// at this generation holds, so every device ends on the earliest
    /// baseline. Returns whether one changed.
    pub(super) async fn compare_baselines(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
    ) -> Result<bool, Failure> {
        let addresses = contact.addresses(transport.kind());
        let manifest = {
            let settings = self.settings.lock().await;
            if settings.lineage.is_empty() || addresses.is_empty() {
                return Ok(false);
            }
            settings.manifest.clone()
        };
        if !self
            .compared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((peer, manifest))
        {
            return Ok(false);
        }
        let reply = crate::generation::ask_bytes(transport, &addresses).await?;
        let mut settings = self.settings.lock().await;
        let Some((draft, transitions)) = settings.earlier_baselines(&reply)? else {
            return Ok(false);
        };
        draft.save(&self.path)?;
        *settings = Shared::new(draft);
        drop(settings);
        for transition in &transitions {
            self.replica.learn_baseline(transition).await?;
        }
        Ok(true)
    }
    /// After a sync found `peer` under a generation this replica lacks and
    /// no newer one: fetches the peer's generations and records the older
    /// ones that lead to a generation the replica knows, so the peer's
    /// frames apply. Returns whether one was learned.
    pub(super) async fn learn_older(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
    ) -> Result<bool, Failure> {
        let addresses = contact.addresses(transport.kind());
        if addresses.is_empty() {
            return Ok(false);
        }
        let reply = crate::generation::ask_bytes(transport, &addresses).await?;
        let steps = self.settings.lock().await.older_steps(&reply)?;
        let learned = self.replica.learn_generations(&steps).await?;
        if learned > 0 {
            self.activity.record(
                "generation_learned",
                json!({"peer": peer.to_hex(), "generations": learned}),
            );
        }
        Ok(learned > 0)
    }
}
