//! Typed strong commands: portable definitions, their canonical descriptors,
//! the argument and outcome codec, and the handler contract. See
//! `docs/runtime/typed-commands.md`.
use crate::{
    AppId, Commitment, DefinitionItem, PrincipalId, RequestId, RowCommandOutput, SqlValue,
    commitment, definition_item_id,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
};

pub const COMMAND_FORMAT: u32 = 1;
/// Largest definition document `CommandDefinitions::parse` accepts.
pub const MAX_COMMAND_DEFINITION_BYTES: usize = 256 * 1024;
/// Descriptors per application, every supported version counted.
pub const MAX_COMMANDS: usize = 64;
pub const MAX_COMMAND_FIELDS: usize = 64;
/// Encoded arguments or one encoded outcome.
pub const MAX_COMMAND_VALUE_BYTES: usize = 48 * 1024;
/// Rows one handler call changes, and the jobs it emits besides its result.
pub const MAX_COMMAND_WRITES: usize = 32;
pub const MAX_COMMAND_JOBS: usize = 31;

const ARGUMENTS: u8 = 1;
const ACCEPTED: u8 = 2;
const REJECTED: u8 = 3;

/// What a typed command refuses. None of these is a business rejection: a
/// rejection is an `Outcome` a handler returned and the coordinator confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// A definition, or bytes that do not match their descriptor.
    Invalid(String),
    UnknownMethod(String),
    /// The version is outside the application's support horizon.
    UnsupportedVersion {
        method: String,
        version: u32,
        supported: Vec<u32>,
    },
}
impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid command: {reason}"),
            Self::UnknownMethod(method) => write!(f, "unregistered command {method}"),
            Self::UnsupportedVersion {
                method,
                version,
                supported,
            } => write!(
                f,
                "{method} version {version} is outside the supported versions {supported:?}"
            ),
        }
    }
}
impl std::error::Error for CommandError {}
fn invalid<T>(reason: impl Into<String>) -> Result<T, CommandError> {
    Err(CommandError::Invalid(reason.into()))
}

/// The scalar types of command fields. Values use the `SqlValue` byte codec;
/// a boolean is the integer 0 or 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    Integer,
    Real,
    Text,
    Blob,
    Boolean,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandField {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ValueKind,
    #[serde(default)]
    pub nullable: bool,
}
impl CommandField {
    pub fn new(name: &str, kind: ValueKind, nullable: bool) -> Self {
        Self {
            name: name.into(),
            kind,
            nullable,
        }
    }
    fn admits(&self, value: &SqlValue) -> bool {
        match (self.kind, value) {
            (_, SqlValue::Null) => self.nullable,
            (ValueKind::Integer, SqlValue::Integer(_))
            | (ValueKind::Real, SqlValue::Real(_))
            | (ValueKind::Text, SqlValue::Text(_))
            | (ValueKind::Blob, SqlValue::Blob(_)) => true,
            (ValueKind::Boolean, SqlValue::Integer(flag)) => matches!(flag, 0 | 1),
            _ => false,
        }
    }
}

