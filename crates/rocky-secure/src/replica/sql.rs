use super::*;
use crate::{
    CommitClass,
    capture::Plan,
    sql::{AbortReason, Control, SqlError, SqlOptions, SqlState, classify},
    sqlite::{self, Run},
    storage,
};
use rocky_core::TableClass;
use std::sync::atomic::AtomicBool;
use tokio::sync::OwnedMutexGuard;
use web_time::Instant;

const SQLITE_CONSTRAINT: i32 = 19;

#[derive(Clone, Debug)]
pub struct SecureRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    pub changes: u64,
    pub last_insert_rowid: i64,
    pub state: SqlState,
    pub committed: Option<SecureCommit>,
}
#[derive(Clone, Debug)]
pub struct SecureCommit {
    pub handle: Commitment,
    pub frames: Vec<EncryptedFrame>,
}

struct Held<A: RowApplication>(Guard<OwnedMutexGuard<Inner<A>>>, bool);
impl<A: RowApplication> Drop for Held<A> {
    fn drop(&mut self) {
        let inner = &mut *self.0;
        inner.invalidate_context();
        if self.1 {
            inner.keep_pending_pass();
        }
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
    plan: Arc<Plan>,
    writable: BTreeSet<usize>,
    held: Option<Held<A>>,
    savepoints: Vec<(String, usize, Reserved, usize)>,
    implicit: bool,
    aborted: Option<AbortReason>,
    turn: u64,
    deadline: Instant,
    observed: sqlite::Observed,
    identities: Reserved,
    intents: Vec<super::intents::Recorded>,
}

type Reserved = BTreeMap<ResourceScope, (WriterId, u64)>;

pub struct SecureSql<A: RowApplication> {
    pub(super) session: SecureSession<A>,
    pub(super) reads: Arc<queries::ReadAccess>,
    pub(super) plan: Arc<Plan>,
    pub(super) writable: BTreeSet<usize>,
    pub(super) options: SqlOptions,
    lease: Arc<tokio::sync::Mutex<Lease<A>>>,
    cancelled: Arc<AtomicBool>,
    pub(super) cursors: Arc<std::sync::atomic::AtomicUsize>,
    pub(super) seal: Option<Arc<super::SealViews>>,
    parents: BTreeMap<String, String>,
    migration: bool,
}
impl<A: RowApplication> Drop for SecureSql<A> {
    fn drop(&mut self) {
        if let Ok(mut lease) = self.lease.try_lock() {
            lease.held = None;
        }
    }
}

#[derive(Clone)]
enum Target {
    Bundle(Option<Commitment>),
    Publication(Commitment),
}
enum Sealed {
    Commit(SecureCommit),
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
    pub async fn migration_sql(
        &self,
        publication: Commitment,
        options: SqlOptions,
    ) -> Result<SecureSql<A>, Error> {
        let mut sql = self.sql(options).await?;
        let mut inner = self
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        inner
            .checkpoint_over(super::group::WAL_CHECKPOINT_BYTES)
            .await;
        let records = collect_rows(
            inner
                .conn
                .query(
                    "SELECT stage FROM secure_staged_schema WHERE publication=? AND state='staged'",
                    [publication.to_vec()],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes)]) = records.first().map(Vec::as_slice) else {
            return Err(denied("migration publication is not staged"));
        };
        let stage: super::staging::SchemaStage = decode(bytes)?;
        sql.migration = true;
        for (index, table) in stage.parents.iter().enumerate() {
            sql.parents.insert(
                format!("parent.{}", table.to_ascii_lowercase()),
                history::migration_parent_name(publication, index),
            );
        }
        Ok(sql)
    }
    pub async fn sql(&self, options: SqlOptions) -> Result<SecureSql<A>, Error> {
        options.validate()?;
        let inner = self
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        inner.check_authority(Some(self.authority))?;
        let typed = inner
            .workspace
            .typed
            .clone()
            .ok_or_else(|| Error::Incompatible("no typed tables are installed".into()))?;
        let mut plan = typed.plan(&self.status.readable)?;
        plan.bind_physical_columns(inner.conn.connection())?;
        let writable: BTreeSet<usize> = plan
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
        let plan = Arc::new(plan);
        Ok(SecureSql {
            reads: inner.register_reader(&self.status, self.authority).await?,
            session: self.clone(),
            plan: plan.clone(),
            writable: writable.clone(),
            options,
            lease: Arc::new(tokio::sync::Mutex::new(Lease {
                plan,
                writable,
                held: None,
                savepoints: Vec::new(),
                implicit: false,
                aborted: None,
                turn: 0,
                deadline: Instant::now(),
                observed: sqlite::Observed::default(),
                identities: BTreeMap::new(),
                intents: Vec::new(),
            })),
            cancelled: Arc::default(),
            cursors: Arc::default(),
            seal: None,
            parents: BTreeMap::new(),
            migration: false,
        })
    }
}

