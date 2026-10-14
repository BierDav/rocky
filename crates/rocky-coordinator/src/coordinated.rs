//! Metadata-only domain log. Storage acknowledgments are signed by trusted nodes.
use crate::{certified::encode, machine::Request};
use p2panda_core::PublicKey;
use rocky_core::{Commitment, CoordinationError, MAX_COUNTER, Scope, commitment};
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
    pub acknowledged: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Head {
    pub epoch: u64,
    pub minimum: usize,
    pub head: u64,
    pub commit: u64,
    pub running_hash: Commitment,
    pub members: Vec<Member>,
    pub pending: Option<Pending>,
    #[serde(with = "hashes")]
    pub hashes: VecDeque<Commitment>,
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
                epoch: 0,
                minimum: 1,
                head: 0,
                commit: 0,
                running_hash: rocky_core::genesis(scope, configuration),
                members: Vec::new(),
                pending: None,
                hashes: VecDeque::new(),
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
        if self.retries.is_empty() {
            0
        } else {
            1024 + 32 * (80 + 512) + HASH_WINDOW * 40
        }
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
        let mut bytes = self.head.running_hash.to_vec();
        bytes.extend_from_slice(&pending.position.to_be_bytes());
        bytes.extend_from_slice(&pending.hash);
        self.head.running_hash = commitment("rocky.coordinated-chain.v1", &bytes);
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
