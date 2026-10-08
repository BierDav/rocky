use std::fmt;

pub const VERSION_1: u32 = 1;
pub const MAX_COUNTER: u64 = i64::MAX as u64;
/// Generic framing ceiling; applications may impose a smaller payload limit.
pub const MAX_PAYLOAD_BYTES: usize = 65_536;

macro_rules! ids {
    ($($name:ident),+) => {$ (
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct $name(pub [u8; 16]);
    )+};
}
ids!(MeshId, AppId, DomainId, NodeId, PrincipalId);

macro_rules! counters {
    ($($(#[$attribute:meta])* $name:ident: $minimum:expr),+) => {$ (
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $(#[$attribute])*
        #[derive(serde::Serialize, serde::Deserialize)]
        #[serde(try_from = "u64", into = "u64")]
        pub struct $name(u64);
        impl TryFrom<u64> for $name {
            type Error = ProtocolError;
            fn try_from(value: u64) -> Result<Self, Self::Error> { Self::new(value) }
        }
        impl From<$name> for u64 { fn from(value: $name) -> Self { value.get() } }
        impl $name {
            pub fn new(value: u64) -> Result<Self, ProtocolError> {
                if !($minimum..=MAX_COUNTER).contains(&value) {
                    return Err(ProtocolError::InvalidCounter);
                }
                Ok(Self(value))
            }
            pub const fn get(self) -> u64 { self.0 }
            pub fn next(self) -> Result<Self, ProtocolError> {
                Self::new(self.0.checked_add(1).ok_or(ProtocolError::InvalidCounter)?)
            }
        }
    )+};
}
counters!(AuthorSequence: 1, LamportTime: 1, #[derive(Default)] LocalRevision: 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionBoundary {
    Local,
    Incoming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId {
    pub node: NodeId,
    pub sequence: AuthorSequence,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Scope {
    pub mesh: MeshId,
    pub app: AppId,
    pub domain: DomainId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Versions {
    pub protocol: u32,
    pub schema: u32,
    pub operation: u32,
}
impl Versions {
    pub const V1: Self = Self {
        protocol: VERSION_1,
        schema: VERSION_1,
        operation: VERSION_1,
    };
    pub fn validate(self) -> Result<(), ProtocolError> {
        for (kind, value) in [
            (VersionKind::Protocol, self.protocol),
            (VersionKind::Schema, self.schema),
            (VersionKind::Operation, self.operation),
        ] {
            if value != VERSION_1 {
                return Err(ProtocolError::UnsupportedVersion { kind, value });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub scope: Scope,
    pub caller: PrincipalId,
    pub versions: Versions,
    pub id: OperationId,
    pub lamport: LamportTime,
    pub entity: OperationId,
    pub dependencies: Vec<OperationId>,
    pub payload: Vec<u8>,
}
impl Envelope {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.versions.validate()?;
        if self.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        if self.dependencies.len() > 2
            || self.dependencies.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(ProtocolError::InvalidDependencies);
        }
        if self.dependencies.iter().any(|id| {
            *id == self.id || (id.node == self.id.node && id.sequence >= self.id.sequence)
        }) {
            return Err(ProtocolError::InvalidDependencies);
        }
        if self.id.sequence.get() > 1 {
            let predecessor = OperationId {
                node: self.id.node,
                sequence: AuthorSequence::new(self.id.sequence.get() - 1)?,
            };
            if !self.dependencies.contains(&predecessor) {
                return Err(ProtocolError::InvalidDependencies);
            }
        }
        Ok(())
    }

    /// Validate the union of predecessor and, for updates, entity creation.
    pub fn validate_dependencies(&self, is_create: bool) -> Result<(), ProtocolError> {
        self.validate()?;
        if is_create && self.entity != self.id {
            return Err(ProtocolError::InvalidDependencies);
        }
        if !is_create
            && (self.entity == self.id
                || (self.entity.node == self.id.node && self.entity.sequence >= self.id.sequence))
        {
            return Err(ProtocolError::InvalidDependencies);
        }
        let expected = required_dependencies(self.id, self.entity, is_create)?;
        if self.dependencies != expected {
            return Err(ProtocolError::InvalidDependencies);
        }
        Ok(())
    }
}

pub fn required_dependencies(
    id: OperationId,
    entity: OperationId,
    is_create: bool,
) -> Result<Vec<OperationId>, ProtocolError> {
    let mut deps = Vec::with_capacity(2);
    if id.sequence.get() > 1 {
        deps.push(OperationId {
            node: id.node,
            sequence: AuthorSequence::new(id.sequence.get() - 1)?,
        });
    }
    if !is_create {
        deps.push(entity);
    }
    deps.sort();
    deps.dedup();
    Ok(deps)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stamp {
    pub lamport: LamportTime,
    pub node: NodeId,
    pub sequence: AuthorSequence,
}
impl From<&Envelope> for Stamp {
    fn from(envelope: &Envelope) -> Self {
        Self {
            lamport: envelope.lamport,
            node: envelope.id.node,
            sequence: envelope.id.sequence,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppliedFrontier(pub Vec<(NodeId, AuthorSequence)>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionKind {
    Protocol,
    Schema,
    Operation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    UnsupportedVersion {
        kind: VersionKind,
        value: u32,
    },
    InvalidCounter,
    InvalidManifest,
    InvalidLength,
    InvalidValue,
    Truncated,
    TrailingBytes,
    InvalidDependencies,
    InvalidLamport {
        operation: OperationId,
        dependency: OperationId,
    },
    IdentityConflict(OperationId),
    ForeignScope,
    NotReady(Vec<OperationId>),
}
impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ProtocolError {}

/// Stable trusted-development identities, each with repeated literal bytes.
pub mod fixtures {
    use super::*;
    pub const MESH: MeshId = MeshId([0x11; 16]);
    pub const APP: AppId = AppId([0x22; 16]);
    pub const DOMAIN: DomainId = DomainId([0x33; 16]);
    pub const OWNER: PrincipalId = PrincipalId([0x44; 16]);
    pub const LAPTOP: NodeId = NodeId([0x55; 16]);
    pub const PHONE: NodeId = NodeId([0x66; 16]);
    pub const SCOPE: Scope = Scope {
        mesh: MESH,
        app: APP,
        domain: DOMAIN,
    };
}

/// Author serialization is scoped to an application domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorKey {
    pub scope: Scope,
    pub node: NodeId,
}
/// Revision counters from different domains cannot be compared as one cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevisionKey {
    pub scope: Scope,
}
