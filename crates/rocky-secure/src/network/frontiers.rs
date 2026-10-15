//! Bounded group frontier exchange; data and horizon repair retain their scope APIs.
use super::*;

use crate::replica::roles::{FrontierReply, MAX_FRONTIER_SCOPES, validate_frontiers};

#[derive(Serialize, Deserialize)]
pub(super) enum FrontierMessage {
    FrontiersQuery {
        frontiers: Vec<ScopeFrontier>,
        /// Tuples offered to a keyless custody peer; see decision 0045.
        partitions: Vec<(ResourceScope, rocky_core::PartitionTuple)>,
        proofs: Vec<(ResourceScope, Vec<holdings::HoldingProof>)>,
    },
    Frontiers {
        states: Vec<FrontierReply>,
    },
}

/// The outcome of one frontier exchange, each list in input order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrontierExchange {
    /// Scopes needing ordinary pull/push or horizon repair.
    pub changed: Vec<ResourceScope>,
    /// Scopes skipped this session: this replica's view of the peer's
    /// credentials, or the peer's view of this replica's, grants no whole-scope
    /// read or custody. A peer that has not learned a partition refuses it.
    pub refused: Vec<ResourceScope>,
    /// Scopes the peer imports from a checkpoint: no rows move this round,
    /// so history does not crowd out the import.
    pub importing: Vec<ResourceScope>,
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
        let mut importing = Vec::new();
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
                .tuple_proofs(self.peer, &self.credentials, &batch)
                .await?;
            self.send(FrontierMessage::FrontiersQuery {
                frontiers: frontiers.clone(),
                partitions,
                proofs: replica.holding_proofs(self.binding, &batch).await?,
            })
            .await?;
            let FrontierMessage::Frontiers { states } = self.reply::<FrontierMessage>().await?
            else {
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
                    FrontierReply::Held(state) => {
                        self.offered.insert(local.scope);
                        Some((local, state))
                    }
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
            replica
                .inner
                .lock()
                .await
                .record_frontiers(self.peer, &self.credentials, &remote)
                .await?;
            for (local, state) in held {
                if state.importing {
                    importing.push(local.scope);
                } else if local != &state.frontier
                    || local.logs.iter().any(|log| log.held != log.applied)
                    || state.behind
                    || replica.horizon_rebootstrap_required(local.scope).await?
                    || replica.admitted_horizon(local.scope).await? != state.signed
                {
                    changed.push(local.scope);
                } else {
                    // Nothing to pull: a resubscribed scope is complete again.
                    replica.restored(local.scope).await?;
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
            importing: order(&importing),
        })
    }
    pub(super) async fn answer_frontiers<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: FrontierMessage,
    ) -> Result<(), Error> {
        let FrontierMessage::FrontiersQuery {
            frontiers,
            partitions,
            proofs,
        } = message
        else {
            return Err(denied("unexpected native request"));
        };
        let scopes: Vec<_> = frontiers.iter().map(|frontier| frontier.scope).collect();
        validate_frontiers(&frontiers)?;
        for frontier in &frontiers {
            frontier.validate()?;
        }
        self.accept_proofs(replica, &scopes, &proofs, &partitions)
            .await?;
        let mut allowed = Vec::new();
        for frontier in &frontiers {
            if self.require_holding(replica, frontier.scope).await.is_ok() {
                allowed.push(frontier.clone());
            }
        }
        let replies = if allowed.is_empty() {
            // Even an entirely unproved batch must verify current credentials.
            replica.admit_chain(self.peer, &self.credentials).await?;
            Vec::new()
        } else {
            replica
                .exchange_frontiers(self.peer, &self.credentials, &allowed)
                .await?
        };
        let mut replies: std::collections::BTreeMap<_, _> = replies
            .into_iter()
            .map(|reply| (reply.scope(), reply))
            .collect();
        let states = scopes
            .iter()
            .map(|scope| {
                replies
                    .remove(scope)
                    .unwrap_or(FrontierReply::Refused(*scope))
            })
            .collect();
        self.send(FrontierMessage::Frontiers { states }).await
    }
}
