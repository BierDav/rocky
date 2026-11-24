use crate::{
    Database, Error, RowIdentity, SqlValue,
    connection::{ConnectionOptions, Page},
    database::Connection,
};
use std::{collections::VecDeque, sync::Arc};

/// Converts a Rust value into a statement parameter; `params!` uses it.
pub trait IntoSqlValue {
    /// The parameter value.
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
impl IntoSqlValue for TransactionToken {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Blob(self.0.to_vec())
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
/// Identifies one committed transaction on every device, for `transaction_outcome` and drop
/// reports.
pub struct TransactionToken(rocky_core::Commitment);
impl TransactionToken {
    pub(crate) fn new(transaction: rocky_core::Commitment) -> Self {
        Self(transaction)
    }
    pub(crate) fn commitment(self) -> rocky_core::Commitment {
        self.0
    }
}
impl std::fmt::Display for TransactionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&data_encoding::HEXLOWER.encode(&self.0[..TOKEN_DISPLAY_BYTES]))
    }
}
const TOKEN_DISPLAY_BYTES: usize = 6;
impl IntoSqlValue for RowIdentity {
    fn into_sql_value(self) -> SqlValue {
        self.sql()
    }
}
impl<const N: usize> IntoSqlValue for [u8; N] {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Blob(self.to_vec())
    }
}
impl IntoSqlValue for serde_json::Value {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Text(self.to_string())
    }
}
impl IntoSqlValue for Timestamp {
    fn into_sql_value(self) -> SqlValue {
        SqlValue::Integer(self.0)
    }
}
impl<T: IntoSqlValue> IntoSqlValue for Option<T> {
    fn into_sql_value(self) -> SqlValue {
        self.map_or(SqlValue::Null, IntoSqlValue::into_sql_value)
    }
}
/// Builds a statement's parameter list from values that implement `IntoSqlValue`.
#[macro_export]
macro_rules! params {
    ($($value:expr),* $(,)?) => { vec![$($crate::IntoSqlValue::into_sql_value($value)),*] };
}

/// A column value that could not become the requested Rust type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError(pub String);
impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DecodeError {}

/// Converts a column value into a Rust value; `Row::get` uses it.
pub trait FromSqlValue: Sized {
    /// The Rust value, or why the column value does not fit.
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError>;
}
impl FromSqlValue for SqlValue {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        Ok(value.clone())
    }
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
impl FromSqlValue for TransactionToken {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        match value {
            SqlValue::Blob(bytes) => bytes
                .as_slice()
                .try_into()
                .map(TransactionToken)
                .map_err(|_| DecodeError("expected a 32-byte transaction token".into())),
            _ => Err(DecodeError("expected a transaction token BLOB".into())),
        }
    }
}
impl FromSqlValue for RowIdentity {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        RowIdentity::from_sql(value)
            .map_err(|_| DecodeError("expected a 40-byte row identity BLOB".into()))
    }
}
impl<const N: usize> FromSqlValue for [u8; N] {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        match value {
            SqlValue::Blob(bytes) => bytes
                .as_slice()
                .try_into()
                .map_err(|_| DecodeError(format!("expected a {N}-byte BLOB"))),
            _ => Err(DecodeError("expected BLOB".into())),
        }
    }
}
impl FromSqlValue for serde_json::Value {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        match value {
            SqlValue::Text(text) => serde_json::from_str(text)
                .map_err(|error| DecodeError(format!("invalid JSON: {error}"))),
            _ => Err(DecodeError("expected JSON TEXT".into())),
        }
    }
}
impl FromSqlValue for Timestamp {
    fn from_sql_value(value: &SqlValue) -> Result<Self, DecodeError> {
        i64::from_sql_value(value).map(Self)
    }
}

