use crate::Commitment;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransactionToken(pub Commitment);
