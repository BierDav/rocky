//! Snapshot cursors, session catalogs, availability and committed table
//! events of a secure replica's typed tables. See
//! `docs/runtime/queries-and-observation.md`.
use super::*;
use crate::{
    cursor::{CursorHost, CursorLimits, CursorPlan, SnapshotCursor, SnapshotRows},
    observe::{Cause, TableEvents, TableObserver},
    sql::{SqlError, Unavailable},
};

/// Typed tables changed inside the open write transaction, and why.
#[derive(Default)]
pub(crate) struct Touched {
    tables: BTreeSet<String>,
    causes: BTreeSet<Cause>,
}
/// Query state shared by a replica's handles.
pub(crate) struct Queries {
    pub(super) cursors: Arc<CursorHost>,
    /// Mirrors `Inner::authority` for cursor threads.
    pub(super) authority: Arc<AtomicU64>,
    pub(super) events: Option<Arc<TableEvents>>,
    pub(super) touched: std::sync::Mutex<Touched>,
    readers: std::sync::Mutex<Vec<std::sync::Weak<ReadAccess>>>,
}
impl Default for Queries {
    fn default() -> Self {
        Self {
            cursors: CursorHost::new(),
            authority: Arc::new(AtomicU64::new(1)),
            events: None,
            touched: Default::default(),
            readers: Default::default(),
        }
    }
}
pub(super) struct ReadAccess {
    authority: u64,
    status: SessionStatus,
    state: std::sync::RwLock<Result<ReadState, Error>>,
    pub(super) host: Arc<CursorHost>,
    current: Arc<AtomicU64>,
}
struct ReadState {
    path: PathBuf,
    epoch: u64,
    history: history::HistorySnapshot,
    views: Vec<rocky_core::LogicalView>,
    rules: BTreeMap<String, rocky_storage::partition_sql::TablePartitions>,
    unavailable: BTreeMap<String, Error>,
}
impl ReadAccess {
    fn snapshot(&self) -> Result<ReadState, Error> {
        if self.current.load(Ordering::SeqCst) != self.authority || now()? >= self.status.expires {
            return Err(Error::Sql(SqlError::Unavailable {
                table: String::new(),
                reason: Unavailable::Revoked,
            }));
        }
        let state = self.state.read().map_err(|_| Error::Closed)?;
        state.as_ref().map(Clone::clone).map_err(group::shared)
    }
}
impl Clone for ReadState {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            epoch: self.epoch,
            history: self.history.clone(),
            views: self.views.clone(),
            rules: self.rules.clone(),
            unavailable: self
                .unavailable
                .iter()
                .map(|(name, error)| (name.clone(), group::shared(error)))
                .collect(),
        }
    }
}
impl ReadState {
    fn statement(&self, sql: &str, params: &[SqlValue]) -> Result<String, Error> {
        rocky_storage::partition_sql::check_with_views(sql, params, &self.rules, &self.views)
            .map_err(|error| Error::Sql(error.into()))
    }
    fn available(&self, tables: &BTreeSet<String>) -> Result<(), Error> {
        for table in tables {
            if let Some(error) = self.unavailable.get(&table.to_ascii_lowercase()) {
                return Err(group::shared(error));
            }
        }
        Ok(())
    }
}
impl<A: RowApplication> Inner<A> {
    async fn read_state(&self, status: &SessionStatus) -> Result<ReadState, Error> {
        let mut unavailable = BTreeMap::new();
        let views = match &self.workspace.typed {
            Some(typed) => {
                for table in &typed.tables {
                    let name = table.table.sql_name.to_ascii_lowercase();
                    if let Err(error) = self
                        .availability(&BTreeSet::from([name.clone()]), status)
                        .await
                    {
                        unavailable.insert(name, error);
                    }
                }
                typed.rows.schema().views.clone()
            }
            None => Vec::new(),
        };
        let mut epoch = self
            .conn
            .query("SELECT epoch FROM secure_read_epoch WHERE id=1", ())
            .await?;
        let epoch = epoch
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("read epoch missing".into()))?
            .get::<i64>(0)? as u64;
        Ok(ReadState {
            path: self.path().to_path_buf(),
            epoch,
            history: self.history_snapshot().await?,
            views,
            rules: self.partition_rules(None, status).await?,
            unavailable,
        })
    }
    pub(super) async fn register_reader(
        &self,
        status: &SessionStatus,
        authority: u64,
    ) -> Result<Arc<ReadAccess>, Error> {
        let reader = Arc::new(ReadAccess {
            status: status.clone(),
            authority,
            state: std::sync::RwLock::new(self.read_state(status).await),
            host: self.workspace.queries.cursors.clone(),
            current: self.workspace.queries.authority.clone(),
        });
        let mut readers = self
            .workspace
            .queries
            .readers
            .lock()
            .map_err(|_| Error::Closed)?;
        readers.retain(|reader| reader.strong_count() > 0);
        readers.push(Arc::downgrade(&reader));
        Ok(reader)
    }
    pub(super) async fn refresh_readers(&self) {
        let readers: Vec<_> = {
            let mut readers = self
                .workspace
                .queries
                .readers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            readers.retain(|reader| reader.strong_count() > 0);
            readers
                .iter()
                .filter_map(std::sync::Weak::upgrade)
                .collect()
        };
        for reader in readers {
            if reader.authority == self.authority {
                let state = self.read_state(&reader.status).await;
                if let Ok(mut current) = reader.state.write() {
                    *current = state;
                }
            }
        }
    }
}
impl Drop for Queries {
    fn drop(&mut self) {
        // The file closes with the replica: end its cursors first.
        self.cursors.close();
    }
}

