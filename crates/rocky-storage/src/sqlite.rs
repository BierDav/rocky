use std::{collections::VecDeque, path::Path, sync::Mutex};
use web_time::Instant;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Busy(String),
    Deadline,
    Denied,
    Unsupported(String),
    Build(String),
    Sqlite(rusqlite::Error),
    Type(String),
}
pub const SQLITE_SOURCE_ID: &str =
    "2026-06-03 19:12:13 d6e03d8c777cfa2d35e3b60d8ec3e0187f3e9f99d8e2ee9cac695fd6fcdf1a24";
fn check_build(raw: &rusqlite::Connection) -> Result<()> {
    static CHECKED: std::sync::OnceLock<std::result::Result<(), String>> =
        std::sync::OnceLock::new();
    CHECKED
        .get_or_init(|| {
            let source: String = raw
                .query_row("SELECT sqlite_source_id()", [], |row| row.get(0))
                .map_err(|error| error.to_string())?;
            bundled(rusqlite::version(), &source)
        })
        .clone()
        .map_err(Error::Build)
}
fn bundled(version: &str, source: &str) -> std::result::Result<(), String> {
    let bundled = rocky_core::BUNDLED_SQLITE_VERSION;
    if version == bundled && source == SQLITE_SOURCE_ID {
        return Ok(());
    }
    Err(format!(
        "SQLite {version} ({source}) is linked, but the engine identity names \
         {bundled} ({SQLITE_SOURCE_ID}); on wasm32 depend on rusqlite with \
         default-features = false, since its ffi-sqlite-wasm-rs links another SQLite"
    ))
}
pub fn engine() -> String {
    format!("sqlite {}", rusqlite::version())
}

impl Error {
    pub fn is_constraint(&self) -> bool {
        matches!(self, Self::Sqlite(error)
            if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation))
            || self.is_overflow()
    }
    pub fn is_overflow(&self) -> bool {
        matches!(self, Self::Sqlite(rusqlite::Error::SqliteFailure(failure, Some(message)))
            if failure.code == rusqlite::ErrorCode::Unknown && message == crate::exact::OVERFLOW)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(message) => write!(f, "database is busy: {message}"),
            Self::Deadline => write!(f, "read transaction exceeded its time bound"),
            Self::Denied => write!(f, "statement is not authorized"),
            Self::Unsupported(reason) => write!(f, "unsupported statement: {reason}"),
            Self::Build(message) => f.write_str(message),
            Self::Sqlite(error) => error.fmt(f),
            Self::Type(message) => write!(f, "unexpected column value: {message}"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                Self::Busy(error.to_string())
            }
            Some(rusqlite::ErrorCode::OperationInterrupted) => Self::Deadline,
            Some(rusqlite::ErrorCode::AuthorizationForStatementDenied) => Self::Denied,
            _ => Self::Sqlite(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}
impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}
impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}
impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}
impl From<Vec<u8>> for Value {
    fn from(value: Vec<u8>) -> Self {
        Self::Blob(value)
    }
}
impl rusqlite::ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(ToSqlOutput::Borrowed(match self {
            Self::Null => ValueRef::Null,
            Self::Integer(value) => ValueRef::Integer(*value),
            Self::Real(value) => ValueRef::Real(*value),
            Self::Text(value) => ValueRef::Text(value.as_bytes()),
            Self::Blob(value) => ValueRef::Blob(value),
        }))
    }
}

pub trait IntoParams {
    fn into_params(self) -> Vec<Value>;
}
impl IntoParams for () {
    fn into_params(self) -> Vec<Value> {
        Vec::new()
    }
}
impl IntoParams for Vec<Value> {
    fn into_params(self) -> Vec<Value> {
        self
    }
}
impl<T: Into<Value>, const N: usize> IntoParams for [T; N] {
    fn into_params(self) -> Vec<Value> {
        self.into_iter().map(Into::into).collect()
    }
}
impl<A: Into<Value>, B: Into<Value>, C: Into<Value>> IntoParams for (A, B, C) {
    fn into_params(self) -> Vec<Value> {
        vec![self.0.into(), self.1.into(), self.2.into()]
    }
}
#[macro_export]
macro_rules! params {
    ($($value:expr),* $(,)?) => {
        vec![$($crate::sqlite::Value::from($value)),*]
    };
}
pub use params;

