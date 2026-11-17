use crate::{
    CredentialChain, Endorsement, Error, IdentityProvider, PublicKey, RevocationSet, TipGraph,
    decode_bounded, denied, encode,
};
use rocky_core::{Commitment, ResourceScope, Scope};
use serde::{Deserialize, Serialize};

pub const EXISTENCE_BASE_FORMAT: u32 = 2;
pub const MAX_EXISTENCE_BASE_LOGS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistenceLog {
    pub signer: PublicKey,
    pub sequence: u64,
    pub head: Commitment,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistenceCut {
    pub scope: ResourceScope,
    pub logs: Vec<ExistenceLog>,
    pub state: Commitment,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistenceIdentity {
    pub author: rocky_core::WriterId,
    pub sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistenceBase {
    pub scope: ResourceScope,
    pub rows: u64,
    pub digest: Commitment,
    pub identities: Vec<ExistenceIdentity>,
}
impl ExistenceBase {
    pub fn state(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.existence-base-state.v1",
            &encode(self)?,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistenceBaseStatement {
    pub format: u32,
    pub scope: Scope,
    pub seal: Commitment,
    pub cut: ExistenceCut,
    pub base: ExistenceBase,
}
impl ExistenceBaseStatement {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != EXISTENCE_BASE_FORMAT {
            return Err(denied("unsupported existence base format"));
        }
        let logs = &self.cut.logs;
        let identities = &self.base.identities;
        if self.cut.scope.domain != self.scope
            || self.base.scope != self.cut.scope
            || self.cut.state != self.base.state()?
            || logs.len() > MAX_EXISTENCE_BASE_LOGS
            || logs.windows(2).any(|pair| pair[0].signer >= pair[1].signer)
            || logs
                .iter()
                .any(|log| log.sequence == 0 || log.sequence > i64::MAX as u64)
            || identities.len() > MAX_EXISTENCE_BASE_LOGS
            || identities
                .windows(2)
                .any(|pair| pair[0].author >= pair[1].author)
            || identities
                .iter()
                .any(|mark| rocky_core::RowIdentity::new(mark.author, mark.sequence).is_err())
        {
            return Err(denied("invalid existence base"));
        }
        Ok(())
    }
}

crate::tips::signed_statement!(
    SignedExistenceBase,
    ExistenceBaseStatement,
    EXISTENCE_BASE_FORMAT,
    "rocky.existence-base.v1",
    "rocky.existence-base.id.v1"
);

impl SignedExistenceBase {
    fn check_graph(&self, graph: &TipGraph, _trusted: bool) -> Result<(), Error> {
        graph.seal_by_id(self.statement.seal)?;
        Ok(())
    }
}

impl crate::tips::OpensStage for ExistenceBaseStatement {}
