//! SQL sessions on a secure replica's typed tables. A transaction holds the
//! replica; its commit admits the changed rows as one bundle in the same
//! engine transaction. See `docs/runtime/secure-sql.md`.
use super::*;
use crate::{
    CommitClass,
    capture::Plan,
    sql::{AbortReason, Control, SqlError, SqlOptions, SqlState, classify},
    sqlite::{self, Run},
    storage,
};
use rocky_core::{TableClass, UpdatePolicy};
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use tokio::sync::OwnedMutexGuard;

/// A statement's whole result. The committing call carries `committed`.
#[derive(Clone, Debug)]
pub struct SecureRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    /// Rows the session's last writing statement changed.
    pub changes: u64,
    pub last_insert_rowid: i64,
    pub state: SqlState,
    pub committed: Option<SecureCommit>,
}
/// What a commit published: one bundle, or nothing when no replicated row changed.
#[derive(Clone, Debug)]
pub struct SecureCommit {
    pub handle: Commitment,
    pub frames: Vec<EncryptedFrame>,
}

/// The replica held for one transaction. Dropping it rolls back whatever
/// the engine still has open, so a cancelled call cannot leave a
/// transaction behind.
struct Held<A: RowApplication>(OwnedMutexGuard<Inner<A>>);
impl<A: RowApplication> Drop for Held<A> {
    fn drop(&mut self) {
        let inner = &mut *self.0;
        inner.strict_projection = false;
        let conn = inner.conn.connection();
        if !conn.is_autocommit().unwrap_or(true) {
            let _ = conn.run_now("ROLLBACK");
        }
        if let Ok(capture) = conn.capture()
            && let Ok(mut sink) = capture.lock()
        {
            sink.disarm();
        }
    }
}
struct Lease<A: RowApplication> {
    held: Option<Held<A>>,
    /// Caller names of open savepoints and their capture marks.
    savepoints: Vec<(String, usize, Reserved)>,
    /// A `SAVEPOINT` began the transaction; releasing it commits.
    implicit: bool,
    aborted: Option<AbortReason>,
    /// Counts transactions, so a timeout ends only its own.
    turn: u64,
    deadline: Instant,
    observed: sqlite::Observed,
    identities: Reserved,
}

/// Each table scope's author and the last sequence a transaction reserved.
type Reserved = BTreeMap<ResourceScope, (u32, u32)>;

/// A SQL connection of a `SecureSession`. It may read the typed tables of
/// the session's readable scopes and every local table, and write the
/// mergeable tables of its writable scopes. Each statement checks the
/// session's authority; the commit admits every changed row and field.
pub struct SecureSql<A: RowApplication> {
    pub(super) session: SecureSession<A>,
    pub(super) plan: Arc<Plan>,
    /// Plan tables a statement may change.
    pub(super) writable: BTreeSet<usize>,
    pub(super) options: SqlOptions,
    lease: Arc<tokio::sync::Mutex<Lease<A>>>,
    cancelled: Arc<AtomicBool>,
    /// Open snapshot cursors of this session.
    pub(super) cursors: Arc<std::sync::atomic::AtomicUsize>,
}
impl<A: RowApplication> Drop for SecureSql<A> {
    fn drop(&mut self) {
        if let Ok(mut lease) = self.lease.try_lock() {
            lease.held = None;
        }
    }
}

enum Sealed {
    Commit(SecureCommit),
    /// The handle committed these frames before; this transaction rolls back.
    Replayed(SecureCommit),
}
fn refused(message: impl Into<String>) -> Error {
    Error::Sql(SqlError::statement(&message.into()))
}
fn engine(error: sqlite::Error) -> Error {
    match error {
        sqlite::Error::Busy(_) => Error::from(error),
        error => Error::Sql(SqlError::engine(&error)),
    }
}