pub trait FromValue: Sized {
    fn from_value(value: Value) -> Result<Self>;
}
impl FromValue for i64 {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Integer(value) => Ok(value),
            other => Err(Error::Type(format!("expected INTEGER, found {other:?}"))),
        }
    }
}
impl FromValue for String {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Text(value) => Ok(value),
            _ => Err(Error::Type("expected TEXT".into())),
        }
    }
}
impl FromValue for Vec<u8> {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Blob(value) => Ok(value),
            _ => Err(Error::Type("expected BLOB".into())),
        }
    }
}

impl Value {
    pub fn weight(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Blob(bytes) => bytes.len(),
            _ => 8,
        }
    }
}
pub struct Page {
    pub rows: Vec<Vec<Value>>,
    pub more: bool,
    pub oversized: bool,
}
pub struct Row(Vec<Value>);
impl Row {
    pub fn column_count(&self) -> usize {
        self.0.len()
    }
    pub fn get_value(&self, column: usize) -> Result<Value> {
        self.0
            .get(column)
            .cloned()
            .ok_or_else(|| Error::Type(format!("missing column {column}")))
    }
    pub fn get<T: FromValue>(&self, column: usize) -> Result<T> {
        T::from_value(self.get_value(column)?)
    }
}
pub struct Rows(VecDeque<Row>);
impl Rows {
    pub async fn next(&mut self) -> Result<Option<Row>> {
        Ok(self.0.pop_front())
    }
}

#[derive(Clone)]
struct Hooks {
    capture: std::sync::Arc<Mutex<crate::capture::Sink>>,
    writes: std::sync::Arc<Writes>,
}
#[derive(Default)]
pub struct Writes {
    tables: std::sync::RwLock<Vec<(String, bool)>>,
    count: std::sync::atomic::AtomicU64,
    counts: Mutex<std::collections::HashMap<String, TableCount>>,
    rows: std::sync::RwLock<RowCounters>,
    row_failures: std::sync::atomic::AtomicU64,
}
#[derive(Default)]
struct TableCount {
    all: u64,
    structural: u64,
}
type RowCounters = std::collections::HashMap<
    String,
    std::collections::BTreeMap<
        i32,
        std::collections::HashMap<i64, std::sync::Weak<std::sync::atomic::AtomicU64>>,
    >,
