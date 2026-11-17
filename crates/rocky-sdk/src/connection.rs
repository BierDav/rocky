use crate::{Failure, node::Node};
use rocky_core::{SqlValue, TypedRows};
use rocky_runtime::{CursorLimits, SnapshotCursor, SqlControl, TableObserver, classify_sql};
use rocky_secure::{Error, SecureSql, SqlError, Unavailable};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::Mutex;

mod room;

const PAGE_ROWS: usize = 256;
const PAGE_BYTES: usize = 1024 * 1024;
const MAX_RESULTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Seal(rocky_core::Commitment),
    Bookmark(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    Buffered,
    Flushed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionOptions {
    pub read_only: bool,
    pub durability: Durability,
    pub profile: Option<Profile>,
    pub transaction_timeout: Option<std::time::Duration>,
    pub binding: Option<Binding>,
}
impl ConnectionOptions {
    fn sql(&self) -> rocky_secure::SqlOptions {
        let mut options = rocky_secure::SqlOptions {
            acknowledge: match self.durability {
                Durability::Buffered => rocky_secure::CommitClass::Buffered,
                Durability::Flushed => rocky_secure::CommitClass::Flushed,
            },
            ..Default::default()
        };
        if let Some(timeout) = self.transaction_timeout {
            options.transaction_timeout = timeout;
        }
        options
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlFailure {
    pub code: i32,
    pub message: String,
    pub retryable: bool,
}
impl std::fmt::Display for SqlFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Error code: {}, message: {}", self.code, self.message)
    }
}
impl std::error::Error for SqlFailure {}

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
        retryable: false,
    }
}
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
    let retryable = matches!(
        error,
        Error::Blocked(_)
            | Error::Backpressure
            | Error::Sql(SqlError::Unavailable {
                reason: Unavailable::NotHeld
                    | Unavailable::KeyMissing
                    | Unavailable::Incomplete
                    | Unavailable::Revoked,
                ..
            })
            | Error::Sql(SqlError::Aborted(
                rocky_secure::AbortReason::Timeout | rocky_secure::AbortReason::Preempted
            ))
    );
    SqlFailure {
        code,
        message,
        retryable,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueryOnly {
    Read,
    Set(bool),
}

fn query_only(sql: &str) -> Option<QueryOnly> {
    let spaced = sql.trim().trim_end_matches(';').replace('=', " = ");
    let words: Vec<String> = spaced
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    match words.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["pragma", "query_only"] => Some(QueryOnly::Read),
        ["pragma", "query_only", "=", value] => match value {
            "1" | "on" | "true" | "yes" => Some(QueryOnly::Set(true)),
            "0" | "off" | "false" | "no" => Some(QueryOnly::Set(false)),
            _ => None,
        },
        _ => None,
    }
}

const NEXT_ID: &str = "rocky_next_id";

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

#[derive(Default)]
struct Reading {
    snapshot: Option<SnapshotCursor>,
    savepoints: Vec<String>,
    implicit: bool,
}

