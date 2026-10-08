use crate::{Application, Envelope, PrincipalId, Scope, SqlValue};

/// Installed SQL: static in a compiled adapter, owned when generated.
pub type Sql = std::borrow::Cow<'static, str>;

/// Only the installed application supplies SQL, never a UI or mutation caller.
#[derive(Clone, Debug)]
pub struct ProjectionStatement {
    pub sql: Sql,
    pub values: Vec<SqlValue>,
}

/// The SQL an installation creates and reads, owned by the application value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledSchema {
    /// `CREATE TABLE`, `CREATE INDEX` and application `CREATE VIEW` statements, in installation order.
    pub statements: Vec<Sql>,
    /// Optional additive, non-unique indexes on existing tables. Entries are
    /// immutable and may only be appended; they do not change reducer identity.
    pub additive_indexes: Vec<Sql>,
    pub projection_query: Sql,
    /// The tables of `statements` that hold the mergeable projection and
    /// nothing else: every row `project` writes, deletion evidence included,
    /// and no row a command result writes. A checkpoint commits to their
    /// rows, a checkpoint import replaces them, and replay must reproduce them.
    pub checkpoint_tables: Vec<Sql>,
}
impl InstalledSchema {
    pub fn statements(&self) -> Vec<&str> {
        self.statements.iter().map(AsRef::as_ref).collect()
    }
}
/// The replacement boundary between compiled and generated schemas: the
/// runtime installs whatever this returns and nothing else.
pub trait ProjectionSchema {
    fn installed_schema(&self) -> InstalledSchema;
}
/// Static SQL of a compiled adapter.
pub trait CompiledSchema {
    fn schema(&self) -> &'static [&'static str];
    /// See `InstalledSchema::additive_indexes`.
    fn additive_indexes(&self) -> &'static [&'static str] {
        &[]
    }
    fn projection_query(&self) -> &'static str;
    /// See `InstalledSchema::checkpoint_tables`.
    fn checkpoint_tables(&self) -> &'static [&'static str];
}
impl<T: CompiledSchema> ProjectionSchema for T {
    fn installed_schema(&self) -> InstalledSchema {
        let owned = |sql: &'static [&'static str]| sql.iter().map(|sql| Sql::Borrowed(sql));
        InstalledSchema {
            statements: owned(self.schema()).collect(),
            additive_indexes: owned(self.additive_indexes()).collect(),
            projection_query: Sql::Borrowed(self.projection_query()),
            checkpoint_tables: owned(self.checkpoint_tables()).collect(),
        }
    }
}

/// Persisted compatibility contract. Reducer/codec changes require a new definition;
/// projection and handler generations are independently bound by the runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplicationDescriptor {
    pub definition: &'static str,
    pub operations: crate::Versions,
    pub projection: u32,
}
/// Projection adapter. Runtime owns execution and transaction completion.
pub trait StoredApplication: Application + ProjectionSchema + Sized + Send + 'static {
    /// Stable installed reducer/projection definition. Change when semantics change.
    fn descriptor(&self) -> ApplicationDescriptor;
    fn manifest(&self) -> Option<crate::ApplicationManifest> {
        None
    }
    fn definition(&self) -> &'static str {
        self.descriptor().definition
    }
    /// Optional immutable command handlers, registered by method identity.
    fn command(&self, _method: &str) -> Option<&dyn CommandHandler<Self>> {
        None
    }
    fn commands(&self) -> &'static [crate::CommandMethod] {
        &[]
    }
    type Row: Clone + Send + PartialEq + 'static;
    /// Translates rows captured from SQL transactions into operations. None:
    /// the application accepts precise mutations only.
    fn sql_writes(&self) -> Option<&dyn crate::SqlWrites<Self>> {
        None
    }
    /// The descriptor the installed tables must match. The runtime verifies
    /// the physical tables against it, persists it and exposes it.
    fn logical_schema(&self) -> Option<&crate::LogicalSchema> {
        None
    }
    /// Every stored row of `state`, in `projection_query` shape and order.
    fn projection_rows(&self, state: &Self::State) -> Vec<Vec<SqlValue>>;
    /// `SELECT`s for the stored rows this operation's transition reads or
    /// writes, in `projection_query` row shape. The runtime never holds a
    /// complete state: it loads these rows inside the transaction.
    fn touched(&self, operation: &Envelope) -> Vec<ProjectionStatement>;
    /// The state holding exactly `rows`; the inverse of `projection_rows`.
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<Self::State, Self::Error>;
    /// Called with the window immediately after this operation, in transition
    /// order. Must write every stored row the transition changed.
    fn project(&self, state: &Self::State, operation: &Envelope) -> Vec<ProjectionStatement>;
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Self::Error>;
    /// Select complete visible rows under current policy. Never return partially
    /// redacted rows whose fields imply permissions on another row.
    fn select_rows(
        &self,
        rows: Vec<Self::Row>,
        context: crate::AdmissionContext,
        policy: &Self::Policy,
    ) -> Result<Vec<Self::Row>, Self::Error> {
        self.admit_read(context.scope, context.caller, policy)?;
        Ok(rows)
    }
    /// The tables bound reads may name, each a `SELECT` over stored rows that
    /// exposes only whole rows and columns this caller may read under `policy`.
    fn read_views(
        &self,
        context: crate::AdmissionContext,
        policy: &Self::Policy,
    ) -> Result<Vec<ReadView>, Self::Error> {
        self.admit_read(context.scope, context.caller, policy)?;
        Ok(Vec::new())
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Self::Policy,
    ) -> Result<(), Self::Error>;
}

