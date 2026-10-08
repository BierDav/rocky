//! SQL sessions on the file writer. A transaction leases the write
//! connection: the writer runs only that session's statements until it
//! commits, rolls back or outlives its timeout, and meanwhile answers the
//! requests that write nothing. The connection's state stays on this thread
//! between calls; no lock is held while a caller decides its next statement.
use super::*;
use crate::scoped_connection::Names;
use crate::{
    CommitClass, capture,
    sql::{
        AbortReason, Call, Control, MAX_SAVEPOINTS, Opening, SqlError, SqlOptions, SqlRows,
        SqlState, SqlStatement, classify,
    },
    worker::Sealed,
};
use rocky_core::{Commitment, TransactionToken};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Mutex, MutexGuard, atomic::AtomicBool},
};

pub(super) struct Session {
    /// The slots of the session's domains, in scope order: one for a
    /// domain's own session, every bound domain for a database.
    members: Vec<usize>,
    /// The session's table names with its analyzed statements.
    names: (Names, HashMap<String, String>),
    calls: mpsc::Receiver<Call>,
    /// Received and not yet run.
    pending: Option<Call>,
    /// The handle was dropped.
    closed: bool,
    options: SqlOptions,
    plan: Arc<capture::Plan>,
    cancelled: Arc<AtomicBool>,
    observed: sqlite::Observed,
    /// A rolled back transaction the caller has not acknowledged.
    aborted: Option<AbortReason>,
}
impl Session {
    /// A call or the end of the handle waits to be served.
    pub(super) fn ready(&self) -> bool {
        self.pending.is_some() || self.closed
    }
}
struct Savepoint {
    name: String,
    /// Captured events when it was set.
    mark: usize,
    identities: HashMap<String, u32>,
}
struct Lease {
    identities: HashMap<String, u32>,
    deadline: Instant,
    savepoints: Vec<Savepoint>,
    /// Begun by `SAVEPOINT`: releasing the outermost one commits.
    implicit: bool,
    /// Commits to every statement of the transaction, in order.
    input: Commitment,
    /// The connection's change counter when the transaction began.
    changes: Option<u64>,
}
enum Start {
    Begin(&'static str),
    Savepoint(String),
    /// A writing statement outside a transaction: it commits by itself.
    Statement,
}
struct Executing {
    sql: String,
    params: Vec<SqlValue>,
    token: Option<TransactionToken>,
    reply: Reply<SqlRows>,
}
enum End {
    Commit {
        token: Option<TransactionToken>,
        reply: Reply<SqlRows>,
        rows: SqlRows,
    },
    /// `abort` stays with the session until it acknowledges the rollback.
    Rollback {
        answer: Option<(Reply<SqlRows>, Result<SqlRows, Error>)>,
        abort: Option<AbortReason>,
    },
}
enum Next {
    Call(Call),
    Closed,
    Timeout,
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, Error> {
    mutex
        .lock()
        .map_err(|_| Error::Corrupt("SQL session state".into()))
}
fn refused(message: impl AsRef<str>) -> Error {
    Error::Sql(SqlError::statement(message.as_ref()))
}
fn chain(input: Commitment, sql: &str, params: &[SqlValue]) -> Commitment {
    let mut bytes = input.to_vec();
    bytes.extend((sql.len() as u64).to_le_bytes());
    bytes.extend(sql.as_bytes());
    bytes.extend((params.len() as u64).to_le_bytes());
    for value in params {
        value.encode(&mut bytes);
    }
    rocky_core::commitment("rocky.sql.input.v1", &bytes)
}

impl<A> Writer<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    /// The slots, plan and names of a bound database. Every domain it names
    /// must be open in this file: a missing one is unavailable, never empty.
    fn bind(
        &mut self,
        binding: &rocky_core::DatabaseBinding,
    ) -> Result<(Vec<usize>, Arc<capture::Plan>, Names), Error> {
        let unavailable =
            || Error::Incompatible("a bound domain is not open in this file".to_string());
        let scopes = binding.scopes();
        let mut members = Vec::new();
        for scope in &scopes {
            let slot = self.slots.iter_mut().position(|slot| {
                slot.worker
                    .as_ref()
                    .is_some_and(|worker| worker.scope() == *scope)
            });
            let slot = slot.ok_or_else(unavailable)?;
            // Refuses an application without SQL writes and installs the gate.
            self.slots[slot]
                .worker
                .as_mut()
                .ok_or(Error::Closed)?
                .sql_plan()?;
            members.push(slot);
        }
        let mut parts = Vec::new();
        for (scope, slot) in scopes.iter().zip(&members) {
            let worker = self.slots[*slot].worker.as_ref().ok_or(Error::Closed)?;
            let (schema, names) = worker.sql_part().ok_or_else(unavailable)?;
            let tables = binding.tables.iter().filter(|table| table.scope == *scope);
            parts.push(capture::Part {
                schema,
                names,
                tables: Some(tables.map(|table| table.table.as_str()).collect()),
            });
        }
        let plan = Arc::new(capture::Plan::bound(&parts)?);
        let mut names = HashMap::new();
        for table in &binding.tables {
            let part = scopes.iter().position(|scope| *scope == table.scope);
            let physical = part
                .and_then(|part| parts[part].names.physical(&table.table))
                .ok_or_else(unavailable)?;
            names.insert(table.name.to_ascii_lowercase(), physical.to_string());
        }
        Ok((members, plan, Names::bound(names)))
    }
    pub(super) fn open_session(&mut self, index: usize, opening: Opening) -> Result<(), Error> {
        let (members, plan, names) = match &opening.binding {
            Some(binding) => self.bind(binding)?,
            None => {
                let worker = self.slots[index].worker.as_mut().ok_or(Error::Closed)?;
                let plan = worker.sql_plan()?;
                (vec![index], plan, worker.sql_names().clone())
            }
        };
        self.sessions.push(Session {
            members,
            names: (names, HashMap::new()),
            calls: opening.calls,
            pending: None,
            closed: false,
            options: opening.options,
            plan,
            cancelled: opening.cancelled,
            observed: sqlite::Observed::default(),
            aborted: None,
        });
        Ok(())
    }
    /// Takes one waiting call of each session; true if any is ready.
    pub(super) fn poll_sessions(&mut self, context: &mut std::task::Context<'_>) -> bool {
        let mut ready = false;
        for session in &mut self.sessions {
            if !session.ready() {
                match session.calls.poll_recv(context) {
                    Poll::Ready(Some(call)) => session.pending = Some(call),
                    Poll::Ready(None) => session.closed = true,
                    Poll::Pending => {}
                }
            }
            ready |= session.ready();
        }
        ready
    }
    /// One call of each session that has one; a transaction runs to its end.
    /// Returns true when the writer must stop.
    pub(super) async fn serve_sessions(&mut self) -> bool {
        let mut s = 0;
        while s < self.sessions.len() {
            let session = &mut self.sessions[s];
            // A closed domain or a dropped handle ends the session.
            if session
                .members
                .iter()
                .any(|member| self.slots[*member].worker.is_none())
                || (session.closed && session.pending.is_none())
            {
                self.sessions.swap_remove(s);
                continue;
            }
            if let Some(call) = session.pending.take()
                && self.turn(s, call).await
            {
                return true;
            }
            s += 1;
        }
        false
    }
    async fn reserve_identity(
        &self,
        s: usize,
        table: &str,
        values: &[SqlValue],
        lease: &mut Lease,
    ) -> Result<rocky_core::RowIdentity, Error> {
        let session = &self.sessions[s];
        let mut allocation = None;
        for member in &session.members {
            let worker = self.slots[*member].worker.as_ref().ok_or(Error::Closed)?;
            if worker
                .sql_part()
                .is_some_and(|(schema, _)| schema.table(table).is_some())
            {
                if allocation.is_some() {
                    return Err(refused("ambiguous identity table"));
                }
                allocation = Some(worker.identity_maximum(table, values).await?);
            }
        }
        let (author, highest, reservation) =
            allocation.ok_or_else(|| refused("unknown identity table"))?;
        let highest = highest.max(lease.identities.get(&reservation).copied().unwrap_or(0));
        let identity = rocky_core::RowIdentity::next(author, highest)
            .map_err(|error| refused(error.to_string()))?;
        lease.identities.insert(reservation, identity.sequence());
        Ok(identity)
    }
    async fn turn(&mut self, s: usize, call: Call) -> bool {
        match call {
            Call::NextIdentity(_, _, reply) => {
                let _ = reply.send(Err(refused("next_id requires an active transaction")));
                false
            }
            Call::Cancel => {
                self.sessions[s].cancelled.store(false, Relaxed);
                false
            }
            Call::Prepare(sql, reply) => {
                let _ = reply.send(self.describe(s, &sql));
                false
            }
            Call::Cursor(sql, reply) => {
                let _ = reply.send(self.cursor_parts(s, &sql));
                false
            }
            Call::Catalog(reply) => {
                let _ = reply.send(Ok(self.catalog(s)));
                false
            }
            Call::Execute {
                sql,
                params,
                token,
                reply,
            } => {
                let call = Executing {
                    sql,
                    params,
                    token,
                    reply,
                };
                self.execute(s, call).await
            }
        }
    }
    /// The write connection and `sql` with the session's physical names,
    /// or why the statement is outside the supported surface.
    fn statement(
        &mut self,
        s: usize,
        sql: &str,
    ) -> Result<(Arc<sqlite::Connection>, sqlite::Result<String>), Error> {
        let conn = self.conn().ok_or(Error::Closed)?;
        let (names, analyzed) = &mut self.sessions[s].names;
        if let Some(text) = analyzed.get(sql) {
            return Ok((conn, Ok(text.clone())));
        }
        let text =
            match crate::statement::analyze(sql, |name| names.physical(name).map(str::to_string)) {
                Ok(statement) => statement.sql,
                Err(rejection) => return Ok((conn, Err(rejection.into()))),
            };
        // Bounds caller-generated SQL independently of application width.
        if analyzed.len() >= 256 {
            analyzed.clear();
        }
        analyzed.insert(sql.to_string(), text.clone());
        Ok((conn, Ok(text)))
    }
    /// Authorizes a snapshot cursor: a reading statement over the session's
    /// tables, resolved to physical names. It runs on its own connection.
    fn cursor_parts(&mut self, s: usize, sql: &str) -> Result<crate::sql::CursorParts, Error> {
        let file = self.file.as_ref().ok_or(Error::Closed)?;
        let session = &self.sessions[s];
        let names = &session.names.0;
        let analyzed =
            crate::statement::analyze(sql, |name| names.physical(name).map(str::to_string))
                .map_err(|rejection| Error::Sql(SqlError::engine(&rejection.into())))?;
        if analyzed.kind != crate::statement::Kind::Read {
            return Err(Error::Sql(SqlError::Unsupported(
                "a cursor only reads".into(),
            )));
        }
        let revision = match session.members.as_slice() {
            [member] => {
                let worker = self.slots[*member].worker.as_ref().ok_or(Error::Closed)?;
                let table = worker
                    .sql_names()
                    .physical("rocky_replica")
                    .unwrap_or("rocky_replica");
                Some(format!(
                    "SELECT revision FROM \"{}\" WHERE id=1",
                    table.replace('"', "\"\"")
                ))
            }
            _ => None,
        };
        Ok(crate::sql::CursorParts {
            sql: analyzed.sql,
            readable: session
                .plan
                .tables
                .iter()
                .map(|table| table.physical.clone())
                .collect(),
            path: file.pool.path()?,
            revision,
            host: file.pool.cursors(),
        })
    }
    fn catalog(&self, s: usize) -> Vec<crate::capture::CatalogTable> {
        let session = &self.sessions[s];
        session
            .plan
            .catalog(|table| session.names.0.visible(&table.physical), |_| true)
    }
    /// Compiles without stepping: nothing is admitted or changed.
    fn describe(&mut self, s: usize, sql: &str) -> Result<SqlStatement, Error> {
        if classify(sql).map_err(Error::Sql)? != Control::Statement {
            return Ok(SqlStatement::new(sql, Vec::new(), 0, false));
        }
        let (conn, text) = self.statement(s, sql)?;
        let text = text.map_err(|error| Error::Sql(SqlError::engine(&error)))?;
        let state = conn.session()?;
        lock(&state.gate)?.arm(Some(self.sessions[s].plan.clone()));
        let described = conn.describe(&text);
        lock(&state.gate)?.arm(None);
        let described = described.map_err(|error| Error::Sql(SqlError::engine(&error)))?;
        Ok(SqlStatement::new(
            sql,
            described.columns,
            described.parameters,
            described.read_only,
        ))
    }
    /// Runs one statement under the session's gate, bounds and counters.
    /// The outer error is the runtime's; the inner one the engine's.
    fn step(
        &mut self,
        s: usize,
        sql: &str,
        params: Vec<SqlValue>,
        write: bool,
        deadline: Instant,
    ) -> Result<sqlite::Result<sqlite::Run>, Error> {
        let (conn, text) = self.statement(s, sql)?;
        let text = match text {
            Ok(text) => text,
            Err(rejected) => return Ok(Err(rejected)),
        };
        let state = conn.session()?;
        let capture = conn.capture()?;
        let session = &mut self.sessions[s];
        *lock(&state.observed)? = session.observed;
        lock(&state.gate)?.arm(Some(session.plan.clone()));
        lock(&capture)?.begin_statement();
        let budget = (Instant::now() + session.options.statement_budget).min(deadline);
        let result = conn
            .set_interrupt(budget, session.cancelled.clone())
            .and_then(|()| {
                conn.session_run(
                    &text,
                    storage::bindings(params),
                    session.options.result_bytes,
                    write,
                )
            });
        let cleared = conn.set_deadline(None);
        let assigned = {
            let mut gate = lock(&state.gate)?;
            let assigned = gate.assigned();
            gate.arm(None);
            assigned
        };
        let failed = !matches!(result, Ok(sqlite::Run::Done(_)));
        let inserted = lock(&capture)?.end_statement(failed, &assigned);
        cleared?;
        if let Ok(sqlite::Run::Done(ran)) = &result
            && !ran.read_only
        {
            session.observed.changes = ran.changes as i64;
            session.observed.total_changes = session
                .observed
                .total_changes
                .saturating_add(ran.total_changes as i64);
            if inserted {
                session.observed.last_insert_rowid = ran.last_insert_rowid;
            }
        }
        Ok(result)
    }
    fn rows(&self, s: usize, ran: Option<sqlite::Ran>, state: SqlState) -> Result<SqlRows, Error> {
        let observed = self.sessions[s].observed;
        let (columns, rows) = match ran {
            Some(ran) => (
                ran.columns,
                ran.rows
                    .into_iter()
                    .map(|row| row.into_iter().map(storage::sql_value).collect())
                    .collect::<Result<_, _>>()?,
            ),
            None => (Vec::new(), Vec::new()),
        };
        Ok(SqlRows {
            columns,
            rows,
            changes: observed.changes as u64,
            last_insert_rowid: observed.last_insert_rowid,
            state,
            committed: None,
        })
    }
    fn empty(&self, s: usize, state: SqlState) -> SqlRows {
        let observed = self.sessions[s].observed;
        SqlRows {
            columns: Vec::new(),
            rows: Vec::new(),
            changes: observed.changes as u64,
            last_insert_rowid: observed.last_insert_rowid,
            state,
            committed: None,
        }
    }
    fn answer(
        &self,
        s: usize,
        ran: Result<sqlite::Result<sqlite::Run>, Error>,
        state: SqlState,
    ) -> Result<SqlRows, Error> {
        match ran? {
            Ok(sqlite::Run::Done(ran)) => self.rows(s, Some(ran), state),
            Ok(sqlite::Run::Oversized(_)) => Err(Error::Quota {
                resource: "SQL result bytes",
                limit: self.sessions[s].options.result_bytes,
            }),
            Ok(sqlite::Run::Writes) => Err(Error::Corrupt("statement was not stepped".into())),
            Err(error) => Err(Error::Sql(SqlError::engine(&error))),
        }
    }
    /// A call outside a transaction.
    async fn execute(&mut self, s: usize, call: Executing) -> bool {
        let control = classify(&call.sql);
        if let Some(reason) = self.sessions[s].aborted.clone() {
            let aborted = Err(Error::Sql(SqlError::Aborted(reason)));
            let answer = match control {
                Ok(Control::Rollback) => {
                    self.sessions[s].aborted = None;
                    Ok(self.empty(s, SqlState::Idle))
                }
                Ok(Control::Commit) => {
                    self.sessions[s].aborted = None;
                    aborted
                }
                _ => aborted,
            };
            let _ = call.reply.send(answer);
            return false;
        }
        let start = match control {
            Err(error) => Err(Error::Sql(error)),
            Ok(Control::Begin(mode)) => Ok(Start::Begin(mode)),
            Ok(Control::Savepoint(name)) => Ok(Start::Savepoint(name)),
            Ok(Control::Commit) => Err(refused("cannot commit - no transaction is active")),
            Ok(Control::Rollback) => Err(refused("cannot rollback - no transaction is active")),
            Ok(Control::RollbackTo(name) | Control::Release(name)) => {
                Err(refused(format!("no such savepoint: {name}")))
            }
            Ok(Control::Statement) => {
                let deadline = Instant::now() + self.sessions[s].options.transaction_timeout;
                // A statement that changes nothing needs no transaction.
                match self.step(s, &call.sql, call.params.clone(), false, deadline) {
                    Ok(Ok(sqlite::Run::Writes)) => Ok(Start::Statement),
                    ran => Err(match self.answer(s, ran, SqlState::Idle) {
                        Ok(rows) => {
                            let _ = call.reply.send(Ok(rows));
                            return false;
                        }
                        Err(error) => error,
                    }),
                }
            }
        };
        match start {
            Ok(start) => self.lease(s, start, call).await,
            Err(error) => {
                let _ = call.reply.send(Err(error));
                false
            }
        }
    }
    /// One transaction from its `BEGIN` to its end. Returns true when the
    /// writer must stop.
    async fn lease(&mut self, s: usize, start: Start, first: Executing) -> bool {
        let Some(conn) = self.conn() else {
            let _ = first.reply.send(Err(Error::Closed));
            return true;
        };
        let options = self.sessions[s].options;
        let started = Instant::now();
        // The flush level cannot change inside the transaction.
        let full = options.acknowledge == CommitClass::Flushed
            || self.flush_wanted
            || self.durability == Durability::Full;
        let begin = match &start {
            Start::Begin(mode) => format!("BEGIN {mode}"),
            Start::Savepoint(_) => "BEGIN DEFERRED".into(),
            Start::Statement => "BEGIN IMMEDIATE".into(),
        };
        let begun = match self.synchronous(&conn, full).await {
            Ok(()) => self.begin_as(&conn, &begin).await,
            Err(error) => Err(error),
        };
        let armed = begun.and_then(|()| {
            lock(&*conn.capture()?)?.arm(
                self.sessions[s].plan.clone(),
                (options.captured_rows, options.captured_bytes),
            );
            Ok(())
        });
        if let Err(error) = armed {
            let stop = self.release(s, &conn).await;
            let _ = first.reply.send(Err(error));
            return stop;
        }
        let mut lease = Lease {
            identities: HashMap::new(),
            deadline: started + options.transaction_timeout,
            savepoints: Vec::new(),
            implicit: false,
            input: [0; 32],
            changes: conn.total_changes().ok(),
        };
        let stop = match self.transact(s, &conn, &mut lease, start, first).await {
            End::Rollback { answer, abort } => {
                let stop = self.release(s, &conn).await;
                self.sessions[s].aborted = abort;
                if let Some((reply, answer)) = answer {
                    let _ = reply.send(answer);
                }
                stop
            }
            End::Commit { token, reply, rows } => {
                self.seal(s, &conn, lease, full, token, reply, rows).await
            }
        };
        let elapsed = micros(started);
        self.metrics.leases.fetch_add(1, Relaxed);
        self.metrics.lease_micros.fetch_add(elapsed, Relaxed);
        self.metrics.lease_max_micros.fetch_max(elapsed, Relaxed);
        stop
    }
    async fn savepoint(
        &self,
        conn: &sqlite::Connection,
        lease: &mut Lease,
        name: String,
    ) -> Result<(), Error> {
        if lease.savepoints.len() >= MAX_SAVEPOINTS {
            return Err(Error::Quota {
                resource: "savepoints",
                limit: MAX_SAVEPOINTS,
            });
        }
        // The engine sees positions, never a caller's name.
        conn.execute(
            format!("SAVEPOINT rocky_user_{}", lease.savepoints.len()),
            (),
        )
        .await
        .map_err(|error| Error::Sql(SqlError::engine(&error)))?;
        lease.savepoints.push(Savepoint {
            name,
            mark: lock(&*conn.capture()?)?.mark(),
            identities: lease.identities.clone(),
        });
        Ok(())
    }
    /// The statements of one transaction, until it asks to end or is ended.
    async fn transact(
        &mut self,
        s: usize,
        conn: &sqlite::Connection,
        lease: &mut Lease,
        start: Start,
        first: Executing,
    ) -> End {
        let single = matches!(start, Start::Statement);
        let abort = |reason: AbortReason| End::Rollback {
            answer: None,
            abort: Some(reason),
        };
        lease.input = chain(lease.input, &first.sql, &first.params);
        let mut next = None;
        match start {
            Start::Begin(_) => {
                let _ = first.reply.send(Ok(self.empty(s, SqlState::Active)));
            }
            Start::Savepoint(name) => {
                lease.implicit = true;
                match self.savepoint(conn, lease, name).await {
                    Ok(()) => {
                        let _ = first.reply.send(Ok(self.empty(s, SqlState::Active)));
                    }
                    Err(error) => {
                        return End::Rollback {
                            answer: Some((first.reply, Err(error))),
                            abort: None,
                        };
                    }
                }
            }
            Start::Statement => next = Some(first),
        }
        loop {
            let call = match next.take() {
                Some(call) => call,
                None => match self.lease_next(s, lease.deadline).await {
                    Next::Call(Call::Execute {
                        sql,
                        params,
                        token,
                        reply,
                    }) => {
                        lease.input = chain(lease.input, &sql, &params);
                        Executing {
                            sql,
                            params,
                            token,
                            reply,
                        }
                    }
                    Next::Call(Call::NextIdentity(table, values, reply)) => {
                        let result = self.reserve_identity(s, &table, &values, lease).await;
                        let _ = reply.send(result);
                        continue;
                    }
                    Next::Call(Call::Prepare(sql, reply)) => {
                        let _ = reply.send(self.describe(s, &sql));
                        continue;
                    }
                    Next::Call(Call::Cursor(sql, reply)) => {
                        let _ = reply.send(self.cursor_parts(s, &sql));
                        continue;
                    }
                    Next::Call(Call::Catalog(reply)) => {
                        let _ = reply.send(Ok(self.catalog(s)));
                        continue;
                    }
                    Next::Call(Call::Cancel) => {
                        self.sessions[s].cancelled.store(false, Relaxed);
                        return abort(AbortReason::Cancelled);
                    }
                    // Nobody is left to acknowledge.
                    Next::Closed => {
                        return End::Rollback {
                            answer: None,
                            abort: None,
                        };
                    }
                    Next::Timeout => return abort(AbortReason::Timeout),
                },
            };
            let control = if single {
                Ok(Control::Statement)
            } else {
                classify(&call.sql)
            };
            let position = |name: &str| {
                lease
                    .savepoints
                    .iter()
                    .rposition(|savepoint| savepoint.name == name)
                    .ok_or_else(|| refused(format!("no such savepoint: {name}")))
            };
            let engine = |error: sqlite::Error| Error::Sql(SqlError::engine(&error));
            let answer = match control {
                Err(error) => Err(Error::Sql(error)),
                Ok(Control::Begin(_)) => {
                    Err(refused("cannot start a transaction within a transaction"))
                }
                Ok(Control::Commit) => {
                    return End::Commit {
                        token: call.token,
                        reply: call.reply,
                        rows: self.empty(s, SqlState::Idle),
                    };
                }
                Ok(Control::Rollback) => {
                    return End::Rollback {
                        answer: Some((call.reply, Ok(self.empty(s, SqlState::Idle)))),
                        abort: None,
                    };
                }
                Ok(Control::Savepoint(name)) => self
                    .savepoint(conn, lease, name)
                    .await
                    .map(|()| self.empty(s, SqlState::Active)),
                Ok(Control::Release(name)) => match position(&name) {
                    Err(error) => Err(error),
                    // Releasing the savepoint that began the transaction commits.
                    Ok(0) if lease.implicit => {
                        return End::Commit {
                            token: call.token,
                            reply: call.reply,
                            rows: self.empty(s, SqlState::Idle),
                        };
                    }
                    Ok(position) => conn
                        .execute(format!("RELEASE rocky_user_{position}"), ())
                        .await
                        .map_err(engine)
                        .map(|_| {
                            lease.savepoints.truncate(position);
                            self.empty(s, SqlState::Active)
                        }),
                },
                Ok(Control::RollbackTo(name)) => match position(&name) {
                    Err(error) => Err(error),
                    Ok(position) => {
                        let mark = lease.savepoints[position].mark;
                        let undone = conn
                            .execute(format!("ROLLBACK TO rocky_user_{position}"), ())
                            .await
                            .map_err(engine)
                            .and_then(|_| {
                                // The captured effects go with the rows.
                                lock(&*conn.capture()?)?.truncate(mark);
                                Ok(())
                            });
                        undone.map(|()| {
                            lease.identities = lease.savepoints[position].identities.clone();
                            lease.savepoints.truncate(position + 1);
                            self.empty(s, SqlState::Active)
                        })
                    }
                },
                Ok(Control::Statement) => {
                    let ran = self.step(s, &call.sql, call.params, true, lease.deadline);
                    let failure = conn
                        .capture()
                        .ok()
                        .and_then(|capture| capture.lock().ok().and_then(|sink| sink.failure()));
                    let ended = conn.is_autocommit().unwrap_or(true);
                    // A lone statement has no transaction left to acknowledge.
                    let poisoned = |reason: String, error: Error, reply| End::Rollback {
                        answer: Some((reply, Err(error))),
                        abort: (!single).then_some(AbortReason::Poisoned(reason)),
                    };
                    match ran {
                        _ if failure.is_some() => {
                            let reason = failure.unwrap_or_default();
                            let error = Error::Sql(SqlError::Capture(reason.clone()));
                            return poisoned(reason, error, call.reply);
                        }
                        Ok(Err(sqlite::Error::Denied)) => {
                            return poisoned(
                                "a statement was not authorized".into(),
                                Error::Sql(SqlError::Denied),
                                call.reply,
                            );
                        }
                        Ok(Ok(sqlite::Run::Oversized(true))) => {
                            let ran = Ok(Ok(sqlite::Run::Oversized(true)));
                            let error = self.answer(s, ran, SqlState::Idle).err();
                            return poisoned(
                                "a writing statement returned more than its bound".into(),
                                error.unwrap_or(Error::Closed),
                                call.reply,
                            );
                        }
                        Ok(Ok(sqlite::Run::Done(ran))) if single && !ended => {
                            return match self.rows(s, Some(ran), SqlState::Idle) {
                                Ok(rows) => End::Commit {
                                    token: call.token,
                                    reply: call.reply,
                                    rows,
                                },
                                Err(error) => End::Rollback {
                                    answer: Some((call.reply, Err(error))),
                                    abort: None,
                                },
                            };
                        }
                        ran => {
                            let mut answer = self.answer(s, ran, SqlState::Active);
                            if ended || single {
                                // The engine ended the transaction, as `OR
                                // ROLLBACK` does, or a lone statement failed.
                                let reason = AbortReason::Engine(match &answer {
                                    Err(error) => error.to_string(),
                                    Ok(_) => "the engine ended the transaction".into(),
                                });
                                if answer.is_ok() {
                                    answer = Err(Error::Sql(SqlError::Aborted(reason.clone())));
                                }
                                return End::Rollback {
                                    answer: Some((call.reply, answer)),
                                    abort: (!single).then_some(reason),
                                };
                            }
                            answer
                        }
                    }
                }
            };
            let _ = call.reply.send(answer);
        }
    }
    /// The leaseholder's next call. Meanwhile requests that write nothing
    /// are answered from committed state on the read connection.
    async fn lease_next(&mut self, s: usize, deadline: Instant) -> Next {
        loop {
            self.serve_reads().await;
            let sleep = tokio::time::sleep_until(deadline.into());
            tokio::pin!(sleep);
            let next = std::future::poll_fn(|context| {
                match self.sessions[s].calls.poll_recv(context) {
                    Poll::Ready(Some(call)) => return Poll::Ready(Some(Next::Call(call))),
                    Poll::Ready(None) => return Poll::Ready(Some(Next::Closed)),
                    Poll::Pending => {}
                }
                if sleep.as_mut().poll(context).is_ready() {
                    return Poll::Ready(Some(Next::Timeout));
                }
                // A domain's queue keeps its backpressure behind a waiting write.
                let mut arrived = false;
                for slot in &mut self.slots {
                    if slot.worker.is_none() || slot.drained || !slot.deferred.is_empty() {
                        continue;
                    }
                    match slot.receiver.poll_recv(context) {
                        Poll::Ready(Some(command)) => {
                            slot.deferred.push_back(command);
                            arrived = true;
                        }
                        Poll::Ready(None) => slot.drained = true,
                        Poll::Pending => {}
                    }
                }
                if arrived {
                    Poll::Ready(None)
                } else {
                    Poll::Pending
                }
            })
            .await;
            if let Some(next) = next {
                return next;
            }
        }
    }
    /// Answers each domain's requests up to its first write or first
    /// exposure that needs a flush; those wait for the lease to end.
    async fn serve_reads(&mut self) {
        for index in 0..self.slots.len() {
            while let Some(queued) = self.slots[index].deferred.front() {
                if queued.command.writes() || (queued.command.exposes() && self.dirty(index)) {
                    break;
                }
                let Some(queued) = self.slots[index].deferred.pop_front() else {
                    break;
                };
                let started = Instant::now();
                self.serve(index, queued.command).await;
                self.metrics.read_micros.fetch_add(micros(started), Relaxed);
            }
        }
    }
    /// Ends a lease without a commit: rolls back, restores the flush level
    /// and forgets what was captured. Returns true when the writer must stop.
    async fn release(&mut self, s: usize, conn: &sqlite::Connection) -> bool {
        let mut fatal =
            !conn.is_autocommit().unwrap_or(false) && conn.execute("ROLLBACK", ()).await.is_err();
        let _ = self.synchronous(conn, false).await;
        fatal |= self.disarm(s, conn).is_err();
        fatal
    }
    fn disarm(&mut self, s: usize, conn: &sqlite::Connection) -> Result<(), Error> {
        lock(&*conn.capture()?)?.disarm();
        let mut result = Ok(());
        for member in self.sessions[s].members.clone() {
            if let Some(worker) = self.slots[member].worker.as_mut() {
                result = result.and(worker.sql_discard());
            }
        }
        result
    }
    /// After a rollback: rereads the counters of the domains whose steps had
    /// adopted theirs. Returns true when the writer must stop.
    async fn forget(&mut self, members: &[usize]) -> bool {
        let mut fatal = false;
        for member in members {
            if let Some(worker) = self.slots[*member].worker.as_mut() {
                fatal |= worker.recover().await.is_err();
            }
        }
        fatal
    }
    /// Commits a lease: each domain's worker seals its captured rows as one
    /// step, in scope order, then the engine commits them together and only
    /// then anything is published or answered.
    #[allow(clippy::too_many_arguments)]
    async fn seal(
        &mut self,
        s: usize,
        conn: &sqlite::Connection,
        lease: Lease,
        full: bool,
        token: Option<TransactionToken>,
        reply: Reply<SqlRows>,
        mut rows: SqlRows,
    ) -> bool {
        let members = self.sessions[s].members.clone();
        let plan = self.sessions[s].plan.clone();
        let acknowledge = self.sessions[s].options.acknowledge;
        let frozen = conn.capture().map_err(Error::from).and_then(|capture| {
            let sink = lock(&capture)?;
            let capture_error = |reason| Error::Sql(SqlError::Capture(reason));
            match sink.failure() {
                Some(reason) => Err(capture_error(reason)),
                None => Ok((sink.freeze().map_err(capture_error)?, sink.local_tables())),
            }
        });
        // Each domain seals the rows of its own tables.
        let mut sealed = Vec::new();
        let mut failure = None;
        match frozen {
            Ok((touched, local)) => {
                let mut parts: Vec<_> = members.iter().map(|_| (Vec::new(), Vec::new())).collect();
                for touched in touched {
                    parts[plan.tables[touched.table].owner].0.push(touched);
                }
                for table in local {
                    parts[plan.tables[table].owner].1.push(table);
                }
                for (member, (touched, local)) in members.iter().zip(parts) {
                    let Some(worker) = self.slots[*member].worker.as_mut() else {
                        failure = Some(Error::Closed);
                        break;
                    };
                    worker.in_group = true;
                    let result = worker
                        .sql_commit(&plan, touched, local, token, lease.input)
                        .await;
                    worker.in_group = false;
                    match result {
                        Ok(part) => sealed.push(part),
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                }
            }
            Err(error) => failure = Some(error),
        }
        let replayed = sealed
            .iter()
            .filter(|part| matches!(part, Sealed::Replayed(_)))
            .count();
        if failure.is_none() && replayed != 0 && replayed != sealed.len() {
            failure = Some(Error::Corrupt(
                "a transaction token is recorded in part of its domains".into(),
            ));
        }
        let outcome = match failure {
            None => crate::sql::combine(
                sealed
                    .into_iter()
                    .map(|part| match part {
                        Sealed::Committed(outcome) | Sealed::Replayed(outcome) => outcome,
                    })
                    .collect(),
            ),
            Some(error) => Err(error),
        };
        let mut outcome = match outcome {
            Ok(outcome) if replayed == 0 => outcome,
            Ok(mut outcome) => {
                let mut stop = self.release(s, conn).await;
                // The earlier commit is reported again only once it is durable.
                if outcome.class == CommitClass::Buffered && acknowledge == CommitClass::Flushed {
                    stop |= self.flush().await;
                    if !members.iter().any(|member| self.dirty(*member)) {
                        outcome.class = CommitClass::Flushed;
                    }
                }
                rows.committed = Some(outcome);
                let _ = reply.send(Ok(rows));
                return stop;
            }
            Err(error) => {
                // One domain's refusal rolls every domain's rows back.
                let fatal = matches!(error, Error::Rollback { .. });
                let mut stop = self.release(s, conn).await || fatal;
                if members.len() > 1 && !stop {
                    stop = self.forget(&members).await;
                }
                let _ = reply.send(Err(error));
                return stop;
            }
        };
        let mut failure = None;
        let mut fatal = false;
        for (member, part) in members.iter().zip(&outcome.domains) {
            if failure.is_none()
                && !part.operations.is_empty()
                && let Some(worker) = self.slots[*member].worker.as_mut()
                && worker.maintenance_due()
            {
                worker.in_group = true;
                let result = worker.maintain(1).await;
                worker.in_group = false;
                if matches!(result, Err(Error::Rollback { .. })) {
                    fatal = true;
                    failure = Some(Error::Corrupt("a step could not roll back".into()));
                } else if conn.is_autocommit().unwrap_or(true) {
                    failure = Some(storage::group_lost());
                }
            }
        }
        if self
            .slots
            .iter()
            .filter_map(|slot| slot.worker.as_ref())
            .any(Worker::wants_flush)
        {
            self.flush_wanted = true;
        }
        #[cfg(test)]
        if matches!(self.fault, Some(crate::worker::Fault::AbortBeforeCommit)) {
            std::process::abort();
        }
        #[cfg(test)]
        if failure.is_none() && matches!(self.fault, Some(crate::worker::Fault::FailCommit)) {
            self.fault = None;
            failure = Some(Error::InjectedFailure);
        }
        // A transaction that changed no row writes no frame and flushes nothing.
        let changed = lease.changes.is_none() || lease.changes != conn.total_changes().ok();
        let committing = Instant::now();
        let committed = match failure {
            None => conn
                .execute("COMMIT", ())
                .await
                .map(|_| ())
                .map_err(Error::from),
            Some(error) => Err(error),
        };
        self.metrics
            .commit_micros
            .fetch_add(micros(committing), Relaxed);
        if committed.is_err()
            && !conn.is_autocommit().unwrap_or(false)
            && conn.execute("ROLLBACK", ()).await.is_err()
        {
            fatal = true;
        }
        let _ = self.synchronous(conn, false).await;
        fatal |= self.disarm(s, conn).is_err();
        match committed {
            Ok(()) => {
                #[cfg(test)]
                if matches!(self.fault, Some(crate::worker::Fault::AbortAfterCommit)) {
                    std::process::abort();
                }
                let flushed = full && changed;
                self.metrics.groups.fetch_add(1, Relaxed);
                self.metrics.steps.fetch_add(1, Relaxed);
                if flushed {
                    self.metrics.flushes.fetch_add(1, Relaxed);
                }
                for member in &members {
                    if let Some(worker) = self.slots[*member].worker.as_mut() {
                        worker.publish();
                        if changed {
                            worker.buffered();
                        }
                    }
                }
                if flushed {
                    self.flushed();
                }
                outcome.class = if flushed || !members.iter().any(|member| self.dirty(*member)) {
                    CommitClass::Flushed
                } else {
                    CommitClass::Buffered
                };
                rows.committed = Some(outcome);
                let _ = reply.send(Ok(rows));
                self.checkpoint(conn).await || fatal
            }
            Err(error) => {
                // Nothing is durable: reread what the steps adopted.
                if full {
                    self.flush_failed(&error);
                }
                fatal |= self.forget(&members).await;
                let _ = reply.send(Err(error));
                fatal
            }
        }
    }
}
