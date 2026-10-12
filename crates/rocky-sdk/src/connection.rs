//! SQL connections for a database driver. A writer is a secure SQL
//! session: one transaction is one commit and one signed bundle. A reader
//! steps snapshot cursors and never holds the writer. Values cross typed,
//! never as JSON; a compatibility profile answers an ORM's housekeeping
//! statements. See `docs/runtime/sql-driver.md`.
use crate::{Failure, node::Node};
use rocky_core::{SqlValue, TypedCommands};
use rocky_runtime::{CursorLimits, SnapshotCursor, SqlControl, TableObserver, classify_sql};
use rocky_secure::{Error, SecureSession, SecureSql, SqlError, Unavailable};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::Mutex;

mod room;

/// Rows of one page a statement result is delivered in.
const PAGE_ROWS: usize = 256;
/// Value bytes of one page; a larger single row is a page of its own.
const PAGE_BYTES: usize = 1024 * 1024;
/// Statements with an undelivered result on one connection.
const MAX_RESULTS: usize = 64;

/// The statements of one ORM version a connection answers itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Room 2.8.4 with androidx.sqlite 2.6.2: its connection setup, schema
    /// validation, identity table and invalidation tracker.
    Room284,
}
impl std::str::FromStr for Profile {
    type Err = Failure;
    fn from_str(name: &str) -> Result<Self, Failure> {
        match name {
            "room-2.8.4" => Ok(Self::Room284),
            other => Err(format!("unsupported connection profile {other}").into()),
        }
    }
}
/// What a connection is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionOptions {
    /// A reader steps snapshot cursors and refuses writes. `PRAGMA
    /// query_only` switches the kind outside a transaction.
    pub read_only: bool,
    /// Answers this ORM's housekeeping statements; without it they are
    /// denied like any pragma.
    pub profile: Option<Profile>,
    /// How long a write transaction may hold the replica; 5 s by default,
    /// at most 60 s.
    pub transaction_timeout: Option<std::time::Duration>,
}
impl ConnectionOptions {
    fn sql(&self) -> rocky_secure::SqlOptions {
        // A driver never hands frames out: sync flushes before it sends them.
        let mut options = rocky_secure::SqlOptions {
            acknowledge: rocky_secure::CommitClass::Buffered,
            ..Default::default()
        };
        if let Some(timeout) = self.transaction_timeout {
            options.transaction_timeout = timeout;
        }
        options
    }
}

/// One page of a statement's result. Columns are known before any row.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    /// No further page follows.
    pub done: bool,
}

/// A failed statement with SQLite's result code, so a driver can raise
/// what the engine would have raised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlFailure {
    pub code: i32,
    pub message: String,
}
impl std::fmt::Display for SqlFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Error code: {}, message: {}", self.code, self.message)
    }
}
impl std::error::Error for SqlFailure {}

/// SQLite result codes this layer reports.
pub(crate) mod code {
    pub const ERROR: i32 = 1;
    pub const ABORT: i32 = 4;
    pub const BUSY: i32 = 5;
    pub const READONLY: i32 = 8;
    pub const TOOBIG: i32 = 18;
    pub const MISUSE: i32 = 21;
    pub const AUTH: i32 = 23;
}
pub(crate) fn failure(code: i32, message: impl Into<String>) -> SqlFailure {
    SqlFailure {
        code,
        message: message.into(),
    }
}
/// The code and message of a runtime failure.
pub(crate) fn sql_failure(error: Failure) -> SqlFailure {
    let error = match error.downcast::<SqlFailure>() {
        Ok(failure) => return *failure,
        Err(error) => error,
    };
    let Some(error) = error.downcast_ref::<Error>() else {
        return failure(code::ERROR, error.to_string());
    };
    let code = match error {
        Error::Sql(SqlError::Statement { code, .. }) => *code,
        Error::Sql(SqlError::Denied) => code::AUTH,
        Error::Sql(SqlError::Aborted(_)) => code::ABORT,
        Error::Quota { .. } => code::TOOBIG,
        Error::Backpressure => code::BUSY,
        Error::Closed => code::MISUSE,
        _ => code::ERROR,
    };
    let message = match error {
        Error::Sql(SqlError::Statement { message, .. }) => message.clone(),
        Error::Quota { resource, limit } => format!("{resource} exceed {limit}"),
        other => other.to_string(),
    };
    failure(code, message)
}