impl<A: RowApplication> SecureSession<A> {
    /// A SQL connection over the replica's installed typed tables.
    pub async fn sql(&self, options: SqlOptions) -> Result<SecureSql<A>, Error> {
        options.validate()?;
        let inner = self.replica.inner.lock().await;
        inner.check_authority(Some(self.authority))?;
        let typed = inner
            .typed
            .clone()
            .ok_or_else(|| Error::Incompatible("no typed tables are installed".into()))?;
        let plan = typed.plan(&self.status.readable)?;
        let writable = plan
            .tables
            .iter()
            .enumerate()
            .filter(|(_, table)| match typed.by_name(&table.name) {
                Some(projected) => {
                    self.status.writable.contains(&projected.scope)
                        || (typed.partitioned(projected)
                            && self.status.writable.iter().any(|scope| {
                                scope.table == projected.scope.table
                                    && scope.domain == projected.scope.domain
                            }))
                }
                None => table.class == TableClass::LocalOnly,
            })
            .map(|(index, _)| index)
            .collect();
        Ok(SecureSql {
            session: self.clone(),
            plan: Arc::new(plan),
            writable,
            options,
            lease: Arc::new(tokio::sync::Mutex::new(Lease {
                held: None,
                savepoints: Vec::new(),
                implicit: false,
                aborted: None,
                turn: 0,
                deadline: Instant::now(),
                observed: sqlite::Observed::default(),
                identities: BTreeMap::new(),
            })),
            cancelled: Arc::default(),
            cursors: Arc::default(),
        })
    }
}

