//! Rust SQL conveniences over the same secure connections used by the bindings.
use crate::{Connection, ConnectionOptions, Database, Failure, Page, SqlFailure, SqlValue};
use std::{collections::VecDeque, sync::Arc};

/// Converts a bound parameter without SQL string interpolation.
/// REAL parameters use `SqlValue::real` to reject NaN explicitly.
pub trait IntoSqlValue {
    fn into_sql_value(self) -> SqlValue;
}
impl IntoSqlValue for SqlValue {
    fn into_sql_value(self) -> SqlValue {
        self
    }
}
impl<T: IntoSqlValue + Clone> IntoSqlValue for &T {
    fn into_sql_value(self) -> SqlValue {
        self.clone().into_sql_value()
    }
}
impl IntoSqlValue for i64 {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Integer(self)
    }
}
impl IntoSqlValue for i32 {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Integer(self.into())
    }
}
impl IntoSqlValue for bool {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Integer(self.into())
    }
}
impl IntoSqlValue for String {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Text(self)
    }
}
impl IntoSqlValue for &str {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Text(self.into())
    }
}
impl IntoSqlValue for Vec<u8> {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Blob(self)
    }
}
impl IntoSqlValue for &[u8] {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Blob(self.into())
    }
}
impl<T: IntoSqlValue> IntoSqlValue for Option<T> {
    fn into_sql_value(self) -> SqlValue {
        self.map_or(SqlValue::Null, IntoSqlValue::into_sql_value)
    }
}
/// Heterogeneous bound parameters: `params![42_i64, "hello", None::<String>]`.
#[macro_export]
macro_rules! params {
    ($($value:expr),* $(,)?) => { vec![$($crate::IntoSqlValue::into_sql_value($value)),*] };
}

/// A missing, ambiguous or incorrectly typed result column. Values are never included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError(pub String);
impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DecodeError {}

/// Strict decoding: NULL requires `Option<T>` and values are never silently coerced.
pub trait FromSqlValue: Sized {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError>;
}
macro_rules! decode {
    ($ty:ty, $variant:ident) => {
        impl FromSqlValue for $ty {
            fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
                match value {
                    SqlValue::$variant(value) => Ok(value.clone()),
                    _ => Err(DecodeError(concat!("expected ", stringify!($ty)).into())),
                }
            }
        }
    };
}
decode!(i64, Integer);
impl FromSqlValue for i32 {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        i64::from_sql_value(value)?
            .try_into()
            .map_err(|_| DecodeError("integer outside i32 range".into()))
    }
}
decode!(String, Text);
decode!(Vec<u8>, Blob);
impl FromSqlValue for f64 {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        match value {
            SqlValue::Real(value) => Ok(value.get()),
            _ => Err(DecodeError("expected REAL".into())),
        }
    }
}
impl FromSqlValue for bool {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        match value {
            SqlValue::Integer(0) => Ok(false),
            SqlValue::Integer(1) => Ok(true),
            _ => Err(DecodeError("expected boolean integer 0 or 1".into())),
        }
    }
}
impl<T: FromSqlValue> FromSqlValue for Option<T> {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        match value {
            SqlValue::Null => Ok(None),
            value => T::from_sql_value(value).map(Some),
        }
    }
}

/// An owned row, valid after advancing or closing the query.
#[derive(Clone, Debug)]
pub struct Row {
    columns: Arc<Vec<String>>,
    values: Vec<SqlValue>,
}
impl Row {
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
    pub fn values(&self) -> &[SqlValue] {
        &self.values
    }
    pub fn get<T: FromSqlValue>(&self, column: &str) -> Result<T, DecodeError> {
        let mut matches = self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, name)| name.as_str() == column);
        let (index, _) = matches
            .next()
            .ok_or_else(|| DecodeError(format!("missing column {column}")))?;
        if matches.next().is_some() {
            return Err(DecodeError(format!(
                "ambiguous column {column}; use an alias or get_at"
            )));
        }
        self.get_at(index)
    }
    pub fn get_at<T: FromSqlValue>(&self, index: usize) -> Result<T, DecodeError> {
        let value = self
            .values
            .get(index)
            .ok_or_else(|| DecodeError(format!("column index {index} out of bounds")))?;
        T::from_sql_value(value)
            .map_err(|error| DecodeError(format!("column {}: {error}", self.columns[index])))
    }
}

