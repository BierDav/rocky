//! Binary range digests identify the statements missing on either side.
use super::*;

impl NativeSession {
    /// Fixed binary ranges give both sides the same subdivision even when
    /// their sets differ. Dependencies are admitted only after the full diff.
    pub(super) async fn reconcile_graph<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        handshake: Option<bool>,
    ) -> Result<(), Error> {
        let graph = graph_statements(replica).await?;
        let mut ranges = vec![([0; 32], 0u16)];
        let mut learned = Vec::new();
        let mut pushed = false;
        while let Some((prefix, depth)) = ranges.pop() {
            let ids = range_ids(&graph, prefix, depth)?;
            let digest = ids_digest(&ids)?;
            let response = match handshake {
                Some(initiator) => {
                    self.swap_tips(
                        initiator,
                        TipMessage::Summary {
                            digest,
                            count: ids.len(),
                        },
                    )
                    .await?
                }
                None => {
                    self.send(TipMessage::Range { prefix, depth }).await?;
                    self.reply::<TipMessage>().await?
                }
            };
            let TipMessage::Summary {
                digest: theirs,
                count,
            } = response
            else {
                return Err(denied("unexpected tip range digest"));
            };
            if digest == theirs {
                continue;
            }
            if ids.len().max(count) > PAGE {
                if depth >= 256 {
                    return Err(denied("tip range count mismatch"));
                }
                let mut upper = prefix;
                upper[usize::from(depth / 8)] |= 128 >> (depth % 8);
                ranges.push((upper, depth + 1));
                ranges.push((prefix, depth + 1));
                continue;
            }
            let response = match handshake {
                Some(initiator) => {
                    self.swap_tips(initiator, TipMessage::Identifiers(ids.clone()))
                        .await?
                }
                None => {
                    self.send(TipMessage::Ids { prefix, depth }).await?;
                    self.reply::<TipMessage>().await?
                }
            };
            let TipMessage::Identifiers(remote) = response else {
                return Err(denied("unexpected tip identifiers"));
            };
            if remote.len() != count
                || remote.windows(2).any(|p| p[0] >= p[1])
                || remote.iter().any(|id| !in_range(id, &prefix, depth))
                || ids_digest(&remote)? != theirs
            {
                return Err(denied("tip identifiers outside range"));
            }
            let wanted: Vec<_> = remote
                .iter()
                .filter(|id| !ids.contains(id))
                .copied()
                .collect();
            let outgoing: Vec<_> = graph
                .iter()
                .filter(|(id, _)| ids.contains(id) && !remote.contains(id))
                .map(|(_, s)| s.clone())
                .collect();
            if let Some(initiator) = handshake {
                let mut pages = statement_pages(outgoing)?.into_iter();
                let (mut sent, mut received) = (false, false);
                let mut incoming = Vec::new();
                while !(sent && received) {
                    let statements = pages.next().unwrap_or_default();
                    sent = pages.len() == 0;
                    let TipMessage::Statements { statements, last } = self
                        .swap_tips(
                            initiator,
                            TipMessage::Statements {
                                statements,
                                last: sent,
                            },
                        )
                        .await?
                    else {
                        return Err(denied("unexpected tip difference"));
                    };
                    if (received && (!last || !statements.is_empty()))
                        || (!last && statements.is_empty())
                    {
                        return Err(denied("tip difference beyond window"));
                    }
                    ordered_page(
                        &statements,
                        incoming.last().map(TipStatement::id).transpose()?,
                    )?;
                    incoming.extend(statements);
                    if incoming.len() > PAGE {
                        return Err(denied("tip difference quota"));
                    }
                    received |= last;
                }
                verify_missing(&incoming, &wanted)?;
                learned.extend(incoming);
            } else {
                if !wanted.is_empty() {
                    self.send(TipMessage::Fetch {
                        ids: wanted.clone(),
                    })
                    .await?;
                    let mut incoming = Vec::new();
                    loop {
                        let TipMessage::Statements { statements, last } =
                            self.reply::<TipMessage>().await?
                        else {
                            return Err(denied("unexpected tip difference"));
                        };
                        ordered_page(
                            &statements,
                            incoming.last().map(TipStatement::id).transpose()?,
                        )?;
                        if !last && statements.is_empty() {
                            return Err(denied("empty tip difference"));
                        }
                        incoming.extend(statements);
                        if incoming.len() > PAGE {
                            return Err(denied("tip difference quota"));
                        }
                        if last {
                            break;
                        }
                    }
                    verify_missing(&incoming, &wanted)?;
                    learned.extend(incoming);
                }
                // Keep the whole outgoing diff together: dependencies can be in
                // a later hash range. The final empty push admits the union.
                for statements in statement_pages(outgoing)? {
                    pushed = true;
                    self.send(TipMessage::Push {
                        statements,
                        last: false,
                    })
                    .await?;
                    if !matches!(self.reply::<TipMessage>().await?, TipMessage::Stored) {
                        return Err(denied("unexpected tip acknowledgment"));
                    }
                }
            }
        }
        if handshake.is_none() && pushed {
            self.send(TipMessage::Push {
                statements: Vec::new(),
                last: true,
            })
            .await?;
            if !matches!(self.reply::<TipMessage>().await?, TipMessage::Stored) {
                return Err(denied("unexpected tip acknowledgment"));
            }
        }
        replica.receive_tip_statements(&learned).await?;
        Ok(())
    }
}

pub(super) async fn graph_statements<A: RowApplication>(
    replica: &EncryptedReplica<A>,
) -> Result<Vec<(Commitment, TipStatement)>, Error> {
    let mut graph = Vec::new();
    let mut after = None;
    loop {
        let page = replica.tip_statements(after, PAGE).await?;
        let last = page.len() < PAGE;
        for statement in page {
            let id = statement.id()?;
            after = Some(id);
            graph.push((id, statement));
        }
        if last {
            return Ok(graph);
        }
    }
}
fn in_range(id: &Commitment, prefix: &Commitment, depth: u16) -> bool {
    let bytes = usize::from(depth / 8);
    id[..bytes] == prefix[..bytes]
        && (depth.is_multiple_of(8) || ((id[bytes] ^ prefix[bytes]) >> (8 - depth % 8)) == 0)
}
pub(super) fn range_ids(
    graph: &[(Commitment, TipStatement)],
    prefix: Commitment,
    depth: u16,
) -> Result<Vec<Commitment>, Error> {
    if depth > 256 {
        return Err(denied("tip range depth"));
    }
    Ok(graph
        .iter()
        .filter(|(id, _)| in_range(id, &prefix, depth))
        .map(|(id, _)| *id)
        .collect())
}
pub(super) fn ids_digest(ids: &[Commitment]) -> Result<Commitment, Error> {
    Ok(rocky_core::commitment("rocky.tip-graph.v1", &encode(&ids)?))
}
fn verify_missing(statements: &[TipStatement], wanted: &[Commitment]) -> Result<(), Error> {
    let received = statements
        .iter()
        .map(TipStatement::id)
        .collect::<Result<Vec<_>, _>>()?;
    if received != wanted {
        return Err(denied("tip difference outside request"));
    }
    Ok(())
}
