//! ORM-neutral native SDK. Generated ABI scaffolding is isolated in sdks/native.
mod activity;
mod backup;
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
mod status;
mod sync;
#[cfg(test)]
mod test_directory;
mod tips;
pub mod transport;
mod typed;

pub use backup::{Entry as BackupEntry, Inventory as BackupInventory};
pub use connection::{ConnectionOptions, Page, Profile, SqlFailure, TableChange, TableEvents};
pub use custody::CustodyPeer;
pub use database::{Connection, Coordination, Database, NetworkService, Open, Opened, Session};
pub use engine::Engine;
pub use enrollment::{DeviceKeys, Supplied, route_enrollment};
/// The client of a coordination service and the bundle that pins it.
#[cfg(feature = "iroh")]
pub use rocky_coordinator::{certified::TrustBundle, service::NetworkCoordinator};
pub use rocky_core::{NodeId, PrincipalId, Scope, SqlValue};
/// Signed credentials, grant profiles and the key provider contract.
pub use rocky_credentials as credentials;
/// The identity port an application supplies, and a file-backed provider.
pub use rocky_credentials::{FileIdentityProvider, IdentityProvider, Purpose};
pub use rollout::{RemovalImpact, RolloutConfirmationRequired, RolloutImpact};
pub use secrets::{FileSecretStore, SecretStore};
pub use service::{Network, ServiceError};
pub use status::*;
pub use tips::*;
pub use typed::{Access, Located, MAX_OBSERVED, TypedApp, TypedError, TypedRow};
pub type Failure = Box<dyn std::error::Error + Send + Sync>;
/// Rocky's handlers for an application-owned iroh endpoint, and the iroh
/// version they attach to.
#[cfg(feature = "iroh")]
pub use rocky_iroh as iroh;
