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
}
impl Default for Queries {
    fn default() -> Self {
        Self {
            cursors: CursorHost::new(),
            authority: Arc::new(AtomicU64::new(1)),
            events: None,
            touched: Default::default(),
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
        if let Some(typed) = &self.typed {
            let tables = typed
                .rows
                .schema()
                .tables
                .iter()
                .map(|table| table.sql_name.clone());
            self.queries.events = Some(TableEvents::new(typed.domain, tables));
        }
    }
    /// Policy, credentials or keys changed: earlier sessions and cursors are
    /// refused, and every observer is told readability may differ.
    pub(super) fn bump_authority(&mut self) {
        self.authority += 1;
        self.queries
            .authority
            .store(self.authority, Ordering::SeqCst);
        if let Some(events) = &self.queries.events {
            events.publish(None, self.revision, &[Cause::Authority]);
        }
    }
    /// A typed table changes inside the open write transaction.
    pub(super) fn touch(&self, table: &str) {
        if let Ok(mut touched) = self.queries.touched.lock() {
            touched.tables.insert(table.to_ascii_lowercase());
        }
    }
    /// Why the open write transaction changes typed tables.
    pub(crate) fn cause(&self, cause: Cause) {
        if let Ok(mut touched) = self.queries.touched.lock() {
            touched.causes.insert(cause);
        }
    }
    /// Forgets what a transaction that did not commit recorded.
    pub(crate) fn untouch(&self) {
        if let Ok(mut touched) = self.queries.touched.lock() {
            *touched = Touched::default();
        }
    }
    pub(crate) fn touched(&self) -> bool {
        self.queries
            .touched
            .lock()
            .is_ok_and(|touched| !touched.tables.is_empty())
    }
    /// Announces what the committed transaction changed, at `self.revision`.
    pub(super) fn announce(&self) {
        let Ok(mut touched) = self.queries.touched.lock() else {
            return;
        };
        let Touched { tables, mut causes } = std::mem::take(&mut *touched);
        if tables.is_empty() {
            return;
        }
        if causes.is_empty() {
            causes.insert(Cause::Rebuild);
        }
        if let Some(events) = &self.queries.events {
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
        let Some(typed) = &self.typed else {
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
            } else if self.has_pending(scope).await? {
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
        let checked = {
            let inner = self.session.replica.inner.lock().await;
            inner.check_authority(Some(self.session.authority))?;
            inner
                .partition_statement(sql, &params, &self.session.status)
                .await?
        };
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
        let checked = {
            let inner = self.session.replica.inner.lock().await;
            inner.check_authority(Some(self.session.authority))?;
            inner
                .partition_statement(sql, &params, &self.session.status)
                .await?
        };
        let analyzed = self.read_statement(&checked)?;
        {
            let inner = self.session.replica.inner.lock().await;
            inner.check_authority(Some(self.session.authority))?;
            inner.local_grants(now()?)?;
            inner
                .availability(&analyzed.tables, &self.session.status)
                .await?;
        }
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
        let plan = {
            let inner = self.session.replica.inner.lock().await;
            inner.check_authority(Some(self.session.authority))?;
            inner.local_grants(now()?)?;
            inner.availability(tables, &self.session.status).await?;
            let authority = inner.queries.authority.clone();
            let (opened, expires) = (self.session.authority, self.session.status.expires);
            CursorPlan {
                path: inner.path().to_path_buf(),
                sql,
                params,
                readable: self
                    .plan
                    .tables
                    .iter()
                    .map(|table| table.physical.clone())
                    .collect(),
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
        let host = self
            .session
            .replica
            .inner
            .lock()
            .await
            .queries
            .cursors
            .clone();
        SnapshotCursor::open(&host, self.cursors.clone(), plan).await
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
            let actions: BTreeMap<String, String> = inner
                .typed
                .as_ref()
                .and_then(|typed| typed.rows.schema().table(&found).cloned())
                .into_iter()
                .flat_map(|table| table.foreign_keys)
                .flat_map(|key| {
                    key.columns
                        .into_iter()
                        .map(move |column| (column, key.on_delete.clone()))
                })
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
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let row = row
                .into_iter()
                .map(crate::storage::sql_value)
                .collect::<Result<Vec<_>, _>>()?;
            // A created index the catalog does not list is Rocky's own.
            if pragma == Describe::IndexList
                && matches!((&row[1], &row[3]), (SqlValue::Text(index), SqlValue::Text(origin))
                    if origin == "c" && !listed.contains(&index.to_ascii_lowercase()))
            {
                continue;
            }
            let mut row = row;
            if pragma == Describe::ForeignKeyList
                && let Some(SqlValue::Text(column)) = row.get(3)
                && let Some(action) = actions.get(column)
            {
                row[6] = SqlValue::Text(action.clone());
            }
            result.push(row);
        }
        Ok(SnapshotRows {
            columns,
            rows: result,
        })
    }
}
