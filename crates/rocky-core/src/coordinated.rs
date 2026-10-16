//! The metadata port of a tip's coordinated log: the order and commit of
//! entries trusted nodes store, never their data. See
//! `docs/runtime/coordinated-log.md` and `docs/runtime/coordinated-writes.md`.
use crate::{Commitment, Configuration, CoordinationFuture, Scope, TipId, commitment};

/// A trusted node's key as its 32 bytes.
pub type NodeKey = [u8; 32];

/// One coordinated log: a domain's log of one tip.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct TipLog {
    pub scope: Scope,
    pub tip: TipId,
}
/// The entry that ends a tip's log: a seal of the tip or a bookmark move off
/// it, as an opaque statement digest. A pending entry it aborts retries in
/// `continuation`'s log, or is held without one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogTerminal {
    pub digest: Commitment,
    pub continuation: Option<TipId>,
}

/// The one assigned, uncommitted entry of a log.
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
/// A tip's log metadata as the coordinator holds it.
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
    /// The latest owner-signed cut, retained so no device must explain it.
    pub cut: Option<LogCut>,
    /// The committed entry at `commit` that ended the log; none while open.
    pub terminal: Option<LogTerminal>,
    /// Encoded `rocky_credentials::TerminalAck`s of the terminal entry, one
    /// per trusted node, as the coordinator keeps them; devices verify them.
    pub acknowledgments: Vec<Vec<u8>>,
}
/// An owner-signed cut as the coordinator retains it: the encoded signed
/// record, whose digest is its entry hash, and the owner's credential chain
/// devices verify. The coordinator checks the record, never the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogCut {
    pub record: Vec<u8>,
    pub authority: Vec<u8>,
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

/// Trusted-node calls of a domain's per-tip coordinated logs. Calls occur
/// outside storage transactions; join and append sequences increase durably
/// per log and node, and a retried call repeats its sequence and arguments.
pub trait DomainLog: Send + Sync {
    /// This client's node key.
    fn node(&self) -> NodeKey;
    fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead>;
    /// Sets the minimum in-sync size a tip's opening names; refused after
    /// the log's first join.
    fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()>;
    /// Enters the in-sync set at the current committed position and hash.
    fn join(
        &self,
        log: TipLog,
        position: u64,
        running_hash: Commitment,
        sequence: u64,
    ) -> CoordinationFuture<'_, u64>;
    fn append(&self, log: TipLog, append: Append) -> CoordinationFuture<'_, LogPending>;
    /// Promises that this node durably stored the exact pending entry.
    fn acknowledge(&self, log: TipLog, pending: LogPending) -> CoordinationFuture<'_, LogResult>;
    /// Submits an owner-signed cut of the exact current head. A retry of the
    /// retained record returns the same commit.
    fn cut(&self, log: TipLog, cut: LogCut) -> CoordinationFuture<'_, LogResult>;
    /// Commits the terminal entry that ends the log, aborting a pending
    /// entry. A retry of the committed terminal returns the same commit;
    /// another one is refused with `Sealed`.
    fn seal(&self, log: TipLog, terminal: LogTerminal) -> CoordinationFuture<'_, LogResult>;
    /// Keeps this node's signed acknowledgment of the terminal entry that
    /// ended `log`. A retry of the same one succeeds; another is refused.
    fn acknowledge_terminal(
        &self,
        log: TipLog,
        acknowledgment: Vec<u8>,
    ) -> CoordinationFuture<'_, ()>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Append {
    pub expected: u64,
    pub epoch: u64,
    pub hash: Commitment,
    pub deadline: u64,
    pub sequence: u64,
}

/// The anchor before the first entry of `log`.
pub fn log_genesis(log: TipLog, configuration: Configuration) -> Commitment {
    let mut bytes = crate::genesis(log.scope, configuration).to_vec();
    bytes.extend_from_slice(&log.tip.0);
    commitment("rocky.coordinated-genesis.v1", &bytes)
}

/// The running hash after committing `hash` at `position`.
pub fn coordinated_chain(previous: Commitment, position: u64, hash: Commitment) -> Commitment {
    let mut bytes = previous.to_vec();
    bytes.extend_from_slice(&position.to_be_bytes());
    bytes.extend_from_slice(&hash);
    commitment("rocky.coordinated-chain.v1", &bytes)
}
