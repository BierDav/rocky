//! Carry records: a coordinated write that a `continue` seal aborted keeps
//! its frames, which sign the tip and entry they were staged at. Its author
//! signs where the write committed instead, so every device locates those
//! frames in the continuation's log. See `docs/runtime/coordinated-writes.md`.
use crate::{
    CredentialChain, Error, PrivateKey, PublicKey, RevocationSet, Signature, TipGraph,
    decode_bounded, denied, encode, signing_bytes,
};
use rocky_core::{Commitment, Scope, TipId};
use serde::{Deserialize, Serialize};

pub const CARRY_FORMAT: u32 = 1;
/// Frames one carried bundle names: the bundle frame cap.
pub const MAX_CARRY_FRAMES: usize = 64;
/// Largest encoded carry record, its node chain included.
pub const MAX_CARRY_BYTES: usize = 16 * 1024;
const CARRY: &str = "rocky.entry-carry.v1";

/// The bundle of frames `ids`, signed at entry `from.1` of tip `from.0`'s
/// log, committed at `position` of the continuation `tip`'s log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarryStatement {
    pub format: u32,
    pub scope: Scope,
    pub from: (TipId, u64),
    pub tip: TipId,
    pub position: u64,
    /// Sorted and unique.
    pub ids: Vec<[u8; 32]>,
}
impl CarryStatement {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != CARRY_FORMAT {
            return Err(denied("unsupported carry record format"));
        }
        // A carried write is retried only as its continuation's first entry.
        if self.position != 1
            || self.from.0 == self.tip
            || self.from.1 == 0
            || self.ids.is_empty()
            || self.ids.len() > MAX_CARRY_FRAMES
            || self.ids.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(denied("invalid carry record"));
        }
        Ok(())
    }
}

/// A carry record signed by the node that authored the carried frames,
/// under its trusted node chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCarry {
    pub statement: CarryStatement,
    pub signer: PublicKey,
    pub credentials: CredentialChain,
    signature: Signature,
}
impl SignedCarry {
    pub fn sign(
        statement: CarryStatement,
        node: &PrivateKey,
        credentials: CredentialChain,
    ) -> Result<Self, Error> {
        statement.validate()?;
        let signature = node.sign(&signing_bytes(CARRY, &statement)?);
        let carry = Self {
            statement,
            signer: node.public_key(),
            credentials,
            signature,
        };
        carry.to_bytes()?;
        Ok(carry)
    }
    /// Checks the signature and that a node of a trusted identity of the
    /// domain signed it, as for a terminal acknowledgment.
    pub fn verify_signature(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<(), Error> {
        self.statement.validate()?;
        self.credentials
            .trusted_node(publisher, self.statement.scope, self.signer, current)?;
        if !self
            .signer
            .verify(&signing_bytes(CARRY, &self.statement)?, &self.signature)
        {
            return Err(denied("invalid carry record signature"));
        }
        Ok(())
    }
    /// Whether this record names exactly the frames `ids`, in any order.
    pub fn names(&self, ids: &[[u8; 32]]) -> bool {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        ids == self.statement.ids
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.entry-carry.id.v1",
            &self.to_bytes()?,
        ))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_CARRY_BYTES {
            return Err(denied("carry record quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let carry: Self = decode_bounded(bytes, MAX_CARRY_BYTES)?;
        carry.statement.validate()?;
        Ok(carry)
    }
}

impl TipGraph {
    /// Verifies a carry record the graph holds: its signer, a structural
    /// continue path from the signed tip to the continuation, never a
    /// branch opening, and no other record for that continuation's entry.
    pub fn verify_carry(
        &self,
        carry: &SignedCarry,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<(), Error> {
        self.check_scope(carry.statement.scope)?;
        carry.verify_signature(publisher, current)?;
        if !self.continues_any(carry.statement.from.0, carry.statement.tip)? {
            return Err(denied("a carry record names a tip its write cannot reach"));
        }
        let id = carry.id()?;
        for other in &self.carries {
            if other.statement.tip == carry.statement.tip
                && other.statement.position == carry.statement.position
                && other.statement.ids != carry.statement.ids
                && other.id()? != id
            {
                return Err(denied("conflicting carry records for one entry"));
            }
        }
        Ok(())
    }
    /// The effective carry record of the frame `id`, which `author` signed
    /// on `from`: of the records its author signed naming it, the one
    /// deepest along its continue path, since a shallower one is of a carry
    /// that aborted. A tie between unrelated tips resolves to the lower
    /// record digest.
    pub fn carry_of(
        &self,
        id: &[u8; 32],
        author: PublicKey,
        from: TipId,
    ) -> Result<Option<&SignedCarry>, Error> {
        let mut winner: Option<(&SignedCarry, Commitment)> = None;
        for carry in &self.carries {
            if carry.statement.scope != self.scope
                || carry.signer != author
                || carry.statement.from.0 != from
                || carry.statement.ids.binary_search(id).is_err()
            {
                continue;
            }
            let digest = carry.id()?;
            winner = match winner {
                None => Some((carry, digest)),
                Some((known, known_digest)) => {
                    let (a, b) = (known.statement.tip, carry.statement.tip);
                    let deeper = if a == b {
                        digest < known_digest
                    } else if self.continues_any(a, b).unwrap_or(false) {
                        true
                    } else if self.continues_any(b, a).unwrap_or(false) {
                        false
                    } else {
                        digest < known_digest
                    };
                    Some(if deeper {
                        (carry, digest)
                    } else {
                        (known, known_digest)
                    })
                }
            };
        }
        Ok(winner.map(|(carry, _)| carry))
    }
}
