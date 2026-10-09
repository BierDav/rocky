//! Bounded group frontier exchange; data and horizon repair retain their scope APIs.
use super::*;

use crate::replica::roles::{FrontierReply, MAX_FRONTIER_SCOPES, validate_frontiers};

/// The outcome of one frontier exchange, each list in input order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrontierExchange {
    /// Scopes needing ordinary pull/push or horizon repair.
    pub changed: Vec<ResourceScope>,
    /// Scopes skipped this session: this replica's view of the peer's
    /// credentials, or the peer's view of this replica's, grants no whole-scope
    /// read or custody. A peer that has not learned a partition refuses it.
    pub refused: Vec<ResourceScope>,
}

impl NativeSession {
    /// Exchanges all selected member frontiers of each group partition together.
    /// Equal, fully applied frontiers with equal horizons need no row transfer
    /// this round. A refused member is skipped alone; the session goes on.
    pub async fn changed_scopes<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: &[ResourceScope],
    ) -> Result<FrontierExchange, Error> {
        let mut changed = Vec::new();
        let mut refused = Vec::new();
        for requested in replica
            .frontier_batches(scopes, MAX_FRONTIER_SCOPES)
            .await?
        {
            // A frontier is metadata: a peer outside a scope never sees it.
            let batch = replica
                .peer_holds(self.peer, &self.credentials, &requested)
                .await?;
            refused.extend(requested.iter().filter(|scope| !batch.contains(scope)));
            if batch.is_empty() {
                continue;
            }
            let mut frontiers = Vec::new();
            for scope in &batch {
                frontiers.push(replica.frontier(*scope).await?);
            }
            validate_frontiers(&frontiers)?;
            let partitions = replica
                .custody_offers(self.peer, &self.credentials, &batch)
                .await?;
            self.send(&Message::FrontiersQuery {
                frontiers: frontiers.clone(),
                partitions,
            })
            .await?;
            let Message::Frontiers { states } = self.receive().await? else {
                return Err(denied("unexpected frontier batch response"));
            };
            if states.len() != batch.len()
                || states.iter().zip(&batch).any(|(reply, scope)| {
                    reply.scope() != *scope
                        || matches!(reply, FrontierReply::Held(state) if state
                            .signed
                            .as_ref()
                            .is_some_and(|signed| signed.statement.scope != *scope))
                })
            {
                return Err(denied("frontier batch outside request"));
            }
            let held: Vec<_> = frontiers
                .iter()
                .zip(states)
                .filter_map(|(local, reply)| match reply {
                    FrontierReply::Held(state) => Some((local, state)),
                    FrontierReply::Refused(scope) => {
                        refused.push(scope);
                        None
                    }
                })
                .collect();
            if held.is_empty() {
                continue;
            }
            let remote: Vec<_> = held
                .iter()
                .map(|(_, state)| state.frontier.clone())
                .collect();
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
            for (local, state) in held {
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
        let order = |found: &[ResourceScope]| -> Vec<ResourceScope> {
            scopes
                .iter()
                .copied()
                .filter(|scope| found.contains(scope))
                .collect()
        };
        Ok(FrontierExchange {
            changed: order(&changed),
            refused: order(&refused),
        })
    }
}
