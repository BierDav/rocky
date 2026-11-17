use super::*;
use crate::TipStatement;
use rocky_core::{CoordinatorIdentity, LogHead, LogTerminal, TipId};
use rocky_credentials::{SealFinality, TERMINAL_ACK_FORMAT, TerminalAck, TerminalAckStatement};

const FINALIZED_TIPS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinalityStall {
    LogDropped { tip: TipId },
    EndRefused { tip: TipId, error: String },
    EntryPending { tip: TipId },
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Finalization {
    pub admitted: usize,
    pub stalled: Option<FinalityStall>,
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn finalize_seals(&self, scope: Scope) -> Result<Finalization, Error> {
        let (graph, path, publisher, revocations, node, chain) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            let graph = inner.scope_graph(scope)?;
            let mut path = vec![(
                inner.workspace.tip,
                entered_by(&graph, inner.workspace.tip)?,
            )];
            while path.len() <= FINALIZED_TIPS {
                match Inner::<A>::tip_parent(&graph, path[path.len() - 1].0)? {
                    Some((parent, _)) => path.push((parent, entered_by(&graph, parent)?)),
                    None => break,
                }
            }
            path.reverse();
            (
                graph,
                path,
                inner.config.publisher,
                inner.config.revocations.clone(),
                inner.config.node.clone(),
                inner
                    .terminal_credentials(inner.config.node.public_key(), &inner.config.credentials)
                    .await?,
            )
        };
        let Some(coordinator) = self.usable_log(&graph)? else {
            return Ok(Finalization::default());
        };
        let time = now()?;
        let current = Some((&revocations, time));
        let acknowledges = coordinator.node() == *node.public_key().as_bytes()
            && chain
                .trusted_node(publisher, scope, node.public_key(), current)
                .is_ok();
        let mut start = 0;
        for (index, (_, entered)) in path.iter().enumerate() {
            if let Some(seal) = entered
                && graph.is_final(*seal).map_err(credential)?
            {
                start = index;
            }
        }
        let mut records = Vec::new();
        let mut stalled = None;
        for (index, &(tip, entered)) in path.iter().enumerate().skip(start) {
            let log = TipLog { scope, tip };
            let Ok(mut head) = coordinator.log_head(log).await else {
                break;
            };
            if head.terminal.is_none() {
                let Some(terminal) = unended_terminal(&graph, tip)? else {
                    break;
                };
                let parent = match index {
                    0 => Inner::<A>::tip_parent(&graph, tip)?.map(|(parent, _)| parent),
                    _ => Some(path[index - 1].0),
                };
                let proven = index > start
                    || !head.ended_dropped
                    || self
                        .parent_ended_into(coordinator.as_ref(), scope, parent, entered)
                        .await;
                if !proven {
                    stalled = Some(FinalityStall::LogDropped { tip });
                    break;
                }
                let submitted = coordinator.seal(log, terminal).await;
                let Ok(after) = coordinator.log_head(log).await else {
                    break;
                };
                if after.terminal.is_none() {
                    stalled = Some(FinalityStall::EndRefused {
                        tip,
                        error: submitted.err().map_or_else(
                            || "the log took the seal and reads as unended".into(),
                            |error| error.to_string(),
                        ),
                    });
                    break;
                }
                head = after;
            }
            let Some(terminal) = head.terminal else {
                break;
            };
            let next = path.get(index + 1).and_then(|(_, entered)| *entered);
            let path_continues = next.is_none_or(|through| terminal.digest == through);
            if let Some(record) = self
                .acknowledged(
                    &graph,
                    coordinator.as_ref(),
                    &head,
                    terminal,
                    log,
                    (acknowledges, &node, &chain),
                )
                .await?
            {
                records.push(TipStatement::Finality(record));
            }
            if !path_continues {
                break;
            }
        }
        let admitted = match records.is_empty() {
            true => 0,
            false => self.admit_tip_statements(&records, None).await?,
        };
        if stalled.is_none() {
            stalled = self.entry_pending(scope, coordinator.as_ref()).await?;
        }
        Ok(Finalization { admitted, stalled })
    }
    async fn parent_ended_into(
        &self,
        coordinator: &dyn DomainLog,
        scope: Scope,
        parent: Option<TipId>,
        entered: Option<Commitment>,
    ) -> bool {
        let (Some(parent), Some(entered)) = (parent, entered) else {
            return false;
        };
        coordinator
            .log_head(TipLog { scope, tip: parent })
            .await
            .is_ok_and(|head| head.terminal.is_some_and(|ended| ended.digest == entered))
    }
    async fn entry_pending(
        &self,
        scope: Scope,
        coordinator: &dyn DomainLog,
    ) -> Result<Option<FinalityStall>, Error> {
        let received = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            inner.pending_tips(Some(false)).await?
        };
        let unended = |tip| async move {
            coordinator
                .log_head(TipLog { scope, tip })
                .await
                .map(|head| head.terminal)
        };
        for (_, statements) in received {
            let graph = self.batch_graph(scope, &statements).await?;
            for statement in &statements {
                let TipStatement::Seal(seal) = statement else {
                    continue;
                };
                if seal.statement.scope != scope || seal.statement.recovery.is_some() {
                    continue;
                }
                let mut tip = match entered_by(&graph, seal.statement.tip)? {
                    Some(entered) if !graph.is_final(entered).map_err(credential)? => {
                        match graph.seal_by_id(entered) {
                            Ok(leading) => leading.statement.tip,
                            Err(_) => continue,
                        }
                    }
                    _ => continue,
                };
                for _ in 0..=FINALIZED_TIPS {
                    if !matches!(unended(tip).await, Ok(None)) {
                        break;
                    }
                    let Some(entered) = entered_by(&graph, tip)? else {
                        return Ok(Some(FinalityStall::EntryPending { tip }));
                    };
                    if graph.is_final(entered).map_err(credential)? {
                        return Ok(Some(FinalityStall::EntryPending { tip }));
                    }
                    let Ok(leading) = graph.seal_by_id(entered) else {
                        break;
                    };
                    let parent = leading.statement.tip;
                    match unended(parent).await {
                        Ok(Some(ended)) if ended.digest == entered => {
                            return Ok(Some(FinalityStall::EntryPending { tip }));
                        }
                        Ok(None) => tip = parent,
                        _ => break,
                    }
                }
            }
        }
        Ok(None)
    }
    async fn acknowledged(
        &self,
        graph: &rocky_credentials::TipGraph,
        coordinator: &dyn DomainLog,
        head: &LogHead,
        terminal: LogTerminal,
        log: TipLog,
        (acknowledges, node, chain): (bool, &PrivateKey, &CredentialChain),
    ) -> Result<Option<SealFinality>, Error> {
        let (publisher, revocations) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            (inner.config.publisher, inner.config.revocations.clone())
        };
        let current = Some((&revocations, now()?));
        let Ok(seal) = graph.seal_by_id(terminal.digest) else {
            return Ok(None);
        };
        let id = terminal.digest;
        if seal.statement.tip != log.tip
            || graph.revoked.contains(&id)
            || graph.is_final(id).map_err(credential)?
        {
            return Ok(None);
        }
        if let Some(recovery) = seal.statement.recovery
            && (head.commit != recovery.replaced.position + 1
                || head.running_hash
                    != coordinated_chain(recovery.replaced.running, head.commit, id))
        {
            return Ok(None);
        }
        let Some(minimum) = graph.line_minimum(log.tip).map_err(credential)? else {
            return Ok(None);
        };
        let statement = ack_statement(coordinator.identity(), log, head, terminal);
        let mut acknowledgments: Vec<TerminalAck> = head
            .acknowledgments
            .iter()
            .filter_map(|bytes| TerminalAck::from_bytes(bytes).ok())
            .filter(|ack| {
                ack.statement == statement
                    && ack.verify(publisher, current).is_ok()
                    && graph
                        .ack_beyond_cut(&revocations, ack)
                        .is_ok_and(|out| !out)
            })
            .collect();
        if acknowledges
            && !acknowledgments
                .iter()
                .any(|ack| ack.signer == node.public_key())
        {
            let ack = self
                .sign_ack(log.scope, statement, node, chain.clone())
                .await?;
            let bytes = ack.to_bytes().map_err(credential)?;
            if coordinator.acknowledge_terminal(log, bytes).await.is_ok() {
                acknowledgments.push(ack);
            }
        }
        let record = SealFinality::new(log.scope, id, acknowledgments);
        Ok((record.acknowledgments.len() >= minimum).then_some(record))
    }
}

