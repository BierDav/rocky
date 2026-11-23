use super::*;
use crate::testing::{
    Bookmark, BookmarkMove, LosingMove, Probe, TipAuthorMark, TipCheckpoint, TipCompaction,
    TipCoordinator, TipDevice, TipFrontierLog, TipInspection, TipNode, TipOpening, TipPruneBlock,
    TipRecovery, TipScopeFrontier, TipSeal, TipSealAction, TipStatus,
};
use rocky_core::Scope;
use rocky_credentials::SealAction;

fn hex(digest: &rocky_core::Commitment) -> String {
    HEXLOWER.encode(digest)
}

impl Node {
    pub(super) async fn probe_tips(&self, probe: Probe) {
        let scope = self.domain().scope;
        match probe {
            Probe::Status(reply) => {
                let _ = reply.send(self.tip_status(scope).await);
            }
            Probe::Inspect(reply) => {
                let _ = reply.send(self.inspect_tips(scope).await);
            }
            Probe::Seal(reply) => {
                let sealed = self.seal_followed(scope).await;
                let _ = reply.send(match sealed {
                    Ok(()) => self.tip_status(scope).await,
                    Err(error) => Err(error),
                });
            }
            Probe::Compact(keep, reply) => {
                let _ = reply.send(self.compact_tips(keep).await);
            }
            Probe::Compaction(reply) => {
                let _ = reply.send(self.compaction(scope, 0, Vec::new()).await);
            }
        }
    }

    pub(super) async fn compact_tips(&self, keep: usize) -> Result<TipCompaction, Failure> {
        let scope = self.domain().scope;
        let report = self.replica.compact(scope, keep).await?;
        self.list_checkpoints(scope).await?;
        self.replica.drop_unrequired_schemas().await?;
        self.observations.touch();
        let blocked = report
            .blocked
            .iter()
            .map(|(scope, block)| {
                Ok(TipPruneBlock {
                    scope: *scope,
                    reason: format!("{block:?}"),
                })
            })
            .collect::<Result<_, Failure>>()?;
        self.compaction(scope, report.pruned, blocked).await
    }

    async fn tip_status(&self, scope: Scope) -> Result<TipStatus, Failure> {
        let observed = self.replica.tip_observation(scope).await?;
        Ok(TipStatus {
            bookmark: observed.bookmark,
            tip: hex(&observed.tip.0),
            operations_since_seal: observed.operations_since_seal,
            evidence_releasable: None,
            devices: observed
                .devices
                .into_iter()
                .map(|device| {
                    Ok(TipDevice {
                        node: device.node.to_hex(),
                        tip: device.tip.map(|tip| hex(&tip.0)),
                        frontiers: device
                            .frontiers
                            .into_iter()
                            .map(|frontier| {
                                Ok(TipScopeFrontier {
                                    scope: frontier.scope,
                                    logs: frontier
                                        .logs
                                        .into_iter()
                                        .map(|log| TipFrontierLog {
                                            author: log.author.to_hex(),
                                            held: log.held,
                                            applied: log.applied,
                                        })
                                        .collect(),
                                })
                            })
                            .collect::<Result<_, Failure>>()?,
                    })
                })
                .collect::<Result<_, Failure>>()?,
        })
    }

    async fn compaction(
        &self,
        scope: Scope,
        pruned: u64,
        blocked: Vec<TipPruneBlock>,
    ) -> Result<TipCompaction, Failure> {
        let status = self.replica.compaction_status(scope).await?;
        Ok(TipCompaction {
            finalized: status.finalized.as_ref().map(hex),
            compacted: status.compacted.as_ref().map(hex),
            uncompacted: status.uncompacted.iter().map(hex).collect(),
            waiting: status.waiting,
            rebootstrap: status.rebootstrap.as_ref().map(hex),
            pruned,
            blocked,
        })
    }

