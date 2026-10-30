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

/// SQLite's result code for a violated constraint.
const SQLITE_CONSTRAINT: i32 = 19;

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
/// transaction behind. The flag keeps the pending pass after a plain commit.
struct Held<A: RowApplication>(Guard<OwnedMutexGuard<Inner<A>>>, bool);
impl<A: RowApplication> Drop for Held<A> {
    fn drop(&mut self) {
        let inner = &mut *self.0;
        inner.strict_projection = false;
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
    /// Caller names of open savepoints, their capture marks, reservations
    /// and recorded intents.
    savepoints: Vec<(String, usize, Reserved, usize)>,
    /// A `SAVEPOINT` began the transaction; releasing it commits.
    implicit: bool,
    aborted: Option<AbortReason>,
    /// Counts transactions, so a timeout ends only its own.
    turn: u64,
    deadline: Instant,
    observed: sqlite::Observed,
    identities: Reserved,
    /// Partition copies of the transaction's replayed writes, in order.
    intents: Vec<super::intents::Recorded>,
}

/// Each table scope's author and the last sequence a transaction reserved.
type Reserved = BTreeMap<ResourceScope, (u32, u32)>;

/// A SQL connection of a `SecureSession`. It may read the typed tables of
/// the session's readable scopes and every local table, and write the
/// mergeable tables of its writable scopes. Each statement checks the
/// session's authority; the commit admits every changed row and field.
pub struct SecureSql<A: RowApplication> {
    pub(super) session: SecureSession<A>,
    pub(super) reads: Arc<queries::ReadAccess>,
    pub(super) plan: Arc<Plan>,
    /// Plan tables a statement may change.
    pub(super) writable: BTreeSet<usize>,
    pub(super) options: SqlOptions,
    lease: Arc<tokio::sync::Mutex<Lease<A>>>,
    cancelled: Arc<AtomicBool>,
    /// Open snapshot cursors of this session.
    pub(super) cursors: Arc<std::sync::atomic::AtomicUsize>,
    /// The kept seal its reads see instead of the live tables.
    pub(super) seal: Option<Arc<super::SealViews>>,
    parents: BTreeMap<String, String>,
    #[cfg(any(test, feature = "intent-shapes"))]
    shape: std::sync::Mutex<crate::shapes::Pending>,
}
impl<A: RowApplication> Drop for SecureSql<A> {
    fn drop(&mut self) {
        if let Ok(mut lease) = self.lease.try_lock() {
            lease.held = None;
        }
    }
}

/// What a commit publishes the transaction as.
#[derive(Clone)]
enum Target {
    /// One bundle, under the caller's handle or a random one.
    Bundle(Option<Commitment>),
    /// The consecutive bundles of a staged publication.
    Publication(Commitment),
    Move {
        handle: Commitment,
        input: Commitment,
        table: String,
        id: rocky_core::RowIdentity,
        from: Vec<SqlValue>,
        to: Vec<SqlValue>,
    },
}
/// What a commit resolves besides its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Plain,
    /// The held change it re-authors.
    Reauthors(Commitment),
    Coordinated(Slot),
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
    /// A local migration session can read only its journaled frozen parents.
    pub async fn migration_sql(
        &self,
        publication: Commitment,
        options: SqlOptions,
    ) -> Result<SecureSql<A>, Error> {
        let mut sql = self.sql(options).await?;
        let inner = self
            .replica
            .inner
            .lock(crate::replica::Class::Background)
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
        for (index, table) in stage.parents.iter().enumerate() {
            sql.parents.insert(
                format!("parent.{}", table.to_ascii_lowercase()),
                history::migration_parent_name(publication, index),
            );
        }
        Ok(sql)
    }
    /// A SQL connection over the replica's installed typed tables.
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
            #[cfg(any(test, feature = "intent-shapes"))]
            shape: Default::default(),
        })
    }
}

