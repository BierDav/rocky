use crate::CoordinationError;
use rocky_core::Commitment;
use rocky_platform::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

#[cfg(feature = "voter")]
mod state;
#[cfg(feature = "voter")]
pub(crate) use state::Counters;

/// Counters one database may hold.
pub const MAX_COUNTERS: usize = 64;
/// The longest counter name in bytes.
pub const MAX_NAME: usize = 64;
/// Seconds a claim stays in force after its row's creation unless `Claim::expires` sets another
/// time: seven days.
pub const CLAIM_WINDOW: u64 = 7 * 86_400;
/// The furthest ahead of the coordinator's clock a claim may expire, in seconds: 366 days.
pub const MAX_CLAIM_LIFETIME: u64 = 366 * 86_400;
/// Claims in force one counter holds; a new key beyond them is refused as `Full` until one expires.
pub const MAX_CLAIMS: usize = 16_384;

/// A counter name of 1 to 64 bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Vec<u8>", into = "Vec<u8>")]
pub struct Name(#[serde(with = "rocky_core::byte_arrays::one")] Vec<u8>);
impl Name {
    /// Refuses an empty name or one over `MAX_NAME` with `Configuration`.
    pub fn new(name: impl Into<Vec<u8>>) -> Result<Self, CoordinationError> {
        let name = name.into();
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(CoordinationError::Configuration);
        }
        Ok(Self(name))
    }
    /// The name's bytes.
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

/// One attempt to take a counter's number for a key such as a ticket or an order id. Build it
/// again from the application's own rows to retry: the same key and attempt answer `First` with
/// the attempt's number, another attempt on a key in force answers `Taken`, and any claim after
/// its window is refused as `Expired` and takes no number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claim {
    pub(crate) key: Commitment,
    pub(crate) attempt: Commitment,
    pub(crate) expires: u64,
}
impl Claim {
    /// An attempt on `key` named by the application's own id for this try, in force for
    /// `CLAIM_WINDOW` from `created`, the time stored with the row it claims for, so a retry
    /// rebuilt from that row never outlives the window; `Claim::new(id, id, created)` takes a
    /// number no other key shares.
    pub fn new(key: impl AsRef<[u8]>, attempt: impl AsRef<[u8]>, created: SystemTime) -> Self {
        Self {
            key: rocky_core::commitment("rocky.counter-claim-key.v2", key.as_ref()),
            attempt: rocky_core::commitment("rocky.counter-claim-attempt.v1", attempt.as_ref()),
            expires: unix_seconds(created).saturating_add(CLAIM_WINDOW),
        }
    }
    /// Keeps the key in force until `at` instead, a time stored with the row like `created`, at
    /// most `MAX_CLAIM_LIFETIME` ahead of the coordinator's clock.
    pub fn expires(self, at: SystemTime) -> Self {
        Self {
            expires: unix_seconds(at),
            ..self
        }
    }
}
#[cfg(test)]
impl Claim {
    pub(crate) fn unique_until(expires: u64) -> Self {
        let id: Commitment = rocky_platform::entropy::random();
        Self::new(id, id, UNIX_EPOCH).expires(UNIX_EPOCH + std::time::Duration::from_secs(expires))
    }
    pub(crate) fn keyed_for_test((key, attempt): (Commitment, u8), expires: u64) -> Self {
        Self {
            key,
            attempt: [attempt; 32],
            expires,
        }
    }
    pub(crate) fn fixed(key: u8, attempt: u8, expires: u64) -> Self {
        Self {
            key: [key; 32],
            attempt: [attempt; 32],
            expires,
        }
    }
}
fn unix_seconds(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// What a claim found: the number it holds, or the number another attempt already took.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Claimed {
    /// This attempt holds the key's number, whether it took it now or on an earlier try.
    First(i64),
    /// Another attempt already holds this key, with this number.
    Taken(i64),
}
impl Claimed {
    /// The key's number, from 1 up to `i64::MAX`, whichever attempt took it.
    pub fn number(self) -> i64 {
        match self {
            Self::First(number) | Self::Taken(number) => number,
        }
    }
}
