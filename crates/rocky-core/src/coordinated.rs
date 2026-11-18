use crate::{Commitment, Configuration, CoordinationFuture, MeshId, Scope, TipId, commitment};

pub type NodeKey = [u8; 32];

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct CoordinatorIdentity {
    pub root: NodeKey,
    pub mesh: MeshId,
    pub genesis: Commitment,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct TipLog {
    pub scope: Scope,
    pub tip: TipId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogTerminal {
    pub digest: Commitment,
    pub continuation: Option<TipId>,
    pub confirms: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogPending {
    pub proposer: NodeKey,
    pub hash: Commitment,
    pub position: u64,
    pub epoch: u64,
    pub deadline: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogMember {
    pub key: NodeKey,
    pub acknowledged: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogHead {
    pub genesis: Commitment,
    pub epoch: u64,
    pub minimum: usize,
    pub head: u64,
    pub commit: u64,
    pub running_hash: Commitment,
    pub members: Vec<LogMember>,
    pub pending: Option<LogPending>,
    pub hashes: Vec<Commitment>,
    pub terminal: Option<LogTerminal>,
    pub acknowledgments: Vec<Vec<u8>>,
    pub closed: bool,
    pub ended_dropped: bool,
}
impl LogHead {
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
    Acknowledged,
    Committed {
        position: u64,
        running_hash: Commitment,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogStatus {
    pub decision: LogDecision,
    pub members: Vec<LogMember>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LogDecision {
    Pending,
    Committed,
    Aborted,
    Unknown,
}
impl LogHead {
    pub fn status(&self, position: u64, hash: Commitment) -> LogStatus {
        let decision = if self
            .pending
            .as_ref()
            .is_some_and(|p| p.position == position && p.hash == hash)
        {
            LogDecision::Pending
        } else {
            match self.committed(position) {
                Some(committed) if committed == hash => LogDecision::Committed,
                Some(_) => LogDecision::Aborted,
                None if position > self.commit => LogDecision::Aborted,
                None => LogDecision::Unknown,
            }
        };
        LogStatus {
            decision,
            members: self.members.clone(),
        }
    }
}

pub trait DomainLog: Send + Sync {
    fn node(&self) -> NodeKey;
    fn identity(&self) -> CoordinatorIdentity;
    fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead>;
    fn pending_status(
        &self,
        log: TipLog,
        position: u64,
        hash: Commitment,
    ) -> CoordinationFuture<'_, LogStatus>;
    fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()>;
    fn join(
        &self,
        log: TipLog,
        position: u64,
        running_hash: Commitment,
        sequence: u64,
    ) -> CoordinationFuture<'_, u64>;
    fn append(&self, log: TipLog, append: Append) -> CoordinationFuture<'_, LogPending>;
    fn acknowledge(&self, log: TipLog, pending: LogPending) -> CoordinationFuture<'_, LogResult>;
    fn seal(&self, log: TipLog, terminal: LogTerminal) -> CoordinationFuture<'_, LogResult>;
    fn recover(&self, log: TipLog, seal: Vec<u8>) -> CoordinationFuture<'_, LogResult>;
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
    pub duration: u64,
    pub sequence: u64,
}

pub fn log_genesis(log: TipLog, configuration: Configuration) -> Commitment {
    let mut bytes = crate::genesis(log.scope, configuration).to_vec();
    bytes.extend_from_slice(&log.tip.0);
    commitment("rocky.coordinated-genesis.v1", &bytes)
}

pub fn coordinated_chain(previous: Commitment, position: u64, hash: Commitment) -> Commitment {
    let mut bytes = previous.to_vec();
    bytes.extend_from_slice(&position.to_be_bytes());
    bytes.extend_from_slice(&hash);
    commitment("rocky.coordinated-chain.v1", &bytes)
}
