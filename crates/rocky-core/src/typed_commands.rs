//! Typed tables with typed commands: the row application a secure replica
//! installs for a schema that declares commands. Handlers decide over typed
//! rows; this turns their decision into precise row changes and scoped
//! outputs. See `docs/runtime/typed-commands.md`.
use crate::{
    ApplicationManifest, CommandCall, CommandFailure, CommandHost, CommandOutputFuture,
    CommandOutputKind, CommandRows, CommandSet, Commitment, HandlerInput, MAX_COMMAND_JOBS,
    MAX_COMMAND_WRITES, MAX_PAYLOAD_BYTES, Outcome, ResourceScope, RowApplication, RowChange,
    RowCommandApplication, RowCommandOutput, RowFields, Scope, ScopedCommandOutput, SqlValue,
    TableWrite, TypedRows, TypedTable,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// `TypedRows` and the commands declared over its tables. Without a host
/// the device holds definitions only: it invokes and reads results, and
/// cannot execute.
#[derive(Clone)]
pub struct TypedCommands {
    rows: TypedRows,
    commands: CommandSet,
    /// The reader scope of every mergeable table, by SQL name.
    scopes: BTreeMap<String, ResourceScope>,
    host: Option<Arc<dyn CommandHost>>,
}
fn invalid(reason: impl Into<String>) -> CommandFailure {
    CommandFailure::Invalid(reason.into())
}
impl TypedCommands {
    /// Every scope a command names must be a table of `rows`.
    pub fn new(
        rows: TypedRows,
        commands: CommandSet,
        scopes: BTreeMap<String, ResourceScope>,
        host: Option<Arc<dyn CommandHost>>,
    ) -> Result<Self, crate::CommandError> {
        for descriptor in commands.descriptors() {
            for name in &descriptor.resources {
                if scopes
                    .get(name)
                    .is_none_or(|scope| rows.table(&scope.table).is_none())
                {
                    return Err(crate::CommandError::Invalid(format!(
                        "command {} names {name}, which is not a table",
                        descriptor.name
                    )));
                }
            }
        }
        Ok(Self {
            rows,
            commands,
            scopes,
            host,
        })
    }
    /// The same definitions with the handlers a device executes with.
    pub fn with_host(mut self, host: Option<Arc<dyn CommandHost>>) -> Self {
        self.host = host;
        self
    }
    /// Where handlers run, if anywhere.
    pub fn host(&self) -> Option<Arc<dyn CommandHost>> {
        self.host.clone()
    }
    pub fn rows(&self) -> &TypedRows {
        &self.rows
    }
    pub fn commands(&self) -> &CommandSet {
        &self.commands
    }
    /// Whether this device can execute, not whether it is eligible to.
    pub fn hosts(&self) -> bool {
        self.host.is_some()
    }
    pub fn scope(&self, table: &str) -> Option<ResourceScope> {
        self.scopes.get(table).copied()
    }
    fn table(&self, name: &str) -> Result<(&TypedTable, ResourceScope), String> {
        let scope = self
            .scopes
            .get(name)
            .ok_or_else(|| format!("{name} is not a table"))?;
        let table = self
            .rows
            .table(&scope.table)
            .ok_or_else(|| format!("{name} is not a table"))?;
        Ok((table, *scope))
    }
    /// The typed rows of confirmed command state, in row identity order.
    pub fn typed_rows(
        &self,
        table: &str,
        rows: &BTreeMap<Commitment, RowFields>,
    ) -> Result<Vec<Vec<SqlValue>>, String> {
        let (table, _) = self.table(table)?;
        rows.values()
            .map(|fields| self.rows.values(table, fields))
            .collect()
    }
    fn resources(&self, scope: Scope, method: &str) -> Result<Vec<ResourceScope>, String> {
        let descriptor = self
            .commands
            .method(method)
            .map_err(|error| error.to_string())?;
        let scopes: Vec<ResourceScope> = descriptor
            .resources
            .iter()
            .filter_map(|name| self.scopes.get(name).copied())
            .collect();
        if scopes.iter().any(|resource| resource.domain != scope) {
            return Err("command outside its domain".into());
        }
        Ok(scopes)
    }
    /// The decision of a handler as row changes and scoped outputs, each
    /// checked against the descriptor and the state it was decided on.
    fn output(
        &self,
        call: &CommandCall<'_>,
        descriptor: &crate::CommandDescriptor,
        decision: crate::HandlerOutput,
    ) -> Result<RowCommandOutput, CommandFailure> {
        let result = descriptor.encode_outcome(&decision.outcome)?;
        if matches!(decision.outcome, Outcome::Rejected(_))
            && !(decision.writes.is_empty() && decision.jobs.is_empty())
        {
            return Err(invalid("a rejection changes no rows and emits no jobs"));
        }
        if decision.writes.len() > MAX_COMMAND_WRITES || decision.jobs.len() > MAX_COMMAND_JOBS {
            return Err(invalid("handler output quota"));
        }
        let resource = |name: &str| {
            if !descriptor.resources.iter().any(|resource| resource == name) {
                return Err(invalid(format!(
                    "{name} is outside the resources of {}",
                    descriptor.name
                )));
            }
            self.table(name).map_err(invalid)
        };
        let mut changes = Vec::new();
        let mut written = BTreeSet::new();
        for write in &decision.writes {
            let (name, values) = match write {
                TableWrite::Put { table, row } => (table, row.clone()),
                TableWrite::Delete { table, key } => {
                    let (typed, _) = resource(table)?;
                    if key.len() != typed.key.len() {
                        return Err(invalid(format!("{table} key")));
                    }
                    let mut row = vec![SqlValue::Null; typed.fields.len()];
                    for (index, value) in typed.key.iter().zip(key) {
                        row[*index] = value.clone();
                    }
                    (table, row)
                }
            };
            let (typed, scope) = resource(name)?;
            let row = self
                .rows
                .row_id(scope.domain.domain, typed, &values)
                .map_err(invalid)?;
            if !written.insert((scope, row)) {
                return Err(invalid(format!("{name} row written twice")));
            }
            let before = call.state.get(&scope).and_then(|rows| rows.get(&row));
            let after = match write {
                TableWrite::Put { .. } => Some(self.rows.fields(typed, &values).map_err(invalid)?),
                TableWrite::Delete { .. } if before.is_none() => {
                    return Err(invalid(format!("{name} row to delete is absent")));
                }
                TableWrite::Delete { .. } => None,
            };
            // Writing a row back unchanged is no transition.
            if before != after.as_ref() {
                changes.push(RowChange {
                    scope,
                    row,
                    before: before.cloned(),
                    after,
                });
            }
        }
        let mut outputs = vec![ScopedCommandOutput {
            scope: resource(&descriptor.result)?.1,
            kind: CommandOutputKind::Result,
            bytes: result,
        }];
        for (name, payload) in decision.jobs {
            if payload.len() > MAX_PAYLOAD_BYTES / 2 {
                return Err(invalid("job payload quota"));
            }
            outputs.push(ScopedCommandOutput {
                scope: resource(&name)?.1,
                kind: CommandOutputKind::Job,
                bytes: payload,
            });
        }
        Ok(RowCommandOutput { changes, outputs })
    }
}
impl RowApplication for TypedCommands {
    fn manifest(&self) -> ApplicationManifest {
        let mut manifest = self.rows.manifest();
        manifest.methods = self.commands.methods(manifest.publisher, manifest.app);
        manifest.handler = self.commands.handler();
        manifest
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        self.rows.permission_fields()
    }
    fn row_partition(&self, change: &RowChange) -> Result<Option<crate::PartitionTuple>, String> {
        self.rows.change_partition(change)
    }
    fn row_identity(&self, change: &RowChange) -> Result<Option<crate::RowIdentity>, String> {
        self.rows.identity(change)
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        self.rows.validate_changes(changes)
    }
}
impl RowCommandApplication for TypedCommands {
    fn validate_confirmed(&self, changes: &[RowChange]) -> Result<(), String> {
        self.rows.validate_confirmed(changes)
    }
    fn command_resources(&self, scope: Scope, method: &str) -> Result<Vec<ResourceScope>, String> {
        self.resources(scope, method)
    }
    fn command_invocation_scope(
        &self,
        scope: Scope,
        method: &str,
    ) -> Result<ResourceScope, String> {
        self.resources(scope, method)?;
        let descriptor = self
            .commands
            .method(method)
            .map_err(|error| error.to_string())?;
        Ok(self.table(&descriptor.invocation)?.1)
    }
    fn execute_command(
        &self,
        _method: &str,
        _arguments: &[u8],
        _state: &CommandRows,
    ) -> Result<RowCommandOutput, String> {
        Err("typed commands execute through their handler host".into())
    }
    fn supports_version(&self, method: &str, version: u32) -> bool {
        self.commands.get(method, version).is_ok()
    }
    fn validate_arguments(
        &self,
        method: &str,
        version: u32,
        arguments: &[u8],
    ) -> Result<(), String> {
        self.commands
            .get(method, version)
            .and_then(|descriptor| descriptor.decode_arguments(arguments))
            .map(drop)
            .map_err(|error| error.to_string())
    }
    fn run_command<'a>(&'a self, call: CommandCall<'a>) -> CommandOutputFuture<'a> {
        Box::pin(async move {
            let descriptor = self.commands.get(call.method, call.version)?;
            let arguments = descriptor.decode_arguments(call.arguments)?;
            let host = self.host.as_ref().ok_or_else(|| {
                invalid(format!(
                    "this device holds the definition of {} and no handler",
                    descriptor.name
                ))
            })?;
            let mut state = BTreeMap::new();
            for name in &descriptor.resources {
                let (table, scope) = self.table(name).map_err(invalid)?;
                let rows = call
                    .state
                    .get(&scope)
                    .ok_or_else(|| invalid(format!("{name} state is missing")))?;
                state.insert(
                    name.clone(),
                    (
                        table.key.clone(),
                        self.typed_rows(name, rows).map_err(invalid)?,
                    ),
                );
            }
            let decision = host
                .handle(HandlerInput {
                    method: descriptor.name.clone(),
                    version: descriptor.version,
                    handler: descriptor.handler.clone(),
                    request: call.request,
                    caller: call.caller,
                    predecessor: call.predecessor,
                    arguments,
                    reads: crate::HandlerReads::new(state).map_err(invalid)?,
                })
                .await?;
            self.output(&call, descriptor, decision)
        })
    }
}