>;
type WatchedInteger = (
    String,
    i32,
    i64,
    std::sync::Arc<std::sync::atomic::AtomicU64>,
);
pub struct RowWrites {
    keys: Vec<WatchedInteger>,
    owner: std::sync::Arc<Writes>,
}
impl RowWrites {
    pub fn count(&self) -> u64 {
        self.owner
            .row_failures
            .load(std::sync::atomic::Ordering::Acquire)
            + self
                .keys
                .iter()
                .map(|(_, _, _, counter)| counter.load(std::sync::atomic::Ordering::Acquire))
                .sum::<u64>()
    }
}
impl Drop for RowWrites {
    fn drop(&mut self) {
        let mut rows = self
            .owner
            .rows
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (table, column, key, counter) in &self.keys {
            if std::sync::Arc::strong_count(counter) != 1 {
                continue;
            }
            if let Some(columns) = rows.get_mut(table) {
                if let Some(keys) = columns.get_mut(column) {
                    keys.remove(key);
                    if keys.is_empty() {
                        columns.remove(column);
                    }
                }
                if columns.is_empty() {
                    rows.remove(table);
                }
            }
        }
    }
}
impl Writes {
    pub fn count(&self) -> u64 {
        self.count.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn table_count(&self, table: &str) -> u64 {
        self.selected_count([(table, true)])
    }
    pub fn selected_count<'a>(&self, tables: impl IntoIterator<Item = (&'a str, bool)>) -> u64 {
        let counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tables.into_iter().fold(0u64, |total, (table, updates)| {
            let count =
                counts.get(table).map_or(
                    0,
                    |count| if updates { count.all } else { count.structural },
                );
            total.wrapping_add(count)
        })
    }
    fn observe(&self, table: &str, row: &rocky_sqlite::PreUpdate) {
        use rocky_sqlite::Change;
        let update = row.change() == Change::Update;
        let rows = self
            .rows
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(columns) = rows.get(table) {
            use rusqlite::types::ValueRef;
            let integer = |value: rusqlite::Result<ValueRef<'_>>| {
                value.map(|value| match value {
                    ValueRef::Integer(value) => Some(value),
                    _ => None,
                })
            };
            for (column, keys) in columns {
                let old = || integer(row.before(*column));
                let new = || integer(row.after(*column));
                let values = match row.change() {
                    Change::Insert => new().map(|new| (None, new)),
                    Change::Delete => old().map(|old| (old, None)),
                    Change::Update => old().and_then(|old| Ok((old, new()?))),
                };
                let Ok((old, new)) = values else {
                    self.row_failures
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    continue;
                };
                for key in old.into_iter().chain(new.filter(|new| Some(*new) != old)) {
                    if let Some(counter) = keys.get(&key).and_then(std::sync::Weak::upgrade) {
                        counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    }
                }
            }
        }
        if self.tables.read().is_ok_and(|tables| {
            tables
                .iter()
                .any(|(watched, updates)| watched == table && (*updates || !update))
        }) {
            self.count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let mut counts = self
                .counts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let count = counts.entry(table.to_owned()).or_default();
            count.all = count.all.wrapping_add(1);
            if !update {
                count.structural = count.structural.wrapping_add(1);
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    pub changes: i64,
    pub last_insert_rowid: i64,
    pub total_changes: i64,
}
#[derive(Clone)]
pub struct Session {
    pub gate: std::sync::Arc<Mutex<crate::capture::Gate>>,
    pub observed: std::sync::Arc<Mutex<Observed>>,
}
pub struct Ran {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub read_only: bool,
    pub changes: u64,
    pub total_changes: u64,
    pub last_insert_rowid: i64,
}
pub enum Run {
    Done(Ran),
    Writes,
    Oversized(bool),
}
pub struct Described {
    pub columns: Vec<String>,
    pub parameters: usize,
    pub read_only: bool,
}
pub struct Connection(
    Mutex<rusqlite::Connection>,
    std::sync::OnceLock<Hooks>,
    std::sync::OnceLock<Session>,
    std::sync::Arc<Mutex<crate::capture::Gate>>,
);
const WHOLE_TRANSACTIONS_RETRY_BUSY_FILES: std::time::Duration = std::time::Duration::ZERO;
pub const HISTORY_TRIGGERS: &str = "secure_kept_";
pub const HISTORY_TABLES: &str = "secure_history_";
impl Connection {
    pub fn open(path: &Path) -> Result<Self> {
        Self::configure(crate::files::connect(path, rusqlite::OpenFlags::default())?)
    }
    pub fn open_read_only(path: &Path) -> Result<Self> {
        use rusqlite::OpenFlags;
        let connection = Self::configure(crate::files::connect(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?)?;
        connection.rows("PRAGMA mmap_size=67108864", ())?;
        connection.load_schema()?;
        Ok(connection)
    }
    fn load_schema(&self) -> Result<()> {
        self.rows("SELECT name FROM sqlite_schema LIMIT 0", ())
            .map(drop)
    }
    pub fn open_in_memory() -> Result<Self> {
        Self::configure(rusqlite::Connection::open_in_memory()?)
    }
    fn configure(raw: rusqlite::Connection) -> Result<Self> {
        check_build(&raw)?;
        raw.set_prepared_statement_cache_capacity(512);
        raw.busy_timeout(WHOLE_TRANSACTIONS_RETRY_BUSY_FILES)?;
        crate::exact::register(&raw)?;
        Ok(Self(
            Mutex::new(raw),
            std::sync::OnceLock::new(),
            std::sync::OnceLock::new(),
            Default::default(),
        ))
    }
    pub fn capture(&self) -> Result<std::sync::Arc<Mutex<crate::capture::Sink>>> {
        Ok(self.hooks()?.capture)
    }
    pub fn watch_writes(
        &self,
        tables: impl IntoIterator<Item = (String, bool)>,
    ) -> Result<std::sync::Arc<Writes>> {
        let writes = self.hooks()?.writes;
        {
            let mut watched = writes
                .tables
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (table, updates) in tables {
                if let Some((_, existing)) = watched.iter_mut().find(|(name, _)| *name == table) {
                    *existing |= updates;
                } else {
                    watched.push((table, updates));
                }
            }
        }
        Ok(writes)
    }
    pub fn watch_integer_rows(
        &self,
        keys: impl IntoIterator<Item = (String, i32, i64)>,
    ) -> Result<std::sync::Arc<RowWrites>> {
        let keys: Vec<_> = keys.into_iter().collect();
        if keys.iter().any(|(_, column, _)| *column < 0) {
            return Err(Error::Type("negative watched column".into()));
        }
        let writes = self.hooks()?.writes;
        let mut rows = writes
            .rows
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut watched = Vec::new();
        for (table, column, key) in keys.into_iter().collect::<std::collections::BTreeSet<_>>() {
            let entry = rows
                .entry(table.clone())
                .or_default()
                .entry(column)
                .or_default()
                .entry(key)
                .or_default();
            let counter = entry.upgrade().unwrap_or_else(|| {
                let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                *entry = std::sync::Arc::downgrade(&counter);
                counter
            });
            watched.push((table, column, key, counter));
        }
        drop(rows);
        Ok(std::sync::Arc::new(RowWrites {
            keys: watched,
            owner: writes,
        }))
    }
    fn hooks(&self) -> Result<Hooks> {
        use rocky_sqlite::Change;
        if let Some(hooks) = self.1.get() {
            return Ok(hooks.clone());
        }
        let hooks = Hooks {
            capture: Default::default(),
            writes: Default::default(),
        };
        let shared = hooks.clone();
        rocky_sqlite::preupdate_hook(
            &*self.raw()?,
            move |database: &str, table: &str, row: &rocky_sqlite::PreUpdate| {
                if database == "main" {
                    shared.writes.observe(table, row);
                }
                let Ok(mut capture) = shared.capture.lock() else {
                    return;
                };
                let main = database == "main";
                if row.change() == Change::Insert && main && row.depth() == 0 {
                    capture.inserted(table);
                }
                if !capture.wants(main, table) {
                    return;
                }
                let old = || {
                    (0..row.columns())
                        .map(|column| value(row.before(column)?))
                        .collect::<Result<Vec<_>>>()
                };
                let new = || {
                    (0..row.columns())
                        .map(|column| value(row.after(column)?))
                        .collect::<Result<Vec<_>>>()
                };
                let rows = match row.change() {
                    Change::Insert => new().map(|added| (None, Some(added))),
                    Change::Delete => old().map(|removed| (Some(removed), None)),
                    Change::Update => old().and_then(|removed| Ok((Some(removed), Some(new()?)))),
                };
                match rows {
                    Ok((removed, added)) => capture.observe(table, removed, added),
                    Err(_) => capture.fail("a changed row could not be read"),
                }
            },
        )?;
        Ok(self.1.get_or_init(|| hooks).clone())
    }
    pub fn session(&self) -> Result<Session> {
        use rusqlite::functions::FunctionFlags;
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        if let Some(session) = self.2.get() {
            return Ok(session.clone());
        }
        let session = Session {
            gate: self.3.clone(),
            observed: Default::default(),
        };
        let raw = self.raw()?;
        let gate = session.gate.clone();
        raw.authorizer(Some(move |context: AuthContext<'_>| {
            use crate::capture::Access;
            let access = match context.action {
                AuthAction::Select | AuthAction::Function { .. } | AuthAction::Recursive => {
                    Access::Neutral
                }
                AuthAction::Read {
                    table_name,
                    column_name,
                } => Access::Read(table_name, column_name),
                AuthAction::Insert { table_name } => Access::Insert(table_name),
                AuthAction::Delete { table_name } => Access::Delete(table_name),
                AuthAction::Update {
                    table_name,
                    column_name,
                } => Access::Update(table_name, column_name),
                _ => Access::Other,
            };
            let fired_by_history_trigger = context
                .accessor
                .is_some_and(|trigger| trigger.starts_with(HISTORY_TRIGGERS));
            if fired_by_history_trigger {
                return Authorization::Allow;
            }
            let outside_session_schema = context.database_name.is_some_and(|name| name != "main");
            let Ok(mut gate) = gate.lock() else {
                return Authorization::Deny;
            };
            if gate.allows(access) && !(outside_session_schema && gate.armed()) {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }))?;
        type Read = fn(&Observed) -> i64;
        let functions: [(&str, Read); 3] = [
            ("changes", |observed| observed.changes),
            ("total_changes", |observed| observed.total_changes),
            ("last_insert_rowid", |observed| observed.last_insert_rowid),
        ];
        for (name, read) in functions {
            let observed = session.observed.clone();
            raw.create_scalar_function(name, 0, FunctionFlags::SQLITE_UTF8, move |_| {
                Ok(observed.lock().map(|observed| read(&observed)).unwrap_or(0))
            })?;
        }
        drop(raw);
        Ok(self.2.get_or_init(|| session).clone())
    }
    fn compile<'c>(
        &self,
        raw: &'c rusqlite::Connection,
        sql: &str,
    ) -> Result<rusqlite::Statement<'c>> {
        let statement = raw.prepare(sql)?;
        let Some(session) = self.2.get() else {
            return Ok(statement);
        };
        let poisoned = || Error::Type("session gate poisoned".into());
        let (plan, unresolved) = {
            let mut gate = session.gate.lock().map_err(|_| poisoned())?;
            (gate.suspend(), gate.unresolved())
        };
        let stored = unresolved.iter().try_fold(false, |found, name| {
            let mut lookup =
                raw.prepare_cached("SELECT 1 FROM pragma_table_list WHERE name=?1 COLLATE NOCASE")?;
            Ok::<_, Error>(found || lookup.exists([name])?)
        });
        session.gate.lock().map_err(|_| poisoned())?.resume(plan);
        if stored? {
            return Err(Error::Denied);
        }
        Ok(statement)
    }
    pub fn describe(&self, sql: &str) -> Result<Described> {
        let raw = self.raw()?;
        let statement = self.compile(&raw, sql)?;
        Ok(Described {
            columns: statement
                .column_names()
                .into_iter()
                .map(String::from)
                .collect(),
            parameters: statement.parameter_count(),
            read_only: statement.readonly(),
        })
    }
    pub fn session_run(
        &self,
        sql: &str,
        params: Vec<Value>,
        max_bytes: usize,
        write: bool,
    ) -> Result<Run> {
        let raw = self.raw()?;
        let mut statement = self.compile(&raw, sql)?;
        let read_only = statement.readonly();
        if !read_only && !write {
            return Ok(Run::Writes);
        }
        let names: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(String::from)
            .collect();
        let before = raw.total_changes();
        let mut result = Vec::new();
        let mut bytes = 0usize;
        let mut rows = statement.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(names.len());
            for column in 0..names.len() {
                values.push(value(row.get_ref(column)?)?);
            }
            bytes = bytes.saturating_add(values.iter().map(Value::weight).sum::<usize>());
            if bytes > max_bytes {
                return Ok(Run::Oversized(!read_only));
            }
            result.push(values);
        }
        drop(rows);
        drop(statement);
        Ok(Run::Done(Ran {
            columns: names,
            rows: result,
            read_only,
            changes: raw.changes(),
            total_changes: raw.total_changes().saturating_sub(before),
            last_insert_rowid: raw.last_insert_rowid(),
        }))
    }
    pub fn set_interrupt(
        &self,
        deadline: Instant,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<()> {
        self.raw()?.progress_handler(
            1000,
            Some(move || {
                cancelled.load(std::sync::atomic::Ordering::Relaxed) || Instant::now() >= deadline
            }),
        )?;
        Ok(())
    }
    pub fn scan(
        &self,
        sql: &str,
        params: Vec<Value>,
        mut visit: impl FnMut(&[Value]) -> Result<()>,
    ) -> Result<()> {
        let raw = self.raw()?;
        let mut statement = raw.prepare_cached(sql)?;
        #[cfg(any(test, feature = "test-support"))]
        {
            STATEMENTS.with(|cell| cell.set(cell.get() + 1));
            STATEMENT_TRACE.with(|trace| {
                if let Some(statements) = trace.borrow_mut().as_mut() {
                    statements.push(sql.to_owned());
                }
            });
        }
        let columns = statement.column_count();
        let mut rows = statement.query(rusqlite::params_from_iter(params))?;
        let mut values = Vec::with_capacity(columns);
        while let Some(row) = rows.next()? {
            values.clear();
            for column in 0..columns {
                values.push(value(row.get_ref(column)?)?);
            }
            visit(&values)?;
        }
        Ok(())
    }
    pub(crate) fn hold(&self) -> Result<std::sync::MutexGuard<'_, rusqlite::Connection>> {
        self.raw()
    }
    fn raw(&self) -> Result<std::sync::MutexGuard<'_, rusqlite::Connection>> {
        self.0
            .lock()
            .map_err(|_| Error::Type("connection poisoned".into()))
    }
    pub fn set_deadline(&self, deadline: Option<Instant>) -> Result<()> {
        let raw = self.raw()?;
        match deadline {
            Some(deadline) => {
                raw.progress_handler(1000, Some(move || Instant::now() >= deadline))?
            }
            None => raw.progress_handler(0, None::<fn() -> bool>)?,
        }
        Ok(())
    }
    pub fn set_busy_timeout(&self, timeout: std::time::Duration) -> Result<()> {
        self.raw()?.busy_timeout(timeout)?;
        Ok(())
    }
    #[cfg(feature = "qualification")]
    pub fn qualification_exit_in_commit(&self) -> Result<()> {
        let raw = self.raw()?;
        raw.commit_hook(Some(|| std::process::exit(87)))?;
        let result = raw.execute_batch("COMMIT");
        raw.commit_hook(None::<fn() -> bool>)?;
        result?;
        Err(Error::Type(
            "qualification COMMIT returned without invoking its callback".into(),
        ))
    }
    pub fn page(
        &self,
        sql: &str,
        params: Vec<Value>,
        max_rows: usize,
        max_bytes: usize,
        unweighed: usize,
    ) -> Result<Page> {
        let raw = self.raw()?;
        let mut statement = raw.prepare_cached(sql)?;
        #[cfg(any(test, feature = "test-support"))]
        {
            STATEMENTS.with(|cell| cell.set(cell.get() + 1));
            STATEMENT_TRACE.with(|trace| {
                if let Some(statements) = trace.borrow_mut().as_mut() {
                    statements.push(sql.to_owned());
                }
            });
        }
        let columns = statement.column_count();
        let mut page = Page {
            rows: Vec::new(),
            more: false,
            oversized: false,
        };
        let mut bytes = 0usize;
        let mut rows = statement.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            if page.rows.len() == max_rows {
                page.more = true;
                break;
            }
            let mut values = Vec::with_capacity(columns);
            for column in 0..columns {
                values.push(value(row.get_ref(column)?)?);
            }
            let weighed = &values[..columns.saturating_sub(unweighed)];
            bytes = bytes.saturating_add(weighed.iter().map(Value::weight).sum::<usize>());
            if bytes > max_bytes {
                page.more = true;
                page.oversized = page.rows.is_empty();
                break;
            }
            page.rows.push(values);
        }
        drop(rows);
        Ok(page)
    }
    pub fn rows(&self, sql: &str, params: impl IntoParams) -> Result<Vec<Vec<Value>>> {
        let (rows, _) = self.run(sql, params.into_params())?;
        Ok(rows.into_iter().map(|row| row.0).collect())
    }
    pub fn run_now(&self, sql: &str) -> Result<()> {
        self.run(sql, Vec::new()).map(|_| ())
    }
    fn run(&self, sql: &str, params: Vec<Value>) -> Result<(VecDeque<Row>, u64)> {
        let raw = self.raw()?;
        let mut statement = raw.prepare_cached(sql)?;
        #[cfg(any(test, feature = "test-support"))]
        {
            STATEMENTS.with(|cell| cell.set(cell.get() + 1));
            STATEMENT_TRACE.with(|trace| {
                if let Some(statements) = trace.borrow_mut().as_mut() {
                    statements.push(sql.to_owned());
                }
            });
        }
        let columns = statement.column_count();
        let mut result = VecDeque::new();
        let mut rows = statement.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(columns);
            for column in 0..columns {
                values.push(value(row.get_ref(column)?)?);
            }
            result.push_back(Row(values));
        }
        drop(rows);
        drop(statement);
        Ok((result, raw.changes()))
    }
    pub async fn query(&self, sql: impl AsRef<str>, params: impl IntoParams) -> Result<Rows> {
        Ok(Rows(self.run(sql.as_ref(), params.into_params())?.0))
    }
    pub async fn execute(&self, sql: impl AsRef<str>, params: impl IntoParams) -> Result<u64> {
        Ok(self.run(sql.as_ref(), params.into_params())?.1)
    }
    pub fn is_autocommit(&self) -> Result<bool> {
        Ok(self.raw()?.is_autocommit())
    }
    pub fn total_changes(&self) -> Result<u64> {
        Ok(self.raw()?.total_changes())
    }
}

