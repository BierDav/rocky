#![cfg(not(target_family = "wasm"))]
mod local;
pub mod provision;
mod raft_store;
mod service;
mod transport;

pub use local::LocalCluster;
pub use service::{Backup, Fence, ServeError, VoterService, backup, restore};

pub type Failure = Box<dyn std::error::Error + Send + Sync>;
