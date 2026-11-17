use crate::{CommitClass, Error, scoped_connection};
use std::time::Duration;

pub const MAX_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_SAVEPOINTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SqlOptions {
    pub transaction_timeout: Duration,
    pub statement_budget: Duration,
    pub captured_rows: usize,
    pub captured_bytes: usize,
    pub result_bytes: usize,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    NotHeld,
    KeyMissing,
    Incomplete,
    Revoked,
    Released,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbortReason {
    Timeout,
    Cancelled,
    Poisoned(String),
    Engine(String),
    Preempted,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlError {
    Statement { code: i32, message: String },
    Denied,
    Unsupported(String),
    Unavailable { table: String, reason: Unavailable },
    Aborted(AbortReason),
    Capture(String),
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
            other => Self::Statement {
                code: SQLITE_MISUSE,
                message: other.to_string(),
            },
        }
    }
}
const SQLITE_MISUSE: i32 = 21;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Certainty {
    Retryable,
    Definite,
    ResolveWithTransactionOutcome,
}
impl Error {
    pub fn sql_certainty(&self) -> Certainty {
        match self {
            Self::Closed | Self::Rollback { .. } => Certainty::ResolveWithTransactionOutcome,
            Self::Backpressure | Self::RetryExhausted => Certainty::Retryable,
            error if crate::storage::retryable(error) => Certainty::Retryable,
            _ => Certainty::Definite,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlState {
    Idle,
    Active,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Control {
    Begin(&'static str),
    Commit,
    Rollback,
    RollbackTo(String),
    Savepoint(String),
    Release(String),
    Statement,
}
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
        let keyword_precedes_name = words.len() > 2;
        let next = if keyword_precedes_name {
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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_failure_that_may_have_committed_is_resolved_with_the_transaction_outcome() {
        assert_eq!(
            Error::Closed.sql_certainty(),
            Certainty::ResolveWithTransactionOutcome
        );
        assert_eq!(
            Error::Rollback {
                cause: "commit".into(),
                rollback: crate::sqlite::Error::Deadline.into(),
            }
            .sql_certainty(),
            Certainty::ResolveWithTransactionOutcome
        );
        assert_eq!(Error::Backpressure.sql_certainty(), Certainty::Retryable);
        assert_eq!(
            Error::Sql(SqlError::Denied).sql_certainty(),
            Certainty::Definite
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
