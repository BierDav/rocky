//! Metadata-only per-tip logs of a domain. Storage acknowledgments are
//! signed by trusted nodes.
use crate::{certified::encode, machine::Request};
use p2panda_core::PublicKey;
use rocky_core::{
    Commitment, Configuration, CoordinationError, LogTerminal, MAX_COUNTER, Scope, TipId, TipLog,
    commitment,
};
use rocky_credentials::{LogPoint, SignedSeal};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const HASH_WINDOW: usize = 1024;
/// Fixed admission caps of the current log machine, in addition to tenant Usage budgets.
pub const TRUSTED_NODES: usize = 32;
pub const APPENDS_PER_MINUTE: u32 = 60;
pub const ACKNOWLEDGMENT_DEADLINE: u64 = 30;
/// Open tip logs one domain holds.
pub const TIP_LOGS: usize = 8;
/// Ended tip logs one domain retains; the earliest ended is dropped beyond.
pub const ENDED_LOGS: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Action {
    Head,
    /// The minimum the tip's opening names; immutable after the first join.
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
    /// The terminal entry: a seal of the tip or a bookmark move off it. Once
    /// the log committed an entry, a node of its in-sync set submits it.
    Seal {
        terminal: LogTerminal,
    },
    /// The owner's recovery seal of a stuck tip, an encoded
    /// `rocky_credentials::SignedSeal`: the one terminal entry committed
    /// without the in-sync set.
    Recover {
        #[serde(with = "bytes")]
        seal: Vec<u8>,
    },
    /// A trusted node's signed `rocky_credentials::TerminalAck` of the
    /// terminal entry, kept with it for devices to verify.
    AcknowledgeTerminal {
        #[serde(with = "bytes")]
        acknowledgment: Vec<u8>,
    },
}
impl Action {
    /// The action's name in diagnostics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Configure { .. } => "configure",
            Self::Join { .. } => "join",
            Self::Append { .. } => "append",
            Self::Acknowledge { .. } => "acknowledge",
            Self::Resolve { .. } => "resolve",
            Self::Seal { .. } => "seal",
            Self::Recover { .. } => "recover",
            Self::AcknowledgeTerminal { .. } => "acknowledge_terminal",
        }
    }
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
    /// The entry at `commit` that ended the log.
    pub terminal: Option<LogTerminal>,
    /// Encoded terminal acknowledgments, one per node, in arrival order.
    pub acknowledgments: Vec<Acknowledgment>,
    /// Set as read: the owner closed the domain, so the log takes no write.
    pub closed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Acknowledgment {
    pub key: PublicKey,
    #[serde(with = "bytes")]
    pub record: Vec<u8>,
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
    TerminalAcknowledged,
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
    pub fn new(log: TipLog, configuration: Configuration, minimum: usize) -> Self {
        let genesis = rocky_core::log_genesis(log, configuration);
        Self {
            head: Head {
                genesis,
                epoch: 0,
                minimum,
                head: 0,
                commit: 0,
                running_hash: genesis,
                members: Vec::new(),
                pending: None,
                hashes: VecDeque::new(),
                terminal: None,
                acknowledgments: Vec::new(),
                closed: false,
            },
            retries: Vec::new(),
            minute: 0,
            appends: 0,
        }
    }
    /// Whether no terminal entry ended the log.
    pub fn open(&self) -> bool {
        self.head.terminal.is_none()
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
        let acknowledgments: usize = self
            .head
            .acknowledgments
            .iter()
            .map(|ack| 64 + ack.record.len())
            .sum();
        if self.retries.is_empty() && acknowledgments == 0 {
            0
        } else {
            1024 + 32 * (80 + 512) + HASH_WINDOW * 40 + acknowledgments
        }
    }
    /// Commits `terminal` from `key` at `commit+1`, aborting the pending
    /// entry. The in-sync set holds every committed entry, so only one of
    /// its nodes ends a log that has any: a seal never leaves a committed
    /// entry behind, and a stuck log ends only by the owner's recovery.
    /// A retry of the same terminal answers the same commit, whoever sends
    /// it; another one is `Sealed`.
    pub fn seal(
        &mut self,
        key: PublicKey,
        terminal: LogTerminal,
    ) -> std::result::Result<Result, CoordinationError> {
        if let Some(result) = self.ended(terminal)? {
            return Ok(result);
        }
        if self.head.commit > 0 && !self.head.member(key) {
            return Err(CoordinationError::Denied);
        }
        self.end(terminal)
    }
    /// Commits the owner's recovery seal of the exact head without the
    /// in-sync set. `owner` is the domain's authority; the seal's retained
    /// prefix must chain to the head where the window reaches.
    pub fn recover(
        &mut self,
        log: TipLog,
        seal: &[u8],
        owner: PublicKey,
    ) -> std::result::Result<Result, CoordinationError> {
        let signed = SignedSeal::from_bytes(seal).map_err(|_| CoordinationError::Denied)?;
        let terminal = LogTerminal {
            digest: signed.id().map_err(|_| CoordinationError::Denied)?,
            continuation: signed.successor().map_err(|_| CoordinationError::Denied)?,
        };
        if let Some(result) = self.ended(terminal)? {
            return Ok(result);
        }
        let recovery = signed
            .verify_recovery(owner)
            .map_err(|_| CoordinationError::Denied)?;
        if signed.statement.scope != log.scope || signed.statement.tip != log.tip {
            return Err(CoordinationError::Denied);
        }
        if recovery.replaced
            != (LogPoint {
                position: self.head.commit,
                running: self.head.running_hash,
            })
        {
            return Err(CoordinationError::StaleHead);
        }
        if !self.endorses(recovery.retained) {
            return Err(CoordinationError::Denied);
        }
        self.end(terminal)
    }
    /// The same commit for a retry of the terminal that ended the log;
    /// `Sealed` for another one.
    fn ended(
        &self,
        terminal: LogTerminal,
    ) -> std::result::Result<Option<Result>, CoordinationError> {
        match &self.head.terminal {
            None => Ok(None),
            Some(ended) if *ended == terminal => Ok(Some(Result::Committed {
                position: self.head.commit,
                running_hash: self.head.running_hash,
            })),
            Some(_) => Err(CoordinationError::Sealed),
        }
    }
    /// Ends the log with `terminal` at `commit+1`: it keeps no in-sync set
    /// and takes no other entry.
    fn end(&mut self, terminal: LogTerminal) -> std::result::Result<Result, CoordinationError> {
        let epoch = self
            .head
            .epoch
            .checked_add(1)
            .ok_or(CoordinationError::StaleEpoch)?;
        let position = self
            .head
            .commit
            .checked_add(1)
            .filter(|p| *p <= MAX_COUNTER)
            .ok_or(CoordinationError::StaleHead)?;
        let running_hash =
            rocky_core::coordinated_chain(self.head.running_hash, position, terminal.digest);
        self.head.pending = None;
        self.head.members.clear();
        self.head.epoch = epoch;
        self.head.head = position;
        self.head.commit = position;
        self.head.running_hash = running_hash;
        self.head.hashes.push_back(terminal.digest);
        if self.head.hashes.len() > HASH_WINDOW {
            self.head.hashes.pop_front();
        }
        self.head.terminal = Some(terminal);
        Ok(Result::Committed {
            position,
            running_hash,
        })
    }
    /// Keeps `key`'s acknowledgment of the terminal entry: it names this
    /// log, the terminal's position, running hash, seal and continuation,
    /// and is signed by `key`. The signer's chain is the devices' to verify.
    /// A retry of the kept record succeeds; a different one is refused.
    pub fn acknowledge_terminal(
        &mut self,
        log: TipLog,
        key: PublicKey,
        record: &[u8],
    ) -> std::result::Result<Result, CoordinationError> {
        let terminal = self.head.terminal.ok_or(CoordinationError::StaleHead)?;
        if let Some(kept) = self.head.acknowledgments.iter().find(|ack| ack.key == key) {
            return match kept.record == record {
                true => Ok(Result::TerminalAcknowledged),
                false => Err(CoordinationError::RequestConflict),
            };
        }
        let ack = rocky_credentials::TerminalAck::from_bytes(record)
            .map_err(|_| CoordinationError::Denied)?;
        let statement = &ack.statement;
        if ack.signer.as_bytes() != key.as_bytes()
            || statement.scope != log.scope
            || statement.tip != log.tip
            || statement.position != self.head.commit
            || statement.running != self.head.running_hash
            || statement.seal != terminal.digest
            || statement.continuation != terminal.continuation
        {
            return Err(CoordinationError::Denied);
        }
        if self.head.acknowledgments.len() >= TRUSTED_NODES {
            return Err(CoordinationError::Backpressure);
        }
        self.head.acknowledgments.push(Acknowledgment {
            key,
            record: record.to_vec(),
        });
        Ok(Result::TerminalAcknowledged)
    }
    /// Whether `retained` chains through the window to the commit: at the
    /// genesis anchor, or at a position the window reaches. A prefix ending
    /// before the window is endorsed by the owner's signature alone.
    fn endorses(&self, retained: LogPoint) -> bool {
        let head = &self.head;
        let first = head.commit + 1 - head.hashes.len() as u64;
        if retained.position == 0 && retained.running != head.genesis {
            return false;
        }
        if retained.position + 1 < first {
            return true;
        }
        let mut running = retained.running;
        for position in retained.position + 1..=head.commit {
            let hash = head.hashes[(position - first) as usize];
            running = rocky_core::coordinated_chain(running, position, hash);
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
        if self.head.terminal.is_some() {
            return Err(CoordinationError::Sealed);
        }
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

/// The tip logs of one domain and the minimum a new one starts with.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Logs {
    scope: Scope,
    configuration: Configuration,
    /// Owner policy for tips whose opening names none; immutable after the
    /// domain's first join.
    minimum: usize,
    /// Oldest first.
    logs: Vec<(TipId, Log)>,
}
impl Logs {
    pub fn new(scope: Scope, configuration: Configuration) -> Self {
        Self {
            scope,
            configuration,
            minimum: 1,
            logs: Vec::new(),
        }
    }
    pub fn get(&self, tip: TipId) -> Option<&Log> {
        self.logs
            .iter()
            .find(|(id, _)| *id == tip)
            .map(|(_, log)| log)
    }
    pub fn get_mut(&mut self, tip: TipId) -> Option<&mut Log> {
        self.logs
            .iter_mut()
            .find(|(id, _)| *id == tip)
            .map(|(_, log)| log)
    }
    fn fresh(&self, tip: TipId) -> Log {
        let log = TipLog {
            scope: self.scope,
            tip,
        };
        Log::new(log, self.configuration, self.minimum)
    }
    /// The head of `tip`'s log; an unopened one is at its genesis.
    pub fn head(&self, tip: TipId) -> Head {
        self.get(tip)
            .map_or_else(|| self.fresh(tip).head, |log| log.head.clone())
    }
    /// Sets the domain's default minimum before any node joined a log.
    pub fn configure(&mut self, minimum: usize) -> std::result::Result<Result, CoordinationError> {
        if minimum == 0
            || minimum > TRUSTED_NODES
            || self.logs.iter().any(|(_, l)| l.head.epoch != 0)
        {
            return Err(CoordinationError::Configuration);
        }
        self.minimum = minimum;
        Ok(Result::Configured)
    }
    /// A copy of `tip`'s log to apply `action` from `key` to; `store` keeps
    /// it. Only a configuration, a join or a terminal entry opens a log, within
    /// `TIP_LOGS` open ones. A node is in sync on one tip of its domain: a
    /// join waits while the tip it leaves has a pending entry.
    pub fn working(
        &self,
        key: PublicKey,
        tip: TipId,
        action: &Action,
    ) -> std::result::Result<Log, CoordinationError> {
        if matches!(action, Action::Join { .. }) {
            for (_, log) in self
                .logs
                .iter()
                .filter(|(id, log)| *id != tip && log.head.member(key))
            {
                if log.head.pending.is_some() {
                    return Err(CoordinationError::Backpressure);
                }
                if log.head.epoch == u64::MAX {
                    return Err(CoordinationError::StaleEpoch);
                }
            }
        }
        if let Some(log) = self.get(tip) {
            return Ok(log.clone());
        }
        match action {
            // An unopened tip's seal ends it at once.
            Action::Seal { .. } | Action::Recover { .. } => return Ok(self.fresh(tip)),
            Action::Configure { .. } | Action::Join { .. } => {}
            Action::Append { .. } => return Err(CoordinationError::Denied),
            _ => return Err(CoordinationError::StaleHead),
        }
        if self.logs.iter().filter(|(_, l)| l.open()).count() >= TIP_LOGS {
            return Err(CoordinationError::Backpressure);
        }
        Ok(self.fresh(tip))
    }
    /// Keeps `log` as `tip`'s. A log that just ended moves last, and the
    /// earliest ended ones beyond `ENDED_LOGS` are dropped.
    pub fn store(&mut self, tip: TipId, log: Log) {
        let index = self.logs.iter().position(|(id, _)| *id == tip);
        match index {
            Some(index) if self.logs[index].1.open() && !log.open() => {
                self.logs.remove(index);
                self.logs.push((tip, log));
            }
            Some(index) => self.logs[index].1 = log,
            None => self.logs.push((tip, log)),
        }
        while self.logs.iter().filter(|(_, l)| !l.open()).count() > ENDED_LOGS {
            if let Some(index) = self.logs.iter().position(|(_, l)| !l.open()) {
                self.logs.remove(index);
            }
        }
    }
    /// After `key` joined `tip`, removes it from every other tip's in-sync
    /// set and moves their epochs; `working` checked that none is pending.
    pub fn leave_others(&mut self, key: PublicKey, tip: TipId) {
        for (_, log) in self
            .logs
            .iter_mut()
            .filter(|(id, log)| *id != tip && log.head.member(key))
        {
            log.head.members.retain(|member| member.key != key);
            log.head.epoch = log.head.epoch.saturating_add(1);
        }
    }
    pub fn pending(&self) -> impl Iterator<Item = (TipId, &Pending)> {
        self.logs
            .iter()
            .filter_map(|(tip, log)| log.head.pending.as_ref().map(|p| (*tip, p)))
    }
    /// Every in-sync node of every tip.
    pub fn members(&self) -> impl Iterator<Item = &Member> {
        self.logs.iter().flat_map(|(_, log)| &log.head.members)
    }
    pub fn bytes(&self) -> usize {
        self.logs.iter().map(|(_, log)| log.bytes()).sum()
    }
    pub fn appends_in(&self, minute: u64) -> u64 {
        self.logs
            .iter()
            .map(|(_, log)| u64::from(log.appends_in(minute)))
            .sum()
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

impl Head {
    pub fn member(&self, key: PublicKey) -> bool {
        self.members.iter().any(|m| m.key == key)
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
            terminal: self.terminal,
            acknowledgments: self
                .acknowledgments
                .iter()
                .map(|ack| ack.record.clone())
                .collect(),
            closed: self.closed,
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
type Acknowledged = std::collections::BTreeMap<TipId, Vec<PublicKey>>;
#[derive(Default)]
struct MemoryLogs {
    domains: std::collections::BTreeMap<Scope, (Logs, Acknowledged)>,
    /// Each domain's control authority, which signs its recovery seals.
    owners: std::collections::BTreeMap<Scope, PublicKey>,
    /// Seconds added to the system clock.
    skew: u64,
    unavailable: bool,
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
    /// Sets the owner's default minimum before the domain's first join.
    pub fn configure(
        &self,
        scope: Scope,
        minimum: usize,
    ) -> std::result::Result<(), CoordinationError> {
        self.domain(scope, |logs, _, _, _| logs.configure(minimum).map(|_| ()))
    }
    /// Names `scope`'s control authority, the only signer of its recovery
    /// seals.
    pub fn set_owner(&self, scope: Scope, owner: PublicKey) {
        self.lock().owners.insert(scope, owner);
    }
    /// Moves this log's clock, so deadlines pass without sleeping.
    pub fn advance(&self, seconds: u64) {
        self.lock().skew += seconds;
    }
    /// Makes every call fail as an unreachable coordinator does.
    pub fn set_available(&self, available: bool) {
        self.lock().unavailable = !available;
    }
    pub fn head(&self, log: TipLog) -> std::result::Result<Head, CoordinationError> {
        self.domain(log.scope, |logs, _, _, _| Ok(logs.head(log.tip)))
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryLogs> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Runs `f` on `scope`'s logs after resolving every passed deadline.
    fn domain<T>(
        &self,
        scope: Scope,
        f: impl FnOnce(
            &mut Logs,
            &mut Acknowledged,
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
        let owner = state.owners.get(&scope).copied();
        let (logs, acknowledged) = state
            .domains
            .entry(scope)
            .or_insert_with(|| (Logs::new(scope, [0; 16]), Default::default()));
        let due: Vec<_> = logs
            .pending()
            .filter(|(_, pending)| time >= pending.deadline)
            .map(|(tip, pending)| (tip, pending.clone()))
            .collect();
        for (tip, pending) in due {
            let acks = acknowledged.remove(&tip).unwrap_or_default();
            if let Some(log) = logs.get_mut(tip) {
                log.resolve(&pending, &acks, time)?;
            }
        }
        f(logs, acknowledged, time, owner)
    }
    /// Applies `action` from `key` to a working copy of `log` and keeps it.
    fn with<T>(
        &self,
        log: TipLog,
        key: PublicKey,
        action: &Action,
        f: impl FnOnce(
            &mut Log,
            &mut Vec<PublicKey>,
            u64,
            Option<PublicKey>,
        ) -> std::result::Result<T, CoordinationError>,
    ) -> std::result::Result<T, CoordinationError> {
        self.domain(log.scope, |logs, acknowledged, time, owner| {
            let mut working = logs.working(key, log.tip, action)?;
            let acks = acknowledged.entry(log.tip).or_default();
            let result = f(&mut working, acks, time, owner);
            logs.store(log.tip, working);
            if matches!(action, Action::Join { .. }) && result.is_ok() {
                logs.leave_others(key, log.tip);
            }
            result
        })
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
    fn log_head(&self, log: TipLog) -> rocky_core::CoordinationFuture<'_, rocky_core::LogHead> {
        Box::pin(async move { self.logs.head(log).map(|head| head.port()) })
    }
    fn configure(&self, log: TipLog, minimum: usize) -> rocky_core::CoordinationFuture<'_, ()> {
        Box::pin(async move {
            let action = Action::Configure { minimum };
            self.logs
                .with(log, self.key, &action, |working, _, time, _| {
                    working.apply(self.key, &action, time, true).map(|_| ())
                })
        })
    }
    fn join(
        &self,
        log: TipLog,
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
            match self
                .logs
                .with(log, self.key, &action, |working, _, time, _| {
                    working.apply(self.key, &action, time, true)
                })? {
                Result::Joined { epoch } => Ok(epoch),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn append(
        &self,
        log: TipLog,
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
            match self
                .logs
                .with(log, self.key, &action, |working, _, time, _| {
                    working.apply(self.key, &action, time, true)
                })? {
                Result::Assigned(pending) => Ok(pending.port()),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn acknowledge(
        &self,
        log: TipLog,
        pending: rocky_core::LogPending,
    ) -> rocky_core::CoordinationFuture<'_, rocky_core::LogResult> {
        Box::pin(async move {
            let pending = Pending::from_port(&pending)?;
            let action = Action::Acknowledge {
                pending: pending.clone(),
            };
            self.logs
                .with(log, self.key, &action, |working, acknowledged, time, _| {
                    if working.head.pending.as_ref() != Some(&pending)
                        || !working.head.members.iter().any(|m| m.key == self.key)
                    {
                        return Err(CoordinationError::StaleHead);
                    }
                    if !acknowledged.contains(&self.key) {
                        acknowledged.push(self.key);
                    }
                    match working.resolve(&pending, acknowledged, time) {
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
                        Err(CoordinationError::Backpressure) => {
                            Ok(rocky_core::LogResult::Acknowledged)
                        }
                        Err(error) => Err(error),
                    }
                })
        })
    }
    fn seal(
        &self,
        log: TipLog,
        terminal: LogTerminal,
    ) -> rocky_core::CoordinationFuture<'_, rocky_core::LogResult> {
        Box::pin(async move {
            let action = Action::Seal { terminal };
            self.logs
                .with(log, self.key, &action, |working, acknowledged, _, _| {
                    let result = working.seal(self.key, terminal)?;
                    committed(result, acknowledged)
                })
        })
    }
    fn recover(
        &self,
        log: TipLog,
        seal: Vec<u8>,
    ) -> rocky_core::CoordinationFuture<'_, rocky_core::LogResult> {
        Box::pin(async move {
            let action = Action::Recover { seal };
            let Action::Recover { seal } = &action else {
                return Err(CoordinationError::Corrupt);
            };
            self.logs
                .with(log, self.key, &action, |working, acknowledged, _, owner| {
                    let owner = owner.ok_or(CoordinationError::Denied)?;
                    let result = working.recover(log, seal, owner)?;
                    committed(result, acknowledged)
                })
        })
    }
    fn acknowledge_terminal(
        &self,
        log: TipLog,
        acknowledgment: Vec<u8>,
    ) -> rocky_core::CoordinationFuture<'_, ()> {
        Box::pin(async move {
            let action = Action::AcknowledgeTerminal { acknowledgment };
            let Action::AcknowledgeTerminal { acknowledgment } = &action else {
                return Err(CoordinationError::Corrupt);
            };
            self.logs.with(log, self.key, &action, |working, _, _, _| {
                working
                    .acknowledge_terminal(log, self.key, acknowledgment)
                    .map(|_| ())
            })
        })
    }
}
/// A terminal entry's commit, which drops the leader's acknowledgments.
fn committed(
    result: Result,
    acknowledged: &mut Vec<PublicKey>,
) -> std::result::Result<rocky_core::LogResult, CoordinationError> {
    match result {
        Result::Committed {
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
}
