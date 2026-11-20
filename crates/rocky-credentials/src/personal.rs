use crate::{
    CredentialChain, DatabaseContext, Error, PeerId, PrivateKey, PublicKey, RevocationSet,
    SignedCapability, encode,
};
use rocky_core::{ApplicationManifest, Commitment, DomainId, MeshId};
use serde::{Deserialize, Serialize};

mod bootstrap;
pub use bootstrap::{
    PersonalAct, PersonalAnswer, PersonalAudience, PersonalAuthorization, PersonalChallenge,
    PersonalCustodyRenewal, PersonalHello, PersonalPublication,
};

const NAMESPACE_FORMAT: u32 = 1;
const MAX_RP_ID: usize = 253;
const MAX_PURPOSE: usize = 64;
pub const MAX_PERSONAL_DEVICES: usize = crate::MAX_MEMBERSHIP_NODES;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "NamespaceRecord", into = "NamespaceRecord")]
pub struct PersonalNamespace {
    rp_id: String,
    purpose: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespaceRecord {
    version: u32,
    rp_id: String,
    purpose: String,
}
impl TryFrom<NamespaceRecord> for PersonalNamespace {
    type Error = Error;
    fn try_from(record: NamespaceRecord) -> Result<Self, Error> {
        if record.version != NAMESPACE_FORMAT {
            return Err(crate::denied("unsupported personal namespace version"));
        }
        Self::new(record.rp_id, record.purpose)
    }
}
impl From<PersonalNamespace> for NamespaceRecord {
    fn from(namespace: PersonalNamespace) -> Self {
        Self {
            version: NAMESPACE_FORMAT,
            rp_id: namespace.rp_id,
            purpose: namespace.purpose,
        }
    }
}
impl PersonalNamespace {
    pub fn new(rp_id: impl Into<String>, purpose: impl Into<String>) -> Result<Self, Error> {
        let (rp_id, purpose) = (rp_id.into(), purpose.into());
        let host =
            |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte);
        if rp_id.is_empty()
            || rp_id.len() > MAX_RP_ID
            || !rp_id.bytes().all(host)
            || rp_id.starts_with('.')
            || rp_id.ends_with('.')
            || purpose.is_empty()
            || purpose.len() > MAX_PURPOSE
            || !purpose
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(crate::denied("invalid personal namespace"));
        }
        Ok(Self { rp_id, purpose })
    }
    pub fn rp_id(&self) -> &str {
        &self.rp_id
    }
    pub fn purpose(&self) -> &str {
        &self.purpose
    }
    pub(crate) fn canonical(&self) -> Result<Vec<u8>, Error> {
        encode(&NamespaceRecord::from(self.clone()))
    }
    fn locate(&self, label: &str, owner: PublicKey) -> Result<[u8; 16], Error> {
        let digest = rocky_core::commitment(
            label,
            &encode(&(owner, NamespaceRecord::from(self.clone())))?,
        );
        let mut id = [0; 16];
        id.copy_from_slice(&digest[..16]);
        Ok(id)
    }
    pub fn mesh(&self, owner: PublicKey) -> Result<MeshId, Error> {
        self.locate("rocky.personal.mesh.v1", owner).map(MeshId)
    }
    pub fn domain(&self, owner: PublicKey) -> Result<DomainId, Error> {
        self.locate("rocky.personal.domain.v1", owner).map(DomainId)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[expect(
    clippy::large_enum_variant,
    reason = "one kind per genesis; boxing would only add an allocation"
)]
pub enum DatabaseKind {
    Shared,
    Personal {
        owner: PublicKey,
        namespace: PersonalNamespace,
        creation: [u8; 16],
    },
}
impl DatabaseKind {
    pub fn owner(&self) -> Option<PublicKey> {
        match self {
            Self::Shared => None,
            Self::Personal { owner, .. } => Some(*owner),
        }
    }
    pub fn is_personal(&self) -> bool {
        matches!(self, Self::Personal { .. })
    }
    pub(crate) fn validate(&self, manifest: &ApplicationManifest) -> Result<(), Error> {
        let Self::Personal {
            owner, namespace, ..
        } = self
        else {
            return Ok(());
        };
        let domain = namespace.domain(*owner)?;
        if manifest.publisher == *owner.as_bytes()
            || manifest.domains.iter().any(|rule| rule.domain != domain)
        {
            return Err(crate::denied("invalid personal genesis"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Genesis {
    kind: DatabaseKind,
    descriptor: Commitment,
}
impl Genesis {
    pub(crate) fn of(genesis: &crate::SignedManifest, publisher: PublicKey) -> Result<Self, Error> {
        let (_, kind) = genesis.verify_kind(publisher)?;
        Ok(Self {
            kind: kind.clone(),
            descriptor: genesis.descriptor()?,
        })
    }
    pub(crate) fn check_node(
        &self,
        issuer: PublicKey,
        capability: &crate::Capability,
    ) -> Result<(), Error> {
        let binding = capability.extended()?.personal;
        match (&self.kind, binding) {
            (DatabaseKind::Shared, None) => Ok(()),
            (DatabaseKind::Shared, Some(_)) => Err(PersonalError::ModeMismatch.into()),
            (DatabaseKind::Personal { owner, .. }, Some(_)) if issuer != *owner => {
                Err(PersonalError::WrongOwner.into())
            }
            (DatabaseKind::Personal { .. }, Some(binding))
                if binding.descriptor != self.descriptor =>
            {
                Err(PersonalError::WrongBinding.into())
            }
            (DatabaseKind::Personal { owner, .. }, None) if issuer == *owner => {
                Err(PersonalError::WrongBinding.into())
            }
            (DatabaseKind::Personal { .. }, _) => Ok(()),
        }
    }
    pub(crate) fn check_trusted(&self, subject: PublicKey) -> Result<(), Error> {
        match self.kind.owner() {
            Some(owner) if owner != subject => Err(PersonalError::OwnerOnly.into()),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonalError {
    ModeMismatch,
    WrongPublisher,
    WrongOwner,
    NamespaceMismatch,
    OwnerOnly,
    WrongBinding,
    Possession,
    Tombstoned { node: PeerId },
    DeviceLimit { limit: usize },
}
impl PersonalError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ModeMismatch => "mode_mismatch",
            Self::WrongPublisher | Self::WrongOwner => "wrong_owner",
            Self::NamespaceMismatch => "namespace_mismatch",
            Self::OwnerOnly => "personal_owner_only",
            Self::WrongBinding | Self::Possession => "invalid_device_certificate",
            Self::Tombstoned { .. } => "device_revoked",
            Self::DeviceLimit { .. } => "device_limit",
        }
    }
}
impl std::fmt::Display for PersonalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModeMismatch => f.write_str("the genesis pins another database mode"),
            Self::WrongPublisher => f.write_str("the genesis belongs to another root"),
            Self::WrongOwner => f.write_str("the personal database belongs to another owner"),
            Self::NamespaceMismatch => {
                f.write_str("the personal database belongs to another namespace")
            }
            Self::OwnerOnly => f.write_str(
                "a personal database admits only the owner's devices and custody principals",
            ),
            Self::WrongBinding => {
                f.write_str("the device certificate binds another database, grant or genesis")
            }
            Self::Possession => f.write_str("the device did not prove possession of its key"),
            Self::Tombstoned { node } => write!(f, "device {} was revoked", node.to_hex()),
            Self::DeviceLimit { limit } => {
                write!(f, "the owner already has {limit} devices in this database")
            }
        }
    }
}
impl std::error::Error for PersonalError {}
impl From<PersonalError> for Error {
    fn from(error: PersonalError) -> Self {
        Self::Personal(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalBinding {
    pub grant: Commitment,
    pub descriptor: Commitment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersonalLink {
    pub database: DatabaseContext,
    pub owner: PublicKey,
    pub node: PublicKey,
    pub grant: Commitment,
    pub descriptor: Commitment,
    pub certificate: Commitment,
}
impl PersonalLink {
    pub fn of(certificate: &SignedCapability) -> Result<Option<Self>, Error> {
        let extended = certificate.capability.extended()?;
        let Some(binding) = extended.personal else {
            return Ok(None);
        };
        let database = extended
            .database
            .ok_or_else(|| crate::denied("device certificate has no database"))?;
        let node = certificate.capability.subject;
        if certificate.capability.kind != rocky_core::PrincipalKind::Node
            || extended.node != Some(PeerId::of(node))
            || extended.sequence.is_some()
            || extended.previous.is_some()
        {
            return Err(crate::denied("invalid device certificate"));
        }
        crate::possession::check(&certificate.capability, certificate.issuer)?;
        Ok(Some(Self {
            database,
            owner: certificate.issuer,
            node,
            grant: binding.grant,
            descriptor: binding.descriptor,
            certificate: certificate.id()?,
        }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersonalContext {
    pub database: DatabaseContext,
    pub owner: PublicKey,
    pub grant: Commitment,
    pub descriptor: Commitment,
}
impl PersonalContext {
    pub fn check(&self, link: &PersonalLink) -> Result<(), PersonalError> {
        if link.owner != self.owner {
            return Err(PersonalError::WrongOwner);
        }
        if link.database != self.database
            || link.grant != self.grant
            || link.descriptor != self.descriptor
        {
            return Err(PersonalError::WrongBinding);
        }
        Ok(())
    }
}

impl CredentialChain {
    pub fn certify_personal(
        mut self,
        signer: &dyn crate::IdentityProvider,
        node: &PrivateKey,
        (owner, descriptor): (PublicKey, Commitment),
    ) -> Result<Self, Error> {
        let grant = self
            .0
            .first()
            .ok_or_else(|| crate::denied("missing authority"))?;
        let database = self.database_context(grant.issuer)?;
        if signer.public_key()? != owner
            || grant.capability.subject != owner
            || grant.capability.extended()?.trusted.is_none()
        {
            return Err(PersonalError::WrongOwner.into());
        }
        let binding = PersonalBinding {
            grant: grant.id()?,
            descriptor,
        };
        let possession = crate::NodePossession::sign(node, owner, database, Some(&binding))?;
        let last = self.holder(signer)?;
        let leaf = crate::Capability {
            subject: node.public_key(),
            kind: rocky_core::PrincipalKind::Node,
            delegations: 0,
            grants: Vec::new(),
            not_before: last.not_before,
            expires: crate::NEVER,
            extension: crate::Extension::new(crate::Extended {
                node: Some(PeerId::of(node.public_key())),
                database: Some(database),
                personal: Some(binding),
                possession: Some(possession),
                ..crate::Extended::default()
            })?,
        };
        self.0.push(SignedCapability::sign(leaf, signer)?);
        Ok(self)
    }
    pub(crate) fn check_personal(&self, index: usize) -> Result<(), Error> {
        let Some(link) = PersonalLink::of(&self.0[index])? else {
            return Ok(());
        };
        let grant = self
            .0
            .first()
            .ok_or_else(|| crate::denied("missing authority"))?;
        if index + 1 != self.0.len()
            || grant.capability.subject != link.owner
            || grant.capability.extended()?.trusted.is_none()
            || grant.id()? != link.grant
        {
            return Err(PersonalError::WrongBinding.into());
        }
        Ok(())
    }
}

impl RevocationSet {
    pub fn tombstones(&self, owner: PublicKey, node: PublicKey) -> bool {
        self.identity_unlinks(owner, PeerId::of(node)) || self.root_names(node)
    }
    pub fn personal_devices(
        &self,
        owner: PublicKey,
    ) -> Result<std::collections::BTreeSet<PublicKey>, Error> {
        let mut devices = std::collections::BTreeSet::new();
        for certificate in self.certificates() {
            if let Some(link) = PersonalLink::of(certificate)?
                && link.owner == owner
                && !self.tombstones(owner, link.node)
                && !self.ends_node_of(owner, link.node, false)
            {
                devices.insert(link.node);
            }
        }
        Ok(devices)
    }
    pub fn admit_personal(
        &mut self,
        certificate: SignedCapability,
        context: &PersonalContext,
    ) -> Result<bool, Error> {
        let link = PersonalLink::of(&certificate)?.ok_or(PersonalError::WrongBinding)?;
        context.check(&link)?;
        certificate.certifies(
            certificate.capability.subject,
            certificate.capability.not_before,
        )?;
        if self
            .certificates()
            .any(|held| held.id().ok() == Some(link.certificate))
        {
            return Ok(false);
        }
        if self.tombstones(link.owner, link.node) {
            return Err(PersonalError::Tombstoned {
                node: PeerId::of(link.node),
            }
            .into());
        }
        let devices = self.personal_devices(link.owner)?;
        if !devices.contains(&link.node) && devices.len() >= MAX_PERSONAL_DEVICES {
            return Err(PersonalError::DeviceLimit {
                limit: MAX_PERSONAL_DEVICES,
            }
            .into());
        }
        self.link(certificate)
    }
}

#[cfg(test)]
mod tests;
