//! An embedded SQLite database that syncs end-to-end encrypted between the people and devices
//! its owner authorizes.
#![deny(missing_docs)]
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
#[cfg_attr(
    any(test, feature = "test-support"),
    expect(
        missing_docs,
        reason = "activity records are fixtures for Rocky's own tests"
    )
)]
mod activity;
mod bootstrap_intent;
mod configuration;
mod connection;
mod database;
mod delivery;
mod domain;
mod enrollment;
mod error;
/// What add-on services such as rocky-coordinator use to verify and sign as this device.
pub mod extension;
mod generation;
mod groups;
mod identities;
/// Held changes and history retention, for Rocky's own tests.
#[cfg(any(test, feature = "test-support"))]
pub mod inspection;
mod invitation;
mod membership;
mod mesh;
mod node;
/// Partition progress, for Rocky's own tests.
#[cfg(any(test, feature = "test-support"))]
pub mod partitions;
#[cfg(not(any(test, feature = "test-support")))]
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
/// The diagnostic detail behind `DatabaseStatus` and `SyncReport`, for Rocky's own tests.
#[cfg(any(test, feature = "test-support"))]
pub mod status;
#[cfg(not(any(test, feature = "test-support")))]
mod status;
mod store_only;
mod sync;
#[cfg(all(test, not(target_family = "wasm")))]
mod test_directory;
#[cfg(any(test, feature = "test-support"))]
#[expect(
    missing_docs,
    reason = "fixtures for Rocky's own tests, not application API"
)]
mod testing;
mod tip_schemas;
/// The transport contract custom transports implement, and an in-process transport.
pub mod transport;

pub use access::Access;
#[cfg(any(test, feature = "test-support"))]
pub use activity::{ActivityEvent, ActivityKind, ActivityPage, ActivityScope, ActivityTransfer};
pub use configuration::Configuration;
pub use connection::{ChangeCause, DroppedTransaction, TableChange, TableEvents};
pub use database::{Created, Database};
#[cfg(any(test, feature = "test-support"))]
pub use delivery::State as DeliveryState;
pub use error::{Blockers, DenyReason, Error, ErrorKind, Limit, Right, Unsupported};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use inspection::*;
pub use invitation::{
    Invitation, InvitationCode, InvitationId, Invite, InviteError, JoinError, PendingInvitation,
};
pub(crate) use partitions::*;
pub use partitions::{CreatedPartition, Partition};
pub use people::{
    AccessError, Device, DeviceId, LinkCode, LinkError, LinkRequest, Logout, Me, Opened,
    PendingLink, Person, PersonId, Removal, ReportedSchema, TableRights,
};
#[cfg(feature = "qualification")]
pub use reactor::{ReactorMeasurements, ReactorSnapshot, SqlCallTiming};

pub use rocky_core::replay::{DropReason, TransactionOutcome};
pub use rocky_core::{Affinity, DeleteAction};
pub use rocky_core::{
    ForeignKeyName, ForeignKeyRule, InvalidName, NameRule, RefusedChange, RoleError, SchemaError,
};
pub use rocky_core::{NotANumber, RowIdentity, SqlValue};
pub use rocky_credentials::{IdentityKey, IdentityProvider, PublicKey, Purpose, RootKey};
pub use rollout::{RemovalImpact, RoleInUse, RolloutConfirmationRequired, RolloutImpact};
pub use schema::{
    Column, ForeignKey, Index, MigrationStep, Role, RoleName, Schema, SchemaBuilder, Table,
};
pub use secrets::{FileSecretStore, SecretStore};
pub use service::{Cause, ContactInfo, Network, ServiceError, StatusChanges, SyncMode};
pub use sql::{
    DecodeError, FromSqlValue, IntoSqlValue, Row, Rows, TRANSACTION_TIMEOUT, Timestamp,
    Transaction, TransactionToken,
};
pub(crate) use status::*;
pub use status::{
    Alert, AlertKind, Blocker, BlockerKind, ClearsBy, DatabaseStatus, DeclaredSchema, EngineStatus,
    FrameReplay, MigrationStatus, PeerStatus, SealSkip, SyncReport, WaitReason,
};
pub use store_only::StoreOnlyRefusal;
#[cfg(any(test, feature = "test-support"))]
pub use testing::*;
pub(crate) type Failure = Box<dyn std::error::Error + Send + Sync>;

#[cfg(test)]
extern crate self as rocky_sdk;
#[cfg(test)]
#[path = "../tests/integration/support/snapshot.rs"]
mod test_snapshot;