pub(crate) struct State {
    options: ConnectionOptions,
    read_only: bool,
    node: Arc<Node>,
    opened: Option<SecureSql<TypedRows>>,
    begin: Option<&'static str>,
    open: bool,
    reading: Option<Reading>,
    results: BTreeMap<u64, Pending>,
    room: room::Housekeeping,
    version: u32,
    declared: Vec<rocky_core::LogicalTable>,
    seal: Option<rocky_core::Commitment>,
    committed: Option<rocky_core::TransactionToken>,
    handle: Option<rocky_core::Commitment>,
}
impl State {
    async fn new(node: Arc<Node>, options: ConnectionOptions) -> Result<Self, Failure> {
        let room = match options.profile {
            Some(Profile::Room284) => {
                room::Housekeeping::new(Some(node.replica.observe_tables(None).await?))
            }
            None => room::Housekeeping::new(None),
        };
        let version = node.declared_version();
        let declared = node.declared_differences()?;
        let seal = match &options.binding {
            None => None,
            Some(Binding::Seal(seal)) => Some(*seal),
            Some(Binding::Bookmark(name)) => Some(
                node.replica
                    .bookmark_seal(node.domain().scope, name)
                    .await?,
            ),
        };
        let bound_to_a_seal = seal.is_some();
        let mut state = Self {
            read_only: options.read_only || bound_to_a_seal,
            options,
            node,
            opened: None,
            begin: None,
            open: false,
            reading: None,
            results: BTreeMap::new(),
            room,
            version,
            declared,
            seal,
            committed: None,
            handle: None,
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
    async fn sql(&mut self) -> Result<&SecureSql<TypedRows>, Failure> {
        let current = match &self.opened {
            Some(opened) => self.authority_pinned() || opened.status().await.is_ok(),
            None => false,
        };
        if !current {
            let mut sql = self.node.sql_session(self.options.sql()).await?;
            if let Some(seal) = self.seal {
                sql.bind_seal(seal).await?;
            }
            self.opened = Some(sql);
        }
        self.opened
            .as_ref()
            .ok_or_else(|| "connection has no session".into())
    }

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
    pub(crate) fn finish(&mut self, statement: u64) {
        self.results.remove(&statement);
    }
    pub(crate) fn in_transaction(&self) -> bool {
        self.active()
    }
    pub(crate) fn committed(&self) -> Option<rocky_core::TransactionToken> {
        self.committed
    }
    pub(crate) async fn begin_identified(
        &mut self,
    ) -> Result<rocky_core::TransactionToken, Failure> {
        if self.read_only {
            return Err(failure(code::READONLY, "a reader connection cannot write").into());
        }
        self.write("BEGIN", Vec::new()).await?;
        let handle: rocky_core::Commitment = rocky_platform::entropy::random();
        let token = rocky_core::TransactionToken(self.node.replica.transaction_of(handle));
        self.handle = Some(handle);
        Ok(token)
    }

    fn query_only(&mut self, pragma: QueryOnly) -> Result<Pending, Failure> {
        match pragma {
            QueryOnly::Read => Ok(Pending::rows(
                vec!["query_only".into()],
                vec![vec![SqlValue::Integer(i64::from(self.read_only))]],
            )),
            QueryOnly::Set(_) if self.active() => {
                Err(failure(code::ERROR, "query_only cannot change inside a transaction").into())
            }
            QueryOnly::Set(false) if self.seal.is_some() => {
                Err(failure(code::READONLY, "a connection bound to a seal is read-only").into())
            }
            QueryOnly::Set(read_only) => {
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
            && answer.as_ref().err().is_some_and(|error| {
                matches!(
                    error.downcast_ref::<Error>(),
                    Some(Error::Sql(SqlError::Unavailable {
                        reason: Unavailable::Revoked,
                        ..
                    }))
                )
            })
        {
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
        let read_only = self.read_only && self.seal.is_none();
        let timeout = self.options.sql().transaction_timeout.as_millis();
        if !self.authority_pinned() {
            self.declared = self.node.declared_differences()?;
            self.sql().await?;
        }
        let Self {
            opened,
            room,
            version,
            declared,
            ..
        } = self;
        let version = *version;
        let sql_session = opened.as_ref().ok_or("connection has no session")?;
        room.answer(
            sql,
            params,
            room::Context {
                sql: sql_session,
                version,
                declared,
                read_only,
                busy_timeout: i64::try_from(timeout).unwrap_or(i64::MAX),
            },
        )
        .await
    }

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
        Ok(Pending::rows(vec![NEXT_ID.into()], vec![vec![id.sql()]]))
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
            SqlControl::Commit | SqlControl::Rollback if self.begin.is_some() => {
                self.begin = None;
                self.handle = None;
                return Ok(Pending::rows(Vec::new(), Vec::new()));
            }
            _ => {}
        }
        self.begun().await?;
        let ending = matches!(control, SqlControl::Commit | SqlControl::Rollback);
        if matches!(control, SqlControl::Savepoint(_)) {
            self.open = true;
        }
        let handle = if ending { self.handle.take() } else { None };
        let ran = match (&control, handle) {
            (SqlControl::Commit, Some(handle)) => self.sql().await?.commit(handle).await,
            _ => self.sql().await?.run(sql, params).await,
        };
        if matches!(control, SqlControl::Commit) {
            self.committed = None;
            if let Ok(rows) = &ran
                && let Some(commit) = rows
                    .committed
                    .as_ref()
                    .filter(|commit| !commit.frames.is_empty())
            {
                let transaction = self.node.replica.transaction_of(commit.handle);
                self.committed = Some(rocky_core::TransactionToken(transaction));
            }
        }
        let session_may_have_ended =
            ending || ran.is_err() || matches!(control, SqlControl::Release(_));
        if session_may_have_ended {
            let aborted = matches!(&ran, Err(Error::Sql(SqlError::Aborted(_))));
            let active = self.sql().await?.state().await == rocky_secure::SqlState::Active;
            let abort_awaits_acknowledgement = aborted && !ending;
            self.open = active || abort_awaits_acknowledgement;
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
            SqlControl::Begin(_) if self.seal.is_some() && self.reading.is_none() => {
                self.reading = Some(Reading::default());
                none()
            }
            SqlControl::Begin(_) if self.seal.is_some() => Err(failure(
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
                        reading.snapshot = Some(opened.snapshot(limits).await?);
                    }
                    let snapshot = reading.snapshot.as_mut().ok_or("no snapshot")?;
                    let rows = opened
                        .snapshot_rows(snapshot, sql, params)
                        .await
                        .map_err(readonly)?;
                    let columns = rows.columns.into_iter().map(|column| column.name).collect();
                    return Ok(Pending::rows(columns, rows.rows));
                }
                let cursor = match opened.cursor(sql, params.clone(), limits).await {
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
        #[cfg(feature = "qualification")]
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let creating = State::new(node, options);
        #[cfg(feature = "qualification")]
        let creating = rocky_secure::with_measurement_connection(id, creating);
        let state = creating.await?;
        #[cfg(not(feature = "qualification"))]
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
    pub(crate) fn close(&self, id: Option<u64>) {
        if let Ok(mut connections) = self.connections.lock() {
            match id {
                Some(id) => drop(connections.remove(&id)),
                None => connections.clear(),
            }
        }
    }
}

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
    Committed,
    Begin,
}
pub(crate) enum Reply {
    Page(Page),
    Flag(bool),
    Token(Option<rocky_core::TransactionToken>),
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
        Call::Committed => Ok(Reply::Token(state.committed())),
        Call::Begin => state
            .begin_identified()
            .await
            .map(|token| Reply::Token(Some(token))),
    };
    reply.map_err(sql_failure)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ChangeCause {
    Local,
    Sync,
    Rebuild,
    Authority,
}
impl From<rocky_storage::observe::Cause> for ChangeCause {
    fn from(cause: rocky_storage::observe::Cause) -> Self {
        match cause {
            rocky_storage::observe::Cause::Local => Self::Local,
            rocky_storage::observe::Cause::Sync => Self::Sync,
            rocky_storage::observe::Cause::Rebuild => Self::Rebuild,
            rocky_storage::observe::Cause::Authority => Self::Authority,
        }
    }
}

pub struct TableEvents {
    observer: Mutex<TableObserver>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableChange {
    pub tables: Vec<String>,
    pub causes: Vec<ChangeCause>,
    pub dropped: Vec<rocky_core::TransactionToken>,
    pub sealed: Vec<rocky_core::TransactionToken>,
    pub reconcile: bool,
}
impl TableEvents {
    pub(crate) fn new(observer: TableObserver) -> Self {
        Self {
            observer: Mutex::new(observer),
        }
    }
    pub async fn next(&self) -> Result<TableChange, Failure> {
        let change = self.observer.lock().await.next().await?;
        Ok(TableChange {
            tables: change.tables.into_iter().collect(),
            causes: change.causes.into_iter().map(ChangeCause::from).collect(),
            dropped: change
                .dropped
                .into_iter()
                .map(rocky_core::TransactionToken)
                .collect(),
            sealed: change
                .sealed
                .into_iter()
                .map(rocky_core::TransactionToken)
                .collect(),
            reconcile: change.reconcile,
        })
    }
}

#[cfg(test)]
mod next_id_tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
