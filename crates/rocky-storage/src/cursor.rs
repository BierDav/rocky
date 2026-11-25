use crate::{
    Error,
    sql::{AbortReason, SqlError},
    sqlite, storage,
};
use rocky_core::SqlValue;
use rocky_platform::{task, time};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use web_time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorLimits {
    pub page_rows: usize,
    pub page_bytes: usize,
    pub idle: Duration,
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
const POLL: Duration = Duration::from_millis(50);
pub const MAX_SESSION_CURSORS: usize = 64;
const MAX_IDLE_WORKERS: usize = 2;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultColumn {
    pub name: String,
    pub declared: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CursorPage {
    pub rows: Vec<Vec<SqlValue>>,
    pub done: bool,
}

pub type Guard = Arc<dyn Fn() -> Result<(), Error> + Send + Sync>;

pub struct CursorPlan {
    pub path: PathBuf,
    pub sql: String,
    pub params: Vec<SqlValue>,
    pub readable: Vec<String>,
    pub revision: Option<String>,
    pub epoch: Option<(String, u64)>,
    pub limits: CursorLimits,
    pub guard: Guard,
}

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
    Wake,
}
impl Request {
    fn fail(self, error: Error) {
        match self {
            Self::Next(reply) => drop(reply.send(Err(error))),
            Self::Reset(reply) => drop(reply.send(Err(error))),
            Self::Run(statement) => drop(statement.reply.send(Err(error))),
            Self::Wake => {}
        }
    }
}
struct Opened {
    columns: Vec<ResultColumn>,
    revision: Option<u64>,
}

struct Control {
    cancelled: Arc<AtomicBool>,
    ended: Mutex<Option<End>>,
    alive: task::Held,
    dropped: AtomicBool,
    wake: mpsc::WeakUnboundedSender<Request>,
    released: watch::Sender<bool>,
}
#[derive(Clone, Copy, Debug)]
enum End {
    Finished,
    Expired,
    Cancelled,
    Preempted,
    Closed,
    Failed,
}
impl Control {
    fn end(&self, end: End) {
        if let Ok(mut ended) = self.ended.lock() {
            ended.get_or_insert(end);
        }
    }
    fn ending(&self) -> Option<End> {
        self.ended.lock().ok().and_then(|ended| *ended)
    }
    fn error(&self) -> Error {
        match self.ending() {
            Some(End::Expired) => Error::Sql(SqlError::Aborted(AbortReason::Timeout)),
            Some(End::Cancelled) => Error::Sql(SqlError::Aborted(AbortReason::Cancelled)),
            Some(End::Preempted) => Error::Sql(SqlError::Aborted(AbortReason::Preempted)),
            _ => Error::Closed,
        }
    }
    fn release(&self) {
        self.alive.set(false);
        self.released.send_replace(true);
    }
}

#[derive(Default)]
pub struct CursorHost {
    controls: Mutex<Vec<Weak<Control>>>,
    closed: AtomicBool,
    workers: Arc<Mutex<Vec<Worker>>>,
    quiet: watch::Sender<usize>,
    #[cfg(any(test, feature = "test-support"))]
    held_opens: watch::Sender<usize>,
}

pub struct Quiet {
    host: Arc<CursorHost>,
    preempted: usize,
}
impl Quiet {
    pub fn preempted(&self) -> usize {
        self.preempted
    }
}
impl Drop for Quiet {
    fn drop(&mut self) {
        self.host
            .quiet
            .send_modify(|quiet| *quiet = quiet.saturating_sub(1));
    }
}

#[cfg(any(test, feature = "test-support"))]
struct HeldOpen<'a>(&'a watch::Sender<usize>);
#[cfg(any(test, feature = "test-support"))]
impl<'a> HeldOpen<'a> {
    fn new(held: &'a watch::Sender<usize>) -> Self {
        held.send_modify(|held| *held += 1);
        Self(held)
    }
}
#[cfg(any(test, feature = "test-support"))]
impl Drop for HeldOpen<'_> {
    fn drop(&mut self) {
        self.0.send_modify(|held| *held = held.saturating_sub(1));
    }
}

