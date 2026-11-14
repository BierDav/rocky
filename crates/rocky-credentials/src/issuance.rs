//! Inputs for issuance and delegation; persisted capabilities retain second timestamps.
use crate::{PartitionGrant, PublicKey};
use rocky_core::{PrincipalKind, ResourceGrant};
use std::time::Duration;

/// An issuance timestamp and its validity allowances. Durations truncate to whole seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Validity {
    /// Seconds since the Unix epoch, matching capability timestamps.
    pub issued_at: u64,
    /// Expiry is this long after `issued_at`, bounded by the parent.
    pub lifetime: Duration,
    /// The start may precede `issued_at` by this much, bounded by the parent.
    pub clock_skew: Duration,
}
/// Authority the subject may pass to its own nodes or further delegates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DelegationTerms {
    /// Remaining links: one permits only the subject's own node certificates.
    pub delegations: u8,
}

/// The subject and attenuated authority of a new delegation link.
#[derive(Clone, Debug)]
pub struct Delegation {
    pub subject: PublicKey,
    pub kind: PrincipalKind,
    pub grants: Vec<ResourceGrant>,
    pub partitions: Vec<PartitionGrant>,
    pub terms: DelegationTerms,
    pub validity: Validity,
}
