use crate::scoped_connection::ScopedConnection as Connection;
use crate::sqlite;
use rocky_core::{ProtocolError, SqlValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureReason {
    Denial,
    UnsupportedGeneration,
    MissingDependencies,
    Quota,
    Corruption,
    Conflict,
    Closed,
    Storage,
}
#[derive(Debug)]
pub struct StorageFailure {
    source: sqlite::Error,
}
impl std::fmt::Display for StorageFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}
impl std::error::Error for StorageFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
impl From<sqlite::Error> for StorageFailure {
    fn from(source: sqlite::Error) -> Self {
        Self { source }
    }
}
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Storage(StorageFailure),
    Protocol(ProtocolError),
    Application(Box<dyn std::error::Error + Send + Sync>),
    Incompatible(String),
    Corrupt(String),
    Durability(String),
    Backpressure,
    Quota {
        resource: &'static str,
        limit: usize,
    },
    Coordination(rocky_core::CoordinationError),
    Rejected(String),
    /// A gate refused: every blocker it found, never only the first.
    Blocked(Vec<rocky_core::Blocker>),
    RetryExhausted,
    Conflict,
    /// Retained history reached its bound. Writes resume once a checkpoint
    /// and delivery acknowledgments let pruning release history.
    Retention {
        retained: usize,
        limit: usize,
    },
    /// The handle committed this operation, which has since been
    /// pruned. It is not executed again; its bytes are no longer available.
    Pruned {
        node: rocky_core::NodeId,
        sequence: u64,
    },
    Closed,
    Rollback {
        cause: String,
        rollback: StorageFailure,
    },
    /// A SQL session call failed; see `Error::sql_certainty`.
    Sql(crate::sql::SqlError),
    #[cfg(any(test, feature = "test-support", feature = "qualification"))]
    InjectedFailure,
}
impl Error {
    /// A short category for diagnostics; it never carries the message.
    pub fn category(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::Storage(_) => "storage",
            Self::Protocol(_) => "protocol",
            Self::Application(_) => "application",
            Self::Incompatible(_) => "incompatible",
            Self::Corrupt(_) => "corrupt",
            Self::Durability(_) => "durability",
            Self::Backpressure => "backpressure",
            Self::Quota { .. } => "quota",
            Self::Coordination(_) => "coordination",
            Self::Rejected(_) => "rejected",
            Self::Blocked(_) => "blocked",
            Self::RetryExhausted => "retry_exhausted",
            Self::Conflict => "conflict",
            Self::Retention { .. } => "retention",
            Self::Pruned { .. } => "pruned",
            Self::Closed => "closed",
            Self::Rollback { .. } => "rollback",
            Self::Sql(_) => "sql",
            #[cfg(any(test, feature = "test-support", feature = "qualification"))]
            Self::InjectedFailure => "injected",
        }
    }
    pub fn reason(&self) -> FailureReason {
        match self {
            Self::Application(_)
            | Self::Rejected(_)
            | Self::Blocked(_)
            | Self::Sql(crate::sql::SqlError::Denied)
            | Self::Coordination(rocky_core::CoordinationError::Denied) => FailureReason::Denial,
            Self::Protocol(ProtocolError::UnsupportedVersion { .. }) | Self::Incompatible(_) => {
                FailureReason::UnsupportedGeneration
            }
            Self::Protocol(ProtocolError::NotReady(_)) => FailureReason::MissingDependencies,
            Self::Quota { .. } | Self::Backpressure | Self::Retention { .. } => {
                FailureReason::Quota
            }
            Self::Corrupt(_) | Self::Rollback { .. } => FailureReason::Corruption,
            Self::Conflict
            | Self::Pruned { .. }
            | Self::Protocol(ProtocolError::IdentityConflict(_))
            | Self::Coordination(rocky_core::CoordinationError::RequestConflict) => {
                FailureReason::Conflict
            }
            Self::Closed => FailureReason::Closed,
            _ => FailureReason::Storage,
        }
    }
    pub fn is_retryable(&self) -> bool {
        retryable(self)
            || matches!(
                self,
                Self::Storage(StorageFailure {
                    source: sqlite::Error::Deadline
                })
            )
            || matches!(
                self,
                Self::Backpressure
                    | Self::Retention { .. }
                    | Self::RetryExhausted
                    | Self::Coordination(
                        rocky_core::CoordinationError::Unavailable
                            | rocky_core::CoordinationError::StaleEpoch
                    )
            )
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Storage(e) => write!(f, "SQLite storage: {e}"),
            Self::Protocol(e) => write!(f, "operation protocol: {e}"),
            Self::Application(e) => write!(f, "application rejected request: {e}"),
            Self::Incompatible(e) => write!(f, "incompatible replica file: {e}"),
            Self::Corrupt(e) => write!(f, "replica consistency check failed: {e}"),
            Self::Durability(e) => write!(f, "required durability setting unavailable: {e}"),
            Self::Rejected(e) => write!(f, "replication data rejected: {e}"),
            Self::Blocked(blockers) => f.write_str(&rocky_core::describe_blockers(blockers)),
            Self::Coordination(e) => write!(f, "coordination: {e}"),
            Self::Quota { resource, limit } => write!(f, "{resource} quota reached ({limit})"),
            Self::Backpressure => {
                write!(f, "storage queue is full; retry when pending work finishes")
            }
            Self::Conflict => write!(f, "submission identity reused with different input"),
            Self::Retention { retained, limit } => write!(
                f,
                "retained history holds {retained} of {limit} bytes; waiting for a checkpoint and delivery acknowledgments"
            ),
            Self::Pruned { sequence, .. } => write!(
                f,
                "already committed as sequence {sequence}, which has been pruned; not executed again"
            ),
            Self::RetryExhausted => write!(
                f,
                "storage remained busy after every whole-transaction attempt"
            ),
            Self::Closed => write!(f, "replica is closed"),
            Self::Sql(error) => write!(f, "SQL session: {error}"),
            Self::Rollback { cause, rollback } => {
                write!(f, "{cause}; rollback failed: {rollback}; replica stopped")
            }
            #[cfg(any(test, feature = "test-support", feature = "qualification"))]
            Self::InjectedFailure => write!(f, "injected pre-commit failure"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Application(error) => Some(error.as_ref()),
            Self::Coordination(error) => Some(error),
            Self::Rollback { rollback, .. } => Some(rollback),
            _ => None,
        }
    }
}
impl From<sqlite::Error> for Error {
    fn from(e: sqlite::Error) -> Self {
        Self::Storage(e.into())
    }
}
impl From<ProtocolError> for Error {
    fn from(e: ProtocolError) -> Self {
        Self::Protocol(e)
    }
}
pub fn application(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Application(Box::new(e))
}
pub fn retryable(e: &Error) -> bool {
    match e {
        Error::Storage(error) => matches!(error.source, sqlite::Error::Busy(_)),
        _ => false,
    }
}