impl<A: RowApplication> Inner<A> {
    /// Starts the table events of the installed typed tables.
    pub(super) fn start_events(&mut self) {
        if let Some(typed) = &self.workspace.typed {
            let tables = typed
                .rows
                .schema()
                .tables
                .iter()
                .map(|table| table.sql_name.clone());
            // Observers of the same domain stay registered across a switch.
            self.workspace.queries.events = Some(match &self.workspace.queries.events {
                Some(events) if events.scope() == typed.domain => events.follow(tables),
                _ => TableEvents::new(typed.domain, tables),
            });
        }
    }
    /// Policy, credentials or keys changed: earlier sessions and cursors are
    /// refused, and every observer is told readability may differ.
    pub(super) fn bump_authority(&mut self) {
        self.authority += 1;
        self.authority_events.send_replace(self.authority);
        self.workspace
            .queries
            .authority
            .store(self.authority, Ordering::SeqCst);
        if let Some(events) = &self.workspace.queries.events {
            events.publish(None, self.revision, &[Cause::Authority]);
        }
    }
    /// A typed table changes inside the open write transaction.
    pub(super) fn touch(&self, table: &str) {
        if let Ok(mut touched) = self.workspace.queries.touched.lock() {
            touched.tables.insert(table.to_ascii_lowercase());
        }
    }
    /// Why the open write transaction changes typed tables.
    pub(crate) fn cause(&self, cause: Cause) {
        if let Ok(mut touched) = self.workspace.queries.touched.lock() {
            touched.causes.insert(cause);
        }
    }
    /// Forgets what a transaction that did not commit recorded.
    pub(crate) fn untouch(&self) {
        if let Ok(mut touched) = self.workspace.queries.touched.lock() {
            *touched = Touched::default();
        }
    }
    pub(crate) fn touched(&self) -> bool {
        self.workspace
            .queries
            .touched
            .lock()
            .is_ok_and(|touched| !touched.tables.is_empty())
    }
    /// Announces what the committed transaction changed, at `self.revision`.
    pub(super) fn announce(&self) {
        let Ok(mut touched) = self.workspace.queries.touched.lock() else {
            return;
        };
        let Touched { tables, mut causes } = std::mem::take(&mut *touched);
        if tables.is_empty() {
            return;
        }
        if causes.is_empty() {
            causes.insert(Cause::Rebuild);
        }
        if let Some(events) = &self.workspace.queries.events {
            let causes: Vec<_> = causes.into_iter().collect();
            events.publish(Some(&tables), self.revision, &causes);
        }
    }
    /// Refuses a statement that names a typed table whose rows are not
    /// locally available to `status`. A held, complete scope with no
    /// matching row is ordinary absence, never unavailability.
    pub(super) async fn availability(
        &self,
        tables: &BTreeSet<String>,
        status: &SessionStatus,
    ) -> Result<(), Error> {
        let Some(typed) = &self.workspace.typed else {
            return Ok(());
        };
        for name in tables {
            let Some(projected) = typed
                .tables
                .iter()
                .find(|table| table.table.sql_name.eq_ignore_ascii_case(name))
            else {
                continue;
            };
            // Each table the statement reads, its own scope's state only;
            // a partitioned table's concrete partitions are checked apart.
            self.require_scope_readable(projected.scope).await?;
            if typed.partitioned(projected) {
                continue;
            }
            let scope = projected.scope;
            if !status.scopes.contains(&scope) {
                // Outside the session: the authorizer refuses it.
                continue;
            }
            let keyed = self.config.keys.iter().any(|key| key.scope == scope);
            let reason = if !keyed {
                Unavailable::KeyMissing
            } else if !status.readable.contains(&scope) {
                continue;
            } else if !self.holds(scope).await? {
                Unavailable::NotHeld
            } else if self.has_pending(scope).await? || self.restoring(scope).await? {
                Unavailable::Incomplete
            } else {
                continue;
            };
            return Err(Error::Sql(SqlError::Unavailable {
                table: projected.table.sql_name.clone(),
                reason,
            }));
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Observes committed changes of typed tables by name; None is every
    /// table. Register before reading the initial snapshot. Authority and
    /// readiness changes wake every observer with `Cause::Authority`.
    pub async fn observe_tables(&self, tables: Option<&[&str]>) -> Result<TableObserver, Error> {
        let inner = self.inner.lock().await;
        let events = inner
            .workspace
            .queries
            .events
            .as_ref()
            .ok_or_else(|| Error::Incompatible("no typed tables are installed".into()))?;
        let labels: Option<Vec<(String, String)>> = tables.map(|tables| {
            tables
                .iter()
                .map(|table| (table.to_string(), table.to_string()))
                .collect()
        });
        let mut observer = TableObserver::new();
        events.register(&observer, labels.as_deref())?;
        observer.registered_at(events.scope(), inner.revision);
        Ok(observer)
    }
}

impl<A: RowApplication> SecureSql<A> {
    /// The tables this session may name: typed tables of its readable
    /// scopes and local tables, with what it may write.
    pub fn catalog(&self) -> Vec<crate::capture::CatalogTable> {
        self.plan.catalog(
            |table| Some(table.name.to_ascii_lowercase()),
            |index| self.writable.contains(&index),
        )
    }
    /// Binds this session's reads to kept `seal`: cursors and snapshots
    /// read its rows through temporary views with the live tables' names
    /// and shape, and every other statement is refused. Reads fail with
    /// `Unavailable::Released` once the seal is no longer kept.
    pub async fn bind_seal(&mut self, seal: Commitment) -> Result<(), Error> {
        let views = self.session.replica.seal_views(seal).await?;
        self.seal = Some(Arc::new(views));
        Ok(())
    }
    /// The kept seal this session's reads see, if bound.
    pub fn bound_seal(&self) -> Option<Commitment> {
        self.seal.as_ref().map(|views| views.seal)
    }
    /// Opens a cursor over the latest committed snapshot. It needs every
    /// named typed table available, holds no replica lease while it runs
    /// and ends with `Unavailable::Revoked` once the session's authority
    /// changes or its credentials expire; undelivered rows are discarded.
    pub async fn cursor(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
        limits: CursorLimits,
    ) -> Result<SnapshotCursor, Error> {
        let readiness = self.reads.snapshot()?;
        let checked = readiness.statement(sql, &params)?;
        let analyzed = self.read_statement(&checked)?;
        self.open_cursor(analyzed.sql, params, &analyzed.tables, limits)
            .await
    }
    /// Opens a snapshot for a read transaction of several statements. It
    /// steps nothing itself; `snapshot_rows` reads each statement's whole
    /// result from it. Its bounds and revocation are a cursor's.
    pub async fn snapshot(&self, limits: CursorLimits) -> Result<SnapshotCursor, Error> {
        self.open_cursor("SELECT 1".into(), Vec::new(), &BTreeSet::new(), limits)
            .await
    }
    /// Reads one statement's whole result, at most `result_bytes`, from
    /// `snapshot`. Availability is checked as for a cursor.
    pub async fn snapshot_rows(
        &self,
        snapshot: &mut SnapshotCursor,
        sql: &str,
        params: Vec<SqlValue>,
    ) -> Result<SnapshotRows, Error> {
        let readiness = self.reads.snapshot()?;
        let checked = readiness.statement(sql, &params)?;
        let analyzed = self.read_statement(&checked)?;
        if analyzed.tables.iter().any(|name| name.contains('@')) {
            return Err(Error::Sql(SqlError::Unsupported(
                "name history in a statement of its own, outside a read transaction".into(),
            )));
        }
        readiness.available(&analyzed.tables)?;
        snapshot
            .run(analyzed.sql, params, self.options.result_bytes)
            .await
    }
    fn read_statement(&self, sql: &str) -> Result<crate::statement::Analyzed, Error> {
        let analyzed = crate::statement::analyze(sql, |_| None)
            .map_err(|rejection| Error::Sql(SqlError::engine(&rejection.into())))?;
        if analyzed.kind != crate::statement::Kind::Read {
            return Err(Error::Sql(SqlError::Unsupported(
                "a cursor only reads".into(),
            )));
        }
        Ok(analyzed)
    }
    async fn open_cursor(
        &self,
        sql: String,
        params: Vec<SqlValue>,
        tables: &BTreeSet<String>,
        limits: CursorLimits,
    ) -> Result<SnapshotCursor, Error> {
        let readiness = self.reads.snapshot()?;
        readiness.available(tables)?;
        let plan = {
            let authority = self.reads.current.clone();
            let (opened, expires) = (self.session.authority, self.session.status.expires);
            let mut readable: Vec<String> = self
                .plan
                .tables
                .iter()
                .map(|table| table.physical.clone())
                .collect();
            // A bound session reads its seal; another names history as
            // `table@seal` or `table@bookmark`.
            let views = match &self.seal {
                Some(views) => Some(views.as_ref().clone()),
                None => readiness.history.named_history::<A>(tables)?,
            };
            let (prelude, requires) = match views {
                Some(views) => {
                    readable.extend(views.readable.iter().cloned());
                    (views.statements.clone(), Some(views.requires()))
                }
                None => (Vec::new(), None),
            };
            CursorPlan {
                path: readiness.path,
                epoch: Some((
                    "SELECT epoch FROM secure_read_epoch WHERE id=1".into(),
                    readiness.epoch,
                )),
                sql,
                params,
                readable,
                prelude,
                requires,
                revision: Some("SELECT revision FROM secure_metadata WHERE id=1".into()),
                limits,
                guard: Arc::new(move || {
                    let current = authority.load(Ordering::SeqCst) == opened;
                    if current && now().is_ok_and(|time| time < expires) {
                        Ok(())
                    } else {
                        Err(Error::Sql(SqlError::Unavailable {
                            table: String::new(),
                            reason: Unavailable::Revoked,
                        }))
                    }
                }),
            }
        };
        SnapshotCursor::open(&self.reads.host, self.cursors.clone(), plan).await
    }
}

/// A schema pragma an ORM reads to validate its tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Describe {
    TableInfo,
    IndexList,
    IndexInfo,
    IndexXinfo,
    ForeignKeyList,
}
impl Describe {
    /// The pragma's name and result columns, as SQLite reports them.
    fn shape(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::TableInfo => (
                "table_info",
                &["cid", "name", "type", "notnull", "dflt_value", "pk"],
            ),
            Self::IndexList => (
                "index_list",
                &["seq", "name", "unique", "origin", "partial"],
            ),
            Self::IndexInfo => ("index_info", &["seqno", "cid", "name"]),
            Self::IndexXinfo => (
                "index_xinfo",
                &["seqno", "cid", "name", "desc", "coll", "key"],
            ),
            Self::ForeignKeyList => (
                "foreign_key_list",
                &[
                    "id",
                    "seq",
                    "table",
                    "from",
                    "to",
                    "on_update",
                    "on_delete",
                    "match",
                ],
            ),
        }
    }
    fn of_index(self) -> bool {
        matches!(self, Self::IndexInfo | Self::IndexXinfo)
    }
}