fn ack_statement(
    coordinator: CoordinatorIdentity,
    log: TipLog,
    head: &LogHead,
    terminal: LogTerminal,
) -> TerminalAckStatement {
    TerminalAckStatement {
        format: TERMINAL_ACK_FORMAT,
        coordinator,
        scope: log.scope,
        tip: log.tip,
        position: head.commit,
        running: head.running_hash,
        seal: terminal.digest,
        continuation: terminal.continuation,
        confirms: terminal.confirms,
    }
}
pub(super) fn entered_by(
    graph: &rocky_credentials::TipGraph,
    tip: rocky_core::TipId,
) -> Result<Option<Commitment>, Error> {
    for seal in &graph.seals {
        if seal.successor().map_err(credential)? == Some(tip) {
            return Ok(Some(seal.id().map_err(credential)?));
        }
    }
    for opening in &graph.openings {
        if opening.tip().map_err(credential)? == tip {
            return Ok(Some(opening.statement.seal));
        }
    }
    Ok(None)
}

fn unended_terminal(
    graph: &rocky_credentials::TipGraph,
    tip: TipId,
) -> Result<Option<LogTerminal>, Error> {
    let Some(seal) = graph.seal(tip).map_err(credential)? else {
        return Ok(None);
    };
    let digest = seal.id().map_err(credential)?;
    if seal.statement.recovery.is_some() || graph.is_final(digest).map_err(credential)? {
        return Ok(None);
    }
    Ok(Some(LogTerminal {
        digest,
        continuation: seal.successor().map_err(credential)?,
        confirms: true,
    }))
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub(super) async fn terminal_finality(
        &self,
        seal: &rocky_credentials::SignedSeal,
        graph: &rocky_credentials::TipGraph,
        coordinator: &dyn DomainLog,
    ) -> Result<Option<SealFinality>, Error> {
        let id = seal.id().map_err(credential)?;
        let log = TipLog {
            scope: seal.statement.scope,
            tip: seal.statement.tip,
        };
        let Some(minimum) = graph.line_minimum(log.tip).map_err(credential)? else {
            return Ok(None);
        };
        let Ok(head) = coordinator.log_head(log).await else {
            return Ok(None);
        };
        let position = head.commit;
        let running = head.running_hash;
        if let Some(recovery) = seal.statement.recovery
            && (position != recovery.replaced.position + 1
                || running != coordinated_chain(recovery.replaced.running, position, id))
        {
            return Ok(None);
        }
        if head.terminal
            != Some(LogTerminal {
                digest: id,
                continuation: seal.successor().map_err(credential)?,
                confirms: false,
            })
        {
            return Ok(None);
        }
        let (publisher, revocations, node, chain) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            let author = inner
                .node_author
                .as_ref()
                .filter(|a| a.key.public_key().as_bytes() == &coordinator.node());
            (
                inner.config.publisher,
                inner.config.revocations.clone(),
                author.map_or_else(|| inner.config.node.clone(), |a| a.key.clone()),
                inner
                    .terminal_credentials(
                        author
                            .map_or_else(|| inner.config.node.public_key(), |a| a.key.public_key()),
                        author.map_or(&inner.config.credentials, |a| &a.credentials),
                    )
                    .await?,
            )
        };
        let current = Some((&revocations, now()?));
        let statement = ack_statement(
            coordinator.identity(),
            log,
            &head,
            LogTerminal {
                digest: id,
                continuation: seal.successor().map_err(credential)?,
                confirms: false,
            },
        );
        let mut acknowledgments: Vec<TerminalAck> = head
            .acknowledgments
            .iter()
            .filter_map(|bytes| TerminalAck::from_bytes(bytes).ok())
            .filter(|ack| {
                ack.statement == statement
                    && ack.verify(publisher, current).is_ok()
                    && graph
                        .ack_beyond_cut(&revocations, ack)
                        .is_ok_and(|out| !out)
            })
            .collect();
        if coordinator.node() == *node.public_key().as_bytes()
            && chain
                .trusted_node(publisher, log.scope, node.public_key(), current)
                .is_ok()
            && !acknowledgments
                .iter()
                .any(|ack| ack.signer == node.public_key())
        {
            let ack = self.sign_ack(log.scope, statement, &node, chain).await?;
            if coordinator
                .acknowledge_terminal(log, ack.to_bytes().map_err(credential)?)
                .await
                .is_ok()
            {
                acknowledgments.push(ack);
            }
        }
        if acknowledgments.len() < minimum {
            return Ok(None);
        }
        let record = SealFinality::new(log.scope, id, acknowledgments);
        graph
            .verify_finality(&record, publisher, current)
            .map_err(credential)?;
        Ok(Some(record))
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    async fn sign_ack(
        &self,
        scope: Scope,
        statement: TerminalAckStatement,
        node: &PrivateKey,
        chain: CredentialChain,
    ) -> Result<TerminalAck, Error> {
        let lane = node.public_key();
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let held = rows(
            &inner.conn,
            &format!(
                "SELECT ack FROM secure_ack_chain WHERE lane=x'{}'",
                lane.to_hex()
            ),
        )
        .await?;
        let last = match held.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(bytes)]) => {
                Some(TerminalAck::from_bytes(bytes).map_err(credential)?)
            }
            Some(_) => return Err(Error::Corrupt("acknowledgment chain".into())),
            None => None,
        };
        if let Some(last) = last
            .as_ref()
            .filter(|last| last.statement == statement && last.credentials == chain)
        {
            return Ok(last.clone());
        }
        let link = match &last {
            Some(last) => last.link.next(last.id().map_err(credential)?),
            None => match inner.graphs.get(&scope) {
                Some(graph) => graph.next_ack_link(&lane).map_err(credential)?,
                None => rocky_credentials::ChainLink::first(lane),
            },
        };
        let ack = TerminalAck::sign(statement, node, chain, link).map_err(credential)?;
        let bytes = ack.to_bytes().map_err(credential)?;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT OR REPLACE INTO secure_ack_chain(lane,ack) VALUES(?,?)",
                    bindings(vec![
                        SqlValue::Blob(lane.as_bytes().to_vec()),
                        SqlValue::Blob(bytes.clone()),
                    ]),
                )
                .await?;
            Ok(())
        })?;
        Ok(ack)
    }
    pub async fn terminal_author(&self) -> Result<(PublicKey, WriterId), Error> {
        let coordinator = self.domain_log().ok_or_else(|| denied("no composed log"))?;
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        if let Some(author) = inner
            .node_author
            .as_ref()
            .filter(|a| *a.key.public_key().as_bytes() == coordinator.node())
        {
            return Ok((author.key.public_key(), author.writer));
        }
        if *inner.config.node.public_key().as_bytes() != coordinator.node() {
            return Err(denied("composed log belongs to another node"));
        }
        let node = inner.config.node.public_key();
        Ok((
            node,
            identity::node_writer(&inner.config.credentials, &node)?,
        ))
    }
    pub async fn install_terminal_credentials(&self, chain: CredentialChain) -> Result<(), Error> {
        let node = chain
            .0
            .last()
            .ok_or_else(|| denied("missing terminal node"))?
            .capability
            .subject;
        let scope = chain
            .0
            .first()
            .ok_or_else(|| denied("missing trusted identity"))?
            .capability
            .extended()
            .map_err(credential)?
            .trusted
            .ok_or_else(|| denied("missing trusted grant"))?;
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        chain
            .verify_database(
                inner
                    .config
                    .credentials
                    .database_context(inner.config.publisher)
                    .map_err(credential)?,
            )
            .map_err(credential)?;
        let owned = node == inner.config.node.public_key()
            || inner
                .line_nodes
                .values()
                .any(|a| a.key.public_key() == node);
        if !owned {
            return Err(denied("terminal credentials belong to another node"));
        }
        chain
            .trusted_node(
                inner.config.publisher,
                scope,
                node,
                Some((&inner.config.revocations, now()?)),
            )
            .map_err(credential)?;
        let bytes = encode(&chain)?;
        let stored = collect_rows(
            inner
                .conn
                .query(
                    "SELECT credentials FROM secure_terminal_credentials WHERE node=?",
                    [node.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        if matches!(stored.first().map(Vec::as_slice), Some([SqlValue::Blob(held)]) if *held == bytes)
        {
            return Ok(());
        }
        transaction!(inner, |inner| {
            inner.conn.execute("INSERT INTO secure_terminal_credentials VALUES(?,?) ON CONFLICT(node) DO UPDATE SET credentials=excluded.credentials", [node.as_bytes().to_vec(), bytes.clone()]).await?;
            Ok(())
        })?;
        flush_guard(inner, &self.inner).await.map(|_| ())
    }
}
impl<A: RowApplication> Inner<A> {
    pub(super) async fn terminal_credentials(
        &self,
        node: PublicKey,
        fallback: &CredentialChain,
    ) -> Result<CredentialChain, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT credentials FROM secure_terminal_credentials WHERE node=?",
                    [node.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(bytes)]) => decode_bounded(bytes, MAX_SECURE_BYTES),
            None => Ok(fallback.clone()),
            _ => Err(Error::Corrupt("terminal credentials".into())),
        }
    }
}
