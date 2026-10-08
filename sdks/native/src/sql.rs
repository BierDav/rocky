//! Typed statement, cursor and table-event handles for a database driver.
//! One call executes a statement or delivers a page; values cross typed,
//! never as JSON. See `docs/runtime/sql-driver.md`.
use crate::{NativeError, database::NativeDatabase, failure};
use rocky_sdk::{ConnectionOptions, SqlFailure, SqlValue};
use std::sync::Arc;

/// One SQLite value.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum NativeValue {
    Null,
    Integer { value: i64 },
    Real { value: f64 },
    Text { value: String },
    Blob { value: Vec<u8> },
}
impl NativeValue {
    fn sql(self) -> Result<SqlValue, NativeError> {
        Ok(match self {
            Self::Null => SqlValue::Null,
            Self::Integer { value } => SqlValue::Integer(value),
            // SQLite stores a NaN as NULL.
            Self::Real { value } => SqlValue::real(value).unwrap_or(SqlValue::Null),
            Self::Text { value } => SqlValue::Text(value),
            Self::Blob { value } => SqlValue::Blob(value),
        })
    }
}
impl From<SqlValue> for NativeValue {
    fn from(value: SqlValue) -> Self {
        match value {
            SqlValue::Null => Self::Null,
            SqlValue::Integer(value) => Self::Integer { value },
            SqlValue::Real(value) => Self::Real { value: value.get() },
            SqlValue::Text(value) => Self::Text { value },
            SqlValue::Blob(value) => Self::Blob { value },
        }
    }
}
/// One page of a statement's result; columns come with the first.
#[derive(uniffi::Record)]
pub struct NativePage {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<NativeValue>>,
    pub done: bool,
}
impl From<rocky_sdk::Page> for NativePage {
    fn from(page: rocky_sdk::Page) -> Self {
        Self {
            columns: page.columns,
            rows: page
                .rows
                .into_iter()
                .map(|row| row.into_iter().map(NativeValue::from).collect())
                .collect(),
            done: page.done,
        }
    }
}
fn sql(error: SqlFailure) -> NativeError {
    NativeError::Sql {
        code: error.code,
        detail: error.message,
    }
}

#[uniffi::export]
impl NativeDatabase {
    /// A connection for a driver: the writer is a secure SQL session, a
    /// reader steps snapshot cursors. `profile` names the ORM version
    /// whose housekeeping the connection answers, such as `room-2.8.4`.
    pub async fn connect(
        &self,
        read_only: bool,
        profile: Option<String>,
        transaction_timeout_millis: Option<u64>,
    ) -> Result<Arc<NativeConnection>, NativeError> {
        let options = ConnectionOptions {
            read_only,
            profile: profile
                .map(|name| name.parse())
                .transpose()
                .map_err(failure)?,
            transaction_timeout: transaction_timeout_millis.map(std::time::Duration::from_millis),
        };
        let connection = self.database().connect(options).await.map_err(failure)?;
        Ok(Arc::new(NativeConnection {
            connection: Arc::new(connection),
        }))
    }
    /// Committed changes of `tables`, or of every table.
    pub async fn observe_tables(
        &self,
        tables: Option<Vec<String>>,
    ) -> Result<Arc<NativeTableEvents>, NativeError> {
        let events = self
            .database()
            .observe_tables(tables)
            .await
            .map_err(failure)?;
        Ok(Arc::new(NativeTableEvents { events }))
    }
}

/// One SQL connection. A driver uses it from one caller at a time.
#[derive(uniffi::Object)]
pub struct NativeConnection {
    connection: Arc<rocky_sdk::Connection>,
}
#[uniffi::export]
impl NativeConnection {
    /// A statement handle for `sql`; nothing runs until `execute`.
    pub fn statement(&self, sql: String) -> Arc<NativeStatement> {
        Arc::new(NativeStatement {
            connection: self.connection.clone(),
            id: self.connection.statement(),
            sql,
        })
    }
    pub async fn in_transaction(&self) -> Result<bool, NativeError> {
        self.connection.in_transaction().await.map_err(sql)
    }
    /// An open transaction rolls back; cursors end.
    pub async fn shutdown(&self) {
        self.connection.close().await;
    }
}

/// One statement of a connection and the undelivered rest of its result.
#[derive(uniffi::Object)]
pub struct NativeStatement {
    connection: Arc<rocky_sdk::Connection>,
    id: u64,
    sql: String,
}
#[uniffi::export]
impl NativeStatement {
    /// Runs the statement with `params` and returns its first page.
    pub async fn execute(&self, params: Vec<NativeValue>) -> Result<NativePage, NativeError> {
        let params = params
            .into_iter()
            .map(NativeValue::sql)
            .collect::<Result<Vec<_>, _>>()?;
        self.connection
            .execute(self.id, self.sql.clone(), params)
            .await
            .map(NativePage::from)
            .map_err(sql)
    }
    pub async fn next(&self) -> Result<NativePage, NativeError> {
        self.connection
            .next(self.id)
            .await
            .map(NativePage::from)
            .map_err(sql)
    }
    /// Drops the rest of the result; a cursor ends.
    pub async fn finish(&self) -> Result<(), NativeError> {
        self.connection.finish(self.id).await.map_err(sql)
    }
}

/// Tables whose committed rows may differ, and why.
#[derive(uniffi::Record)]
pub struct NativeTableChange {
    pub tables: Vec<String>,
    pub causes: Vec<String>,
}
#[derive(uniffi::Object)]
pub struct NativeTableEvents {
    events: rocky_sdk::TableEvents,
}
#[uniffi::export]
impl NativeTableEvents {
    /// Waits for the next change; an error once the database closed.
    pub async fn next(&self) -> Result<NativeTableChange, NativeError> {
        let change = self.events.next().await.map_err(failure)?;
        Ok(NativeTableChange {
            tables: change.tables,
            causes: change.causes,
        })
    }
}
