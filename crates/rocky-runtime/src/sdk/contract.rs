use crate::*;
use rocky_core::{AuthorSequence, Commitment, SqlValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 2;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum BoundValue {
    Null,
    Integer(String),
    /// The 16 hexadecimal digits of the double's bits.
    Real(String),
    Text(String),
    Blob(Vec<u8>),
}
impl BoundValue {
    pub(super) fn into_sql(self) -> Result<SqlValue, Error> {
        Ok(match self {
            Self::Null => SqlValue::Null,
            Self::Integer(value) => SqlValue::Integer(
                value
                    .parse()
                    .map_err(|_| Error::Rejected("invalid signed integer".into()))?,
            ),
            Self::Real(value) => SqlValue::Real(
                Some(value.as_str())
                    .filter(|value| value.len() == 16)
                    .and_then(|value| u64::from_str_radix(value, 16).ok())
                    .and_then(rocky_core::Real::from_bits)
                    .ok_or_else(|| Error::Rejected("invalid REAL bits".into()))?,
            ),
            Self::Text(value) => SqlValue::Text(value),
            Self::Blob(value) => SqlValue::Blob(value),
        })
    }
}
impl From<SqlValue> for BoundValue {
    fn from(value: SqlValue) -> Self {
        match value {
            SqlValue::Null => Self::Null,
            SqlValue::Integer(value) => Self::Integer(value.to_string()),
            SqlValue::Real(value) => Self::Real(format!("{:016x}", value.bits())),
            SqlValue::Text(value) => Self::Text(value),
            SqlValue::Blob(value) => Self::Blob(value),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub sql: String,
    pub values: Vec<BoundValue>,
    pub max_rows: usize,
    /// Byte limit of the page; 1 MiB when absent.
    #[serde(default)]
    pub max_bytes: Option<usize>,
    /// `next` of the previous page of the same query.
    #[serde(default)]
    pub cursor: Option<String>,
}
impl Query {
    pub(super) fn bound(self, scope: Scope) -> Result<(BoundQuery, ReadOptions), Error> {
        if self.max_rows == 0
            || self.max_rows > 1024
            || self.sql.len() > 8192
            || self.values.len() > 256
        {
            return Err(Error::Quota {
                resource: "SDK query",
                limit: 1024,
            });
        }
        Ok((
            BoundQuery {
                sql: self.sql,
                values: self
                    .values
                    .into_iter()
                    .map(BoundValue::into_sql)
                    .collect::<Result<_, _>>()?,
            },
            ReadOptions {
                max_bytes: self.max_bytes.unwrap_or(MAX_FRAME_BYTES),
                cursor: self.cursor.as_deref().map(ReadCursor::decode).transpose()?,
                ..ReadOptions::new(scope, self.max_rows)
            },
        ))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRef {
    pub node: NodeId,
    pub sequence: String,
}
impl OperationRef {
    pub(super) fn id(self) -> Result<OperationId, Error> {
        Ok(OperationId {
            node: self.node,
            sequence: AuthorSequence::new(
                self.sequence
                    .parse()
                    .map_err(|_| Error::Rejected("invalid operation sequence".into()))?,
            )?,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Read {
        query: Query,
    },
    Subscribe {
        query: Query,
    },
    Next {
        subscription: String,
    },
    Unsubscribe {
        subscription: String,
    },
    Mutate {
        handle: Commitment,
        entity: Option<OperationRef>,
        payload: Vec<u8>,
        baseline: Option<String>,
    },
    MutationProgress {
        handle: Commitment,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub version: u32,
    pub scope: Scope,
    pub request: Request,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct SdkError {
    pub code: String,
    pub retryable: bool,
    pub message: String,
}
impl From<Error> for SdkError {
    fn from(error: Error) -> Self {
        let code = match error.reason() {
            FailureReason::Denial => "denied",
            FailureReason::UnsupportedGeneration => "unsupported_generation",
            FailureReason::MissingDependencies => "missing_dependencies",
            FailureReason::Quota => "quota",
            FailureReason::Corruption => "corruption",
            FailureReason::Conflict => "conflict",
            FailureReason::Closed => "closed",
            FailureReason::Storage => "storage",
        };
        Self {
            code: code.into(),
            retryable: error.is_retryable(),
            message: error.to_string(),
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Reply {
    Ok { version: u32, value: Value },
    Error { version: u32, error: SdkError },
}