impl<A: RowApplication> SecureSql<A> {
    /// Commits a move as its own atomic bundle; an active SQL transaction must end first.
    pub async fn move_row(
        &self,
        handle: Commitment,
        table: &str,
        id: rocky_core::RowIdentity,
        from: &[SqlValue],
        to: &[SqlValue],
    ) -> Result<SecureCommit, Error> {
        let lease = self.lease.lock().await;
        if lease.held.is_some() || lease.aborted.is_some() {
            return Err(denied("move requires an idle SQL session"));
        }
        let frames = self.session.move_row(handle, table, id, from, to).await?;
        Ok(SecureCommit { handle, frames })
    }
    /// Reserves a row id in an active transaction, before inserting it.
    /// Rollback releases reservations; a committed insert must use the next
    /// sequence of the table author's creation log.
    pub async fn next_id(&self, table: &str) -> Result<rocky_core::RowIdentity, Error> {
        self.reserve_id(table, None).await
    }
    pub async fn next_id_partition(
        &self,
        table: &str,
        values: &[SqlValue],
    ) -> Result<rocky_core::RowIdentity, Error> {
        self.reserve_id(table, Some(values)).await
    }
    async fn reserve_id(
        &self,
        table: &str,
        partition_values: Option<&[SqlValue]>,
    ) -> Result<rocky_core::RowIdentity, Error> {
        let mut lease = self.lease.lock().await;
        let held = lease
            .held
            .as_ref()
            .ok_or_else(|| refused("next_id requires an active transaction"))?;
        let inner = &held.0;
        admit(inner, &self.session)?;
        let typed = inner
            .typed
            .as_ref()
            .ok_or_else(|| refused("no typed tables"))?;
        let table = typed
            .by_name(table)
            .ok_or_else(|| refused("unknown typed table"))?;
        if table.confirmed() {
            return Err(denied("identity allocation requires a writable table"));
        }
        let author = inner
            .row_authors
            .get(&table.scope.table)
            .ok_or_else(|| denied("register a table pseudonym before issuing row identities"))?;
        let own_id = typed
            .rows
            .schema()
            .table(&table.table.sql_name)
            .is_some_and(|logical| logical.partition_by == ["id"]);
        let own_values = if own_id && partition_values.is_none() {
            let highest = match lease.identities.get(&table.scope) {
                Some((number, sequence)) if *number == author.number => *sequence,
                _ => inner.highest_identity(table.scope, author.number).await?,
            };
            Some([SqlValue::Integer(
                rocky_core::RowIdentity::next(author.number, highest)
                    .map_err(|error| denied(&error.to_string()))?
                    .integer(),
            )])
        } else {
            None
        };
        let partition_values = own_values
            .as_ref()
            .map(|values| values.as_slice())
            .or(partition_values);
        let tuple = if typed.partitioned(table) {
            let logical = typed
                .rows
                .schema()
                .table(&table.table.sql_name)
                .ok_or_else(|| denied("unknown table"))?;
            let values = partition_values.ok_or_else(|| {
                denied("partitioned identity allocation requires partition values")
            })?;
            if values.len() != logical.partition_by.len() {
                return Err(denied("partition value count"));
            }
            let mut row = vec![SqlValue::Null; logical.columns.len()];
            for (name, value) in logical.partition_by.iter().zip(values) {
                let position = logical
                    .columns
                    .iter()
                    .position(|column| column.sql_name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| denied("unknown partition column"))?;
                row[position] = value.clone();
            }
            typed
                .rows
                .partition(&table.table, &row)
                .map_err(|error| denied(&error))?
        } else {
            if partition_values.is_some() {
                return Err(denied("unpartitioned table takes no partition values"));
            }
            None
        };
        let scope = match &tuple {
            Some(tuple) => tuple
                .scope(typed.domain, table.table.id)
                .map_err(|error| denied(&error.to_string()))?,
            None => table.scope,
        };
        if !self.session.status.writable.contains(&scope) {
            return Err(denied("identity allocation requires a writable table"));
        }
        // Later reservations of this transaction continue the first, whose
        // author it verified; the commit verifies every inserted id again.
        let number = author.number;
        let allocation = inner.identity_allocation_scope(scope);
        if let Some(&(reserved, sequence)) = lease.identities.get(&allocation)
            && reserved == number
        {
            let identity = rocky_core::RowIdentity::next(number, sequence)
                .map_err(|error| denied(&error.to_string()))?;
            lease
                .identities
                .insert(allocation, (number, identity.sequence()));
            return Ok(identity);
        }
        let grants = match &tuple {
            Some(tuple) => author
                .credentials
                .verify_partition_author(
                    inner.config.publisher,
                    inner.manifest,
                    author.key.public_key(),
                    author.number,
                    scope,
                    tuple,
                    &inner.config.revocations,
                    now()?,
                )
                .map_err(credential)?,
            None => author
                .credentials
                .verify_author(
                    inner.config.publisher,
                    inner.manifest,
                    author.key.public_key(),
                    author.number,
                    table.scope.table,
                    &inner.config.revocations,
                    now()?,
                )
                .map_err(credential)?,
        };
        if !grants
            .iter()
            .any(|grant| grant.scope == scope && grant.rights.contains(&Right::Insert))
        {
            return Err(denied("identity allocation requires an insert grant"));
        }
        let highest = inner.highest_identity(allocation, author.number).await?;
        let identity = rocky_core::RowIdentity::next(author.number, highest)
            .map_err(|error| denied(&error.to_string()))?;
        lease
            .identities
            .insert(allocation, (number, identity.sequence()));
        Ok(identity)
    }
    /// Runs one statement. Transaction control is interpreted here, as in
    /// a trusted `SqlSession`.
    pub async fn run(&self, sql: &str, params: Vec<SqlValue>) -> Result<SecureRows, Error> {
        self.call(sql, params, None).await
    }
    /// Commits the open transaction under `handle`. The same handle and
    /// changed rows return the committed frames again; other rows conflict.
    pub async fn commit(&self, handle: Commitment) -> Result<SecureRows, Error> {
        self.call("COMMIT", Vec::new(), Some(handle)).await
    }
    /// Rolls the open transaction back; later statements report
    /// `Aborted(Cancelled)` until the caller sends `ROLLBACK`.
    pub async fn cancel(&self) {
        let mut lease = self.lease.lock().await;
        if lease.held.take().is_some() {
            lease.aborted = Some(AbortReason::Cancelled);
        }
    }
    pub async fn state(&self) -> SqlState {
        if self.lease.lock().await.held.is_some() {
            SqlState::Active
        } else {
            SqlState::Idle
        }
    }
    async fn call(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
        handle: Option<Commitment>,
    ) -> Result<SecureRows, Error> {
        let control = classify(sql).map_err(Error::Sql)?;
        let mut lease = self.lease.lock().await;
        if let Some(reason) = lease.aborted.clone() {
            return match control {
                Control::Rollback => {
                    lease.aborted = None;
                    Ok(empty(&lease, SqlState::Idle))
                }
                Control::Commit => {
                    lease.aborted = None;
                    Err(Error::Sql(SqlError::Aborted(reason)))
                }
                _ => Err(Error::Sql(SqlError::Aborted(reason))),
            };
        }
        let active = lease.held.is_some();
        let position = |lease: &Lease<A>, name: &str| {
            lease
                .savepoints
                .iter()
                .rposition(|(savepoint, _, _)| savepoint.eq_ignore_ascii_case(name))
                .ok_or_else(|| refused(format!("no such savepoint: {name}")))
        };
        match control {
            Control::Begin(_) if active => {
                Err(refused("cannot start a transaction within a transaction"))
            }
            Control::Begin(mode) => {
                self.begin(&mut lease, &format!("BEGIN {mode}")).await?;
                Ok(empty(&lease, SqlState::Active))
            }
            Control::Savepoint(name) => {
                if !active {
                    self.begin(&mut lease, "BEGIN DEFERRED").await?;
                    lease.implicit = true;
                }
                if let Err(error) = self.savepoint(&mut lease, name) {
                    if !active {
                        lease.held = None;
                    }
                    return Err(error);
                }
                Ok(empty(&lease, SqlState::Active))
            }
            Control::Commit | Control::Rollback | Control::Release(_) | Control::RollbackTo(_)
                if !active =>
            {
                Err(refused("no transaction is active"))
            }
            Control::Commit => self.finish(&mut lease, handle, None).await,
            Control::Rollback => {
                lease.held = None;
                Ok(empty(&lease, SqlState::Idle))
            }
            Control::Release(name) => {
                let index = position(&lease, &name)?;
                if index == 0 && lease.implicit {
                    return self.finish(&mut lease, handle, None).await;
                }
                self.engine(&lease, &format!("RELEASE rocky_user_{index}"))?;
                lease.savepoints.truncate(index);
                Ok(empty(&lease, SqlState::Active))
            }
            Control::RollbackTo(name) => {
                let index = position(&lease, &name)?;
                self.engine(&lease, &format!("ROLLBACK TO rocky_user_{index}"))?;
                let mark = lease.savepoints[index].1;
                if let Some(held) = &lease.held {
                    held.0
                        .conn
                        .connection()
                        .capture()?
                        .lock()
                        .map_err(|_| Error::Corrupt("capture poisoned".into()))?
                        .truncate(mark);
                }
                lease.identities = lease.savepoints[index].2.clone();
                lease.savepoints.truncate(index + 1);
                Ok(empty(&lease, SqlState::Active))
            }
            Control::Statement => {
                let checked = match &lease.held {
                    Some(held) => {
                        held.0
                            .partition_statement(sql, &params, &self.session.status)
                            .await?
                    }
                    None => {
                        self.session
                            .replica
                            .inner
                            .lock()
                            .await
                            .partition_statement(sql, &params, &self.session.status)
                            .await?
                    }
                };
                let sql = checked.as_str();
                // A read of a table whose rows are not all here is refused,
                // never answered as absence.
                if let Ok(analyzed) = crate::statement::analyze(sql, |_| None)
                    && analyzed.kind == crate::statement::Kind::Read
                {
                    match &lease.held {
                        Some(held) => {
                            held.0
                                .availability(&analyzed.tables, &self.session.status)
                                .await?
                        }
                        None => {
                            self.session
                                .replica
                                .inner
                                .lock()
                                .await
                                .availability(&analyzed.tables, &self.session.status)
                                .await?
                        }
                    }
                }
                if active {
                    self.statement(&mut lease, sql, params, false)
                } else {
                    self.autocommit(&mut lease, sql, params, handle).await
                }
            }
        }
    }
    /// A statement outside a transaction: one that writes nothing runs at
    /// once; a writing one commits by itself.
    async fn autocommit(
        &self,
        lease: &mut Lease<A>,
        sql: &str,
        params: Vec<SqlValue>,
        handle: Option<Commitment>,
    ) -> Result<SecureRows, Error> {
        {
            let inner = self.session.replica.inner.lock().await;
            let deadline = Instant::now() + self.options.transaction_timeout;
            match self.step(&inner, lease, sql, params.clone(), false, deadline)? {
                Ok(Run::Writes) => {}
                ran => return self.answer(lease, ran, SqlState::Idle),
            }
        }
        self.begin(lease, "BEGIN IMMEDIATE").await?;
        let rows = match self.statement(lease, sql, params, true) {
            Ok(rows) => rows,
            Err(error) => {
                lease.held = None;
                return Err(error);
            }
        };
        self.finish(lease, handle, Some(rows)).await
    }
    async fn begin(&self, lease: &mut Lease<A>, sql: &str) -> Result<(), Error> {
        let inner = self.session.replica.inner.clone().lock_owned().await;
        admit(&inner, &self.session)?;
        inner.untouch();
        let held = Held(inner);
        let conn = held.0.conn.connection();
        conn.run_now(sql).map_err(engine)?;
        lease.identities.clear();
        conn.capture()?
            .lock()
            .map_err(|_| Error::Corrupt("capture poisoned".into()))?
            .arm(
                self.plan.clone(),
                (self.options.captured_rows, self.options.captured_bytes),
            );
        lease.held = Some(held);
        lease.savepoints.clear();
        lease.implicit = false;
        lease.turn += 1;
        lease.deadline = Instant::now() + self.options.transaction_timeout;
        self.cancelled.store(false, Ordering::Relaxed);
        let (watched, turn, deadline) = (self.lease.clone(), lease.turn, lease.deadline);
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline.into()).await;
            let mut lease = watched.lock().await;
            if lease.turn == turn && lease.held.take().is_some() {
                lease.aborted = Some(AbortReason::Timeout);
            }
        });
        Ok(())
    }
    fn savepoint(&self, lease: &mut Lease<A>, name: String) -> Result<(), Error> {
        if lease.savepoints.len() >= crate::sql::MAX_SAVEPOINTS {
            return Err(Error::Quota {
                resource: "savepoints",
                limit: crate::sql::MAX_SAVEPOINTS,
            });
        }
        // The engine sees positions, never a caller's name.
        self.engine(
            lease,
            &format!("SAVEPOINT rocky_user_{}", lease.savepoints.len()),
        )?;
        let mark = match &lease.held {
            Some(held) => held
                .0
                .conn
                .connection()
                .capture()?
                .lock()
                .map_err(|_| Error::Corrupt("capture poisoned".into()))?
                .mark(),
            None => 0,
        };
        lease
            .savepoints
            .push((name, mark, lease.identities.clone()));
        Ok(())
    }
    fn engine(&self, lease: &Lease<A>, sql: &str) -> Result<(), Error> {
        let held = lease.held.as_ref().ok_or(Error::Closed)?;
        held.0.conn.connection().run_now(sql).map_err(engine)
    }
    /// One statement inside the transaction. A refused or uncapturable
    /// statement ends the transaction; later statements report it.
    fn statement(
        &self,
        lease: &mut Lease<A>,
        sql: &str,
        params: Vec<SqlValue>,
        single: bool,
    ) -> Result<SecureRows, Error> {
        let held = lease.held.take().ok_or(Error::Closed)?;
        let mark = held
            .0
            .conn
            .connection()
            .capture()?
            .lock()
            .map_err(|_| Error::Corrupt("capture poisoned".into()))?
            .mark();
        let ran = self.step(&held.0, lease, sql, params, true, lease.deadline);
        let conn = held.0.conn.connection();
        let (failure, touched) = {
            let capture = conn.capture()?;
            let sink = capture
                .lock()
                .map_err(|_| Error::Corrupt("capture poisoned".into()))?;
            (sink.failure(), sink.tables_since(mark))
        };
        let ended = conn.is_autocommit().unwrap_or(true);
        let outside = touched.iter().any(|table| !self.writable.contains(table));
        let mut poison = |reason: String, error: Error| {
            // `held` drops here and rolls back.
            if !single {
                lease.aborted = Some(AbortReason::Poisoned(reason));
            }
            Err(error)
        };
        let ran = match ran {
            Err(error) => return poison(error.to_string(), error),
            Ok(ran) => ran,
        };
        match ran {
            _ if failure.is_some() => {
                let reason = failure.unwrap_or_default();
                poison(reason.clone(), Error::Sql(SqlError::Capture(reason)))
            }
            Err(sqlite::Error::Denied) => poison(
                "a statement was not authorized".into(),
                Error::Sql(SqlError::Denied),
            ),
            _ if outside => poison(
                "a statement wrote a table the session may not write".into(),
                Error::Sql(SqlError::Denied),
            ),
            Ok(Run::Oversized(true)) => poison(
                "a writing statement returned more than its bound".into(),
                Error::Quota {
                    resource: "SQL result bytes",
                    limit: self.options.result_bytes,
                },
            ),
            ran if ended => {
                let reason = AbortReason::Engine(match &ran {
                    Err(error) => error.to_string(),
                    _ => "the engine ended the transaction".into(),
                });
                if !single {
                    lease.aborted = Some(reason.clone());
                }
                match self.answer(lease, ran, SqlState::Idle) {
                    Err(error) => Err(error),
                    Ok(_) => Err(Error::Sql(SqlError::Aborted(reason))),
                }
            }
            ran => {
                let answer = self.answer(lease, ran, SqlState::Active);
                lease.held = Some(held);
                answer
            }
        }
    }
    /// Steps one statement under the session's gate and bounds.
    fn step(
        &self,
        inner: &Inner<A>,
        lease: &mut Lease<A>,
        sql: &str,
        params: Vec<SqlValue>,
        write: bool,
        deadline: Instant,
    ) -> Result<sqlite::Result<Run>, Error> {
        admit(inner, &self.session)?;
        // Typed tables keep their names here; the surface is still checked.
        if let Err(rejection) = crate::statement::analyze(sql, |_| None) {
            return Ok(Err(rejection.into()));
        }
        let conn = inner.conn.connection();
        let state = conn.session()?;
        let capture = conn.capture()?;
        let poisoned = || Error::Corrupt("session state poisoned".into());
        *state.observed.lock().map_err(|_| poisoned())? = lease.observed;
        state
            .gate
            .lock()
            .map_err(|_| poisoned())?
            .arm(Some(self.plan.clone()));
        capture.lock().map_err(|_| poisoned())?.begin_statement();
        let budget = (Instant::now() + self.options.statement_budget).min(deadline);
        let result = conn
            .set_interrupt(budget, self.cancelled.clone())
            .and_then(|()| {
                conn.session_run(
                    sql,
                    storage::bindings(params),
                    self.options.result_bytes,
                    write,
                )
            });
        let cleared = conn.set_deadline(None);
        let assigned = {
            let mut gate = state.gate.lock().map_err(|_| poisoned())?;
            let assigned = gate.assigned();
            gate.arm(None);
            assigned
        };
        let failed = !matches!(result, Ok(Run::Done(_)));
        let inserted = capture
            .lock()
            .map_err(|_| poisoned())?
            .end_statement(failed, &assigned);
        cleared?;
        if let Ok(Run::Done(ran)) = &result
            && !ran.read_only
        {
            lease.observed.changes = ran.changes as i64;
            lease.observed.total_changes = lease
                .observed
                .total_changes
                .saturating_add(ran.total_changes as i64);
            if inserted {
                lease.observed.last_insert_rowid = ran.last_insert_rowid;
            }
        }
        Ok(result)
    }
    fn answer(
        &self,
        lease: &Lease<A>,
        ran: sqlite::Result<Run>,
        state: SqlState,
    ) -> Result<SecureRows, Error> {
        match ran {
            Ok(Run::Done(ran)) => Ok(SecureRows {
                columns: ran.columns,
                rows: ran
                    .rows
                    .into_iter()
                    .map(|row| row.into_iter().map(storage::sql_value).collect())
                    .collect::<Result<_, _>>()?,
                ..empty(lease, state)
            }),
            Ok(Run::Oversized(_)) => Err(Error::Quota {
                resource: "SQL result bytes",
                limit: self.options.result_bytes,
            }),
            Ok(Run::Writes) => Err(Error::Corrupt("statement was not stepped".into())),
            Err(error) => Err(Error::Sql(SqlError::engine(&error))),
        }
    }
    /// Seals and commits the transaction, or rolls it back on any failure.
    async fn finish(
        &self,
        lease: &mut Lease<A>,
        handle: Option<Commitment>,
        rows: Option<SecureRows>,
    ) -> Result<SecureRows, Error> {
        let mut held = lease.held.take().ok_or(Error::Closed)?;
        lease.savepoints.clear();
        lease.implicit = false;
        let mut rows = rows.unwrap_or_else(|| empty(lease, SqlState::Idle));
        rows.state = SqlState::Idle;
        let inner = &mut *held.0;
        #[cfg(feature = "qualification")]
        qualification_kill("before");
        let sealed = self.seal(inner, handle).await;
        inner.strict_projection = false;
        let committed = match sealed? {
            // `held` rolls the repeated transaction back.
            Sealed::Replayed(committed) => {
                drop(held);
                rows.committed = Some(committed);
                return Ok(rows);
            }
            Sealed::Commit(committed) => committed,
        };
        let conn = inner.conn.connection();
        #[cfg(feature = "qualification")]
        if std::env::var("ROCKY_SQL_KILL_AT").is_ok_and(|point| point == "during") {
            conn.qualification_exit_in_commit().map_err(Error::from)?;
        }
        conn.run_now("COMMIT").map_err(Error::from)?;
        #[cfg(feature = "qualification")]
        qualification_kill("after");
        inner.note_commit().await;
        inner.publish_revision().await?;
        let scopes = committed
            .frames
            .iter()
            .map(EncryptedFrame::scope)
            .collect::<Result<BTreeSet<_>, _>>()?;
        if self.options.acknowledge == CommitClass::Flushed {
            // The frames may leave the device.
            inner.flush()?;
            inner.maintain(&scopes).await;
            inner.checkpoint_if_due().await;
        } else {
            // Buffered frames leave only through a call that flushes first,
            // such as a transfer page; `EncryptedReplica::maintain` runs the
            // maintenance step after this commit answered.
            inner.deferred_maintenance.extend(scopes);
        }
        rows.committed = Some(committed);
        Ok(rows)
    }
    /// Turns the captured rows into writes and authors them as one bundle
    /// inside the open engine transaction.
    async fn seal(
        &self,
        inner: &mut Inner<A>,
        handle: Option<Commitment>,
    ) -> Result<Sealed, Error> {
        let capture_error = |reason: String| Error::Sql(SqlError::Capture(reason));
        let conn = inner.conn.connection();
        let touched = {
            let capture = conn.capture()?;
            let mut sink = capture
                .lock()
                .map_err(|_| Error::Corrupt("capture poisoned".into()))?;
            if let Some(reason) = sink.failure() {
                return Err(capture_error(reason));
            }
            let touched = sink.freeze().map_err(capture_error)?;
            for index in touched
                .iter()
                .map(|touched| touched.table)
                .chain(sink.local_tables())
            {
                inner.touch(&self.plan.tables[index].name);
            }
            inner.cause(crate::observe::Cause::Local);
            sink.disarm();
            touched
        };
        let typed = inner
            .typed
            .clone()
            .ok_or_else(|| Error::Incompatible("no typed tables are installed".into()))?;
        let assigned_policy = typed.rows.schema().update_policy == UpdatePolicy::Assigned;
        let mut writes = Vec::new();
        for touched in touched {
            let table = &self.plan.tables[touched.table];
            if table.class != TableClass::Mergeable {
                continue;
            }
            let projected = typed
                .by_name(&table.name)
                .ok_or_else(|| Error::Corrupt(format!("{} is not typed", table.name)))?;
            let after = match conn
                .rows(&table.select, storage::bindings(touched.key.clone()))?
                .first()
            {
                Some(values) => Some(table.image(values).map_err(capture_error)?),
                None => None,
            };
            if let Some(after) = &after {
                inner
                    .check_references(typed.row_scope(projected, after)?, after)
                    .await?;
            }
            let mut before = touched.before;
            // Values a constraint policy shows as NULL stay merged unless the
            // transaction wrote a value; references must name typed rows.
            let mut substituted = Vec::new();
            if let Some(before) = before.as_mut() {
                substituted = inner
                    .merged_image(typed.row_scope(projected, before)?, before)
                    .await?;
            }
            let mut after = after;
            if let (Some(after), Some(before), false) =
                (after.as_mut(), before.as_ref(), touched.deleted)
            {
                for column in &substituted {
                    if after[*column] == SqlValue::Null {
                        after[*column] = before[*column].clone();
                    }
                }
            }
            let Some(anchor) = after.as_ref().or(before.as_ref()) else {
                continue;
            };
            let scope = typed.row_scope(projected, anchor)?;
            if !self.session.status.writable.contains(&scope) {
                return Err(denied("row partition is outside this session"));
            }
            let fields = |image: &Vec<SqlValue>| typed.rows.fields(&projected.table, image);
            if touched.deleted && before.is_some() && after.is_some() {
                for (old, new) in [(before.as_ref(), None), (None, after.as_ref())] {
                    let anchor = old
                        .or(new)
                        .ok_or_else(|| capture_error("missing incarnation image".into()))?;
                    let row = typed
                        .rows
                        .row_id(typed.domain.domain, &projected.table, anchor)
                        .map_err(capture_error)?;
                    writes.push(RowWrite::changed(RowChange {
                        scope,
                        row,
                        before: old.map(fields).transpose().map_err(capture_error)?,
                        after: new.map(fields).transpose().map_err(capture_error)?,
                    }));
                }
                continue;
            }
            let row = typed
                .rows
                .row_id(typed.domain.domain, &projected.table, anchor)
                .map_err(capture_error)?;
            let fields = |image: &Vec<SqlValue>| typed.rows.fields(&projected.table, image);
            // A SQL deletion removes the natural key, including held hidden incarnations.
            if after.is_none() && before.is_some() {
                let held = collect_rows(inner.conn.query(
                    "SELECT r.fields FROM secure_row_keys k JOIN secure_rows r ON r.scope=k.scope AND r.row=k.row WHERE k.scope=? AND k.key=?",
                    [encode(&scope)?, row.to_vec()],
                ).await?).await?;
                let visible = before
                    .as_ref()
                    .map(fields)
                    .transpose()
                    .map_err(capture_error)?;
                for held in held {
                    let [SqlValue::Blob(bytes)] = held.as_slice() else {
                        return Err(Error::Corrupt("SQL deleted incarnation".into()));
                    };
                    let record: Materialized = decode(bytes)?;
                    if !record.deleted && Some(&record.fields) != visible.as_ref() {
                        writes.push(RowWrite::changed(RowChange {
                            scope,
                            row,
                            before: Some(record.fields),
                            after: None,
                        }));
                    }
                }
            }
            let reinserted = touched.deleted && before.is_some() && after.is_some();
            let assigned = if before.is_none() || after.is_none() {
                BTreeSet::new()
            } else if reinserted {
                // A deleted and reinserted key republishes its whole row.
                projected.table.fields.iter().copied().collect()
            } else if assigned_policy {
                projected
                    .table
                    .fields
                    .iter()
                    .zip(&touched.assigned)
                    .filter(|(_, assigned)| **assigned)
                    .map(|(field, _)| *field)
                    .collect()
            } else {
                BTreeSet::new()
            };
            let mut assigned = assigned;
            for column in &substituted {
                assigned.remove(&projected.table.fields[*column]);
            }
            let mut write = RowWrite {
                change: RowChange {
                    scope,
                    row,
                    before: before
                        .as_ref()
                        .map(fields)
                        .transpose()
                        .map_err(capture_error)?,
                    after: after
                        .as_ref()
                        .map(fields)
                        .transpose()
                        .map_err(capture_error)?,
                },
                assigned,
            };
            // A foreign key's columns publish as one unit.
            if write.change.before.is_some() && write.change.after.is_some() {
                let published = write.published_fields();
                for unit in projected.table.units() {
                    if !unit.is_disjoint(&published) {
                        write.assigned.extend(unit.iter().copied());
                    }
                }
            }
            if write.right().is_some() {
                writes.push(write);
            }
        }
        admit(inner, &self.session)?;
        let handle = handle.unwrap_or_else(|| {
            let mut handle = [0; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut handle);
            handle
        });
        if writes.is_empty() {
            return Ok(Sealed::Commit(SecureCommit {
                handle,
                frames: Vec::new(),
            }));
        }
        let encoded = encode(&(&writes, Vec::<Hash>::new()))?;
        if encoded.len() > MAX_BUNDLE_BYTES {
            return Err(Error::Quota {
                resource: "bundle bytes",
                limit: MAX_BUNDLE_BYTES,
            });
        }
        let input = rocky_core::commitment("rocky.secure-bundle-input.v1", &encoded);
        match inner.lookup_bundle(handle, input).await? {
            Some(Ok(frames)) => return Ok(Sealed::Replayed(SecureCommit { handle, frames })),
            Some(Err(_)) => {
                return Err(denied(
                    "transaction handle was committed before the pruning horizon",
                ));
            }
            None => {}
        }
        inner.strict_projection = true;
        let frames = inner
            .author_bundle(handle, input, &writes, &[], now()?)
            .await?;
        Ok(Sealed::Commit(SecureCommit { handle, frames }))
    }
}

/// A statement or commit runs only under the session's current authority.
fn admit<A: RowApplication>(inner: &Inner<A>, session: &SecureSession<A>) -> Result<(), Error> {
    inner.check_authority(Some(session.authority))?;
    inner.local_grants(now()?)?;
    Ok(())
}
fn empty<A: RowApplication>(lease: &Lease<A>, state: SqlState) -> SecureRows {
    SecureRows {
        columns: Vec::new(),
        rows: Vec::new(),
        changes: lease.observed.changes as u64,
        last_insert_rowid: lease.observed.last_insert_rowid,
        state,
        committed: None,
    }
}

// Only qualification builds read this variable. Exit without destructors so
// SQLite must recover the pending WAL/transaction on the next process open.
#[cfg(feature = "qualification")]
fn qualification_kill(point: &str) {
    if std::env::var("ROCKY_SQL_KILL_AT").is_ok_and(|selected| selected == point) {
        std::process::exit(87);
    }
}
