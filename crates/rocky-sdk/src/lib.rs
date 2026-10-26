//! Official Rust SDK for Rocky DB: secure local storage and optional peer sync.
//!
//! Start with [`Schema::builder`], [`Configuration`] and [`Database::transaction`].
//! See `examples/notes.rs` for a complete application without an ORM.
mod activity;
mod backup;
mod configuration;
mod connection;
mod custody;
mod database;
mod delivery;
mod domain;
mod engine;
mod enrollment;
mod generation;
mod groups;
mod identities;
mod mesh;
mod node;
mod owner;
mod registry;
mod rollout;
mod scheduling;
mod schema;
mod secrets;
mod self_enrollment;
mod service;
mod session;
#[cfg(test)]
mod simulation;
mod sql;
mod status;
mod sync;
#[cfg(test)]
mod test_directory;
mod tip_schemas;
mod tips;
pub mod transport;
mod typed;

pub use backup::{Entry as BackupEntry, Inventory as BackupInventory};
pub use configuration::Configuration;
pub use connection::{
    Binding, ConnectionOptions, Durability, Page, Profile, SqlFailure, TableChange, TableEvents,
};
pub use custody::CustodyPeer;
pub use database::{Connection, Coordination, Database, NetworkService, Open, Opened, Session};
pub use engine::Engine;
pub use enrollment::{DeviceKeys, Supplied, route_enrollment};
/// The client of a coordination service and the bundle that pins it.
#[cfg(feature = "iroh")]
pub use rocky_coordinator::{certified::TrustBundle, service::NetworkCoordinator};
pub use rocky_core::{
    Affinity, Collation, ColumnDefinition, ConflictPolicy, DeleteAction, ForeignKeyDefinition,
    IndexColumn, IndexDefinition, LogicalView, TableClass, TableDefinition, UpdatePolicy,
};
pub use rocky_core::{NodeId, PrincipalId, Scope, SqlValue};
/// Signed credentials, grant profiles and the key provider contract.
pub use rocky_credentials as credentials;
pub use rocky_credentials::Profile as GrantProfile;
/// The identity port an application supplies, and a file-backed provider.
pub use rocky_credentials::{FileIdentityProvider, IdentityProvider, Purpose};
pub use rollout::{RemovalImpact, RolloutConfirmationRequired, RolloutImpact};
pub use schema::{Column, Migration, MigrationStep, Schema, SchemaBuilder, Table};
pub use secrets::{FileSecretStore, SecretStore};
pub use service::{Network, ServiceError};
pub use session::Invitation;
pub use sql::{DecodeError, FromSqlValue, IntoSqlValue, Row, Rows, Transaction};
pub use status::*;
pub use tips::*;
pub use typed::{Access, Located, MAX_OBSERVED, TypedApp, TypedError, TypedRow};
pub type Failure = Box<dyn std::error::Error + Send + Sync>;
/// Rocky's handlers for an application-owned iroh endpoint, and the iroh
/// version they attach to.
#[cfg(feature = "iroh")]
pub use rocky_iroh as iroh;