/// `PRAGMA query_only`: Some(None) reads it, Some(Some(on)) sets it.
fn query_only(sql: &str) -> Option<Option<bool>> {
    let spaced = sql.trim().trim_end_matches(';').replace('=', " = ");
    let words: Vec<String> = spaced
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    match words.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["pragma", "query_only"] => Some(None),
        ["pragma", "query_only", "=", value] => match value {
            "1" | "on" | "true" | "yes" => Some(Some(true)),
            "0" | "off" | "false" | "no" => Some(Some(false)),
            _ => None,
        },
        _ => None,
    }
}

/// The statement that reserves a row id before its insert.
const NEXT_ID: &str = "rocky_next_id";

/// The table of `SELECT rocky_next_id('table')` or `SELECT
/// rocky_next_id(?)`; refuses other calls and leaves unrelated SQL alone.
fn next_id(sql: &str, params: &[SqlValue]) -> Result<Option<(String, Vec<SqlValue>)>, Failure> {
    let words = room::words(sql);
    let function =
        |word: &str| !word.starts_with('\'') && room::unquote(word).eq_ignore_ascii_case(NEXT_ID);
    let names_function = words
        .windows(2)
        .any(|pair| function(&pair[0]) && pair[1] == "(");
    if !names_function {
        return Ok(None);
    }
    if words.len() < 5
        || !words[0].eq_ignore_ascii_case("SELECT")
        || !function(&words[1])
        || words[2] != "("
        || words.last().is_none_or(|word| word != ")")
        || words[3..words.len() - 1]
            .iter()
            .any(|word| word == "(" || word == ")")
    {
        return Err(failure(code::ERROR, "rocky_next_id requires a standalone SELECT rocky_next_id(table, ...) without aliases or expressions").into());
    }
    if words[3..words.len() - 1].iter().any(|word| word == "-") {
        return Err(failure(
            code::ERROR,
            "rocky_next_id negative literals must be supplied as bound parameters",
        )
        .into());
    }
    if words.len().is_multiple_of(2) {
        return Err(failure(
            code::ERROR,
            "rocky_next_id requires an argument after each comma",
        )
        .into());
    }
    let mut bound = params.iter();
    let mut args = Vec::new();
    for (index, word) in words[3..words.len() - 1].iter().enumerate() {
        if index % 2 == 1 {
            if word != "," {
                return Err(failure(code::ERROR, "rocky_next_id argument separator").into());
            }
            continue;
        }
        let value = if word == "?" {
            bound
                .next()
                .cloned()
                .ok_or_else(|| failure(code::ERROR, "rocky_next_id missing parameter"))?
        } else if word.starts_with('\'') {
            SqlValue::Text(room::unquote(word))
        } else {
            SqlValue::Integer(
                word.parse()
                    .map_err(|_| failure(code::ERROR, "rocky_next_id requires scalar arguments"))?,
            )
        };
        args.push(value);
    }
    if bound.next().is_some() {
        return Err(failure(code::ERROR, "rocky_next_id extra parameters").into());
    }
    let Some(SqlValue::Text(table)) = args.first() else {
        return Err(failure(
            code::ERROR,
            "rocky_next_id takes a table name followed by partition values",
        )
        .into());
    };
    Ok(Some((table.clone(), args[1..].to_vec())))
}

