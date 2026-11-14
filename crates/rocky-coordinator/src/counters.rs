//! Named counters of a domain. A claim takes the counter's next value
//! through the replicated log, so values are unique and strictly increasing
//! across every voter. Claims are pure coordination: the application uses a
//! value in its own ordinary writes, and numbering policy stays its own.
use rocky_core::{CoordinationError, MAX_COUNTER};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Counters one domain holds.
pub const MAX_COUNTERS: usize = 64;
/// How far a claim's issue time may lead authority time, as request
/// signing times may.
pub const ISSUE_SKEW: u64 = 30;
/// Accounted bytes of a counter beside its name and claims, and of a claim.
const COUNTER_BYTES: usize = 128;
const CLAIM_BYTES: usize = 64;

pub use rocky_credentials::counters::{
    CLAIM_RECORDS, CLAIM_WINDOW, Claim, Lookup, MAX_NAME, Name, Reading,
};
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
        COUNTER_BYTES + self.name.as_bytes().len() + self.records.len() * CLAIM_BYTES
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
            growth += COUNTER_BYTES + counter.name.as_bytes().len();
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
