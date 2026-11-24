use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoordinationError {
    Unavailable,
    ClockSkew { coordinator_time: u64 },
    CoordinationStateLost,
    UnknownDomain,
    Denied(Refusal),
    Configuration,
    StaleEpoch,
    StaleHead,
    InvalidCommitment,
    RequestConflict,
    Corrupt,
    Retired,
    Expired,
    Backpressure,
    UnsupportedFormat,
    Sealed,
}
impl CoordinationError {
    pub fn category(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::ClockSkew { .. } => "clock_skew",
            Self::CoordinationStateLost => "state_lost",
            Self::UnknownDomain => "unknown_domain",
            Self::Denied(_) => "denied",
            Self::Configuration => "configuration",
            Self::StaleEpoch => "stale_epoch",
            Self::StaleHead => "stale_head",
            Self::InvalidCommitment => "invalid_commitment",
            Self::RequestConflict => "request_conflict",
            Self::Corrupt => "corrupt",
            Self::Retired => "retired",
            Self::Expired => "expired",
            Self::Backpressure => "backpressure",
            Self::UnsupportedFormat => "unsupported_format",
            Self::Sealed => "sealed",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    Signature,
    Oversized,
    UnexpectedMessage,
    UnknownVoter,
    NotMember,
    ReadOnly,
    Revoked,
    InvalidRevocations,
    ClosedDomain,
    InvalidControl,
    NotDomainAuthority,
    NotMembershipAuthority,
    InvalidGrant,
    NotAdmitted,
    AlreadyRegistered,
    UnknownTenant,
    NoPublicService,
    NoPublisher,
}
impl std::fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CoordinationError {}
