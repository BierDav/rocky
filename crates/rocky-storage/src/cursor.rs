//! Snapshot cursors served by a bounded pool of reusable read-only connections.
//! Each active cursor leases one worker and pins its snapshot until it ends.
use crate::{
    Error,
    sql::{AbortReason, SqlError},
    sqlite, storage,
};
use rocky_core::SqlValue;
use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

/// Bounds of one cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorLimits {
    /// Rows of one page.
    pub page_rows: usize,
    /// Bytes of one page; a single larger row fails with `Quota`.
    pub page_bytes: usize,
    /// The cursor ends when no call arrives for this long.
    pub idle: Duration,
    /// The cursor ends this long after it opened, however busy.
    pub lifetime: Duration,
}
impl Default for CursorLimits {
    fn default() -> Self {
        Self {
            page_rows: 256,
            page_bytes: crate::MAX_FRAME_BYTES,
            idle: Duration::from_secs(30),
            lifetime: Duration::from_secs(300),
        }
    }
}
/// How often an idle cursor thread checks for a closing host.
const POLL: Duration = Duration::from_millis(50);
/// Open cursors one session may hold.
pub const MAX_SESSION_CURSORS: usize = 64;
/// Longest cursor lifetime a caller may ask for.
pub const MAX_CURSOR_LIFETIME: Duration = Duration::from_secs(300);
impl CursorLimits {
    pub fn validate(&self) -> Result<(), Error> {
        if self.page_rows == 0
            || self.page_rows > 4096
            || self.page_bytes == 0
            || self.page_bytes > crate::MAX_FRAME_BYTES
            || self.idle.is_zero()
            || self.lifetime.is_zero()
            || self.lifetime > MAX_CURSOR_LIFETIME
        {
            return Err(Error::Incompatible("cursor limits".into()));
        }
        Ok(())
    }
}

/// One result column: its name and the declared type of the column it
/// reads, if it reads one directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultColumn {
    pub name: String,
    pub declared: Option<String>,
}
/// One page of a cursor. `done` is set with the last row.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorPage {
    pub rows: Vec<Vec<SqlValue>>,
    pub done: bool,
}

/// Checked before each page is stepped and again before it is delivered.
/// An error ends the cursor and discards the page.
pub type Guard = Arc<dyn Fn() -> Result<(), Error> + Send + Sync>;

/// What a runtime authorized for one cursor.
pub struct CursorPlan {
    pub path: PathBuf,
    /// The statement with physical names; it must only read.
    pub sql: String,
    pub params: Vec<SqlValue>,
    /// Physical tables the statement may read.
    pub readable: Vec<String>,
    /// Reads the snapshot's revision before the statement is compiled.
    pub revision: Option<String>,
    /// Commit epoch the admission snapshot describes, checked inside the read transaction.
    pub epoch: Option<(String, u64)>,
    pub limits: CursorLimits,
    pub guard: Guard,
    /// Run on the connection before the snapshot opens, such as the
    /// temporary views of a connection bound to a seal.
    pub prelude: Vec<String>,
    /// Must return a row inside the snapshot, or the cursor fails with
    /// `Unavailable::Released`.
    pub requires: Option<String>,
}

/// A whole result another statement read from the cursor's snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotRows {
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Vec<SqlValue>>,
}
struct Statement {
    sql: String,
    params: Vec<SqlValue>,
    max_bytes: usize,
    reply: oneshot::Sender<Result<SnapshotRows, Error>>,
}
enum Request {
    Next(oneshot::Sender<Result<CursorPage, Error>>),
    Reset(oneshot::Sender<Result<(), Error>>),
    Run(Statement),
}
impl Request {
    /// Answers whatever the request waits for with `error`.
    fn fail(self, error: Error) {
        match self {
            Self::Next(reply) => drop(reply.send(Err(error))),
            Self::Reset(reply) => drop(reply.send(Err(error))),
            Self::Run(statement) => drop(statement.reply.send(Err(error))),
        }
    }
}
struct Opened {
    columns: Vec<ResultColumn>,
    revision: Option<u64>,
}

