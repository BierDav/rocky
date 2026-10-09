//! ORM-neutral native SDK. Generated ABI scaffolding is isolated in sdks/native.
mod activity;
mod backup;
mod commands;
mod connection;
mod custody;
mod database;
mod domain;
mod engine;
mod enrollment;
mod generation;
mod groups;
mod node;
mod owner;
mod registry;
mod scheduling;
mod schema;
mod secrets;
mod service;
mod sync;
pub mod transport;
mod typed;

pub use backup::{Entry as BackupEntry, Inventory as BackupInventory};
pub use commands::{
    CommandHandler, Commands, Coordination, HANDLER_DEADLINE, handler_get_json, handler_input_json,
    handler_output_json, handler_scan_json,
};
pub use connection::{ConnectionOptions, Page, Profile, SqlFailure, TableChange, TableEvents};
pub use custody::CustodyPeer;
pub use database::{Connection, Database, NetworkService, Open, Opened, Session};
pub use engine::Engine;
pub use enrollment::{DeviceKeys, Supplied, route_enrollment};
/// The client of a coordination service and the bundle that pins it.
#[cfg(feature = "iroh")]
pub use rocky_coordinator::{certified::TrustBundle, service::NetworkCoordinator};
/// The coordinator port and the typed values handlers decide over.
pub use rocky_core::{
    Coordinator, HandlerInput, HandlerOutput, HandlerReads, NodeId, Outcome, PrincipalId, Scope,
    SqlValue, TableWrite,
};
/// Signed credentials, grant profiles and the key provider contract.
pub use rocky_credentials as credentials;
pub use secrets::{FileSecretStore, SecretStore};
pub use service::{Network, ServiceError};
pub use typed::{Access, Located, MAX_OBSERVED, TypedApp, TypedError, TypedRow};
pub type Failure = Box<dyn std::error::Error + Send + Sync>;
/// Rocky's handlers for an application-owned iroh endpoint, and the iroh
/// version they attach to.
#[cfg(feature = "iroh")]
pub use rocky_iroh as iroh;

pub use rocky_core::Activation;
