use crate::{PartitionGrant, PublicKey};
use rocky_core::{PrincipalKind, ResourceGrant};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Validity {
    pub issued_at: u64,
    pub lifetime: Duration,
    pub clock_skew: Duration,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DelegationTerms {
    pub delegations: u8,
}

#[derive(Clone, Debug)]
pub struct Delegation {
    pub subject: PublicKey,
    pub kind: PrincipalKind,
    pub grants: Vec<ResourceGrant>,
    pub partitions: Vec<PartitionGrant>,
    pub terms: DelegationTerms,
    pub validity: Validity,
}
