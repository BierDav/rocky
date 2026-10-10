//! Opaque peer references for durable receipts and local routing metadata.
use crate::PublicKey;
use rocky_core::{Commitment, commitment};
use serde::{Deserialize, Serialize};

/// A one-way reference to a node, never a signing or transport key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeerId(Commitment);

impl PeerId {
    pub fn of(node: PublicKey) -> Self {
        Self(commitment("rocky.secure-node.v1", node.as_bytes()))
    }

    pub fn node_id(self) -> rocky_core::NodeId {
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&self.0[..16]);
        rocky_core::NodeId(bytes)
    }

    pub fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
    pub fn as_bytes(&self) -> &Commitment {
        &self.0
    }

    pub fn from_bytes(bytes: Commitment) -> Self {
        Self(bytes)
    }
}

impl Serialize for PeerId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}
impl<'de> Deserialize<'de> for PeerId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(serde::de::Error::custom("invalid peer reference"));
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_is_stable_distinct_and_contains_no_public_key() {
        let a = crate::PrivateKey::from_bytes(&[1; 32]).public_key();
        let b = crate::PrivateKey::from_bytes(&[2; 32]).public_key();
        assert_eq!(PeerId::of(a), PeerId::of(a));
        assert_ne!(PeerId::of(a), PeerId::of(b));
        assert_ne!(PeerId::of(a).as_bytes(), a.as_bytes());
    }
}
