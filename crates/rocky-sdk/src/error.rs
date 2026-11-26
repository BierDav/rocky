use rocky_core::Blocker;

/// The one error every SDK call returns; match on `kind()`, and its message explains the cause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    detail: String,
}

/// What went wrong, as a value an application matches on.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// This device's access does not allow the call.
    Denied {
        /// The table, or `None` for a database-wide right.
        table: Option<String>,
        /// The right the call needed.
        need: Right,
        /// Why the device lacks it.
        reason: DenyReason,
    },
    /// The person was removed or the device unlinked; it no longer reads, writes or syncs, even
    /// after reopening, and has deleted the rows it held.
    Removed,
    /// The data has not arrived or cannot be decrypted yet; `blockers` say what clears it.
    Blocked {
        /// The table, when one read was refused.
        table: Option<String>,
        /// What the data waits for and what clears each; never empty.
        blockers: Blockers,
    },
    /// The device's credentials changed or expired during the call; retry.
    AuthorityChanged,
    /// A concurrent change conflicted; retry.
    Conflict,
    /// Too many calls are pending, or another write held the database for all of
    /// `TRANSACTION_TIMEOUT`; retry.
    Busy,
    /// A write held the database past `TRANSACTION_TIMEOUT` and was rolled back; keep slow work
    /// outside `Database::write`.
    TimedOut,
    /// A write exceeded a size limit.
    TooLarge {
        /// Which limit.
        limit: Limit,
        /// The size in bytes.
        size: usize,
        /// The limit in bytes; a replayed write's intent holds at most 128 KiB.
        max: usize,
    },
    /// Storage is over its quota.
    Quota,
    /// SQLite or Rocky refused a statement.
    Sql {
        /// The SQLite result code.
        code: i32,
        /// SQLite's message.
        message: String,
        /// Why Rocky refused the statement in a replayed write, or `None` when SQLite refused it.
        unsupported: Option<Unsupported>,
    },
    /// A column value did not fit the requested type.
    Decode(crate::DecodeError),
    /// `Schema::build` refused the schema, or the installed version cannot become it; the
    /// `SchemaError` names the table, column or foreign key and the rule.
    Schema(Box<crate::SchemaError>),
    /// A migration step failed and its version was withdrawn; a retry does not clear it.
    MigrationFailed {
        /// The version the migration targeted.
        version: u32,
        /// The failing step, counted from 1 in the order the migration lists its steps.
        step: usize,
    },
    /// The installed schema version and the application's declared one cannot serve this call;
    /// when `installed` is newer upgrade the application, and when `declared` is newer an admin
    /// device must publish it first, or this device sync with one that has.
    SchemaMismatch {
        /// The version the database runs.
        installed: u32,
        /// The version the application declares.
        declared: u32,
    },
    /// This copy is older than what peers have seen from it, as after restoring a backup; it reads
    /// and syncs but refuses writes, so re-invite it as a new device.
    StoreSuperseded,
    /// A store-only configuration was asked for something it cannot do.
    StoreOnly(crate::StoreOnlyRefusal),
    /// The data needs a newer version of Rocky.
    UnsupportedGeneration,
    /// The database holds as many people and devices as it can.
    RegistryFull,
    /// The database holds as many revocations as it can.
    RevocationSetFull,
    /// A peer or this device rejected a credential or message.
    Rejected,
    /// An invitation could not be issued or cancelled.
    Invite(crate::InviteError),
    /// An invitation could not be redeemed.
    Join(crate::JoinError),
    /// A device could not be linked.
    Link(crate::LinkError),
    /// An access change was refused.
    Access(crate::AccessError),
    /// The network refused or failed the call.
    Service(crate::ServiceError),
    /// The schema removes data or narrows a role; open with `Configuration::open_confirmed` and the
    /// report's digest.
    RolloutConfirmation(Box<crate::RolloutConfirmationRequired>),
    /// The schema drops a role people still hold.
    RoleInUse(crate::RoleInUse),
    /// A peer failed the call.
    Remote {
        /// Whether the peer says a retry may succeed.
        retryable: bool,
    },
    /// `open` found no database in the directory; `create` or `join` one there.
    NoDatabase,
    /// `create` or `join` found a database in the directory; `open` it instead.
    DatabaseExists,
    /// A join or link into the directory did not finish; repeating it with the same or a newer
    /// invitation, or the same contact, resumes it.
    JoinUnfinished,
    /// The database or transaction is closed.
    Closed,
    /// Local storage failed.
    Storage,
    /// An unexpected internal failure; never retryable.
    Internal,
}

/// A right a call needs.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Right {
    /// Read the table.
    Read,
    /// Write the table.
    Write,
    /// Insert rows into the table.
    Append,
    /// Update this column.
    Update(String),
    /// Allocate a row id in the table.
    Allocate,
    /// Administer people and access.
    Administer,
    /// Sign as the person, which needs `Configuration::identity`.
    Sign,
}

