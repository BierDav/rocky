use crate::CoordinationError;
pub use crate::client::{NetworkCoordinator, Observer};
pub use crate::machine::Usage;
pub use crate::machine::{Operation, Outcome, Registration};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub leader: Option<u64>,
    pub result: Result<Outcome, CoordinationError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub node: u64,
    pub leader: Option<u64>,
    pub applied: Option<u64>,
    pub voters: BTreeSet<u64>,
    pub snapshot: Option<u64>,
    pub health: String,
    pub logs: usize,
    pub snapshots: usize,
    pub disk_bytes: u64,
    pub allocated_bytes: u64,
    pub state_bytes: usize,
    pub retry_records: usize,
    pub tenants: usize,
    pub fenced: bool,
    pub metrics: Metrics,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metrics {
    pub requests: u64,
    pub reads: u64,
    pub busy: u64,
    pub denied: u64,
    pub unavailable: u64,
}