/// One version of one command, as an application declares it. Reader scopes
/// are named by table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDefinition {
    pub name: String,
    pub version: u32,
    /// The declared identity of the handler that executes this version. It
    /// is installed and relied on, not a proof of the executed code.
    pub handler: String,
    pub arguments: Vec<CommandField>,
    pub accepted: Vec<CommandField>,
    pub rejected: Vec<CommandField>,
    /// The reader scope arguments are sealed for.
    pub invocation: String,
    /// The reader scope of the typed result.
    pub result: String,
    /// Every invariant-relevant reader scope; the handler reads and writes
    /// these and nothing else.
    pub resources: Vec<String>,
}
/// The commands of an application: every version inside its support horizon.
/// A version that is not listed fails admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDefinitions {
    pub format: u32,
    /// The handler generation coordinators bind, distinct from the schema's.
    pub handler: u32,
    pub commands: Vec<CommandDefinition>,
}
impl CommandDefinitions {
    pub fn parse(json: &[u8]) -> Result<Self, CommandError> {
        if json.len() > MAX_COMMAND_DEFINITION_BYTES {
            return invalid("definition quota");
        }
        serde_json::from_slice(json).map_err(|error| CommandError::Invalid(error.to_string()))
    }
    /// The canonical descriptors. The same definitions always build the same set.
    pub fn build(&self) -> Result<CommandSet, CommandError> {
        if self.format != COMMAND_FORMAT {
            return invalid("unsupported command definition format");
        }
        if self.handler == 0 || self.commands.len() > MAX_COMMANDS {
            return invalid("handler generation or command count");
        }
        let mut descriptors: Vec<CommandDescriptor> = self
            .commands
            .iter()
            .map(CommandDescriptor::new)
            .collect::<Result<_, _>>()?;
        descriptors.sort_by(|a, b| (&a.name, a.version).cmp(&(&b.name, b.version)));
        for pair in descriptors.windows(2) {
            if pair[0].name != pair[1].name {
                continue;
            }
            if pair[0].version == pair[1].version {
                return invalid(format!("{} declares a version twice", pair[0].name));
            }
            // Every version of a method seals its arguments for one scope.
            if pair[0].invocation != pair[1].invocation {
                return invalid(format!(
                    "versions of {} must share their invocation scope",
                    pair[0].name
                ));
            }
        }
        // One confirmed sequence orders every command of the set, and an
        // executor proves the predecessor of each scope it reads.
        if descriptors
            .windows(2)
            .any(|pair| pair[0].resources != pair[1].resources)
        {
            return invalid("the commands of one domain must declare the same resources");
        }
        Ok(CommandSet {
            handler: self.handler,
            descriptors,
        })
    }
}

