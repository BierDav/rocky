use crate::{Commitment, OperationId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransactionToken(pub Commitment);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    pub entity: Option<OperationId>,
    pub payload: Vec<u8>,
}
