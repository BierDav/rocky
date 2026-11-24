use rocky_core::Blocker;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    Denied {
        table: Option<String>,
        need: Right,
        reason: DenyReason,
    },
    Removed,
    Blocked {
        table: Option<String>,
        blockers: Vec<Blocker>,
    },
    AuthorityChanged,
    Conflict,
    Busy,
    TooLarge {
        limit: Limit,
        size: usize,
        max: usize,
    },
    Quota,
    Sql {
        code: i32,
        message: String,
        unsupported: Option<Unsupported>,
    },
    Decode(crate::DecodeError),
    Schema(Box<crate::SchemaError>),
    MigrationPending,
    MigrationFailed {
        version: u32,
        step: usize,
    },
    UpgradeRequired {
        current: u32,
        declared: u32,
    },
    StoreSuperseded,
    StoreOnly(crate::StoreOnlyRefusal),
    UnsupportedGeneration,
    RegistryFull,
    RevocationSetFull,
    Rejected,
    Invite(crate::InviteError),
    Join(crate::JoinError),
    Link(crate::LinkError),
    Access(crate::AccessError),
    Service(crate::ServiceError),
    RolloutConfirmation(Box<crate::RolloutConfirmationRequired>),
    RoleInUse(crate::RoleInUse),
    Remote {
        retryable: bool,
    },
    Closed,
    Storage,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Right {
    Read,
    Write,
    Append,
    Update(String),
    Allocate,
    Administer,
    Sign,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DenyReason {
    NotGranted,
    PartitionNotGranted { group: String },
    PartitionNotCreated { group: String },
    StoreOnly,
    AdminRequired,
    OwnerRequired,
    IdentityRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Limit {
    Intent,
    Frame,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Unsupported {
    NonDeterministic(String),
    With,
    Statement,
}

impl ErrorKind {
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::Denied { reason, .. } => reason.code(),
            Self::Removed => "removed",
            Self::Blocked { .. } => "blocked",
            Self::AuthorityChanged => "authority_changed",
            Self::Conflict => "conflict",
            Self::Busy => "busy",
            Self::TooLarge { .. } => "too_large",
            Self::Quota => "quota",
            Self::Sql { .. } => "sql",
            Self::Decode(_) => "decode",
            Self::Schema(_) => "invalid_schema",
            Self::MigrationPending => "migration_pending",
            Self::MigrationFailed { .. } => "migration_failed",
            Self::UpgradeRequired { .. } => "upgrade_required",
            Self::StoreSuperseded => "store_superseded",
            Self::StoreOnly(_) => "store_only",
            Self::UnsupportedGeneration => "unsupported_generation",
            Self::RegistryFull => "registry_full",
            Self::RevocationSetFull => "revocation_set_full",
            Self::Rejected => "rejected",
            Self::Invite(error) => error.code(),
            Self::Join(error) => error.code(),
            Self::Link(error) => error.code(),
            Self::Access(error) => error.code(),
            Self::Service(service) => service.code(),
            Self::RolloutConfirmation(_) => "rollout_confirmation_required",
            Self::RoleInUse(_) => "role_in_use",
            Self::Remote { .. } => "remote",
            Self::Closed => "closed",
            Self::Storage => "storage",
            Self::Internal => "internal",
        }
    }
}

impl DenyReason {
    fn code(&self) -> &'static str {
        match self {
            Self::NotGranted => "not_granted",
            Self::PartitionNotGranted { .. } => "partition_not_granted",
            Self::PartitionNotCreated { .. } => "partition_not_created",
            Self::StoreOnly => "store_only",
            Self::AdminRequired => "admin_required",
            Self::OwnerRequired => "owner_required",
            Self::IdentityRequired => "identity_required",
        }
    }
}

impl std::fmt::Display for Right {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
            Self::Append => f.write_str("append to"),
            Self::Update(column) => write!(f, "update column {column} of"),
            Self::Allocate => f.write_str("allocate a row id in"),
            Self::Administer => f.write_str("administer"),
            Self::Sign => f.write_str("sign for the person in"),
        }
    }
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotGranted => f.write_str("this device's access does not grant it"),
            Self::PartitionNotGranted { group } => write!(
                f,
                "no partition of group `{group}` with these values is granted to this person; an admin grants one with grant_partition"
            ),
            Self::PartitionNotCreated { group } => write!(
                f,
                "no partition of group `{group}` with these values exists; create it with create_partition"
            ),
            Self::StoreOnly => f.write_str("a store-only device holds no readable data"),
            Self::AdminRequired => f.write_str("only an admin does this"),
            Self::OwnerRequired => f.write_str("only the owner's root key does this"),
            Self::IdentityRequired => f.write_str(
                "it needs the person's identity; supply it with Configuration::identity",
            ),
        }
    }
}

impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Intent => "a replayed write's intent",
            Self::Frame => "an encrypted frame",
        })
    }
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    pub(crate) fn denied(table: Option<String>, need: Right, reason: DenyReason) -> Self {
        let detail = match &table {
            Some(table) => format!("cannot {need} table {table}: {reason}"),
            None => format!("cannot {need} this database: {reason}"),
        };
        Self::new(
            ErrorKind::Denied {
                table,
                need,
                reason,
            },
            detail,
        )
    }

    pub(crate) fn refused(need: Right, reason: DenyReason) -> Self {
        Self::denied(None, need, reason)
    }

    pub(crate) fn removed() -> Self {
        Self::new(
            ErrorKind::Removed,
            "the person or device was removed from this database",
        )
    }

    pub(crate) fn sql(code: i32, message: impl Into<String>) -> Self {
        let message = message.into();
        Self::new(
            ErrorKind::Sql {
                code,
                message: message.clone(),
                unsupported: None,
            },
            message,
        )
    }

    pub(crate) fn upgrade_required(current: u32, declared: u32) -> Self {
        Self::new(
            ErrorKind::UpgradeRequired { current, declared },
            format!(
                "the application declares schema version {declared}, but this database is at version {current}, which removed a table or field it uses; upgrade the application"
            ),
        )
    }

    pub(crate) fn closed(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Closed, detail)
    }

    pub(crate) fn internal(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, detail)
    }

    pub(crate) fn from_failure(error: &crate::Failure) -> Self {
        crate::node::typed_error(error)
    }

    pub(crate) fn failed(error: crate::Failure) -> Self {
        crate::node::typed_error(&error)
    }

    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn code(&self) -> &str {
        self.kind.code()
    }
    #[cfg(not(any(test, feature = "test-support")))]
    pub(crate) fn code(&self) -> &str {
        self.kind.code()
    }

    pub fn retryable(&self) -> bool {
        match &self.kind {
            ErrorKind::Blocked { .. }
            | ErrorKind::AuthorityChanged
            | ErrorKind::Conflict
            | ErrorKind::Busy => true,
            ErrorKind::Join(error) => error.retryable(),
            ErrorKind::Link(error) => error.retryable(),
            ErrorKind::Service(error) => error.retryable(),
            ErrorKind::Remote { retryable } => *retryable,
            _ => false,
        }
    }

    pub fn blockers(&self) -> &[Blocker] {
        match &self.kind {
            ErrorKind::Blocked { blockers, .. } => blockers,
            _ => &[],
        }
    }
}

impl From<crate::InviteError> for Error {
    fn from(error: crate::InviteError) -> Self {
        Self::new(ErrorKind::Invite(error), error.to_string())
    }
}

impl From<crate::JoinError> for Error {
    fn from(error: crate::JoinError) -> Self {
        Self::new(ErrorKind::Join(error), error.to_string())
    }
}

impl From<crate::AccessError> for Error {
    fn from(error: crate::AccessError) -> Self {
        let detail = error.to_string();
        Self::new(ErrorKind::Access(error), detail)
    }
}

impl From<crate::LinkError> for Error {
    fn from(error: crate::LinkError) -> Self {
        Self::new(ErrorKind::Link(error), error.to_string())
    }
}

impl From<crate::ServiceError> for Error {
    fn from(error: crate::ServiceError) -> Self {
        let detail = error.to_string();
        Self::new(ErrorKind::Service(error), detail)
    }
}

impl From<crate::SchemaError> for Error {
    fn from(error: crate::SchemaError) -> Self {
        let detail = error.to_string();
        Self::new(ErrorKind::Schema(Box::new(error)), detail)
    }
}

impl From<crate::DecodeError> for Error {
    fn from(error: crate::DecodeError) -> Self {
        let detail = error.to_string();
        Self::new(ErrorKind::Decode(error), detail)
    }
}

impl From<crate::StoreOnlyRefusal> for Error {
    fn from(refusal: crate::StoreOnlyRefusal) -> Self {
        Self::new(ErrorKind::StoreOnly(refusal), refusal.to_string())
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
            ErrorKind::RolloutConfirmation(required) => Some(required.as_ref()),
            ErrorKind::Invite(error) => Some(error),
            ErrorKind::Join(error) => Some(error),
            ErrorKind::Link(error) => Some(error),
            ErrorKind::Access(error) => Some(error),
            ErrorKind::Schema(error) => Some(error.as_ref()),
            ErrorKind::Decode(error) => Some(error),
            _ => None,
        }
    }
}