/// The canonical form of one command version. Its `id` binds the method,
/// version, handler identity, every field type and the scopes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub name: String,
    pub version: u32,
    pub handler: String,
    pub arguments: Vec<CommandField>,
    pub accepted: Vec<CommandField>,
    pub rejected: Vec<CommandField>,
    pub invocation: String,
    pub result: String,
    /// Sorted and unique.
    pub resources: Vec<String>,
    id: Commitment,
}
fn put_text(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend((text.len() as u32).to_le_bytes());
    bytes.extend(text.as_bytes());
}
impl CommandDescriptor {
    fn new(definition: &CommandDefinition) -> Result<Self, CommandError> {
        let name = &definition.name;
        let named = [
            name,
            &definition.handler,
            &definition.invocation,
            &definition.result,
        ];
        if definition.version == 0
            || named.iter().any(|text| text.is_empty() || text.len() > 128)
            || !crate::identifier(name)
        {
            return invalid(format!("command {name}: name, version, handler or scope"));
        }
        for fields in [
            &definition.arguments,
            &definition.accepted,
            &definition.rejected,
        ] {
            let mut names = BTreeSet::new();
            if fields.len() > MAX_COMMAND_FIELDS
                || fields
                    .iter()
                    .any(|field| !crate::identifier(&field.name) || !names.insert(&field.name))
            {
                return invalid(format!("command {name}: fields"));
            }
        }
        let resources: BTreeSet<&String> = definition.resources.iter().collect();
        if resources.is_empty()
            || resources.len() != definition.resources.len()
            || resources.len() > 32
            || !resources.contains(&definition.invocation)
            || !resources.contains(&definition.result)
        {
            return invalid(format!(
                "command {name}: resources must be unique and hold the invocation and result scopes"
            ));
        }
        let mut descriptor = Self {
            name: name.clone(),
            version: definition.version,
            handler: definition.handler.clone(),
            arguments: definition.arguments.clone(),
            accepted: definition.accepted.clone(),
            rejected: definition.rejected.clone(),
            invocation: definition.invocation.clone(),
            result: definition.result.clone(),
            resources: resources.into_iter().cloned().collect(),
            id: [0; 32],
        };
        descriptor.id = commitment("rocky.command.descriptor.v1", &descriptor.canonical());
        Ok(descriptor)
    }
    /// Little-endian counts and length-prefixed UTF-8, in declared order.
    pub fn canonical(&self) -> Vec<u8> {
        let mut bytes = b"rocky.command.v1".to_vec();
        put_text(&mut bytes, &self.name);
        bytes.extend(self.version.to_le_bytes());
        put_text(&mut bytes, &self.handler);
        for fields in [&self.arguments, &self.accepted, &self.rejected] {
            bytes.extend((fields.len() as u32).to_le_bytes());
            for field in fields {
                put_text(&mut bytes, &field.name);
                bytes.push(match field.kind {
                    ValueKind::Integer => 1,
                    ValueKind::Real => 2,
                    ValueKind::Text => 3,
                    ValueKind::Blob => 4,
                    ValueKind::Boolean => 5,
                });
                bytes.push(u8::from(field.nullable));
            }
        }
        put_text(&mut bytes, &self.invocation);
        put_text(&mut bytes, &self.result);
        bytes.extend((self.resources.len() as u32).to_le_bytes());
        for resource in &self.resources {
            put_text(&mut bytes, resource);
        }
        bytes
    }
    pub fn id(&self) -> Commitment {
        self.id
    }
    fn encode(
        &self,
        kind: u8,
        fields: &[CommandField],
        values: &[SqlValue],
    ) -> Result<Vec<u8>, CommandError> {
        if values.len() != fields.len() {
            return invalid(format!(
                "{} takes {} values, not {}",
                self.name,
                fields.len(),
                values.len()
            ));
        }
        let mut bytes = vec![COMMAND_FORMAT as u8, kind];
        bytes.extend(self.id);
        bytes.extend((values.len() as u32).to_le_bytes());
        for (field, value) in fields.iter().zip(values) {
            if !field.admits(value) {
                return invalid(format!("{}.{} holds another type", self.name, field.name));
            }
            value.encode(&mut bytes);
        }
        if bytes.len() > MAX_COMMAND_VALUE_BYTES {
            return invalid(format!("{} value quota", self.name));
        }
        Ok(bytes)
    }
    /// The kind byte and values of `bytes`, which must be exactly what
    /// `encode` writes for this descriptor.
    fn decode(&self, bytes: &[u8]) -> Result<(u8, Vec<SqlValue>), CommandError> {
        let mismatch = || CommandError::Invalid(format!("bytes are not a {} value", self.name));
        if bytes.len() > MAX_COMMAND_VALUE_BYTES || bytes.len() < 38 {
            return Err(mismatch());
        }
        let (kind, fields) = match bytes[1] {
            ARGUMENTS => (ARGUMENTS, &self.arguments),
            ACCEPTED => (ACCEPTED, &self.accepted),
            REJECTED => (REJECTED, &self.rejected),
            _ => return Err(mismatch()),
        };
        if bytes[0] != COMMAND_FORMAT as u8
            || bytes[2..34] != self.id
            || bytes[34..38] != (fields.len() as u32).to_le_bytes()
        {
            return Err(mismatch());
        }
        let mut rest = &bytes[38..];
        let mut values = Vec::with_capacity(fields.len());
        for _ in fields {
            values.push(SqlValue::decode(&mut rest).map_err(|_| mismatch())?);
        }
        // Rejects trailing bytes, other types and non-canonical spellings.
        if !rest.is_empty() || self.encode(kind, fields, &values)? != bytes {
            return Err(mismatch());
        }
        Ok((kind, values))
    }
    pub fn encode_arguments(&self, values: &[SqlValue]) -> Result<Vec<u8>, CommandError> {
        self.encode(ARGUMENTS, &self.arguments, values)
    }
    pub fn decode_arguments(&self, bytes: &[u8]) -> Result<Vec<SqlValue>, CommandError> {
        match self.decode(bytes)? {
            (ARGUMENTS, values) => Ok(values),
            _ => invalid(format!("bytes are not {} arguments", self.name)),
        }
    }
    pub fn encode_outcome(&self, outcome: &Outcome) -> Result<Vec<u8>, CommandError> {
        match outcome {
            Outcome::Accepted(values) => self.encode(ACCEPTED, &self.accepted, values),
            Outcome::Rejected(values) => self.encode(REJECTED, &self.rejected, values),
        }
    }
    pub fn decode_outcome(&self, bytes: &[u8]) -> Result<Outcome, CommandError> {
        match self.decode(bytes)? {
            (ACCEPTED, values) => Ok(Outcome::Accepted(values)),
            (REJECTED, values) => Ok(Outcome::Rejected(values)),
            _ => invalid(format!("bytes are not a {} outcome", self.name)),
        }
    }
}

