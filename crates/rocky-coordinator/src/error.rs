use serde::{Deserialize, Serialize};

/// Why a coordinator call failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoordinationError {
    /// No voter accepted the request, so it took no number and changed nothing; it may be retried.
    Unavailable,
    /// The request may have reached the coordinator but no answer came back; only a retry of the
    /// same `Claim` or request is safe, and it answers what the first one did.
    OutcomeUnknown,
    /// The caller's clock is more than 30 seconds from the coordinator's.
    ClockSkew {
        /// The coordinator's Unix time in seconds.
        coordinator_time: u64,
    },
    /// A voter lost its durable state and refuses to serve.
    CoordinationStateLost,
    /// The database is not registered with the coordinator.
    UnknownDomain,
    /// The coordinator refused the call for the stated reason.
    Denied(Refusal),
    /// The trust bundle, a name or a lifetime is invalid, or the transport is not one the bundle
    /// lists.
    Configuration,
    /// The same request or claim id came back with different contents.
    RequestConflict,
    /// A message failed to decode or held an unexpected answer.
    Corrupt,
    /// The claim's expiry has passed, or the request is older than the coordinator still
    /// remembers, so a retry cannot be matched.
    Expired,
    /// The coordinator or this tenant is at a limit; retry later.
    Backpressure,
    /// The counter holds `MAX_CLAIMS` claims still in force, so a new key takes no number until
    /// one expires.
    Full,
    /// The trust bundle has a format this version does not read.
    UnsupportedFormat,
}
impl CoordinationError {
    /// A stable snake-case label for logs and metrics.
    pub fn category(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::ClockSkew { .. } => "clock_skew",
            Self::CoordinationStateLost => "state_lost",
            Self::UnknownDomain => "unknown_domain",
            Self::Denied(_) => "denied",
            Self::Configuration => "configuration",
            Self::RequestConflict => "request_conflict",
            Self::Corrupt => "corrupt",
            Self::Expired => "expired",
            Self::Backpressure => "backpressure",
            Self::Full => "full",
            Self::UnsupportedFormat => "unsupported_format",
        }
    }
    /// Whether sending the same call again may succeed; after `OutcomeUnknown` only the same
    /// `Claim` or request is safe to send.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::OutcomeUnknown | Self::Backpressure
        )
    }
}
/// Why the coordinator refused a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// A signature or the genesis it names did not verify.
    Signature,
    /// A message exceeded its size limit.
    Oversized,
    /// A message of the wrong kind for this connection.
    UnexpectedMessage,
    /// A voter id the genesis does not list.
    UnknownVoter,
    /// The device holds no current membership of the database.
    NotMember,
    /// The person may read the database but not claim numbers.
    ReadOnly,
    /// The device or person was revoked.
    Revoked,
    /// A pushed revocation set did not verify.
    InvalidRevocations,
    /// The database's registration was closed.
    ClosedDomain,
    /// A control certificate is malformed, expired or outside its allowed lifetime.
    InvalidControl,
    /// The signer may not control this database.
    NotDomainAuthority,
    /// The signer may not change the voters.
    NotMembershipAuthority,
    /// The registration grant is not the owner's or is outside its validity.
    InvalidGrant,
    /// The tenant lacks a valid admission from the coordinator's provider.
    NotAdmitted,
    /// The database is already registered with a different certificate.
    AlreadyRegistered,
    /// The tenant has no registration with the coordinator.
    UnknownTenant,
    /// The coordinator accepts no public registrations.
    NoPublicService,
    /// The registration names no database publisher.
    NoPublisher,
}
impl std::fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CoordinationError {}
