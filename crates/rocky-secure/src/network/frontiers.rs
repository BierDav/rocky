use super::*;

use crate::replica::roles::{FrontierReply, MAX_FRONTIER_SCOPES, validate_frontiers};

#[derive(Serialize, Deserialize)]
pub(super) enum FrontierMessage {
    FrontiersQuery {
        frontiers: Vec<ScopeFrontier>,
        partitions: Vec<crate::replica::PartitionOffer>,
        proofs: Vec<(ResourceScope, Vec<holdings::HoldingProof>)>,
    },
    Frontiers {
        states: Vec<FrontierReply>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrontierExchange {
    pub changed: Vec<ResourceScope>,
    pub refused: Vec<ResourceScope>,
    pub importing: Vec<ResourceScope>,
}

impl NativeSession {
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
            let held = replica.held_of(&requested).await?;
            let batch = replica
                .peer_holds(self.peer, &self.credentials, &held)
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
                let mut unheld = Vec::new();
                let held: Vec<_> = frontiers
                    .iter()
                    .zip(states)
                    .filter_map(|(local, reply)| match reply {
                        FrontierReply::Held(state) => {
                            self.offered.insert(local.scope);
                            Some((local, state))
                        }
                        FrontierReply::Refused(scope) => {
                            unheld.push(scope);
                            None
                        }
                    })
                    .collect();
                replica.peer_holds_none_of(&unheld).await?;
                refused.extend(unheld);

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
                    let needs_rows = local != &state.frontier
                        || local.logs.iter().any(|log| log.held != log.applied);
                    if state.importing {
                        importing.push(local.scope);
                    } else if needs_rows {
                        changed.push(local.scope);
                    } else {
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
    partitions: &[crate::replica::PartitionOffer],
    proofs: &[(ResourceScope, Vec<holdings::HoldingProof>)],
) -> Result<FrontierMessage, Error> {
    loop {
        let scopes: Vec<_> = frontiers.iter().map(|frontier| frontier.scope).collect();
        let query = FrontierMessage::FrontiersQuery {
            frontiers: frontiers.clone(),
            partitions: partitions
                .iter()
                .filter(|(scope, ..)| scopes.contains(scope))
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
