#![recursion_limit = "256"]
#![cfg_attr(
    all(test, target_family = "wasm"),
    expect(
        dead_code,
        unused_imports,
        reason = "native protocol test support is unused in browser tests"
    )
)]
mod access;
mod activity;
mod bootstrap_intent;
mod configuration;
mod connection;
mod database;
mod delivery;
mod domain;
mod enrollment;
mod error;
mod extension;
mod generation;
mod groups;
mod identities;
mod inspection;
mod invitation;
mod membership;
mod mesh;
mod node;
mod partitions;
mod pause;
mod peer;
mod people;
mod reactor;
mod registry;
mod request;
mod rollout;
mod scheduling;
mod schema;
mod secrets;
mod self_enrollment;
mod service;
#[cfg(all(test, not(target_family = "wasm")))]
mod simulation;
mod sql;
mod status;
mod store_only;
mod sync;
#[cfg(all(test, not(target_family = "wasm")))]
mod test_directory;
#[cfg(any(test, feature = "test-support"))]
mod testing;
mod tip_schemas;
pub mod transport;

pub use access::Access;
pub use activity::{ActivityEvent, ActivityKind, ActivityPage, ActivityScope, ActivityTransfer};
pub use configuration::Configuration;
pub use connection::{
    ChangeCause, SqlFailure, SqlReason, TableChange, TableEvents, Unavailability,
};
pub use database::{Created, Database};
pub use delivery::State as DeliveryState;
pub use error::{Error, ErrorKind};
pub use extension::DeviceCredential;
pub use inspection::*;
pub use invitation::{
    Invitation, InvitationCode, InvitationId, Invite, InviteError, JoinError, PendingInvitation,
};
pub use partitions::*;
pub use people::{
    AccessError, AdminRevocation, Device, DeviceId, DeviceInfo, DeviceRevocationReceipt, LinkCode,
    LinkError, LinkRequest, Me, Opened, PendingLink, Person, PersonId, Removal, TableRights,
};
#[cfg(feature = "qualification")]
pub use reactor::{ReactorMeasurements, ReactorSnapshot, SqlCallTiming};

pub use rocky_core::replay::{DropReason, IntentStatement, TransactionOutcome};
pub use rocky_core::{
    Affinity, Collation, ColumnDefinition, DeleteAction, ForeignKeyDefinition, IndexColumn,
    IndexDefinition, LogicalAssertion, LogicalRestriction, LogicalView, TableClass,
    TableDefinition,
};
pub use rocky_core::{
    KeyEpoch, PartitionName, PartitionTuple, ResourceScope, RowIdentity, Scope, SqlValue,
    TransactionToken, WriterId,
};
pub use rocky_core::{RoleError, SchemaError};
pub use rocky_credentials::{
    FileIdentityProvider, IdentityKey, IdentityProvider, PeerId, PublicKey, Purpose, RootKey,
};
pub use rocky_secure::{
    CheckpointStatus, CurrentValue, HeldChange, HeldField, HeldIntent, HeldKind, HeldRow, Holding,
    RetentionLimits, RetentionPolicy,
};
pub use rocky_secure::{PageCache, PartitionCustody, ReplicaRole, RoleConfig as ReplicaRoleConfig};
pub use rollout::{RemovalImpact, RolloutConfirmationRequired, RolloutImpact};
pub use schema::{
    Column, InvalidName, Migration, MigrationStep, Role, RoleName, Schema, SchemaBuilder, Table,
};
pub use secrets::{FileSecretStore, SecretStore};
pub use service::{ContactInfo, Network, ServiceError, StatusChanges, SyncMode};
pub use sql::{DecodeError, FromSqlValue, IntoSqlValue, Row, Rows, Timestamp, Transaction};
pub use status::*;
pub use store_only::StoreOnlyRefusal;
#[cfg(any(test, feature = "test-support"))]
pub use testing::*;
pub type Failure = Box<dyn std::error::Error + Send + Sync>;

#[cfg(test)]
extern crate self as rocky_sdk;
#[cfg(test)]
#[path = "../tests/integration/support/snapshot.rs"]
mod test_snapshot;
