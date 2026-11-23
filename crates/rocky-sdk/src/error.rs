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
    MigrationPending,
    Invitation(crate::InvitationRefusal),
    Service(String),
    Other(String),
}

impl ErrorKind {
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
            Self::MigrationPending => "migration_pending",
            Self::Invitation(refusal) => refusal.code(),
            Self::Service(code) | Self::Other(code) => code,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub retryable: bool,
    pub detail: String,
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
            blockers: error.blockers,
        }
    }
}
