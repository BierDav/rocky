//! Bounded admission windows and retirement floors. The protocol and its
//! storage formula are in `docs/runtime/coordinator-retention.md`.
use crate::certified::encode;
use rocky_core::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DAY: u64 = 86_400;
/// Accounting overhead of a session's key and floor, and of an entry's key.
pub const SESSION_BYTES: usize = 64;
pub const ENTRY_BYTES: usize = 16;

/// Retention budgets, fixed when a store is provisioned. Times are seconds
/// of authority time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// How long after its issue time an identity may still be confirmed.
    pub admission: u64,
    /// How long an unacknowledged confirmed response stays retrievable.
    pub horizon: u64,
    /// How far ahead of authority time an identity may be issued.
    pub skew: u64,
    /// Retained responses per session, by count and bytes.
    pub window_entries: usize,
    pub window_bytes: usize,
    /// Sessions per domain, including idle ones that still hold a floor.
    pub sessions: usize,
    /// Open domains and accounted bytes per tenant.
    pub tenant_domains: usize,
    pub tenant_bytes: usize,
    /// Accounted bytes of all domains, below the snapshot ceiling.
    pub total_bytes: usize,
    /// How long a mutation's retry outcome is kept, and how many per signer.
    pub retry_window: u64,
    pub retry_records: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            admission: DAY,
            horizon: 7 * DAY,
            skew: 300,
            window_entries: 32,
            window_bytes: 64 * 1024,
            sessions: 256,
            tenant_domains: 16,
            tenant_bytes: 4 * 1024 * 1024,
            total_bytes: 24 * 1024 * 1024,
            retry_window: 3600,
            retry_records: 64,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<(), CoordinationError> {
        if self.admission == 0
            || self.horizon < self.admission
            || self.window_entries == 0
            || self.window_bytes <= SESSION_BYTES
            || self.sessions == 0
            || self.tenant_domains == 0
            || self.retry_window == 0
            || self.retry_records == 0
            || self.total_bytes > crate::certified::MAX_STORE / 4 * 3
        {
            return Err(CoordinationError::Configuration);
        }
        Ok(())
    }
}

/// What the coordinator can say about one request identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Confirmed, with its retained response.
    Confirmed(Box<StrongReceipt>),
    /// Not confirmed yet; admission is open, so a confirmation may follow.
    Open,
    /// The admission window closed without a confirmation.
    NotConfirmed,
    /// Behind a floor or the horizon: it never executes, outcome unavailable.
    Retired,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    receipt: StrongReceipt,
    bytes: u32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Session {
    /// Every identity at or below the floor is retired.
    floor: u64,
    #[serde(with = "pairs")]
    entries: BTreeMap<u64, Entry>,
}
impl Session {
    fn bytes(&self) -> usize {
        SESSION_BYTES
            + self
                .entries
                .values()
                .map(|e| ENTRY_BYTES + e.bytes as usize)
                .sum::<usize>()
    }
}

/// Request identities of one domain, by invoking node.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sessions(#[serde(with = "pairs")] BTreeMap<NodeId, Session>);

fn past(issued: u64, span: u64, now: u64) -> bool {
    issued.saturating_add(span) < now
}
impl Sessions {
    /// Drops responses past the horizon and sessions whose floor no longer
    /// protects anything the horizon does not.
    pub fn prune(&mut self, limits: &Limits, now: u64) {
        self.0.retain(|_, session| {
            session
                .entries
                .retain(|sequence, _| !past(issued_at(*sequence), limits.horizon, now));
            !session.entries.is_empty() || !past(issued_at(session.floor), limits.horizon, now)
        });
    }
    pub fn status(&self, limits: &Limits, now: u64, id: RequestId) -> Status {
        let sequence = id.sequence.get();
        if let Some(session) = self.0.get(&id.node) {
            if let Some(entry) = session.entries.get(&sequence) {
                return Status::Confirmed(Box::new(entry.receipt.clone()));
            }
            if sequence <= session.floor {
                return Status::Retired;
            }
        }
        let issued = issued_at(sequence);
        if past(issued, limits.horizon, now) {
            Status::Retired
        } else if past(issued, limits.admission, now) {
            Status::NotConfirmed
        } else {
            Status::Open
        }
    }
    /// Admission of a new confirmation of `receipt`, costing at most
    /// `available` more accounted bytes. Returns the bytes it adds.
    pub fn admit(
        &self,
        limits: &Limits,
        now: u64,
        receipt: &StrongReceipt,
        available: usize,
    ) -> Result<usize, CoordinationError> {
        let id = receipt.confirmation.request;
        match self.status(limits, now, id) {
            Status::Open => {}
            Status::Confirmed(_) => return Err(CoordinationError::Corrupt),
            Status::NotConfirmed => return Err(CoordinationError::Expired),
            Status::Retired => return Err(CoordinationError::Retired),
        }
        if issued_at(id.sequence.get()) > now.saturating_add(limits.skew) {
            return Err(CoordinationError::Backpressure);
        }
        let bytes = ENTRY_BYTES + encode(receipt)?.len();
        let (cost, fits) = match self.0.get(&id.node) {
            Some(session) => (
                bytes,
                session.entries.len() < limits.window_entries
                    && session.bytes() + bytes <= limits.window_bytes,
            ),
            None => (
                SESSION_BYTES + bytes,
                self.0.len() < limits.sessions && SESSION_BYTES + bytes <= limits.window_bytes,
            ),
        };
        if !fits || cost > available {
            return Err(CoordinationError::Backpressure);
        }
        Ok(cost)
    }
    pub fn insert(&mut self, receipt: StrongReceipt) -> Result<(), CoordinationError> {
        let bytes =
            u32::try_from(encode(&receipt)?.len()).map_err(|_| CoordinationError::Corrupt)?;
        let id = receipt.confirmation.request;
        self.0
            .entry(id.node)
            .or_default()
            .entries
            .insert(id.sequence.get(), Entry { receipt, bytes });
        Ok(())
    }
    /// Moves `node`'s floor to `through`, dropping the responses it covers.
    pub fn retire(
        &mut self,
        limits: &Limits,
        now: u64,
        node: NodeId,
        through: u64,
    ) -> Result<u64, CoordinationError> {
        if issued_at(through) > now.saturating_add(limits.skew) {
            return Err(CoordinationError::Denied);
        }
        if !self.0.contains_key(&node) {
            // Even an unconfirmed prefix must stay closed after acknowledgment.
            if past(issued_at(through), limits.horizon, now) {
                return Ok(through);
            }
            if self.0.len() >= limits.sessions {
                return Err(CoordinationError::Backpressure);
            }
        }
        let session = self.0.entry(node).or_default();
        if through > session.floor {
            session.floor = through;
            session.entries.retain(|sequence, _| *sequence > through);
        }
        Ok(session.floor)
    }
    pub fn bytes(&self) -> usize {
        self.0.values().map(Session::bytes).sum()
    }
    pub fn sessions(&self) -> usize {
        self.0.len()
    }
    pub fn entries(&self) -> usize {
        self.0.values().map(|s| s.entries.len()).sum()
    }
    pub fn retirement_cost(&self, limits: &Limits, now: u64, node: NodeId, through: u64) -> usize {
        if self.0.contains_key(&node) || past(issued_at(through), limits.horizon, now) {
            0
        } else {
            SESSION_BYTES
        }
    }
}

/// Maps serialize as ordered pairs, which canonical decoding then checks.
pub(crate) mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;
    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(
        value: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.iter().collect::<Vec<_>>().serialize(serializer)
    }
    pub fn deserialize<'de, K, V, D>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}