impl CursorHost {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }
    pub fn close(&self) {
        let closing = self.close_to_leases();
        self.quiet.send_modify(|_| {});
        let Some(workers) = closing else {
            return;
        };
        let controls = match self.controls.lock() {
            Ok(mut controls) => std::mem::take(&mut *controls),
            Err(_) => return,
        };
        for control in controls.iter().filter_map(Weak::upgrade) {
            control.end(End::Closed);
            control.cancelled.store(true, Ordering::SeqCst);
            control.alive.wait_released(POLL);
        }
        for worker in workers {
            drop(worker.jobs);
            worker.task.release();
        }
    }
    fn close_to_leases(&self) -> Option<Vec<Worker>> {
        let mut workers = self.workers.lock().ok()?;
        self.closed.store(true, Ordering::SeqCst);
        Some(std::mem::take(&mut *workers))
    }
    pub fn workers(&self) -> usize {
        self.workers.lock().map_or(0, |workers| {
            workers
                .iter()
                .filter(|worker| !worker.retired.load(Ordering::SeqCst))
                .count()
        })
    }
    pub async fn quiesce(self: &Arc<Self>) -> Quiet {
        {
            let _no_lease_in_flight = self
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.quiet.send_modify(|quiet| *quiet += 1);
        }
        Quiet {
            host: self.clone(),
            preempted: self.preempt().await,
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn held_opens(&self) -> watch::Receiver<usize> {
        self.held_opens.subscribe()
    }
    async fn reopened(&self, quiet: &mut watch::Receiver<usize>) {
        let reopened = |quiet: &usize| *quiet == 0 || self.closed.load(Ordering::SeqCst);
        if reopened(&quiet.borrow_and_update()) {
            return;
        }
        #[cfg(any(test, feature = "test-support"))]
        let _held = HeldOpen::new(&self.held_opens);
        let _reopened_or_host_gone = quiet.wait_for(reopened).await;
    }
    async fn preempt(&self) -> usize {
        let open: Vec<_> = match self.controls.lock() {
            Ok(controls) => controls
                .iter()
                .filter_map(Weak::upgrade)
                .filter(|control| control.ending().is_none())
                .collect(),
            Err(_) => return 0,
        };
        let mut released = Vec::with_capacity(open.len());
        for control in &open {
            control.end(End::Preempted);
            control.cancelled.store(true, Ordering::SeqCst);
            if let Some(worker) = control.wake.upgrade() {
                let _ = worker.send(Request::Wake);
            }
            released.push(control.released.subscribe());
        }
        for mut released in released {
            let _released_or_gone = released.wait_for(|released| *released).await;
        }
        open.len()
    }
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

pub struct SnapshotCursor {
    columns: Vec<ResultColumn>,
    revision: Option<u64>,
    requests: Option<mpsc::UnboundedSender<Request>>,
    control: Arc<Control>,
    count: Arc<AtomicUsize>,
}
impl Drop for SnapshotCursor {
    fn drop(&mut self) {
        self.requests = None;
        self.control.cancelled.store(true, Ordering::SeqCst);
        self.control.dropped.store(true, Ordering::SeqCst);
        self.count.fetch_sub(1, Ordering::SeqCst);
        self.control.alive.wait_released(POLL);
    }
}
impl SnapshotCursor {
    pub async fn open(
        host: &Arc<CursorHost>,
        count: Arc<AtomicUsize>,
        plan: CursorPlan,
    ) -> Result<Self, Error> {
        plan.limits.validate()?;
        let mut quiet = host.quiet.subscribe();
        let mut plan = plan;
        let (mut cursor, opening) = loop {
            host.reopened(&mut quiet).await;
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
            let (requests, receiver) = mpsc::unbounded_channel();
            let control = Arc::new(Control {
                cancelled: Arc::new(AtomicBool::new(false)),
                ended: Mutex::new(None),
                alive: task::Held::new(true),
                dropped: AtomicBool::new(false),
                wake: requests.downgrade(),
                released: watch::Sender::new(false),
            });
            let (opened, opening) = oneshot::channel();
            let cursor = Self {
                columns: Vec::new(),
                revision: None,
                requests: Some(requests),
                control,
                count: count.clone(),
            };
            let job = Job {
                plan,
                requests: receiver,
                opened,
                control: cursor.control.clone(),
            };
            match host.lease(job) {
                Ok(Lease::Serving) => break (cursor, opening),
                Ok(Lease::Quiet(job)) => {
                    plan = job.plan;
                    cursor.control.release();
                }
                Err(error) => {
                    cursor.control.release();
                    return Err(error);
                }
            }
        };
        let opened = opening.await.map_err(|_| cursor.control.error())??;
        cursor.columns = opened.columns;
        cursor.revision = opened.revision;
        Ok(cursor)
    }
    pub fn columns(&self) -> &[ResultColumn] {
        &self.columns
    }
    pub fn revision(&self) -> Option<u64> {
        self.revision
    }
    pub async fn next(&mut self) -> Result<CursorPage, Error> {
        let (reply, answer) = oneshot::channel();
        self.send(Request::Next(reply))?;
        answer.await.map_err(|_| self.control.error())?
    }
    pub async fn reset(&mut self) -> Result<(), Error> {
        let (reply, answer) = oneshot::channel();
        self.send(Request::Reset(reply))?;
        answer.await.map_err(|_| self.control.error())?
    }
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
    pub fn cancel(&self) {
        self.control.end(End::Cancelled);
        self.control.cancelled.store(true, Ordering::SeqCst);
    }
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
    requests: mpsc::UnboundedReceiver<Request>,
    opened: oneshot::Sender<Result<Opened, Error>>,
    control: Arc<Control>,
}
struct Worker {
    jobs: mpsc::Sender<Job>,
    busy: Arc<AtomicBool>,
    retired: Arc<AtomicBool>,
    task: task::Dedicated<()>,
    serving: Weak<Control>,
}
impl Worker {
    fn available(&self) -> bool {
        if self.retired.load(Ordering::SeqCst) {
            return false;
        }
        !self.busy.load(Ordering::SeqCst)
            || (self.jobs.capacity() > 0
                && self
                    .serving
                    .upgrade()
                    .is_none_or(|control| control.dropped.load(Ordering::SeqCst)))
    }
}
impl Drop for CursorHost {
    fn drop(&mut self) {
        self.close();
    }
}
enum Lease {
    Serving,
    Quiet(Box<Job>),
}
impl CursorHost {
    fn lease(&self, job: Job) -> Result<Lease, Error> {
        let mut workers = self.workers.lock().map_err(|_| Error::Closed)?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        if *self.quiet.borrow() > 0 {
            return Ok(Lease::Quiet(Box::new(job)));
        }
        workers.retain(|worker| !worker.retired.load(Ordering::SeqCst));
        let worker = match workers.iter_mut().find(|worker| worker.available()) {
            Some(worker) => worker,
            None => {
                if workers.len() >= MAX_SESSION_CURSORS {
                    return Err(Error::Quota {
                        resource: "cursor workers",
                        limit: MAX_SESSION_CURSORS,
                    });
                }
                let (jobs, mut receiver) = mpsc::channel::<Job>(1);
                let busy = Arc::new(AtomicBool::new(false));
                let retired = Arc::new(AtomicBool::new(false));
                let (working, retiring) = (busy.clone(), retired.clone());
                let pool = Arc::downgrade(&self.workers);
                let task = task::dedicated("rocky-reader", move || async move {
                    let mut connection: Option<(PathBuf, sqlite::Connection)> = None;
                    while let Some(job) = receiver.recv().await {
                        let control = job.control.clone();
                        serve(job, &mut connection).await;
                        let retire = free(&pool, &working, &retiring, &receiver);
                        control.release();
                        if retire {
                            break;
                        }
                    }
                })
                .map_err(Error::Io)?;
                workers.push(Worker {
                    jobs,
                    busy,
                    retired,
                    task,
                    serving: Weak::new(),
                });
                workers.last_mut().ok_or(Error::Closed)?
            }
        };
        if let Ok(mut controls) = self.controls.lock() {
            controls.retain(|control| control.strong_count() > 0);
            controls.push(Arc::downgrade(&job.control));
        }
        worker.busy.store(true, Ordering::SeqCst);
        worker.serving = Arc::downgrade(&job.control);
        worker
            .jobs
            .try_send(job)
            .map(|()| Lease::Serving)
            .map_err(|_| Error::Closed)
    }
}
fn free(
    pool: &Weak<Mutex<Vec<Worker>>>,
    busy: &AtomicBool,
    retired: &AtomicBool,
    jobs: &mpsc::Receiver<Job>,
) -> bool {
    let workers = pool.upgrade();
    let workers = workers.as_deref().map(Mutex::lock);
    if !jobs.is_empty() {
        return false;
    }
    let retire = match &workers {
        Some(Ok(workers)) => {
            workers
                .iter()
                .filter(|worker| {
                    !worker.busy.load(Ordering::SeqCst) && !worker.retired.load(Ordering::SeqCst)
                })
                .count()
                >= MAX_IDLE_WORKERS
        }
        _ => false,
    };
    retired.store(retire, Ordering::SeqCst);
    busy.store(false, Ordering::SeqCst);
    retire
}
fn start_snapshot(
    raw: &rusqlite::Connection,
    revision: Option<&str>,
) -> Result<Option<u64>, Error> {
    match revision {
        Some(sql) => Ok(Some(
            raw.query_row(sql, [], |row| row.get::<_, i64>(0))
                .map_err(engine)? as u64,
        )),
        None => {
            raw.query_row("SELECT 1 FROM sqlite_schema LIMIT 1", [], |_| Ok(()))
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(()),
                    error => Err(error),
                })
                .map_err(engine)?;
            Ok(None)
        }
    }
}
fn snapshot_authorizer(
    readable: Vec<String>,
) -> impl FnMut(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization + Send + 'static
{
    use rusqlite::hooks::{AuthAction, Authorization};
    move |context| match context.action {
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
        _ => Authorization::Deny,
    }
}
async fn report_on_next_request(
    requests: &mut mpsc::UnboundedReceiver<Request>,
    idle: Duration,
    error: Error,
) {
    if let Ok(Some(request)) = time::timeout(idle, requests.recv()).await {
        request.fail(error);
    }
}
#[expect(
    clippy::await_holding_lock,
    reason = "The worker owns its connection; nothing else locks it across page awaits."
)]
async fn serve(job: Job, connection: &mut Option<(PathBuf, sqlite::Connection)>) {
    let Job {
        plan,
        mut requests,
        opened,
        control,
    } = job;
    let (end, reusable) = async {
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
        let Ok(raw) = connection.hold() else {
            return (End::Failed, false);
        };
        let end = run(&raw, &plan, &mut requests, opened, &control).await;
        (end, clean(&raw).is_ok())
    }
    .await;
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
    Ok(())
}