/// Shared between a cursor's handle, its thread and its host.
struct Control {
    cancelled: Arc<AtomicBool>,
    /// Why the cursor ended, once it has.
    ended: Mutex<Option<End>>,
    /// The thread still holds its connection.
    alive: Mutex<bool>,
    finished: Condvar,
}
#[derive(Clone, Copy, Debug)]
enum End {
    Finished,
    Expired,
    Cancelled,
    Closed,
    Failed,
}
impl Control {
    fn end(&self, end: End) {
        if let Ok(mut ended) = self.ended.lock() {
            ended.get_or_insert(end);
        }
        self.finished.notify_all();
    }
    fn error(&self) -> Error {
        match self.ended.lock().ok().and_then(|ended| *ended) {
            Some(End::Expired) => Error::Sql(SqlError::Aborted(AbortReason::Timeout)),
            Some(End::Cancelled) => Error::Sql(SqlError::Aborted(AbortReason::Cancelled)),
            _ => Error::Closed,
        }
    }
}

/// The cursors of one file, ended together when the file closes.
#[derive(Default)]
pub struct CursorHost {
    controls: Mutex<Vec<Weak<Control>>>,
    closed: AtomicBool,
    workers: Mutex<Vec<Worker>>,
}
impl CursorHost {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }
    /// Ends every cursor and waits until their connections are closed.
    /// Later opens are refused.
    pub fn close(&self) {
        let mut workers = match self.workers.lock() {
            Ok(workers) => workers,
            Err(_) => return,
        };
        self.closed.store(true, Ordering::SeqCst);
        let controls = match self.controls.lock() {
            Ok(mut controls) => std::mem::take(&mut *controls),
            Err(_) => return,
        };
        for control in controls.iter().filter_map(Weak::upgrade) {
            control.end(End::Closed);
            control.cancelled.store(true, Ordering::SeqCst);
            let Ok(mut alive) = control.alive.lock() else {
                continue;
            };
            // An idle thread notices within its poll interval.
            while *alive {
                match control.finished.wait_timeout(alive, POLL) {
                    Ok((next, _)) => alive = next,
                    Err(_) => break,
                }
            }
        }
        for worker in std::mem::take(&mut *workers) {
            drop(worker.jobs);
            let _ = worker.thread.join();
        }
    }
    /// Workers allocated so far; connections are reused between leases.
    pub fn workers(&self) -> usize {
        self.workers.lock().map_or(0, |workers| workers.len())
    }
    /// Open cursors, for tests and diagnostics.
    pub fn open_cursors(&self) -> usize {
        self.controls
            .lock()
            .map(|controls| {
                controls
                    .iter()
                    .filter_map(Weak::upgrade)
                    .filter(|control| control.ended.lock().is_ok_and(|ended| ended.is_none()))
                    .count()
            })
            .unwrap_or(0)
    }
}

