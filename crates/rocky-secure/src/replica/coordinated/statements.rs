//! Tip statements on a trusted node: a seal ends its tip's coordinated log
//! with its digest before it enters the graph, so no entry commits on a tip
//! the graph sealed; an opening's minimum configures its line's logs before
//! their first join. See `docs/runtime/coordinated-writes.md#seals-and-moves`.
use super::cut::refusal;
use super::*;
use crate::TipStatement;
use rocky_core::{CoordinationError, LogTerminal};
use rocky_credentials::SignedSeal;

/// Where a seal's tip log stands after its terminal entry was submitted.
enum Ending {
    /// Ended by this seal, or by a terminal entry this replica holds no
    /// statement for: the graph decides between concurrent seals.
    Ended,
    /// A seal this replica already holds ended the log first.
    Lost,
    /// The coordinator did not confirm that the log ended.
    Unconfirmed(CoordinationError),
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The statements that may enter the graph. Without a composed log, all
    /// of them. With one, every new seal is verified and its tip's log ended
    /// first; a seal whose log the coordinator did not confirm ended is left
    /// out with what depends on it, until a later exchange. `strict`, for
    /// this node's own publication, fails instead, and also when a seal this
    /// replica holds ended the log first, so nothing is published.
    pub(in crate::replica) async fn coordinate_tip_statements(
        &self,
        statements: &[TipStatement],
        strict: bool,
    ) -> Result<Vec<TipStatement>, Error> {
        let Some(coordinator) = self.domain_log() else {
            return Ok(statements.to_vec());
        };
        let mut fresh = Vec::new();
        for statement in statements {
            if !self.holds_statement(statement.id()?).await? {
                fresh.push(statement.clone());
            }
        }
        if !fresh.iter().any(|s| matches!(s, TipStatement::Seal(_))) {
            return Ok(statements.to_vec());
        }
        // Nothing ends a log for a statement admission would refuse.
        self.check_statements(&fresh).await?;
        let mut refused = BTreeSet::new();
        for statement in &fresh {
            let TipStatement::Seal(seal) = statement else {
                continue;
            };
            match (self.end_log(seal, coordinator.as_ref()).await?, strict) {
                (Ending::Ended, _) | (Ending::Lost, false) => {}
                (Ending::Lost, true) => {
                    return Err(refusal(
                        "sealed",
                        "another seal ended the tip's coordinated log first",
                    ));
                }
                (Ending::Unconfirmed(error), true) => {
                    return Err(refusal(
                        "coordinator_unavailable",
                        &format!("the tip's coordinated log did not confirm the seal: {error}"),
                    ));
                }
                (Ending::Unconfirmed(_), false) => {
                    refused.insert(statement.id()?);
                }
            }
        }
        dependents(statements, &mut refused)?;
        let mut kept = Vec::with_capacity(statements.len());
        for statement in statements {
            if !refused.contains(&statement.id()?) {
                kept.push(statement.clone());
            }
        }
        Ok(kept)
    }
    /// Ends `seal`'s tip log with its digest unless it ended already.
    async fn end_log(
        &self,
        seal: &SignedSeal,
        coordinator: &dyn DomainLog,
    ) -> Result<Ending, Error> {
        let log = TipLog {
            scope: seal.statement.scope,
            tip: seal.statement.tip,
        };
        let terminal = LogTerminal {
            digest: seal.id().map_err(credential)?,
            continuation: seal.successor().map_err(credential)?,
        };
        let head = match coordinator.log_head(log).await {
            Ok(head) => head,
            Err(error) => return Ok(Ending::Unconfirmed(error)),
        };
        let ended = match head.terminal {
            Some(ended) => ended,
            None => match coordinator.seal(log, terminal).await {
                Ok(_) => return Ok(Ending::Ended),
                Err(CoordinationError::Sealed) => match coordinator.log_head(log).await {
                    Ok(LogHead {
                        terminal: Some(ended),
                        ..
                    }) => ended,
                    Ok(_) => return Ok(Ending::Unconfirmed(CoordinationError::Sealed)),
                    Err(error) => return Ok(Ending::Unconfirmed(error)),
                },
                Err(error) => return Ok(Ending::Unconfirmed(error)),
            },
        };
        Ok(
            match ended.digest != terminal.digest && self.holds_statement(ended.digest).await? {
                true => Ending::Lost,
                false => Ending::Ended,
            },
        )
    }
    async fn holds_statement(&self, digest: Commitment) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        let found = collect_rows(
            inner
                .conn
                .query(
                    "SELECT 1 FROM secure_tip_statements WHERE digest=?",
                    [digest.to_vec()],
                )
                .await?,
        )
        .await?;
        Ok(!found.is_empty())
    }
    /// Verifies `statements` against the graph they extend, as admission does.
    async fn check_statements(&self, statements: &[TipStatement]) -> Result<(), Error> {
        let inner = self.inner.lock().await;
        let mut graphs = BTreeMap::new();
        for statement in statements {
            let scope = statement.scope();
            if let std::collections::btree_map::Entry::Vacant(entry) = graphs.entry(scope) {
                entry.insert(inner.scope_graph(scope)?);
            }
            if let Some(graph) = graphs.get_mut(&scope) {
                statement.append(graph);
            }
        }
        for statement in statements {
            statement.verify(
                inner.config.publisher,
                &graphs[&statement.scope()],
                Some((&inner.config.revocations, now()?)),
            )?;
        }
        Ok(())
    }
}
impl<A: RowApplication> Inner<A> {
    /// The minimum in-sync size the opening of `log`'s line names; none on
    /// the genesis line, which takes the domain's, or without a graph.
    fn line_minimum(&self, log: TipLog) -> Result<Option<usize>, Error> {
        let Some(graph) = self.graphs.get(&log.scope) else {
            return Ok(None);
        };
        // A tip of an explicit schema switch is outside the graph.
        if graph.schema_any(log.tip).is_err() {
            return Ok(None);
        }
        let root = graph.line_root(log.tip).map_err(credential)?;
        for opening in &graph.openings {
            if opening.tip().map_err(credential)? == root {
                return Ok(Some(opening.statement.minimum_in_sync as usize));
            }
        }
        Ok(None)
    }
    /// Where a write the terminal entry of `log` aborted continues: the
    /// continuation of the tip's winning seal in this replica's graph, else
    /// the terminal entry's own.
    pub(super) fn continuation(
        &self,
        log: TipLog,
        terminal: LogTerminal,
    ) -> Result<Option<TipId>, Error> {
        match self.graphs.get(&log.scope) {
            Some(graph) => match graph.seal(log.tip).map_err(credential)? {
                Some(seal) => seal.successor().map_err(credential),
                None => Ok(terminal.continuation),
            },
            None => Ok(terminal.continuation),
        }
    }
    /// The winning continuation `log`'s tip resolves to when its seal lost.
    pub(super) fn canonical(&self, log: TipLog) -> Result<TipId, Error> {
        match self.graphs.get(&log.scope) {
            Some(graph) if graph.schema_any(log.tip).is_ok() => {
                graph.canonical_tip(log.tip).map_err(credential)
            }
            _ => Ok(log.tip),
        }
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    /// Before this node joins `log`, relays the minimum of its line if no
    /// node joined yet. A log that took a smaller one is not joined.
    pub(super) async fn configure_line(
        &self,
        log: TipLog,
        head: &LogHead,
        coordinator: &dyn DomainLog,
    ) -> Result<(), CoordinatedError> {
        let Some(minimum) = self.inner.lock().await.line_minimum(log)? else {
            return Ok(());
        };
        if head.epoch == 0 && head.minimum != minimum {
            // A concurrent first join may refuse it; the head decides.
            let _ = coordinator.configure(log, minimum).await;
        }
        let head = coordinator.log_head(log).await.map_err(coordination)?;
        if head.minimum < minimum {
            return Err(CoordinatedError::Refused(CoordinationError::Configuration));
        }
        Ok(())
    }
}
/// Extends `refused` with every statement of `statements` that depends on
/// one in it: an opening on a refused seal, a move off a refused seal's tip
/// or naming a tip a refused statement identifies, and so on transitively.
fn dependents(
    statements: &[TipStatement],
    refused: &mut BTreeSet<Commitment>,
) -> Result<(), Error> {
    loop {
        let (mut sealed, mut tips) = (BTreeSet::new(), BTreeSet::new());
        for statement in statements {
            if !refused.contains(&statement.id()?) {
                continue;
            }
            match statement {
                TipStatement::Seal(seal) => {
                    sealed.insert(seal.statement.tip);
                    tips.extend(seal.successor().map_err(credential)?);
                }
                TipStatement::Opening(opening) => {
                    tips.insert(opening.tip().map_err(credential)?);
                }
                TipStatement::Bookmark(_) => {}
            }
        }
        let before = refused.len();
        for statement in statements {
            let depends = match statement {
                TipStatement::Seal(seal) => tips.contains(&seal.statement.tip),
                TipStatement::Opening(opening) => refused.contains(&opening.statement.seal),
                TipStatement::Bookmark(movement) => {
                    movement
                        .statement
                        .from
                        .is_some_and(|tip| sealed.contains(&tip) || tips.contains(&tip))
                        || movement.statement.to.is_some_and(|tip| tips.contains(&tip))
                }
            };
            if depends {
                refused.insert(statement.id()?);
            }
        }
        if refused.len() == before {
            return Ok(());
        }
    }
}
