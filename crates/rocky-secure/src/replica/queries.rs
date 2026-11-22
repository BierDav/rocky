use super::*;
use crate::{
    cursor::{CursorHost, CursorLimits, CursorPlan, SnapshotCursor, SnapshotRows},
    observe::{Cause, TableEvents, TableObserver},
    sql::{SqlError, Unavailable},
};

#[derive(Default)]
pub(crate) struct Touched {
    tables: BTreeSet<String>,
    causes: BTreeSet<Cause>,
    outcomes: crate::observe::Outcomes,
}
pub(crate) struct Queries {
    pub(super) cursors: Arc<CursorHost>,
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
    fn statement(
        &self,
        sql: &str,
        params: &[SqlValue],
    ) -> Result<rocky_storage::partition_sql::Checked, Error> {
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
                let readiness = self.readiness(typed.domain).await;
                for table in &typed.tables {
                    let available = match &readiness {
                        Ok(readiness) => {
                            self.table_availability(typed, readiness, table, status)
                                .await
                        }
                        Err(error) => Err(group::shared(error)),
                    };
                    if let Err(error) = available {
                        unavailable.insert(table.table.sql_name.to_ascii_lowercase(), error);
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
    pub(crate) async fn refresh_readers(&self) {
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
        self.cursors.close();
    }
}

impl<A: RowApplication> Inner<A> {
    pub(super) fn start_events(&mut self) {
        if let Some(typed) = &self.workspace.typed {
            let tables = typed
                .rows
                .schema()
                .tables
                .iter()
                .map(|table| table.sql_name.clone());
            self.workspace.queries.events = Some(match &self.workspace.queries.events {
                Some(events) if events.scope() == typed.domain => events.follow(tables),
                _ => TableEvents::new(typed.domain, tables),
            });
        }
    }
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
    pub(super) fn touch(&self, table: &str) {
        if let Ok(mut touched) = self.workspace.queries.touched.lock() {
            touched.tables.insert(table.to_ascii_lowercase());
        }
    }
    pub(super) fn touch_dropped(&self, transaction: Commitment) {
        if let Ok(mut touched) = self.workspace.queries.touched.lock() {
            touched.outcomes.dropped.insert(transaction);
        }
    }
    pub(super) fn touch_sealed(&self, transaction: Commitment) {
        if let Ok(mut touched) = self.workspace.queries.touched.lock() {
            touched.outcomes.sealed.insert(transaction);
        }
    }
    pub(crate) fn cause(&self, cause: Cause) {
        if let Ok(mut touched) = self.workspace.queries.touched.lock() {
            touched.causes.insert(cause);
        }
    }
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
    pub(super) fn announce(&self) {
        let Ok(mut touched) = self.workspace.queries.touched.lock() else {
            return;
        };
        let Touched {
            tables,
            mut causes,
            outcomes,
        } = std::mem::take(&mut *touched);
        let Some(events) = &self.workspace.queries.events else {
            return;
        };
        events.publish_outcomes(self.revision, &outcomes);
        if tables.is_empty() {
            return;
        }
        if causes.is_empty() {
            causes.insert(Cause::Rebuild);
        }
        let causes: Vec<_> = causes.into_iter().collect();
        events.publish(Some(&tables), self.revision, &causes);
    }
    pub(super) async fn availability(
        &self,
        tables: &BTreeSet<String>,
        status: &SessionStatus,
    ) -> Result<(), Error> {
        let Some(typed) = &self.workspace.typed else {
            return Ok(());
        };
        let projected: Vec<_> = tables
            .iter()
            .filter_map(|name| {
                typed
                    .tables
                    .iter()
                    .find(|table| table.table.sql_name.eq_ignore_ascii_case(name))
            })
            .collect();
        if projected.is_empty() {
            return Ok(());
        }
        let readiness = self.readiness(typed.domain).await?;
        for projected in projected {
            self.table_availability(typed, &readiness, projected, status)
                .await?;
        }
        Ok(())
    }
    async fn table_availability(
        &self,
        typed: &typed::Typed,
        readiness: &blockers::DomainReadiness,
        projected: &typed::Projected,
        status: &SessionStatus,
    ) -> Result<(), Error> {
        let scope = projected.scope;
        blockers::refuse(self.readable_in(readiness, scope).await?)?;
        if typed.partitioned(projected) || !status.scopes.contains(&scope) {
            return Ok(());
        }
        let keyed = self.config.keys.iter().any(|key| key.scope == scope);
        let reason = if !keyed {
            Unavailable::KeyMissing
        } else if !status.readable.contains(&scope) {
            return Ok(());
        } else if !self.holds(scope).await? {
            Unavailable::NotHeld
        } else if self.restoring(scope).await?
            || (self.engine_matches() && self.pending_leaves_incomplete(scope).await?)
        {
            Unavailable::Incomplete
        } else {
            return Ok(());
        };
        Err(Error::Sql(SqlError::Unavailable {
            table: projected.table.sql_name.clone(),
            reason,
        }))
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn observe_tables(&self, tables: Option<&[&str]>) -> Result<TableObserver, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
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

const READ_ATTEMPTS: usize = 16;

impl<A: RowApplication> SecureSql<A> {
    pub fn catalog(&self) -> Vec<crate::capture::CatalogTable> {
        self.plan.catalog(
            |table| Some(table.name.to_ascii_lowercase()),
            |index| self.writable.contains(&index),
        )
    }
    pub async fn bind_seal(&mut self, seal: Commitment) -> Result<(), Error> {
        let views = self.session.replica.seal_views(seal).await?;
        self.seal = Some(Arc::new(views));
        Ok(())
    }
    pub fn bound_seal(&self) -> Option<Commitment> {
        self.seal.as_ref().map(|views| views.seal)
    }
    pub async fn cursor(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
        limits: CursorLimits,
    ) -> Result<SnapshotCursor, Error> {
        let open = async || {
            let readiness = self.reads.snapshot()?;
            let checked = readiness.statement(sql, &params)?;
            let analyzed = self.read_statement(&checked.sql)?;
            self.open_cursor(analyzed.sql, checked.params, &analyzed.tables, limits)
                .await
        };
        self.current_read(open).await
    }
    async fn current_read(
        &self,
        mut open: impl AsyncFnMut() -> Result<SnapshotCursor, Error>,
    ) -> Result<SnapshotCursor, Error> {
        for _ in 0..READ_ATTEMPTS {
            match open().await {
                Err(Error::Conflict) => {
                    drop(
                        self.session
                            .replica
                            .inner
                            .lock(crate::replica::Class::Interactive)
                            .await,
                    );
                }
                result => return result,
            }
        }
        open().await
    }
    pub async fn snapshot(&self, limits: CursorLimits) -> Result<SnapshotCursor, Error> {
        self.current_read(async || {
            self.open_cursor("SELECT 1".into(), Vec::new(), &BTreeSet::new(), limits)
                .await
        })
        .await
    }
    pub async fn snapshot_rows(
        &self,
        snapshot: &mut SnapshotCursor,
        sql: &str,
        params: Vec<SqlValue>,
    ) -> Result<SnapshotRows, Error> {
        let readiness = self.reads.snapshot()?;
        let checked = readiness.statement(sql, &params)?;
        let analyzed = self.read_statement(&checked.sql)?;
        if analyzed.tables.iter().any(|name| name.contains('@')) {
            return Err(Error::Sql(SqlError::Unsupported(
                "name history in a statement of its own, outside a read transaction".into(),
            )));
        }
        readiness.available(&analyzed.tables)?;
        snapshot
            .run(analyzed.sql, checked.params, self.options.result_bytes)
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