impl<A: RowApplication> SecureSql<A> {
    /// The SQL session's bindings, checked as interactive work.
    pub async fn status(&self) -> Result<SessionStatus, Error> {
        self.session
            .status_with(crate::replica::Class::Interactive)
            .await
    }

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
        #[cfg(any(test, feature = "intent-shapes"))]
        {
            let mut shape = self.shape();
            shape.begin();
            shape.committed(crate::shapes::Origin::Move);
        }
        Ok(SecureCommit { handle, frames })
    }
    /// Executes related SQL and a move as one bundle. Commands may delete cascade
    /// children or change other rows, but may not edit rows carried by the move.
    pub async fn move_row_with(
        &self,
        handle: Commitment,
        table: &str,
        id: rocky_core::RowIdentity,
        from: &[SqlValue],
        to: &[SqlValue],
        statements: &[(String, Vec<SqlValue>)],
    ) -> Result<SecureCommit, Error> {
        if self.seal.is_some() {
            return Err(denied("a seal is read only"));
        }
        let input = rocky_core::commitment(
            "rocky.row-move-sql-input.v1",
            &encode(&(
                table,
                id,
                super::moves::scalar_bytes(from),
                super::moves::scalar_bytes(to),
                statements
                    .iter()
                    .map(|(sql, values)| (sql, super::moves::scalar_bytes(values)))
                    .collect::<Vec<_>>(),
            ))?,
        );
        let mut lease = self.lease.lock().await;
        if lease.held.is_some() || lease.aborted.is_some() {
            return Err(denied("move requires an idle SQL session"));
        }
        self.begin(&mut lease, "BEGIN IMMEDIATE").await?;
        let result = async {
            let inner = &lease.held.as_ref().ok_or(Error::Closed)?.0;
            if let Some(previous) = inner.lookup_bundle(handle, input).await? {
                return Ok(SecureCommit {
                    handle,
                    frames: previous.map_err(|_| denied("move handle is behind pruned prefix"))?,
                });
            }
            for (sql, params) in statements {
                if !matches!(classify(sql).map_err(Error::Sql)?, Control::Statement) {
                    return Err(refused("move changes must be ordinary SQL statements"));
                }
                self.statement(&mut lease, sql, params.clone(), false, None)?;
            }
            self.finish(
                &mut lease,
                Target::Move {
                    handle,
                    input,
                    table: table.into(),
                    id,
                    from: from.to_vec(),
                    to: to.to_vec(),
                },
                None,
                Kind::Plain,
            )
            .await?
            .committed
            .ok_or(Error::Closed)
        }
        .await;
        // Replays and every error roll back provisional SQL without publication.
        lease.held = None;
        lease.aborted = None;
        result
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
            // While this device imports, or for a known partition this
            // session predates, the table may become writable: retry.
            if !inner.importing.is_empty()
                || inner.importing(scope).await?
                || (inner.partitions.contains_key(&scope)
                    && !self.session.status.scopes.contains(&scope))
            {
                return Err(Error::Sql(SqlError::Unavailable {
                    table: table.table.sql_name.clone(),
                    reason: crate::sql::Unavailable::Incomplete,
                }));
            }
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
        self.call(sql, params, Target::Bundle(None), Kind::Plain)
            .await
    }
    /// Commits the open transaction under `handle`. The same handle and
    /// changed rows return the committed frames again; other rows conflict.
    pub async fn commit(&self, handle: Commitment) -> Result<SecureRows, Error> {
        self.call(
            "COMMIT",
            Vec::new(),
            Target::Bundle(Some(handle)),
            Kind::Plain,
        )
        .await
    }
    /// Commits the open transaction as the re-authoring of the held change
    /// `held`, which leaves the list in the same transaction. The rows are
    /// an ordinary new bundle stamped now.
    pub async fn commit_reauthoring(
        &self,
        handle: Commitment,
        held: Commitment,
    ) -> Result<SecureRows, Error> {
        self.call(
            "COMMIT",
            Vec::new(),
            Target::Bundle(Some(handle)),
            Kind::Reauthors(held),
        )
        .await
    }
    /// Commits the open transaction as a coordinated write staged at
    /// `slot`: the bundle stays unsent until `EncryptedReplica::propose`
    /// commits it, and is voided if it aborts. Durable on return.
    pub async fn commit_coordinated(
        &self,
        handle: Commitment,
        slot: Slot,
    ) -> Result<SecureRows, Error> {
        let trusted = &self.session.replica.domain_log;
        trusted.begin_proposal(slot.log);
        let staged = self
            .call(
                "COMMIT",
                Vec::new(),
                Target::Bundle(Some(handle)),
                Kind::Coordinated(slot),
            )
            .await;
        if staged.is_err() {
            trusted.end_proposal(slot.log);
        }
        staged
    }
    /// Commits the open transaction, of any size, as the consecutive
    /// bundles of `publication`, each within the bundle bounds, parents
    /// first. Each bundle's handle derives from `publication` and its
    /// position; the record of the publication commits in the same
    /// transaction. A recorded publication returns its frames again and
    /// this transaction rolls back. See [`EncryptedReplica::publications`].
    pub async fn commit_publication(&self, publication: Commitment) -> Result<SecureRows, Error> {
        self.call(
            "COMMIT",
            Vec::new(),
            Target::Publication(publication),
            Kind::Plain,
        )
        .await
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
        target: Target,
        commit: Kind,
    ) -> Result<SecureRows, Error> {
        // A bound session reads the seal through cursors; the live tables
        // are not its to read or write.
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
            Control::Commit => self.finish(&mut lease, target, None, commit).await,
            Control::Rollback => {
                lease.held = None;
                Ok(empty(&lease, SqlState::Idle))
            }
            Control::Release(name) => {
                let index = position(&lease, &name)?;
                if index == 0 && lease.implicit {
                    return self.finish(&mut lease, target, None, Kind::Plain).await;
                }
                self.engine(&lease, &format!("RELEASE rocky_user_{index}"))?;
                lease.savepoints.truncate(index);
                #[cfg(any(test, feature = "intent-shapes"))]
                self.shape().release(index);
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
                #[cfg(any(test, feature = "intent-shapes"))]
                self.shape().rollback_to(index);
                Ok(empty(&lease, SqlState::Active))
            }
            Control::Statement => {
                #[cfg(any(test, feature = "intent-shapes"))]
                self.shape().statement(sql, &params);
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
                                .lock(crate::replica::Class::Background)
                                .await
                                .availability(&analyzed.tables, &self.session.status)
                                .await?
                        }
                    }
                }
                if active {
                    let copies = self.intent_copies(&lease, original).await?;
                    self.statement(&mut lease, sql, params, false, copies)
                } else {
                    self.autocommit(&mut lease, (sql, params), original, target)
                        .await
                }
            }
        }
    }
    /// A statement outside a transaction: one that writes nothing runs at
    /// once; a writing one commits by itself.
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
        self.finish(lease, target, Some(rows), Kind::Plain).await
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
        // Each written scope's own blockers refuse its frames at commit.
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
        #[cfg(any(test, feature = "intent-shapes"))]
        self.shape().begin();
        lease.deadline = Instant::now() + self.options.transaction_timeout;
        self.cancelled.store(false, Ordering::Relaxed);
        // The watcher waits on the runtime's clock; `deadline` bounds the
        // engine's work by the wall clock.
        let (watched, turn, timeout) = (
            self.lease.clone(),
            lease.turn,
            self.options.transaction_timeout,
        );
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
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
            .push((name, mark, lease.identities.clone(), lease.intents.len()));
        #[cfg(any(test, feature = "intent-shapes"))]
        self.shape().savepoint();
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
        copies: Option<Vec<super::intents::Recorded>>,
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
        let outside = touched.iter().any(|table| !lease.writable.contains(table));
        // Only a split statement may change a replayed table; anything else
        // would change rows no replica replays.
        let unreplayed = copies.is_none()
            && touched
                .iter()
                .any(|table| lease.plan.tables[*table].class == TableClass::Replayed);
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
                #[cfg(any(test, feature = "intent-shapes"))]
                if answer.is_ok() {
                    self.shape().succeeded();
                }
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
    /// Seals and commits the transaction, or rolls it back on any failure.
    async fn finish(
        &self,
        lease: &mut Lease<A>,
        target: Target,
        rows: Option<SecureRows>,
        commit: Kind,
    ) -> Result<SecureRows, Error> {
        let mut held = lease.held.take().ok_or(Error::Closed)?;
        lease.savepoints.clear();
        lease.implicit = false;
        let mut rows = rows.unwrap_or_else(|| empty(lease, SqlState::Idle));
        rows.state = SqlState::Idle;
        let inner = &mut *held.0;
        #[cfg(feature = "qualification")]
        qualification_kill("before");
        #[cfg(any(test, feature = "intent-shapes"))]
        let origin = self.origin(&target, commit);
        // Only a plain bundle is local authoring alone.
        let local = matches!(target, Target::Bundle(_)) && commit == Kind::Plain;
        let intents = std::mem::take(&mut lease.intents);
        let sealed = self
            .seal(inner, &lease.plan, target, commit, &intents)
            .await;
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
        #[cfg(any(test, feature = "intent-shapes"))]
        self.shape().committed(origin);
        #[cfg(feature = "qualification")]
        qualification_kill("after");
        let current = local && inner.pending_pass_current(inner.commit);
        inner.note_commit().await;
        held.1 = current;
        let scopes = committed
            .frames
            .iter()
            .map(EncryptedFrame::scope)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let flushed = self.options.acknowledge == CommitClass::Flushed
            || matches!(commit, Kind::Coordinated(_));
        // Queue before announcing the revision, including when the caller flushes.
        inner.deferred_maintenance.extend(scopes);
        inner.publish_revision().await?;
        if flushed {
            // The frames may leave the device.
            let (flusher, committed_at) = (inner.flusher.clone(), inner.commit);
            drop(held);
            flusher.run(committed_at).await?;
        }
        rows.committed = Some(committed);
        Ok(rows)
    }
    /// Turns the captured rows into writes and authors them as one bundle
    /// inside the open engine transaction.
    async fn seal(
        &self,
        inner: &mut Inner<A>,
        plan: &Plan,
        target: Target,
        commit: Kind,
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
        let assigned_policy = typed.rows.schema().update_policy == UpdatePolicy::Assigned;
        let mut writes = Vec::new();
        let mut undo = BTreeMap::<ResourceScope, Vec<super::replay::Undo>>::new();
        let mut written = Vec::new();
        for touched in touched {
            let table = &plan.tables[touched.table];
            // The author admits its replayed effects as every replica will.
            if table.class == TableClass::Replayed {
                #[cfg(test)]
                let unchecked = inner.unchecked_replayed_admission;
                #[cfg(not(test))]
                let unchecked = false;
                let Some((change, image)) =
                    super::replay::replayed_change(conn, &typed, table, &touched)?
                else {
                    continue;
                };
                let logical = typed
                    .rows
                    .schema()
                    .table(&table.name)
                    .ok_or_else(|| Error::Corrupt("replayed table".into()))?;
                super::replay::note(&mut written, super::replay::written(logical, &image)?);
                // Rows only a delete action changed need no grant, as on
                // every replica.
                let direct = intents
                    .iter()
                    .any(|recorded| recorded.scope.table == change.scope.table);
                if !unchecked && direct && !change.permitted_by(&inner.local_grants(now()?)?) {
                    return Err(denied(
                        "row/field transition denied; the replayed effects are not granted",
                    ));
                }
                let author = inner.node_author.as_ref().map(|author| author.number);
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
                    inner
                        .put_identity_mark(
                            change.scope,
                            &super::checkpoint::IdentityMark {
                                author: identity.author(),
                                signer,
                                sequence: identity.sequence(),
                            },
                            super::identity::Raise::Own,
                        )
                        .await?;
                }
                undo.entry(change.scope)
                    .or_default()
                    .push(super::replay::Undo {
                        table: table.name.clone(),
                        key: touched.key,
                        before: touched.before,
                    });
                continue;
            }
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
            #[cfg(any(test, feature = "intent-shapes"))]
            self.shape().row(typed.rows.schema(), &table.name, anchor);
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
                let visible = RowChange {
                    scope,
                    row,
                    before: before
                        .as_ref()
                        .map(fields)
                        .transpose()
                        .map_err(capture_error)?,
                    after: None,
                };
                let visible_id = typed
                    .rows
                    .identity(&visible)
                    .map_err(capture_error)?
                    .ok_or_else(|| capture_error("missing visible incarnation identity".into()))?;
                let held = collect_rows(inner.conn.query(
                    "SELECT r.row,r.fields FROM secure_row_keys k JOIN secure_rows r ON r.scope=k.scope AND r.row=k.row WHERE k.scope=(SELECT id FROM secure_scope_state WHERE scope=?) AND k.key=?",
                    [encode(&scope)?, super::identity::key_digest(&row)],
                ).await?).await?;
                for held in held {
                    let [SqlValue::Blob(stored), SqlValue::Blob(bytes)] = held.as_slice() else {
                        return Err(Error::Corrupt("SQL deleted incarnation".into()));
                    };
                    let record = inner.decode_record(bytes).await?;
                    if record.deleted
                        || record.key.unwrap_or(super::checkpoint::row_of(stored)?) != row
                    {
                        continue;
                    }
                    let candidate = RowChange {
                        scope,
                        row,
                        before: Some(record.fields),
                        after: None,
                    };
                    let id = typed
                        .rows
                        .identity(&candidate)
                        .map_err(capture_error)?
                        .ok_or_else(|| {
                            capture_error("missing indexed incarnation identity".into())
                        })?;
                    let values = typed
                        .rows
                        .values(
                            &projected.table,
                            candidate.before.as_ref().ok_or_else(|| {
                                capture_error("missing indexed incarnation image".into())
                            })?,
                        )
                        .map_err(capture_error)?;
                    let key = typed
                        .rows
                        .row_id(typed.domain.domain, &projected.table, &values)
                        .map_err(capture_error)?;
                    let incarnation = rocky_core::commitment(
                        "rocky.row-incarnation.v2",
                        &id.integer().to_le_bytes(),
                    );
                    if record.identity != Some(id.integer())
                        || stored.as_slice() != incarnation
                        || record.key != Some(row)
                        || key != row
                    {
                        return Err(Error::Corrupt("SQL indexed incarnation binding".into()));
                    }
                    if id != visible_id {
                        writes.push(RowWrite::changed(candidate));
                    }
                }
                writes.push(RowWrite::changed(visible));
                continue;
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
        // Every replica checks these before releasing the transaction's
        // savepoint; the author refuses what they would drop.
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
        // Replayed writes travel only as a plain bundle's intents. Rows of
        // mergeable tables could not drop with them, so they never share a
        // transaction.
        if !intents.is_empty() && (commit != Kind::Plain || !matches!(target, Target::Bundle(_))) {
            return Err(denied("replayed writes commit only as a plain transaction"));
        }
        if !intents.is_empty() && !writes.is_empty() {
            return Err(denied(
                "a transaction writes replayed tables or mergeable ones, not both",
            ));
        }
        let handle = match target {
            Target::Move {
                handle,
                input,
                table,
                id,
                from,
                to,
            } => {
                let related = writes.clone();
                inner.strict_projection = false;
                let frames = inner
                    .move_row_with(
                        (handle, input),
                        &table,
                        id,
                        &from,
                        &to,
                        &self.session.status.scopes,
                        writes,
                    )
                    .await?;
                inner.strict_projection = true;
                inner.verify_sql_projection(&related).await?;
                return Ok(Sealed::Commit(SecureCommit { handle, frames }));
            }
            Target::Bundle(handle) => handle,
            Target::Publication(publication) => {
                inner.strict_projection = true;
                let (frames, replayed) = inner.stage_publication(publication, writes).await?;
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
        };
        let handle = handle.unwrap_or_else(|| {
            let mut handle = [0; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut handle);
            handle
        });
        if writes.is_empty() && intents.is_empty() && commit != Kind::Plain {
            return Err(denied("re-authoring or a coordinated write writes rows"));
        }
        if writes.is_empty() && intents.is_empty() {
            return Ok(Sealed::Commit(SecureCommit {
                handle,
                frames: Vec::new(),
            }));
        }
        let encoded = super::bundle::input_bytes(&writes, &[], &intents)?;
        // The author's execution rewinds like a replay of it. Rows a delete
        // action changed in a scope without statements undo with the first
        // member: a rewind undoes every member of a transaction.
        let mut intents: super::bundle::Intents = intents
            .into_iter()
            .map(|(scope, statements)| {
                (scope, (statements, undo.remove(&scope).unwrap_or_default()))
            })
            .collect();
        if let Some((_, (_, first))) = intents.iter_mut().next() {
            first.extend(undo.into_values().flatten());
        }
        if encoded.len() > MAX_BUNDLE_BYTES {
            return Err(Error::Quota {
                resource: "bundle bytes",
                limit: MAX_BUNDLE_BYTES,
            });
        }
        let input = match commit {
            Kind::Reauthors(held) => rocky_core::commitment(
                "rocky.secure-reauthor-input.v1",
                &encode(&(held, serde_bytes::Bytes::new(&encoded)))?,
            ),
            Kind::Coordinated(slot) => rocky_core::commitment(
                "rocky.secure-coordinated-input.v1",
                &encode(&(slot.log, slot.expected, serde_bytes::Bytes::new(&encoded)))?,
            ),
            Kind::Plain => rocky_core::commitment("rocky.secure-bundle-input.v1", &encoded),
        };
        match inner.lookup_bundle(handle, input).await? {
            Some(Ok(frames)) => return Ok(Sealed::Replayed(SecureCommit { handle, frames })),
            Some(Err(_)) => {
                return Err(denied(
                    "transaction handle was committed before the pruned prefix",
                ));
            }
            None => {}
        }
        if let Kind::Reauthors(held) = commit {
            inner.resolve_held(held).await?;
        }
        inner.strict_projection = true;
        let entry = match commit {
            Kind::Coordinated(slot) => Some(slot.expected + 1),
            _ => None,
        };
        let frames = match commit {
            Kind::Reauthors(held) => {
                inner
                    .author_bundle_reauthoring(handle, input, &writes, now()?, held)
                    .await?
            }
            _ => {
                inner
                    .author_sql_bundle((handle, input), &writes, intents, now()?, entry)
                    .await?
            }
        };

        match commit {
            Kind::Reauthors(_) => {}
            Kind::Coordinated(slot) => {
                let transaction =
                    super::bundle::transaction_id(inner.config.node.public_key(), &handle);
                inner.stage_entry(&slot, transaction, &frames).await?;
            }
            Kind::Plain => {}
        }
        Ok(Sealed::Commit(SecureCommit { handle, frames }))
    }
}

impl<A: RowApplication> SecureSql<A> {
    /// The partition copies of a write to a replayed table, split on the
    /// open transaction's snapshot before the statement runs.
    async fn intent_copies(
        &self,
        lease: &Lease<A>,
        (sql, params): (&str, Vec<SqlValue>),
    ) -> Result<Option<Vec<super::intents::Recorded>>, Error> {
        let held = lease.held.as_ref().ok_or(Error::Closed)?;
        held.0
            .intent_copies(sql, &params, &self.session.status)
            .await
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

#[cfg(any(test, feature = "intent-shapes"))]
impl<A: RowApplication> SecureSql<A> {
    fn shape(&self) -> std::sync::MutexGuard<'_, crate::shapes::Pending> {
        self.shape
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn origin(&self, target: &Target, commit: Kind) -> crate::shapes::Origin {
        use crate::shapes::Origin;
        match (target, commit) {
            (Target::Publication(_), _) => Origin::Migration,
            _ if !self.parents.is_empty() => Origin::Migration,
            (Target::Move { .. }, _) => Origin::Move,
            (_, Kind::Coordinated(_)) => Origin::Coordinated,
            (_, Kind::Reauthors(_)) => Origin::Reauthoring,
            _ => Origin::Plain,
        }
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