impl<A: RowApplication> SecureSql<A> {
    pub async fn status(&self) -> Result<SessionStatus, Error> {
        self.session
            .status_with(crate::replica::Class::Interactive)
            .await
    }

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
            .workspace
            .typed
            .as_ref()
            .ok_or_else(|| refused("no typed tables"))?;
        let table = typed
            .by_name(table)
            .ok_or_else(|| refused("unknown typed table"))?;
        let author = inner
            .node_author
            .as_ref()
            .ok_or_else(|| denied("node author credential missing"))?;
        let own_id = typed
            .rows
            .schema()
            .table(&table.table.sql_name)
            .is_some_and(|logical| logical.partition_by == ["id"]);
        let own_values = if own_id && partition_values.is_none() {
            let highest = match lease.identities.get(&table.scope) {
                Some((number, sequence)) if *number == author.writer => *sequence,
                _ => inner.highest_identity(table.scope, author.writer).await?,
            };
            Some([rocky_core::RowIdentity::next(author.writer, highest)
                .map_err(|error| denied(&error.to_string()))?
                .sql()])
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
            Some(tuple) => super::partition::registered_scope(
                &inner.partitions,
                typed.domain,
                table.table.id,
                tuple,
            )
            .map_err(|_| denied("identity allocation requires a writable table"))?,
            None => table.scope,
        };
        if !self.session.status.writable.contains(&scope) {
            let may_become_writable = !inner.importing.is_empty()
                || inner.importing(scope).await?
                || (inner.partitions.contains_key(&scope)
                    && !self.session.status.scopes.contains(&scope));
            if may_become_writable {
                return Err(Error::Sql(SqlError::Unavailable {
                    table: table.table.sql_name.clone(),
                    reason: crate::sql::Unavailable::Incomplete,
                }));
            }
            return Err(denied("identity allocation requires a writable table"));
        }
        let number = author.writer;
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
                .verified_with_families(
                    inner.config.publisher,
                    &inner.config.revocations,
                    now()?,
                    inner.grant_families(),
                )
                .map_err(credential)?
                .partition_author(author.key.public_key(), scope, tuple, now()?)
                .map_err(credential)?,
            None => author
                .credentials
                .verified_with_families(
                    inner.config.publisher,
                    &inner.config.revocations,
                    now()?,
                    inner.grant_families(),
                )
                .map_err(credential)?
                .author(author.key.public_key(), table.scope.table, now()?)
                .map_err(credential)?,
        };
        if !grants
            .iter()
            .any(|grant| grant.scope == scope && grant.rights.contains(&Right::Insert))
        {
            return Err(denied("identity allocation requires an insert grant"));
        }
        let highest = inner.highest_identity(allocation, author.writer).await?;
        let identity = rocky_core::RowIdentity::next(author.writer, highest)
            .map_err(|error| denied(&error.to_string()))?;
        lease
            .identities
            .insert(allocation, (number, identity.sequence()));
        Ok(identity)
    }
    pub async fn run(&self, sql: &str, params: Vec<SqlValue>) -> Result<SecureRows, Error> {
        self.call(sql, params, Target::Bundle(None)).await
    }
    pub async fn commit(&self, handle: Commitment) -> Result<SecureRows, Error> {
        self.call("COMMIT", Vec::new(), Target::Bundle(Some(handle)))
            .await
    }
    pub async fn commit_publication(&self, publication: Commitment) -> Result<SecureRows, Error> {
        self.call("COMMIT", Vec::new(), Target::Publication(publication))
            .await
    }
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
        target: Target,
    ) -> Result<SecureRows, Error> {
        if self.seal.is_some() {
            return Err(Error::Sql(SqlError::Unsupported(
                "a connection bound to a seal only reads".into(),
            )));
        }
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
                .rposition(|(savepoint, ..)| savepoint.eq_ignore_ascii_case(name))
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
            Control::Commit => self.finish(&mut lease, target, None).await,
            Control::Rollback => {
                lease.held = None;
                Ok(empty(&lease, SqlState::Idle))
            }
            Control::Release(name) => {
                let index = position(&lease, &name)?;
                if index == 0 && lease.implicit {
                    return self.finish(&mut lease, target, None).await;
                }
                self.engine(&lease, &format!("RELEASE rocky_user_{index}"))?;
                lease.savepoints.truncate(index);
                Ok(empty(&lease, SqlState::Active))
            }
            Control::RollbackTo(name) => {
                let index = position(&lease, &name)?;
                if let Some(held) = &lease.held {
                    held.0.invalidate_context();
                }
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
                let (identities, intents) =
                    (lease.savepoints[index].2.clone(), lease.savepoints[index].3);
                lease.identities = identities;
                lease.intents.truncate(intents);
                lease.savepoints.truncate(index + 1);
                Ok(empty(&lease, SqlState::Active))
            }
            Control::Statement => {
                let original = (sql, params.clone());
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
                            .lock(statement_class(sql))
                            .await
                            .partition_statement(sql, &params, &self.session.status)
                            .await?
                    }
                };
                let (sql, params) = (checked.sql.as_str(), checked.params);
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
                                .lock(crate::replica::Class::Background)
                                .await
                                .availability(&analyzed.tables, &self.session.status)
                                .await?
                        }
                    }
                }
                if active {
                    let copies = match self.intent_copies(&lease, original).await {
                        Ok(copies) => copies,
                        Err(error) => {
                            lease.held = None;
                            lease.aborted = Some(AbortReason::Poisoned(error.to_string()));
                            return Err(error);
                        }
                    };
                    self.statement(&mut lease, sql, params, false, copies)
                } else {
                    self.autocommit(&mut lease, (sql, params), original, target)
                        .await
                }
            }
        }
    }
    async fn autocommit(
        &self,
        lease: &mut Lease<A>,
        (sql, params): (&str, Vec<SqlValue>),
        original: (&str, Vec<SqlValue>),
        target: Target,
    ) -> Result<SecureRows, Error> {
        {
            let inner = self.session.replica.inner.lock(statement_class(sql)).await;
            let deadline = Instant::now() + self.options.transaction_timeout;
            match self.step(&inner, lease, sql, params.clone(), false, deadline)? {
                Ok(Run::Writes) => {}
                ran => return self.answer(lease, ran, SqlState::Idle),
            }
        }
        self.begin(lease, "BEGIN IMMEDIATE").await?;
        let rows = match self.intent_copies(lease, original).await {
            Ok(copies) => self.statement(lease, sql, params, true, copies),
            Err(error) => Err(error),
        };
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                lease.held = None;
                return Err(error);
            }
        };
        self.finish(lease, target, Some(rows)).await
    }
    async fn begin(&self, lease: &mut Lease<A>, sql: &str) -> Result<(), Error> {
        let inner = self
            .session
            .replica
            .inner
            .clone()
            .lock_owned(Class::Interactive)
            .await;
        inner.invalidate_frontiers();
        admit(&inner, &self.session)?;
        let domains: BTreeSet<_> = self
            .session
            .status
            .scopes
            .iter()
            .map(|scope| scope.domain)
            .collect();
        for domain in domains {
            let writable = self.session.status.writable.iter().copied();
            inner
                .require_domain_ready(domain, writable.filter(|scope| scope.domain == domain))
                .await?;
        }
        inner.untouch();
        let typed = inner
            .workspace
            .typed
            .as_ref()
            .ok_or_else(|| refused("no typed tables"))?;
        let mut plan = typed.plan(&self.session.status.readable)?;
        plan.bind_physical_columns(inner.conn.connection())?;
        plan.allow_read_only(self.parents.values().cloned());
        lease.writable = plan
            .tables
            .iter()
            .enumerate()
            .filter(|(_, table)| {
                typed.by_name(&table.name).map_or(
                    table.class == TableClass::LocalOnly,
                    |projected| {
                        self.session.status.writable.contains(&projected.scope)
                            || typed.partitioned(projected)
                                && self.session.status.writable.iter().any(|scope| {
                                    scope.domain == projected.scope.domain
                                        && scope.table == projected.scope.table
                                })
                    },
                )
            })
            .map(|(index, _)| index)
            .collect();
        lease.plan = Arc::new(plan);
        let held = Held(inner, false);
        let conn = held.0.conn.connection();
        conn.run_now(sql).map_err(engine)?;
        lease.identities.clear();
        lease.intents.clear();
        conn.capture()?
            .lock()
            .map_err(|_| Error::Corrupt("capture poisoned".into()))?
            .arm(
                lease.plan.clone(),
                (self.options.captured_rows, self.options.captured_bytes),
            );
        lease.held = Some(held);
        lease.savepoints.clear();
        lease.implicit = false;
        lease.turn += 1;
        lease.deadline = Instant::now() + self.options.transaction_timeout;
        self.cancelled.store(false, Ordering::Relaxed);
        let (watched, turn, timeout) = (
            self.lease.clone(),
            lease.turn,
            self.options.transaction_timeout,
        );
        rocky_platform::task::spawn(async move {
            rocky_platform::time::sleep(timeout).await;
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
            .push((name, mark, lease.identities.clone(), lease.intents.len()));
        Ok(())
    }
    fn engine(&self, lease: &Lease<A>, sql: &str) -> Result<(), Error> {
        let held = lease.held.as_ref().ok_or(Error::Closed)?;
        held.0.conn.connection().run_now(sql).map_err(engine)
    }
    fn statement(
        &self,
        lease: &mut Lease<A>,
        sql: &str,
        params: Vec<SqlValue>,
        single: bool,
        copies: Option<Vec<super::intents::Recorded>>,
    ) -> Result<SecureRows, Error> {
        let checked = copies
            .is_some()
            .then(|| rocky_storage::exact::checked_statement(sql))
            .transpose()
            .map_err(|reason| Error::Sql(SqlError::Unsupported(reason)))?;
        let sql = checked.as_deref().unwrap_or(sql);
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
        let outside = touched.iter().any(|table| !lease.writable.contains(table));
        let unreplayed = copies.is_none()
            && touched
                .iter()
                .any(|table| lease.plan.tables[*table].class == TableClass::Replayed);
        let mut poison = |reason: String, error: Error| {
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
            _ if unreplayed => poison(
                "a statement changed a replayed table outside its intent".into(),
                Error::Sql(SqlError::Unsupported(
                    "a statement changed a replayed table outside its intent".into(),
                )),
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
                if answer.is_ok() {
                    lease.intents.extend(copies.into_iter().flatten());
                }
                answer
            }
        }
    }
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
        let analyzed = if self.parents.is_empty() {
            crate::statement::analyze(sql, |_| None)
        } else {
            crate::statement::analyze_migration(sql, |name| self.parents.get(name).cloned())
        };
        let analyzed = match analyzed {
            Ok(analyzed) => analyzed,
            Err(rejection) => return Ok(Err(rejection.into())),
        };
        let sql = analyzed.sql.as_str();
        let conn = inner.conn.connection();
        let state = conn.session()?;
        let capture = conn.capture()?;
        let poisoned = || Error::Corrupt("session state poisoned".into());
        *state.observed.lock().map_err(|_| poisoned())? = lease.observed;
        state
            .gate
            .lock()
            .map_err(|_| poisoned())?
            .arm(Some(lease.plan.clone()));
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
    async fn finish(
        &self,
        lease: &mut Lease<A>,
        target: Target,
        rows: Option<SecureRows>,
    ) -> Result<SecureRows, Error> {
        let mut held = lease.held.take().ok_or(Error::Closed)?;
        lease.savepoints.clear();
        lease.implicit = false;
        let mut rows = rows.unwrap_or_else(|| empty(lease, SqlState::Idle));
        rows.state = SqlState::Idle;
        let inner = &mut *held.0;
        #[cfg(feature = "qualification")]
        super::crash::qualification_kill("before");
        let local = matches!(target, Target::Bundle(_));
        let intents = std::mem::take(&mut lease.intents);
        let sealed = self.seal(inner, &lease.plan, target, &intents).await;
        let committed = match sealed? {
            Sealed::Replayed(committed) => {
                drop(held);
                rows.committed = Some(committed);
                return Ok(rows);
            }
            Sealed::Commit(committed) => committed,
        };
        inner.finish_admission_revision().await?;
        let conn = inner.conn.connection();
        #[cfg(feature = "qualification")]
        if std::env::var("ROCKY_SQL_KILL_AT").is_ok_and(|point| point == "during") {
            conn.qualification_exit_in_commit().map_err(Error::from)?;
        }
        #[cfg(test)]
        if std::mem::take(&mut inner.fail_sql_commit) {
            return Err(Error::InjectedFailure);
        }
        conn.run_now("COMMIT").map_err(Error::from)?;
        #[cfg(feature = "qualification")]
        super::crash::qualification_kill("after");
        let current = local && inner.pending_pass_current(inner.commit);
        inner.note_commit().await;
        held.1 = current;
        let scopes = committed
            .frames
            .iter()
            .map(EncryptedFrame::scope)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let flushed = self.options.acknowledge == CommitClass::Flushed;
        inner.deferred_maintenance.extend(scopes);
        inner.publish_revision().await?;
        if flushed {
            let (flusher, committed_at) = (inner.flusher.clone(), inner.locked_commit());
            drop(held);
            flusher.run(committed_at).await?;
        }
        rows.committed = Some(committed);
        Ok(rows)
    }
    async fn seal(
        &self,
        inner: &mut Inner<A>,
        plan: &Plan,
        target: Target,
        intents: &[super::intents::Recorded],
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
                inner.touch(&plan.tables[index].name);
            }
            inner.cause(crate::observe::Cause::Local);
            sink.disarm();
            touched
        };
        let typed = inner
            .workspace
            .typed
            .clone()
            .ok_or_else(|| Error::Incompatible("no typed tables are installed".into()))?;
        let replayed = touched
            .iter()
            .filter(|touched| plan.tables[touched.table].class == TableClass::Replayed)
            .count();
        if matches!(target, Target::Bundle(_)) && replayed > super::bundle::MAX_BUNDLE_ROWS {
            return Err(Error::Quota {
                resource: "replayed rows",
                limit: super::bundle::MAX_BUNDLE_ROWS,
            });
        }
        let mut undo = BTreeMap::<ResourceScope, Vec<super::replay::Undo>>::new();
        let mut births = BTreeMap::<ResourceScope, Vec<super::checkpoint::IdentityMark>>::new();
        let mut written = Vec::new();
        for touched in touched {
            let table = &plan.tables[touched.table];
            if table.class != TableClass::Replayed {
                continue;
            }
            #[cfg(test)]
            let unchecked = inner.unchecked_replayed_admission;
            #[cfg(not(test))]
            let unchecked = false;
            let Some((change, image)) =
                super::replay::replayed_change(conn, &typed, &inner.partitions, table, &touched)?
            else {
                continue;
            };
            let logical = typed
                .rows
                .schema()
                .table(&table.name)
                .ok_or_else(|| Error::Corrupt("replayed table".into()))?;
            super::replay::note(&mut written, super::replay::written(logical, &image)?);
            if !unchecked
                && let Some(violated) = super::replay::restricted(
                    conn,
                    typed.rows.schema(),
                    logical,
                    touched.before.as_deref(),
                    change.right(),
                )?
            {
                return Err(Error::Sql(SqlError::Statement {
                    code: SQLITE_CONSTRAINT,
                    message: violated,
                }));
            }
            let changed_by_statement = intents
                .iter()
                .any(|recorded| recorded.scope.table == change.scope.table);
            if !unchecked
                && changed_by_statement
                && !change.permitted_by(&inner.local_grants(now()?)?)
            {
                return Err(denied(
                    "row/field transition denied; the replayed effects are not granted",
                ));
            }
            let author = inner.node_author.as_ref().map(|author| author.writer);
            let birth = match inner.replayed_birth(&change, author) {
                Err(_) if unchecked => None,
                birth => birth?,
            };
            if let Some(identity) = birth {
                let signer = inner
                    .node_author
                    .as_ref()
                    .ok_or_else(|| denied("node author credential missing"))?
                    .key
                    .public_key();
                births
                    .entry(change.scope)
                    .or_default()
                    .push(super::checkpoint::IdentityMark {
                        author: identity.writer(),
                        signer,
                        sequence: identity.sequence(),
                    });
            }
            undo.entry(change.scope)
                .or_default()
                .push(super::replay::Undo {
                    table: table.name.clone(),
                    key: touched.key,
                    before: touched.before,
                });
        }
        let mut allocated = BTreeMap::<ResourceScope, Vec<u64>>::new();
        let own = inner.node_author.as_ref().map(|author| author.writer);
        for (scope, marks) in &births {
            for mark in marks.iter().filter(|mark| Some(mark.author) == own) {
                allocated
                    .entry(inner.identity_allocation_scope(*scope))
                    .or_default()
                    .push(mark.sequence);
            }
        }
        for (scope, mut sequences) in allocated {
            sequences.sort_unstable();
            let author = own.ok_or_else(|| denied("node author credential missing"))?;
            let mut highest = inner.highest_identity(scope, author).await?;
            for sequence in sequences {
                if Some(sequence) != highest.checked_add(1) {
                    return Err(denied(&format!(
                        "insert id must continue the device's own table creation log: author {author}, highest {highest}, received {sequence}"
                    )));
                }
                highest = sequence;
            }
        }
        for (scope, marks) in &births {
            for mark in marks {
                inner
                    .put_identity_mark(*scope, mark, super::identity::Raise::Own)
                    .await?;
            }
        }
        admit(inner, &self.session)?;
        let intents = super::intents::grouped(intents)?;
        if intents.len() > 1 {
            let named: Vec<(&str, &[rocky_core::replay::IntentStatement])> = intents
                .iter()
                .filter_map(|(scope, statements)| {
                    Some((
                        typed.rows.table(&scope.table)?.sql_name.as_str(),
                        statements.as_slice(),
                    ))
                })
                .collect();
            super::replay::check_member_order(typed.rows.schema(), named.iter().copied())?;
        }
        if let Some(violated) = inner.replayed_violation(&written, false).await? {
            return Err(Error::Sql(SqlError::Statement {
                code: SQLITE_CONSTRAINT,
                message: violated,
            }));
        }
        let handle = match target {
            Target::Publication(publication) if intents.is_empty() => {
                let (frames, replayed) = inner.stage_publication(publication).await?;
                let committed = SecureCommit {
                    handle: publication,
                    frames,
                };
                return Ok(if replayed {
                    Sealed::Replayed(committed)
                } else {
                    Sealed::Commit(committed)
                });
            }
            Target::Publication(publication) => {
                let committed = SecureCommit {
                    handle: publication,
                    frames: Vec::new(),
                };
                let staged = inner
                    .stage_intents(publication, with_undo(intents, undo), &births)
                    .await?;
                return Ok(if staged {
                    Sealed::Commit(committed)
                } else {
                    Sealed::Replayed(committed)
                });
            }
            Target::Bundle(handle) => handle,
        };
        let handle = handle.unwrap_or_else(rocky_platform::entropy::random);
        if intents.is_empty() {
            return Ok(Sealed::Commit(SecureCommit {
                handle,
                frames: Vec::new(),
            }));
        }
        let encoded = super::bundle::input_bytes(&[], &[], &intents)?;
        let intents = with_undo(intents, undo);
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
                    "transaction handle was committed before the pruned prefix",
                ));
            }
            None => {}
        }
        let frames = inner
            .author_sql_bundle((handle, input), intents, now()?)
            .await?;
        for frame in &frames {
            if let Some(births) = births.get(&frame.scope()?) {
                inner.record_births(frame, births).await?;
            }
        }
        Ok(Sealed::Commit(SecureCommit { handle, frames }))
    }
}

