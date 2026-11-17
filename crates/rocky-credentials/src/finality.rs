//! Final seals: trusted nodes acknowledge the terminal entry a tip's
//! coordinated log committed, and a record carrying enough of them makes
//! that seal final for every member, without a coordinator call.
use crate::{
    ChainLink, CredentialChain, Error, PrivateKey, PublicKey, RevocationSet, Signature,
    decode_bounded, denied, encode, signing_bytes,
};
use rocky_core::{Commitment, CoordinatorIdentity, PrincipalKind, Scope, TipId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const TERMINAL_ACK_FORMAT: u32 = 4;
pub const SEAL_FINALITY_FORMAT: u32 = 2;
/// The coordinator's in-sync cap: no record needs more acknowledgments.
pub const MAX_FINALITY_ACKS: usize = 15;
/// Largest encoded acknowledgment, its node chain included.
pub const MAX_TERMINAL_ACK_BYTES: usize = 16 * 1024;
/// Largest encoded finality record.
pub const MAX_FINALITY_BYTES: usize = MAX_FINALITY_ACKS * MAX_TERMINAL_ACK_BYTES + 1024;

/// The format field of a versioned record, read without the rest of its
/// shape, which another release's record need not have.
#[derive(Deserialize)]
pub struct NamedFormat {
    pub format: u32,
}
/// An acknowledgment's statement format, read the same way.
#[derive(Deserialize)]
struct NamedAckFormat {
    statement: NamedFormat,
}
fn unsupported_ack(format: u32) -> Error {
    Error::UnsupportedFormat {
        record: "terminal acknowledgment",
        format,
        supported: TERMINAL_ACK_FORMAT,
    }
}
/// The refusal of a finality record of `format`, another release's.
pub fn unsupported_finality(format: u32) -> Error {
    Error::UnsupportedFormat {
        record: "seal finality record",
        format,
        supported: SEAL_FINALITY_FORMAT,
    }
}

/// What a trusted node read from `coordinator`'s head of `(scope, tip)`'s
/// ended log: the terminal entry's position and running hash, the seal it
/// committed and that seal's continuation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAckStatement {
    pub format: u32,
    /// The coordination service whose log ended: a record counts only for
    /// the coordinator the database's settings name.
    pub coordinator: CoordinatorIdentity,
    pub scope: Scope,
    pub tip: TipId,
    pub position: u64,
    pub running: Commitment,
    pub seal: Commitment,
    pub continuation: Option<TipId>,
    /// The terminal entry confirms a seal tip graphs held before it, as
    /// `rocky_core::LogTerminal::confirms` says.
    pub confirms: bool,
}

/// A terminal acknowledgment signed by a trusted node under its chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAck {
    pub statement: TerminalAckStatement,
    pub signer: PublicKey,
    pub credentials: CredentialChain,
    /// Its place in the signing node's chain of acknowledgments.
    pub link: ChainLink,
    signature: Signature,
}
const TERMINAL_ACK: &str = "rocky.terminal-ack.v4";
impl TerminalAck {
    /// `node` acknowledges at `link` of its own chain.
    pub fn sign(
        statement: TerminalAckStatement,
        node: &PrivateKey,
        credentials: CredentialChain,
        link: ChainLink,
    ) -> Result<Self, Error> {
        if statement.format != TERMINAL_ACK_FORMAT {
            return Err(denied("unsupported terminal acknowledgment format"));
        }
        if link.lane != node.public_key() || link.stage.is_some() {
            return Err(denied("an acknowledgment names its own node's chain"));
        }
        link.validate()?;
        let signature = node.sign(&signing_bytes(TERMINAL_ACK, &(&statement, &link))?);
        let ack = Self {
            statement,
            signer: node.public_key(),
            credentials,
            link,
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
        if self.link.lane != self.signer || self.link.stage.is_some() {
            return Err(denied("an acknowledgment names its own node's chain"));
        }
        self.link.validate()?;
        let current_unless_cut = current.filter(|(revocations, _)| {
            self.identity()
                .is_none_or(|identity| revocations.cut(&identity, &self.signer).is_none())
        });
        let identity = self.credentials.trusted_node(
            publisher,
            self.statement.scope,
            self.signer,
            current_unless_cut,
        )?;
        if !self.signer.verify(
            &signing_bytes(TERMINAL_ACK, &(&self.statement, &self.link))?,
            &self.signature,
        ) {
            return Err(denied("invalid terminal acknowledgment signature"));
        }
        Ok(identity)
    }
    /// The trusted identity the node's chain starts at, as named, unverified.
    pub fn identity(&self) -> Option<PublicKey> {
        self.credentials
            .0
            .first()
            .map(|link| link.capability.subject)
    }
    /// Its hash: what the next acknowledgment of its node names.
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.terminal-ack.id.v1",
            &self.to_bytes()?,
        ))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_TERMINAL_ACK_BYTES {
            return Err(denied("terminal acknowledgment quota"));
        }
        Ok(bytes)
    }
    /// Decodes an acknowledgment, refusing one of another format by the
    /// format it names, whether or not its shape is this release's.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let ack: Self = decode_bounded(bytes, MAX_TERMINAL_ACK_BYTES).map_err(|error| {
            match p2panda_core::cbor::decode_cbor::<NamedAckFormat, _>(bytes) {
                Ok(named) if named.statement.format != TERMINAL_ACK_FORMAT => {
                    unsupported_ack(named.statement.format)
                }
                _ => error,
            }
        })?;
        if ack.statement.format != TERMINAL_ACK_FORMAT {
            return Err(unsupported_ack(ack.statement.format));
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
            return Err(unsupported_finality(self.format));
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
                || statement.coordinator != first.coordinator
                || statement.seal != self.seal
                || statement.tip != first.tip
                || statement.position != first.position
                || statement.running != first.running
                || statement.continuation != first.continuation
                || statement.confirms != first.confirms
            {
                return Err(denied("seal finality acknowledgments disagree"));
            }
        }
        Ok(())
    }
    /// The coordination service every acknowledgment names.
    pub fn coordinator(&self) -> Option<CoordinatorIdentity> {
        self.acknowledgments
            .first()
            .map(|ack| ack.statement.coordinator)
    }
    /// Whether the coordinator's log decided the seal: its terminal entry
    /// ended the log with a seal no graph held yet, instead of confirming a
    /// choice graphs had made.
    pub fn decided(&self) -> bool {
        self.acknowledgments
            .first()
            .is_some_and(|ack| !ack.statement.confirms)
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
    pub fn id(&self) -> Result<Commitment, Error> {
        #[cfg(test)]
        crate::tips::tests::count_id();
        Ok(rocky_core::commitment(
            "rocky.seal-finality.id.v2",
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
    /// Decodes a record, refusing one of another format by the format it
    /// names, whether or not its shape is this release's.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let record: Self = decode_bounded(bytes, MAX_FINALITY_BYTES).map_err(|error| {
            match p2panda_core::cbor::decode_cbor::<NamedFormat, _>(bytes) {
                Ok(named) if named.format != SEAL_FINALITY_FORMAT => {
                    unsupported_finality(named.format)
                }
                _ => error,
            }
        })?;
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
