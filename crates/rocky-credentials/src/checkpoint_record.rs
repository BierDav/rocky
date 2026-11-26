use crate::{
    CredentialChain, Endorsement, Error, IdentityProvider, PublicKey, RevocationSet, TipGraph,
    decode_bounded, denied, encode,
};
use rocky_core::{Commitment, ResourceScope, Scope};
use serde::{Deserialize, Serialize};

pub const CHECKPOINT_RECORD_FORMAT: u32 = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRecordStatement {
    pub format: u32,
    pub scope: Scope,
    #[serde(with = "rocky_core::byte_arrays::one")]
    pub seal: Commitment,
    pub table: ResourceScope,
    #[serde(with = "rocky_core::byte_arrays::one")]
    pub checkpoint: Commitment,
    pub signer: PublicKey,
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
    "rocky.checkpoint-record.v2",
    "rocky.checkpoint-record.id.v2"
);

impl SignedCheckpointRecord {
    fn check_graph(&self, graph: &TipGraph, _trusted: bool) -> Result<(), Error> {
        graph.seal_by_id(self.statement.seal)?;
        Ok(())
    }
}

impl crate::tips::OpensStage for CheckpointRecordStatement {}
