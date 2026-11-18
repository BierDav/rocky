use crate::{Commitment, RowIdentity, WriterId, commitment};
use serde::{Deserialize, Serialize};

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
        let mut bytes = b"rocky.row-creation-statement.v3".to_vec();
        bytes.extend(self.manifest);
        bytes.extend(self.table);
        bytes.extend(self.identity.to_bytes());
        bytes.extend(self.stamp.clock.to_le_bytes());
        bytes.extend(self.stamp.author.as_bytes());
        bytes.extend(self.stamp.sequence.to_le_bytes());
        bytes.extend(self.bindings);
        bytes
    }

    pub fn commitment(&self) -> Commitment {
        commitment("rocky.creation-provenance.v1", &self.canonical_bytes())
    }
}

pub const MAX_CLOCK: u64 = i64::MAX as u64;

pub const MAX_CLOCK_AHEAD: std::time::Duration = std::time::Duration::from_secs(300);

pub fn bounded_clock<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let clock = u64::deserialize(deserializer)?;
    if clock > MAX_CLOCK {
        return Err(serde::de::Error::custom("clock beyond its bound"));
    }
    Ok(clock)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(try_from = "(u64, WriterId, u64)", into = "(u64, WriterId, u64)")]
pub struct SecureStamp {
    pub clock: u64,
    pub author: WriterId,
    pub sequence: u64,
}

impl TryFrom<(u64, WriterId, u64)> for SecureStamp {
    type Error = &'static str;
    fn try_from((clock, author, sequence): (u64, WriterId, u64)) -> Result<Self, Self::Error> {
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

impl From<SecureStamp> for (u64, WriterId, u64) {
    fn from(stamp: SecureStamp) -> Self {
        (stamp.clock, stamp.author, stamp.sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(clock: u64, writer: u8, sequence: u64) -> SecureStamp {
        SecureStamp {
            clock,
            author: WriterId::from_bytes([writer; 32]),
            sequence,
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn stamps_order_by_clock_then_writer_then_sequence() {
        assert!(stamp(1, 9, 9) < stamp(2, 0, 0));
        assert!(stamp(2, 1, 9) < stamp(2, 2, 0));
        assert!(stamp(2, 2, 1) < stamp(2, 2, 2));
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_clock_past_sqlite_signed_integers_is_refused_when_decoded() {
        assert_eq!(MAX_CLOCK, i64::MAX as u64);
        let largest = serde_json::to_string(&stamp(MAX_CLOCK, 1, 1)).unwrap();
        assert!(serde_json::from_str::<SecureStamp>(&largest).is_ok());
        let past = largest.replacen(&MAX_CLOCK.to_string(), &(MAX_CLOCK + 1).to_string(), 1);
        assert!(serde_json::from_str::<SecureStamp>(&past).is_err());
    }
}
