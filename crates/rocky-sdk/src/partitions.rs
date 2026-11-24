use crate::{Database, SqlValue, request::Request};
#[cfg(any(test, feature = "test-support"))]
use rocky_credentials::PublicKey;
use serde::{Deserialize, Serialize};

/// One partition: a group and the values of the columns its tables are partitioned by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "StoredPartition", from = "StoredPartition")]
pub struct Partition {
    /// The partition group, which is the table's name unless `Table::partition_group` sets one.
    pub group: String,
    /// The partition column values.
    pub values: Vec<SqlValue>,
}
impl Partition {
    /// The partition of `group` with these values.
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
/// A partition `next_partition` created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedPartition {
    /// The row id it is keyed by; insert the row with it.
    pub id: crate::RowIdentity,
    /// The partition.
    pub partition: Partition,
}
/// Whether this device holds one partition it should.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionProgress {
    /// The partition.
    pub partition: Partition,
    /// Whether its key arrived.
    pub held: bool,
    /// Whether its rows arrived.
    pub complete: bool,
}
impl Database {
    /// Allocates a row id in a table partitioned by its own `id` and creates that partition.
    pub async fn next_partition(&self, table: &str) -> Result<CreatedPartition, crate::Error> {
        self.send(|reply| Request::NextPartition(table.into(), reply))
            .await
    }
    /// The keys and deliveries of one partition, for tests.
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

/// The keys and deliveries of one partition.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct PartitionStatus {
    /// The group.
    pub group: String,
    /// The partition's internal address.
    pub address: String,
    /// The key new writes use.
    pub current: Option<rocky_core::KeyEpoch>,
    /// Every key held.
    pub epochs: Vec<rocky_core::KeyEpoch>,
    /// Per table.
    pub tables: Vec<PartitionTableStatus>,
    /// Deliveries of the key to devices.
    pub recipients: Vec<PartitionDelivery>,
}
/// One table of a partition.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct PartitionTableStatus {
    /// The table.
    pub table: String,
    /// Its synced scope.
    pub scope: Option<rocky_core::ResourceScope>,
    /// Every key held.
    pub epochs: Vec<rocky_core::KeyEpoch>,
    /// The key new writes use.
    pub current: Option<rocky_core::KeyEpoch>,
    /// Whether this device syncs it.
    pub subscribed: bool,
    /// Whether this device holds its rows, by a subscription or as a full replica.
    pub held: bool,
}
/// A partition key's delivery to one device.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct PartitionDelivery {
    /// The device, once known.
    pub node: Option<PublicKey>,
    /// The recipient.
    pub reference: rocky_credentials::PeerId,
    /// The key.
    pub epoch: rocky_core::KeyEpoch,
    /// How far delivery got.
    pub state: crate::DeliveryState,
}