/// What remains of an executed statement's result.
enum Pending {
    Rows {
        columns: Vec<String>,
        rows: VecDeque<Vec<SqlValue>>,
    },
    Cursor(SnapshotCursor),
}
impl Pending {
    fn rows(columns: Vec<String>, rows: Vec<Vec<SqlValue>>) -> Self {
        Self::Rows {
            columns,
            rows: rows.into(),
        }
    }
    async fn page(&mut self) -> Result<Page, Failure> {
        match self {
            Self::Rows { columns, rows } => {
                let mut page = Vec::new();
                let mut bytes = 0;
                while let Some(row) = rows.front() {
                    let size = weight(row);
                    if !page.is_empty() && (page.len() == PAGE_ROWS || bytes + size > PAGE_BYTES) {
                        break;
                    }
                    bytes += size;
                    page.extend(rows.pop_front());
                }
                Ok(Page {
                    columns: columns.clone(),
                    rows: page,
                    done: rows.is_empty(),
                })
            }
            Self::Cursor(cursor) => {
                let page = cursor.next().await?;
                Ok(Page {
                    columns: names(cursor),
                    rows: page.rows,
                    done: page.done,
                })
            }
        }
    }
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
fn names(cursor: &SnapshotCursor) -> Vec<String> {
    cursor
        .columns()
        .iter()
        .map(|column| column.name.clone())
        .collect()
}

/// A reader's transaction: statements share one snapshot, taken by the
/// first of them, as SQLite's deferred transaction does.
#[derive(Default)]
struct Reading {
    snapshot: Option<SnapshotCursor>,
    savepoints: Vec<String>,
    /// A `SAVEPOINT` began it; releasing that savepoint ends it.
    implicit: bool,
}

/// The session a connection runs on, opened again after an authority
/// change ended it.
struct Opened {
    session: SecureSession<TypedCommands>,
    sql: SecureSql<TypedCommands>,
}

/// One connection's state on the database's reactor.
pub(crate) struct State {
    options: ConnectionOptions,
    /// The connection is a reader now; see `query_only`.
    read_only: bool,
    node: Arc<Node>,
    opened: Option<Opened>,
    /// A writer's `BEGIN` not yet sent: the session takes the writer only
    /// once a statement needs the file.
    begin: Option<&'static str>,
    /// The writer's transaction has not ended.
    open: bool,
    reading: Option<Reading>,
    results: BTreeMap<u64, Pending>,
    room: room::Housekeeping,
    /// The installed schema's version, read between transactions: a
    /// statement must not wait for the settings while its transaction holds
    /// the replica, since a renewal holds them while it waits for the replica.
    version: u32,
}
impl State {
    async fn new(node: Arc<Node>, options: ConnectionOptions) -> Result<Self, Failure> {
        let room = match options.profile {
            Some(Profile::Room284) => {
                room::Housekeeping::new(Some(node.replica.observe_tables(None).await?))
            }
            None => room::Housekeeping::new(None),
        };
        let version = node.settings.lock().await.schema.version;
        let mut state = Self {
            read_only: options.read_only,
            options,
            node,
            opened: None,
            begin: None,
            open: false,
            reading: None,
            results: BTreeMap::new(),
            room,
            version,
        };
        state.sql().await?;
        Ok(state)
    }
    fn active(&self) -> bool {
        if self.read_only {
            self.reading.is_some()
        } else {
            self.begin.is_some() || self.open
        }
    }
    fn authority_pinned(&self) -> bool {
        self.open
            || self
                .reading
                .as_ref()
                .is_some_and(|reading| reading.snapshot.is_some())
    }
    /// The connection's session; a new one once the old one lost its
    /// authority outside a transaction.
    async fn sql(&mut self) -> Result<&SecureSql<TypedCommands>, Failure> {
        let current = match &self.opened {
            Some(opened) => self.authority_pinned() || opened.session.status().await.is_ok(),
            None => false,
        };
        if !current {
            let (session, sql) = self.node.sql_session(self.options.sql()).await?;
            self.opened = Some(Opened { session, sql });
        }
        Ok(&self.opened.as_ref().ok_or("connection has no session")?.sql)
    }

    /// Runs one statement and returns the first page of its result.
    pub(crate) async fn execute(
        &mut self,
        statement: u64,
        sql: &str,
        params: Vec<SqlValue>,
    ) -> Result<Page, Failure> {
        self.results.remove(&statement);
        if self.results.len() >= MAX_RESULTS {
            return Err(failure(code::MISUSE, "too many statements with open results").into());
        }
        let mut pending = match (query_only(sql), next_id(sql, &params)?) {
            (Some(set), _) => self.query_only(set)?,
            (None, Some((table, values))) => self.next_id(&table, &values).await?,
            (None, None) => match self.room_answer(sql, &params).await? {
                Some(answer) => answer,
                None if self.read_only => self.read(sql, params).await?,
                None => self.write(sql, params).await?,
            },
        };
        let page = pending.page().await?;
        if !page.done {
            self.results.insert(statement, pending);
        }
        Ok(page)
    }
    /// The next page of `statement`'s result.
    pub(crate) async fn next(&mut self, statement: u64) -> Result<Page, Failure> {
        let mut pending = self
            .results
            .remove(&statement)
            .ok_or_else(|| failure(code::MISUSE, "the statement has no further rows"))?;
        let page = pending.page().await?;
        if !page.done {
            self.results.insert(statement, pending);
        }
        Ok(page)
    }
    /// Drops the rest of `statement`'s result; its cursor ends.
    pub(crate) fn finish(&mut self, statement: u64) {
        self.results.remove(&statement);
    }
    pub(crate) fn in_transaction(&self) -> bool {
        self.active()
    }

