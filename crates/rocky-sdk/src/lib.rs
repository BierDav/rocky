#![recursion_limit = "256"]
#![cfg_attr(
    all(test, target_family = "wasm"),
    expect(
        dead_code,
        unused_imports,
        reason = "native protocol test support is unused in browser tests"
    )
)]
mod activity;
mod backup;
mod bootstrap_intent;
mod claims;
mod configuration;
mod connection;
#[cfg(not(target_family = "wasm"))]
mod custody;
mod database;
mod delivery;
mod domain;
mod enrollment;
mod error;
mod escrow;
mod generation;
mod groups;
mod identities;
mod inspection;
mod membership;
mod mesh;
mod node;
mod partitions;
mod pause;
mod peer;
mod personal;
mod reactor;
mod recovery;
mod registry;
mod request;
mod reset;
mod rollout;
mod scheduling;
mod schema;
mod secrets;
mod self_enrollment;
mod service;
mod session;
#[cfg(all(test, not(target_family = "wasm")))]
mod simulation;
mod sql;
mod status;
mod sync;
#[cfg(all(test, not(target_family = "wasm")))]
mod test_directory;
mod tip_schemas;
mod tips;
pub mod transport;

pub use activity::{ActivityEvent, ActivityKind, ActivityPage, ActivityScope, ActivityTransfer};
pub use backup::{Entry as BackupEntry, Inventory as BackupInventory};
pub use claims::{ClaimCounters, ClaimGrant};
pub use configuration::Configuration;
pub use connection::{
    Binding, ChangeCause, ConnectionOptions, Durability, Page, SqlFailure, TableChange, TableEvents,
};
#[cfg(not(target_family = "wasm"))]
pub use custody::{CustodyConfiguration, CustodyPeer};
pub use database::{Connection, CoordinatorSlot, Database, NetworkService, Open, Opened, Session};
pub use delivery::State as DeliveryState;
pub use enrollment::{
    NodeKeys, PublisherRequired, Supplied, Terms as GrantTerms, route_enrollment,
};
pub use error::{Error, ErrorKind};
pub use inspection::*;
pub use partitions::*;
pub use personal::{
    PersonalOpened, PersonalSignInError, RestoreCoverage, SignInSource, VerifiedBackup,
};
#[cfg(feature = "qualification")]
pub use reactor::{ReactorMeasurements, ReactorSnapshot, SqlCallTiming};
pub use registry::{
    ClaimRecord as RecordedClaimRight, Grant as RecordedPartitionGrant, Kind as IdentityOrigin,
};
pub(crate) use reset::ResetRefused;
pub use reset::{Freshness, ResetKeep, ResetReceipt, RestoreChoice};
pub use rocky_credentials::counters;
pub use rocky_credentials::{Coordinator, CoordinatorBundle, CoordinatorConnector};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoordinatorNotConfigured;
impl std::fmt::Display for CoordinatorNotConfigured {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("coordinator not configured")
    }
}
impl std::error::Error for CoordinatorNotConfigured {}
pub use rocky_core::CoordinationError;
pub use rocky_core::replay::{DropReason, TransactionOutcome};
pub use rocky_core::{
    Affinity, Collation, ColumnDefinition, DeleteAction, ForeignKeyDefinition, IndexColumn,
    IndexDefinition, LogicalAssertion, LogicalRestriction, LogicalView, TableClass,
    TableDefinition,
};
pub use rocky_core::{
    Commitment, KeyEpoch, NodeId, PartitionName, PartitionTuple, PrincipalId, ResourceScope,
    RowIdentity, Scope, SqlValue, TipId, TransactionToken, WriterId,
};
pub use rocky_credentials as credentials;
pub use rocky_credentials::Profile as GrantProfile;
pub use rocky_credentials::{
    DatabaseKind, DiscoveryCapability, FileIdentityProvider, IdentityKey, IdentityProvider, PeerId,
    PersonalAccount, PersonalError, PersonalLocator, PersonalNamespace, PrfInput, PublicKey,
    Purpose, RevocationMode, RootKey, VerifiedPasskeyPrf,
};
pub use rocky_secure::{
    CheckpointStatus, CurrentValue, HeldChange, HeldField, HeldKind, HeldRow, Holding,
    RetentionLimits, RetentionPolicy,
};
pub use rocky_secure::{PartitionCustody, ReplicaRole, RoleConfig as ReplicaRoleConfig};
pub use rollout::{RemovalImpact, RolloutConfirmationRequired, RolloutImpact};
pub use schema::{Column, Migration, MigrationStep, Schema, SchemaBuilder, Table};
pub use secrets::{FileSecretStore, SecretStore};
pub use service::{Network, ServiceError};
pub use session::{
    ContactInfo, Discovered, DroppedStage, LinkRequest, Provisioning, RevocationResult, SeenDevice,
    SessionInfo,
};
pub use sql::{DecodeError, FromSqlValue, IntoSqlValue, Row, Rows, Timestamp, Transaction};
pub use status::*;
pub use tips::*;
pub type Failure = Box<dyn std::error::Error + Send + Sync>;
pub use self_enrollment::AwaitingLink;

#[cfg(test)]
extern crate self as rocky_sdk;
#[cfg(test)]
#[path = "../tests/integration/support/snapshot.rs"]
mod test_snapshot;

pub use personal::devices::{DeviceRevocationReceipt, PersonalDeviceNotice};
