use crate::{Commitment, RowIdentity, commitment};
use serde::{Deserialize, Serialize};

/// The original birth statement, independent of its transport and signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreationStatement {
    pub manifest: Commitment,
    pub table: Commitment,
    pub identity: RowIdentity,
    pub stamp: SecureStamp,
    pub bindings: Commitment,
}

impl CreationStatement {
    pub fn derived(
        manifest: Commitment,
        table: Commitment,
        identity: RowIdentity,
        stamp: SecureStamp,
        bindings: Commitment,
    ) -> Self {
        Self {
            manifest,
            table,
            identity,
            stamp,
            bindings,
        }
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = b"rocky.row-creation-statement.v2".to_vec();
        bytes.extend(self.manifest);
        bytes.extend(self.table);
        bytes.extend(self.identity.integer().to_le_bytes());
        bytes.extend(self.stamp.clock.to_le_bytes());
        bytes.extend(self.stamp.author.to_le_bytes());
        bytes.extend(self.stamp.sequence.to_le_bytes());
        bytes.extend(self.bindings);
        bytes
    }

    pub fn commitment(&self) -> Commitment {
        commitment("rocky.creation-provenance.v1", &self.canonical_bytes())
    }
}

/// The largest valid hybrid clock: SQLite stores clocks as signed integers,
/// and its order must be the stamps' order. Every replica refuses a larger
/// one alike, so the refusal is permanent.
pub const MAX_CLOCK: u64 = i64::MAX as u64;

/// How far a received stamp may be ahead of this device's clock before
/// its frame waits: the most skew a peer's clock imposes on hybrid stamps
/// and last-writer-wins order here.
pub const MAX_CLOCK_AHEAD: std::time::Duration = std::time::Duration::from_secs(300);

/// Decodes a hybrid clock, refusing one above `MAX_CLOCK`.
pub fn bounded_clock<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let clock = u64::deserialize(deserializer)?;
    if clock > MAX_CLOCK {
        return Err(serde::de::Error::custom("clock beyond its bound"));
    }
    Ok(clock)
}

/// Register order within a domain; author numbers are unique in that domain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(try_from = "(u64, u32, u64)", into = "(u64, u32, u64)")]
pub struct SecureStamp {
    pub clock: u64,
    pub author: u32,
    pub sequence: u64,
}

impl TryFrom<(u64, u32, u64)> for SecureStamp {
    type Error = &'static str;
    fn try_from((clock, author, sequence): (u64, u32, u64)) -> Result<Self, Self::Error> {
        if clock > MAX_CLOCK {
            return Err("clock beyond its bound");
        }
        Ok(Self {
            clock,
            author,
            sequence,
        })
    }
}

impl From<SecureStamp> for (u64, u32, u64) {
    fn from(stamp: SecureStamp) -> Self {
        (stamp.clock, stamp.author, stamp.sequence)
    }
}
