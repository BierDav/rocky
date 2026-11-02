//! Named counters of a domain. A claim takes the counter's next value
//! through the replicated log, so values are unique and strictly increasing
//! across every voter. Claims are pure coordination: the application uses a
//! value in its own ordinary writes, and numbering policy stays its own.
use crate::retention::DAY;
use rocky_core::{Commitment, CoordinationError, MAX_COUNTER};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Longest counter name, in bytes.
pub const MAX_NAME: usize = 64;
/// Counters one domain holds.
pub const MAX_COUNTERS: usize = 64;
/// Seconds after its issue time that a claim's value answers retries.
pub const CLAIM_WINDOW: u64 = 7 * DAY;
/// Latest claims one counter keeps for retries; older ones are evicted
/// before the window ends.
pub const CLAIM_RECORDS: usize = 1024;
/// How far a claim's issue time may lead authority time, as request
/// signing times may.
pub const ISSUE_SKEW: u64 = 30;
/// Accounted bytes of a counter beside its name and claims, and of a claim.
const COUNTER_BYTES: usize = 128;
const CLAIM_BYTES: usize = 64;

/// An opaque counter name of 1 through `MAX_NAME` bytes, such as one per
/// company and series.
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
/// A claim's stable identity. The application stores both fields before
/// its first attempt and repeats them on every retry; `issued` bounds how
/// long the value answers retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: Commitment,
    /// Unix seconds when the application created the claim.
    pub issued: u64,
}
impl Claim {
    /// A new claim with a random identity, issued now.
    pub fn fresh() -> Result<Self, CoordinationError> {
        Ok(Self {
            id: crate::transport::nonce(),
            issued: crate::certified::now()?,
        })
    }
}
/// A counter's last claimed value, 0 before the first claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    pub value: u64,
    /// What the claim asked about was given, when one was.
    pub claim: Option<Lookup>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lookup {
    /// The claim took this value.
    Claimed(u64),
    /// The claim took no value; claiming it now takes the next one.
    Unclaimed,
    /// Outside the window: claiming it is refused with `Expired`, whether
    /// or not it took a value.
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
    /// The latest issue time of an evicted claim: a claim issued no later
    /// is not known to be new.
    floor: u64,
    /// Claims within the window, oldest value first.
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
        COUNTER_BYTES + self.name.0.len() + self.records.len() * CLAIM_BYTES
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

/// The counters of one domain.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Counters {
    counters: Vec<Counter>,
}
impl Counters {
    pub fn bytes(&self) -> usize {
        self.counters.iter().map(Counter::bytes).sum()
    }
    /// The value `claim` took from `name`, taking the next one if it took
    /// none. A new claim may add at most `available` accounted bytes.
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
        let mut growth = CLAIM_BYTES;
        if counter.value == 0 {
            growth += COUNTER_BYTES + counter.name.0.len();
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
    /// `name`'s last value and, for `claim`, what it took.
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
