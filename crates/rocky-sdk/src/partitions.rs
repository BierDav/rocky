use crate::{Database, SqlValue, request::Request};
#[cfg(any(test, feature = "test-support"))]
use rocky_credentials::PublicKey;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "StoredPartition", from = "StoredPartition")]
pub struct Partition {
    pub group: String,
    pub values: Vec<SqlValue>,
}
impl Partition {
    pub fn new(group: impl Into<String>, values: Vec<SqlValue>) -> Self {
        Self {
            group: group.into(),
            values,
        }
    }
    pub(crate) fn target(&self) -> PartitionTarget {
        PartitionTarget::Group {
            group: self.group.clone(),
            values: self.values.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredValue {
    Null,
    Integer(i64),
    Real(u64),
    Text(String),
    Blob(Vec<u8>),
}
impl StoredValue {
    fn of(value: &SqlValue) -> Self {
        match value {
            SqlValue::Null => Self::Null,
            SqlValue::Integer(value) => Self::Integer(*value),
            SqlValue::Real(value) => Self::Real(value.bits()),
            SqlValue::Text(value) => Self::Text(value.clone()),
            SqlValue::Blob(value) => Self::Blob(value.clone()),
        }
    }
    fn value(&self) -> SqlValue {
        match self {
            Self::Null => SqlValue::Null,
            Self::Integer(value) => SqlValue::Integer(*value),
            Self::Real(bits) => {
                rocky_core::Real::from_bits(*bits).map_or(SqlValue::Null, SqlValue::Real)
            }
            Self::Text(value) => SqlValue::Text(value.clone()),
            Self::Blob(value) => SqlValue::Blob(value.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredPartition {
    group: String,
    values: Vec<StoredValue>,
}
impl From<Partition> for StoredPartition {
    fn from(partition: Partition) -> Self {
        Self::of(&partition)
    }
}
impl From<StoredPartition> for Partition {
    fn from(stored: StoredPartition) -> Self {
        stored.partition()
    }
}
impl StoredPartition {
    pub(crate) fn of(partition: &Partition) -> Self {
        Self {
            group: partition.group.clone(),
            values: partition.values.iter().map(StoredValue::of).collect(),
        }
    }
    pub(crate) fn partition(&self) -> Partition {
        Partition::new(
            self.group.clone(),
            self.values.iter().map(StoredValue::value).collect(),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PartitionTarget {
    Group {
        group: String,
        values: Vec<SqlValue>,
    },
    Table {
        table: String,
        values: Vec<SqlValue>,
    },
}
impl PartitionTarget {
    pub(crate) fn parts(&self) -> (Option<&str>, Option<&str>, &[SqlValue]) {
        match self {
            Self::Group { group, values } => (Some(group), None, values),
            Self::Table { table, values } => (None, Some(table), values),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedPartition {
    pub id: crate::RowIdentity,
    pub partition: Partition,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionProgress {
    pub partition: Partition,
    pub held: bool,
    pub complete: bool,
}
impl Database {
    pub async fn next_partition(&self, table: &str) -> Result<CreatedPartition, crate::Error> {
        self.send(|reply| Request::NextPartition(table.into(), reply))
            .await
    }
    #[cfg(any(test, feature = "test-support"))]
    pub async fn partition_status(
        &self,
        partition: &Partition,
    ) -> Result<PartitionStatus, crate::Error> {
        let target = partition.target();
        self.send(|reply| Request::PartitionStatus(target, reply))
            .await
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct PartitionStatus {
    pub group: String,
    pub address: String,
    pub current: Option<rocky_core::KeyEpoch>,
    pub epochs: Vec<rocky_core::KeyEpoch>,
    pub tables: Vec<PartitionTableStatus>,
    pub recipients: Vec<PartitionDelivery>,
}
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct PartitionTableStatus {
    pub table: String,
    pub scope: Option<rocky_core::ResourceScope>,
    pub epochs: Vec<rocky_core::KeyEpoch>,
    pub current: Option<rocky_core::KeyEpoch>,
    pub holding: Option<rocky_secure::Holding>,
}
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct PartitionDelivery {
    pub node: Option<PublicKey>,
    pub reference: rocky_credentials::PeerId,
    pub epoch: rocky_core::KeyEpoch,
    pub state: crate::DeliveryState,
}
