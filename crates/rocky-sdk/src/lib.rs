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
mod extension;
mod generation;
mod groups;
mod identities;
/// Held changes and history retention.
pub mod inspection;
mod invitation;
mod membership;
mod mesh;
mod node;
/// Partition progress and key deliveries.
pub mod partitions;
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
/// The reports `Database::status` and `sync_now` return.
pub mod status;
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
pub use error::{DenyReason, Error, ErrorKind, Limit, Right, Unsupported};
pub use extension::DeviceCredential;
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
pub use rocky_core::{Affinity, DeleteAction, ForeignKeyDefinition, IndexColumn, IndexDefinition};
pub use rocky_core::{InvalidName, NameRule, RoleError, SchemaError};
pub use rocky_core::{NotANumber, RowIdentity, Scope, SqlValue};
pub use rocky_credentials::{
    FileIdentityProvider, IdentityKey, IdentityProvider, PeerId, PublicKey, Purpose, RootKey,
};
pub use rollout::{RemovalImpact, RoleInUse, RolloutConfirmationRequired, RolloutImpact};
pub use schema::{Column, MigrationStep, Role, RoleName, Schema, SchemaBuilder, Table};
pub use secrets::{FileSecretStore, SecretStore};
pub use service::{ContactInfo, Network, ServiceError, StatusChanges, SyncMode};
pub use sql::{
    Commit, DecodeError, FromSqlValue, IntoSqlValue, Row, Rows, Timestamp, Transaction,
    TransactionToken,
};
pub(crate) use status::*;
pub use store_only::StoreOnlyRefusal;
#[cfg(any(test, feature = "test-support"))]
pub use testing::*;
pub(crate) type Failure = Box<dyn std::error::Error + Send + Sync>;

#[cfg(test)]
extern crate self as rocky_sdk;
#[cfg(test)]
#[path = "../tests/integration/support/snapshot.rs"]
mod test_snapshot;