    /// `PRAGMA query_only` as SQLite keeps it per connection: a writer
    /// holds nothing until it begins, so the kind can change between
    /// transactions.
    fn query_only(&mut self, set: Option<bool>) -> Result<Pending, Failure> {
        match set {
            None => Ok(Pending::rows(
                vec!["query_only".into()],
                vec![vec![SqlValue::Integer(i64::from(self.read_only))]],
            )),
            Some(_) if self.active() => {
                Err(failure(code::ERROR, "query_only cannot change inside a transaction").into())
            }
            Some(read_only) => {
                self.read_only = read_only;
                Ok(Pending::rows(Vec::new(), Vec::new()))
            }
        }
    }
    async fn room_answer(
        &mut self,
        sql: &str,
        params: &[SqlValue],
    ) -> Result<Option<Pending>, Failure> {
        let answer = self.room_answer_once(sql, params).await;
        if !self.authority_pinned()
            && answer
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("session authority changed"))
        {
            // Schema reads may race a key install after the status check.
            self.opened = None;
            return self.room_answer_once(sql, params).await;
        }
        answer
    }
    async fn room_answer_once(
        &mut self,
        sql: &str,
        params: &[SqlValue],
    ) -> Result<Option<Pending>, Failure> {
        if self.options.profile.is_none() {
            return Ok(None);
        }
        let read_only = self.read_only;
        let timeout = self.options.sql().transaction_timeout.as_millis();
        // The session of an idle connection may have lost its authority.
        if !self.authority_pinned() {
            self.version = self.node.settings.lock().await.schema.version;
            self.sql().await?;
        }
        let Self {
            opened,
            room,
            version,
            ..
        } = self;
        let version = *version;
        let sql_session = &opened.as_ref().ok_or("connection has no session")?.sql;
        room.answer(
            sql,
            params,
            room::Context {
                sql: sql_session,
                version,
                read_only,
                busy_timeout: i64::try_from(timeout).unwrap_or(i64::MAX),
            },
        )
        .await
    }

    /// Sends a pending `BEGIN`: the transaction now holds the replica.
    async fn begun(&mut self) -> Result<(), Failure> {
        if let Some(mode) = self.begin.take() {
            self.sql()
                .await?
                .run(&format!("BEGIN {mode}"), Vec::new())
                .await?;
            self.open = true;
        }
        Ok(())
    }

    /// `SELECT rocky_next_id(table)`: reserves the row id the next insert
    /// into `table` uses, inside the writer's transaction; a rollback
    /// releases it.
    async fn next_id(&mut self, table: &str, values: &[SqlValue]) -> Result<Pending, Failure> {
        if self.read_only {
            return Err(failure(
                code::READONLY,
                "rocky_next_id needs a write transaction; a reader cannot reserve row ids",
            )
            .into());
        }
        if !self.active() {
            return Err(failure(
                code::ERROR,
                "rocky_next_id needs an open transaction that also inserts the row",
            )
            .into());
        }
        self.begun().await?;
        let id = if values.is_empty() {
            self.sql().await?.next_id(table).await?
        } else {
            self.sql().await?.next_id_partition(table, values).await?
        };
        Ok(Pending::rows(
            vec![NEXT_ID.into()],
            vec![vec![SqlValue::Integer(id.integer())]],
        ))
    }

    async fn write(&mut self, sql: &str, params: Vec<SqlValue>) -> Result<Pending, Failure> {
        let control = classify_sql(sql).map_err(Error::Sql)?;
        match control {
            SqlControl::Begin(mode) => {
                if self.active() {
                    return Err(failure(
                        code::ERROR,
                        "cannot start a transaction within a transaction",
                    )
                    .into());
                }
                self.begin = Some(mode);
                return Ok(Pending::rows(Vec::new(), Vec::new()));
            }
            // Nothing reached the session: ending the transaction is local.
            SqlControl::Commit | SqlControl::Rollback if self.begin.is_some() => {
                self.begin = None;
                return Ok(Pending::rows(Vec::new(), Vec::new()));
            }
            _ => {}
        }
        self.begun().await?;
        let ending = matches!(control, SqlControl::Commit | SqlControl::Rollback);
        if matches!(control, SqlControl::Savepoint(_)) {
            self.open = true;
        }
        let ran = self.sql().await?.run(sql, params).await;
        if ending || ran.is_err() || matches!(control, SqlControl::Release(_)) {
            // A failed commit, an engine rollback or the release of the
            // outermost savepoint ends the transaction in the session; an
            // abort stays open until it is acknowledged.
            let aborted = matches!(&ran, Err(Error::Sql(SqlError::Aborted(_))));
            let active = self.sql().await?.state().await == rocky_secure::SqlState::Active;
            self.open = active || (aborted && !ending);
        }
        let rows = ran?;
        Ok(Pending::rows(rows.columns, rows.rows))
    }

