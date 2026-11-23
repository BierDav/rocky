use crate::CoordinationError;
use rocky_core::{Commitment, MAX_COUNTER};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const MAX_COUNTERS: usize = 64;
pub const ISSUE_SKEW: u64 = crate::transport::MAX_CLOCK_SKEW_SECONDS;
const ACCOUNTED_COUNTER_BYTES: usize = 128;
const ACCOUNTED_CLAIM_BYTES: usize = 64;

pub const MAX_NAME: usize = 64;
pub const CLAIM_WINDOW: u64 = 7 * 86_400;
pub const CLAIM_RECORDS: usize = 1024;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Vec<u8>", into = "Vec<u8>")]
pub struct Name(Vec<u8>);
impl Name {
    pub fn new(name: impl Into<Vec<u8>>) -> Result<Self, CoordinationError> {
        let name = name.into();
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(CoordinationError::Configuration);
        }
        Ok(Self(name))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl TryFrom<Vec<u8>> for Name {
    type Error = CoordinationError;
    fn try_from(name: Vec<u8>) -> Result<Self, Self::Error> {
        Self::new(name)
    }
}
impl From<Name> for Vec<u8> {
    fn from(name: Name) -> Self {
        name.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: Commitment,
    pub issued: u64,
}
impl Claim {
    pub fn fresh() -> Result<Self, CoordinationError> {
        Ok(Self {
            id: rocky_platform::entropy::random(),
            issued: crate::certified::now()?,
        })
    }
    pub fn keyed(key: &[u8], issued: u64) -> Self {
        Self {
            id: rocky_core::commitment("rocky.counter-claim.v1", key),
            issued,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    pub value: u64,
    pub claim: Option<Lookup>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lookup {
    Claimed(u64),
    Unclaimed,
    Expired,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Record {
    claim: Claim,
    value: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Counter {
    name: Name,
    value: u64,
    floor: u64,
    records: VecDeque<Record>,
}
impl Counter {
    fn new(name: Name) -> Self {
        Self {
            name,
            value: 0,
            floor: 0,
            records: VecDeque::new(),
        }
    }
    fn bytes(&self) -> usize {
        ACCOUNTED_COUNTER_BYTES
            + self.name.as_bytes().len()
            + self.records.len() * ACCOUNTED_CLAIM_BYTES
    }
    fn evict(&mut self, time: u64) {
        let floor = &mut self.floor;
        self.records.retain(|record| {
            let live = record.claim.issued.saturating_add(CLAIM_WINDOW) >= time;
            if !live {
                *floor = (*floor).max(record.claim.issued);
            }
            live
        });
    }
    fn lookup(&self, claim: Claim, time: u64) -> Result<Lookup, CoordinationError> {
        if let Some(record) = self.records.iter().find(|r| r.claim.id == claim.id) {
            return match record.claim.issued == claim.issued {
                true if record.claim.issued.saturating_add(CLAIM_WINDOW) >= time => {
                    Ok(Lookup::Claimed(record.value))
                }
                true => Ok(Lookup::Expired),
                false => Err(CoordinationError::RequestConflict),
            };
        }
        if claim.issued > time.saturating_add(ISSUE_SKEW) {
            return Err(CoordinationError::ClockSkew {
                coordinator_time: time,
            });
        }
        if claim.issued.saturating_add(CLAIM_WINDOW) < time || claim.issued <= self.floor {
            return Ok(Lookup::Expired);
        }
        Ok(Lookup::Unclaimed)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Counters {
    counters: Vec<Counter>,
}
impl Counters {
    pub fn bytes(&self) -> usize {
        self.counters.iter().map(Counter::bytes).sum()
    }
    pub fn claim(
        &mut self,
        name: &Name,
        claim: Claim,
        time: u64,
        available: usize,
    ) -> Result<u64, CoordinationError> {
        let index = match self.counters.iter().position(|c| c.name == *name) {
            Some(index) => index,
            None => {
                if self.counters.len() >= MAX_COUNTERS {
                    return Err(CoordinationError::Backpressure);
                }
                self.counters.push(Counter::new(name.clone()));
                self.counters.len() - 1
            }
        };
        let result = Self::take(&mut self.counters[index], claim, time, available);
        if self.counters[index].value == 0 {
            self.counters.remove(index);
        }
        result
    }
    fn take(
        counter: &mut Counter,
        claim: Claim,
        time: u64,
        available: usize,
    ) -> Result<u64, CoordinationError> {
        counter.evict(time);
        match counter.lookup(claim, time)? {
            Lookup::Claimed(value) => return Ok(value),
            Lookup::Expired => return Err(CoordinationError::Expired),
            Lookup::Unclaimed => {}
        }
        let mut growth = ACCOUNTED_CLAIM_BYTES;
        if counter.value == 0 {
            growth += ACCOUNTED_COUNTER_BYTES + counter.name.as_bytes().len();
        } else if counter.records.len() >= CLAIM_RECORDS {
            growth = 0;
        }
        if growth > available {
            return Err(CoordinationError::Backpressure);
        }
        let value = counter
            .value
            .checked_add(1)
            .filter(|value| *value <= MAX_COUNTER)
            .ok_or(CoordinationError::Configuration)?;
        if counter.records.len() >= CLAIM_RECORDS
            && let Some(evicted) = counter.records.pop_front()
        {
            counter.floor = counter.floor.max(evicted.claim.issued);
        }
        counter.value = value;
        counter.records.push_back(Record { claim, value });
        Ok(value)
    }
    pub fn read(
        &self,
        name: &Name,
        claim: Option<Claim>,
        time: u64,
    ) -> Result<Reading, CoordinationError> {
        let unopened;
        let counter = match self.counters.iter().find(|c| c.name == *name) {
            Some(counter) => counter,
            None => {
                unopened = Counter::new(name.clone());
                &unopened
            }
        };
        Ok(Reading {
            value: counter.value,
            claim: claim.map(|claim| counter.lookup(claim, time)).transpose()?,
        })
    }
}
