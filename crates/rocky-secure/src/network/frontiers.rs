//! Bounded group frontier exchange; data and horizon repair retain their scope APIs.
use super::*;

use crate::replica::roles::{MAX_FRONTIER_SCOPES, validate_frontiers};

impl NativeSession {
    /// Exchanges all selected member frontiers of each group partition together.
    /// Returns scopes needing ordinary pull/push or horizon repair. Equal, fully
    /// applied frontiers with equal horizons need no row transfer this round.
    pub async fn changed_scopes<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: &[ResourceScope],
    ) -> Result<Vec<ResourceScope>, Error> {
        let mut changed = Vec::new();
        for batch in replica
            .frontier_batches(scopes, MAX_FRONTIER_SCOPES)
            .await?
        {
            let mut frontiers = Vec::new();
            for scope in &batch {
                frontiers.push(replica.frontier(*scope).await?);
            }
            validate_frontiers(&frontiers)?;
            self.send(&Message::FrontiersQuery {
                frontiers: frontiers.clone(),
            })
            .await?;
            let Message::Frontiers { states } = self.receive().await? else {
                return Err(denied("unexpected frontier batch response"));
            };
            if states.len() != batch.len()
                || states.iter().zip(&batch).any(|(state, scope)| {
                    state.frontier.scope != *scope
                        || state
                            .signed
                            .as_ref()
                            .is_some_and(|signed| signed.statement.scope != *scope)
                })
            {
                return Err(denied("frontier batch outside request"));
            }
            let remote: Vec<_> = states.iter().map(|state| state.frontier.clone()).collect();
            let newer = {
                let mut inner = replica.inner.lock().await;
                let newer = self
                    .credentials
                    .0
                    .first()
                    .is_some_and(|root| inner.known(root.capability.manifest).is_err());
                inner
                    .record_frontiers(self.peer, &self.credentials, &remote)
                    .await?;
                newer
            };
            for (local, state) in frontiers.iter().zip(states) {
                if newer
                    || local != &state.frontier
                    || local.logs.iter().any(|log| log.held != log.applied)
                    || state.behind
                    || replica.horizon_rebootstrap_required(local.scope).await?
                    || replica.admitted_horizon(local.scope).await? != state.signed
                {
                    changed.push(local.scope);
                }
            }
        }
        Ok(scopes
            .iter()
            .copied()
            .filter(|scope| changed.contains(scope))
            .collect())
    }
}