enum Source<'a> {
    Owned(Connection),
    Borrowed(&'a Connection),
}
impl Source<'_> {
    fn connection(&self) -> &Connection {
        match self {
            Self::Owned(c) => c,
            Self::Borrowed(c) => c,
        }
    }
}
/// Paged results with bounded buffering. Dropping releases the underlying statement.
/// Transaction results borrow the transaction, preventing commit while they are live.
pub struct Rows<'a> {
    source: Source<'a>,
    statement: u64,
    columns: Arc<Vec<String>>,
    pending: VecDeque<Vec<SqlValue>>,
    done: bool,
}
impl<'a> Rows<'a> {
    async fn new(source: Source<'a>, sql: &str, params: Vec<SqlValue>) -> Result<Self, SqlFailure> {
        let statement = source.connection().statement();
        // Install the guard before awaiting so cancellation also releases this statement.
        let mut rows = Self {
            source,
            statement,
            columns: Arc::default(),
            pending: VecDeque::new(),
            done: false,
        };
        let page = rows
            .source
            .connection()
            .execute(statement, sql.into(), params)
            .await?;
        rows.page(page);
        Ok(rows)
    }
    fn page(&mut self, page: Page) {
        self.columns = Arc::new(page.columns);
        self.pending = page.rows.into();
        self.done = page.done;
    }
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
    pub async fn next(&mut self) -> Result<Option<Row>, SqlFailure> {
        loop {
            if let Some(values) = self.pending.pop_front() {
                return Ok(Some(Row {
                    columns: self.columns.clone(),
                    values,
                }));
            }
            if self.done {
                return Ok(None);
            }
            // A cancelled page read retires this result rather than skipping rows on reuse.
            self.done = true;
            let page = self.source.connection().next(self.statement).await?;
            self.page(page);
        }
    }
    /// Collect explicitly when the application knows the result fits in memory.
    pub async fn collect(mut self) -> Result<Vec<Row>, SqlFailure> {
        let mut rows = Vec::new();
        while let Some(row) = self.next().await? {
            rows.push(row);
        }
        Ok(rows)
    }
}
impl Drop for Rows<'_> {
    fn drop(&mut self) {
        self.source.connection().discard(self.statement);
    }
}

fn options(read_only: bool) -> ConnectionOptions {
    ConnectionOptions {
        read_only,
        durability: crate::Durability::Buffered,
        profile: None,
        transaction_timeout: None,
        binding: None,
    }
}
impl Database {
    /// Reads on an independent snapshot connection. No statement handles or ORM profile.
    pub async fn query(&self, sql: &str, params: Vec<SqlValue>) -> Result<Rows<'static>, Failure> {
        let connection = self.connect(options(true)).await?;
        Ok(Rows::new(Source::Owned(connection), sql, params).await?)
    }
    /// Starts an atomic write. Call `commit` explicitly; dropping rolls it back.
    /// On commit return, the write is visible to readers and observers and survives
    /// an application crash (WAL with `synchronous=NORMAL`), but not power loss.
    /// The background `settle_commits` flush makes it durable and publishable.
    /// For a flushed commit, use `Database::connect` with `Durability::Flushed`.
    /// Application callbacks are never automatically retried.
    pub async fn transaction(&self) -> Result<Transaction, Failure> {
        let connection = self.connect(options(false)).await?;
        connection
            .execute(connection.statement(), "BEGIN".into(), Vec::new())
            .await?;
        Ok(Transaction { connection })
    }
}

/// An exclusive write session. Drop (including task cancellation) disconnects and rolls back.
#[must_use = "a transaction must be committed or it rolls back"]
pub struct Transaction {
    connection: Connection,
}
impl Transaction {
    #[cfg(feature = "qualification")]
    pub fn measurement_id(&self) -> u64 {
        self.connection.measurement_id()
    }
    /// Runs a statement, discarding returned rows. Use `query` for RETURNING or SELECT.
    pub async fn execute(&mut self, sql: &str, params: Vec<SqlValue>) -> Result<(), SqlFailure> {
        let rows = self.query(sql, params).await?;
        rows.source.connection().finish(rows.statement).await
    }
    pub async fn query(
        &mut self,
        sql: &str,
        params: Vec<SqlValue>,
    ) -> Result<Rows<'_>, SqlFailure> {
        if !matches!(
            rocky_runtime::classify_sql(sql),
            Ok(rocky_runtime::SqlControl::Statement)
        ) {
            return Err(crate::connection::failure(
                crate::connection::code::MISUSE,
                "transaction control is managed by Transaction; use commit or rollback",
            ));
        }
        Rows::new(Source::Borrowed(&self.connection), sql, params).await
    }
    /// Allocates an identity in this transaction, optionally within a partition.
    pub async fn next_id(&mut self, table: &str, partition: Vec<SqlValue>) -> Result<i64, Failure> {
        let mut values = vec![SqlValue::Text(table.into())];
        values.extend(partition);
        let placeholders = vec!["?"; values.len()].join(",");
        let mut rows = self
            .query(&format!("SELECT rocky_next_id({placeholders})"), values)
            .await?;
        let row = rows.next().await?.ok_or("ID allocation returned no row")?;
        Ok(row.get_at(0)?)
    }
    /// Waits for local durability. A commit is never retried automatically. Cancellation while committing can
    /// leave its outcome uncertain; reconcile before repeating application work.
    pub async fn commit(self) -> Result<(), SqlFailure> {
        let result = self
            .connection
            .execute(self.connection.statement(), "COMMIT".into(), Vec::new())
            .await;
        self.connection.close().await;
        result.map(drop)
    }
    pub async fn rollback(self) -> Result<(), SqlFailure> {
        let result = self
            .connection
            .execute(self.connection.statement(), "ROLLBACK".into(), Vec::new())
            .await;
        self.connection.close().await;
        result.map(drop)
    }
}
