//! Final seals on a trusted node: it acknowledges the terminal entry that
//! ended a tip's log with a seal it holds, and publishes the seal's
//! finality record once the coordinator keeps enough acknowledgments. See
//! `docs/runtime/seal-finality.md`.
use super::*;
use crate::TipStatement;
use rocky_core::LogTerminal;
use rocky_credentials::{SealFinality, TERMINAL_ACK_FORMAT, TerminalAck, TerminalAckStatement};

/// Ended tips before the followed one a node acknowledges: the
/// coordinator's retained ended logs.
const FINALIZED_TIPS: usize = 8;

impl<A: RowApplication> EncryptedReplica<A> {
    /// Acknowledges, as a node of a trusted identity, the terminal entries
    /// that ended the followed tip's log or its line's latest ones with a
    /// seal of the graph not yet final, and admits a finality record for
    /// each seal enough distinct trusted nodes acknowledged. Returns how
    /// many records it admitted. Without a composed log it does nothing; a
    /// coordinator it cannot reach leaves the seal to a later call.
    pub async fn finalize_seals(&self, scope: Scope) -> Result<usize, Error> {
        let Some(coordinator) = self.domain_log() else {
            return Ok(0);
        };
        let (graph, tips, publisher, revocations, node, chain) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            let graph = inner.scope_graph(scope)?;
            // The followed tip and the nearest ones its line came through:
            // a coordinator keeps that many ended logs.
            let mut tips = vec![inner.workspace.tip];
            while tips.len() <= FINALIZED_TIPS {
                match Inner::<A>::tip_parent(&graph, tips[tips.len() - 1])? {
                    Some((parent, _)) => tips.push(parent),
                    None => break,
                }
            }
            (
                graph,
                tips,
                inner.config.publisher,
                inner.config.revocations.clone(),
                inner.config.node.clone(),
                inner
                    .terminal_credentials(inner.config.node.public_key(), &inner.config.credentials)
                    .await?,
            )
        };
        let time = now()?;
        let current = Some((&revocations, time));
        // Only the node the coordinator knows, certified by a trusted
        // identity, acknowledges; any trusted node publishes.
        let acknowledges = coordinator.node() == *node.public_key().as_bytes()
            && chain
                .trusted_node(publisher, scope, node.public_key(), current)
                .is_ok();
        let mut records = Vec::new();
        for tip in tips {
            let log = TipLog { scope, tip };
            let Ok(head) = coordinator.log_head(log).await else {
                continue;
            };
            let Some(terminal) = head.terminal else {
                continue;
            };
            let Ok(seal) = graph.seal_by_id(terminal.digest) else {
                continue;
            };
            let id = terminal.digest;
            if seal.statement.tip != tip
                || graph.revoked.contains(&id)
                || graph.is_final(id).map_err(credential)?
            {
                continue;
            }
            if let Some(recovery) = seal.statement.recovery
                && (head.commit != recovery.replaced.position + 1
                    || head.running_hash
                        != coordinated_chain(recovery.replaced.running, head.commit, id))
            {
                continue;
            }
            let Some(minimum) = graph.line_minimum(tip).map_err(credential)? else {
                continue;
            };
            let statement = TerminalAckStatement {
                format: TERMINAL_ACK_FORMAT,
                scope,
                tip,
                position: head.commit,
                running: head.running_hash,
                seal: id,
                continuation: terminal.continuation,
            };
            let mut acknowledgments: Vec<TerminalAck> = head
                .acknowledgments
                .iter()
                .filter_map(|bytes| TerminalAck::from_bytes(bytes).ok())
                .filter(|ack| ack.statement == statement && ack.verify(publisher, current).is_ok())
                .collect();
            if acknowledges
                && !acknowledgments
                    .iter()
                    .any(|ack| ack.signer == node.public_key())
            {
                let ack = TerminalAck::sign(statement, &node, chain.clone()).map_err(credential)?;
                let bytes = ack.to_bytes().map_err(credential)?;
                if coordinator.acknowledge_terminal(log, bytes).await.is_ok() {
                    acknowledgments.push(ack);
                }
            }
            let record = SealFinality::new(scope, id, acknowledgments);
            if record.acknowledgments.len() >= minimum {
                records.push(TipStatement::Finality(record));
            }
        }
        if records.is_empty() {
            return Ok(0);
        }
        self.admit_tip_statements(&records, None).await
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Inspects a pending recovery without activating it. Only the exact
    /// committed terminal is acknowledged, under the line's signed minimum.
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
        let statement = TerminalAckStatement {
            format: TERMINAL_ACK_FORMAT,
            scope: log.scope,
            tip: log.tip,
            position,
            running,
            seal: id,
            continuation: seal.successor().map_err(credential)?,
        };
        let mut acknowledgments: Vec<TerminalAck> = head
            .acknowledgments
            .iter()
            .filter_map(|bytes| TerminalAck::from_bytes(bytes).ok())
            .filter(|ack| ack.statement == statement && ack.verify(publisher, current).is_ok())
            .collect();
        if coordinator.node() == *node.public_key().as_bytes()
            && chain
                .trusted_node(publisher, log.scope, node.public_key(), current)
                .is_ok()
            && !acknowledgments
                .iter()
                .any(|ack| ack.signer == node.public_key())
        {
            let ack = TerminalAck::sign(statement, &node, chain).map_err(credential)?;
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
    /// The node and author number used to acknowledge the composed log.
    pub async fn terminal_author(&self) -> Result<(PublicKey, u32), Error> {
        let coordinator = self.domain_log().ok_or_else(|| denied("no composed log"))?;
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        if let Some(author) = inner
            .node_author
            .as_ref()
            .filter(|a| *a.key.public_key().as_bytes() == coordinator.node())
        {
            return Ok((author.key.public_key(), author.number));
        }
        if *inner.config.node.public_key().as_bytes() != coordinator.node() {
            return Err(denied("composed log belongs to another node"));
        }
        Ok((
            inner.config.node.public_key(),
            inner
                .config
                .credentials
                .author_number()
                .map_err(credential)?,
        ))
    }
    /// Installs an owner-direct trusted acknowledgment chain for an owned
    /// node. Its row-authoring chain is independent of this authority.
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