/// The business result of a confirmed command. A rejection is confirmed
/// like an acceptance and changes no rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Accepted(Vec<SqlValue>),
    Rejected(Vec<SqlValue>),
}

/// The descriptors of an application, by method and version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandSet {
    handler: u32,
    descriptors: Vec<CommandDescriptor>,
}
impl CommandSet {
    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }
    /// The handler generation; 1 for an application without commands.
    pub fn handler(&self) -> u32 {
        self.handler.max(1)
    }
    pub fn descriptors(&self) -> &[CommandDescriptor] {
        &self.descriptors
    }
    /// Any version of `method`: versions share their scopes.
    pub fn method(&self, method: &str) -> Result<&CommandDescriptor, CommandError> {
        self.descriptors
            .iter()
            .rfind(|descriptor| descriptor.name == method)
            .ok_or_else(|| CommandError::UnknownMethod(method.into()))
    }
    pub fn get(&self, method: &str, version: u32) -> Result<&CommandDescriptor, CommandError> {
        self.method(method)?;
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.name == method && descriptor.version == version)
            .ok_or_else(|| CommandError::UnsupportedVersion {
                method: method.into(),
                version,
                supported: self
                    .descriptors
                    .iter()
                    .filter(|descriptor| descriptor.name == method)
                    .map(|descriptor| descriptor.version)
                    .collect(),
            })
    }
    /// One manifest method per command name. Its shape commits to every
    /// supported version, so the signed manifest binds the descriptors.
    pub fn methods(&self, publisher: [u8; 32], app: AppId) -> Vec<DefinitionItem> {
        let mut methods: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
        for descriptor in &self.descriptors {
            let versions = methods.entry(&descriptor.name).or_default();
            versions.extend(descriptor.version.to_le_bytes());
            versions.extend(descriptor.id);
        }
        methods
            .into_iter()
            .map(|(name, versions)| DefinitionItem {
                id: definition_item_id(publisher, app, "method", name),
                name: name.into(),
                shape: format!(
                    "rocky.command.v1:{}",
                    commitment("rocky.command.method.v1", &versions)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                ),
            })
            .collect()
    }
}

