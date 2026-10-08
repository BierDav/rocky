//! Version-1 todo semantics. See `docs/applications/todo/operation-semantics.md` for integration.
mod payload;
mod policy;
mod reducer;
mod replay;
mod storage;
mod strong;
pub use strong::{decode_snapshot, encode_snapshot};

pub use payload::{Action, ActionKind, MAX_TITLE_BYTES};
pub use policy::{RowGrant, StaticPolicy};
pub use reducer::{Register, TodoEntry, TodoState, VisibleRow};
pub use replay::{Replay, replay};
pub use rocky_core::AdmissionBoundary;
use rocky_core::{Envelope, OperationId, ProtocolError, Versions};

pub const TABLE_ID: u32 = 1;
pub const TITLE_FIELD_ID: u32 = 1;
pub const COMPLETED_FIELD_ID: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionShape {
    pub action: ActionKind,
    pub fields: &'static [u32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppDefinition {
    pub versions: Versions,
    pub table: u32,
    pub fields: &'static [u32],
    pub actions: &'static [ActionShape],
}
pub const DEFINITION: AppDefinition = AppDefinition {
    versions: Versions::V1,
    table: TABLE_ID,
    fields: &[TITLE_FIELD_ID, COMPLETED_FIELD_ID],
    actions: &[
        ActionShape {
            action: ActionKind::Create,
            fields: &[TITLE_FIELD_ID, COMPLETED_FIELD_ID],
        },
        ActionShape {
            action: ActionKind::Title,
            fields: &[TITLE_FIELD_ID],
        },
        ActionShape {
            action: ActionKind::Completed,
            fields: &[COMPLETED_FIELD_ID],
        },
        ActionShape {
            action: ActionKind::Delete,
            fields: &[],
        },
    ],
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Protocol(ProtocolError),
    InvalidTitle,
    InvalidProjection,
    InvalidBoolean,
    InvalidUtf8,
    UnknownAction(u8),
    DeniedScope,
    DeniedNode,
    DeniedCaller,
    DeniedAction(ActionKind),
    DeniedField(u32),
    DeniedRead,
    DeniedCommand,
    MissingCreate(OperationId),
    Tombstoned(OperationId),
    UnresolvedDependencies(Vec<OperationId>),
}
impl From<ProtocolError> for Error {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Default)]
pub struct TodoApp;
impl TodoApp {
    pub fn validate(&self, envelope: &Envelope) -> Result<Action, Error> {
        envelope.validate()?;
        let action = Action::decode(&envelope.payload)?;
        envelope.validate_dependencies(matches!(action, Action::CreateTodo { .. }))?;
        Ok(action)
    }
}
impl rocky_core::Application for TodoApp {
    type State = TodoState;
    fn policy_version(&self, policy: &StaticPolicy) -> u64 {
        policy.version
    }
    type Error = Error;
    type Policy = StaticPolicy;
    fn validate(&self, envelope: &Envelope) -> Result<(), Error> {
        self.validate(envelope).map(|_| ())
    }
    fn admit(&self, envelope: &Envelope, policy: &StaticPolicy) -> Result<(), Error> {
        policy.admit(envelope, &self.validate(envelope)?)
    }
    fn definite_drop(&self, state: &TodoState, envelope: &Envelope) -> bool {
        self.validate(envelope).is_ok_and(|action| {
            !matches!(action, Action::CreateTodo { .. })
                && !matches!(state.entry(envelope.entity), Some(TodoEntry::Live { .. }))
        })
    }
    fn apply_ready(
        &self,
        state: &mut TodoState,
        envelope: &Envelope,
        boundary: AdmissionBoundary,
    ) -> Result<(), Error> {
        let action = self.validate(envelope)?;
        if boundary == AdmissionBoundary::Local
            && matches!(state.entry(envelope.entity), Some(TodoEntry::Tombstone))
        {
            return Err(Error::Tombstoned(envelope.entity));
        }
        state.reduce(envelope, action)
    }
}

/// Todo composition selects this optional registered command.
pub const PUBLISH_LIST_SNAPSHOT: &str = "PublishListSnapshot";