/// A statement stepping through one committed snapshot. Dropping it ends
/// the read transaction and returns its worker to the pool.
pub struct SnapshotCursor {
    columns: Vec<ResultColumn>,
    revision: Option<u64>,
    requests: Option<mpsc::Sender<Request>>,
    control: Arc<Control>,
    /// The session's open-cursor count, released on drop.
    count: Arc<AtomicUsize>,
}
impl Drop for SnapshotCursor {
    fn drop(&mut self) {
        self.requests = None;
        self.control.cancelled.store(true, Ordering::SeqCst);
        self.count.fetch_sub(1, Ordering::SeqCst);
        // The disconnected channel wakes the leased worker immediately.
        if let Ok(mut alive) = self.control.alive.lock() {
            while *alive {
                match self.control.finished.wait_timeout(alive, POLL) {
                    Ok((next, _)) => alive = next,
                    Err(_) => break,
                }
            }
        }
    }
}
impl SnapshotCursor {
    /// Opens `plan` on `host`. `count` is the session's open cursors.
    pub async fn open(
        host: &Arc<CursorHost>,
        count: Arc<AtomicUsize>,
        plan: CursorPlan,
    ) -> Result<Self, Error> {
        plan.limits.validate()?;
        if host.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        if count.fetch_add(1, Ordering::SeqCst) >= MAX_SESSION_CURSORS {
            count.fetch_sub(1, Ordering::SeqCst);
            return Err(Error::Quota {
                resource: "open cursors",
                limit: MAX_SESSION_CURSORS,
            });
        }
        let control = Arc::new(Control {
            cancelled: Arc::new(AtomicBool::new(false)),
            ended: Mutex::new(None),
            alive: Mutex::new(true),
            finished: Condvar::new(),
        });
        let (requests, receiver) = mpsc::channel();
        let (opened, opening) = oneshot::channel();
        let cursor = Self {
            columns: Vec::new(),
            revision: None,
            requests: Some(requests),
            control,
            count,
        };
        let job = Job {
            plan,
            requests: receiver,
            opened,
            control: cursor.control.clone(),
        };
        if let Err(error) = host.lease(job) {
            if let Ok(mut alive) = cursor.control.alive.lock() {
                *alive = false;
            }
            cursor.control.finished.notify_all();
            return Err(error);
        }
        let mut cursor = cursor;
        let opened = opening.await.map_err(|_| cursor.control.error())??;
        cursor.columns = opened.columns;
        cursor.revision = opened.revision;
        Ok(cursor)
    }
    /// Result columns, fixed for the cursor's life.
    pub fn columns(&self) -> &[ResultColumn] {
        &self.columns
    }
    /// The revision of the snapshot, when the runtime reports one.
    pub fn revision(&self) -> Option<u64> {
        self.revision
    }
    /// The next page. After the last one, pages are empty and done.
    pub async fn next(&mut self) -> Result<CursorPage, Error> {
        let (reply, answer) = oneshot::channel();
        self.send(Request::Next(reply))?;
        answer.await.map_err(|_| self.control.error())?
    }
    /// Starts again from the first row of the same snapshot.
    pub async fn reset(&mut self) -> Result<(), Error> {
        let (reply, answer) = oneshot::channel();
        self.send(Request::Reset(reply))?;
        answer.await.map_err(|_| self.control.error())?
    }
    /// Reads the whole result of another statement from the same snapshot,
    /// at most `max_bytes` of values. `sql` must only read the plan's
    /// tables, as the cursor's own statement does; its pages are unaffected.
    pub async fn run(
        &mut self,
        sql: String,
        params: Vec<SqlValue>,
        max_bytes: usize,
    ) -> Result<SnapshotRows, Error> {
        let (reply, answer) = oneshot::channel();
        self.send(Request::Run(Statement {
            sql,
            params,
            max_bytes,
            reply,
        }))?;
        answer.await.map_err(|_| self.control.error())?
    }
    /// Interrupts a running step; the cursor ends with `Aborted(Cancelled)`.
    pub fn cancel(&self) {
        self.control.end(End::Cancelled);
        self.control.cancelled.store(true, Ordering::SeqCst);
    }
    /// Ends the cursor now; equivalent to dropping it.
    pub fn close(self) {}
    fn send(&self, request: Request) -> Result<(), Error> {
        self.requests
            .as_ref()
            .ok_or(Error::Closed)?
            .send(request)
            .map_err(|_| self.control.error())
    }
}

fn engine(error: rusqlite::Error) -> Error {
    match sqlite::Error::from(error) {
        sqlite::Error::Busy(message) => Error::Rejected(format!("database is busy: {message}")),
        error => Error::Sql(SqlError::engine(&error)),
    }
}

struct Job {
    plan: CursorPlan,
    requests: mpsc::Receiver<Request>,
    opened: oneshot::Sender<Result<Opened, Error>>,
    control: Arc<Control>,
}
struct Worker {
    jobs: mpsc::SyncSender<Job>,
    busy: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}
impl Drop for CursorHost {
    fn drop(&mut self) {
        self.close();
    }
}
impl CursorHost {
    fn lease(&self, job: Job) -> Result<(), Error> {
        let mut workers = self.workers.lock().map_err(|_| Error::Closed)?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        let worker = match workers
            .iter_mut()
            .find(|worker| !worker.busy.load(Ordering::SeqCst))
        {
            Some(worker) => worker,
            None => {
                if workers.len() >= MAX_SESSION_CURSORS {
                    return Err(Error::Quota {
                        resource: "cursor workers",
                        limit: MAX_SESSION_CURSORS,
                    });
                }
                let (jobs, receiver) = mpsc::sync_channel::<Job>(1);
                let busy = Arc::new(AtomicBool::new(false));
                let working = busy.clone();
                let thread = std::thread::Builder::new()
                    .name("rocky-reader".into())
                    .spawn(move || {
                        let mut connection: Option<(PathBuf, sqlite::Connection)> = None;
                        while let Ok(job) = receiver.recv() {
                            let control = job.control.clone();
                            serve(job, &mut connection);
                            working.store(false, Ordering::SeqCst);
                            if let Ok(mut alive) = control.alive.lock() {
                                *alive = false;
                            }
                            control.finished.notify_all();
                        }
                    })
                    .map_err(Error::Io)?;
                workers.push(Worker { jobs, busy, thread });
                workers.last_mut().ok_or(Error::Closed)?
            }
        };
        if let Ok(mut controls) = self.controls.lock() {
            controls.retain(|control| control.strong_count() > 0);
            controls.push(Arc::downgrade(&job.control));
        }
        worker.busy.store(true, Ordering::SeqCst);
        worker.jobs.send(job).map_err(|_| Error::Closed)
    }
}
fn serve(job: Job, connection: &mut Option<(PathBuf, sqlite::Connection)>) {
    let Job {
        plan,
        requests,
        opened,
        control,
    } = job;
    let (end, reusable) = (|| {
        if connection
            .as_ref()
            .is_none_or(|(path, _)| *path != plan.path)
        {
            match sqlite::Connection::open_read_only(&plan.path) {
                Ok(opened) => *connection = Some((plan.path.clone(), opened)),
                Err(error) => {
                    let _ = opened.send(Err(error.into()));
                    return (End::Failed, false);
                }
            }
        }
        let Some((_, connection)) = connection else {
            return (End::Failed, false);
        };
        let (end, reusable) = connection
            .hold(|raw| {
                let end = run(raw, &plan, &requests, opened, &control);
                (end, clean(raw).is_ok())
            })
            .unwrap_or((End::Failed, false));
        (end, reusable)
    })();
    if !reusable {
        *connection = None;
    }
    control.end(end);
}
fn clean(raw: &rusqlite::Connection) -> rusqlite::Result<()> {
    raw.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>)?;
    raw.progress_handler(0, None::<fn() -> bool>)?;
    if !raw.is_autocommit() {
        raw.execute_batch("ROLLBACK")?;
    }
    raw.pragma_update(None, "query_only", 0)?;
    let views = raw
        .prepare("SELECT name FROM sqlite_temp_schema WHERE type='view'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for view in views {
        raw.execute_batch(&format!("DROP VIEW temp.\"{}\"", view.replace('"', "\"\"")))?;
    }
    Ok(())
}

