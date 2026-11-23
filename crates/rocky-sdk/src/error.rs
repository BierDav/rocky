use std::sync::Arc;

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
    Schema(Box<crate::SchemaError>),
    Invitation(crate::InvitationRefusal),
    Service(crate::ServiceError),
    StoreOnly(crate::StoreOnlyRefusal),
    AwaitingLink(Arc<crate::AwaitingLink>),
    RolloutConfirmation(Box<crate::RolloutConfirmationRequired>),
    Sql(Box<crate::SqlFailure>),
    Decode(crate::DecodeError),
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
            Self::Schema(_) => "invalid_schema",
            Self::Invitation(refusal) => refusal.code(),
            Self::Service(service) => service.code(),
            Self::StoreOnly(_) => "store_only",
            Self::AwaitingLink(_) => "awaiting_link",
            Self::RolloutConfirmation(_) => "rollout_confirmation_required",
            Self::Sql(failure) => match failure.reason {
                crate::SqlReason::Unavailable {
                    reason: crate::Unavailability::Incomplete { .. },
                    ..
                } => "missing_dependencies",
                crate::SqlReason::Unavailable { .. } | crate::SqlReason::Statement => "sql",
            },
            Self::Decode(_) => "decode",
            Self::Other(code) => code,
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
    pub(crate) fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            retryable: false,
            detail: detail.into(),
            blockers: Vec::new(),
        }
    }

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

impl From<crate::Failure> for Error {
    fn from(error: crate::Failure) -> Self {
        match error.downcast::<crate::AwaitingLink>() {
            Ok(pending) => Self {
                detail: pending.to_string(),
                kind: ErrorKind::AwaitingLink(Arc::new(*pending)),
                retryable: false,
                blockers: Vec::new(),
            },
            Err(error) => crate::node::typed_error(&error),
        }
    }
}

impl From<crate::ServiceError> for Error {
    fn from(error: crate::ServiceError) -> Self {
        Self {
            retryable: error.retryable(),
            detail: error.to_string(),
            blockers: Vec::new(),
            kind: ErrorKind::Service(error),
        }
    }
}

impl From<crate::SchemaError> for Error {
    fn from(error: crate::SchemaError) -> Self {
        Self::new(
            ErrorKind::Schema(Box::new(error.clone())),
            error.to_string(),
        )
    }
}

impl From<crate::DecodeError> for Error {
    fn from(error: crate::DecodeError) -> Self {
        Self::new(ErrorKind::Decode(error.clone()), error.to_string())
    }
}

impl From<rocky_secure::Error> for Error {
    fn from(error: rocky_secure::Error) -> Self {
        crate::Failure::from(error).into()
    }
}

impl From<rocky_credentials::Error> for Error {
    fn from(error: rocky_credentials::Error) -> Self {
        crate::Failure::from(error).into()
    }
}

impl From<crate::SqlFailure> for Error {
    fn from(failure: crate::SqlFailure) -> Self {
        let blockers = match &failure.reason {
            crate::SqlReason::Unavailable {
                reason: crate::Unavailability::Incomplete { blockers },
                ..
            } => blockers.clone(),
            _ => Vec::new(),
        };
        Self {
            retryable: failure.retryable,
            detail: failure.to_string(),
            blockers,
            kind: ErrorKind::Sql(Box::new(failure)),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Service(service) => Some(service),
            ErrorKind::StoreOnly(refusal) => Some(refusal),
            ErrorKind::AwaitingLink(pending) => Some(pending.as_ref()),
            ErrorKind::RolloutConfirmation(required) => Some(required.as_ref()),
            ErrorKind::Sql(failure) => Some(failure.as_ref()),
            ErrorKind::Invitation(refusal) => Some(refusal),
            ErrorKind::Schema(error) => Some(error.as_ref()),
            ErrorKind::Decode(error) => Some(error),
            _ => None,
        }
    }
}