    async fn read(&mut self, sql: &str, params: Vec<SqlValue>) -> Result<Pending, Failure> {
        let control = classify_sql(sql).map_err(Error::Sql)?;
        let none = || Ok(Pending::rows(Vec::new(), Vec::new()));
        let no_transaction = || -> Result<Pending, Failure> {
            Err(failure(code::ERROR, "no transaction is active").into())
        };
        let release = matches!(control, SqlControl::Release(_));
        match control {
            SqlControl::Begin("DEFERRED") if self.reading.is_none() => {
                self.reading = Some(Reading::default());
                none()
            }
            SqlControl::Begin("DEFERRED") => Err(failure(
                code::ERROR,
                "cannot start a transaction within a transaction",
            )
            .into()),
            SqlControl::Begin(_) => {
                Err(failure(code::READONLY, "a reader connection cannot write").into())
            }
            SqlControl::Commit | SqlControl::Rollback => match self.reading.take() {
                Some(_) => none(),
                None => no_transaction(),
            },
            SqlControl::Savepoint(name) => {
                let reading = self.reading.get_or_insert_with(|| Reading {
                    implicit: true,
                    ..Reading::default()
                });
                reading.savepoints.push(name);
                none()
            }
            SqlControl::Release(name) | SqlControl::RollbackTo(name) => {
                let Some(reading) = &mut self.reading else {
                    return no_transaction();
                };
                let Some(index) = reading
                    .savepoints
                    .iter()
                    .rposition(|savepoint| savepoint.eq_ignore_ascii_case(&name))
                else {
                    return Err(failure(code::ERROR, format!("no such savepoint: {name}")).into());
                };
                if release {
                    reading.savepoints.truncate(index);
                    if index == 0 && reading.implicit {
                        self.reading = None;
                    }
                } else {
                    reading.savepoints.truncate(index + 1);
                }
                none()
            }
            SqlControl::Statement => {
                let limits = CursorLimits::default();
                let opened = self.opened.as_ref().ok_or("connection has no session")?;
                let readonly = |error: Error| -> Failure {
                    match error {
                        Error::Sql(SqlError::Unsupported(reason))
                            if reason.contains("only reads") =>
                        {
                            failure(code::READONLY, "a reader connection cannot write").into()
                        }
                        error => error.into(),
                    }
                };
                if let Some(reading) = &mut self.reading {
                    if reading.snapshot.is_none() {
                        reading.snapshot = Some(opened.sql.snapshot(limits).await?);
                    }
                    let snapshot = reading.snapshot.as_mut().ok_or("no snapshot")?;
                    let rows = opened
                        .sql
                        .snapshot_rows(snapshot, sql, params)
                        .await
                        .map_err(readonly)?;
                    let columns = rows.columns.into_iter().map(|column| column.name).collect();
                    return Ok(Pending::rows(columns, rows.rows));
                }
                let cursor = match opened.sql.cursor(sql, params.clone(), limits).await {
                    // The session lost its authority: a new one may read.
                    Err(Error::Sql(SqlError::Unavailable {
                        reason: Unavailable::Revoked,
                        ..
                    })) => {
                        self.opened = None;
                        self.sql().await?.cursor(sql, params, limits).await
                    }
                    other => other,
                }
                .map_err(readonly)?;
                Ok(Pending::Cursor(cursor))
            }
        }
    }
}

