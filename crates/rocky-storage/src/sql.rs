//! Session types and transaction-control parsing shared by SQL sessions.
use crate::{CommitClass, Error, scoped_connection};
use std::time::Duration;

/// Longest transaction a session may ask for. While it runs, no other write
/// of the file commits.
pub const MAX_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(60);
/// Nested savepoints of one transaction.
pub const MAX_SAVEPOINTS: usize = 64;

/// Bounds of one session. They are compatibility limits: a transaction that
/// exceeds one is rolled back and reports it; nothing is truncated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SqlOptions {
    /// From `BEGIN` to the end of the transaction, waiting included.
    pub transaction_timeout: Duration,
    /// Longest run of one statement.
    pub statement_budget: Duration,
    /// Row changes and bytes of row images one transaction may capture.
    pub captured_rows: usize,
    pub captured_bytes: usize,
    /// Bytes one statement may return.
    pub result_bytes: usize,
    /// What a commit has reached before it is acknowledged.
    pub acknowledge: CommitClass,
}
impl Default for SqlOptions {
    fn default() -> Self {
        Self {
            transaction_timeout: Duration::from_secs(5),
            statement_budget: Duration::from_secs(2),
            captured_rows: 65_536,
            captured_bytes: 32 * crate::MAX_FRAME_BYTES,
            result_bytes: 16 * crate::MAX_FRAME_BYTES,
            acknowledge: CommitClass::Flushed,
        }
    }
}
impl SqlOptions {
    pub fn validate(&self) -> Result<(), Error> {
        if self.transaction_timeout.is_zero()
            || self.transaction_timeout > MAX_TRANSACTION_TIMEOUT
            || self.statement_budget.is_zero()
            || self.captured_rows == 0
            || self.captured_bytes == 0
            || self.result_bytes == 0
        {
            return Err(Error::Incompatible("SQL session options".into()));
        }
        Ok(())
    }
}