/// A point in time as Unix microseconds, stored as an integer column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);
impl Timestamp {
    /// The instant `micros` after the Unix epoch.
    pub const fn from_unix_micros(micros: i64) -> Self {
        Self(micros)
    }
    /// Microseconds since the Unix epoch.
    pub const fn unix_micros(self) -> i64 {
        self.0
    }
    /// The instant as a `SystemTime`, or `None` outside its range.
    pub fn system_time(self) -> Option<web_time::SystemTime> {
        let offset = std::time::Duration::from_micros(self.0.unsigned_abs());
        if self.0 < 0 {
            web_time::UNIX_EPOCH.checked_sub(offset)
        } else {
            web_time::UNIX_EPOCH.checked_add(offset)
        }
    }
}
impl TryFrom<web_time::SystemTime> for Timestamp {
    type Error = DecodeError;
    fn try_from(time: web_time::SystemTime) -> Result<Self, DecodeError> {
        let out_of_range = || DecodeError("instant outside i64 microseconds".into());
        let micros = match time.duration_since(web_time::UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_micros()).map_err(|_| out_of_range())?,
            Err(before) => {
                let before = before.duration();
                let whole = i128::try_from(before.as_micros()).map_err(|_| out_of_range())?;
                let partial = i128::from(before.subsec_nanos() % 1_000 != 0);
                i64::try_from(-whole - partial).map_err(|_| out_of_range())?
            }
        };
        Ok(Self(micros))
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

/// One result row.
#[derive(Clone, Debug)]
pub struct Row {
    columns: Arc<Vec<String>>,
    values: Vec<SqlValue>,
}
impl Row {
    /// The column names, in order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
    /// The values, in column order.
    pub fn values(&self) -> &[SqlValue] {
        &self.values
    }
    /// The value of the named column; a name that appears twice needs an alias or `get_at`.
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
    /// The value of the column at `index`.
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
    Done,
    Owned(Connection),
    Borrowed(&'a Connection),
}
impl Source<'_> {
    fn connection(&self) -> Option<&Connection> {
        match self {
            Self::Done => None,
            Self::Owned(c) => Some(c),
            Self::Borrowed(c) => Some(c),
        }
    }
}
/// A query's rows, read in pages as `next` is called.
pub struct Rows<'a> {
    source: Source<'a>,
    statement: u64,
    columns: Arc<Vec<String>>,
    pending: VecDeque<Vec<SqlValue>>,
    done: bool,
}
impl<'a> Rows<'a> {
    async fn new(source: Source<'a>, sql: &str, params: Vec<SqlValue>) -> Result<Self, Error> {
        let closed = || Error::closed("the transaction is finished");
        let statement = source.connection().ok_or_else(closed)?.statement();
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
            .ok_or_else(closed)?
            .execute(statement, sql.into(), params)
            .await?;
        rows.page(page);
        Ok(rows)
    }
    fn from_page(source: Source<'a>, statement: u64, page: Page) -> Self {
        let mut rows = Self {
            source,
            statement,
            columns: Arc::default(),
            pending: VecDeque::new(),
            done: false,
        };
        rows.page(page);
        rows
    }
    fn page(&mut self, page: Page) {
        self.columns = Arc::new(page.columns);
        self.pending = page.rows.into();
        self.done = page.done;
    }
    /// The column names, in order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
    /// The next row, or `None` after the last.
    pub async fn next(&mut self) -> Result<Option<Row>, Error> {
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
            let Some(connection) = self.source.connection() else {
                self.done = true;
                return Ok(None);
            };
            let page = match connection.next(self.statement).await {
                Ok(page) => page,
                Err(error) => {
                    self.done = true;
                    return Err(error);
                }
            };
            self.page(page);
        }
    }
    /// Every remaining row.
    pub async fn collect(mut self) -> Result<Vec<Row>, Error> {
        let mut rows = Vec::new();
        while let Some(row) = self.next().await? {
            rows.push(row);
        }
        Ok(rows)
    }
}
impl Drop for Rows<'_> {
    fn drop(&mut self) {
        if !self.done
            && let Some(connection) = self.source.connection()
        {
            connection.discard(self.statement);
        }
    }
}