fn run(
    raw: &rusqlite::Connection,
    plan: &CursorPlan,
    requests: &mpsc::Receiver<Request>,
    opened: oneshot::Sender<Result<Opened, Error>>,
    control: &Control,
) -> End {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let started = Instant::now();
    let expires = started + plan.limits.lifetime;
    let cancelled = control.cancelled.clone();
    let prepared = (|| {
        for sql in &plan.prelude {
            raw.execute_batch(sql).map_err(engine)?;
        }
        raw.pragma_update(None, "query_only", 1).map_err(engine)?;
        raw.execute_batch("BEGIN").map_err(engine)?;
        // Reading anything starts the snapshot every later page shares.
        let revision = match &plan.revision {
            Some(sql) => Some(
                raw.query_row(sql, [], |row| row.get::<_, i64>(0))
                    .map_err(engine)? as u64,
            ),
            None => {
                raw.query_row("SELECT 1 FROM sqlite_schema LIMIT 1", [], |_| Ok(()))
                    .or_else(|error| match error {
                        rusqlite::Error::QueryReturnedNoRows => Ok(()),
                        error => Err(error),
                    })
                    .map_err(engine)?;
                None
            }
        };
        if let Some((sql, expected)) = &plan.epoch {
            let epoch: i64 = raw.query_row(sql, [], |row| row.get(0)).map_err(engine)?;
            if u64::try_from(epoch).ok() != Some(*expected) {
                return Err(Error::Conflict);
            }
        }
        if let Some(sql) = &plan.requires {
            let found = raw
                .query_row(sql, [], |_| Ok(()))
                .map(|_| true)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(false),
                    error => Err(error),
                })
                .map_err(engine)?;
            if !found {
                return Err(Error::Sql(SqlError::Unavailable {
                    table: String::new(),
                    reason: crate::sql::Unavailable::Released,
                }));
            }
        }
        let readable = plan.readable.clone();
        let viewed = !plan.prelude.is_empty();
        raw.authorizer(Some(move |context: AuthContext<'_>| match context.action {
            AuthAction::Select | AuthAction::Function { .. } | AuthAction::Recursive => {
                Authorization::Allow
            }
            AuthAction::Read { table_name, .. }
                if context.database_name.is_none_or(|name| name == "main")
                    && readable
                        .iter()
                        .any(|table| table.eq_ignore_ascii_case(table_name)) =>
            {
                Authorization::Allow
            }
            // The prelude's temporary views read only readable tables.
            AuthAction::Read { .. } if viewed && context.database_name == Some("temp") => {
                Authorization::Allow
            }
            _ => Authorization::Deny,
        }))
        .map_err(engine)?;
        let interrupt = cancelled.clone();
        raw.progress_handler(
            1000,
            Some(move || interrupt.load(Ordering::SeqCst) || Instant::now() >= expires),
        )
        .map_err(engine)?;
        (plan.guard)()?;
        let statement = raw.prepare(&plan.sql).map_err(engine)?;
        if !statement.readonly() {
            return Err(Error::Sql(SqlError::Denied));
        }
        let columns = statement
            .columns()
            .iter()
            .map(|column| ResultColumn {
                name: column.name().to_string(),
                declared: column.decl_type().map(str::to_string),
            })
            .collect();
        Ok((statement, Opened { columns, revision }))
    })();
    let mut statement = match prepared {
        Ok((statement, open)) => {
            if opened.send(Ok(open)).is_err() {
                return End::Closed;
            }
            statement
        }
        Err(error) => {
            let _ = opened.send(Err(error));
            return End::Failed;
        }
    };
    let params = storage::bindings(plan.params.clone());
    let end = 'exec: loop {
        let mut rows = match statement.query(rusqlite::params_from_iter(params.iter())) {
            Ok(rows) => rows,
            Err(error) => {
                // Reported on the next request.
                let error = engine(error);
                if let Ok(request) = requests.recv_timeout(plan.limits.idle) {
                    request.fail(error);
                }
                break 'exec End::Failed;
            }
        };
        let mut position = Position::default();
        let mut active = Instant::now();
        loop {
            let now = Instant::now();
            if now >= expires || now >= active + plan.limits.idle {
                break 'exec End::Expired;
            }
            let wait = (active + plan.limits.idle).min(expires) - now;
            let request = requests.recv_timeout(wait.min(POLL));
            if request.is_ok() {
                active = Instant::now();
            }
            if control.cancelled.load(Ordering::SeqCst) {
                if let Ok(request) = request {
                    request.fail(control.error());
                }
                break 'exec End::Cancelled;
            }
            match request {
                Ok(Request::Next(reply)) => {
                    let page = page(&mut rows, plan, &mut position, control);
                    let failed = page.is_err();
                    let _ = reply.send(page);
                    if failed {
                        break 'exec control
                            .ended
                            .lock()
                            .ok()
                            .and_then(|ended| *ended)
                            .unwrap_or(End::Failed);
                    }
                }
                Ok(Request::Reset(reply)) => {
                    let _ = reply.send(Ok(()));
                    continue 'exec;
                }
                Ok(Request::Run(statement)) => {
                    let result = whole(
                        raw,
                        plan,
                        statement.sql,
                        statement.params,
                        statement.max_bytes,
                    );
                    let failed = result.is_err() && control.cancelled.load(Ordering::SeqCst);
                    let _ = statement.reply.send(result);
                    if failed {
                        break 'exec End::Cancelled;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break 'exec End::Finished,
            }
        }
    };
    drop(statement);
    let _ = raw.execute_batch("ROLLBACK");
    end
}

