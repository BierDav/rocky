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
mod bootstrap_intent;
mod claims;
mod configuration;
mod connection;
mod database;
mod delivery;
mod domain;
mod enrollment;
mod error;
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
mod reactor;
mod registry;
mod request;
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
mod store_only;
mod sync;
#[cfg(all(test, not(target_family = "wasm")))]
mod test_directory;
#[cfg(any(test, feature = "test-support"))]
mod testing;
mod tip_schemas;
pub mod transport;

pub use activity::{ActivityEvent, ActivityKind, ActivityPage, ActivityScope, ActivityTransfer};
pub use claims::{ClaimCounters, ClaimGrant};
pub use configuration::Configuration;
pub use connection::{
    ChangeCause, ConnectionOptions, Durability, Page, SqlFailure, TableChange, TableEvents,
};
pub use database::{Connection, CoordinatorSlot, Database, NetworkService, Open, Opened, Session};
pub use delivery::State as DeliveryState;
pub use enrollment::{NodeKeys, PublisherRequired, Supplied, Terms as GrantTerms};
pub use error::{Error, ErrorKind};
pub use inspection::*;
pub use partitions::*;
#[cfg(feature = "qualification")]
pub use reactor::{ReactorMeasurements, ReactorSnapshot, SqlCallTiming};
pub use registry::{
    ClaimRecord as RecordedClaimRight, Grant as RecordedPartitionGrant, Kind as IdentityOrigin,
};
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
pub use invitation::{
    DEFAULT_INVITATION_VALIDITY, Invitation, InvitationId, InvitationRefusal, InviteTerms,
    MAX_OPEN_INVITATIONS, OpenInvitation,
};
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
    FileIdentityProvider, IdentityKey, IdentityProvider, PeerId, PublicKey, Purpose,
    RevocationMode, RootKey,
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
    ContactInfo, DeviceRevocationReceipt, Discovered, DroppedStage, LinkRequest, Provisioning,
    RevocationResult, SeenDevice, SessionInfo, TrustGrant,
};
pub use sql::{DecodeError, FromSqlValue, IntoSqlValue, Row, Rows, Timestamp, Transaction};
pub use status::*;
pub use store_only::StoreOnlyRefusal;
#[cfg(any(test, feature = "test-support"))]
pub use testing::*;
pub type Failure = Box<dyn std::error::Error + Send + Sync>;
pub use self_enrollment::AwaitingLink;

#[cfg(test)]
extern crate self as rocky_sdk;
#[cfg(test)]
#[path = "../tests/integration/support/snapshot.rs"]
mod test_snapshot;
