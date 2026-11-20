#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Revoked,
    PublisherRequired,
    RegistryFull,
    RevocationSetFull,
    Rejected,
    Denied,
    Blocked,
    UnsupportedGeneration,
    MissingDependencies,
    AuthorityChanged,
    Conflict,
    Quota,
    Storage,
    ClockSkew,
    CoordinatorRequired,
    CoordinatorMismatch,
    CoordinatorUnavailable,
    Sealed,
    SealPending,
    ChainPending,
    EntryPending,
    RecoveryPending,
    Behind,
    StaleRecovery,
    GenesisMinimumMissing,
    MigrationPending,
    Personal(rocky_credentials::PersonalError),
    Service(String),
    Reset(String),
    Other(String),
}

impl ErrorKind {
    pub fn from_refusal(refusal: &rocky_secure::Refusal) -> Self {
        match refusal {
            rocky_secure::Refusal::CoordinatorRequired(_) => Self::CoordinatorRequired,
            rocky_secure::Refusal::CoordinatorMismatch(_) => Self::CoordinatorMismatch,
            rocky_secure::Refusal::CoordinatorUnavailable(_) => Self::CoordinatorUnavailable,
            rocky_secure::Refusal::Sealed(_) => Self::Sealed,
            rocky_secure::Refusal::SealPending(_) => Self::SealPending,
            rocky_secure::Refusal::ChainPending(_) => Self::ChainPending,
            rocky_secure::Refusal::EntryPending(_) => Self::EntryPending,
            rocky_secure::Refusal::RecoveryPending(_) => Self::RecoveryPending,
            rocky_secure::Refusal::Behind(_) => Self::Behind,
            rocky_secure::Refusal::StaleRecovery(_) => Self::StaleRecovery,
            rocky_secure::Refusal::Denied(_) => Self::Denied,
            rocky_secure::Refusal::GenesisMinimumMissing(_) => Self::GenesisMinimumMissing,
            rocky_secure::Refusal::MigrationPending(_) => Self::MigrationPending,
        }
    }

    pub fn code(&self) -> &str {
        match self {
            Self::Revoked => "revoked",
            Self::PublisherRequired => "publisher_required",
            Self::RegistryFull => "registry_full",
            Self::RevocationSetFull => "revocation_set_full",
            Self::Rejected => "rejected",
            Self::Denied => "denied",
            Self::Blocked => "blocked",
            Self::UnsupportedGeneration => "unsupported_generation",
            Self::MissingDependencies => "missing_dependencies",
            Self::AuthorityChanged => "authority_changed",
            Self::Conflict => "conflict",
            Self::Quota => "quota",
            Self::Storage => "storage",
            Self::ClockSkew => "clock_skew",
            Self::CoordinatorRequired => rocky_secure::COORDINATOR_REQUIRED,
            Self::CoordinatorMismatch => rocky_secure::COORDINATOR_MISMATCH,
            Self::CoordinatorUnavailable => "coordinator_unavailable",
            Self::Sealed => "sealed",
            Self::SealPending => "seal_pending",
            Self::ChainPending => "chain_pending",
            Self::EntryPending => "entry_pending",
            Self::RecoveryPending => "recovery_pending",
            Self::Behind => "behind",
            Self::StaleRecovery => "stale_recovery",
            Self::GenesisMinimumMissing => "genesis_minimum_missing",
            Self::MigrationPending => "migration_pending",
            Self::Personal(error) => error.code(),
            Self::Service(code) | Self::Reset(code) | Self::Other(code) => code,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub retryable: bool,
    pub detail: String,
    pub coordinator_time: Option<u64>,
    pub blockers: Vec<rocky_core::Blocker>,
}

impl Error {
    pub fn from_failure(error: &crate::Failure) -> Self {
        crate::node::typed_error(error)
    }

    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    pub fn code(&self) -> &str {
        self.kind.code()
    }

    pub fn retryable(&self) -> bool {
        self.retryable
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn coordinator_time(&self) -> Option<u64> {
        self.coordinator_time
    }

    pub fn blockers(&self) -> &[rocky_core::Blocker] {
        &self.blockers
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for Error {}

impl From<Error> for crate::CallFailure {
    fn from(error: Error) -> Self {
        Self {
            code: error.code().to_owned(),
            retryable: error.retryable,
            detail: error.detail,
            coordinator_time: error.coordinator_time,
            blockers: error.blockers,
        }
    }
}