async fn run(
    raw: &rusqlite::Connection,
    plan: &CursorPlan,
    requests: &mut mpsc::UnboundedReceiver<Request>,
    opened: oneshot::Sender<Result<Opened, Error>>,
    control: &Control,
) -> End {
    let started = Instant::now();
    let expires = started + plan.limits.lifetime;
    let cancelled = control.cancelled.clone();
    let prepared = (|| {
        raw.pragma_update(None, "query_only", 1).map_err(engine)?;
        raw.execute_batch("BEGIN").map_err(engine)?;
        let revision = start_snapshot(raw, plan.revision.as_deref())?;
        if let Some((sql, expected)) = &plan.epoch {
            let epoch: i64 = raw.query_row(sql, [], |row| row.get(0)).map_err(engine)?;
            if u64::try_from(epoch).ok() != Some(*expected) {
                return Err(Error::Conflict);
            }
        }
        raw.authorizer(Some(snapshot_authorizer(plan.readable.clone())))
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
                report_on_next_request(requests, plan.limits.idle, engine(error)).await;
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
            let request = time::timeout(wait.min(POLL), requests.recv()).await;
            if matches!(request, Ok(Some(ref request)) if !matches!(request, Request::Wake)) {
                active = Instant::now();
            }
            if control.cancelled.load(Ordering::SeqCst) {
                if let Ok(Some(request)) = request {
                    request.fail(control.error());
                }
                break 'exec End::Cancelled;
            }
            match request {
                Ok(Some(Request::Next(reply))) => {
                    let page = page(&mut rows, plan, &mut position, control);
                    let failed = page.is_err();
                    let _ = reply.send(page);
                    if failed {
                        break 'exec control.ending().unwrap_or(End::Failed);
                    }
                }
                Ok(Some(Request::Reset(reply))) => {
                    let _ = reply.send(Ok(()));
                    continue 'exec;
                }
                Ok(Some(Request::Run(statement))) => {
                    let result = whole(
                        raw,
                        plan,
                        statement.sql,
                        statement.params,
                        statement.max_bytes,
                    );
                    let failed = result.is_err() && control.cancelled.load(Ordering::SeqCst);
                    let _ = statement
                        .reply
                        .send(result.map_err(|error| match control.ending() {
                            Some(End::Cancelled | End::Preempted) => control.error(),
                            _ => error,
                        }));
                    if failed {
                        break 'exec End::Cancelled;
                    }
                }
                Ok(Some(Request::Wake)) | Err(_) => {}
                Ok(None) => break 'exec End::Finished,
            }
        }
    };
    drop(statement);
    let _ = raw.execute_batch("ROLLBACK");
    end
}

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

