//! The commit step of a SQL transaction: reconciles the captured keys with
//! the final rows, turns them into operations through the reducer and
//! records the transaction token, all inside the leased transaction.
use super::*;
use crate::{
    capture::{Plan, Touched},
    sql::{SqlError, TransactionOutcome},
};
use rocky_core::{CapturedRow, TransactionToken, intent_commitment};

pub(crate) enum Sealed {
    Committed(TransactionOutcome),
    /// The token already committed with the same input.
    Replayed(TransactionOutcome),
}
enum Seal {
    Replayed(TransactionOutcome),
    Written {
        operations: Vec<(Envelope, usize)>,
        revision: LocalRevision,
        changed: Changed,
        /// A row a read can see changed.
        visible: bool,
        outcome: TransactionOutcome,
    },
}
fn capture_error(reason: String) -> Error {
    Error::Sql(SqlError::Capture(reason))
}

impl<A> Worker<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    pub(crate) async fn identity_maximum(
        &self,
        table: &str,
        values: &[SqlValue],
    ) -> Result<(u32, u32, String), Error> {
        let writes = self
            .app
            .sql_writes()
            .ok_or_else(|| Error::Incompatible("application has no SQL writes".into()))?;
        let (author, query) = writes
            .identity_allocation_partition(table, self.identity.node, values)
            .map_err(application)?
            .ok_or_else(|| {
                Error::Incompatible("register a row author for this format-2 table".into())
            })?;
        let mut encoded = Vec::new();
        for value in &query.values {
            value.encode(&mut encoded);
        }
        let reservation = format!(
            "{:?}",
            rocky_core::commitment("rocky.sql-identity-reservation.v1", &encoded)
        );
        let rows = storage::collect_rows(
            self.conn()
                .query(&query.sql, bindings(query.values))
                .await?,
        )
        .await?;
        let Some([SqlValue::Integer(highest)]) = rows.first().map(Vec::as_slice) else {
            return Err(Error::Corrupt("own row creation-log maximum".into()));
        };
        Ok((
            author,
            u32::try_from(*highest).map_err(|_| Error::Corrupt("row sequence".into()))?,
            reservation,
        ))
    }
    /// The session classification of this domain's tables.
    pub(crate) fn sql_plan(&mut self) -> Result<Arc<Plan>, Error> {
        if let Some(plan) = &self.sql {
            return Ok(plan.clone());
        }
        let (Some(schema), Some(_)) = (self.app.logical_schema(), self.app.sql_writes()) else {
            return Err(Error::Incompatible(
                "the installed application accepts no SQL writes".into(),
            ));
        };
        let plan = Arc::new(Plan::new(schema, self.writer.names())?);
        self.writer.connection().session()?;
        self.sql = Some(plan.clone());
        Ok(plan)
    }
    pub(crate) fn scope(&self) -> rocky_core::Scope {
        self.identity.scope
    }
    /// The schema and installed names a bound database's plan takes tables from.
    pub(crate) fn sql_part(
        &self,
    ) -> Option<(&rocky_core::LogicalSchema, &crate::scoped_connection::Names)> {
        Some((self.app.logical_schema()?, self.writer.names()))
    }
    /// The installed names a session statement resolves through.
    pub(crate) fn sql_names(&self) -> &crate::scoped_connection::Names {
        self.writer.names()
    }
    /// Forgets the row changes a rolled back transaction left in the tracker.
    pub(crate) fn sql_discard(&mut self) -> Result<(), Error> {
        self.retention.begin_step()
    }
    fn sql_views(&self, table: &str) -> Vec<usize> {
        self.plan.reading(&table.to_ascii_lowercase()).collect()
    }
    /// Seals a leased transaction as one step. `touched` are the keys its
    /// statements changed and `local` the uncaptured tables. Counters and
    /// notifications are adopted here; the caller commits and publishes.
    pub(crate) async fn sql_commit(
        &mut self,
        plan: &Plan,
        touched: Vec<Touched>,
        local: Vec<usize>,
        token: Option<TransactionToken>,
        input: rocky_core::Commitment,
    ) -> Result<Sealed, Error> {
        let capture = self.writer.connection().capture()?;
        let result = self
            .transaction(async |worker| worker.seal(plan, &touched, &local, token, input).await)
            .await;
        capture
            .lock()
            .map_err(|_| Error::Corrupt("row capture".into()))?
            .reconcile(false);
        match result? {
            Seal::Replayed(outcome) => Ok(Sealed::Replayed(outcome)),
            Seal::Written {
                operations,
                revision,
                changed,
                visible,
                outcome,
            } => {
                let wrote = !operations.is_empty();
                for (operation, bytes) in &operations {
                    self.adopt(operation, *bytes, revision);
                }
                self.unpublished
                    .changed(changed, crate::observe::Cause::Local);
                if visible {
                    self.notify(wrote);
                }
                Ok(Sealed::Committed(outcome))
            }
        }
    }
    async fn seal(
        &mut self,
        plan: &Plan,
        touched: &[Touched],
        local: &[usize],
        token: Option<TransactionToken>,
        input: rocky_core::Commitment,
    ) -> Result<Seal, Error> {
        if let Some(token) = token
            && let Some((recorded, outcome)) = self.sql_recorded(token).await?
        {
            return if recorded == input {
                Ok(Seal::Replayed(outcome))
            } else {
                Err(Error::Conflict)
            };
        }
        let mut changed = Changed::default();
        let mut visible = !local.is_empty();
        for index in local {
            changed.record(Some(&self.sql_views(&plan.tables[*index].name)));
            changed.table(&plan.tables[*index].name);
        }
        // Freeze: the first state of each touched key against its final row.
        let mut rows = Vec::new();
        let mut tables = Vec::new();
        for touched in touched {
            let table = &plan.tables[touched.table];
            let found = self
                .writer
                .connection()
                .rows(&table.select, bindings(touched.key.clone()))?;
            let after = match found.as_slice() {
                [] => None,
                [row] => Some(table.image(row).map_err(capture_error)?),
                _ => return Err(Error::Corrupt("a key holds several rows".into())),
            };
            // An equal row is still the application's to judge when a
            // statement assigned it: the update policy may republish it.
            if after == touched.before && !touched.assigned.contains(&true) {
                continue;
            }
            rows.push(CapturedRow {
                table: table.name.clone(),
                before: touched.before.clone(),
                after,
                assigned: touched.assigned.clone(),
                reincarnated: touched.deleted,
            });
            tables.push(touched.table);
        }
        // The tracker saw every event, rolled back ones included, and was
        // reset when this step began: count the net change of each row.
        {
            let mut tracker = self.retention.tracker()?;
            for (row, index) in rows.iter().zip(&tables) {
                let table = &plan.tables[*index];
                if row.before == row.after {
                    continue;
                }
                visible = true;
                changed.record(Some(&self.sql_views(&table.name)));
                changed.table(&table.name);
                let Some((slot, logical)) = tracker
                    .table(&table.physical)
                    .map(|(slot, logical)| (slot, logical.to_string()))
                else {
                    continue;
                };
                let hash = |image: &Vec<SqlValue>| {
                    crate::digest::row_hash(&logical, bindings(image.clone()).iter())
                };
                tracker.record(
                    slot,
                    row.before.as_ref().map(hash),
                    row.after.as_ref().map(hash),
                );
            }
        }
        let writes = self.app.sql_writes().ok_or_else(|| {
            Error::Incompatible("the installed application accepts no SQL writes".into())
        })?;
        let mut loaded = Vec::new();
        for row in &rows {
            for statement in writes.located(row).map_err(application)? {
                loaded.extend(
                    storage::collect_rows(
                        self.conn()
                            .query(&statement.sql, bindings(statement.values))
                            .await?,
                    )
                    .await?,
                );
            }
        }
        let state = self.app.window(loaded).map_err(application)?;
        let mut intents = Vec::new();
        let mut unpublished = Vec::new();
        for (row, index) in rows.iter().zip(&tables) {
            match writes.intent(&state, row).map_err(application)? {
                Some(intent) => intents.push(intent),
                None if row.before == row.after => {}
                None => unpublished.push((row, *index)),
            }
        }
        // From here the runtime writes: the projection must reproduce the
        // rows the transaction left, or the commit is refused.
        let capture = self.writer.connection().capture()?;
        let reconciling = |active: bool| {
            capture
                .lock()
                .map(|mut sink| sink.reconcile(active))
                .map_err(|_| Error::Corrupt("row capture".into()))
        };
        reconciling(true)?;
        for (row, index) in unpublished {
            // Nothing replicated differs, so only derived columns can: the
            // installed rule's earlier value stays authoritative.
            let (Some(before), Some(after)) = (&row.before, &row.after) else {
                return Err(capture_error(format!(
                    "{} gave no operation for an inserted or deleted row",
                    row.table
                )));
            };
            let table = &plan.tables[index];
            if let Some(restore) = &table.restore {
                let mut values = table.derived(before);
                values.extend(table.key_values(after));
                self.writer.connection().rows(restore, bindings(values))?;
            }
        }
        let wrote = !intents.is_empty();
        let revision = if wrote {
            self.revision.next()?
        } else {
            self.revision
        };
        let (mut sequence, mut lamport, mut retained) = (self.sequence, self.lamport, 0usize);
        let mut operations = Vec::new();
        for intent in intents {
            let overflow = rocky_core::ProtocolError::InvalidCounter;
            sequence = sequence.checked_add(1).ok_or(overflow.clone())?;
            lamport = lamport.checked_add(1).ok_or(overflow)?;
            let id = OperationId {
                node: self.identity.node,
                sequence: AuthorSequence::new(sequence)?,
            };
            let entity = intent.entity.unwrap_or(id);
            let request_hash = crate::mutation_hash(&intent.payload, intent.entity);
            let op = Envelope {
                scope: self.identity.scope,
                caller: self.identity.caller,
                versions: self.app.descriptor().operations,
                id,
                lamport: LamportTime::new(lamport)?,
                entity,
                dependencies: required_dependencies(id, entity, intent.entity.is_none())?,
                payload: intent.payload,
            };
            let bytes = op.encode()?;
            self.operation_budget(retained.saturating_add(bytes.len()))?;
            self.app.validate(&op).map_err(application)?;
            self.app.admit(&op, &self.policy).map_err(application)?;
            if !matches!(self.readiness(&op).await?, incoming::Readiness::Ready) {
                return Err(Error::Protocol(rocky_core::ProtocolError::NotReady(
                    op.dependencies.clone(),
                )));
            }
            let statements = self
                .transition(&op, AdmissionBoundary::Local)
                .await?
                .map_err(application)?;
            changed.merge(
                self.write_operation(
                    crate::sql::operation_handle(id),
                    request_hash,
                    &op,
                    &bytes,
                    statements,
                )
                .await?,
            );
            retained = retained
                .saturating_add(bytes.len())
                .saturating_add(RECORD_OVERHEAD);
            operations.push((op, bytes.len()));
        }
        if wrote {
            self.write_counters(sequence, lamport, revision).await?;
        }
        let outcome = self.sql_outcome_of(
            token,
            intent_commitment(&rows),
            operations.iter().map(|(op, _)| op.id).collect(),
            revision,
            crate::CommitClass::Buffered,
            false,
        );
        if let Some(token) = token {
            self.conn()
                .execute(
                    "INSERT INTO rocky_sql_transactions VALUES(?,?,?,?,?,?,?)",
                    crate::sqlite::params![
                        token.0.to_vec(),
                        input.to_vec(),
                        outcome.intent.to_vec(),
                        self.identity.node.0.to_vec(),
                        self.sequence as i64 + 1,
                        operations.len() as i64,
                        revision.get() as i64
                    ],
                )
                .await?;
        }
        reconciling(false)?;
        let diverged = capture
            .lock()
            .map_err(|_| Error::Corrupt("row capture".into()))?
            .failure();
        if let Some(reason) = diverged {
            return Err(capture_error(reason));
        }
        self.inject_fault().await?;
        Ok(Seal::Written {
            operations,
            revision,
            changed,
            visible,
            outcome,
        })
    }
    /// The input and outcome recorded under `token`.
    async fn sql_recorded(
        &self,
        token: TransactionToken,
    ) -> Result<Option<(rocky_core::Commitment, TransactionOutcome)>, Error> {
        let mut records = self
            .conn()
            .query(
                "SELECT input,intent,node,first,count,revision FROM rocky_sql_transactions WHERE token=?",
                [token.0.to_vec()],
            )
            .await?;
        let Some(row) = records.next().await? else {
            return Ok(None);
        };
        let corrupt = || Error::Corrupt("SQL transaction record".into());
        let commitment = |bytes: Vec<u8>| bytes.as_slice().try_into().map_err(|_| corrupt());
        let (first, count): (i64, i64) = (row.get(3)?, row.get(4)?);
        let node = row.get::<Vec<u8>>(2)?;
        let operations = (first..first.saturating_add(count))
            .map(|sequence| incoming::operation_id(&node, sequence))
            .collect::<Result<Vec<_>, _>>()?;
        let flushed = match operations.last() {
            Some(last) => last.sequence.get() <= self.flushed_sequence,
            None => !self.dirty,
        };
        Ok(Some((
            commitment(row.get(0)?)?,
            self.sql_outcome_of(
                Some(token),
                commitment(row.get(1)?)?,
                operations,
                LocalRevision::new(row.get::<i64>(5)? as u64)?,
                if flushed {
                    crate::CommitClass::Flushed
                } else {
                    crate::CommitClass::Buffered
                },
                true,
            ),
        )))
    }
    fn sql_outcome_of(
        &self,
        token: Option<TransactionToken>,
        intent: rocky_core::Commitment,
        operations: Vec<OperationId>,
        revision: LocalRevision,
        class: crate::CommitClass,
        replayed: bool,
    ) -> TransactionOutcome {
        TransactionOutcome {
            token,
            intent,
            operations: operations.clone(),
            revision,
            class,
            replayed,
            domains: vec![crate::sql::DomainOutcome {
                scope: self.identity.scope,
                intent,
                operations,
                revision,
            }],
        }
    }
    pub(super) async fn sql_outcome(
        &mut self,
        token: TransactionToken,
    ) -> Result<Option<TransactionOutcome>, Error> {
        if self.app.sql_writes().is_none() {
            return Ok(None);
        }
        self.read_transaction(async |worker| {
            Ok(worker
                .sql_recorded(token)
                .await?
                .map(|(_, outcome)| outcome))
        })
        .await
    }
}
