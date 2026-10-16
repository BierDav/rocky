//! SQL sessions: ordinary statements and transactions on a domain's typed
//! tables, committed as precise operations. See `docs/runtime/sql-sessions.md`.
use crate::{Command, CommitClass, Error, MutationHandle, Replica, Reply};
use rocky_core::{
    Commitment, DatabaseBinding, LocalRevision, OperationId, Scope, SqlValue, StoredApplication,
    TransactionToken,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering::Relaxed},
};
use tokio::sync::{mpsc, oneshot};

pub use rocky_storage::sql::*;

/// `first` is the sequence after the last one committed before the
/// transaction, so `first+count-1` is the last sequence it covers.
pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS rocky_sql_transactions(token BLOB PRIMARY KEY,input BLOB NOT NULL,intent BLOB NOT NULL,node BLOB NOT NULL,first INTEGER NOT NULL,count INTEGER NOT NULL,revision INTEGER NOT NULL) WITHOUT ROWID",
    // Pruning walks the records in commit order.
    "CREATE INDEX IF NOT EXISTS rocky_sql_transactions_first ON rocky_sql_transactions(first)",
];
/// What one domain committed of a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainOutcome {
    pub scope: Scope,
    /// Commits to the domain's changed rows; see `rocky_core::intent_commitment`.
    pub intent: Commitment,
    /// The operations the domain's rows became, in order. Their bytes are
    /// `Replica::lookup_mutation(operation_handle(id))` until pruned.
    pub operations: Vec<OperationId>,
    pub revision: LocalRevision,
}
/// The committed result of one transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionOutcome {
    pub token: Option<TransactionToken>,
    /// Of a domain session: its domain's intent. Of a bound database with
    /// several domains: a commitment to every domain's scope and intent.
    pub intent: Commitment,
    /// Every operation, domain by domain in scope order.
    pub operations: Vec<OperationId>,
    /// The revision of the first domain; `domains` has each.
    pub revision: LocalRevision,
    pub class: CommitClass,
    /// An earlier commit of this token is reported; the transaction that
    /// asked was rolled back.
    pub replayed: bool,
    /// One entry per domain of the session, in scope order.
    pub domains: Vec<DomainOutcome>,
}
/// One outcome from each domain's part of the same transaction.
pub(crate) fn combine(mut parts: Vec<TransactionOutcome>) -> Result<TransactionOutcome, Error> {
    let corrupt = || Error::Corrupt("domains disagree about a SQL transaction".into());
    if parts.len() < 2 {
        return parts.pop().ok_or_else(corrupt);
    }
    let first = parts[0].clone();
    let mut bytes = Vec::new();
    let mut combined = TransactionOutcome {
        operations: Vec::new(),
        domains: Vec::new(),
        ..first
    };
    for part in parts {
        if part.token != combined.token || part.replayed != combined.replayed {
            return Err(corrupt());
        }
        if part.class == CommitClass::Buffered {
            combined.class = CommitClass::Buffered;
        }
        for domain in &part.domains {
            for id in [
                domain.scope.mesh.0,
                domain.scope.app.0,
                domain.scope.domain.0,
            ] {
                bytes.extend(id);
            }
            bytes.extend(domain.intent);
        }
        combined.operations.extend(part.operations);
        combined.domains.extend(part.domains);
    }
    combined.intent = rocky_core::commitment("rocky.sql.database-intent.v1", &bytes);
    Ok(combined)
}
/// Stamps the statements of a domain's own session.
pub(crate) fn domain_stamp(scope: Scope) -> Commitment {
    let bytes = [scope.mesh.0, scope.app.0, scope.domain.0].concat();
    rocky_core::commitment("rocky.sql.domain-session.v1", &bytes)
}
/// The handle under which an operation of a SQL transaction is recorded.
pub fn operation_handle(id: OperationId) -> MutationHandle {
    let mut bytes = id.node.0.to_vec();
    bytes.extend(id.sequence.get().to_le_bytes());
    MutationHandle(rocky_core::commitment("rocky.sql.operation", &bytes))
}

