//! Retaining and comparing the schemas of the locally installed generation.
use super::*;

impl Node {
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
    /// Drops the manifests of earlier generations nothing this replica
    /// retains names any longer, in the replica and then in the saved
    /// lineage; their transitions stay. Runs after a horizon finalized,
    /// when retained frames go.
    pub(super) async fn forget_generations(&self) -> Result<(), Failure> {
        let dropped = self.replica.drop_unreferenced_generations().await?;
        let mut settings = self.settings.lock().await;
        let mut draft = (**settings).clone();
        // Also catches up on a drop whose save a crash interrupted.
        let held: std::collections::BTreeSet<_> = self
            .replica
            .generations()
            .await?
            .previous
            .into_iter()
            .map(|(_, manifest)| manifest)
            .collect();
        let unheld: Vec<_> = draft
            .lineage
            .iter()
            .filter(|step| step.manifest.is_some())
            .map(|step| step.verified(draft.publisher).map(|step| step.from))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|from| !held.contains(from))
            .collect();
        if draft.forget_manifests(&unheld)? {
            draft.save(&self.path)?;
            *settings = Shared::new(draft);
        }
        drop(settings);
        if !dropped.is_empty() {
            self.activity
                .record("generation_dropped", json!({"generations": dropped.len()}));
        }
        Ok(())
    }
}
