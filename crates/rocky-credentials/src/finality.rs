//! Final seals: trusted nodes acknowledge the terminal entry a tip's
//! coordinated log committed, and a record carrying enough of them makes
//! that seal final for every member, without a coordinator call. See
//! `docs/runtime/seal-finality.md`.
use crate::{
    CredentialChain, Error, PrivateKey, PublicKey, RevocationSet, Signature, decode_bounded,
    denied, encode, signing_bytes,
};
use rocky_core::{Commitment, PrincipalKind, Scope, TipId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const TERMINAL_ACK_FORMAT: u32 = 1;
pub const SEAL_FINALITY_FORMAT: u32 = 1;
/// The coordinator's in-sync cap: no record needs more acknowledgments.
pub const MAX_FINALITY_ACKS: usize = 32;
/// Largest encoded acknowledgment, its node chain included.
pub const MAX_TERMINAL_ACK_BYTES: usize = 16 * 1024;
/// Largest encoded finality record.
pub const MAX_FINALITY_BYTES: usize = MAX_FINALITY_ACKS * MAX_TERMINAL_ACK_BYTES + 1024;

/// What a trusted node read from the coordinator's head of `(scope, tip)`'s
/// ended log: the terminal entry's position and running hash, the seal it
/// committed and that seal's continuation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAckStatement {
    pub format: u32,
    pub scope: Scope,
    pub tip: TipId,
    pub position: u64,
    pub running: Commitment,
    pub seal: Commitment,
    pub continuation: Option<TipId>,
}

/// A terminal acknowledgment signed by a trusted node under its chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAck {
    pub statement: TerminalAckStatement,
    pub signer: PublicKey,
    pub credentials: CredentialChain,
    signature: Signature,
}
const TERMINAL_ACK: &str = "rocky.terminal-ack.v1";
impl TerminalAck {
    pub fn sign(
        statement: TerminalAckStatement,
        node: &PrivateKey,
        credentials: CredentialChain,
    ) -> Result<Self, Error> {
        if statement.format != TERMINAL_ACK_FORMAT {
            return Err(denied("unsupported terminal acknowledgment format"));
        }
        let signature = node.sign(&signing_bytes(TERMINAL_ACK, &statement)?);
        let ack = Self {
            statement,
            signer: node.public_key(),
            credentials,
            signature,
        };
        ack.to_bytes()?;
        Ok(ack)
    }
    /// Checks the signature and that the signer is a node of an identity
    /// holding the domain's trusted grant; returns that identity.
    pub fn verify(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<PublicKey, Error> {
        if self.statement.format != TERMINAL_ACK_FORMAT {
            return Err(denied("unsupported terminal acknowledgment format"));
        }
        let identity =
            self.credentials
                .trusted_node(publisher, self.statement.scope, self.signer, current)?;
        if !self.signer.verify(
            &signing_bytes(TERMINAL_ACK, &self.statement)?,
            &self.signature,
        ) {
            return Err(denied("invalid terminal acknowledgment signature"));
        }
        Ok(identity)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_TERMINAL_ACK_BYTES {
            return Err(denied("terminal acknowledgment quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let ack: Self = decode_bounded(bytes, MAX_TERMINAL_ACK_BYTES)?;
        if ack.statement.format != TERMINAL_ACK_FORMAT {
            return Err(denied("unsupported terminal acknowledgment format"));
        }
        Ok(ack)
    }
}

/// A seal's finality: acknowledgments of the terminal entry that committed
/// it, by distinct trusted nodes. It needs no signature of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealFinality {
    pub format: u32,
    pub scope: Scope,
    pub seal: Commitment,
    pub acknowledgments: Vec<TerminalAck>,
}
impl SealFinality {
    /// Orders the acknowledgments by signer, so every publisher of the same
    /// set produces the same record.
    pub fn new(scope: Scope, seal: Commitment, mut acknowledgments: Vec<TerminalAck>) -> Self {
        acknowledgments.sort_by_key(|ack| *ack.signer.as_bytes());
        acknowledgments.dedup_by_key(|ack| *ack.signer.as_bytes());
        acknowledgments.truncate(MAX_FINALITY_ACKS);
        Self {
            format: SEAL_FINALITY_FORMAT,
            scope,
            seal,
            acknowledgments,
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != SEAL_FINALITY_FORMAT {
            return Err(denied("unsupported seal finality format"));
        }
        if self.acknowledgments.is_empty()
            || self.acknowledgments.len() > MAX_FINALITY_ACKS
            || self
                .acknowledgments
                .windows(2)
                .any(|pair| pair[0].signer.as_bytes() >= pair[1].signer.as_bytes())
        {
            return Err(denied("invalid seal finality acknowledgments"));
        }
        let first = &self.acknowledgments[0].statement;
        for ack in &self.acknowledgments {
            let statement = &ack.statement;
            if statement.scope != self.scope
                || statement.seal != self.seal
                || statement.tip != first.tip
                || statement.position != first.position
                || statement.running != first.running
                || statement.continuation != first.continuation
            {
                return Err(denied("seal finality acknowledgments disagree"));
            }
        }
        Ok(())
    }
    /// The tip and continuation every acknowledgment names.
    pub fn terminal(&self) -> Option<(TipId, Option<TipId>)> {
        self.acknowledgments
            .first()
            .map(|ack| (ack.statement.tip, ack.statement.continuation))
    }
    /// Distinct nodes whose acknowledgments verify, optionally under a
    /// revocation set at a time.
    pub fn verified_nodes(
        &self,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<usize, Error> {
        self.validate()?;
        let mut nodes = BTreeSet::new();
        for ack in &self.acknowledgments {
            ack.verify(publisher, current)?;
            nodes.insert(*ack.signer.as_bytes());
        }
        Ok(nodes.len())
    }
    /// Acknowledgments whose chains `revocations` ends.
    pub fn revoked(&self, revocations: &RevocationSet) -> Result<usize, Error> {
        let mut count = 0;
        for ack in &self.acknowledgments {
            for link in &ack.credentials.0 {
                if revocations.ends(link)? {
                    count += 1;
                    break;
                }
            }
        }
        Ok(count)
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.seal-finality.id.v1",
            &self.to_bytes()?,
        ))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_FINALITY_BYTES {
            return Err(denied("seal finality quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let record: Self = decode_bounded(bytes, MAX_FINALITY_BYTES)?;
        record.validate()?;
        Ok(record)
    }
}

impl CredentialChain {
    /// The identity whose trusted grant for `scope` certifies the node
    /// `node`: exactly the owner-direct trusted link and its node
    /// certificate.
    pub fn trusted_node(
        &self,
        publisher: PublicKey,
        scope: Scope,
        node: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<PublicKey, Error> {
        let leaf = self.links(publisher, current)?;
        let identity = &self
            .0
            .first()
            .ok_or_else(|| denied("credential depth"))?
            .capability;
        if self.0.len() != 2
            || leaf.kind != PrincipalKind::Node
            || leaf.delegations != 0
            || leaf.subject != node
            || leaf.extended()?.node != Some(crate::PeerId::of(node))
            || identity.kind != PrincipalKind::User
            || identity.extended()?.trusted != Some(scope)
        {
            return Err(denied("not a node of a trusted identity of this domain"));
        }
        Ok(identity.subject)
    }
}
