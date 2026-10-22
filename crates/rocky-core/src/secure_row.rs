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

/// Register order within a domain; author numbers are unique in that domain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(from = "(u64, u32, u64)", into = "(u64, u32, u64)")]
pub struct SecureStamp {
    pub clock: u64,
    pub author: u32,
    pub sequence: u64,
}

impl From<(u64, u32, u64)> for SecureStamp {
    fn from((clock, author, sequence): (u64, u32, u64)) -> Self {
        Self {
            clock,
            author,
            sequence,
        }
    }
}

impl From<SecureStamp> for (u64, u32, u64) {
    fn from(stamp: SecureStamp) -> Self {
        (stamp.clock, stamp.author, stamp.sequence)
    }
}