/// Steps another read-only statement to its end inside the cursor's
/// transaction. The authorizer installed for the cursor applies to it.
fn whole(
    raw: &rusqlite::Connection,
    plan: &CursorPlan,
    sql: String,
    params: Vec<SqlValue>,
    max_bytes: usize,
) -> Result<SnapshotRows, Error> {
    (plan.guard)()?;
    let mut statement = raw.prepare(&sql).map_err(engine)?;
    if !statement.readonly() {
        return Err(Error::Sql(SqlError::Denied));
    }
    let columns: Vec<ResultColumn> = statement
        .columns()
        .iter()
        .map(|column| ResultColumn {
            name: column.name().to_string(),
            declared: column.decl_type().map(str::to_string),
        })
        .collect();
    let params = storage::bindings(params);
    let mut rows = statement
        .query(rusqlite::params_from_iter(params.iter()))
        .map_err(engine)?;
    let mut result = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().map_err(engine)? {
        let mut values = Vec::with_capacity(columns.len());
        for column in 0..columns.len() {
            let value = sqlite::value(row.get_ref(column).map_err(engine)?)?;
            values.push(storage::sql_value(value)?);
        }
        bytes = bytes.saturating_add(weight(&values));
        if bytes > max_bytes {
            return Err(Error::Quota {
                resource: "SQL result bytes",
                limit: max_bytes,
            });
        }
        result.push(values);
    }
    (plan.guard)()?;
    Ok(SnapshotRows {
        columns,
        rows: result,
    })
}

/// Where a cursor's execution stands between pages.
#[derive(Default)]
struct Position {
    exhausted: bool,
    /// A stepped row that did not fit its page; it opens the next one.
    carry: Option<Vec<SqlValue>>,
}
fn weight(row: &[SqlValue]) -> usize {
    row.iter()
        .map(|value| match value {
            SqlValue::Text(text) => text.len(),
            SqlValue::Blob(bytes) => bytes.len(),
            _ => 8,
        })
        .sum()
}

