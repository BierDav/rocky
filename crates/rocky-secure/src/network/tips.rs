use super::*;
use crate::TipStatement;

const PAGE: usize = 32;
mod diff;
use diff::{graph_statements, ids_digest, range_ids};

#[derive(Serialize, Deserialize)]
pub(super) enum TipMessage {
    Digest(#[serde(with = "rocky_core::byte_arrays::one")] Commitment),
    Statements {
        statements: Vec<TipStatement>,
        last: bool,
    },
    Followed(rocky_core::TipId),
    Range {
        #[serde(with = "rocky_core::byte_arrays::one")]
        prefix: Commitment,
        depth: u16,
    },
    Summary {
        #[serde(with = "rocky_core::byte_arrays::one")]
        digest: Commitment,
        count: usize,
    },
    Ids {
        #[serde(with = "rocky_core::byte_arrays::one")]
        prefix: Commitment,
        depth: u16,
    },
    Identifiers(#[serde(with = "rocky_core::byte_arrays::each")] Vec<Commitment>),
    Fetch {
        #[serde(with = "rocky_core::byte_arrays::each")]
        ids: Vec<Commitment>,
    },
    Push {
        statements: Vec<TipStatement>,
        last: bool,
    },
    Stored,
}

impl NativeSession {
    pub async fn follows_same_tip<A: RowApplication>(&self, replica: &EncryptedReplica<A>) -> bool {
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        inner.peer_tip(self.peer) == Some(inner.workspace.tip)
    }

    pub async fn sync_tip_graph<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        self.reconcile_graph(replica, None).await?;
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
        let TipMessage::Digest(theirs) = self
            .swap_tips(initiator, TipMessage::Digest(digest))
            .await?
        else {
            return Err(denied("unexpected tip graph digest"));
        };
        if theirs != digest {
            self.reconcile_graph(replica, Some(initiator)).await?;
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
        replica.admit_peer(self.peer, &self.credentials, tip).await
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
