//! Authenticated finite migration runs; ciphertext bundle membership stays private.
use crate::{Commitment, NodeKey, Scope, TipId, commitment};
use serde::{Deserialize, Serialize};

pub const MIGRATION_PUBLICATION_FORMAT: u32 = 1;
pub const MIGRATION_PROOF_FORMAT: u32 = 1;
pub const MAX_MIGRATION_PROOF_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationPublicationStatement {
    pub format: u32,
    pub scope: Scope,
    pub tip: TipId,
    pub opening: Commitment,
    pub parent: Commitment,
    pub schema: Commitment,
    pub publication: Commitment,
    pub writer: NodeKey,
    pub author: u32,
    pub frames: u64,
    pub bytes: u64,
    pub root: Commitment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationSum {
    pub hash: Commitment,
    pub frames: u64,
    pub bytes: u64,
}

/// Siblings are ordered from the leaf toward the root. Tree shape follows
/// `migration_split`; no padding or uncommitted directions are accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationProof {
    pub format: u32,
    pub index: u64,
    pub siblings: Vec<MigrationSum>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigrationError(pub &'static str);
impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for MigrationError {}

/// Largest power of two strictly below `frames`, for a non-leaf tree.
pub fn migration_split(frames: u64) -> Result<u64, MigrationError> {
    if frames < 2 {
        return Err(MigrationError("migration tree is a leaf"));
    }
    Ok(1u64 << (63 - (frames - 1).leading_zeros()))
}
impl MigrationPublicationStatement {
    pub fn validate(&self) -> Result<(), MigrationError> {
        if self.format != MIGRATION_PUBLICATION_FORMAT {
            return Err(MigrationError("unsupported migration descriptor format"));
        }
        if self.author == 0
            || self.author > 0x7fff_ffff
            || self.publication != commitment("rocky.sdk.migration.v3", &self.tip.0)
            || (self.frames == 0) != (self.bytes == 0)
            || self.bytes < self.frames
        {
            return Err(MigrationError("invalid migration descriptor"));
        }
        if self.frames == 0 && self.root != self.empty().hash {
            return Err(MigrationError("invalid empty migration root"));
        }
        Ok(())
    }
    /// Fixed-width little-endian counters, in declaration order, excluding root.
    pub fn context(&self) -> Commitment {
        let mut bytes = Vec::with_capacity(280);
        bytes.extend(self.format.to_le_bytes());
        bytes.extend(self.scope.mesh.0);
        bytes.extend(self.scope.app.0);
        bytes.extend(self.scope.domain.0);
        for value in [
            self.tip.0,
            self.opening,
            self.parent,
            self.schema,
            self.publication,
            self.writer,
        ] {
            bytes.extend(value);
        }
        bytes.extend(self.author.to_le_bytes());
        bytes.extend(self.frames.to_le_bytes());
        bytes.extend(self.bytes.to_le_bytes());
        commitment("rocky.migration.context.v1", &bytes)
    }
    pub fn empty(&self) -> MigrationSum {
        MigrationSum {
            hash: commitment("rocky.migration.empty.v1", &self.context()),
            frames: 0,
            bytes: 0,
        }
    }
    pub fn leaf(
        &self,
        index: u64,
        hash: Commitment,
        bytes: u64,
    ) -> Result<MigrationSum, MigrationError> {
        if index >= self.frames || bytes == 0 {
            return Err(MigrationError("invalid migration leaf"));
        }
        let mut value = Vec::with_capacity(80);
        value.extend(self.context());
        value.extend(index.to_le_bytes());
        value.extend(hash);
        value.extend(bytes.to_le_bytes());
        Ok(MigrationSum {
            hash: commitment("rocky.migration.leaf.v1", &value),
            frames: 1,
            bytes,
        })
    }
    pub fn verify(
        &self,
        proof: &MigrationProof,
        hash: Commitment,
        bytes: u64,
    ) -> Result<(), MigrationError> {
        self.validate()?;
        if proof.format != MIGRATION_PROOF_FORMAT
            || proof.index >= self.frames
            || proof.siblings.len() > MAX_MIGRATION_PROOF_DEPTH
        {
            return Err(MigrationError("invalid migration inclusion proof"));
        }
        // At most 64 entries, independent of the advertised extent.
        let (mut count, mut index) = (self.frames, proof.index);
        let mut shape = Vec::new();
        while count > 1 {
            let left = migration_split(count)?;
            if index < left {
                shape.push((false, count - left));
                count = left;
            } else {
                shape.push((true, left));
                index -= left;
                count -= left;
            }
        }
        if shape.len() != proof.siblings.len() {
            return Err(MigrationError("migration proof shape"));
        }
        let mut sum = self.leaf(proof.index, hash, bytes)?;
        for ((right, expected), sibling) in shape.into_iter().rev().zip(&proof.siblings) {
            if sibling.frames != expected || sibling.bytes < sibling.frames {
                return Err(MigrationError("migration sibling extent"));
            }
            sum = if right {
                MigrationSum::join(*sibling, sum)?
            } else {
                MigrationSum::join(sum, *sibling)?
            };
        }
        if sum
            != (MigrationSum {
                hash: self.root,
                frames: self.frames,
                bytes: self.bytes,
            })
        {
            return Err(MigrationError("migration inclusion mismatch"));
        }
        Ok(())
    }
}
impl MigrationSum {
    pub fn join(left: Self, right: Self) -> Result<Self, MigrationError> {
        if left.frames == 0 || right.frames == 0 {
            return Err(MigrationError("empty migration child"));
        }
        let frames = left
            .frames
            .checked_add(right.frames)
            .ok_or(MigrationError("migration count overflow"))?;
        let bytes = left
            .bytes
            .checked_add(right.bytes)
            .ok_or(MigrationError("migration bytes overflow"))?;
        let mut value = Vec::with_capacity(96);
        for child in [left, right] {
            value.extend(child.hash);
            value.extend(child.frames.to_le_bytes());
            value.extend(child.bytes.to_le_bytes());
        }
        Ok(Self {
            hash: commitment("rocky.migration.node.v1", &value),
            frames,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn statement(frames: u64) -> MigrationPublicationStatement {
        let tip = TipId([3; 32]);
        MigrationPublicationStatement {
            format: 1,
            scope: Scope {
                mesh: crate::MeshId([1; 16]),
                app: crate::AppId([2; 16]),
                domain: crate::DomainId([3; 16]),
            },
            tip,
            opening: [4; 32],
            parent: [5; 32],
            schema: [6; 32],
            publication: commitment("rocky.sdk.migration.v3", &tip.0),
            writer: [7; 32],
            author: 1,
            frames,
            bytes: frames * 10,
            root: [0; 32],
        }
    }
    fn tree(
        s: &MigrationPublicationStatement,
        start: u64,
        n: u64,
        index: u64,
        proof: &mut Vec<MigrationSum>,
    ) -> MigrationSum {
        if n == 1 {
            return s.leaf(start, [start as u8; 32], 10).unwrap();
        }
        let split = migration_split(n).unwrap();
        let (left, right) = if index < start + split {
            let left = tree(s, start, split, index, proof);
            let right = tree(s, start + split, n - split, u64::MAX, &mut vec![]);
            proof.push(right);
            (left, right)
        } else {
            let left = tree(s, start, split, u64::MAX, &mut vec![]);
            let right = tree(s, start + split, n - split, index, proof);
            proof.push(left);
            (left, right)
        };
        MigrationSum::join(left, right).unwrap()
    }
    #[test]
    fn proofs_bind_every_position_exact_hash_length_and_tree_shape() {
        for n in 1..10 {
            for index in 0..n {
                let mut s = statement(n);
                let mut siblings = vec![];
                s.root = tree(&s, 0, n, index, &mut siblings).hash;
                let p = MigrationProof {
                    format: 1,
                    index,
                    siblings,
                };
                s.verify(&p, [index as u8; 32], 10).unwrap();
                assert!(s.verify(&p, [99; 32], 10).is_err());
                assert!(s.verify(&p, [index as u8; 32], 11).is_err());
                let mut malformed = p.clone();
                malformed.siblings.push(s.empty());
                assert!(s.verify(&malformed, [index as u8; 32], 10).is_err());
            }
        }
    }
    #[test]
    fn empty_and_hostile_extents_are_bounded_and_overflow_is_refused() {
        let mut s = statement(0);
        s.root = s.empty().hash;
        s.validate().unwrap();
        assert!(
            s.verify(
                &MigrationProof {
                    format: 1,
                    index: 0,
                    siblings: vec![]
                },
                [0; 32],
                1
            )
            .is_err()
        );
        s.frames = u64::MAX;
        s.bytes = u64::MAX;
        assert!(
            s.verify(
                &MigrationProof {
                    format: 1,
                    index: 0,
                    siblings: vec![]
                },
                [0; 32],
                1
            )
            .is_err()
        );
        assert!(
            MigrationSum::join(
                MigrationSum {
                    hash: [0; 32],
                    frames: 1,
                    bytes: u64::MAX
                },
                MigrationSum {
                    hash: [0; 32],
                    frames: 1,
                    bytes: 1
                }
            )
            .is_err()
        );
    }
}