/// The connections of one database, served on its reactor.
#[derive(Default)]
pub(crate) struct Registry {
    connections: StdMutex<BTreeMap<u64, Arc<Mutex<State>>>>,
    next: std::sync::atomic::AtomicU64,
}
impl Registry {
    pub(crate) async fn connect(
        &self,
        node: Arc<Node>,
        options: ConnectionOptions,
    ) -> Result<u64, Failure> {
        let state = State::new(node, options).await?;
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        self.connections
            .lock()
            .map_err(|_| "connections poisoned")?
            .insert(id, Arc::new(Mutex::new(state)));
        Ok(id)
    }
    pub(crate) fn get(&self, id: u64) -> Result<Arc<Mutex<State>>, Failure> {
        self.connections
            .lock()
            .map_err(|_| "connections poisoned")?
            .get(&id)
            .cloned()
            .ok_or_else(|| failure(code::MISUSE, "the connection is closed").into())
    }
    /// Closes `id`, or every connection: an open transaction rolls back
    /// and cursors end.
    pub(crate) fn close(&self, id: Option<u64>) {
        if let Ok(mut connections) = self.connections.lock() {
            match id {
                Some(id) => drop(connections.remove(&id)),
                None => connections.clear(),
            }
        }
    }
}

/// A call on one connection, served on the reactor.
pub(crate) enum Call {
    Execute {
        statement: u64,
        sql: String,
        params: Vec<SqlValue>,
    },
    Next {
        statement: u64,
    },
    Finish {
        statement: u64,
    },
    InTransaction,
}
pub(crate) enum Reply {
    Page(Page),
    Flag(bool),
    Done,
}
pub(crate) async fn serve(state: Arc<Mutex<State>>, call: Call) -> Result<Reply, SqlFailure> {
    let mut state = state.lock().await;
    let reply = match call {
        Call::Execute {
            statement,
            sql,
            params,
        } => state
            .execute(statement, &sql, params)
            .await
            .map(Reply::Page),
        Call::Next { statement } => state.next(statement).await.map(Reply::Page),
        Call::Finish { statement } => {
            state.finish(statement);
            Ok(Reply::Done)
        }
        Call::InTransaction => Ok(Reply::Flag(state.in_transaction())),
    };
    reply.map_err(sql_failure)
}

/// Committed changes of a database's tables, for a driver to invalidate
/// what it read. See `docs/runtime/queries-and-observation.md#table-events`.
pub struct TableEvents {
    observer: Mutex<TableObserver>,
}
/// Tables whose committed rows may differ, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableChange {
    /// Lowercase table names.
    pub tables: Vec<String>,
    /// `local`, `sync`, `confirmed`, `rebuild` or `authority`.
    pub causes: Vec<String>,
}
impl TableEvents {
    pub(crate) fn new(observer: TableObserver) -> Self {
        Self {
            observer: Mutex::new(observer),
        }
    }
    /// Waits for the next change; after the database closed, an error.
    pub async fn next(&self) -> Result<TableChange, Failure> {
        let change = self.observer.lock().await.next().await?;
        Ok(TableChange {
            tables: change.tables.into_iter().collect(),
            causes: change
                .causes
                .into_iter()
                .map(|cause| format!("{cause:?}").to_ascii_lowercase())
                .collect(),
        })
    }
}

#[cfg(test)]
mod next_id_tests {
    use super::*;

    #[test]
    fn unsupported_next_id_shapes_are_refused_before_sql_execution() {
        for sql in [
            "SELECT rocky_next_id('t') AS x",
            "SELECT \"rocky_next_id\"('t') AS x",
            "SELECT rocky_next_id('t') + 1",
            "SELECT 1, rocky_next_id('t')",
            "SELECT rocky_next_id('t', 1), rocky_next_id('t', 2)",
        ] {
            let error = next_id(sql, &[]).unwrap_err();
            assert!(
                error.to_string().contains("standalone SELECT"),
                "{sql}: {error}"
            );
        }
        let error = next_id("SELECT rocky_next_id('t', -1)", &[]).unwrap_err();
        assert!(error.to_string().contains("negative literals"), "{error}");
        let error = next_id("SELECT rocky_next_id('t',)", &[]).unwrap_err();
        assert!(
            error.to_string().contains("argument after each comma"),
            "{error}"
        );
        assert_eq!(
            next_id("SELECT rocky_next_id('t', ?)", &[SqlValue::Integer(-1)]).unwrap(),
            Some(("t".into(), vec![SqlValue::Integer(-1)]))
        );
        assert!(next_id("SELECT 'rocky_next_id(t)'", &[]).unwrap().is_none());
    }
}
