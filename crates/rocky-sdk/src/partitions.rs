use crate::{Database, SqlValue, request::Request};
use rocky_credentials::{CredentialChain, PublicKey};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq)]
pub enum PartitionTarget {
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
#[derive(Clone, Debug, Serialize)]
pub struct PreparedPartition {
    pub address: String,
    pub epoch: rocky_core::KeyEpoch,
    pub name: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct CreatedPartition {
    pub id: rocky_core::RowIdentity,
    pub scope: rocky_core::ResourceScope,
    #[serde(flatten)]
    pub partition: PreparedPartition,
}
#[derive(Clone, Debug, Serialize)]
pub struct PartitionEnvelope {
    pub recipient: PublicKey,
    pub envelope: Vec<u8>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PartitionShare {
    pub epoch: rocky_core::KeyEpoch,
    pub recipients: BTreeSet<PublicKey>,
    pub envelopes: Vec<PartitionEnvelope>,
    pub sender: Option<CredentialChain>,
}
impl Database {
    pub async fn set_partition_custody(
        &self,
        scope: rocky_core::ResourceScope,
        policy: crate::PartitionCustody,
    ) -> Result<(), crate::Error> {
        self.send(|reply| Request::PartitionCustody(scope, policy, reply))
            .await
    }
    pub async fn set_replica_role(
        &self,
        role: crate::ReplicaRoleConfig,
    ) -> Result<(), crate::Error> {
        self.send(|reply| Request::ReplicaRole(role, reply)).await
    }
    pub async fn next_partition(&self, table: &str) -> Result<CreatedPartition, crate::Error> {
        self.send(|reply| Request::NextPartition(table.into(), reply))
            .await
    }
    pub async fn partition_recipients(
        &self,
        target: PartitionTarget,
    ) -> Result<Vec<PublicKey>, crate::Error> {
        self.send(|reply| Request::PartitionRecipients(target, reply))
            .await
    }
    pub async fn share_partition(
        &self,
        target: PartitionTarget,
        recipients: BTreeSet<PublicKey>,
        epoch: Option<rocky_core::KeyEpoch>,
    ) -> Result<PartitionShare, crate::Error> {
        self.send(|reply| Request::SharePartition(target, recipients, epoch, reply))
            .await
    }
    pub async fn grant_partition(
        &self,
        identity: PublicKey,
        target: PartitionTarget,
    ) -> Result<PartitionShare, crate::Error> {
        self.send(|reply| Request::GrantPartition(identity, target, reply))
            .await
    }
    pub async fn identity_for_node(
        &self,
        node: PublicKey,
    ) -> Result<Option<PublicKey>, crate::Error> {
        self.send(|reply| Request::IdentityForNode(node, reply))
            .await
    }
}

#[derive(Clone, Debug)]
pub struct PartitionAddress {
    pub scope: rocky_core::ResourceScope,
    pub tuple: rocky_core::PartitionTuple,
    pub group: String,
    pub address: String,
    pub name: rocky_core::PartitionName,
}
#[derive(Clone, Debug)]
pub struct PartitionStatus {
    pub group: String,
    pub address: String,
    pub current: Option<rocky_core::KeyEpoch>,
    pub epochs: Vec<rocky_core::KeyEpoch>,
    pub tables: Vec<PartitionTableStatus>,
    pub recipients: Vec<PartitionDelivery>,
}
#[derive(Clone, Debug)]
pub struct PartitionTableStatus {
    pub table: String,
    pub scope: Option<rocky_core::ResourceScope>,
    pub epochs: Vec<rocky_core::KeyEpoch>,
    pub current: Option<rocky_core::KeyEpoch>,
    pub holding: Option<rocky_secure::Holding>,
}
#[derive(Clone, Debug)]
pub struct PartitionDelivery {
    pub node: Option<PublicKey>,
    pub reference: rocky_credentials::PeerId,
    pub epoch: rocky_core::KeyEpoch,
    pub state: crate::DeliveryState,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PartitionEviction {
    pub rows: u64,
    pub operations: u64,
    pub checkpoints: u64,
    pub recovery_unconfirmed: bool,
}
#[derive(Clone, Debug, Default)]
pub struct PartitionSelection {
    pub name: Option<rocky_core::PartitionName>,
    pub omit: BTreeSet<String>,
}
impl Database {
    pub async fn partition_address(
        &self,
        table: &str,
        values: Vec<SqlValue>,
    ) -> Result<PartitionAddress, crate::Error> {
        self.send(|reply| Request::PartitionAddress(table.into(), values, reply))
            .await
    }
    pub async fn partition_complete(&self, table: &str) -> Result<bool, crate::Error> {
        self.send(|reply| Request::PartitionComplete(table.into(), reply))
            .await
    }
    pub async fn create_partition(
        &self,
        target: PartitionTarget,
    ) -> Result<PreparedPartition, crate::Error> {
        self.send(|reply| Request::CreatePartition(target, reply))
            .await
    }
    pub async fn install_partition(
        &self,
        target: PartitionTarget,
        envelope: rocky_credentials::GroupSecretEnvelope,
        sender: CredentialChain,
    ) -> Result<(), crate::Error> {
        self.send(|reply| Request::InstallPartition(target, Box::new(envelope), sender, reply))
            .await
    }
    pub async fn partition_status(
        &self,
        target: PartitionTarget,
    ) -> Result<PartitionStatus, crate::Error> {
        self.send(|reply| Request::PartitionStatus(target, reply))
            .await
    }
    pub async fn register_partition(
        &self,
        target: PartitionTarget,
        selection: PartitionSelection,
    ) -> Result<(), crate::Error> {
        self.send(|reply| Request::RegisterPartition(target, selection, reply))
            .await
    }
    pub async fn subscribe_partition(
        &self,
        target: PartitionTarget,
        selection: PartitionSelection,
    ) -> Result<(), crate::Error> {
        self.send(|reply| Request::HoldPartition(target, selection, false, reply))
            .await
    }
    pub async fn bootstrap_partition(
        &self,
        target: PartitionTarget,
        selection: PartitionSelection,
    ) -> Result<(), crate::Error> {
        self.send(|reply| Request::HoldPartition(target, selection, true, reply))
            .await
    }
    pub async fn unsubscribe_partition(
        &self,
        target: PartitionTarget,
        omit: BTreeSet<String>,
    ) -> Result<(), crate::Error> {
        self.send(|reply| Request::ReleasePartition(target, omit, reply))
            .await
    }
    pub async fn evict_partition(
        &self,
        target: PartitionTarget,
        omit: BTreeSet<String>,
    ) -> Result<PartitionEviction, crate::Error> {
        self.send(|reply| Request::EvictPartition(target, omit, reply))
            .await
    }
}
