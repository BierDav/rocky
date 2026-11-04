//! The coordination service's client-facing types and its client, on every
//! target. The voter (Raft, its file store and sockets) is native only.
pub use crate::client::{NetworkCoordinator, Observer};
pub use crate::machine::Usage;
pub use crate::machine::{Operation, Outcome, Registration};
use rocky_core::CoordinationError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[cfg(not(target_family = "wasm"))]
pub use voter::{Backup, Fence, VoterService, backup, restore};

#[cfg(not(target_family = "wasm"))]
mod voter;

/// How old committed authority time may be for a read to skip the log.
pub const READ_STALENESS: u64 = 30;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Reply {
    pub(crate) leader: Option<u64>,
    pub(crate) result: Result<Outcome, CoordinationError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub node: u64,
    pub leader: Option<u64>,
    pub applied: Option<u64>,
    pub voters: BTreeSet<u64>,
    pub snapshot: Option<u64>,
    pub health: String,
    /// Retained consensus and state machine footprint of this voter.
    pub logs: usize,
    pub snapshots: usize,
    pub disk_bytes: u64,
    pub allocated_bytes: u64,
    pub state_bytes: usize,
    pub retry_records: usize,
    pub tenants: usize,
    /// Restored and not yet re-added: it neither votes nor serves clients.
    pub fenced: bool,
    pub metrics: Metrics,
}
/// Counters since this voter started. They count requests, never their content.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metrics {
    /// Client requests answered, and those answered from a read barrier.
    pub requests: u64,
    pub reads: u64,
    /// Refused for a full voter connection, client or tenant lane.
    pub busy: u64,
    /// Answered `Denied`, and answered without an authoritative result.
    pub denied: u64,
    pub unavailable: u64,
}