impl<A: RowApplication> SecureSql<A> {
    /// Answers `pragma` about a table or index of the session's catalog from
    /// the file itself, so a tool sees the real keys and indexes. Another
    /// name is an empty result, as SQLite answers an unknown one; indexes
    /// the catalog does not list are left out.
    pub async fn describe(&self, pragma: Describe, name: &str) -> Result<SnapshotRows, Error> {
        let (verb, columns) = pragma.shape();
        let catalog = self.catalog();
        let found = if pragma.of_index() {
            catalog
                .iter()
                .flat_map(|table| &table.indexes)
                .find(|index| index.name.eq_ignore_ascii_case(name))
                .map(|index| index.name.clone())
        } else {
            catalog
                .iter()
                .find(|table| table.name.eq_ignore_ascii_case(name))
                .map(|table| table.name.clone())
        };
        let columns = columns
            .iter()
            .map(|column| crate::cursor::ResultColumn {
                name: (*column).into(),
                declared: None,
            })
            .collect();
        let Some(found) = found else {
            return Ok(SnapshotRows {
                columns,
                rows: Vec::new(),
            });
        };
        let (path, actions) = {
            let inner = self.session.replica.inner.lock().await;
            inner.check_authority(Some(self.session.authority))?;
            // SQLite defers the check; Rocky performs the declared action.
            // References may share a column, such as a partition column, so
            // each is named by its parent and its whole column set.
            let actions: BTreeMap<(String, BTreeSet<String>), String> = inner
                .workspace
                .typed
                .as_ref()
                .and_then(|typed| typed.rows.schema().table(&found).cloned())
                .into_iter()
                .flat_map(|table| table.foreign_keys)
                .map(|key| (foreign_key_name(&key.table, &key.columns), key.on_delete))
                .collect();
            (inner.path().to_path_buf(), actions)
        };
        let listed: BTreeSet<String> = catalog
            .iter()
            .flat_map(|table| &table.indexes)
            .map(|index| index.name.to_ascii_lowercase())
            .collect();
        let quoted = format!("\"{}\"", found.replace('"', "\"\""));
        let rows = tokio::task::spawn_blocking(move || {
            let connection = crate::sqlite::Connection::open_read_only(&path)?;
            connection
                .rows(&format!("PRAGMA {verb}({quoted})"), ())
                .map_err(Error::from)
        })
        .await
        .map_err(|_| Error::Corrupt("describe failed".into()))??;
        let rows = rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(crate::storage::sql_value)
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SnapshotRows {
            columns,
            rows: shaped(pragma, rows, &actions, &listed),
        })
    }
    /// Answers `pragma` about `table`, or one of its indexes, as a file
    /// installed with that definition would: what an application declaring
    /// another generation's schema validates. See
    /// `docs/runtime/sql-driver.md#room-284`.
    pub async fn describe_declared(
        &self,
        pragma: Describe,
        name: &str,
        table: &rocky_core::LogicalTable,
    ) -> Result<SnapshotRows, Error> {
        let (verb, columns) = pragma.shape();
        let columns = columns
            .iter()
            .map(|column| crate::cursor::ResultColumn {
                name: (*column).into(),
                declared: None,
            })
            .collect();
        let found = if pragma.of_index() {
            table
                .indexes
                .iter()
                .find(|index| index.name.eq_ignore_ascii_case(name))
                .map(|index| index.name.clone())
        } else {
            table
                .sql_name
                .eq_ignore_ascii_case(name)
                .then(|| table.sql_name.clone())
        };
        let Some(found) = found else {
            return Ok(SnapshotRows {
                columns,
                rows: Vec::new(),
            });
        };
        let connection = crate::sqlite::Connection::open_in_memory()?;
        for sql in std::iter::once(&table.create_sql)
            .chain(table.indexes.iter().map(|index| &index.create_sql))
        {
            connection.execute(sql, ()).await?;
        }
        let quoted = format!("\"{}\"", found.replace('"', "\"\""));
        let rows = connection
            .rows(&format!("PRAGMA {verb}({quoted})"), ())?
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(crate::storage::sql_value)
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let actions = table
            .foreign_keys
            .iter()
            .map(|key| {
                (
                    foreign_key_name(&key.table, &key.columns),
                    key.on_delete.clone(),
                )
            })
            .collect();
        let listed = table
            .indexes
            .iter()
            .map(|index| index.name.to_ascii_lowercase())
            .collect();
        Ok(SnapshotRows {
            columns,
            rows: shaped(pragma, rows, &actions, &listed),
        })
    }
}
/// Pragma rows as an ORM should see them: indexes outside `listed` are
/// Rocky's own and left out, and foreign keys report their declared action.
fn shaped(
    pragma: Describe,
    rows: Vec<Vec<SqlValue>>,
    actions: &BTreeMap<(String, BTreeSet<String>), String>,
    listed: &BTreeSet<String>,
) -> Vec<Vec<SqlValue>> {
    // Each foreign key spans the rows sharing its id: (id, seq, table, from, ...).
    let mut keys: BTreeMap<i64, (String, Vec<String>)> = BTreeMap::new();
    if pragma == Describe::ForeignKeyList {
        for row in &rows {
            if let (
                Some(SqlValue::Integer(id)),
                Some(SqlValue::Text(table)),
                Some(SqlValue::Text(column)),
            ) = (row.first(), row.get(2), row.get(3))
            {
                let key = keys
                    .entry(*id)
                    .or_insert_with(|| (table.clone(), Vec::new()));
                key.1.push(column.clone());
            }
        }
    }
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        // A created index the catalog does not list is Rocky's own.
        if pragma == Describe::IndexList
            && matches!((&row[1], &row[3]), (SqlValue::Text(index), SqlValue::Text(origin))
                if origin == "c" && !listed.contains(&index.to_ascii_lowercase()))
        {
            continue;
        }
        let mut row = row;
        if pragma == Describe::ForeignKeyList
            && let Some(SqlValue::Integer(id)) = row.first()
            && let Some((table, columns)) = keys.get(id)
            && let Some(action) = actions.get(&foreign_key_name(table, columns))
        {
            row[6] = SqlValue::Text(action.clone());
        }
        result.push(row);
    }
    result
}
/// A foreign key of one table, case-insensitively by parent and column set.
fn foreign_key_name(table: &str, columns: &[String]) -> (String, BTreeSet<String>) {
    (
        table.to_ascii_lowercase(),
        columns
            .iter()
            .map(|column| column.to_ascii_lowercase())
            .collect(),
    )
}
