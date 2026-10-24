//! The complete metadata graph travels during admission, independently of tips.
use super::*;
use crate::TipStatement;

const PAGE: usize = 32;

#[derive(Serialize, Deserialize)]
pub(super) enum TipMessage {
    Digest(Commitment, usize),
    Statements {
        statements: Vec<TipStatement>,
        last: bool,
    },
    Followed(rocky_core::TipId),
    Query {
        after: Option<Commitment>,
    },
    Page {
        statements: Vec<TipStatement>,
        next: Option<Commitment>,
    },
    Push {
        statements: Vec<TipStatement>,
        last: bool,
    },
    Stored,
    CandidateQuery,
    CandidatePush {
        statements: Vec<TipStatement>,
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
        let mut after = None;
        let mut learned = Vec::new();
        loop {
            self.send(TipMessage::Query { after }).await?;
            let TipMessage::Page { statements, next } = self.reply::<TipMessage>().await? else {
                return Err(denied("unexpected tip graph response"));
            };
            let end = ordered_page(&statements, after)?;
            if next.is_some() && (statements.len() != PAGE || next != end) {
                return Err(denied("tip graph cursor outside its page"));
            }
            learned.extend(statements);
            match next {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        // A dependency may sort after every record depending on it.
        replica.receive_tip_statements(&learned).await?;
        after = None;
        loop {
            let statements = replica.tip_statements(after, PAGE).await?;
            let last = statements.len() < PAGE;
            after = ordered_page(&statements, after)?;
            self.send(TipMessage::Push { statements, last }).await?;
            if !matches!(self.reply::<TipMessage>().await?, TipMessage::Stored) {
                return Err(denied("unexpected tip graph acknowledgment"));
            }
            if last {
                break;
            }
        }
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
                let batches = replica.pending_tip_candidates().await?;
                self.send(TipMessage::CandidateCount(batches.len())).await?;
                for statements in batches {
                    self.send(TipMessage::CandidatePush { statements }).await?;
                }
                Ok(())
            }
            TipMessage::CandidatePush { statements } => {
                replica.receive_tip_statements(&statements).await?;
                self.send(TipMessage::Stored).await
            }
            TipMessage::Query { after } => {
                if !self.tip_incoming.is_empty() {
                    return Err(denied("tip graph push is incomplete"));
                }
                let statements = replica.tip_statements(after, PAGE).await?;
                let end = ordered_page(&statements, after)?;
                let next = (statements.len() == PAGE).then_some(end).flatten();
                self.send(TipMessage::Page { statements, next }).await
            }
            TipMessage::Push { statements, last } => {
                let previous = self.tip_incoming.last().map(TipStatement::id).transpose()?;
                ordered_page(&statements, previous)?;
                if !last && statements.len() != PAGE {
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
        let mut ours = Vec::new();
        let mut after = None;
        loop {
            let page = replica.tip_statements(after, PAGE).await?;
            let last = page.len() < PAGE;
            if let Some(statement) = page.last() {
                after = Some(statement.id()?);
            }
            ours.extend(page);
            if last {
                break;
            }
        }
        let ids = ours
            .iter()
            .map(TipStatement::id)
            .collect::<Result<Vec<_>, _>>()?;
        let digest = rocky_core::commitment("rocky.tip-graph.v1", &encode(&ids)?);
        let batches = replica.pending_tip_candidates().await?;
        let TipMessage::Digest(theirs, count) = self
            .swap_tips(initiator, TipMessage::Digest(digest, batches.len()))
            .await?
        else {
            return Err(denied("unexpected tip graph digest"));
        };
        if theirs != digest {
            let mut pages = ours.chunks(PAGE);
            let mut learned = Vec::new();
            let (mut sent, mut received) = (false, false);
            let mut previous = None;
            while !(sent && received) {
                let page = pages.next().unwrap_or_default();
                sent = pages.len() == 0;
                let TipMessage::Statements { statements, last } = self
                    .swap_tips(
                        initiator,
                        TipMessage::Statements {
                            statements: page.to_vec(),
                            last: sent,
                        },
                    )
                    .await?
                else {
                    return Err(denied("unexpected tip graph page"));
                };
                if statements.len() > PAGE
                    || (received && (!statements.is_empty() || !last))
                    || (!last && statements.is_empty())
                {
                    return Err(denied("tip graph page outside the exchange"));
                }
                for statement in statements {
                    let id = statement.id()?;
                    if previous.is_some_and(|previous| id <= previous) {
                        return Err(denied("tip graph statements are not ordered"));
                    }
                    previous = Some(id);
                    learned.push(statement);
                }
                received |= last;
            }
            let received_ids = learned
                .iter()
                .map(TipStatement::id)
                .collect::<Result<Vec<_>, _>>()?;
            if rocky_core::commitment("rocky.tip-graph.v1", &encode(&received_ids)?) != theirs {
                return Err(denied("tip graph digest mismatch"));
            }
            // Dependencies may sort into later pages; verify the complete union.
            replica.receive_tip_statements(&learned).await?;
        }
        let mut batches = batches.into_iter();
        for index in 0..count.max(batches.len()) {
            let statements = batches.next().unwrap_or_default();
            let TipMessage::CandidatePush { statements } = self
                .swap_tips(initiator, TipMessage::CandidatePush { statements })
                .await?
            else {
                return Err(denied("unexpected pending candidate"));
            };
            if index < count {
                replica.receive_tip_statements(&statements).await?;
            } else if !statements.is_empty() {
                return Err(denied("candidate beyond declared count"));
            }
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
        for _ in 0..count {
            let TipMessage::CandidatePush { statements } = self.reply::<TipMessage>().await? else {
                return Err(denied("unexpected pending candidate"));
            };
            replica.receive_tip_statements(&statements).await?;
        }
        for statements in replica.pending_tip_candidates().await? {
            self.send(TipMessage::CandidatePush { statements }).await?;
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