/// A Rust value with a fixed field list, for `TypedCommand`.
pub trait Record: Sized {
    fn fields() -> Vec<CommandField>;
    fn to_values(&self) -> Vec<SqlValue>;
    /// None when the values are not this record's.
    fn from_values(values: Vec<SqlValue>) -> Option<Self>;
}
/// A command whose argument and result types are checked at compile time.
/// The descriptor it is used with is checked against these types at run time.
///
/// ```
/// use rocky_core::*;
/// struct Quantity(i64);
/// struct Left(i64);
/// macro_rules! integer { ($record:ident, $field:literal) => {
///     impl Record for $record {
///         fn fields() -> Vec<CommandField> { vec![CommandField::new($field, ValueKind::Integer, false)] }
///         fn to_values(&self) -> Vec<SqlValue> { vec![SqlValue::Integer(self.0)] }
///         fn from_values(values: Vec<SqlValue>) -> Option<Self> {
///             match values.as_slice() { [SqlValue::Integer(value)] => Some(Self(*value)), _ => None }
///         }
///     }
/// } }
/// integer!(Quantity, "quantity");
/// integer!(Left, "remaining");
/// struct Reserve;
/// impl TypedCommand for Reserve {
///     const NAME: &'static str = "Reserve";
///     const VERSION: u32 = 1;
///     type Arguments = Quantity;
///     type Accepted = Left;
///     type Rejected = Left;
/// }
/// let set = CommandDefinitions { format: 1, handler: 1, commands: vec![Reserve::definition("reserve.v1", "stock", "stock", &["stock"])] }
///     .build().unwrap();
/// let descriptor = set.get("Reserve", 1).unwrap();
/// let bytes = Reserve::encode_arguments(descriptor, &Quantity(3)).unwrap();
/// assert_eq!(descriptor.decode_arguments(&bytes).unwrap(), vec![SqlValue::Integer(3)]);
/// let outcome = descriptor.encode_outcome(&Outcome::Rejected(vec![SqlValue::Integer(1)])).unwrap();
/// assert!(matches!(Reserve::decode_outcome(descriptor, &outcome), Ok(Err(Left(1)))));
/// ```
///
/// Another command's argument type does not compile:
///
/// ```compile_fail
/// use rocky_core::*;
/// # struct Quantity(i64);
/// # struct Left(i64);
/// # macro_rules! integer { ($record:ident, $field:literal) => {
/// #     impl Record for $record {
/// #         fn fields() -> Vec<CommandField> { vec![CommandField::new($field, ValueKind::Integer, false)] }
/// #         fn to_values(&self) -> Vec<SqlValue> { vec![SqlValue::Integer(self.0)] }
/// #         fn from_values(values: Vec<SqlValue>) -> Option<Self> {
/// #             match values.as_slice() { [SqlValue::Integer(value)] => Some(Self(*value)), _ => None }
/// #         }
/// #     }
/// # } }
/// # integer!(Quantity, "quantity");
/// # integer!(Left, "remaining");
/// # struct Reserve;
/// # impl TypedCommand for Reserve {
/// #     const NAME: &'static str = "Reserve";
/// #     const VERSION: u32 = 1;
/// #     type Arguments = Quantity;
/// #     type Accepted = Left;
/// #     type Rejected = Left;
/// # }
/// fn submit(descriptor: &CommandDescriptor) {
///     // `Left` is the result of `Reserve`, not its arguments.
///     let _ = Reserve::encode_arguments(descriptor, &Left(3));
/// }
/// ```
///
/// Neither does reading its result as another type:
///
/// ```compile_fail
/// use rocky_core::*;
/// # struct Quantity(i64);
/// # struct Left(i64);
/// # macro_rules! integer { ($record:ident, $field:literal) => {
/// #     impl Record for $record {
/// #         fn fields() -> Vec<CommandField> { vec![CommandField::new($field, ValueKind::Integer, false)] }
/// #         fn to_values(&self) -> Vec<SqlValue> { vec![SqlValue::Integer(self.0)] }
/// #         fn from_values(values: Vec<SqlValue>) -> Option<Self> {
/// #             match values.as_slice() { [SqlValue::Integer(value)] => Some(Self(*value)), _ => None }
/// #         }
/// #     }
/// # } }
/// # integer!(Quantity, "quantity");
/// # integer!(Left, "remaining");
/// # struct Reserve;
/// # impl TypedCommand for Reserve {
/// #     const NAME: &'static str = "Reserve";
/// #     const VERSION: u32 = 1;
/// #     type Arguments = Quantity;
/// #     type Accepted = Left;
/// #     type Rejected = Left;
/// # }
/// fn read(descriptor: &CommandDescriptor, bytes: &[u8]) {
///     let _: Result<Result<Quantity, Left>, CommandError> = Reserve::decode_outcome(descriptor, bytes);
/// }
/// ```
pub trait TypedCommand {
    const NAME: &'static str;
    const VERSION: u32;
    type Arguments: Record;
    type Accepted: Record;
    type Rejected: Record;