/// What a `Blocked` call waits for; it holds at least one blocker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blockers(Vec<Blocker>);

impl Blockers {
    pub(crate) fn new(blockers: Vec<Blocker>) -> Option<Self> {
        (!blockers.is_empty()).then_some(Self(blockers))
    }
}

impl std::ops::Deref for Blockers {
    type Target = [Blocker];

    fn deref(&self) -> &[Blocker] {
        &self.0
    }
}

/// Why a device lacks a right.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DenyReason {
    /// The person's access does not include it, whatever partitions they hold.
    NotGranted,
    /// The role grants it, but the person holds no partition with these values; an admin grants one with `grant_partition`.
    PartitionNotGranted {
        /// The partition group.
        group: String,
    },
    /// No partition with these values exists; create it with `create_partition`.
    PartitionNotCreated {
        /// The partition group.
        group: String,
    },
    /// A store-only device holds no readable data.
    StoreOnly,
    /// Only an admin may do this.
    AdminRequired,
    /// Only the owner's root key may do this.
    OwnerRequired,
    /// The call needs the person key; supply it with `Configuration::identity`.
    IdentityRequired,
}

/// A size limit a write can exceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Limit {
    /// The SQL intent a replayed write records, at most 128 KiB.
    Intent,
    /// One encrypted frame.
    Frame,
}

/// Why Rocky refused a statement in a replayed write.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Unsupported {
    /// The statement calls a clock or random function, which would differ on replay; bind the value
    /// as a parameter instead.
    NonDeterministic(String),
    /// The statement uses a `WITH` clause.
    With,
    /// The statement kind is not allowed in a transaction.
    Statement,
    /// The statement writes `column`, part of `table`'s primary key; a row's key never changes, so
    /// delete the row and insert it again.
    KeyUpdate {
        /// The table whose key the statement writes.
        table: String,
        /// The key column the statement writes.
        column: String,
    },
    /// An `INSERT … SELECT` into a table with a generated id that does not name `id`; insert the
    /// rows with `VALUES`, and Rocky fills each id.
    SelectWithoutId(String),
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
            Self::TimedOut => "timed_out",
            Self::TooLarge { .. } => "too_large",
            Self::Quota => "quota",
            Self::Sql { .. } => "sql",
            Self::Decode(_) => "decode",
            Self::Schema(_) => "invalid_schema",
            Self::MigrationFailed { .. } => "migration_failed",
            Self::SchemaMismatch { .. } => "schema_mismatch",
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
            Self::NoDatabase => "no_database",
            Self::DatabaseExists => "database_exists",
            Self::JoinUnfinished => "join_unfinished",
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
            "this device's person was removed or the device was unlinked: it no longer reads, writes or syncs and has deleted the rows it held; logout(Logout::PurgeOnClose) deletes the rest of its directory",
        )
    }

    pub(crate) fn authority_changed() -> Self {
        Self::new(
            ErrorKind::AuthorityChanged,
            "this device's credentials changed or expired during the call; retry it",
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

    pub(crate) fn schema_mismatch(installed: u32, declared: u32) -> Self {
        let remedy = if installed > declared {
            "it removed a table or field the application uses, so upgrade the application"
        } else {
            "an admin device must publish the declared version first, or this device sync with one that has"
        };
        Self::new(
            ErrorKind::SchemaMismatch {
                installed,
                declared,
            },
            format!(
                "the application declares schema version {declared}, but this database runs version {installed}: {remedy}"
            ),
        )
    }

    pub(crate) fn no_database() -> Self {
        Self::new(
            ErrorKind::NoDatabase,
            "no database in this directory: create or join one",
        )
    }

    pub(crate) fn database_exists() -> Self {
        Self::new(
            ErrorKind::DatabaseExists,
            "a database exists in this directory: open it instead",
        )
    }

    pub(crate) fn join_unfinished() -> Self {
        Self::new(
            ErrorKind::JoinUnfinished,
            "a join or link in this directory is unfinished: repeat it to resume",
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

    /// What went wrong.
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// A stable snake-case code for tests and logs.
    #[cfg(any(test, feature = "test-support"))]
    pub fn code(&self) -> &str {
        self.kind.code()
    }
    #[cfg(not(any(test, feature = "test-support")))]
    pub(crate) fn code(&self) -> &str {
        self.kind.code()
    }

    /// Whether retrying the same call may succeed; it follows from the kind.
    pub fn retryable(&self) -> bool {
        match &self.kind {
            ErrorKind::Blocked { .. }
            | ErrorKind::AuthorityChanged
            | ErrorKind::Conflict
            | ErrorKind::Busy => true,
            ErrorKind::Link(error) => error.retryable(),
            ErrorKind::Service(error) => error.retryable(),
            ErrorKind::Remote { retryable } => *retryable,
            _ => false,
        }
    }

    /// The blockers of a `Blocked` error, or none.
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
