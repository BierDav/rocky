//! Bounded group frontier exchange; data and checkpoint repair retain their scope APIs.
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
    /// Scopes needing ordinary pull/push or checkpoint repair.
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
    /// Equal, fully applied frontiers need no row transfer
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
            let mut remaining = batch.as_slice();
            while !remaining.is_empty() {
                let mut frontiers = Vec::new();
                for scope in remaining {
                    frontiers.push(replica.frontier(*scope).await?);
                }
                validate_frontiers(&frontiers)?;
                let partitions = replica
                    .tuple_proofs(self.peer, &self.credentials, remaining)
                    .await?;
                let proofs = replica.holding_proofs(self.binding, remaining).await?;
                let query = frontier_query(frontiers, &partitions, &proofs)?;
                let FrontierMessage::FrontiersQuery {
                    frontiers: selected,
                    ..
                } = &query
                else {
                    return Err(denied("frontier query"));
                };
                let mut frontiers = selected.clone();
                self.send(query).await?;
                let FrontierMessage::Frontiers { states } = self.reply::<FrontierMessage>().await?
                else {
                    return Err(denied("unexpected frontier batch response"));
                };
                if states.is_empty()
                    || states.len() > frontiers.len()
                    || states
                        .iter()
                        .zip(&frontiers)
                        .any(|(reply, frontier)| reply.scope() != frontier.scope)
                {
                    return Err(denied("frontier batch outside request"));
                }
                remaining = &remaining[states.len()..];
                frontiers.truncate(states.len());
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

                let remote: Vec<_> = held
                    .iter()
                    .map(|(_, state)| state.frontier.clone())
                    .collect();
                replica
                    .inner
                    .lock(crate::replica::Class::Background)
                    .await
                    .record_frontiers(self.peer, &self.credentials, &remote)
                    .await?;
                for (local, state) in held {
                    if state.importing {
                        importing.push(local.scope);
                    } else if local != &state.frontier
                        || local.logs.iter().any(|log| log.held != log.applied)
                    {
                        changed.push(local.scope);
                    } else {
                        // Nothing to pull: a resubscribed scope is complete again.
                        replica.restored(local.scope).await?;
                    }
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
        let mut states: Vec<_> = scopes
            .iter()
            .map(|scope| {
                replies
                    .remove(scope)
                    .unwrap_or(FrontierReply::Refused(*scope))
            })
            .collect();
        let count = page_len(&states, MAX_FRONTIER_SCOPES)?;
        states.truncate(count);
        self.send(FrontierMessage::Frontiers { states }).await
    }
}

pub(super) fn frontier_query(
    mut frontiers: Vec<ScopeFrontier>,
    partitions: &[(ResourceScope, rocky_core::PartitionTuple)],
    proofs: &[(ResourceScope, Vec<holdings::HoldingProof>)],
) -> Result<FrontierMessage, Error> {
    loop {
        let scopes: Vec<_> = frontiers.iter().map(|frontier| frontier.scope).collect();
        let query = FrontierMessage::FrontiersQuery {
            frontiers: frontiers.clone(),
            partitions: partitions
                .iter()
                .filter(|(scope, _)| scopes.contains(scope))
                .cloned()
                .collect(),
            proofs: proofs
                .iter()
                .filter(|(scope, _)| scopes.contains(scope))
                .cloned()
                .collect(),
        };
        if encode(&query)?.len() <= SECURE_MESSAGE_BYTES {
            return Ok(query);
        }
        if frontiers.len() == 1 {
            return Err(Error::Quota {
                resource: "frontier",
                limit: SECURE_MESSAGE_BYTES,
            });
        }
        frontiers.pop();
    }
}
