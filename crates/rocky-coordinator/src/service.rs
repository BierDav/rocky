use crate::CoordinationError;
pub use crate::client::{CALL_DEADLINE, NetworkCoordinator, Observer};
pub use crate::machine::Usage;
pub use crate::machine::{Operation, Outcome, Registration};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A voter's answer to a client request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    /// The leader the voter knows of, so the client asks it next.
    pub leader: Option<u64>,
    /// The outcome.
    pub result: Result<Outcome, CoordinationError>,
}
/// One voter's health and metrics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    /// The voter's id.
    pub node: u64,
    /// The leader it knows of.
    pub leader: Option<u64>,
    /// The last applied log index.
    pub applied: Option<u64>,
    /// The current voters.
    pub voters: BTreeSet<u64>,
    /// The index of its last snapshot.
    pub snapshot: Option<u64>,
    /// A short health label.
    pub health: String,
    /// Log entries held.
    pub logs: usize,
    /// Snapshots held.
    pub snapshots: usize,
    /// Bytes on disk.
    pub disk_bytes: u64,
    /// Bytes allocated on disk.
    pub allocated_bytes: u64,
    /// Accounted bytes of state.
    pub state_bytes: usize,
    /// Remembered request outcomes.
    pub retry_records: usize,
    /// Registered tenants.
    pub tenants: usize,
    /// Whether the voter was restored from older state and holds back from elections.
    pub fenced: bool,
    /// Request counters since start.
    pub metrics: Metrics,
}
/// Request counters of one voter since start.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metrics {
    /// Client requests received.
    pub requests: u64,
    /// Read-only requests answered.
    pub reads: u64,
    /// Requests refused as busy.
    pub busy: u64,
    /// Requests refused as denied.
    pub denied: u64,
    /// Requests that found no quorum.
    pub unavailable: u64,
}