/// A compiled description of one statement. Preparing admits nothing: the
/// statement is authorized again and executed only by `SqlSession::execute`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlStatement {
    sql: Arc<str>,
    /// The session binding that compiled it.
    binding: Commitment,
    pub columns: Vec<String>,
    pub parameters: usize,
    /// The statement changes no row. Transaction control is not read-only.
    pub read_only: bool,
}
impl SqlStatement {
    pub fn sql(&self) -> &str {
        &self.sql
    }
    pub(crate) fn new(sql: &str, columns: Vec<String>, parameters: usize, read_only: bool) -> Self {
        Self {
            sql: sql.into(),
            binding: [0; 32],
            columns,
            parameters,
            read_only,
        }
    }
}
/// A statement's complete result. Inside a transaction its rows, counts and
/// row id are provisional until the commit.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    /// As SQL `changes()` and `last_insert_rowid()` report to this session.
    pub changes: u64,
    pub last_insert_rowid: i64,
    pub state: SqlState,
    /// Set by the call that committed: `COMMIT`, or a writing statement
    /// outside a transaction.
    pub committed: Option<TransactionOutcome>,
}
impl SqlRows {
    /// A cursor over the buffered result.
    pub fn cursor(self) -> SqlCursor {
        SqlCursor {
            columns: self.columns,
            rows: self.rows.into_iter(),
        }
    }
}
pub struct SqlCursor {
    columns: Vec<String>,
    rows: std::vec::IntoIter<Vec<SqlValue>>,
}
impl SqlCursor {
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
}
impl Iterator for SqlCursor {
    type Item = Vec<SqlValue>;
    fn next(&mut self) -> Option<Self::Item> {
        self.rows.next()
    }
}

/// What the writer authorized for one snapshot cursor of a session.
pub(crate) struct CursorParts {
    pub(crate) sql: String,
    pub(crate) readable: Vec<String>,
    pub(crate) path: std::path::PathBuf,
    pub(crate) revision: Option<String>,
    pub(crate) host: Arc<crate::cursor::CursorHost>,
}
pub(crate) enum Call {
    NextIdentity(String, Vec<SqlValue>, Reply<rocky_core::RowIdentity>),
    Prepare(String, Reply<SqlStatement>),
    Cursor(String, Reply<CursorParts>),
    Catalog(Reply<Vec<crate::capture::CatalogTable>>),
    Execute {
        sql: String,
        params: Vec<SqlValue>,
        token: Option<TransactionToken>,
        reply: Reply<SqlRows>,
    },
    Cancel,
}
pub(crate) struct Opening {
    pub(crate) calls: mpsc::Receiver<Call>,
    pub(crate) options: SqlOptions,
    pub(crate) cancelled: Arc<AtomicBool>,
    /// Set for a bound database; None is the domain's own tables.
    pub(crate) binding: Option<Arc<DatabaseBinding>>,
}
pub(crate) enum Request {
    Open(Box<Opening>),
    Outcome(TransactionToken),
}
impl Request {
    /// A recovered outcome leaves the device, so it waits for a flush.
    pub(crate) fn exposes(&self) -> bool {
        matches!(self, Self::Outcome(_))
    }
}
pub(crate) enum Response {
    Opened,
    Outcome(Option<TransactionOutcome>),
}

