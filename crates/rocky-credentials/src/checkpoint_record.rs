//! Checkpoint records: graph statements listing a checkpoint of one scope at
//! a seal, so every device learns where it may rebootstrap. The body moves
//! only on request. See `docs/runtime/lazy-compaction.md#checkpoints-at-seals`.
use crate::{
    CredentialChain, Endorsement, Error, IdentityProvider, PublicKey, RevocationSet, TipGraph,
    decode_bounded, denied, encode,
};
use rocky_core::{Commitment, ResourceScope, Scope};
use serde::{Deserialize, Serialize};

pub const CHECKPOINT_RECORD_FORMAT: u32 = 1;

/// A checkpoint of `table` holding the state of `seal`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRecordStatement {
    pub format: u32,
    /// The domain.
    pub scope: Scope,
    pub seal: Commitment,
    /// The encryption scope the checkpoint holds.
    pub table: ResourceScope,
    /// The checkpoint's digest.
    pub checkpoint: Commitment,
    /// The node that signed the checkpoint.
    pub signer: PublicKey,
    /// Its size: the stored rows it holds.
    pub rows: u64,
}
impl CheckpointRecordStatement {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != CHECKPOINT_RECORD_FORMAT {
            return Err(denied("unsupported checkpoint record format"));
        }
        if self.table.domain != self.scope {
            return Err(denied("checkpoint record names another domain"));
        }
        Ok(())
    }
}

crate::tips::signed_statement!(
    SignedCheckpointRecord,
    CheckpointRecordStatement,
    CHECKPOINT_RECORD_FORMAT,
    "rocky.checkpoint-record.v1",
    "rocky.checkpoint-record.id.v1"
);

impl SignedCheckpointRecord {
    /// The owner, a trusted identity or an issuer lists a checkpoint of a
    /// seal the graph holds.
    fn check_graph(&self, graph: &TipGraph, _trusted: bool) -> Result<(), Error> {
        graph.seal_by_id(self.statement.seal)?;
        Ok(())
    }
}
