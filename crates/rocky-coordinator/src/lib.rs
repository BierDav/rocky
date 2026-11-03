//! Trusted metadata coordination: a certified persistent Raft service and
//! the coordinated log. Application payloads and policy remain outside it.
//! The protocol, the state machine and the client build on every target;
//! the voter (openraft, its file store and sockets) is native only.
pub use retention::Limits;
use rocky_core::*;

/// Retained state of one coordinator, by count and accounted bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    pub domains: usize,
    pub bytes: usize,
}

/// Certified bootstrap and authenticated durable service.
pub mod certified;
mod client;
// The authority state machine is the voter's. It builds on every target so
// its tests also run in a browser, where nothing applies it yet.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
mod machine;
#[cfg(not(target_family = "wasm"))]
mod raft_store;
mod retention;
pub mod service;
mod transport;

/// Ordered metadata for coordinated ordinary writes.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub mod coordinated;
/// Named counters with idempotent claims.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub mod counters;