#[derive(Default)]
struct Position {
    exhausted: bool,
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

fn page(
    rows: &mut rusqlite::Rows<'_>,
    plan: &CursorPlan,
    position: &mut Position,
    control: &Control,
) -> Result<CursorPage, Error> {
    let interrupted = |error: Error| {
        if let Some(End::Cancelled | End::Preempted) = control.ending() {
            return control.error();
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
    (plan.guard)().inspect_err(|_| control.end(End::Failed))?;
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct File {
        _directory: rocky_platform::fs::Scratch,
        path: PathBuf,
        writer: sqlite::Connection,
    }
    async fn file(rows: i64) -> File {
        let directory = rocky_platform::fs::Scratch::new().await;
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
        plan_at(&file.path, sql, limits, guard)
    }
    fn plan_at(
        path: &std::path::Path,
        sql: &str,
        limits: CursorLimits,
        guard: Guard,
    ) -> CursorPlan {
        CursorPlan {
            path: path.to_path_buf(),
            sql: sql.into(),
            params: Vec::new(),
            readable: vec!["items".into()],
            epoch: None,
            revision: Some("SELECT count(*) FROM items".into()),
            limits,
            guard,
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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_thousand_cursor_leases_reuse_one_worker() {
        let file = file(1).await;
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        for _ in 0..1000 {
            let plan = plan(&file, "SELECT id,title FROM items", small(1), open());
            let mut cursor = SnapshotCursor::open(&host, count.clone(), plan)
                .await
                .unwrap();
            assert_eq!(cursor.next().await.unwrap().rows.len(), 1);
            drop(cursor);
        }
        assert_eq!(host.workers(), 1);
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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn freed_workers_beyond_the_idle_bound_retire() {
        let file = file(1).await;
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        let mut cursors = Vec::new();
        for _ in 0..5 {
            let plan = plan(&file, "SELECT id FROM items", small(1), open());
            cursors.push(
                SnapshotCursor::open(&host, count.clone(), plan)
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(host.workers(), 5);
        drop(cursors);
        let a_worker_still_holds_its_control = || {
            host.controls
                .lock()
                .unwrap()
                .iter()
                .any(|control| control.strong_count() > 0)
        };
        while a_worker_still_holds_its_control() {
            task::yield_now().await;
        }
        assert_eq!(host.workers(), MAX_IDLE_WORKERS);
        let mut cursor = SnapshotCursor::open(
            &host,
            count,
            plan(&file, "SELECT id FROM items", small(1), open()),
        )
        .await
        .unwrap();
        assert_eq!(cursor.next().await.unwrap().rows.len(), 1);
        assert_eq!(host.workers(), MAX_IDLE_WORKERS);
        drop(cursor);
        host.close();
        assert_eq!(host.workers(), 0);
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn readiness_from_another_commit_is_refused_inside_the_snapshot() {
        let file = file(1).await;
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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn pages_come_from_one_snapshot_and_reset_restarts_it() {
        let file = file(5).await;
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
        assert_eq!(
            ids,
            (0..5).map(SqlValue::Integer).collect::<Vec<_>>(),
            "commits after the cursor opened are not part of its snapshot"
        );
        assert!(cursor.next().await.unwrap().done);
        cursor.reset().await.unwrap();
        assert_eq!(
            cursor.next().await.unwrap().rows[0][0],
            SqlValue::Integer(0)
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(cursor);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let mut cursor = SnapshotCursor::open(
            &host,
            count,
            plan(&file, "SELECT id FROM items", small(10), open()),
        )
        .await
        .unwrap();
        assert_eq!(cursor.revision(), Some(3));
        assert_eq!(
            cursor.next().await.unwrap().rows.len(),
            3,
            "a new cursor sees the new snapshot"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn further_statements_read_the_cursor_snapshot() {
        let file = file(3).await;
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
        let counted = cursor
            .run(
                "SELECT count(*) AS n FROM items WHERE id >= ?".into(),
                vec![SqlValue::Integer(1)],
                1024,
            )
            .await
            .unwrap();
        assert_eq!(counted.columns[0].name, "n");
        assert_eq!(
            counted.rows,
            vec![vec![SqlValue::Integer(2)]],
            "another statement between pages still reads the opened snapshot"
        );
        assert_eq!(
            cursor.next().await.unwrap().rows,
            vec![vec![SqlValue::Integer(1)]]
        );
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
            vec![vec![SqlValue::Integer(2)]],
            "refused writes, other tables and oversized results leave the cursor usable"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn statements_are_confined_and_pages_are_bounded() {
        let file = file(3).await;
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
        assert!(
            pages.len() > 1,
            "a row that does not fit a page opens the next one"
        );
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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn bounds_revocation_and_host_close_end_cursors_deterministically() {
        let file = file(3).await;
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
        while host.open_cursors() > 0 {
            task::yield_now().await;
        }
        assert!(matches!(
            cursor.next().await,
            Err(Error::Sql(SqlError::Aborted(AbortReason::Timeout)))
        ));
        let revoked = Arc::new(AtomicBool::new(false));
        let flag = revoked.clone();
        let guard: Guard = Arc::new(move || {
            if flag.load(Ordering::SeqCst) {
                Err(Error::Sql(SqlError::AuthorityChanged))
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
        assert!(
            matches!(
                cursor.next().await,
                Err(Error::Sql(SqlError::AuthorityChanged))
            ),
            "the guard discards a stepped page once authority is gone"
        );
        assert!(cursor.next().await.is_err());
        let mut cursor = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(&file, "SELECT id FROM items", small(1), open()),
        )
        .await
        .unwrap();
        cursor.cancel();
        assert!(cursor.next().await.is_err(), "cancelling ends the cursor");
        let mut cursor = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(&file, "SELECT id FROM items", small(1), open()),
        )
        .await
        .unwrap();
        host.close();
        assert!(cursor.next().await.is_err(), "closing ends open cursors");
        assert!(
            matches!(
                SnapshotCursor::open(
                    &host,
                    count.clone(),
                    plan(&file, "SELECT id FROM items", small(1), open())
                )
                .await,
                Err(Error::Closed)
            ),
            "closing refuses new cursors"
        );
        drop(cursor);
        file.writer
            .run_now("PRAGMA wal_checkpoint(TRUNCATE)")
            .expect("the WAL resets once no cursor pins it");
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn preempting_ends_the_cursors_that_pin_the_log() {
        let file = file(3).await;
        file.writer.set_busy_timeout(Duration::ZERO).unwrap();
        let busy_log_and_checkpointed_frames_of_reset =
            || match file.writer.rows("PRAGMA wal_checkpoint(TRUNCATE)", ()) {
                Ok(rows) => rows[0].clone(),
                Err(error) => panic!("{error}"),
            };
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        let mut idle = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(&file, "SELECT id FROM items", small(1), open()),
        )
        .await
        .unwrap();
        idle.next().await.unwrap();
        let mut unread = SnapshotCursor::open(
            &host,
            count.clone(),
            plan(&file, "SELECT id FROM items", small(1), open()),
        )
        .await
        .unwrap();
        file.writer
            .run_now("INSERT INTO items VALUES(10,'later')")
            .unwrap();
        assert_eq!(
            busy_log_and_checkpointed_frames_of_reset()[0],
            Value::Integer(1),
            "the open snapshots pin the log"
        );
        let quiet = host.quiesce().await;
        assert_eq!(quiet.preempted(), 2);
        assert_eq!(
            busy_log_and_checkpointed_frames_of_reset(),
            vec![Value::Integer(0), Value::Integer(0), Value::Integer(0)]
        );
        assert_eq!(host.open_cursors(), 0);
        for cursor in [&mut idle, &mut unread] {
            assert!(matches!(
                cursor.next().await,
                Err(Error::Sql(SqlError::Aborted(AbortReason::Preempted)))
            ));
        }
        drop(quiet);
        assert_eq!(host.quiesce().await.preempted(), 0);
        drop((idle, unread));
        let mut cursor = SnapshotCursor::open(
            &host,
            count,
            plan(&file, "SELECT id FROM items", small(10), open()),
        )
        .await
        .unwrap();
        assert_eq!(
            cursor.revision(),
            Some(4),
            "later cursors read the new snapshot"
        );
        assert_eq!(cursor.next().await.unwrap().rows.len(), 4);
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn one_quiesce_truncates_the_log_under_readers_that_keep_reopening() {
        let file = file(3).await;
        file.writer.set_busy_timeout(Duration::ZERO).unwrap();
        let host = CursorHost::new();
        let count = Arc::new(AtomicUsize::new(0));
        let (seen, mut revisions) = mpsc::unbounded_channel();
        let reader = {
            let (host, count, path) = (host.clone(), count.clone(), file.path.clone());
            let reopen = move || {
                let (host, count) = (host.clone(), count.clone());
                let plan = plan_at(&path, "SELECT id FROM items", small(1), open());
                async move { SnapshotCursor::open(&host, count, plan).await }
            };
            rocky_platform::task::spawn(async move {
                let Ok(mut current) = reopen().await else {
                    return;
                };
                loop {
                    let _ = current.next().await;
                    let Ok(next) = reopen().await else {
                        return;
                    };
                    if seen.send(next.revision()).is_err() {
                        return;
                    }
                    current = next;
                }
            })
        };
        let observed = async |revisions: &mut mpsc::UnboundedReceiver<Option<u64>>, rows: u64| {
            while revisions.recv().await.expect("the reader keeps reopening") != Some(rows) {}
        };
        observed(&mut revisions, 3).await;
        file.writer
            .run_now("INSERT INTO items VALUES(10,'during')")
            .unwrap();
        observed(&mut revisions, 4).await;

        let quiet = host.quiesce().await;
        assert!(
            quiet.preempted() >= 1,
            "the reopening reader always holds a snapshot"
        );
        host.held_opens().wait_for(|held| *held >= 1).await.unwrap();
        assert_eq!(host.open_cursors(), 0, "the reopen waits at the gate");
        file.writer
            .run_now("INSERT INTO items VALUES(11,'gated')")
            .unwrap();
        assert_eq!(
            file.writer
                .rows("PRAGMA wal_checkpoint(TRUNCATE)", ())
                .unwrap()[0],
            vec![Value::Integer(0), Value::Integer(0), Value::Integer(0)],
            "one quiesce resets the log although the reader keeps reopening"
        );
        let mut wal = file.path.as_os_str().to_owned();
        wal.push("-wal");
        assert_eq!(
            rocky_platform::fs::len(std::path::Path::new(&wal)).unwrap(),
            0
        );
        drop(quiet);

        file.writer
            .run_now("INSERT INTO items VALUES(12,'after')")
            .unwrap();
        observed(&mut revisions, 6).await;
        host.close();
        reader.await.unwrap();
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_session_holds_at_most_its_cursor_quota() {
        let file = file(1).await;
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
