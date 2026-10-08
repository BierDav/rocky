//! Issuer-declared mergeable cuts; `docs/runtime/mergeable-horizon.md`.
use crate::{
    Error, PrivateKey, PublicKey, Signature, decode_bounded, denied, encode, signing_bytes,
};
use rocky_core::{Commitment, ResourceScope};
use serde::{Deserialize, Serialize};

pub const HORIZON_FORMAT: u32 = 2;
pub const HORIZON_WAIT_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorizonLog {
    pub signer: PublicKey,
    pub sequence: u64,
    pub head: Commitment,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorizonCut {
    pub scope: ResourceScope,
    pub logs: Vec<HorizonLog>,
    pub state: Commitment,
}
/// Identity sequences survive existence-record pruning, per scope and author.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistenceIdentity {
    pub author: u32,
    pub signer: PublicKey,
    pub sequence: u32,
}
/// Compact live existence state authenticated by the issuer's horizon signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistenceBase {
    pub scope: ResourceScope,
    pub rows: u64,
    pub digest: Commitment,
    pub identities: Vec<ExistenceIdentity>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorizonPeer {
    pub node: PublicKey,
    /// Minimum expiry of the admitted credential chain.
    pub expires: u64,
}
impl HorizonPeer {
    pub fn deadline(&self, issued: u64) -> u64 {
        self.expires
            .min(issued.saturating_add(HORIZON_WAIT_SECONDS))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorizonStatement {
    pub format: u32,
    pub manifest: Commitment,
    pub policy_epoch: u64,
    pub scope: ResourceScope,
    pub sequence: u64,
    pub previous: Option<Commitment>,
    pub issued: u64,
    pub logs: Vec<HorizonLog>,
    /// Digest of the materialized input, including hidden rows and tombstones.
    pub state: Commitment,
    pub existence: Vec<HorizonCut>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<ExistenceBase>,
    /// Parent row cuts in the same reader scope; logged parents need only liveness.
    pub related: Vec<HorizonCut>,
    pub peers: Vec<HorizonPeer>,
}
impl HorizonStatement {
    fn validate(&self) -> Result<(), Error> {
        let logs_valid = |logs: &[HorizonLog]| {
            logs.len() <= 256
                && logs.windows(2).all(|p| p[0].signer < p[1].signer)
                && logs.iter().all(|log| log.sequence <= i64::MAX as u64)
        };
        if ![1, HORIZON_FORMAT].contains(&self.format)
            || (self.format == 1 && !self.bases.is_empty())
            || (self.format == 2
                && (self.bases.len() != self.existence.len()
                    || self.bases.iter().zip(&self.existence).any(|(base, cut)| {
                        base.scope != cut.scope
                            || !encode(base).is_ok_and(|bytes| {
                                rocky_core::commitment("rocky.horizon-existence-state.v2", &bytes)
                                    == cut.state
                            })
                            || base.identities.len() > 256
                            || base
                                .identities
                                .windows(2)
                                .any(|pair| pair[0].author >= pair[1].author)
                            || base.identities.iter().any(|mark| {
                                rocky_core::RowIdentity::new(mark.author, mark.sequence).is_err()
                            })
                    })))
            || self.sequence == 0
            || (self.sequence == 1) != self.previous.is_none()
            || !logs_valid(&self.logs)
            || self.existence.len() > 256
            || !self.existence.windows(2).all(|p| p[0].scope < p[1].scope)
            || self
                .existence
                .iter()
                .any(|cut| cut.scope.domain != self.scope.domain || !logs_valid(&cut.logs))
            || self.related.len() > 256
            || !self.related.windows(2).all(|p| p[0].scope < p[1].scope)
            || self
                .related
                .iter()
                .any(|cut| cut.scope.domain != self.scope.domain || !logs_valid(&cut.logs))
            || self.peers.len() > 64
            || !self.peers.windows(2).all(|p| p[0].node < p[1].node)
            || self.peers.iter().any(|peer| peer.expires <= self.issued)
        {
            return Err(denied(
                "unsupported or invalid horizon statement; upgrade the runtime",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedHorizon {
    pub statement: HorizonStatement,
    signature: Signature,
}
impl SignedHorizon {
    pub fn sign(statement: HorizonStatement, issuer: &PrivateKey) -> Result<Self, Error> {
        statement.validate()?;
        let signature = issuer.sign(&signing_bytes("rocky.horizon-statement.v1", &statement)?);
        let signed = Self {
            statement,
            signature,
        };
        signed.to_bytes()?;
        Ok(signed)
    }
    pub fn verify(&self, issuer: PublicKey) -> Result<(), Error> {
        self.statement.validate()?;
        if !issuer.verify(
            &signing_bytes("rocky.horizon-statement.v1", &self.statement)?,
            &self.signature,
        ) {
            return Err(denied("invalid horizon issuer signature"));
        }
        Ok(())
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.horizon-statement.id.v1",
            &self.to_bytes()?,
        ))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > crate::MAX_CREDENTIAL_BYTES {
            return Err(denied("horizon statement quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, crate::MAX_CREDENTIAL_BYTES)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HorizonAck {
    pub horizon: Commitment,
    pub node: PublicKey,
    signature: Signature,
}
impl HorizonAck {
    pub fn sign(horizon: Commitment, node: &PrivateKey) -> Result<Self, Error> {
        let public = node.public_key();
        Ok(Self {
            horizon,
            node: public,
            signature: node.sign(&signing_bytes("rocky.horizon-ack.v1", &(horizon, public))?),
        })
    }
    pub fn verify(&self) -> Result<(), Error> {
        if !self.node.verify(
            &signing_bytes("rocky.horizon-ack.v1", &(self.horizon, self.node))?,
            &self.signature,
        ) {
            return Err(denied("invalid horizon acknowledgment"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn behind_deadline_uses_the_shorter_bound_without_overflow() {
        let node = PrivateKey::new().public_key();
        assert_eq!(HorizonPeer { node, expires: 110 }.deadline(100), 110);
        assert_eq!(
            HorizonPeer {
                node,
                expires: u64::MAX
            }
            .deadline(100),
            100 + HORIZON_WAIT_SECONDS
        );
        assert_eq!(
            HorizonPeer {
                node,
                expires: u64::MAX
            }
            .deadline(u64::MAX - 1),
            u64::MAX
        );
        let ack = HorizonAck::sign([7; 32], &PrivateKey::new()).unwrap();
        ack.verify().unwrap();
        let mut forged = ack;
        forged.horizon = [8; 32];
        assert!(forged.verify().is_err());
    }
}
