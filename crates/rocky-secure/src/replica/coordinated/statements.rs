use super::*;
use crate::TipStatement;
use rocky_core::{CoordinationError, LogHead, LogTerminal};
use rocky_credentials::SignedSeal;

pub(in crate::replica) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_terminal_credentials(node BLOB PRIMARY KEY,credentials BLOB NOT NULL) WITHOUT ROWID",
    PENDING_TIPS,
];
const PENDING_TIPS: &str = "CREATE TABLE IF NOT EXISTS secure_pending_tips(batch BLOB PRIMARY KEY,own INTEGER NOT NULL,statements BLOB NOT NULL) WITHOUT ROWID";
const MAX_PENDING_RECEIVED: usize = 32;

enum Ending {
    Ended,
    Lost,
    Refused(Refusal),
    Unconfirmed(Refusal),
}
pub(super) enum Entry {
    Won,
    Lost,
    Waiting(Refusal),
}
pub(in crate::replica) struct Coordinated {
    pub kept: Vec<TipStatement>,
    pub deferred: Vec<TipStatement>,
}

impl<A: RowApplication> EncryptedReplica<A> {
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
        self.check_statements(&fresh).await?;
        let (mut deferred, mut dropped) = (BTreeSet::new(), BTreeSet::new());
        let mut proofs = Vec::new();
        let mut graphs = BTreeMap::new();
        for statement in &fresh {
            let TipStatement::Seal(seal) = statement else {
                continue;
            };
            let scope = seal.statement.scope;
            if let std::collections::btree_map::Entry::Vacant(entry) = graphs.entry(scope) {
                entry.insert(self.batch_graph(scope, statements).await?);
            }
            let graph = &graphs[&scope];
            let named = graph.coordinator().map_err(credential)?.is_some();
            let usable = match coordinator.as_deref() {
                Some(log) if graph_names(graph, log)? => Some(log),
                _ => None,
            };
            if seal.statement.recovery.is_some() {
                if graph
                    .line_minimum(seal.statement.tip)
                    .map_err(credential)?
                    .is_none()
                {
                    if strict {
                        return Err(refusal(Refusal::GenesisMinimumMissing(
                            "the owner must provision the genesis line's signed minimum".into(),
                        )));
                    }
                    deferred.insert(statement.id()?);
                    continue;
                }
                if graph
                    .is_final(seal.id().map_err(credential)?)
                    .map_err(credential)?
                {
                    continue;
                }
                let ending = match usable {
                    Some(coordinator) => self.end_log(seal, coordinator, own).await?,
                    None => Ending::Unconfirmed(Refusal::RecoveryPending(
                        "recovery needs its line's trusted acknowledgments".into(),
                    )),
                };
                if matches!(ending, Ending::Ended)
                    && let Some(coordinator) = usable
                    && let Some(proof) = self.terminal_finality(seal, graph, coordinator).await?
                {
                    proofs.push(TipStatement::Finality(proof));
                    continue;
                }
                match ending {
                    Ending::Refused(reason) if strict => return Err(refusal(reason)),
                    Ending::Refused(..) => {
                        dropped.insert(statement.id()?);
                    }
                    Ending::Unconfirmed(reason) if strict => {
                        return Err(Error::Application(Box::new(Unconfirmed(reason))));
                    }
                    _ if strict => {
                        return Err(Error::Application(Box::new(Unconfirmed(
                            Refusal::RecoveryPending(
                                "the signed recovery is durable and waiting for its line's trusted acknowledgments".into(),
                            ),
                        ))));
                    }
                    _ => {
                        deferred.insert(statement.id()?);
                    }
                }
                continue;
            }
            let id = seal.id().map_err(credential)?;
            if graph.is_final(id).map_err(credential)? {
                continue;
            }
            if own && another_seal_is_final(graph, seal)? {
                if strict {
                    return Err(refusal(Refusal::Sealed(
                        "another seal of the tip is final".into(),
                    )));
                }
                dropped.insert(statement.id()?);
                continue;
            }
            let Some(coordinator) = usable else {
                if !named {
                    continue;
                }
                if strict && let Some(refused) = self.named_log_refusal(graph)? {
                    return Err(refused);
                }
                if own {
                    deferred.insert(statement.id()?);
                }
                continue;
            };
            match self.entry_standing(graph, seal, coordinator).await? {
                Entry::Won => {}
                Entry::Lost if strict => {
                    return Err(refusal(Refusal::Sealed(
                        "the seal that leads into the tip lost its own log: the tip is on a losing branch".into(),
                    )));
                }
                Entry::Lost => {
                    if own {
                        dropped.insert(statement.id()?);
                    }
                    continue;
                }
                Entry::Waiting(reason) if strict => {
                    return Err(Error::Application(Box::new(Unconfirmed(reason))));
                }
                Entry::Waiting(_) => {
                    deferred.insert(statement.id()?);
                    continue;
                }
            }
            match self.end_log(seal, coordinator, own).await? {
                Ending::Ended => {}
                Ending::Lost if strict => {
                    return Err(refusal(Refusal::Sealed(
                        "another seal ended the tip's coordinated log first".into(),
                    )));
                }
                Ending::Lost if own => {
                    dropped.insert(statement.id()?);
                }
                Ending::Lost => {}
                Ending::Refused(reason) if strict => return Err(refusal(reason)),
                Ending::Refused(..) => {
                    dropped.insert(statement.id()?);
                }
                Ending::Unconfirmed(reason) if strict => {
                    return Err(Error::Application(Box::new(Unconfirmed(
                        reason.with_prefix("the tip's coordinated log did not confirm the seal: "),
                    ))));
                }
                Ending::Unconfirmed(_) => {
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
    pub(in crate::replica) fn usable_log(
        &self,
        graph: &rocky_credentials::TipGraph,
    ) -> Result<Option<Arc<dyn DomainLog>>, Error> {
        let Some(log) = self.domain_log() else {
            return Ok(None);
        };
        Ok(graph_names(graph, log.as_ref())?.then_some(log))
    }
    pub(in crate::replica) fn named_log_refusal(
        &self,
        graph: &rocky_credentials::TipGraph,
    ) -> Result<Option<Error>, Error> {
        if graph.coordinator().map_err(credential)?.is_none() {
            return Ok(None);
        }
        Ok(match self.domain_log() {
            Some(log) if graph_names(graph, log.as_ref())? => None,
            Some(_) => Some(refusal(Refusal::CoordinatorMismatch(
                "this node's coordinated log is not the coordinator the owner's database settings name".into(),
            ))),
            None => Some(refusal(Refusal::CoordinatorRequired(
                "the owner's database settings name a coordinator: compose this node's coordinated log before it seals a tip, moves a bookmark off one or publishes a schema".into(),
            ))),
        })
    }
    pub(super) async fn batch_graph(
        &self,
        scope: rocky_core::Scope,
        statements: &[TipStatement],
    ) -> Result<rocky_credentials::TipGraph, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut graph = Arc::unwrap_or_clone(inner.scope_graph(scope)?);
        for statement in statements.iter().filter(|s| s.scope() == scope) {
            statement.append(&mut graph);
        }
        graph
            .exclude_revoked(&inner.config.revocations)
            .map_err(credential)?;
        Ok(graph)
    }
    pub(super) async fn entry_standing(
        &self,
        graph: &rocky_credentials::TipGraph,
        seal: &SignedSeal,
        coordinator: &dyn DomainLog,
    ) -> Result<Entry, Error> {
        let Some(entered) = super::finality::entered_by(graph, seal.statement.tip)? else {
            return Ok(Entry::Won);
        };
        if graph.is_final(entered).map_err(credential)? {
            return Ok(Entry::Won);
        }
        let parent = graph.seal_by_id(entered).map_err(credential)?.statement.tip;
        let log = TipLog {
            scope: seal.statement.scope,
            tip: parent,
        };
        Ok(match coordinator.log_head(log).await {
            Ok(head) => match head.terminal {
                Some(terminal) if terminal.digest == entered => Entry::Won,
                Some(_) => Entry::Lost,
                None => Entry::Waiting(Refusal::EntryPending(
                    "the seal that leads into the tip has not ended its own log yet".into(),
                )),
            },
            Err(error) => Entry::Waiting(Refusal::CoordinatorUnavailable(format!(
                "the tip's coordinated log did not confirm the seal: {error}"
            ))),
        })
    }
    async fn end_log(
        &self,
        seal: &SignedSeal,
        coordinator: &dyn DomainLog,
        own: bool,
    ) -> Result<Ending, Error> {
        let log = TipLog {
            scope: seal.statement.scope,
            tip: seal.statement.tip,
        };
        let terminal = LogTerminal {
            digest: seal.id().map_err(credential)?,
            continuation: seal.successor().map_err(credential)?,
            confirms: false,
        };
        let unavailable = |error: CoordinationError| {
            Ending::Unconfirmed(Refusal::CoordinatorUnavailable(error.to_string()))
        };
        let head = match coordinator.log_head(log).await {
            Ok(head) => head,
            Err(error) => return Ok(unavailable(error)),
        };
        if let Some(ended) = head.terminal {
            return self.ended_by(seal, terminal, ended, &head, own).await;
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
                    self.ended_by(
                        seal,
                        terminal,
                        head.terminal.unwrap_or(terminal),
                        &head,
                        own,
                    )
                    .await
                }
                Ok(_) => Ok(unavailable(CoordinationError::Sealed)),
                Err(error) => Ok(unavailable(error)),
            },
            Err(CoordinationError::StaleHead) if seal.statement.recovery.is_some() => {
                Ok(Ending::Refused(Refusal::StaleRecovery(
                    "the log moved since the owner signed the recovery; collect and sign again"
                        .into(),
                )))
            }
            Err(CoordinationError::Denied) if seal.statement.recovery.is_some() => {
                Ok(Ending::Refused(Refusal::Denied(
                    "the coordinator refused the recovery".into(),
                )))
            }
            Err(CoordinationError::Denied) => Ok(Ending::Unconfirmed(Refusal::Behind(
                "this node is not in the tip's in-sync set".into(),
            ))),
            Err(error) => Ok(unavailable(error)),
        }
    }
    async fn ended_by(
        &self,
        seal: &SignedSeal,
        terminal: LogTerminal,
        ended: LogTerminal,
        head: &LogHead,
        own: bool,
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
                return Ok(Ending::Refused(Refusal::StaleRecovery(
                    "the terminal does not bind the recovery's replaced head".into(),
                )));
            }
            return Ok(Ending::Ended);
        }
        if seal.statement.recovery.is_some() {
            return Ok(Ending::Refused(Refusal::Sealed(
                "another terminal entry ended the tip's log; the recovery never committed".into(),
            )));
        }
        let another_terminal_seal_wins =
            ended.digest != terminal.digest && (own || self.holds_statement(ended.digest).await?);
        Ok(if another_terminal_seal_wins {
            Ending::Lost
        } else {
            Ending::Ended
        })
    }
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
    pub async fn settle_tip_statements(&self) -> Result<usize, Error> {
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
                Err(Error::Rejected(_)) => {
                    self.forget_refused(batch, own).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
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
    pub(in crate::replica) async fn forget_own_batch(
        &self,
        batch: Commitment,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(inner, |inner| inner.forget_tips(batch).await)?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(())
    }
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
    pub(in crate::replica) async fn reseals_waiting_tip(
        &self,
        statements: &[TipStatement],
    ) -> Result<bool, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let published = statements
            .iter()
            .map(TipStatement::id)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut waiting = BTreeSet::new();
        for (_, batch) in inner.pending_tips(Some(true)).await? {
            for statement in batch {
                if let TipStatement::Seal(seal) = &statement
                    && !published.contains(&statement.id()?)
                {
                    waiting.insert(seal.statement.tip);
                }
            }
        }
        Ok(statements.iter().any(|statement| {
            matches!(statement, TipStatement::Seal(seal)
                if seal.statement.recovery.is_none() && waiting.contains(&seal.statement.tip))
        }))
    }
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
                return Err(refusal(Refusal::MigrationPending(
                    "a staged schema switch must finish or be withdrawn before this node signs again".into(),
                )));
            }
        }
        Ok(())
    }
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
    pub(in crate::replica) async fn check_statements(
        &self,
        statements: &[TipStatement],
    ) -> Result<(), Error> {
        self.check_statements_over(statements, false).await
    }
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
                    decode_bounded(statements, MAX_SECURE_BYTES).map_err(|error| {
                        super::super::tips::foreign_finality(statements).unwrap_or(error)
                    })?,
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
pub const COORDINATOR_REQUIRED: &str = "coordinator_required";
pub const COORDINATOR_MISMATCH: &str = "coordinator_mismatch";
fn graph_names(graph: &rocky_credentials::TipGraph, log: &dyn DomainLog) -> Result<bool, Error> {
    Ok(graph
        .coordinator()
        .map_err(credential)?
        .is_none_or(|bundle| bundle.identity == log.identity()))
}
fn another_seal_is_final(
    graph: &rocky_credentials::TipGraph,
    seal: &SignedSeal,
) -> Result<bool, Error> {
    let id = seal.id().map_err(credential)?;
    for other in graph
        .seals
        .iter()
        .filter(|other| other.statement.tip == seal.statement.tip)
    {
        let other = other.id().map_err(credential)?;
        if other != id && graph.is_final(other).map_err(credential)? {
            return Ok(true);
        }
    }
    Ok(false)
}
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
                | TipStatement::Checkpoint(_)
                | TipStatement::Settings(_) => {}
            }
        }
        let before = refused.len();
        for statement in statements {
            let depends = match statement {
                TipStatement::Seal(seal) => tips.contains(&seal.statement.tip),
                TipStatement::Opening(opening) => refused.contains(&opening.statement.seal),
                TipStatement::Finality(record) => refused.contains(&record.seal),
                TipStatement::Checkpoint(record) => refused.contains(&record.statement.seal),
                TipStatement::GenesisLine(_) | TipStatement::Settings(_) => false,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unconfirmed(pub Refusal);
impl std::fmt::Display for Unconfirmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}
impl std::error::Error for Unconfirmed {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    CoordinatorRequired(String),
    CoordinatorMismatch(String),
    CoordinatorUnavailable(String),
    Sealed(String),
    SealPending(String),
    ChainPending(String),
    EntryPending(String),
    RecoveryPending(String),
    Behind(String),
    StaleRecovery(String),
    Denied(String),
    GenesisMinimumMissing(String),
    MigrationPending(String),
}
impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CoordinatorRequired(_) => COORDINATOR_REQUIRED,
            Self::CoordinatorMismatch(_) => COORDINATOR_MISMATCH,
            Self::CoordinatorUnavailable(_) => "coordinator_unavailable",
            Self::Sealed(_) => "sealed",
            Self::SealPending(_) => "seal_pending",
            Self::ChainPending(_) => "chain_pending",
            Self::EntryPending(_) => "entry_pending",
            Self::RecoveryPending(_) => "recovery_pending",
            Self::Behind(_) => "behind",
            Self::StaleRecovery(_) => "stale_recovery",
            Self::Denied(_) => "denied",
            Self::GenesisMinimumMissing(_) => "genesis_minimum_missing",
            Self::MigrationPending(_) => "migration_pending",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::CoordinatorUnavailable(_)
                | Self::Behind(_)
                | Self::RecoveryPending(_)
                | Self::ChainPending(_)
                | Self::SealPending(_)
                | Self::EntryPending(_)
        )
    }
    pub fn message(&self) -> &str {
        match self {
            Self::CoordinatorRequired(message)
            | Self::CoordinatorMismatch(message)
            | Self::CoordinatorUnavailable(message)
            | Self::Sealed(message)
            | Self::SealPending(message)
            | Self::ChainPending(message)
            | Self::EntryPending(message)
            | Self::RecoveryPending(message)
            | Self::Behind(message)
            | Self::StaleRecovery(message)
            | Self::Denied(message)
            | Self::GenesisMinimumMissing(message)
            | Self::MigrationPending(message) => message,
        }
    }
    pub(super) fn with_prefix(self, prefix: &str) -> Self {
        let format = |message: String| format!("{prefix}{message}");
        match self {
            Self::CoordinatorRequired(message) => Self::CoordinatorRequired(format(message)),
            Self::CoordinatorMismatch(message) => Self::CoordinatorMismatch(format(message)),
            Self::CoordinatorUnavailable(message) => Self::CoordinatorUnavailable(format(message)),
            Self::Sealed(message) => Self::Sealed(format(message)),
            Self::SealPending(message) => Self::SealPending(format(message)),
            Self::ChainPending(message) => Self::ChainPending(format(message)),
            Self::EntryPending(message) => Self::EntryPending(format(message)),
            Self::RecoveryPending(message) => Self::RecoveryPending(format(message)),
            Self::Behind(message) => Self::Behind(format(message)),
            Self::StaleRecovery(message) => Self::StaleRecovery(format(message)),
            Self::Denied(message) => Self::Denied(format(message)),
            Self::GenesisMinimumMissing(message) => Self::GenesisMinimumMissing(format(message)),
            Self::MigrationPending(message) => Self::MigrationPending(format(message)),
        }
    }
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}
impl std::error::Error for Refusal {}
pub(in crate::replica) fn refusal(refusal: Refusal) -> Error {
    Error::Application(Box::new(refusal))
}