    async fn inspect_tips(&self, scope: Scope) -> Result<TipInspection, Failure> {
        let graph = self.replica.tip_graph(scope).await?;
        let mut held = std::collections::BTreeMap::new();
        for seal in &graph.seals {
            let id = seal.id()?;
            if let Some(how) = self.replica.held_seal(scope, id).await? {
                held.insert(id, how);
            }
        }
        let seals = graph
            .seals
            .iter()
            .map(|seal| {
                let statement = &seal.statement;
                Ok(TipSeal {
                    id: hex(&seal.id()?),
                    tip: hex(&statement.tip.0),
                    frontier: statement
                        .frontier
                        .iter()
                        .map(|mark| TipAuthorMark {
                            author: mark.author,
                            sequence: mark.sequence,
                        })
                        .collect(),
                    action: match statement.action {
                        SealAction::Continue { .. } => TipSealAction::Continue,
                        SealAction::Close => TipSealAction::Close,
                    },
                    schema: match statement.action {
                        SealAction::Continue { schema } => Some(hex(&schema)),
                        SealAction::Close => None,
                    },
                    canonical: graph.seal(statement.tip)? == Some(seal),
                    authority: graph.authority(seal.endorsement()),
                    revoked: graph.revoked.contains(&seal.id()?),
                    finalized: graph.is_final(seal.id()?)?,
                    checkpoints: graph
                        .checkpoints_at(seal.id()?)?
                        .into_iter()
                        .map(|record| {
                            Ok(TipCheckpoint {
                                id: hex(&record.id()?),
                                scope: record.statement.table,
                                checkpoint: hex(&record.statement.checkpoint),
                                signer: record.statement.signer.to_hex(),
                                authority: graph.authority(record.endorsement()),
                                rows: record.statement.rows,
                            })
                        })
                        .collect::<Result<_, Failure>>()?,
                    held: held.get(&seal.id()?).copied(),
                    recovery: statement.recovery.map(|recovery| TipRecovery {
                        retained: recovery.retained.position,
                        replaced: recovery.replaced.position,
                    }),
                })
            })
            .collect::<Result<_, Failure>>()?;
        let openings = graph
            .openings
            .iter()
            .map(|opening| {
                Ok(TipOpening {
                    id: hex(&opening.id()?),
                    seal: hex(&opening.statement.seal),
                    schema: hex(&opening.statement.schema),
                    minimum_in_sync: opening.statement.minimum_in_sync,
                })
            })
            .collect::<Result<_, Failure>>()?;
        let names = graph
            .moves
            .iter()
            .map(|movement| movement.statement.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let bookmarks = names
            .into_iter()
            .map(|name| {
                Ok(Bookmark {
                    tip: graph.bookmark(&name)?.map(|tip| hex(&tip.0)),
                    moves: graph
                        .moves
                        .iter()
                        .filter(|movement| movement.statement.name == name)
                        .map(|movement| {
                            Ok(BookmarkMove {
                                id: hex(&movement.id()?),
                                from: movement.statement.from.map(|tip| hex(&tip.0)),
                                to: movement.statement.to.map(|tip| hex(&tip.0)),
                                authority: graph.authority(movement.endorsement()),
                                revoked: graph.revoked.contains(&movement.id()?),
                            })
                        })
                        .collect::<Result<_, Failure>>()?,
                    losers: graph
                        .losing_moves(&name)?
                        .into_iter()
                        .enumerate()
                        .map(|(index, movement)| {
                            Ok(LosingMove {
                                name: format!("{name}/{}", index + 1),
                                movement: hex(&movement.id()?),
                                tip: movement.statement.to.map(|tip| hex(&tip.0)),
                            })
                        })
                        .collect::<Result<_, Failure>>()?,
                    name,
                })
            })
            .collect::<Result<_, Failure>>()?;
        let nodes = self
            .replica
            .tip_nodes(scope)
            .await?
            .into_iter()
            .map(|node| TipNode {
                node: node.node.to_hex(),
                followed_tip: node.followed_tip.map(|tip| hex(&tip.0)),
                kept_tips: node
                    .kept_tips
                    .map(|tips| tips.iter().map(|tip| hex(&tip.0)).collect()),
            })
            .collect();
        Ok(TipInspection {
            scope,
            genesis: hex(&graph.genesis().0),
            schema: hex(&graph.genesis_schema),
            genesis_minimum: graph
                .line_minimum(graph.genesis())?
                .and_then(|minimum| u32::try_from(minimum).ok()),
            coordinator: graph.coordinator()?.map(|bundle| TipCoordinator {
                root: HEXLOWER.encode(&bundle.identity.root),
                genesis: hex(&bundle.identity.genesis),
                url: bundle.url.clone(),
            }),
            seals,
            openings,
            bookmarks,
            nodes,
        })
    }
}
