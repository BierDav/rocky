//! Final seals on a trusted node: it acknowledges the terminal entry that
//! ended a tip's log with a seal it holds, and publishes the seal's
//! finality record once the coordinator keeps enough acknowledgments. See
//! `docs/runtime/seal-finality.md`.
use super::*;
use crate::TipStatement;
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
            let inner = self.inner.lock().await;
            let graph = inner.scope_graph(scope)?;
            // The followed tip and the nearest ones its line came through:
            // a coordinator keeps that many ended logs.
            let mut tips = vec![inner.tip];
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
                inner.config.credentials.clone(),
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
        self.admit_tip_statements(&records).await
    }
}
