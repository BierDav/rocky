//! Reconcile sorted statement digests before transferring missing metadata.
use super::*;
use crate::TipStatement;

const PAGE: usize = 32;
mod diff;
use diff::{graph_statements, ids_digest, range_ids};

#[derive(Serialize, Deserialize)]
pub(super) enum TipMessage {
    Digest(Commitment, usize),
    Statements {
        statements: Vec<TipStatement>,
        last: bool,
    },
    Followed(rocky_core::TipId),
    Range {
        prefix: Commitment,
        depth: u16,
    },
    Summary {
        digest: Commitment,
        count: usize,
    },
    Ids {
        prefix: Commitment,
        depth: u16,
    },
    Identifiers(Vec<Commitment>),
    Fetch {
        ids: Vec<Commitment>,
    },
    Push {
        statements: Vec<TipStatement>,
        last: bool,
    },
    Stored,
    CandidateQuery,
    CandidatePush {
        statements: Vec<TipStatement>,
        last: bool,
    },
    CandidateCount(usize),
}

impl NativeSession {
    /// Whether this admitted peer follows the local workspace's tip.
    pub async fn follows_same_tip<A: RowApplication>(&self, replica: &EncryptedReplica<A>) -> bool {
        let inner = replica.inner.lock().await;
        inner.peer_tip(self.peer) == Some(inner.workspace.tip)
    }

    /// Refreshes the graph on a request session of an already admitted link.
    pub async fn sync_tip_graph<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        self.reconcile_graph(replica, None).await?;
        self.sync_candidates(replica).await?;
        self.send(TipMessage::Followed(replica.followed_tip().await))
            .await?;
        let TipMessage::Followed(tip) = self.reply::<TipMessage>().await? else {
            return Err(denied("unexpected followed tip response"));
        };
        replica.admit_peer(self.peer, &self.credentials, tip).await
    }

    pub(super) async fn answer_tips<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: TipMessage,
    ) -> Result<(), Error> {
        match message {
            TipMessage::CandidateQuery => {
                let batches = candidate_pages(replica.pending_tip_candidates().await?)?;
                self.send(TipMessage::CandidateCount(batches.len())).await?;
                for (statements, last) in batches {
                    self.send(TipMessage::CandidatePush { statements, last })
                        .await?;
                }
                Ok(())
            }
            TipMessage::CandidatePush { statements, last } => {
                if statements.len() > PAGE || (!last && statements.is_empty()) {
                    return Err(denied("candidate page quota"));
                }
                self.tip_incoming.extend(statements);
                if last {
                    let batch = std::mem::take(&mut self.tip_incoming);
                    replica.receive_tip_statements(&batch).await?;
                }
                self.send(TipMessage::Stored).await
            }
            TipMessage::Range { prefix, depth } => {
                let graph = graph_statements(replica).await?;
                let ids = range_ids(&graph, prefix, depth)?;
                self.send(TipMessage::Summary {
                    digest: ids_digest(&ids)?,
                    count: ids.len(),
                })
                .await
            }
            TipMessage::Ids { prefix, depth } => {
                let graph = graph_statements(replica).await?;
                let ids = range_ids(&graph, prefix, depth)?;
                if ids.len() > PAGE {
                    return Err(denied("tip identifiers quota"));
                }
                self.send(TipMessage::Identifiers(ids)).await
            }
            TipMessage::Fetch { ids } => {
                if ids.len() > PAGE || ids.windows(2).any(|p| p[0] >= p[1]) {
                    return Err(denied("tip fetch quota or order"));
                }
                let graph = graph_statements(replica).await?;
                let statements = graph
                    .into_iter()
                    .filter(|(id, _)| ids.contains(id))
                    .map(|(_, s)| s)
                    .collect();
                let mut pages = statement_pages(statements)?;
                if pages.is_empty() {
                    pages.push(Vec::new());
                }
                let count = pages.len();
                for (i, statements) in pages.into_iter().enumerate() {
                    self.send(TipMessage::Statements {
                        statements,
                        last: i + 1 == count,
                    })
                    .await?;
                }
                Ok(())
            }
            TipMessage::Push { statements, last } => {
                let previous = self.tip_incoming.last().map(TipStatement::id).transpose()?;
                ordered_page(&statements, previous)?;
                if !last && statements.is_empty() {
                    return Err(denied("incomplete tip graph push page"));
                }
                self.tip_incoming.extend(statements);
                if last {
                    let statements = std::mem::take(&mut self.tip_incoming);
                    replica.receive_tip_statements(&statements).await?;
                }
                self.send(TipMessage::Stored).await
            }
            TipMessage::Followed(tip) => {
                if !self.tip_incoming.is_empty() {
                    return Err(denied("tip graph push is incomplete"));
                }
                replica
                    .admit_peer(self.peer, &self.credentials, tip)
                    .await?;
                self.send(TipMessage::Followed(replica.followed_tip().await))
                    .await
            }
            _ => Err(denied("unexpected tip graph request")),
        }
    }

    pub(super) async fn exchange_tips<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        initiator: bool,
    ) -> Result<(), Error> {
        let graph = graph_statements(replica).await?;
        let ids: Vec<_> = graph.iter().map(|(id, _)| *id).collect();
        let digest = ids_digest(&ids)?;
        let batches = candidate_pages(replica.pending_tip_candidates().await?)?;
        let TipMessage::Digest(theirs, count) = self
            .swap_tips(initiator, TipMessage::Digest(digest, batches.len()))
            .await?
        else {
            return Err(denied("unexpected tip graph digest"));
        };
        if theirs != digest {
            self.reconcile_graph(replica, Some(initiator)).await?;
        }
        let mut batches = batches.into_iter();
        let mut candidate = Vec::new();
        for index in 0..count.max(batches.len()) {
            let (statements, last) = batches.next().unwrap_or((Vec::new(), true));
            let TipMessage::CandidatePush { statements, last } = self
                .swap_tips(initiator, TipMessage::CandidatePush { statements, last })
                .await?
            else {
                return Err(denied("unexpected pending candidate"));
            };
            if index < count {
                if statements.len() > PAGE || (!last && statements.is_empty()) {
                    return Err(denied("candidate page quota"));
                }
                candidate.extend(statements);
                if last {
                    replica
                        .receive_tip_statements(&std::mem::take(&mut candidate))
                        .await?;
                }
            } else if !statements.is_empty() || !last {
                return Err(denied("candidate beyond declared count"));
            }
        }
        if !candidate.is_empty() {
            return Err(denied("incomplete pending candidate"));
        }
        let TipMessage::Followed(tip) = self
            .swap_tips(
                initiator,
                TipMessage::Followed(replica.followed_tip().await),
            )
            .await?
        else {
            return Err(denied("unexpected followed tip"));
        };
        // A received move may have changed either tip since the signed hello.
        replica.admit_peer(self.peer, &self.credentials, tip).await
    }

    async fn sync_candidates<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        self.send(TipMessage::CandidateQuery).await?;
        let TipMessage::CandidateCount(count) = self.reply::<TipMessage>().await? else {
            return Err(denied("unexpected pending candidate count"));
        };
        let mut candidate = Vec::new();
        for _ in 0..count {
            let TipMessage::CandidatePush { statements, last } = self.reply::<TipMessage>().await?
            else {
                return Err(denied("unexpected pending candidate"));
            };
            if statements.len() > PAGE || (!last && statements.is_empty()) {
                return Err(denied("candidate page quota"));
            }
            candidate.extend(statements);
            if last {
                replica
                    .receive_tip_statements(&std::mem::take(&mut candidate))
                    .await?;
            }
        }
        if !candidate.is_empty() {
            return Err(denied("incomplete pending candidate"));
        }
        for (statements, last) in candidate_pages(replica.pending_tip_candidates().await?)? {
            self.send(TipMessage::CandidatePush { statements, last })
                .await?;
            if !matches!(self.reply::<TipMessage>().await?, TipMessage::Stored) {
                return Err(denied("unexpected pending candidate acknowledgment"));
            }
        }
        Ok(())
    }

    async fn swap_tips(
        &mut self,
        initiator: bool,
        message: TipMessage,
    ) -> Result<TipMessage, Error> {
        if initiator {
            self.send(message).await?;
            self.reply::<TipMessage>().await
        } else {
            let reply = self.reply::<TipMessage>().await?;
            self.send(message).await?;
            Ok(reply)
        }
    }
}