/// Why a table's rows cannot be read locally. An authorized empty table is
/// never unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The table's domain is not open, or its scope was never held,
    /// is unsubscribed or is evicted.
    NotHeld,
    /// The reader key of its scope is not installed.
    KeyMissing,
    /// Admitted operations of its scope are not applied yet, or the
    /// domain is being rebuilt.
    Incomplete,
    /// The session's authority changed; open a new session.
    Revoked,
}
/// Why the runtime rolled a transaction back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbortReason {
    /// The transaction outlived `SqlOptions::transaction_timeout`.
    Timeout,
    /// `SqlSession::cancel` was called.
    Cancelled,
    /// A statement was refused or a changed row could not be captured.
    Poisoned(String),
    /// The engine ended the transaction, as `OR ROLLBACK` or an interrupt does.
    Engine(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlError {
    /// The engine refused the statement with this extended result code. An
    /// open transaction continues, as the statement's conflict clause decides.
    Statement { code: i32, message: String },
    /// The statement reaches beyond the tables a session may read or write.
    Denied,
    /// The statement is outside the supported SQL surface. It had no effect
    /// and an open transaction continues.
    Unsupported(String),
    /// A table the statement names is not available locally: its scope is
    /// not held, its key is missing or its rows are incomplete. Nothing ran.
    Unavailable { table: String, reason: Unavailable },
    /// The transaction was rolled back. Until the session acknowledges with
    /// `ROLLBACK` or `COMMIT`, every statement reports this.
    Aborted(AbortReason),
    /// The transaction's rows cannot be published; it was rolled back.
    Capture(String),
    /// The statement was prepared by a session of another domain, binding or
    /// binding generation. Prepare it again; nothing ran.
    Stale,
}
impl std::fmt::Display for SqlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Statement { code, message } => write!(f, "{message} (code {code})"),
            Self::Denied => write!(f, "statement is not authorized"),
            Self::Unsupported(reason) => write!(f, "unsupported statement: {reason}"),
            Self::Unavailable { table, reason } => {
                write!(f, "{table} is not available: {reason:?}")
            }
            Self::Aborted(reason) => write!(f, "transaction was rolled back: {reason:?}"),
            Self::Capture(reason) => write!(f, "transaction cannot be published: {reason}"),
            Self::Stale => write!(f, "statement was prepared under another binding"),
        }
    }
}
impl SqlError {
    pub fn statement(message: &str) -> Self {
        Self::Statement {
            code: 1,
            message: message.into(),
        }
    }
    pub fn engine(error: &crate::sqlite::Error) -> Self {
        use crate::sqlite::Error as Engine;
        match error {
            Engine::Denied => Self::Denied,
            Engine::Unsupported(reason) => Self::Unsupported(reason.clone()),
            Engine::Deadline => Self::Statement {
                code: 9,
                message: "interrupted".into(),
            },
            Engine::Busy(message) => Self::Statement {
                code: 5,
                message: message.clone(),
            },
            Engine::Sqlite(rusqlite::Error::SqliteFailure(failure, message)) => Self::Statement {
                code: failure.extended_code,
                message: message.clone().unwrap_or_else(|| failure.to_string()),
            },
            // Binding and conversion errors of the adapter: a misused call.
            other => Self::Statement {
                code: 21,
                message: other.to_string(),
            },
        }
    }
}
/// What a caller knows after a failed call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Certainty {
    /// Nothing happened; the same call may be sent again.
    Retryable,
    /// The outcome is known: the statement or transaction had no effect.
    Definite,
    /// A commit may have happened. Ask `Replica::sql_outcome` with its token.
    Uncertain,
}
impl Error {
    /// How much a failed session call tells. A transaction body is never
    /// replayed by the runtime; a retryable error concerns one call.
    pub fn sql_certainty(&self) -> Certainty {
        match self {
            Self::Closed | Self::Rollback { .. } => Certainty::Uncertain,
            Self::Backpressure | Self::RetryExhausted => Certainty::Retryable,
            error if crate::storage::retryable(error) => Certainty::Retryable,
            _ => Certainty::Definite,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlState {
    /// No transaction: a statement commits by itself.
    Idle,
    /// A transaction holds the file's writer. It ends with a commit, a
    /// rollback or an error that reports `SqlError::Aborted`.
    Active,
}
/// How the session interprets one statement.
#[derive(Debug, PartialEq, Eq)]
pub enum Control {
    /// `BEGIN` with its mode keyword.
    Begin(&'static str),
    Commit,
    Rollback,
    RollbackTo(String),
    Savepoint(String),
    Release(String),
    Statement,
}
/// Transaction control is interpreted by the session and never reaches the
/// engine as text. Anything else must be a single statement.
pub fn classify(sql: &str) -> Result<Control, SqlError> {
    let mut words: Vec<String> = scoped_connection::tokens(sql)
        .into_iter()
        .filter(|token| !token.trim().is_empty())
        .collect();
    while words.last().is_some_and(|word| word == ";") {
        words.pop();
    }
    let syntax = || SqlError::statement("transaction control syntax");
    let is = |index: usize, word: &str| {
        words
            .get(index)
            .is_some_and(|token| token.eq_ignore_ascii_case(word))
    };
    let name = |index: usize| {
        let name = words.get(index).filter(|_| words.len() == index + 1);
        name.map(|name| {
            name.trim_matches(['"', '`', '[', ']', '\''])
                .to_ascii_lowercase()
        })
        .filter(|name| !name.is_empty())
        .ok_or_else(syntax)
    };
    let optional = |index: usize, word: &str| index + usize::from(is(index, word));
    if words.is_empty() {
        return Err(SqlError::statement("empty statement"));
    }
    if is(0, "BEGIN") {
        let mode = ["DEFERRED", "IMMEDIATE", "EXCLUSIVE"]
            .into_iter()
            .find(|mode| is(1, mode));
        let end = optional(1 + usize::from(mode.is_some()), "TRANSACTION");
        return (words.len() == end)
            .then_some(Control::Begin(mode.unwrap_or("DEFERRED")))
            .ok_or_else(syntax);
    }
    if is(0, "COMMIT") || is(0, "END") {
        return (words.len() == optional(1, "TRANSACTION"))
            .then_some(Control::Commit)
            .ok_or_else(syntax);
    }
    if is(0, "ROLLBACK") {
        let next = optional(1, "TRANSACTION");
        if words.len() == next {
            return Ok(Control::Rollback);
        }
        if !is(next, "TO") {
            return Err(syntax());
        }
        return name(optional(next + 1, "SAVEPOINT")).map(Control::RollbackTo);
    }
    if is(0, "SAVEPOINT") {
        return name(1).map(Control::Savepoint);
    }
    if is(0, "RELEASE") {
        // `RELEASE savepoint` names a savepoint called "savepoint".
        let next = if words.len() > 2 {
            optional(1, "SAVEPOINT")
        } else {
            1
        };
        return name(next).map(Control::Release);
    }
    if words.iter().any(|word| word == ";") {
        return Err(SqlError::statement("one statement per call"));
    }
    Ok(Control::Statement)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_control_is_classified_and_other_text_is_one_statement() {
        let name = |name: &str| name.to_string();
        for (sql, expected) in [
            ("BEGIN", Control::Begin("DEFERRED")),
            ("begin immediate transaction;", Control::Begin("IMMEDIATE")),
            ("BEGIN EXCLUSIVE", Control::Begin("EXCLUSIVE")),
            ("BEGIN TRANSACTION", Control::Begin("DEFERRED")),
            ("END TRANSACTION", Control::Commit),
            ("commit ;", Control::Commit),
            ("ROLLBACK TRANSACTION", Control::Rollback),
            (
                "ROLLBACK TO SAVEPOINT \"Outer\"",
                Control::RollbackTo(name("outer")),
            ),
            (
                "rollback transaction to s1",
                Control::RollbackTo(name("s1")),
            ),
            ("SAVEPOINT 's 1'", Control::Savepoint(name("s 1"))),
            ("RELEASE SAVEPOINT s1", Control::Release(name("s1"))),
            ("RELEASE savepoint", Control::Release(name("savepoint"))),
            ("SELECT ';' FROM t;", Control::Statement),
            ("INSERT INTO t VALUES('BEGIN')", Control::Statement),
        ] {
            assert_eq!(classify(sql), Ok(expected), "{sql}");
        }
        for sql in [
            "",
            " ; ",
            "BEGIN CONCURRENT",
            "COMMIT AND CHAIN",
            "ROLLBACK TO",
            "SAVEPOINT",
            "SAVEPOINT a b",
            "SELECT 1; SELECT 2",
            "SELECT 1; COMMIT",
        ] {
            assert!(classify(sql).is_err(), "{sql}");
        }
    }
}