/// Steps one page, checking the guard before stepping and before delivery.
fn page(
    rows: &mut rusqlite::Rows<'_>,
    plan: &CursorPlan,
    position: &mut Position,
    control: &Control,
) -> Result<CursorPage, Error> {
    let interrupted = |error: Error| {
        if let Some(End::Cancelled) = control.ended.lock().ok().and_then(|ended| *ended) {
            return Error::Sql(SqlError::Aborted(AbortReason::Cancelled));
        }
        if control.cancelled.load(Ordering::SeqCst) {
            return Error::Closed;
        }
        match error {
            Error::Sql(SqlError::Statement { code: 9, .. }) => {
                control.end(End::Expired);
                Error::Sql(SqlError::Aborted(AbortReason::Timeout))
            }
            error => error,
        }
    };
    (plan.guard)().inspect_err(|_| control.end(End::Failed))?;
    let mut page = CursorPage {
        rows: Vec::new(),
        done: false,
    };
    let mut bytes = 0usize;
    loop {
        let row = match position.carry.take() {
            Some(row) => row,
            None if position.exhausted || page.rows.len() == plan.limits.page_rows => break,
            None => {
                let Some(row) = rows.next().map_err(|error| interrupted(engine(error)))? else {
                    position.exhausted = true;
                    break;
                };
                let columns = row.as_ref().column_count();
                let mut values = Vec::with_capacity(columns);
                for column in 0..columns {
                    let value = sqlite::value(row.get_ref(column).map_err(engine)?)?;
                    values.push(storage::sql_value(value)?);
                }
                values
            }
        };
        let row_bytes = weight(&row);
        if row_bytes > plan.limits.page_bytes {
            control.end(End::Failed);
            return Err(Error::Quota {
                resource: "cursor page bytes",
                limit: plan.limits.page_bytes,
            });
        }
        if bytes + row_bytes > plan.limits.page_bytes {
            position.carry = Some(row);
            break;
        }
        bytes += row_bytes;
        page.rows.push(row);
    }
    page.done = position.exhausted && position.carry.is_none();
    // Revocation discards what was stepped but not yet delivered.
    (plan.guard)().inspect_err(|_| control.end(End::Failed))?;
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct File {
        _directory: tempfile::TempDir,
        path: PathBuf,
        writer: sqlite::Connection,
    }
    fn file(rows: i64) -> File {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cursor.db");
        let writer = sqlite::Connection::open(&path).unwrap();
        writer.run_now("PRAGMA journal_mode=wal").unwrap();
        writer
            .run_now("CREATE TABLE items(id INTEGER PRIMARY KEY, title TEXT)")
            .unwrap();
        writer.run_now("CREATE TABLE secret(id INTEGER)").unwrap();
        for id in 0..rows {
            writer
                .rows(
                    "INSERT INTO items VALUES(?,?)",
                    [Value::Integer(id), Value::Text(format!("t{id}"))],
                )
                .unwrap();
        }
        File {
            _directory: directory,
            path,
            writer,
        }
    }
    use crate::sqlite::Value;
    fn plan(file: &File, sql: &str, limits: CursorLimits, guard: Guard) -> CursorPlan {
        CursorPlan {
            path: file.path.clone(),
            sql: sql.into(),
            params: Vec::new(),
            readable: vec!["items".into()],
            epoch: None,
            revision: Some("SELECT count(*) FROM items".into()),
            limits,
            guard,
            prelude: Vec::new(),
            requires: None,
        }
    }
    fn open() -> Guard {
        Arc::new(|| Ok(()))
    }
    fn small(rows: usize) -> CursorLimits {
        CursorLimits {
            page_rows: rows,
            ..CursorLimits::default()
        }
    }

    #[tokio::test]
    async fn a_thousand_cursor_leases_reuse_the_worker_and_clear_temporary_views() {
        let file = file(1);
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        for _ in 0..1000 {
            let mut plan = plan(&file, "SELECT * FROM leased", small(1), open());
            plan.prelude
                .push("CREATE TEMP VIEW leased AS SELECT id,title FROM items".into());
            let mut cursor = SnapshotCursor::open(&host, count.clone(), plan)
                .await
                .unwrap();
            assert_eq!(cursor.next().await.unwrap().rows.len(), 1);
            drop(cursor);
        }
        assert_eq!(host.workers(), 1);
        assert!(
            SnapshotCursor::open(
                &host,
                count.clone(),
                plan(&file, "SELECT * FROM leased", small(1), open())
            )
            .await
            .is_err()
        );
        let mut cursor = SnapshotCursor::open(
            &host,
            count,
            plan(&file, "SELECT id FROM items", small(1), open()),
        )
        .await
        .unwrap();
        assert_eq!(cursor.next().await.unwrap().rows.len(), 1);
        drop(cursor);
        host.close();
        assert_eq!(host.workers(), 0);
    }

    #[tokio::test]
    async fn readiness_from_another_commit_is_refused_inside_the_snapshot() {
        let file = file(1);
        let host = CursorHost::new();
        let mut plan = plan(&file, "SELECT id FROM items", small(1), open());
        plan.epoch = Some(("SELECT count(*) FROM items".into(), 1));
        file.writer
            .run_now("INSERT INTO items VALUES(2,'new')")
            .unwrap();
        assert!(matches!(
            SnapshotCursor::open(&host, Arc::default(), plan).await,
            Err(Error::Conflict)
        ));
    }

    #[tokio::test]
    async fn pages_come_from_one_snapshot_and_reset_restarts_it() {
        let file = file(5);
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        let mut cursor = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(
                &file,
                "SELECT id, title AS name FROM items ORDER BY id",
                small(2),
                open(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(cursor.revision(), Some(5));
        assert_eq!(
            cursor.columns(),
            [
                ResultColumn {
                    name: "id".into(),
                    declared: Some("INTEGER".into())
                },
                ResultColumn {
                    name: "name".into(),
                    declared: Some("TEXT".into())
                },
            ]
        );
        let first = cursor.next().await.unwrap();
        assert_eq!(first.rows.len(), 2);
        // Commits after the cursor opened are not part of its snapshot.
        file.writer
            .run_now("DELETE FROM items WHERE id >= 2")
            .unwrap();
        file.writer
            .run_now("INSERT INTO items VALUES(9,'new')")
            .unwrap();
        let mut seen = first.rows;
        loop {
            let page = cursor.next().await.unwrap();
            seen.extend(page.rows);
            if page.done {
                break;
            }
        }
        let ids: Vec<_> = seen.iter().map(|row| row[0].clone()).collect();
        assert_eq!(ids, (0..5).map(SqlValue::Integer).collect::<Vec<_>>());
        assert!(cursor.next().await.unwrap().done);
        cursor.reset().await.unwrap();
        assert_eq!(
            cursor.next().await.unwrap().rows[0][0],
            SqlValue::Integer(0)
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(cursor);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        // A new cursor sees the new snapshot.
        let mut cursor = SnapshotCursor::open(
            &host,
            count,
            plan(&file, "SELECT id FROM items", small(10), open()),
        )
        .await
        .unwrap();
        assert_eq!(cursor.revision(), Some(3));
        assert_eq!(cursor.next().await.unwrap().rows.len(), 3);
    }

    #[tokio::test]
    async fn further_statements_read_the_cursor_snapshot() {
        let file = file(3);
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        let mut cursor = SnapshotCursor::open(
            &host,
            count,
            plan(&file, "SELECT id FROM items ORDER BY id", small(1), open()),
        )
        .await
        .unwrap();
        assert_eq!(cursor.next().await.unwrap().rows.len(), 1);
        file.writer.run_now("DELETE FROM items").unwrap();
        // Another statement, between pages, still reads the opened snapshot.
        let counted = cursor
            .run(
                "SELECT count(*) AS n FROM items WHERE id >= ?".into(),
                vec![SqlValue::Integer(1)],
                1024,
            )
            .await
            .unwrap();
        assert_eq!(counted.columns[0].name, "n");
        assert_eq!(counted.rows, vec![vec![SqlValue::Integer(2)]]);
        assert_eq!(
            cursor.next().await.unwrap().rows,
            vec![vec![SqlValue::Integer(1)]]
        );
        // Writes, other tables and oversized results are refused; the cursor stays usable.
        for sql in ["DELETE FROM items", "SELECT * FROM secret"] {
            assert!(
                cursor.run(sql.into(), Vec::new(), 1024).await.is_err(),
                "{sql}"
            );
        }
        assert!(matches!(
            cursor
                .run("SELECT title FROM items".into(), Vec::new(), 2)
                .await,
            Err(Error::Quota { .. })
        ));
        assert_eq!(
            cursor.next().await.unwrap().rows,
            vec![vec![SqlValue::Integer(2)]]
        );
    }

    #[tokio::test]
    async fn statements_are_confined_and_pages_are_bounded() {
        let file = file(3);
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        for sql in [
            "SELECT * FROM secret",
            "DELETE FROM items",
            "SELECT * FROM sqlite_schema",
        ] {
            let opened =
                SnapshotCursor::open(&host, count.clone(), plan(&file, sql, small(2), open()))
                    .await;
            assert!(opened.is_err(), "{sql}");
        }
        assert_eq!(count.load(Ordering::SeqCst), 0);
        // A row that does not fit a page opens the next one.
        let limits = CursorLimits {
            page_rows: 10,
            page_bytes: 4,
            ..CursorLimits::default()
        };
        let mut cursor = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(&file, "SELECT title FROM items", limits, open()),
        )
        .await
        .unwrap();
        let mut pages = Vec::new();
        loop {
            let page = cursor.next().await.unwrap();
            pages.push(page.rows.len());
            if page.done {
                break;
            }
        }
        assert_eq!(pages.iter().sum::<usize>(), 3);
        assert!(pages.len() > 1);
        let limits = CursorLimits {
            page_rows: 10,
            page_bytes: 1,
            ..CursorLimits::default()
        };
        let mut cursor = SnapshotCursor::open(
            &host,
            count,
            plan(&file, "SELECT title FROM items", limits, open()),
        )
        .await
        .unwrap();
        assert!(matches!(cursor.next().await, Err(Error::Quota { .. })));
        assert!(cursor.next().await.is_err());
    }

    #[tokio::test]
    async fn bounds_revocation_and_host_close_end_cursors_deterministically() {
        let file = file(3);
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        let idle = CursorLimits {
            idle: Duration::from_millis(30),
            ..small(1)
        };
        let mut cursor = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(&file, "SELECT id FROM items", idle, open()),
        )
        .await
        .unwrap();
        cursor.next().await.unwrap();
        // Waits for the thread to end the transaction rather than sleeping blindly.
        while host.open_cursors() > 0 {
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            cursor.next().await,
            Err(Error::Sql(SqlError::Aborted(AbortReason::Timeout)))
        ));
        // The guard discards a stepped page once authority is gone.
        let revoked = Arc::new(AtomicBool::new(false));
        let flag = revoked.clone();
        let guard: Guard = Arc::new(move || {
            if flag.load(Ordering::SeqCst) {
                Err(Error::Sql(SqlError::Unavailable {
                    table: "items".into(),
                    reason: crate::sql::Unavailable::Revoked,
                }))
            } else {
                Ok(())
            }
        });
        let mut cursor = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(&file, "SELECT id FROM items", small(1), guard),
        )
        .await
        .unwrap();
        cursor.next().await.unwrap();
        revoked.store(true, Ordering::SeqCst);
        assert!(matches!(
            cursor.next().await,
            Err(Error::Sql(SqlError::Unavailable { .. }))
        ));
        assert!(cursor.next().await.is_err());
        // Cancelling ends the cursor.
        let mut cursor = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(&file, "SELECT id FROM items", small(1), open()),
        )
        .await
        .unwrap();
        cursor.cancel();
        assert!(cursor.next().await.is_err());
        // Closing the host ends open cursors and refuses new ones.
        let mut cursor = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(&file, "SELECT id FROM items", small(1), open()),
        )
        .await
        .unwrap();
        host.close();
        assert!(cursor.next().await.is_err());
        assert!(matches!(
            SnapshotCursor::open(
                &host,
                count.clone(),
                plan(&file, "SELECT id FROM items", small(1), open())
            )
            .await,
            Err(Error::Closed)
        ));
        // The WAL can be reset once no cursor pins it.
        drop(cursor);
        file.writer
            .run_now("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
    }

    #[tokio::test]
    async fn a_session_holds_at_most_its_cursor_quota() {
        let file = file(1);
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        let mut open_cursors = Vec::new();
        for _ in 0..MAX_SESSION_CURSORS {
            open_cursors.push(
                SnapshotCursor::open(
                    &host,
                    count.clone(),
                    plan(&file, "SELECT id FROM items", small(1), open()),
                )
                .await
                .unwrap(),
            );
        }
        assert!(matches!(
            SnapshotCursor::open(
                &host,
                count.clone(),
                plan(&file, "SELECT id FROM items", small(1), open())
            )
            .await,
            Err(Error::Quota { .. })
        ));
        open_cursors.pop();
        SnapshotCursor::open(
            &host,
            count,
            plan(&file, "SELECT id FROM items", small(1), open()),
        )
        .await
        .unwrap();
    }
}