fn ordered_page(
    statements: &[TipStatement],
    after: Option<Commitment>,
) -> Result<Option<Commitment>, Error> {
    if statements.len() > PAGE {
        return Err(denied("tip graph page quota"));
    }
    let mut previous = after;
    for statement in statements {
        let id = statement.id()?;
        if previous.is_some_and(|known| id <= known) {
            return Err(denied("tip graph statements are not ordered"));
        }
        previous = Some(id);
    }
    Ok(previous)
}

pub(super) fn statement_pages(
    statements: Vec<TipStatement>,
) -> Result<Vec<Vec<TipStatement>>, Error> {
    let mut pages = Vec::new();
    let mut remaining = statements.as_slice();
    while !remaining.is_empty() {
        let count = page_len(remaining, PAGE)?;
        pages.push(remaining[..count].to_vec());
        remaining = &remaining[count..];
    }
    Ok(pages)
}
pub(super) fn candidate_pages(
    batches: Vec<Vec<TipStatement>>,
) -> Result<Vec<(Vec<TipStatement>, bool)>, Error> {
    let mut result = Vec::new();
    for batch in batches {
        let pages = statement_pages(batch)?;
        let length = pages.len();
        result.extend(
            pages
                .into_iter()
                .enumerate()
                .map(|(i, page)| (page, i + 1 == length)),
        );
    }
    Ok(result)
}
