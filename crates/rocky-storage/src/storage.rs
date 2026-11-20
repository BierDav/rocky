use crate::scoped_connection::ScopedConnection as Connection;
use crate::sqlite;
use rocky_core::{ProtocolError, SqlValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureReason {
    Denial,
    UnsupportedGeneration,
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
    Blocked(Vec<rocky_core::Blocker>),
    RetryExhausted,
    Conflict,
    Closed,
    Rollback {
        cause: String,
        rollback: StorageFailure,
    },
    Sql(crate::sql::SqlError),
    #[cfg(any(test, feature = "test-support", feature = "qualification"))]
    InjectedFailure,
}
impl Error {
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
            Self::Incompatible(_) => FailureReason::UnsupportedGeneration,
            Self::Quota { .. } | Self::Backpressure => FailureReason::Quota,
            Self::Corrupt(_) | Self::Rollback { .. } => FailureReason::Corruption,
            Self::Conflict | Self::Coordination(rocky_core::CoordinationError::RequestConflict) => {
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
            Self::Protocol(e) => write!(f, "protocol: {e}"),
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
            Self::Conflict => write!(
                f,
                "the operation conflicts with a concurrent change or an earlier submission of the same identity; retry against the current state"
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

pub const TRANSACTION_ATTEMPTS: usize = 10;
const FIRST_BACKOFF_MILLIS: u64 = 2;
const BACKOFF_DOUBLINGS_BEFORE_PLATEAU: usize = 5;
pub fn backoff(attempt: usize) -> std::time::Duration {
    std::time::Duration::from_millis(
        FIRST_BACKOFF_MILLIS << attempt.min(BACKOFF_DOUBLINGS_BEFORE_PLATEAU),
    )
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
pub async fn finish<T>(conn: &sqlite::Connection, result: Result<T, Error>) -> Result<T, Error> {
    let cause = match result {
        Ok(value) => match conn.execute("COMMIT", ()).await {
            Ok(_) => return Ok(value),
            Err(commit) => Error::from(commit),
        },
        Err(cause) => cause,
    };
    let sqlite_already_ended_transaction = conn.is_autocommit()?;
    if sqlite_already_ended_transaction {
        return Err(cause);
    }
    match conn.execute("ROLLBACK", ()).await {
        Ok(_) => Err(cause),
        Err(rollback) => Err(Error::Rollback {
            cause: cause.to_string(),
            rollback: rollback.into(),
        }),
    }
}

impl From<rocky_core::CoordinationError> for Error {
    fn from(e: rocky_core::CoordinationError) -> Self {
        Self::Coordination(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &[&str] = &[
        "CREATE TABLE parent(id INTEGER PRIMARY KEY)",
        "CREATE TABLE child(parent INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
    ];

    enum Body {
        Succeeds,
        Fails,
        ViolatesDeferredForeignKey,
        RollsBackItselfThenFails,
    }

    #[derive(Debug, PartialEq)]
    struct Outcome {
        result: Result<i64, &'static str>,
        children: Vec<Vec<SqlValue>>,
        transaction_open: bool,
    }

    async fn run(body: Body) -> Outcome {
        let conn = Connection::new(
            sqlite::Connection::open_in_memory().unwrap(),
            Some("scope"),
            SCHEMA,
        );
        conn.execute("PRAGMA foreign_keys=ON", ()).await.unwrap();
        for sql in SCHEMA {
            conn.execute(sql, ()).await.unwrap();
        }
        conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
        let result = match body {
            Body::Succeeds => {
                conn.execute("INSERT INTO parent(id) VALUES(1)", ())
                    .await
                    .unwrap();
                conn.execute("INSERT INTO child(parent) VALUES(1)", ())
                    .await
                    .unwrap();
                Ok(7)
            }
            Body::Fails => {
                conn.execute("INSERT INTO parent(id) VALUES(1)", ())
                    .await
                    .unwrap();
                conn.execute("INSERT INTO child(parent) VALUES(1)", ())
                    .await
                    .unwrap();
                Err(Error::Conflict)
            }
            Body::ViolatesDeferredForeignKey => {
                conn.execute("INSERT INTO child(parent) VALUES(9)", ())
                    .await
                    .unwrap();
                Ok(7)
            }
            Body::RollsBackItselfThenFails => {
                conn.execute("ROLLBACK", ()).await.unwrap();
                Err(Error::Conflict)
            }
        };
        let result = finish(conn.connection(), result).await;
        Outcome {
            result: result.map_err(|error| error.category()),
            children: rows(&conn, "SELECT parent FROM child").await.unwrap(),
            transaction_open: !conn.is_autocommit().unwrap(),
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_successful_body_commits_its_writes_and_returns_its_value() {
        assert_eq!(
            run(Body::Succeeds).await,
            Outcome {
                result: Ok(7),
                children: vec![vec![SqlValue::Integer(1)]],
                transaction_open: false,
            },
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_failed_body_rolls_back_and_returns_its_own_error() {
        assert_eq!(
            run(Body::Fails).await,
            Outcome {
                result: Err("conflict"),
                children: Vec::new(),
                transaction_open: false,
            },
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_refused_commit_rolls_back_and_returns_the_storage_error() {
        assert_eq!(
            run(Body::ViolatesDeferredForeignKey).await,
            Outcome {
                result: Err("storage"),
                children: Vec::new(),
                transaction_open: false,
            },
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_transaction_already_ended_before_finish_is_not_rolled_back_again() {
        assert_eq!(
            run(Body::RollsBackItselfThenFails).await,
            Outcome {
                result: Err("conflict"),
                children: Vec::new(),
                transaction_open: false,
            },
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn backoff_doubles_from_two_milliseconds_and_stops_at_sixty_four() {
        let millis: Vec<_> = (0..TRANSACTION_ATTEMPTS)
            .map(|attempt| backoff(attempt).as_millis())
            .collect();
        assert_eq!(millis, [2, 4, 8, 16, 32, 64, 64, 64, 64, 64]);
        assert_eq!(backoff(usize::MAX).as_millis(), 64);
    }
}
