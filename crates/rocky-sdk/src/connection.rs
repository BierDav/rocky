use crate::{Failure, node::Node};
use rocky_core::{SqlValue, TypedRows};
use rocky_secure::{AbortReason, Error, SecureSql, SqlError, Unavailable};
use rocky_storage::partition_sql::IntentRefusal;
use rocky_storage::{
    cursor::{CursorLimits, SnapshotCursor},
    observe::TableObserver,
    sql::{Control, classify},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::Mutex;

const PAGE_ROWS: usize = 256;
const PAGE_BYTES: usize = 1024 * 1024;
const MAX_RESULTS: usize = 64;
const MAX_EXPLAINED_WRITES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionOptions {
    pub(crate) read_only: bool,
    pub(crate) transaction_timeout: Option<std::time::Duration>,
}
impl ConnectionOptions {
    fn sql(&self) -> rocky_secure::SqlOptions {
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Page {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<SqlValue>>,
    pub(crate) done: bool,
}

pub(crate) mod code {
    pub const ERROR: i32 = 1;
    pub const ABORT: i32 = 4;
    pub const READONLY: i32 = 8;
    pub const MISUSE: i32 = 21;
    pub const AUTH: i32 = 23;
}
pub(crate) fn failure(code: i32, message: impl Into<String>) -> crate::Error {
    crate::Error::sql(code, message)
}
async fn explained(
    node: &Node,
    sql: &str,
    next_id: Option<&str>,
    writes: &BTreeSet<String>,
    error: Failure,
) -> Failure {
    let typed = crate::Error::from_failure(&error);
    let mut explained = match next_id {
        Some(table) => node.allocation_refusal(table, &typed).await,
        None => node.statement_refusal(sql).await,
    };
    for write in writes {
        if explained.is_some() {
            break;
        }
        explained = node.statement_refusal(write).await;
    }
    Box::new(match explained {
        Some(explained) => explained,
        None => node.partition_refusal(typed).await,
    })
}
pub(crate) fn sql_error(error: &SqlError, detail: String) -> crate::Error {
    use crate::ErrorKind as Kind;
    let sql = |code: i32, unsupported: Option<crate::Unsupported>| {
        let message = error.to_string();
        crate::Error::new(
            Kind::Sql {
                code,
                message: message.clone(),
                unsupported,
            },
            message,
        )
    };
    match error {
        SqlError::Statement { code, .. } => sql(*code, None),
        SqlError::Denied => sql(code::AUTH, Some(crate::Unsupported::Statement)),
        SqlError::Unsupported(_) => sql(code::ERROR, Some(crate::Unsupported::Statement)),
        SqlError::Refused(IntentRefusal::Unavailable { table, reason }) => {
            unavailable(table, reason, detail)
        }
        SqlError::Refused(refusal) => {
            let unsupported = match refusal {
                IntentRefusal::Clock(name)
                | IntentRefusal::Random(name)
                | IntentRefusal::Environment(name)
                | IntentRefusal::UnknownFunction(name) => {
                    crate::Unsupported::NonDeterministic(name.clone())
                }
                IntentRefusal::With => crate::Unsupported::With,
                _ => crate::Unsupported::Statement,
            };
            let hint = match &unsupported {
                crate::Unsupported::NonDeterministic(_) => {
                    "; compute the value in the application and bind it as a parameter"
                }
                crate::Unsupported::With => "; write the subquery inline instead",
                crate::Unsupported::Statement => "",
            };
            let message = format!("{error}{hint}");
            crate::Error::new(
                Kind::Sql {
                    code: code::ERROR,
                    message: message.clone(),
                    unsupported: Some(unsupported),
                },
                message,
            )
        }
        SqlError::Unavailable { table, reason } => unavailable(table, reason, detail),
        SqlError::Aborted(AbortReason::Timeout | AbortReason::Preempted) => {
            crate::Error::new(Kind::Busy, detail)
        }
        SqlError::Aborted(_) => sql(code::ABORT, None),
        SqlError::Capture(_) => sql(code::ERROR, None),
        SqlError::Stale => crate::Error::new(Kind::AuthorityChanged, detail),
    }
}
fn unavailable(table: &str, reason: &Unavailable, detail: String) -> crate::Error {
    use crate::ErrorKind as Kind;
    match reason {
        Unavailable::Incomplete { blockers } => crate::Error::new(
            Kind::Blocked {
                table: Some(table.to_owned()),
                blockers: blockers.clone(),
            },
            detail,
        ),
        Unavailable::KeyMissing => crate::Error::new(
            Kind::Blocked {
                table: Some(table.to_owned()),
                blockers: Vec::new(),
            },
            detail,
        ),
        Unavailable::NotHeld => crate::Error::denied(
            Some(table.to_owned()),
            crate::Right::Read,
            crate::DenyReason::NotGranted,
        ),
        Unavailable::Revoked => crate::Error::new(Kind::AuthorityChanged, detail),
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
    let words = words(sql);
    let function =
        |word: &str| !word.starts_with('\'') && unquote(word).eq_ignore_ascii_case(NEXT_ID);
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
            SqlValue::Text(unquote(word))
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

fn unquote(word: &str) -> String {
    let quote = word.chars().next().unwrap_or(' ');
    let close = if quote == '[' { ']' } else { quote };
    if ['`', '"', '\'', '['].contains(&quote) && word.len() >= 2 && word.ends_with(close) {
        let inner = &word[1..word.len() - 1];
        inner.replace(&format!("{close}{close}"), &close.to_string())
    } else {
        word.to_string()
    }
}

fn words(sql: &str) -> Vec<String> {
    let mut chars = sql.chars().peekable();
    let mut words = Vec::new();
    while let Some(first) = chars.next() {
        if first.is_whitespace() {
            continue;
        }
        let mut word = String::from(first);
        if ['\'', '"', '`', '['].contains(&first) {
            let end = if first == '[' { ']' } else { first };
            while let Some(next) = chars.next() {
                word.push(next);
                if next == end {
                    if chars.peek() == Some(&end) {
                        word.push(chars.next().unwrap_or(end));
                    } else {
                        break;
                    }
                }
            }
        } else if first.is_ascii_alphanumeric() || first == '_' {
            while let Some(next) = chars.peek().copied() {
                if !(next.is_ascii_alphanumeric() || next == '_') {
                    break;
                }
                word.push(next);
                chars.next();
            }
        }
        words.push(word);
    }
    if words.last().is_some_and(|word| word == ";") {
        words.pop();
    }
    words
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
    committed: Option<crate::TransactionToken>,
    handle: Option<rocky_core::Commitment>,
    writes: BTreeSet<String>,
}
impl State {
    async fn new(node: Arc<Node>, options: ConnectionOptions) -> Result<Self, Failure> {
        let mut state = Self {
            read_only: options.read_only,
            options,
            node,
            opened: None,
            begin: None,
            open: false,
            reading: None,
            results: BTreeMap::new(),
            committed: None,
            handle: None,
            writes: BTreeSet::new(),
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
            self.opened = Some(self.node.sql_session(self.options.sql()).await?);
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
        let normalized = rocky_storage::parsed::normalized(sql);
        let sql = normalized.as_ref();
        self.results.remove(&statement);
        if self.results.len() >= MAX_RESULTS {
            return Err(failure(code::MISUSE, "too many statements with open results").into());
        }
        let allocation = next_id(sql, &params)?;
        let allocated = allocation.as_ref().map(|(table, _)| table.as_str());
        let dispatched = match (query_only(sql), &allocation) {
            (Some(set), _) => self.query_only(set),
            (None, Some((table, values))) => self.next_id(table, values).await,
            (None, None) if self.read_only => self.read(sql, params).await,
            (None, None) => self.write(sql, params).await,
        };
        let mut pending = match dispatched {
            Ok(pending) => pending,
            Err(error) => {
                return Err(explained(&self.node, sql, allocated, &self.writes, error).await);
            }
        };
        let page = match pending.page().await {
            Ok(page) => page,
            Err(error) => {
                return Err(explained(&self.node, sql, allocated, &self.writes, error).await);
            }
        };
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
    pub(crate) fn committed(&self) -> Option<crate::TransactionToken> {
        self.committed
    }
    pub(crate) async fn begin_identified(&mut self) -> Result<crate::TransactionToken, Failure> {
        if self.read_only {
            return Err(failure(code::READONLY, "a reader connection cannot write").into());
        }
        self.write("BEGIN", Vec::new()).await?;
        let handle: rocky_core::Commitment = rocky_platform::entropy::random();
        let token = crate::TransactionToken::new(self.node.replica.transaction_of(handle));
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
            QueryOnly::Set(read_only) => {
                self.read_only = read_only;
                Ok(Pending::rows(Vec::new(), Vec::new()))
            }
        }
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
        let control = classify(sql).map_err(Error::Sql)?;
        match control {
            Control::Begin(mode) => {
                if self.active() {
                    return Err(failure(
                        code::ERROR,
                        "cannot start a transaction within a transaction",
                    )
                    .into());
                }
                self.begin = Some(mode);
                self.writes.clear();
                return Ok(Pending::rows(Vec::new(), Vec::new()));
            }
            Control::Commit | Control::Rollback if self.begin.is_some() => {
                self.begin = None;
                self.handle = None;
                return Ok(Pending::rows(Vec::new(), Vec::new()));
            }
            _ => {}
        }
        self.begun().await?;
        let ending = matches!(control, Control::Commit | Control::Rollback);
        if matches!(control, Control::Savepoint(_)) {
            self.open = true;
        }
        let handle = if ending { self.handle.take() } else { None };
        let ran = match (&control, handle) {
            (Control::Commit, Some(handle)) => self.sql().await?.commit(handle).await,
            _ => self.sql().await?.run(sql, params).await,
        };
        if matches!(control, Control::Commit) {
            self.committed = None;
            if let Ok(rows) = &ran
                && let Some(commit) = rows
                    .committed
                    .as_ref()
                    .filter(|commit| !commit.frames.is_empty())
            {
                let transaction = self.node.replica.transaction_of(commit.handle);
                self.committed = Some(crate::TransactionToken::new(transaction));
            }
        }
        let session_may_have_ended =
            ending || ran.is_err() || matches!(control, Control::Release(_));
        if session_may_have_ended {
            let aborted = matches!(&ran, Err(Error::Sql(SqlError::Aborted(_))));
            let active = self.sql().await?.state().await == rocky_secure::SqlState::Active;
            let abort_awaits_acknowledgement = aborted && !ending;
            self.open = active || abort_awaits_acknowledgement;
        }
        let rows = ran?;
        if ending {
            self.writes.clear();
        } else if control == Control::Statement && self.writes.len() < MAX_EXPLAINED_WRITES {
            self.writes.insert(sql.to_owned());
        }
        Ok(Pending::rows(rows.columns, rows.rows))
    }

    async fn read(&mut self, sql: &str, params: Vec<SqlValue>) -> Result<Pending, Failure> {
        let control = classify(sql).map_err(Error::Sql)?;
        let none = || Ok(Pending::rows(Vec::new(), Vec::new()));
        let no_transaction = || -> Result<Pending, Failure> {
            Err(failure(code::ERROR, "no transaction is active").into())
        };
        let release = matches!(control, Control::Release(_));
        match control {
            Control::Begin("DEFERRED") if self.reading.is_none() => {
                self.reading = Some(Reading::default());
                none()
            }
            Control::Begin("DEFERRED") => Err(failure(
                code::ERROR,
                "cannot start a transaction within a transaction",
            )
            .into()),
            Control::Begin(_) => {
                Err(failure(code::READONLY, "a reader connection cannot write").into())
            }
            Control::Commit | Control::Rollback => match self.reading.take() {
                Some(_) => none(),
                None => no_transaction(),
            },
            Control::Savepoint(name) => {
                let reading = self.reading.get_or_insert_with(|| Reading {
                    implicit: true,
                    ..Reading::default()
                });
                reading.savepoints.push(name);
                none()
            }
            Control::Release(name) | Control::RollbackTo(name) => {
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
            Control::Statement => {
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
                if self.reading.is_some() {
                    let needs_snapshot = self
                        .reading
                        .as_ref()
                        .is_some_and(|reading| reading.snapshot.is_none());
                    if needs_snapshot {
                        let snapshot = match opened.snapshot(limits).await {
                            Err(Error::Sql(SqlError::Unavailable {
                                reason: Unavailable::Revoked,
                                ..
                            })) => {
                                self.opened = None;
                                self.sql().await?.snapshot(limits).await
                            }
                            other => other,
                        }?;
                        if let Some(reading) = &mut self.reading {
                            reading.snapshot = Some(snapshot);
                        }
                    }
                    let opened = self.opened.as_ref().ok_or("connection has no session")?;
                    let snapshot = self
                        .reading
                        .as_mut()
                        .and_then(|reading| reading.snapshot.as_mut())
                        .ok_or("no snapshot")?;
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

const MAX_IDLE_CONNECTIONS: usize = 4;

type IdlePool = BTreeMap<bool, Vec<Arc<Mutex<State>>>>;

impl State {
    fn recycle(&mut self) -> bool {
        if self.active() || !self.results.is_empty() {
            return false;
        }
        if let Some(opened) = &self.opened
            && !opened.reset_for_reuse()
        {
            return false;
        }
        self.committed = None;
        self.handle = None;
        self.writes.clear();
        self.read_only = self.options.read_only;
        true
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    connections: StdMutex<BTreeMap<u64, Arc<Mutex<State>>>>,
    idle: StdMutex<IdlePool>,
    closed: std::sync::atomic::AtomicBool,
    next: std::sync::atomic::AtomicU64,
}
impl Registry {
    fn pool_key(options: &ConnectionOptions) -> Option<bool> {
        options
            .transaction_timeout
            .is_none()
            .then_some(options.read_only)
    }
    fn return_idle(&self, key: bool, entry: Arc<Mutex<State>>) {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        if let Ok(mut idle) = self.idle.lock() {
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let bucket = idle.entry(key).or_default();
            if bucket.len() < MAX_IDLE_CONNECTIONS {
                bucket.push(entry);
            }
        }
    }
    fn insert_connection(&self, id: u64, entry: Arc<Mutex<State>>) -> Result<(), Failure> {
        let mut connections = self
            .connections
            .lock()
            .map_err(|_| "connections poisoned")?;
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(crate::Error::closed("the connection is closed").into());
        }
        connections.insert(id, entry);
        Ok(())
    }
    pub(crate) async fn connect(
        &self,
        node: Arc<Node>,
        options: ConnectionOptions,
    ) -> Result<u64, Failure> {
        if let Some(key) = Self::pool_key(&options)
            && let Some(reused) = self
                .idle
                .lock()
                .ok()
                .and_then(|mut idle| idle.get_mut(&key).and_then(Vec::pop))
        {
            node.admit_sql().await?;
            let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            self.insert_connection(id, reused)?;
            return Ok(id);
        }
        #[cfg(feature = "qualification")]
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let creating = State::new(node, options);
        #[cfg(feature = "qualification")]
        let creating = rocky_secure::with_measurement_connection(id, creating);
        let state = creating.await?;
        #[cfg(not(feature = "qualification"))]
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        self.insert_connection(id, Arc::new(Mutex::new(state)))?;
        Ok(id)
    }
    pub(crate) async fn query_once(
        &self,
        node: Arc<Node>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> Result<(Page, Option<u64>), Failure> {
        let key = true;
        let entry = match self
            .idle
            .lock()
            .ok()
            .and_then(|mut idle| idle.get_mut(&key).and_then(Vec::pop))
        {
            Some(reused) => {
                node.admit_sql().await?;
                reused
            }
            None => {
                let options = ConnectionOptions {
                    read_only: true,
                    transaction_timeout: None,
                };
                Arc::new(Mutex::new(State::new(node, options).await?))
            }
        };
        let (page, return_to_pool) = {
            let mut state = entry.lock().await;
            let result = state.execute(1, &sql, params).await;
            match result {
                Ok(page) if page.done && state.recycle() => (Ok(page), true),
                Ok(page) => (Ok(page), false),
                Err(error) => {
                    let reusable = state.recycle();
                    (Err(error), reusable)
                }
            }
        };
        if return_to_pool {
            self.return_idle(key, entry);
            return Ok((page?, None));
        }
        let page = page?;
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        self.insert_connection(id, entry)?;
        Ok((page, Some(id)))
    }
    pub(crate) fn get(&self, id: u64) -> Result<Arc<Mutex<State>>, Failure> {
        self.connections
            .lock()
            .map_err(|_| "connections poisoned")?
            .get(&id)
            .cloned()
            .ok_or_else(|| crate::Error::closed("the connection is closed").into())
    }
    pub(crate) fn close(&self, id: Option<u64>) {
        let removed = if let Ok(mut connections) = self.connections.lock() {
            match id {
                Some(id) => connections.remove(&id),
                None => {
                    self.closed
                        .store(true, std::sync::atomic::Ordering::Release);
                    connections.clear();
                    if let Ok(mut idle) = self.idle.lock() {
                        idle.clear();
                    }
                    None
                }
            }
        } else {
            None
        };
        if let Some(entry) = removed
            && Arc::strong_count(&entry) == 1
        {
            let key = {
                if let Ok(mut state) = entry.try_lock()
                    && let Some(key) = Self::pool_key(&state.options)
                    && state.recycle()
                {
                    Some(key)
                } else {
                    None
                }
            };
            if let Some(key) = key {
                self.return_idle(key, entry);
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
    #[cfg(test)]
    InTransaction,
    Committed,
    Begin,
}
pub(crate) enum Reply {
    Page(Page),
    #[cfg(test)]
    Flag(bool),
    Token(Option<crate::TransactionToken>),
    Done,
}
pub(crate) async fn serve(state: Arc<Mutex<State>>, call: Call) -> Result<Reply, crate::Error> {
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
        #[cfg(test)]
        Call::InTransaction => Ok(Reply::Flag(state.active())),
        Call::Committed => Ok(Reply::Token(state.committed())),
        Call::Begin => state
            .begin_identified()
            .await
            .map(|token| Reply::Token(Some(token))),
    };
    reply.map_err(crate::Error::failed)
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
    pub dropped: Vec<crate::TransactionToken>,
    pub sealed: Vec<crate::TransactionToken>,
    pub reconcile: bool,
}
impl TableEvents {
    pub(crate) fn new(observer: TableObserver) -> Self {
        Self {
            observer: Mutex::new(observer),
        }
    }
    pub async fn next(&self) -> Result<TableChange, crate::Error> {
        let change = self
            .observer
            .lock()
            .await
            .next()
            .await
            .map_err(|error| crate::Error::failed(error.into()))?;
        Ok(TableChange {
            tables: change.tables.into_iter().collect(),
            causes: change.causes.into_iter().map(ChangeCause::from).collect(),
            dropped: change
                .dropped
                .into_iter()
                .map(crate::TransactionToken::new)
                .collect(),
            sealed: change
                .sealed
                .into_iter()
                .map(crate::TransactionToken::new)
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
        assert_eq!(
            next_id("SELECT [rocky_next_id]('it''s', `x`);", &[])
                .unwrap_err()
                .to_string(),
            "rocky_next_id requires scalar arguments"
        );
        assert_eq!(
            next_id("SELECT [rocky_next_id]('it''s', 7);", &[]).unwrap(),
            Some(("it's".into(), vec![SqlValue::Integer(7)]))
        );
    }
}
