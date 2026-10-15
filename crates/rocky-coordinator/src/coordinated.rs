//! Metadata-only domain log. Storage acknowledgments are signed by trusted nodes.
use crate::{certified::encode, machine::Request};
use p2panda_core::PublicKey;
use rocky_core::{Commitment, CoordinationError, MAX_COUNTER, Scope, commitment};
use rocky_credentials::{CutPoint, MAX_CUT_AUTHORITY_BYTES, SignedCut};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const HASH_WINDOW: usize = 1024;
/// Fixed admission caps of the current log machine, in addition to tenant Usage budgets.
pub const TRUSTED_NODES: usize = 32;
pub const APPENDS_PER_MINUTE: u32 = 60;
pub const ACKNOWLEDGMENT_DEADLINE: u64 = 30;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Action {
    Head,
    /// Owner policy; immutable after the first join.
    Configure {
        minimum: usize,
    },
    Join {
        position: u64,
        running_hash: Commitment,
        sequence: u64,
    },
    Append {
        expected: u64,
        epoch: u64,
        hash: Commitment,
        deadline: u64,
        sequence: u64,
    },
    /// Volatile leader knowledge. The node must have durably stored this exact entry.
    Acknowledge {
        pending: Pending,
    },
    /// Voter-authored resolution with the original signed node acknowledgments.
    Resolve {
        pending: Pending,
        acknowledgments: Vec<Request>,
    },
    /// An owner-signed `rocky_credentials::SignedCut` of the exact head,
    /// with the owner's credential chain for devices.
    Cut {
        #[serde(with = "bytes")]
        record: Vec<u8>,
        #[serde(with = "bytes")]
        authority: Vec<u8>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pending {
    pub proposer: PublicKey,
    pub hash: Commitment,
    pub position: u64,
    pub epoch: u64,
    pub deadline: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub key: PublicKey,
    /// The identity that certified the node; its own key without one.
    pub identity: PublicKey,
    pub acknowledged: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Head {
    pub genesis: Commitment,
    pub epoch: u64,
    pub minimum: usize,
    pub head: u64,
    pub commit: u64,
    pub running_hash: Commitment,
    pub members: Vec<Member>,
    pub pending: Option<Pending>,
    #[serde(with = "hashes")]
    pub hashes: VecDeque<Commitment>,
    /// The latest admitted cut; its digest is the entry hash at `position`.
    pub cut: Option<Box<Cut>>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cut {
    #[serde(with = "bytes")]
    pub record: Vec<u8>,
    #[serde(with = "bytes")]
    pub authority: Vec<u8>,
    pub digest: Commitment,
    pub sequence: u64,
    pub position: u64,
    pub running_hash: Commitment,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Result {
    Joined {
        epoch: u64,
    },
    Assigned(Box<Pending>),
    Acknowledged,
    Committed {
        position: u64,
        running_hash: Commitment,
    },
    Aborted {
        position: u64,
        epoch: u64,
    },
    Configured,
    Cut {
        position: u64,
        running_hash: Commitment,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Retry {
    key: PublicKey,
    sequence: u64,
    digest: Commitment,
    result: std::result::Result<Result, CoordinationError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Log {
    pub head: Head,
    retries: Vec<Retry>,
    minute: u64,
    appends: u32,
}
impl Log {
    pub fn new(scope: Scope, configuration: [u8; 16]) -> Self {
        Self {
            head: Head {
                genesis: rocky_core::genesis(scope, configuration),
                epoch: 0,
                minimum: 1,
                head: 0,
                commit: 0,
                running_hash: rocky_core::genesis(scope, configuration),
                members: Vec::new(),
                pending: None,
                hashes: VecDeque::new(),
                cut: None,
            },
            retries: Vec::new(),
            minute: 0,
            appends: 0,
        }
    }
    pub fn appends_in(&self, minute: u64) -> u32 {
        if self.minute == minute {
            self.appends
        } else {
            0
        }
    }
    pub fn bytes(&self) -> usize {
        // Conservative admission accounting including encoded retry outcomes.
        let cut = self
            .head
            .cut
            .as_ref()
            .map_or(0, |cut| 256 + cut.record.len() + cut.authority.len());
        if self.retries.is_empty() && cut == 0 {
            0
        } else {
            1024 + 32 * (80 + 512) + HASH_WINDOW * 40 + cut
        }
    }
    /// Admits an owner-signed cut of the exact head; the caller has checked
    /// the signer's authority. The cut commits at `a+1`, clears the in-sync
    /// set and moves the epoch. A retry of the retained record answers the
    /// same commit.
    pub fn cut(
        &mut self,
        scope: Scope,
        record: &[u8],
        authority: &[u8],
    ) -> std::result::Result<Result, CoordinationError> {
        let signed = SignedCut::from_bytes(record).map_err(|_| CoordinationError::Denied)?;
        if authority.len() > MAX_CUT_AUTHORITY_BYTES {
            return Err(CoordinationError::Denied);
        }
        let digest = signed.digest().map_err(|_| CoordinationError::Denied)?;
        if let Some(cut) = self.head.cut.as_ref().filter(|cut| cut.digest == digest) {
            return Ok(Result::Cut {
                position: cut.position,
                running_hash: cut.running_hash,
            });
        }
        let statement = &signed.statement;
        if statement.log != scope {
            return Err(CoordinationError::Denied);
        }
        let (sequence, previous, floor) = match &self.head.cut {
            Some(cut) => (cut.sequence + 1, Some(cut.digest), cut.position),
            None => (1, None, 0),
        };
        if self.head.pending.is_some()
            || statement.replaced.position != self.head.commit
            || statement.replaced.running != self.head.running_hash
            || statement.epoch != self.head.epoch
            || statement.sequence != sequence
            || statement.previous != previous
        {
            return Err(CoordinationError::StaleHead);
        }
        // A cut never abandons a cut, and its prefix chains from a point
        // the window or the genesis anchor authenticates.
        if statement.retained.position < floor
            || !self.endorses(statement.verified, statement.retained)
        {
            return Err(CoordinationError::Denied);
        }
        let epoch = self
            .head
            .epoch
            .checked_add(1)
            .ok_or(CoordinationError::StaleEpoch)?;
        let position = statement.entry();
        let running_hash = rocky_core::coordinated_chain(self.head.running_hash, position, digest);
        self.head.epoch = epoch;
        self.head.head = position;
        self.head.commit = position;
        self.head.running_hash = running_hash;
        self.head.members.clear();
        self.head.hashes.push_back(digest);
        if self.head.hashes.len() > HASH_WINDOW {
            self.head.hashes.pop_front();
        }
        self.head.cut = Some(Box::new(Cut {
            record: record.to_vec(),
            authority: authority.to_vec(),
            digest,
            sequence,
            position,
            running_hash,
        }));
        Ok(Result::Cut {
            position,
            running_hash,
        })
    }
    /// Whether `verified` is the genesis anchor or chains through the window
    /// to the commit, and `retained` follows from it where the window
    /// reaches. Entries before the window are endorsed by the record alone.
    fn endorses(&self, verified: CutPoint, retained: CutPoint) -> bool {
        let head = &self.head;
        let first = head.commit + 1 - head.hashes.len() as u64;
        let hash = |position: u64| head.hashes[(position - first) as usize];
        if verified.position == 0 {
            if verified.running != head.genesis {
                return false;
            }
        } else if verified.position + 1 < first {
            return false;
        }
        if verified.position + 1 < first {
            return true;
        }
        let mut running = verified.running;
        for position in verified.position + 1..=head.commit {
            running = rocky_core::coordinated_chain(running, position, hash(position));
            if position == retained.position && running != retained.running {
                return false;
            }
        }
        running == head.running_hash
    }
    pub fn apply(
        &mut self,
        key: PublicKey,
        action: &Action,
        time: u64,
        tenant_admits: bool,
    ) -> std::result::Result<Result, CoordinationError> {
        let sequence = match action {
            Action::Join { sequence, .. } | Action::Append { sequence, .. } => *sequence,
            _ => return self.transition(key, action, time, tenant_admits),
        };
        if sequence == 0 || sequence > MAX_COUNTER {
            return Err(CoordinationError::Denied);
        }
        let digest = commitment("rocky.coordinated-action.v1", &encode(action)?);
        if let Some(old) = self.retries.iter().find(|r| r.key == key) {
            if sequence < old.sequence {
                return Err(CoordinationError::Retired);
            }
            if sequence == old.sequence {
                return if digest == old.digest {
                    old.result.clone()
                } else {
                    Err(CoordinationError::RequestConflict)
                };
            }
        } else if self.retries.len() >= TRUSTED_NODES {
            return Err(CoordinationError::Backpressure);
        }
        let result = self.transition(key, action, time, tenant_admits);
        let retry = Retry {
            key,
            sequence,
            digest,
            result: result.clone(),
        };
        if let Some(old) = self.retries.iter_mut().find(|r| r.key == key) {
            *old = retry;
        } else {
            self.retries.push(retry);
        }
        result
    }
    fn transition(
        &mut self,
        key: PublicKey,
        action: &Action,
        time: u64,
        tenant_admits: bool,
    ) -> std::result::Result<Result, CoordinationError> {
        match action {
            Action::Configure { minimum } => {
                if self.head.epoch != 0 || *minimum == 0 || *minimum > TRUSTED_NODES {
                    return Err(CoordinationError::Configuration);
                }
                self.head.minimum = *minimum;
                Ok(Result::Configured)
            }
            Action::Join {
                position,
                running_hash,
                ..
            } => {
                if *position != self.head.commit
                    || *running_hash != self.head.running_hash
                    || self.head.pending.is_some()
                {
                    return Err(CoordinationError::StaleHead);
                }
                if !self.head.members.iter().any(|m| m.key == key) {
                    if self.head.members.len() >= TRUSTED_NODES {
                        return Err(CoordinationError::Backpressure);
                    }
                    let epoch = self
                        .head
                        .epoch
                        .checked_add(1)
                        .ok_or(CoordinationError::StaleEpoch)?;
                    self.head.members.push(Member {
                        key,
                        identity: key,
                        acknowledged: *position,
                    });
                    self.head.epoch = epoch;
                }
                Ok(Result::Joined {
                    epoch: self.head.epoch,
                })
            }
            Action::Append {
                expected,
                epoch,
                hash,
                deadline,
                ..
            } => {
                if !self.head.members.iter().any(|m| m.key == key) {
                    return Err(CoordinationError::Denied);
                }
                if !tenant_admits || self.head.members.len() < self.head.minimum {
                    return Err(CoordinationError::Backpressure);
                }
                if *expected != self.head.head || self.head.pending.is_some() {
                    return Err(CoordinationError::StaleHead);
                }
                if *epoch != self.head.epoch {
                    return Err(CoordinationError::StaleEpoch);
                }
                if *deadline <= time || *deadline > time.saturating_add(ACKNOWLEDGMENT_DEADLINE) {
                    return Err(CoordinationError::Configuration);
                }
                let minute = time / 60;
                if self.minute == minute && self.appends >= APPENDS_PER_MINUTE {
                    return Err(CoordinationError::Backpressure);
                }
                let position = expected
                    .checked_add(1)
                    .filter(|p| *p <= MAX_COUNTER)
                    .ok_or(CoordinationError::StaleHead)?;
                let pending = Pending {
                    proposer: key,
                    hash: *hash,
                    position,
                    epoch: *epoch,
                    deadline: *deadline,
                };
                if minute != self.minute {
                    self.minute = minute;
                    self.appends = 0;
                }
                self.appends += 1;
                self.head.head = position;
                self.head.pending = Some(pending.clone());
                Ok(Result::Assigned(Box::new(pending)))
            }
            _ => Err(CoordinationError::Denied),
        }
    }
    pub fn resolve(
        &mut self,
        pending: &Pending,
        acknowledged: &[PublicKey],
        time: u64,
    ) -> std::result::Result<Result, CoordinationError> {
        if self.head.pending.as_ref() != Some(pending) {
            return Err(CoordinationError::StaleHead);
        }
        let all = self
            .head
            .members
            .iter()
            .all(|m| acknowledged.contains(&m.key));
        if !all && time < pending.deadline {
            return Err(CoordinationError::Backpressure);
        }
        let remaining = self
            .head
            .members
            .iter()
            .filter(|m| acknowledged.contains(&m.key))
            .count();
        let epoch = self
            .head
            .epoch
            .checked_add(1)
            .ok_or(CoordinationError::StaleEpoch)?;
        self.head.members.retain(|m| acknowledged.contains(&m.key));
        self.head.epoch = epoch;
        self.head.pending = None;
        if remaining < self.head.minimum {
            self.head.head = self.head.commit;
            return Ok(Result::Aborted {
                position: pending.position,
                epoch,
            });
        }
        self.head.running_hash =
            rocky_core::coordinated_chain(self.head.running_hash, pending.position, pending.hash);
        self.head.commit = pending.position;
        for member in &mut self.head.members {
            member.acknowledged = pending.position;
        }
        self.head.hashes.push_back(pending.hash);
        if self.head.hashes.len() > HASH_WINDOW {
            self.head.hashes.pop_front();
        }
        Ok(Result::Committed {
            position: pending.position,
            running_hash: self.head.running_hash,
        })
    }
}

mod bytes {
    use serde::{Deserializer, Serializer, de::Visitor};
    pub fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(value)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        struct Bytes;
        impl<'de> Visitor<'de> for Bytes {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a byte string")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut bytes = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    if bytes.len() == 64 * 1024 {
                        return Err(serde::de::Error::custom("byte string too large"));
                    }
                    bytes.push(byte);
                }
                Ok(bytes)
            }
            fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Self::Value, E> {
                Ok(bytes)
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
                Ok(bytes.to_vec())
            }
        }
        deserializer.deserialize_byte_buf(Bytes)
    }
}

mod hashes {
    use super::*;
    use serde::{
        Deserializer, Serializer,
        de::{Error, Visitor},
    };
    pub fn serialize<S: Serializer>(
        value: &VecDeque<Commitment>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let bytes: Vec<_> = value.iter().flat_map(|hash| hash.iter().copied()).collect();
        serializer.serialize_bytes(&bytes)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<VecDeque<Commitment>, D::Error> {
        struct Hashes {
            human: bool,
        }
        impl<'de> Visitor<'de> for Hashes {
            type Value = VecDeque<Commitment>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("up to 1024 concatenated hashes")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                if !self.human {
                    return Err(A::Error::custom("hash window must be a byte string"));
                }
                let mut bytes = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    if bytes.len() == HASH_WINDOW * 32 {
                        return Err(A::Error::custom("hash window too large"));
                    }
                    bytes.push(byte);
                }
                self.visit_bytes(&bytes)
            }
            fn visit_byte_buf<E: Error>(
                self,
                bytes: Vec<u8>,
            ) -> std::result::Result<Self::Value, E> {
                self.visit_bytes(&bytes)
            }
            fn visit_bytes<E: Error>(self, bytes: &[u8]) -> std::result::Result<Self::Value, E> {
                if !bytes.len().is_multiple_of(32) || bytes.len() > HASH_WINDOW * 32 {
                    return Err(E::custom("invalid hash window"));
                }
                Ok(bytes.as_chunks::<32>().0.iter().copied().collect())
            }
        }
        let human = deserializer.is_human_readable();
        deserializer.deserialize_byte_buf(Hashes { human })
    }
}

/// Conversions between this log's types and the `rocky_core` port.
impl Pending {
    pub fn port(&self) -> rocky_core::LogPending {
        rocky_core::LogPending {
            proposer: *self.proposer.as_bytes(),
            hash: self.hash,
            position: self.position,
            epoch: self.epoch,
            deadline: self.deadline,
        }
    }
    pub fn from_port(
        pending: &rocky_core::LogPending,
    ) -> std::result::Result<Self, CoordinationError> {
        Ok(Self {
            proposer: PublicKey::from_bytes(&pending.proposer)
                .map_err(|_| CoordinationError::InvalidCommitment)?,
            hash: pending.hash,
            position: pending.position,
            epoch: pending.epoch,
            deadline: pending.deadline,
        })
    }
}
impl Head {
    pub fn port(&self) -> rocky_core::LogHead {
        rocky_core::LogHead {
            genesis: self.genesis,
            epoch: self.epoch,
            minimum: self.minimum,
            head: self.head,
            commit: self.commit,
            running_hash: self.running_hash,
            members: self
                .members
                .iter()
                .map(|member| rocky_core::LogMember {
                    key: *member.key.as_bytes(),
                    acknowledged: member.acknowledged,
                })
                .collect(),
            pending: self.pending.as_ref().map(Pending::port),
            hashes: self.hashes.iter().copied().collect(),
            cut: self.cut.as_ref().map(|cut| rocky_core::LogCut {
                record: cut.record.clone(),
                authority: cut.authority.clone(),
            }),
        }
    }
}

/// One process's coordinated logs without Raft or signatures: the same log
/// machine, acknowledgments and deadline resolution, for tests and
/// single-process composition. Deadlines resolve on the next call after
/// them, as the service's leader timer does.
#[derive(Clone, Default)]
pub struct MemoryLog {
    state: std::sync::Arc<std::sync::Mutex<MemoryLogs>>,
}
#[derive(Default)]
struct MemoryLogs {
    logs: std::collections::BTreeMap<[u8; 48], (Log, Vec<PublicKey>)>,
    /// Each domain's control authority, which signs its cuts.
    owners: std::collections::BTreeMap<[u8; 48], PublicKey>,
    /// Seconds added to the system clock.
    skew: u64,
    unavailable: bool,
}
fn scope_key(scope: Scope) -> [u8; 48] {
    let mut key = [0; 48];
    key[..16].copy_from_slice(&scope.mesh.0);
    key[16..32].copy_from_slice(&scope.app.0);
    key[32..].copy_from_slice(&scope.domain.0);
    key
}
impl MemoryLog {
    pub fn new() -> Self {
        Self::default()
    }
    /// A trusted node's client of these logs.
    pub fn client(&self, key: PublicKey) -> MemoryLogClient {
        MemoryLogClient {
            logs: self.clone(),
            key,
        }
    }
    /// Sets the owner's minimum before the first join.
    pub fn configure(
        &self,
        scope: Scope,
        minimum: usize,
    ) -> std::result::Result<(), CoordinationError> {
        let owner = PublicKey::from_bytes(&[0; 32]).map_err(|_| CoordinationError::Corrupt)?;
        self.with(scope, |log, _, time, _| {
            log.apply(owner, &Action::Configure { minimum }, time, true)
                .map(|_| ())
        })
    }
    /// Names `scope`'s control authority, the only signer of its cuts.
    pub fn set_owner(&self, scope: Scope, owner: PublicKey) {
        self.lock().owners.insert(scope_key(scope), owner);
    }
    /// Moves this log's clock, so deadlines pass without sleeping.
    pub fn advance(&self, seconds: u64) {
        self.lock().skew += seconds;
    }
    /// Makes every call fail as an unreachable coordinator does.
    pub fn set_available(&self, available: bool) {
        self.lock().unavailable = !available;
    }
    pub fn head(&self, scope: Scope) -> std::result::Result<Head, CoordinationError> {
        self.with(scope, |log, _, _, _| Ok(log.head.clone()))
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryLogs> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn with<T>(
        &self,
        scope: Scope,
        f: impl FnOnce(
            &mut Log,
            &mut Vec<PublicKey>,
            u64,
            Option<PublicKey>,
        ) -> std::result::Result<T, CoordinationError>,
    ) -> std::result::Result<T, CoordinationError> {
        let mut state = self.lock();
        if state.unavailable {
            return Err(CoordinationError::Unavailable);
        }
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| CoordinationError::Corrupt)?
            .as_secs()
            + state.skew;
        let owner = state.owners.get(&scope_key(scope)).copied();
        let (log, acknowledged) = state
            .logs
            .entry(scope_key(scope))
            .or_insert_with(|| (Log::new(scope, [0; 16]), Vec::new()));
        if let Some(pending) = log.head.pending.clone()
            && time >= pending.deadline
        {
            log.resolve(&pending, acknowledged, time)?;
            acknowledged.clear();
        }
        f(log, acknowledged, time, owner)
    }
}
pub struct MemoryLogClient {
    logs: MemoryLog,
    key: PublicKey,
}
impl rocky_core::DomainLog for MemoryLogClient {
    fn node(&self) -> rocky_core::NodeKey {
        *self.key.as_bytes()
    }
    fn log_head(&self, scope: Scope) -> rocky_core::CoordinationFuture<'_, rocky_core::LogHead> {
        Box::pin(async move { self.logs.head(scope).map(|head| head.port()) })
    }
    fn join(
        &self,
        scope: Scope,
        position: u64,
        running_hash: Commitment,
        sequence: u64,
    ) -> rocky_core::CoordinationFuture<'_, u64> {
        Box::pin(async move {
            let action = Action::Join {
                position,
                running_hash,
                sequence,
            };
            match self.logs.with(scope, |log, _, time, _| {
                log.apply(self.key, &action, time, true)
            })? {
                Result::Joined { epoch } => Ok(epoch),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn append(
        &self,
        scope: Scope,
        append: rocky_core::Append,
    ) -> rocky_core::CoordinationFuture<'_, rocky_core::LogPending> {
        Box::pin(async move {
            let action = Action::Append {
                expected: append.expected,
                epoch: append.epoch,
                hash: append.hash,
                deadline: append.deadline,
                sequence: append.sequence,
            };
            match self.logs.with(scope, |log, _, time, _| {
                log.apply(self.key, &action, time, true)
            })? {
                Result::Assigned(pending) => Ok(pending.port()),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn acknowledge(
        &self,
        scope: Scope,
        pending: rocky_core::LogPending,
    ) -> rocky_core::CoordinationFuture<'_, rocky_core::LogResult> {
        Box::pin(async move {
            let pending = Pending::from_port(&pending)?;
            self.logs.with(scope, |log, acknowledged, time, _| {
                if log.head.pending.as_ref() != Some(&pending)
                    || !log.head.members.iter().any(|m| m.key == self.key)
                {
                    return Err(CoordinationError::StaleHead);
                }
                if !acknowledged.contains(&self.key) {
                    acknowledged.push(self.key);
                }
                match log.resolve(&pending, acknowledged, time) {
                    Ok(Result::Committed {
                        position,
                        running_hash,
                    }) => {
                        acknowledged.clear();
                        Ok(rocky_core::LogResult::Committed {
                            position,
                            running_hash,
                        })
                    }
                    Ok(_) => Err(CoordinationError::Corrupt),
                    Err(CoordinationError::Backpressure) => Ok(rocky_core::LogResult::Acknowledged),
                    Err(error) => Err(error),
                }
            })
        })
    }
    fn cut(
        &self,
        scope: Scope,
        cut: rocky_core::LogCut,
    ) -> rocky_core::CoordinationFuture<'_, rocky_core::LogResult> {
        Box::pin(async move {
            let signer = SignedCut::from_bytes(&cut.record)
                .map_err(|_| CoordinationError::Denied)?
                .signer;
            self.logs.with(scope, |log, acknowledged, _, owner| {
                if owner != Some(signer) {
                    return Err(CoordinationError::Denied);
                }
                match log.cut(scope, &cut.record, &cut.authority)? {
                    Result::Cut {
                        position,
                        running_hash,
                    } => {
                        acknowledged.clear();
                        Ok(rocky_core::LogResult::Committed {
                            position,
                            running_hash,
                        })
                    }
                    _ => Err(CoordinationError::Corrupt),
                }
            })
        })
    }
}