/// Whole-transaction attempts before a conflict is reported as `RetryExhausted`.
/// The delays add up to 318 ms. Inside one process the single writer never
/// conflicts; this bounds the wait for a file briefly held by another process.
pub const TRANSACTION_ATTEMPTS: usize = 10;
pub fn backoff(attempt: usize) -> std::time::Duration {
    std::time::Duration::from_millis(2 << attempt.min(5))
}
/// Longest read transaction. An open reader stops the WAL from being reset,
/// so a read that runs longer is interrupted with a retryable error.
pub const READ_TRANSACTION_BOUND: std::time::Duration = std::time::Duration::from_millis(500);

/// The engine ended the file writer's group while a step was running.
pub fn group_lost() -> Error {
    Error::Storage(sqlite::Error::Group("the engine ended the transaction".into()).into())
}
/// The error every further waiter of a failed group receives; the original
/// goes to one waiter.
pub fn group_error(error: &Error) -> Error {
    match error {
        Error::RetryExhausted => Error::RetryExhausted,
        Error::Closed => Error::Closed,
        error if retryable(error) => Error::RetryExhausted,
        error => Error::Storage(sqlite::Error::Group(error.to_string()).into()),
    }
}

pub async fn rows(conn: &Connection, sql: &str) -> Result<Vec<Vec<SqlValue>>, Error> {
    collect_rows(conn.query(sql, ()).await?).await
}
pub async fn collect_rows(mut result: sqlite::Rows) -> Result<Vec<Vec<SqlValue>>, Error> {
    let mut values = Vec::new();
    while let Some(row) = result.next().await? {
        let mut columns = Vec::new();
        for column in 0..row.column_count() {
            columns.push(sql_value(row.get_value(column)?)?);
        }
        values.push(columns);
    }
    Ok(values)
}
/// An engine value at the application boundary. The engine stores NaN as
/// NULL, so a NaN can only come from an expression.
pub fn sql_value(value: sqlite::Value) -> Result<SqlValue, Error> {
    Ok(match value {
        sqlite::Value::Null => SqlValue::Null,
        sqlite::Value::Integer(v) => SqlValue::Integer(v),
        sqlite::Value::Real(v) => {
            SqlValue::real(v).ok_or_else(|| Error::Rejected("NaN is not a REAL value".into()))?
        }
        sqlite::Value::Text(v) => SqlValue::Text(v),
        sqlite::Value::Blob(v) => SqlValue::Blob(v),
    })
}
pub fn bindings(values: Vec<SqlValue>) -> Vec<sqlite::Value> {
    values
        .into_iter()
        .map(|v| match v {
            SqlValue::Null => sqlite::Value::Null,
            SqlValue::Integer(v) => sqlite::Value::Integer(v),
            SqlValue::Real(v) => sqlite::Value::Real(v.get()),
            SqlValue::Text(v) => sqlite::Value::Text(v),
            SqlValue::Blob(v) => sqlite::Value::Blob(v),
        })
        .collect()
}
/// `finish` on an unscoped connection.
pub async fn finish_raw<T>(
    conn: &sqlite::Connection,
    result: Result<T, Error>,
) -> Result<T, Error> {
    let result = match result {
        Ok(value) => match conn.execute("COMMIT", ()).await {
            Ok(_) => return Ok(value),
            Err(e) => Err(Error::from(e)),
        },
        Err(e) => Err(e),
    };
    if conn.is_autocommit()? {
        return result;
    }
    if let Err(rollback) = conn.execute("ROLLBACK", ()).await {
        return Err(Error::Rollback {
            cause: result.err().map(|e| e.to_string()).unwrap_or_default(),
            rollback: rollback.into(),
        });
    }
    result
}
pub async fn finish<T>(conn: &Connection, result: Result<T, Error>) -> Result<T, Error> {
    let result = match result {
        Ok(value) => match conn.execute("COMMIT", ()).await {
            Ok(_) => return Ok(value),
            Err(e) => Err(Error::from(e)),
        },
        Err(e) => Err(e),
    };
    // SQLite ends the transaction itself after some errors, such as a full disk.
    if conn.is_autocommit()? {
        return result;
    }
    if let Err(rollback) = conn.execute("ROLLBACK", ()).await {
        return Err(Error::Rollback {
            cause: result.err().map(|e| e.to_string()).unwrap_or_default(),
            rollback: rollback.into(),
        });
    }
    result
}

impl From<rocky_core::CoordinationError> for Error {
    fn from(e: rocky_core::CoordinationError) -> Self {
        Self::Coordination(e)
    }
}
