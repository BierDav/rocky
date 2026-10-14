//! The metadata port of a domain's coordinated log: the order and commit of
//! entries trusted nodes store, never their data. See
//! `docs/runtime/coordinated-log.md` and `docs/runtime/coordinated-writes.md`.
use crate::{Commitment, CoordinationFuture, Scope, commitment};

/// A trusted node's key as its 32 bytes.
pub type NodeKey = [u8; 32];

/// The one assigned, uncommitted entry of a domain.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogPending {
    pub proposer: NodeKey,
    pub hash: Commitment,
    pub position: u64,
    pub epoch: u64,
    /// Absolute authority-time second.
    pub deadline: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogMember {
    pub key: NodeKey,
    pub acknowledged: u64,
}
/// A domain's log metadata as the coordinator holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogHead {
    /// The authenticated anchor before the first entry.
    pub genesis: Commitment,
    pub epoch: u64,
    pub minimum: usize,
    /// The assigned head: `commit`, or the pending entry's position.
    pub head: u64,
    pub commit: u64,
    pub running_hash: Commitment,
    pub members: Vec<LogMember>,
    pub pending: Option<LogPending>,
    /// The newest committed entry hashes, the last at `commit`.
    pub hashes: Vec<Commitment>,
}
impl LogHead {
    /// The committed hash at `position`, while the window still holds it.
    pub fn committed(&self, position: u64) -> Option<Commitment> {
        if position == 0 || position > self.commit {
            return None;
        }
        let back = usize::try_from(self.commit - position).ok()?;
        let index = self.hashes.len().checked_sub(back + 1)?;
        self.hashes.get(index).copied()
    }
    pub fn member(&self, key: &NodeKey) -> bool {
        self.members.iter().any(|member| member.key == *key)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogResult {
    /// The leader holds this node's storage promise; others are missing.
    Acknowledged,
    Committed {
        position: u64,
        running_hash: Commitment,
    },
}

/// Trusted-node calls of a coordinated log. Calls occur outside storage
/// transactions; join and append sequences increase durably per domain
/// and node, and a retried call repeats its sequence and arguments.
pub trait DomainLog: Send + Sync {
    /// This client's node key.
    fn node(&self) -> NodeKey;
    fn log_head(&self, scope: Scope) -> CoordinationFuture<'_, LogHead>;
    /// Enters the in-sync set at the current committed position and hash.
    fn join(
        &self,
        scope: Scope,
        position: u64,
        running_hash: Commitment,
        sequence: u64,
    ) -> CoordinationFuture<'_, u64>;
    fn append(&self, scope: Scope, append: Append) -> CoordinationFuture<'_, LogPending>;
    /// Promises that this node durably stored the exact pending entry.
    fn acknowledge(&self, scope: Scope, pending: LogPending) -> CoordinationFuture<'_, LogResult>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Append {
    pub expected: u64,
    pub epoch: u64,
    pub hash: Commitment,
    pub deadline: u64,
    pub sequence: u64,
}

/// The running hash after committing `hash` at `position`.
pub fn coordinated_chain(previous: Commitment, position: u64, hash: Commitment) -> Commitment {
    let mut bytes = previous.to_vec();
    bytes.extend_from_slice(&position.to_be_bytes());
    bytes.extend_from_slice(&hash);
    commitment("rocky.coordinated-chain.v1", &bytes)
}