impl Database {
    /// Runs one read outside a transaction and returns its rows; a read of data that has not
    /// arrived yet fails with `Blocked`.
    pub async fn query(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
    ) -> Result<Rows<'static>, crate::Error> {
        let (page, connection) = self
            .query_once(sql.into(), params)
            .await
            .map_err(Error::failed)?;
        let source = match connection {
            Some(connection) => Source::Owned(connection),
            None => Source::Done,
        };
        Ok(Rows::from_page(source, 1, page))
    }
    /// Starts a write transaction, which rolls back unless committed.
    pub async fn transaction(&self) -> Result<Transaction, crate::Error> {
        let connection = self
            .connect(ConnectionOptions {
                read_only: false,
                transaction_timeout: None,
            })
            .await
            .map_err(Error::failed)?;
        let token = connection.begin_identified().await?;
        Ok(Transaction { connection, token })
    }
}

/// What a commit recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Commit {
    /// The transaction changed nothing, so nothing syncs.
    NoChanges,
    /// The transaction was recorded and syncs under this token.
    Recorded(TransactionToken),
}

/// A write transaction whose statements are recorded as SQL intents and replayed on every device.
#[must_use = "a transaction must be committed or it rolls back"]
pub struct Transaction {
    connection: Connection,
    token: TransactionToken,
}
impl Transaction {
    /// The token the transaction commits under.
    pub fn token(&self) -> TransactionToken {
        self.token
    }
    /// The connection id the qualification measurements name.
    #[cfg(feature = "qualification")]
    pub fn measurement_id(&self) -> u64 {
        self.connection.measurement_id()
    }
    /// Runs a statement that returns no rows; a constraint violation on replay drops the whole
    /// transaction on every device.
    pub async fn execute(&mut self, sql: &str, params: Vec<SqlValue>) -> Result<(), Error> {
        let rows = self.query(sql, params).await?;
        if rows.done {
            return Ok(());
        }
        match rows.source.connection() {
            Some(connection) => connection.finish(rows.statement).await,
            None => Ok(()),
        }
    }
    /// Runs a statement inside the transaction and returns its rows; `BEGIN`, `COMMIT` and
    /// `ROLLBACK` are refused.
    pub async fn query(&mut self, sql: &str, params: Vec<SqlValue>) -> Result<Rows<'_>, Error> {
        if !matches!(
            rocky_storage::sql::classify(sql),
            Ok(rocky_storage::sql::Control::Statement)
        ) {
            return Err(crate::connection::failure(
                crate::connection::code::MISUSE,
                "transaction control is managed by Transaction; use commit or rollback",
            ));
        }
        Rows::new(Source::Borrowed(&self.connection), sql, params).await
    }
    /// Allocates a row id in `table`, unique across devices; `partition` names the partition's
    /// values, empty for an unpartitioned table.
    pub async fn next_id(
        &mut self,
        table: &str,
        partition: Vec<SqlValue>,
    ) -> Result<RowIdentity, Error> {
        let mut values = vec![SqlValue::Text(table.into())];
        values.extend(partition);
        let placeholders = vec!["?"; values.len()].join(",");
        let mut rows = self
            .query(&format!("SELECT rocky_next_id({placeholders})"), values)
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| Error::internal("row id allocation returned no row"))?;
        Ok(row.get_at(0)?)
    }
    /// Commits locally and returns at once; the commit syncs later, and `Database::flush` makes it
    /// durable.
    pub async fn commit(self) -> Result<Commit, Error> {
        let result = async {
            self.connection
                .execute(self.connection.statement(), "COMMIT".into(), Vec::new())
                .await?;
            self.connection.committed().await
        }
        .await;
        self.connection.close().await;
        Ok(match result? {
            Some(token) => Commit::Recorded(token),
            None => Commit::NoChanges,
        })
    }
    /// Discards the transaction.
    pub async fn rollback(self) -> Result<(), Error> {
        let result = self
            .connection
            .execute(self.connection.statement(), "ROLLBACK".into(), Vec::new())
            .await;
        self.connection.close().await;
        result.map(drop)
    }
}