pub(crate) fn value(value: rusqlite::types::ValueRef<'_>) -> Result<Value> {
    use rusqlite::types::ValueRef;
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::Integer(value),
        ValueRef::Real(value) => Value::Real(value),
        ValueRef::Text(value) => Value::Text(
            std::str::from_utf8(value)
                .map_err(|_| Error::Type("TEXT is not UTF-8".into()))?
                .into(),
        ),
        ValueRef::Blob(value) => Value::Blob(value.to_vec()),
    })
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static STATEMENTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static STATEMENT_TRACE: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(any(test, feature = "test-support"))]
pub fn statements() -> u64 {
    STATEMENTS.with(std::cell::Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub fn trace_statements(enabled: bool) -> Vec<String> {
    STATEMENT_TRACE.with(|trace| trace.replace(enabled.then(Vec::new)).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn every_target_links_the_same_sqlite() {
        const TARGET_ONLY: &[&str] = &[
            "ATOMIC_INTRINSICS",
            "COMPILER",
            "DEFAULT_WORKER_THREADS",
            "ENABLE_LOAD_EXTENSION",
            "MAX_MMAP_SIZE",
            "MAX_WORKER_THREADS",
            "MUTEX_OMIT",
            "MUTEX_PTHREADS",
            "OMIT_LOAD_EXTENSION",
            "OS_OTHER",
            "SYSTEM_MALLOC",
            "TEMP_STORE",
            "THREADSAFE",
        ];
        const SHARED: &str = "DEFAULT_AUTOVACUUM DEFAULT_CACHE_SIZE=-2000 \
            DEFAULT_FILE_FORMAT=4 DEFAULT_FOREIGN_KEYS DEFAULT_JOURNAL_SIZE_LIMIT=-1 \
            DEFAULT_MMAP_SIZE=0 DEFAULT_PAGE_SIZE=4096 DEFAULT_PCACHE_INITSZ=20 \
            DEFAULT_RECURSIVE_TRIGGERS DEFAULT_SECTOR_SIZE=4096 DEFAULT_SYNCHRONOUS=2 \
            DEFAULT_WAL_AUTOCHECKPOINT=1000 DEFAULT_WAL_SYNCHRONOUS=2 DIRECT_OVERFLOW_READ \
            ENABLE_API_ARMOR ENABLE_COLUMN_METADATA ENABLE_DBSTAT_VTAB ENABLE_FTS3 \
            ENABLE_FTS3_PARENTHESIS ENABLE_FTS5 ENABLE_MEMORY_MANAGEMENT \
            ENABLE_PREUPDATE_HOOK ENABLE_RTREE ENABLE_STAT4 HAVE_ISNAN \
            MALLOC_SOFT_LIMIT=1024 MAX_ATTACHED=10 MAX_COLUMN=2000 \
            MAX_COMPOUND_SELECT=500 MAX_DEFAULT_PAGE_SIZE=8192 MAX_EXPR_DEPTH=1000 \
            MAX_FUNCTION_ARG=1000 MAX_LENGTH=1000000000 MAX_LIKE_PATTERN_LENGTH=50000 \
            MAX_PAGE_COUNT=0xfffffffe MAX_PAGE_SIZE=65536 MAX_SQL_LENGTH=1000000000 \
            MAX_TRIGGER_DEPTH=1000 MAX_VARIABLE_NUMBER=32766 MAX_VDBE_OP=250000000 \
            SOUNDEX USE_URI";
        let connection = Connection::open_in_memory().unwrap();
        let raw = connection.raw().unwrap();
        assert_eq!(rusqlite::version(), rocky_core::BUNDLED_SQLITE_VERSION);
        let source: String = raw
            .query_row("SELECT sqlite_source_id()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source, SQLITE_SOURCE_ID);
        let mut statement = raw.prepare("PRAGMA compile_options").unwrap();
        let options = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|option| option.unwrap())
            .filter(|option| {
                let name = option.split('=').next().unwrap_or_default();
                !TARGET_ONLY.contains(&name)
            })
            .collect::<Vec<_>>();
        assert_eq!(options.join(" "), SHARED);
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn another_sqlite_build_is_refused() {
        assert!(bundled(rocky_core::BUNDLED_SQLITE_VERSION, SQLITE_SOURCE_ID).is_ok());
        let upstream_sqlite_wasm_rs = bundled("3.53.0", "2026-04-09 11:41:38 other").unwrap_err();
        assert!(
            upstream_sqlite_wasm_rs.contains("default-features = false"),
            "rusqlite's default features link upstream sqlite-wasm-rs"
        );
        assert!(bundled(rocky_core::BUNDLED_SQLITE_VERSION, "another source").is_err());
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn repeated_table_watches_share_one_registration_and_keep_update_tracking() {
        let conn = Connection::open_in_memory().unwrap();
        conn.run_now("CREATE TABLE watched(id INTEGER PRIMARY KEY,value INTEGER)")
            .unwrap();
        let writes = conn.watch_writes([("watched".into(), false)]).unwrap();
        conn.watch_writes([("watched".into(), true)]).unwrap();
        conn.watch_writes([("watched".into(), false)]).unwrap();
        assert_eq!(writes.tables.read().unwrap().len(), 1);
        conn.run_now("INSERT INTO watched VALUES(1,0)").unwrap();
        conn.run_now("UPDATE watched SET value=1 WHERE id=1")
            .unwrap();
        assert_eq!(writes.table_count("watched"), 2);
        assert_eq!(writes.selected_count([("watched", false)]), 1);
        assert_eq!(writes.selected_count([("unwatched", true)]), 0);
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn integer_row_watches_isolate_scopes_and_include_moves_and_rollback() {
        let conn = Connection::open_in_memory().unwrap();
        conn.raw().unwrap().execute_batch("CREATE TABLE rows(scope INTEGER, id INTEGER PRIMARY KEY, value TEXT); CREATE TABLE copies(scope INTEGER, id INTEGER PRIMARY KEY);
            CREATE TRIGGER copy_row AFTER INSERT ON rows BEGIN INSERT INTO copies VALUES(NEW.scope,NEW.id); END;
            INSERT INTO rows VALUES(2,1,'other')").unwrap();
        let watch = conn
            .watch_integer_rows([("rows".into(), 0, 1), ("copies".into(), 0, 1)])
            .unwrap();
        conn.run_now("UPDATE rows SET value='unrelated' WHERE id=1")
            .unwrap();
        assert_eq!(watch.count(), 0);
        conn.run_now("INSERT INTO rows VALUES(1,2,'own')").unwrap();
        assert_eq!(watch.count(), 2, "the trigger counts too");
        conn.run_now("UPDATE rows SET scope=2 WHERE id=2").unwrap();
        conn.run_now("UPDATE rows SET scope=1 WHERE id=1").unwrap();
        assert_eq!(watch.count(), 4, "both sides of a scope move invalidate");
        conn.raw()
            .unwrap()
            .execute_batch("BEGIN; DELETE FROM rows WHERE scope=1; ROLLBACK")
            .unwrap();
        assert_eq!(
            watch.count(),
            5,
            "rollback never rewinds validation counters"
        );
        drop(watch);
        let next = conn.watch_integer_rows([("rows".into(), 0, 1)]).unwrap();
        assert_eq!(conn.hooks().unwrap().writes.rows.read().unwrap().len(), 1);
        assert_eq!(next.count(), 0);
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_wide_read_only_connection_keeps_first_reads_bounded_and_read_only() {
        let directory = rocky_platform::fs::Scratch::new().await;
        let path = directory.path().join("wide.db");
        let writer = Connection::open(&path).unwrap();
        let mut schema = String::from(
            "BEGIN; CREATE TABLE marker(value INTEGER); INSERT INTO marker VALUES(7);",
        );
        for table in 0..2000 {
            schema.push_str(&format!("CREATE TABLE t{table}(id INTEGER PRIMARY KEY);"));
        }
        schema.push_str("COMMIT;");
        writer.raw().unwrap().execute_batch(&schema).unwrap();
        drop(writer);

        let reader = Connection::open_read_only(&path).unwrap();
        assert!(reader.is_autocommit().unwrap());
        reader
            .set_deadline(Some(Instant::now() + std::time::Duration::from_millis(20)))
            .unwrap();
        assert_eq!(
            reader.rows("SELECT value FROM marker", ()).unwrap(),
            vec![vec![Value::Integer(7)]]
        );
        reader
            .set_deadline(Some(Instant::now() + std::time::Duration::from_millis(20)))
            .unwrap();
        assert!(matches!(reader.rows("WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<1000000000) SELECT COUNT(*) FROM c", ()), Err(Error::Deadline)));
        reader.set_deadline(None).unwrap();
        assert_eq!(
            reader.rows("SELECT value FROM marker", ()).unwrap(),
            vec![vec![Value::Integer(7)]]
        );
        assert!(
            matches!(reader.run_now("INSERT INTO marker VALUES(8)"), Err(Error::Sqlite(error)) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ReadOnly))
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_deadline_interrupts_a_long_read_with_a_typed_error() {
        let conn = Connection::open_in_memory().unwrap();
        let count = |limit: i64| {
            format!(
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<{limit}) \
                 SELECT COUNT(*) FROM c"
            )
        };
        conn.set_deadline(Some(Instant::now() + std::time::Duration::from_millis(20)))
            .unwrap();
        assert!(matches!(
            conn.query(count(1_000_000_000), ()).await,
            Err(Error::Deadline)
        ));
        conn.set_deadline(None).unwrap();
        let mut rows = conn.query(count(1000), ()).await.unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1000
        );
        let error = crate::Error::from(Error::Deadline);
        assert!(error.is_retryable() && !crate::storage::retryable(&error));
    }
}
