//! Tip statements on a trusted node: a seal ends its tip's coordinated log
//! with its digest before it enters the graph; the owner's recovery is a
//! terminal entry that ends a log no ordinary seal can. Seals waiting for
//! their terminal entry are journaled.
use super::*;
use crate::TipStatement;
use rocky_core::{CoordinationError, LogHead, LogTerminal};
use rocky_credentials::SignedSeal;

pub(in crate::replica) const SCHEMA: &[&str] = &[
    // Statement batches whose seals wait for their tip's terminal entry:
    // this node's own publication, flushed before the coordinator sees it,
    // and received seals it could not confirm yet. Never part of the graph.
    "CREATE TABLE IF NOT EXISTS secure_terminal_credentials(node BLOB PRIMARY KEY,credentials BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_pending_tips(batch BLOB PRIMARY KEY,own INTEGER NOT NULL,statements BLOB NOT NULL) WITHOUT ROWID",
];
/// Received batches a node keeps waiting at once; more wait for a resend.
const MAX_PENDING_RECEIVED: usize = 32;

/// Where a seal's tip log stands after its terminal entry was submitted.
enum Ending {
    /// Ended by this seal, or by a terminal entry this replica holds no
    /// statement for: the graph decides between concurrent ordinary seals.
    Ended,
    /// A seal this replica already holds ended the log first.
    Lost,
    /// Never enters the graph: a recovery the log did not commit.
    Refused(&'static str, String),
    /// Not confirmed yet; the coordinator may have committed it after a lost reply.
    Unconfirmed { code: &'static str, reason: String },
}
/// What coordinating a batch decided.
pub(in crate::replica) struct Coordinated {
    /// May enter the graph now.
    pub kept: Vec<TipStatement>,
    /// Wait for their seal's terminal entry, with what depends on them.
    pub deferred: Vec<TipStatement>,
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Splits `statements` into those that may enter the graph and those
    /// that wait. A recovery on every device waits for verified finality;
    /// ordinary seals without a composed log enter immediately. Every
    /// new ordinary seal is verified and its tip's log ended first; a seal whose log
    /// the coordinator did not confirm ended waits with what depends on it,
    /// and a recovery the log did not commit is dropped with its
    /// dependents. `strict`, for this node's own publication, fails
    /// instead, and also when a seal this replica holds ended the log
    /// first, so nothing is published.
    pub(in crate::replica) async fn coordinate_tip_statements(
        &self,
        statements: &[TipStatement],
        strict: bool,
        own: bool,
    ) -> Result<Coordinated, Error> {
        let all = || Coordinated {
            kept: statements.to_vec(),
            deferred: Vec::new(),
        };
        let coordinator = self.domain_log();
        let mut fresh = Vec::new();
        for statement in statements {
            if !self.holds_statement(statement.id()?).await? {
                fresh.push(statement.clone());
            }
        }
        if !fresh.iter().any(|s| matches!(s, TipStatement::Seal(_))) {
            return Ok(all());
        }
        // Nothing ends a log for a statement admission would refuse.
        self.check_statements(&fresh).await?;
        let (mut deferred, mut dropped) = (BTreeSet::new(), BTreeSet::new());
        let mut proofs = Vec::new();
        for statement in &fresh {
            let TipStatement::Seal(seal) = statement else {
                continue;
            };
            if seal.statement.recovery.is_some() {
                let graph = {
                    let inner = self.inner.lock(crate::replica::Class::Background).await;
                    let mut graph = Arc::unwrap_or_clone(inner.scope_graph(seal.statement.scope)?);
                    for statement in statements
                        .iter()
                        .filter(|s| s.scope() == seal.statement.scope)
                    {
                        statement.append(&mut graph);
                    }
                    graph
                        .exclude_revoked(&inner.config.revocations)
                        .map_err(credential)?;
                    graph
                };
                if graph
                    .line_minimum(seal.statement.tip)
                    .map_err(credential)?
                    .is_none()
                {
                    if strict {
                        return Err(refusal(
                            "genesis_minimum_missing",
                            "the owner must provision the genesis line's signed minimum",
                        ));
                    }
                    deferred.insert(statement.id()?);
                    continue;
                }
                // Verified finality is sufficient even after coordinator retention.
                if graph
                    .is_final(seal.id().map_err(credential)?)
                    .map_err(credential)?
                {
                    continue;
                }
                let ending = match coordinator.as_deref() {
                    Some(coordinator) => self.end_log(seal, coordinator).await?,
                    None => Ending::Unconfirmed {
                        code: "recovery_pending",
                        reason: "recovery needs its line's trusted acknowledgments".into(),
                    },
                };
                if matches!(ending, Ending::Ended)
                    && let Some(coordinator) = coordinator.as_deref()
                    && let Some(proof) = self.terminal_finality(seal, &graph, coordinator).await?
                {
                    proofs.push(TipStatement::Finality(proof));
                    continue;
                }
                match ending {
                    Ending::Refused(code, reason) if strict => return Err(refusal(code, &reason)),
                    Ending::Refused(..) => { dropped.insert(statement.id()?); }
                    Ending::Unconfirmed { code, reason, .. } if strict => return Err(Error::Application(Box::new(Unconfirmed { code, message: reason }))),
                    _ if strict => return Err(Error::Application(Box::new(Unconfirmed {
                        code: "recovery_pending", message: "the signed recovery is durable and waiting for its line's trusted acknowledgments".into(),
                    }))),
                    _ => { deferred.insert(statement.id()?); }
                }
                continue;
            }
            let Some(coordinator) = coordinator.as_deref() else {
                continue;
            };
            match self.end_log(seal, coordinator).await? {
                Ending::Ended => {}
                Ending::Lost if strict => {
                    return Err(refusal(
                        "sealed",
                        "another seal ended the tip's coordinated log first",
                    ));
                }
                Ending::Lost if own => {
                    dropped.insert(statement.id()?);
                }
                Ending::Lost => {}
                Ending::Refused(code, reason) if strict => return Err(refusal(code, &reason)),
                Ending::Refused(..) => {
                    dropped.insert(statement.id()?);
                }
                Ending::Unconfirmed { code, reason } if strict => {
                    return Err(Error::Application(Box::new(Unconfirmed {
                        code,
                        message: format!(
                            "the tip's coordinated log did not confirm the seal: {reason}"
                        ),
                    })));
                }
                Ending::Unconfirmed { .. } => {
                    deferred.insert(statement.id()?);
                }
            }
        }
        dependents(statements, &mut dropped)?;
        dependents(statements, &mut deferred)?;
        let mut coordinated = Coordinated {
            kept: Vec::new(),
            deferred: Vec::new(),
        };
        for statement in statements {
            let id = statement.id()?;
            if dropped.contains(&id) {
                continue;
            }
            match deferred.contains(&id) {
                true => coordinated.deferred.push(statement.clone()),
                false => coordinated.kept.push(statement.clone()),
            }
        }
        for proof in proofs {
            if let TipStatement::Finality(record) = &proof {
                if dropped.contains(&record.seal) {
                    continue;
                }
                if deferred.contains(&record.seal) {
                    coordinated.deferred.push(proof);
                    continue;
                }
            }
            coordinated.kept.push(proof);
        }
        Ok(coordinated)
    }
    /// Ends `seal`'s tip log with it unless it ended already. A recovery is
    /// confirmed only by the exact terminal entry it is.
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
        let unavailable = |error: CoordinationError| Ending::Unconfirmed {
            code: "coordinator_unavailable",
            reason: error.to_string(),
        };
        let head = match coordinator.log_head(log).await {
            Ok(head) => head,
            Err(error) => return Ok(unavailable(error)),
        };
        if let Some(ended) = head.terminal {
            return self.ended_by(seal, terminal, ended, &head).await;
        }
        let submitted = match seal.statement.recovery {
            Some(_) => {
                let bytes = seal.to_bytes().map_err(credential)?;
                coordinator.recover(log, bytes).await
            }
            None => coordinator.seal(log, terminal).await,
        };
        match submitted {
            Ok(_) => Ok(Ending::Ended),
            Err(CoordinationError::Sealed) => match coordinator.log_head(log).await {
                Ok(head) if head.terminal.is_some() => {
                    self.ended_by(seal, terminal, head.terminal.unwrap_or(terminal), &head)
                        .await
                }
                Ok(_) => Ok(unavailable(CoordinationError::Sealed)),
                Err(error) => Ok(unavailable(error)),
            },
            Err(CoordinationError::StaleHead) if seal.statement.recovery.is_some() => {
                Ok(Ending::Refused(
                    "stale_recovery",
                    "the log moved since the owner signed the recovery; collect and sign again"
                        .into(),
                ))
            }
            Err(CoordinationError::Denied) if seal.statement.recovery.is_some() => Ok(
                Ending::Refused("denied", "the coordinator refused the recovery".into()),
            ),
            Err(CoordinationError::Denied) => Ok(Ending::Unconfirmed {
                code: "behind",
                reason: "this node is not in the tip's in-sync set".into(),
            }),
            Err(error) => Ok(unavailable(error)),
        }
    }
    /// What the terminal entry `ended` that ended `seal`'s log means for it.
    async fn ended_by(
        &self,
        seal: &SignedSeal,
        terminal: LogTerminal,
        ended: LogTerminal,
        head: &LogHead,
    ) -> Result<Ending, Error> {
        if ended == terminal {
            if let Some(recovery) = seal.statement.recovery
                && (head.commit != recovery.replaced.position + 1
                    || head.running_hash
                        != coordinated_chain(
                            recovery.replaced.running,
                            head.commit,
                            terminal.digest,
                        ))
            {
                return Ok(Ending::Refused(
                    "stale_recovery",
                    "the terminal does not bind the recovery's replaced head".into(),
                ));
            }
            return Ok(Ending::Ended);
        }
        // A recovery that never committed abandons nothing.
        if seal.statement.recovery.is_some() {
            return Ok(Ending::Refused(
                "sealed",
                "another terminal entry ended the tip's log; the recovery never committed".into(),
            ));
        }
        Ok(
            match ended.digest != terminal.digest && self.holds_statement(ended.digest).await? {
                true => Ending::Lost,
                false => Ending::Ended,
            },
        )
    }
    /// Journals a batch, flushed, before its seals reach the coordinator.
    /// Received batches beyond the bound are not kept; a peer resends them.
    pub(in crate::replica) async fn journal_tips(
        &self,
        statements: &[TipStatement],
        own: bool,
    ) -> Result<Commitment, Error> {
        let ids = statements
            .iter()
            .map(TipStatement::id)
            .collect::<Result<Vec<_>, _>>()?;
        let batch = rocky_core::commitment("rocky.pending-tips.v1", &encode(&ids)?);
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let all = inner.pending_tips(None).await?;
        if all.iter().any(|(id, _)| *id == batch) {
            return Ok(batch);
        }
        let pending = inner.pending_tips(Some(false)).await?;
        if !own
            && pending.len() >= MAX_PENDING_RECEIVED
            && !pending.iter().any(|(id, _)| *id == batch)
        {
            return Err(denied("pending tip statement quota"));
        }
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT OR IGNORE INTO secure_pending_tips VALUES(?,?,?)",
                    bindings(vec![
                        SqlValue::Blob(batch.to_vec()),
                        SqlValue::Integer(i64::from(own)),
                        SqlValue::Blob(encode(&statements)?),
                    ]),
                )
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            Ok(())
        })?;
        inner = flush_guard(inner, &self.inner).await?;
        inner.publish_revision().await?;
        Ok(batch)
    }
    /// Ends the logs of journaled seals and admits what they confirm: own
    /// batches after a restart or a lost reply, with the same bytes, and
    /// received ones. Returns the statements admitted.
    pub async fn settle_tip_statements(&self) -> Result<usize, Error> {
        // A batch chained after another settles in a later round.
        let mut admitted = 0;
        loop {
            let round = self.settle_round().await?;
            if round == 0 {
                return Ok(admitted);
            }
            admitted += round;
        }
    }
    async fn settle_round(&self) -> Result<usize, Error> {
        let (pending, own) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            let own = inner
                .pending_tips(Some(true))
                .await?
                .into_iter()
                .map(|(batch, _)| batch)
                .collect::<BTreeSet<_>>();
            (inner.pending_tips(None).await?, own)
        };
        let mut admitted = 0;
        for (batch, statements) in pending {
            let own = own.contains(&batch);
            let coordinated = match self
                .coordinate_tip_statements(&statements, false, own)
                .await
            {
                Ok(coordinated) => coordinated,
                // A revoked or permanently invalid candidate cannot block later
                // batches. An own batch keeps its exact submission bytes even if
                // another statement was revoked after external commitment; a
                // received one leaves the quota, and its peer resends it.
                Err(Error::Rejected(_)) => {
                    self.forget_refused(batch, own).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            admitted += match self
                .admit_pending_tips(&coordinated.kept, Some(batch), &coordinated.deferred)
                .await
            {
                Ok(count) => count,
                // A statement the graph refuses does not block later batches.
                Err(Error::Rejected(_)) => {
                    self.forget_refused(batch, own).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            // An own statement dropped as lost takes the own statements
            // chained after it: none of them is ever published.
            if own {
                let mut dropped = BTreeSet::new();
                for statement in &statements {
                    let id = statement.id()?;
                    let kept = coordinated
                        .kept
                        .iter()
                        .chain(&coordinated.deferred)
                        .any(|kept| kept.id().is_ok_and(|kept| kept == id));
                    if !kept && statement.chain_place().is_some() {
                        dropped.insert(id);
                    }
                }
                self.forget_chained_after(dropped).await?;
            }
        }
        Ok(admitted)
    }
    /// Forgets own journaled batches chained after `dropped`, transitively.
    async fn forget_chained_after(&self, mut dropped: BTreeSet<Commitment>) -> Result<(), Error> {
        if dropped.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut forget = BTreeSet::new();
        let pending = inner.pending_tips(Some(true)).await?;
        loop {
            let before = forget.len();
            for (batch, statements) in &pending {
                if forget.contains(batch) {
                    continue;
                }
                let after = statements.iter().any(|statement| {
                    statement
                        .chain_place()
                        .and_then(|(_, link)| link.previous)
                        .is_some_and(|previous| dropped.contains(&previous))
                });
                if after {
                    forget.insert(*batch);
                    for statement in statements {
                        dropped.insert(statement.id()?);
                    }
                }
            }
            if forget.len() == before {
                break;
            }
        }
        if forget.is_empty() {
            return Ok(());
        }
        tracing::warn!(
            batches = forget.len(),
            "own tip statements chained after a lost seal are dropped unpublished"
        );
        transaction!(inner, |inner| {
            for batch in &forget {
                inner.forget_tips(*batch).await?;
            }
            Ok(())
        })?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(())
    }
    /// Forgets a received journaled batch admission refused; own batches stay.
    pub(in crate::replica) async fn forget_refused(
        &self,
        batch: Commitment,
        own: bool,
    ) -> Result<(), Error> {
        if own {
            return Ok(());
        }
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "DELETE FROM secure_pending_tips WHERE batch=? AND own=0",
                    [batch.to_vec()],
                )
                .await?;
            Ok(())
        })?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(())
    }
    /// Forgets an own journaled batch the coordinator refused for good.
    pub(in crate::replica) async fn forget_own_batch(
        &self,
        batch: Commitment,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(inner, |inner| inner.forget_tips(batch).await)?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(())
    }
    /// This node's live statements on `identity`'s chain through `lane`,
    /// held or journaled, and the live link its next one takes: after all
    /// of them. Staged statements, which may be withdrawn, are on their own
    /// branch and count for neither.
    pub async fn own_chain(
        &self,
        scope: rocky_core::Scope,
        identity: rocky_credentials::PublicKey,
        lane: rocky_credentials::PublicKey,
    ) -> Result<(rocky_credentials::ChainLink, Vec<TipStatement>), Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut graph = Arc::unwrap_or_clone(inner.scope_graph(scope)?);
        for (_, statements) in inner.pending_tips(Some(true)).await? {
            for statement in statements.iter().filter(|s| s.scope() == scope) {
                statement.append(&mut graph);
            }
        }
        let staged: BTreeSet<Commitment> = rows(
            &inner.conn,
            "SELECT digest FROM secure_tip_statements WHERE staged=1",
        )
        .await?
        .into_iter()
        .filter_map(|record| match record.as_slice() {
            [SqlValue::Blob(digest)] => Commitment::try_from(digest.as_slice()).ok(),
            _ => None,
        })
        .collect();
        graph.remove_statements(&staged).map_err(credential)?;
        let next = graph.next_link(&identity, &lane).map_err(credential)?;
        let statements = crate::replica::tips::graph_statements(&graph)
            .into_iter()
            .filter(|statement| {
                statement
                    .chain_place()
                    .is_some_and(|(signer, link)| signer == identity && link.lane == lane)
            })
            .collect();
        Ok((next, statements))
    }
    /// Whether a statement of `statements` names as predecessor an own
    /// journaled statement the graph does not hold yet.
    pub(in crate::replica) async fn after_pending(
        &self,
        statements: &[TipStatement],
    ) -> Result<bool, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut waiting = BTreeSet::new();
        for (_, batch) in inner.pending_tips(Some(true)).await? {
            for statement in batch {
                waiting.insert(statement.id()?);
            }
        }
        for statement in statements {
            waiting.remove(&statement.id()?);
        }
        for statement in statements {
            let Some(previous) = statement.chain_place().and_then(|(_, link)| link.previous) else {
                continue;
            };
            if waiting.contains(&previous)
                && collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT 1 FROM secure_tip_statements WHERE digest=?",
                            [previous.to_vec()],
                        )
                        .await?,
                )
                .await?
                .is_empty()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Refuses own statements chained after a staged one: the stage may be
    /// withdrawn and never published.
    pub(in crate::replica) async fn refuse_after_stage(
        &self,
        statements: &[TipStatement],
    ) -> Result<(), Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let staged: BTreeSet<Vec<u8>> = rows(
            &inner.conn,
            "SELECT digest FROM secure_tip_statements WHERE staged=1",
        )
        .await?
        .into_iter()
        .filter_map(|record| match record.as_slice() {
            [SqlValue::Blob(digest)] => Some(digest.clone()),
            _ => None,
        })
        .collect();
        for statement in statements {
            if let Some(previous) = statement.chain_place().and_then(|(_, link)| link.previous)
                && staged.contains(previous.as_slice())
            {
                return Err(refusal(
                    "migration_pending",
                    "a staged schema switch must finish or be withdrawn before this node signs again",
                ));
            }
        }
        Ok(())
    }
    /// Unadmitted candidates, for relay to trusted nodes that can inspect
    /// and acknowledge them. They are never returned by `tip_statements`.
    pub async fn pending_tip_candidates(&self) -> Result<Vec<Vec<TipStatement>>, Error> {
        Ok(self
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .pending_tips(None)
            .await?
            .into_iter()
            .map(|(_, statements)| statements)
            .filter(|statements| statements.iter().any(|statement| matches!(statement, TipStatement::Seal(seal) if seal.statement.recovery.is_some())))
            .collect())
    }
    pub(in crate::replica) async fn holds_statement(
        &self,
        digest: Commitment,
    ) -> Result<bool, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
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
    pub(in crate::replica) async fn check_statements(
        &self,
        statements: &[TipStatement],
    ) -> Result<(), Error> {
        self.check_statements_over(statements, false).await
    }
    /// `check_statements`, over this node's journaled statements too when
    /// `pending`.
    pub(in crate::replica) async fn check_statements_over(
        &self,
        statements: &[TipStatement],
        pending: bool,
    ) -> Result<(), Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut graphs = BTreeMap::new();
        let journaled = match pending {
            true => inner
                .pending_tips(Some(true))
                .await?
                .into_iter()
                .flat_map(|(_, batch)| batch)
                .collect(),
            false => Vec::new(),
        };
        for statement in journaled.iter().chain(statements) {
            statement.id()?;
            let scope = statement.scope();
            if let std::collections::btree_map::Entry::Vacant(entry) = graphs.entry(scope) {
                entry.insert(inner.scope_graph(scope)?);
            }
            if let Some(graph) = graphs.get_mut(&scope) {
                statement.append(Arc::make_mut(graph));
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
    /// Journaled batches, own ones or received ones when `own` names which.
    pub(crate) async fn pending_tips(
        &self,
        own: Option<bool>,
    ) -> Result<Vec<(Commitment, Vec<TipStatement>)>, Error> {
        let mut pending = Vec::new();
        for record in rows(
            &self.conn,
            "SELECT batch,own,statements FROM secure_pending_tips ORDER BY batch",
        )
        .await?
        {
            let [
                SqlValue::Blob(batch),
                SqlValue::Integer(kind),
                SqlValue::Blob(statements),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("pending tip statements".into()));
            };
            if own.is_none_or(|own| own == (*kind == 1)) {
                pending.push((
                    batch
                        .as_slice()
                        .try_into()
                        .map_err(|_| Error::Corrupt("pending tip batch".into()))?,
                    decode_bounded(statements, MAX_SECURE_BYTES)?,
                ));
            }
        }
        Ok(pending)
    }
    pub(in crate::replica) async fn prune_pending_tips(&self) -> Result<(), Error> {
        for (batch, statements) in self.pending_tips(None).await? {
            let mut pending = Vec::new();
            for statement in &statements {
                let known = collect_rows(
                    self.conn
                        .query(
                            "SELECT 1 FROM secure_tip_statements WHERE digest=?",
                            [statement.id()?.to_vec()],
                        )
                        .await?,
                )
                .await?;
                if known.is_empty() {
                    pending.push(statement.clone());
                }
            }
            if pending.len() != statements.len() {
                self.replace_pending_tips(batch, &pending).await?;
            }
        }
        Ok(())
    }
    /// Replaces the unresolved remainder without losing ownership or the
    /// original batch identity, in the graph admission transaction.
    pub(in crate::replica) async fn replace_pending_tips(
        &self,
        batch: Commitment,
        remainder: &[TipStatement],
    ) -> Result<(), Error> {
        if remainder.is_empty() {
            return self.forget_tips(batch).await;
        }
        self.conn
            .execute(
                "UPDATE secure_pending_tips SET statements=? WHERE batch=?",
                [encode(&remainder)?, batch.to_vec()],
            )
            .await?;
        Ok(())
    }
    /// Drops a journaled batch inside the caller's transaction.
    pub(in crate::replica) async fn forget_tips(&self, batch: Commitment) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM secure_pending_tips WHERE batch=?",
                [batch.to_vec()],
            )
            .await?;
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
                TipStatement::Bookmark(_)
                | TipStatement::GenesisLine(_)
                | TipStatement::Finality(_)
                | TipStatement::Checkpoint(_) => {}
            }
        }
        let before = refused.len();
        for statement in statements {
            let depends = match statement {
                TipStatement::Seal(seal) => tips.contains(&seal.statement.tip),
                TipStatement::Opening(opening) => refused.contains(&opening.statement.seal),
                TipStatement::Finality(record) => refused.contains(&record.seal),
                TipStatement::Checkpoint(record) => refused.contains(&record.statement.seal),
                TipStatement::GenesisLine(_) => false,
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

/// A seal the coordinator did not confirm. `code` is the JSON error code;
/// `submitted` when it may still commit after a lost reply, so the journal
/// keeps it to send again.
#[derive(Debug)]
pub struct Unconfirmed {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for Unconfirmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Unconfirmed {}

/// Why the replica refused a call. `code` is the JSON error code.
#[derive(Debug)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Refusal {}
pub(in crate::replica) fn refusal(code: &'static str, message: &str) -> Error {
    Error::Application(Box::new(Refusal {
        code,
        message: message.into(),
    }))
}