    /// This command's definition with its handler identity and scopes.
    fn definition(
        handler: &str,
        invocation: &str,
        result: &str,
        resources: &[&str],
    ) -> CommandDefinition {
        CommandDefinition {
            name: Self::NAME.into(),
            version: Self::VERSION,
            handler: handler.into(),
            arguments: Self::Arguments::fields(),
            accepted: Self::Accepted::fields(),
            rejected: Self::Rejected::fields(),
            invocation: invocation.into(),
            result: result.into(),
            resources: resources.iter().map(|name| (*name).into()).collect(),
        }
    }
    /// Refuses a descriptor of another command, version or field list.
    fn check(descriptor: &CommandDescriptor) -> Result<(), CommandError> {
        if descriptor.name != Self::NAME
            || descriptor.version != Self::VERSION
            || descriptor.arguments != Self::Arguments::fields()
            || descriptor.accepted != Self::Accepted::fields()
            || descriptor.rejected != Self::Rejected::fields()
        {
            return invalid(format!(
                "descriptor is not {} version {}",
                Self::NAME,
                Self::VERSION
            ));
        }
        Ok(())
    }
    fn encode_arguments(
        descriptor: &CommandDescriptor,
        arguments: &Self::Arguments,
    ) -> Result<Vec<u8>, CommandError> {
        Self::check(descriptor)?;
        descriptor.encode_arguments(&arguments.to_values())
    }
    fn decode_arguments(
        descriptor: &CommandDescriptor,
        bytes: &[u8],
    ) -> Result<Self::Arguments, CommandError> {
        Self::check(descriptor)?;
        Self::Arguments::from_values(descriptor.decode_arguments(bytes)?)
            .ok_or_else(|| CommandError::Invalid(format!("{} arguments", Self::NAME)))
    }
    fn encode_outcome(
        descriptor: &CommandDescriptor,
        outcome: &Result<Self::Accepted, Self::Rejected>,
    ) -> Result<Vec<u8>, CommandError> {
        Self::check(descriptor)?;
        descriptor.encode_outcome(&match outcome {
            Ok(accepted) => Outcome::Accepted(accepted.to_values()),
            Err(rejected) => Outcome::Rejected(rejected.to_values()),
        })
    }
    /// The outer result is the protocol's, the inner one the business's.
    #[allow(clippy::type_complexity)]
    fn decode_outcome(
        descriptor: &CommandDescriptor,
        bytes: &[u8],
    ) -> Result<Result<Self::Accepted, Self::Rejected>, CommandError> {
        Self::check(descriptor)?;
        let mismatch = || CommandError::Invalid(format!("{} outcome", Self::NAME));
        Ok(match descriptor.decode_outcome(bytes)? {
            Outcome::Accepted(values) => {
                Ok(Self::Accepted::from_values(values).ok_or_else(mismatch)?)
            }
            Outcome::Rejected(values) => {
                Err(Self::Rejected::from_values(values).ok_or_else(mismatch)?)
            }
        })
    }
}

/// Why a handler produced no output. Neither is a business rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandFailure {
    /// The input or the handler's output breaks the contract; retrying the
    /// same request cannot succeed.
    Invalid(String),
    /// The handler missed its deadline, failed or was busy. Nothing was
    /// prepared; the same ticket can run again.
    Retryable(String),
}
impl std::fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid command: {reason}"),
            Self::Retryable(reason) => write!(f, "handler unavailable: {reason}"),
        }
    }
}
impl From<CommandError> for CommandFailure {
    fn from(error: CommandError) -> Self {
        Self::Invalid(error.to_string())
    }
}

/// What a row command executes against: immutable and bound to the
/// confirmed predecessor it was read at.
#[derive(Clone, Copy, Debug)]
pub struct CommandCall<'a> {
    pub method: &'a str,
    pub version: u32,
    pub request: RequestId,
    pub caller: PrincipalId,
    pub arguments: &'a [u8],
    pub state: &'a crate::CommandRows,
    /// Sequence and head of the confirmed predecessor.
    pub predecessor: (u64, Commitment),
}
pub type CommandFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CommandFailure>> + Send + 'a>>;
pub type CommandOutputFuture<'a> = CommandFuture<'a, RowCommandOutput>;