/// Optional sealed-input command extension. Merge-only apps need no handler.
pub trait CommandHandler<A: Application> {
    /// `state` is complete for the sealed cut, unlike a reducer's window.
    fn seal(&self, state: &A::State) -> Result<Vec<u8>, A::Error>;
    fn admit(&self, request: &crate::StrongRequest, policy: &A::Policy) -> Result<(), A::Error>;
    /// Select the complete immutable result under current read policy.
    fn select_result(
        &self,
        bytes: &[u8],
        context: crate::AdmissionContext,
        policy: &A::Policy,
    ) -> Result<bool, A::Error>;
    fn validate(&self, request: &crate::StrongRequest) -> Result<(), A::Error>;
    /// Previous confirmed result bytes are separate from the sealed mergeable cut.
    fn execute_after(
        &self,
        state: &A::State,
        request: &crate::StrongRequest,
        _previous: Option<&[u8]>,
    ) -> Result<Vec<u8>, A::Error> {
        self.execute(state, request)
    }
    /// Atomic strong projection and job intents, supplied only by installed code.
    fn result_projection_query(&self) -> Option<&'static str> {
        None
    }
    fn result_projection_rows(&self, _result: &[u8]) -> Result<Vec<Vec<SqlValue>>, A::Error> {
        Ok(Vec::new())
    }
    fn project_result(&self, _result: &[u8]) -> Result<Vec<ProjectionStatement>, A::Error> {
        Ok(Vec::new())
    }
    fn execute(
        &self,
        state: &A::State,
        request: &crate::StrongRequest,
    ) -> Result<Vec<u8>, A::Error>;
}

/// An authorized read table supplied by installed application code: bound
/// reads see `name` as the result of `sql`, never the stored tables themselves.
/// Each `?` in `sql` takes the next of `values`; a policy filter belongs here.
///
/// `key` decides how reads of this view page; see "Paging" in
/// `docs/runtime/app-runtime.md`. With a key, a read ordered by plain columns
/// continues by seeking to its last row, at a cost proportional to the page.
/// Without one, every read continues by row offset, at a cost that grows with
/// the rows already passed.
#[derive(Clone, Debug, Default)]
pub struct ReadView {
    pub name: Sql,
    pub sql: Sql,
    pub values: Vec<SqlValue>,
    /// Columns of the view that together identify a row and are never NULL.
    /// Empty: offset cursors only. A key that is not unique makes paged reads
    /// skip rows that tie on it.
    pub key: Vec<Sql>,
    /// Further columns that are never NULL. A descending order gets a keyset
    /// cursor only over `key` and these columns; a wrong entry makes
    /// descending pages skip the rows that hold NULL.
    pub not_null: Vec<Sql>,
}
/// Static column names as owned view columns.
pub fn columns(names: &[&'static str]) -> Vec<Sql> {
    names.iter().map(|name| Sql::Borrowed(name)).collect()
}

/// Fingerprint of compiled semantics and base schema; additive indexes are separate.
pub fn installed_definition<A: StoredApplication>(app: &A) -> crate::Commitment {
    let mut definition = app.definition().as_bytes().to_vec();
    if let Some(manifest) = app.manifest() {
        definition.extend(manifest.hash());
    }
    let schema = app.installed_schema();
    for sql in schema.statements.iter().chain([&schema.projection_query]) {
        definition.extend((sql.len() as u64).to_le_bytes());
        definition.extend(sql.as_bytes());
    }
    for handler in app.commands() {
        definition.extend((handler.name.len() as u64).to_le_bytes());
        definition.extend(handler.name.as_bytes());
        for version in [handler.version, handler.schema, handler.handler] {
            definition.extend(version.to_le_bytes());
        }
    }
    crate::commitment("rocky.installed-definition.v2", &definition)
}