impl<A: RowApplication> SecureSql<A> {
    async fn intent_copies(
        &self,
        lease: &Lease<A>,
        (sql, params): (&str, Vec<SqlValue>),
    ) -> Result<Option<Vec<super::intents::Recorded>>, Error> {
        let held = lease.held.as_ref().ok_or(Error::Closed)?;
        held.0
            .intent_copies(
                sql,
                &params,
                &self.session.status,
                if self.migration {
                    usize::MAX
                } else {
                    rocky_storage::partition_sql::MAX_INTENT_PARTITIONS
                },
                &self.parents,
            )
            .await
    }
}

fn with_undo(
    intents: BTreeMap<ResourceScope, Vec<rocky_core::replay::IntentStatement>>,
    mut undo: BTreeMap<ResourceScope, Vec<super::replay::Undo>>,
) -> super::bundle::Intents {
    let mut intents: super::bundle::Intents = intents
        .into_iter()
        .map(|(scope, statements)| (scope, (statements, undo.remove(&scope).unwrap_or_default())))
        .collect();
    if let Some((_, (_, first))) = intents.iter_mut().next() {
        first.extend(undo.into_values().flatten());
    }
    intents
}

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

#[cfg(test)]
impl<A: RowApplication> SecureSql<A> {
    pub(crate) async fn transactional_frontier(
        &self,
        scope: ResourceScope,
    ) -> Result<(ScopeFrontier, usize), Error> {
        let lease = self.lease.lock().await;
        let held = lease.held.as_ref().ok_or(Error::Closed)?;
        let frontier = held.0.tip_frontier(scope).await?;
        Ok((frontier, held.0.frontier_cache_stats().1))
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn fail_next_sql_commit(&self) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .fail_sql_commit = true;
    }
}

fn statement_class(sql: &str) -> Class {
    if crate::statement::analyze(sql, |_| None)
        .is_ok_and(|statement| statement.kind != crate::statement::Kind::Read)
    {
        Class::Interactive
    } else {
        Class::Background
    }
}