/// Typed arguments and immutable keyed reads at the confirmed predecessor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerInput {
    pub method: String,
    pub version: u32,
    /// The declared handler identity of this version.
    pub handler: String,
    pub request: RequestId,
    pub caller: PrincipalId,
    pub predecessor: (u64, Commitment),
    pub arguments: Vec<SqlValue>,
    pub reads: crate::HandlerReads,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableWrite {
    /// Inserts or replaces the row with this key; every column, in order.
    Put { table: String, row: Vec<SqlValue> },
    /// Deletes the row with this primary key.
    Delete { table: String, key: Vec<SqlValue> },
}
/// What a handler decides. A rejection carries no writes and no jobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerOutput {
    pub outcome: Outcome,
    pub writes: Vec<TableWrite>,
    /// Durable job intents, each readable by the named table's scope.
    pub jobs: Vec<(String, Vec<u8>)>,
}
/// Where handlers run: in this process, or in the application's language
/// behind a callback. The host owns scheduling and the deadline. A handler
/// gets no database connection, key or coordinator credential, and must
/// decide from its input alone.
pub trait CommandHost: Send + Sync {
    fn handle(&self, input: HandlerInput) -> CommandFuture<'_, HandlerOutput>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definitions() -> CommandDefinitions {
        CommandDefinitions::parse(
            br#"{"format":1,"handler":2,"commands":[
            {"name":"Reserve","version":2,"handler":"stock.reserve.v2",
             "arguments":[{"name":"item","type":"text"},{"name":"quantity","type":"integer"},{"name":"note","type":"text","nullable":true}],
             "accepted":[{"name":"remaining","type":"integer"}],
             "rejected":[{"name":"remaining","type":"integer"},{"name":"final","type":"boolean"}],
             "invocation":"stock","result":"stock","resources":["stock","audit"]},
            {"name":"Reserve","version":1,"handler":"stock.reserve.v1",
             "arguments":[{"name":"quantity","type":"integer"}],
             "accepted":[],"rejected":[],
             "invocation":"stock","result":"stock","resources":["audit","stock"]}]}"#,
        )
        .unwrap()
    }
    fn text(value: &str) -> SqlValue {
        SqlValue::Text(value.into())
    }

    #[test]
    fn descriptors_are_canonical_and_bind_every_declared_part() {
        let set = definitions().build().unwrap();
        assert_eq!(set, definitions().build().unwrap());
        assert_eq!(set.handler(), 2);
        let current = set.get("Reserve", 2).unwrap();
        assert_eq!(current.resources, ["audit", "stock"]);
        // The identity is a fixed function of the definition.
        assert_eq!(
            current
                .id()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "af9352f5853f0796cbd631a64d8f70953576f563c9b2a2946ad476748f973036"
        );
        let mut ids = BTreeSet::from([current.id(), set.get("Reserve", 1).unwrap().id()]);
        let mut changed = definitions();
        for change in [
            |d: &mut CommandDefinition| d.handler = "stock.reserve.v3".into(),
            |d: &mut CommandDefinition| d.arguments[1].kind = ValueKind::Real,
            |d: &mut CommandDefinition| d.arguments[2].nullable = false,
            |d: &mut CommandDefinition| d.rejected.swap(0, 1),
            |d: &mut CommandDefinition| d.accepted[0].name = "left".into(),
        ] {
            change(&mut changed.commands[0]);
            assert!(ids.insert(changed.build().unwrap().get("Reserve", 2).unwrap().id()));
        }
        // The manifest method commits to every supported version.
        let methods = |set: &CommandSet| set.methods([7; 32], AppId([8; 16]));
        assert_eq!(methods(&set).len(), 1);
        assert_ne!(methods(&set), methods(&changed.build().unwrap()));
        let mut one = definitions();
        one.commands.pop();
        assert_ne!(
            methods(&set)[0].shape,
            methods(&one.build().unwrap())[0].shape
        );
        assert_eq!(methods(&set)[0].id, methods(&one.build().unwrap())[0].id);
    }

    #[test]
    fn values_round_trip_only_as_their_descriptor_declares() {
        let set = definitions().build().unwrap();
        let current = set.get("Reserve", 2).unwrap();
        let arguments = vec![text("bolt"), SqlValue::Integer(3), SqlValue::Null];
        let bytes = current.encode_arguments(&arguments).unwrap();
        assert_eq!(current.decode_arguments(&bytes).unwrap(), arguments);
        let rejected = Outcome::Rejected(vec![SqlValue::Integer(1), SqlValue::Integer(1)]);
        let outcome = current.encode_outcome(&rejected).unwrap();
        assert_eq!(current.decode_outcome(&outcome).unwrap(), rejected);

        // Arguments are not an outcome, and another version's bytes are foreign.
        assert!(current.decode_outcome(&bytes).is_err());
        assert!(current.decode_arguments(&outcome).is_err());
        assert!(
            set.get("Reserve", 1)
                .unwrap()
                .decode_arguments(&bytes)
                .is_err()
        );
        for invalid in [
            vec![text("bolt"), text("3"), SqlValue::Null],
            vec![SqlValue::Null, SqlValue::Integer(3), SqlValue::Null],
            vec![text("bolt"), SqlValue::Integer(3)],
        ] {
            assert!(current.encode_arguments(&invalid).is_err());
        }
        assert!(
            current
                .encode_outcome(&Outcome::Rejected(vec![
                    SqlValue::Integer(1),
                    SqlValue::Integer(2)
                ]))
                .is_err()
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        let mut truncated = bytes.clone();
        truncated.pop();
        let mut retyped = bytes.clone();
        retyped[1] = ACCEPTED;
        for corrupt in [trailing, truncated, retyped, Vec::new()] {
            assert!(current.decode_arguments(&corrupt).is_err());
            assert!(current.decode_outcome(&corrupt).is_err());
        }
        assert!(
            current
                .encode_arguments(&[
                    SqlValue::Text("x".repeat(MAX_COMMAND_VALUE_BYTES)),
                    SqlValue::Integer(1),
                    SqlValue::Null
                ])
                .is_err()
        );
    }

    #[test]
    fn versions_outside_the_horizon_and_malformed_definitions_are_refused() {
        let set = definitions().build().unwrap();
        assert_eq!(
            set.get("Reserve", 3),
            Err(CommandError::UnsupportedVersion {
                method: "Reserve".into(),
                version: 3,
                supported: vec![1, 2]
            })
        );
        assert_eq!(
            set.get("Release", 1),
            Err(CommandError::UnknownMethod("Release".into()))
        );
        for change in [
            |d: &mut CommandDefinitions| d.format = 2,
            |d: &mut CommandDefinitions| d.handler = 0,
            |d: &mut CommandDefinitions| d.commands[1].version = 2,
            |d: &mut CommandDefinitions| d.commands[1].resources.pop().map(drop).unwrap_or(()),
            |d: &mut CommandDefinitions| d.commands[0].result = "elsewhere".into(),
            |d: &mut CommandDefinitions| d.commands[0].version = 0,
            |d: &mut CommandDefinitions| d.commands[0].name = "Reserve; DROP".into(),
            |d: &mut CommandDefinitions| {
                let field = d.commands[0].arguments[0].clone();
                d.commands[0].arguments.push(field)
            },
        ] {
            let mut definitions = definitions();
            change(&mut definitions);
            assert!(definitions.build().is_err());
        }
        assert!(
            CommandDefinitions::parse(br#"{"format":1,"handler":1,"commands":[],"x":1}"#).is_err()
        );
        assert!(CommandSet::default().is_empty());
        assert_eq!(CommandSet::default().handler(), 1);
    }
}