/// A connection handle. Its transaction leases the file's writer; dropping
/// the session rolls an open transaction back.
pub struct SqlSession {
    calls: mpsc::Sender<Call>,
    /// Open snapshot cursors of this session.
    cursors: Arc<std::sync::atomic::AtomicUsize>,
    cancelled: Arc<AtomicBool>,
    /// Stamps this session's statements: its domain, or its binding and generation.
    binding: Commitment,
}
impl SqlSession {
    /// Opens a session through `queue`, the command queue of one of its domains.
    pub(crate) async fn open<R>(
        queue: &crate::CommandQueue<R>,
        options: SqlOptions,
        binding: Option<Arc<DatabaseBinding>>,
        stamp: Commitment,
    ) -> Result<Self, Error> {
        options.validate()?;
        let (calls, receiver) = mpsc::channel(4);
        let cancelled = Arc::new(AtomicBool::new(false));
        let (reply, result) = oneshot::channel();
        let opening = Opening {
            calls: receiver,
            options,
            cancelled: cancelled.clone(),
            binding,
        };
        queue
            .send(Command::Sql(Request::Open(Box::new(opening)), reply))
            .await?;
        result.await.map_err(|_| Error::Closed)??;
        Ok(Self {
            calls,
            cursors: Arc::default(),
            cancelled,
            binding: stamp,
        })
    }
    /// Reserves an identity in an active transaction; rollback releases it.
    pub async fn next_id(&self, table: &str) -> Result<rocky_core::RowIdentity, Error> {
        self.next_id_partition(table, &[]).await
    }
    pub async fn next_id_partition(
        &self,
        table: &str,
        values: &[SqlValue],
    ) -> Result<rocky_core::RowIdentity, Error> {
        let (reply, result) = oneshot::channel();
        self.call(Call::NextIdentity(table.into(), values.to_vec(), reply))
            .await?;
        result.await.map_err(|_| Error::Closed)?
    }
    pub async fn prepare(&self, sql: &str) -> Result<SqlStatement, Error> {
        let (reply, result) = oneshot::channel();
        self.call(Call::Prepare(sql.into(), reply)).await?;
        let mut statement = result.await.map_err(|_| Error::Closed)??;
        statement.binding = self.binding;
        Ok(statement)
    }
    /// A statement prepared by a session of another domain or binding is
    /// refused with `SqlError::Stale`.
    fn current<'a>(&self, statement: &'a SqlStatement) -> Result<&'a str, Error> {
        if statement.binding != self.binding {
            return Err(Error::Sql(SqlError::Stale));
        }
        Ok(statement.sql())
    }
    pub async fn execute(
        &self,
        statement: &SqlStatement,
        params: Vec<SqlValue>,
    ) -> Result<SqlRows, Error> {
        self.submit(self.current(statement)?, params, None).await
    }
    /// `execute` for the call that commits: `token` recovers its outcome.
    pub async fn execute_with_token(
        &self,
        statement: &SqlStatement,
        params: Vec<SqlValue>,
        token: TransactionToken,
    ) -> Result<SqlRows, Error> {
        self.submit(self.current(statement)?, params, Some(token))
            .await
    }
    /// Prepares and executes in one call.
    pub async fn run(&self, sql: &str, params: Vec<SqlValue>) -> Result<SqlRows, Error> {
        self.submit(sql, params, None).await
    }
    /// `COMMIT` under a token.
    pub async fn commit(&self, token: TransactionToken) -> Result<SqlRows, Error> {
        self.submit("COMMIT", Vec::new(), Some(token)).await
    }
    /// Opens a cursor over the latest committed snapshot of the session's
    /// tables. It reads committed rows only, never the session's own open
    /// transaction, and holds no writer lease. See `CursorLimits`.
    pub async fn cursor(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
        limits: crate::cursor::CursorLimits,
    ) -> Result<crate::cursor::SnapshotCursor, Error> {
        let (reply, result) = oneshot::channel();
        self.call(Call::Cursor(sql.into(), reply)).await?;
        let parts = result.await.map_err(|_| Error::Closed)??;
        let plan = crate::cursor::CursorPlan {
            path: parts.path,
            sql: parts.sql,
            params,
            readable: parts.readable,
            revision: parts.revision,
            limits,
            guard: Arc::new(|| Ok(())),
            prelude: Vec::new(),
            requires: None,
        };
        crate::cursor::SnapshotCursor::open(&parts.host, self.cursors.clone(), plan).await
    }
    /// The tables this session may name, under the names it uses. Protocol
    /// tables, other domains and physical names never appear.
    pub async fn catalog(&self) -> Result<Vec<crate::capture::CatalogTable>, Error> {
        let (reply, result) = oneshot::channel();
        self.call(Call::Catalog(reply)).await?;
        result.await.map_err(|_| Error::Closed)?
    }
    /// Interrupts the running statement and rolls the transaction back.
    pub fn cancel(&self) {
        self.cancelled.store(true, Relaxed);
        let _ = self.calls.try_send(Call::Cancel);
    }
    async fn submit(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
        token: Option<TransactionToken>,
    ) -> Result<SqlRows, Error> {
        let weight = params.iter().map(weight).sum::<usize>() + sql.len();
        if weight > crate::MAX_FRAME_BYTES {
            return Err(Error::Quota {
                resource: "frame bytes",
                limit: crate::MAX_FRAME_BYTES,
            });
        }
        let (reply, result) = oneshot::channel();
        self.call(Call::Execute {
            sql: sql.into(),
            params,
            token,
            reply,
        })
        .await?;
        result.await.map_err(|_| Error::Closed)?
    }
    async fn call(&self, call: Call) -> Result<(), Error> {
        self.calls.send(call).await.map_err(|_| Error::Closed)
    }
}
fn weight(value: &SqlValue) -> usize {
    match value {
        SqlValue::Text(text) => text.len(),
        SqlValue::Blob(bytes) => bytes.len(),
        _ => 8,
    }
}

impl<A> Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    /// Opens a SQL session on this domain's typed tables. The installed
    /// application must translate captured rows; see `StoredApplication::sql_writes`.
    pub async fn sql_session(&self, options: SqlOptions) -> Result<SqlSession, Error> {
        let stamp = domain_stamp(self.inner.scope);
        SqlSession::open(&self.inner.commands, options, None, stamp).await
    }
    /// The outcome committed under `token`, or None if no commit carried it:
    /// then the transaction did not happen and its body may run again.
    /// Answered once a flush covers the commit. A record is forgotten with
    /// the mutation handles of its time, `RetentionPolicy::handles` local
    /// operations after its history was pruned; ask before that.
    pub async fn sql_outcome(
        &self,
        token: TransactionToken,
    ) -> Result<Option<TransactionOutcome>, Error> {
        let (reply, result) = oneshot::channel();
        self.inner
            .commands
            .send(Command::Sql(Request::Outcome(token), reply))
            .await?;
        match result.await.map_err(|_| Error::Closed)?? {
            Response::Outcome(outcome) => Ok(outcome),
            Response::Opened => Err(Error::Corrupt("SQL outcome reply".into())),
        }
    }
}
