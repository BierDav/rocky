//! Canonical operation protocol and the application boundary for pure reducers.
mod binding;
mod capture;
mod codec;
mod definition;
pub use definition::*;
mod generation;
pub use generation::{
    Generation, GenerationTransition, SchemaChange, Unsupported, classify, classify_commands,
};
mod manifest;
pub use manifest::*;
mod history;
mod key;
mod logical;
pub mod net;
mod storage;
mod types;
mod value;

pub use binding::{BindingError, BoundTable, DatabaseBinding, MAX_BOUND_DOMAINS, MAX_BOUND_TABLES};
pub use capture::{CapturedRow, Intent, SqlWrites, TransactionToken, intent_commitment};
pub use history::{AppliedOperations, Delivery};
pub use key::{KeyColumn, MAX_KEY_BYTES, bound_key, key_components, key_values};
pub use logical::*;
pub use storage::{
    ApplicationDescriptor, CommandHandler, CompiledSchema, InstalledSchema, ProjectionSchema,
    ProjectionStatement, ReadView, Sql, StoredApplication, columns, installed_definition,
};
pub use types::*;
pub use value::{Real, SqlValue};

/// Authorization uses the installed current policy; historical replay bypasses it.
#[derive(Clone, Copy, Debug)]
pub struct AdmissionContext {
    pub scope: Scope,
    pub caller: PrincipalId,
    pub policy_version: u64,
    pub boundary: AdmissionBoundary,
}
/// Applications own validation, admission, and projection meaning. The runtime
/// checks readiness and commits history and projection together before publishing.
pub trait Application {
    type State: Default;
    type Error: std::error::Error + Send + Sync + 'static;
    type Policy;

    fn validate(&self, envelope: &Envelope) -> Result<(), Self::Error>;
    fn admit(&self, envelope: &Envelope, policy: &Self::Policy) -> Result<(), Self::Error>;
    fn policy_version(&self, _policy: &Self::Policy) -> u64 {
        1
    }
    /// Check the whole candidate transition, including paired row/field grants
    /// on both before and after state. A runtime passes windows holding only
    /// the rows the operation touches. Rejection discards all candidate changes.
    fn admit_candidate(
        &self,
        _before: &Self::State,
        _after: &Self::State,
        envelope: &Envelope,
        _context: AdmissionContext,
        policy: &Self::Policy,
    ) -> Result<(), Self::Error> {
        self.admit(envelope, policy)
    }
    /// Classifies an effect-free operation on an incarnation covered by a runtime horizon.
    /// Admission still runs; implementations must identify only absent/retired rows or insert conflicts.
    fn definite_drop(&self, _state: &Self::State, _envelope: &Envelope) -> bool {
        false
    }
    /// Caller must first admit the operation and establish readiness with
    /// `AppliedOperations::classify`. Local business checks run before mutation.
    fn apply_ready(
        &self,
        state: &mut Self::State,
        envelope: &Envelope,
        boundary: AdmissionBoundary,
    ) -> Result<(), Self::Error>;
}

mod strong;
pub use strong::*;

mod partition;
pub use partition::{
    MAX_PARTITION_BYTES, PartitionError, PartitionSelection, PartitionSelector, PartitionTuple,
    PartitionValue,
};
mod permissions;
pub use permissions::*;

mod row_identity;
pub use row_identity::{RowIdentity, RowIdentityError};

mod typed_rows;
pub use typed_rows::{TypedRows, TypedTable};

mod handler_reads;
pub use handler_reads::{HandlerReads, MAX_HANDLER_SCAN_ROWS};
mod command;
pub use command::*;
mod typed_commands;
pub use typed_commands::TypedCommands;
