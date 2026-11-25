use super::{Claim, Claimed, MAX_CLAIM_LIFETIME, MAX_CLAIMS, MAX_COUNTERS, Name};
use crate::CoordinationError;
use rocky_core::{Commitment, MAX_COUNTER};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const ACCOUNTED_COUNTER_BYTES: usize = 128;
const ACCOUNTED_CLAIM_BYTES: usize = 96;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Held {
    attempt: Commitment,
    value: u64,
    expires: u64,
}
impl Held {
    fn in_force(&self, time: u64) -> bool {
        self.expires > time
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Counter {
    name: Name,
    value: u64,
    claims: BTreeMap<Commitment, Held>,
}
impl Counter {
    fn new(name: Name) -> Self {
        Self {
            name,
            value: 0,
            claims: BTreeMap::new(),
        }
    }
    fn bytes(&self) -> usize {
        ACCOUNTED_COUNTER_BYTES
            + self.name.as_bytes().len()
            + self.claims.len() * ACCOUNTED_CLAIM_BYTES
    }
    fn evict_expired(&mut self, time: u64) -> usize {
        let before = self.claims.len();
        self.claims.retain(|_, held| held.in_force(time));
        (before - self.claims.len()) * ACCOUNTED_CLAIM_BYTES
    }
    fn take(
        &mut self,
        claim: Claim,
        time: u64,
        mut available: usize,
    ) -> Result<Claimed, CoordinationError> {
        if let Some(held) = self.claims.get(&claim.key) {
            if held.in_force(time) {
                return Ok(match held.attempt == claim.attempt {
                    true => Claimed::First(held.value),
                    false => Claimed::Taken(held.value),
                });
            }
            self.claims.remove(&claim.key);
            available = available.saturating_add(ACCOUNTED_CLAIM_BYTES);
        }
        if claim.expires <= time {
            return Err(CoordinationError::Expired);
        }
        if claim.expires > time.saturating_add(MAX_CLAIM_LIFETIME) {
            return Err(CoordinationError::Configuration);
        }
        let mut growth = ACCOUNTED_CLAIM_BYTES;
        if self.value == 0 {
            growth += ACCOUNTED_COUNTER_BYTES + self.name.as_bytes().len();
        }
        if self.claims.len() >= MAX_CLAIMS || growth > available {
            available = available.saturating_add(self.evict_expired(time));
        }
        if self.claims.len() >= MAX_CLAIMS {
            return Err(CoordinationError::Full);
        }
        if growth > available {
            return Err(CoordinationError::Backpressure);
        }
        let value = self
            .value
            .checked_add(1)
            .filter(|value| *value <= MAX_COUNTER)
            .ok_or(CoordinationError::Configuration)?;
        self.value = value;
        self.claims.insert(
            claim.key,
            Held {
                attempt: claim.attempt,
                value,
                expires: claim.expires,
            },
        );
        Ok(Claimed::First(value))
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
    ) -> Result<Claimed, CoordinationError> {
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
        let result = self.counters[index].take(claim, time, available);
        if self.counters[index].value == 0 {
            self.counters.remove(index);
        }
        result
    }
    pub fn read(&self, name: &Name) -> u64 {
        self.counters
            .iter()
            .find(|c| c.name == *name)
            .map_or(0, |counter| counter.value)
    }
}
