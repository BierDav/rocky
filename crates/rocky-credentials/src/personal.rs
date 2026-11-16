//! Personal databases: one owner identity pinned in the root-signed genesis.
//! Owner devices hold independent certificates, indexed by owner and node
//! key, so concurrent additions never fork each other.
use crate::{
    CredentialChain, DatabaseContext, Error, PeerId, PrivateKey, PublicKey, RevocationSet,
    Signature, SignedCapability, encode, signing_bytes,
};
use rocky_core::{ApplicationManifest, Commitment, DomainId, MeshId};
use serde::{Deserialize, Serialize};

mod bootstrap;
pub use bootstrap::{
    PersonalAct, PersonalAnswer, PersonalAudience, PersonalAuthorization, PersonalChallenge,
    PersonalCustodyRenewal, PersonalHello, PersonalPublication,
};

/// The one current namespace format; others are refused.
const NAMESPACE_FORMAT: u32 = 1;
const MAX_RP_ID: usize = 253;
const MAX_PURPOSE: usize = 64;
/// Live owner devices one replica admits by itself. Independent offline
/// additions can exceed it after merging; nothing is ever evicted for it.
pub const MAX_PERSONAL_DEVICES: usize = crate::MAX_MEMBERSHIP_NODES;
const POSSESSION: &str = "rocky.personal-device.possession.v1";

/// Which personal database of an owner: the WebAuthn relying party and the
/// application's account-root purpose. Changing either names another database.
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
    /// `rp_id` is a lowercase DNS name; `purpose` is printable ASCII.
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
    /// The one current canonical encoding, versioned.
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
    /// The mesh a fresh device that knows only `owner` and this namespace finds.
    pub fn mesh(&self, owner: PublicKey) -> Result<MeshId, Error> {
        self.locate("rocky.personal.mesh.v1", owner).map(MeshId)
    }
    /// The domain of the owner's personal database in this namespace.
    pub fn domain(&self, owner: PublicKey) -> Result<DomainId, Error> {
        self.locate("rocky.personal.domain.v1", owner).map(DomainId)
    }
}

/// The database mode the root pins in its genesis; it never changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[expect(
    clippy::large_enum_variant,
    reason = "one kind per genesis; boxing would only add an allocation"
)]
pub enum DatabaseKind {
    /// Identities the root or its issuers admit; links need matching codes.
    Shared,
    /// Only the owner's devices and explicitly granted custody principals.
    Personal {
        owner: PublicKey,
        namespace: PersonalNamespace,
        /// Random per create: independent creates of one owner and
        /// namespace never share a descriptor, so they conflict.
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
    /// A personal owner is never the root, and its manifest names only
    /// the derived domain.
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

/// The verified genesis a holder judges node certificates and trusted
/// grants by: its mode and descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// A shared database holds no personal certificate. A personal one
    /// trusts only its owner, whose nodes each hold a personal certificate
    /// bound to this genesis; custody principals keep sequenced links.
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
    /// The root's trusted grant in a personal database goes to the owner.
    pub(crate) fn check_trusted(&self, subject: PublicKey) -> Result<(), Error> {
        match self.kind.owner() {
            Some(owner) if owner != subject => Err(PersonalError::OwnerOnly.into()),
            _ => Ok(()),
        }
    }
}

/// Why a personal database refused a mode, owner, device or admission.
/// Nothing was stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonalError {
    /// The genesis pins another mode than the caller requires.
    ModeMismatch,
    /// The genesis is signed by another root.
    WrongPublisher,
    /// The genesis or certificate names another owner.
    WrongOwner,
    /// The genesis pins another namespace, or the database is not where
    /// the namespace locates it.
    NamespaceMismatch,
    /// A personal database admits no other readers, writers or issuers.
    OwnerOnly,
    /// The certificate binds another database, grant or genesis.
    WrongBinding,
    /// The node did not prove possession of its key.
    Possession,
    /// A permanent statement ended this node; no certificate restores it.
    Tombstoned { node: PeerId },
    /// The owner already has this many live devices here.
    DeviceLimit { limit: usize },
}
impl PersonalError {
    /// The stable code of the call contract.
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

/// What a personal device certificate binds besides its database: the
/// root's trusted grant to the owner, the genesis descriptor and the node
/// key's own signature over both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalBinding {
    pub grant: Commitment,
    pub descriptor: Commitment,
    pub possession: Signature,
}
impl PersonalBinding {
    fn message(
        database: DatabaseContext,
        owner: PublicKey,
        node: PublicKey,
        (grant, descriptor): (Commitment, Commitment),
    ) -> Result<Vec<u8>, Error> {
        signing_bytes(POSSESSION, &(database, owner, node, grant, descriptor))
    }
    pub(crate) fn sign(
        database: DatabaseContext,
        owner: PublicKey,
        node: &PrivateKey,
        (grant, descriptor): (Commitment, Commitment),
    ) -> Result<Self, Error> {
        let message = Self::message(database, owner, node.public_key(), (grant, descriptor))?;
        Ok(Self {
            grant,
            descriptor,
            possession: node.sign(&message),
        })
    }
}

/// A personal node certificate's key signed its own binding.
pub(crate) fn check_possession(
    capability: &crate::Capability,
    owner: PublicKey,
) -> Result<(), Error> {
    let extended = capability.extended()?;
    let (Some(binding), Some(database)) = (extended.personal, extended.database) else {
        return Ok(());
    };
    let node = capability.subject;
    let message =
        PersonalBinding::message(database, owner, node, (binding.grant, binding.descriptor))?;
    if !node.verify(&message, &binding.possession) {
        return Err(PersonalError::Possession.into());
    }
    Ok(())
}

/// One owner device: a personal node certificate, checked for shape and
/// possession. It carries no sequence and names no predecessor.
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
    /// `None` for a certificate that is not a personal one.
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
        check_possession(&certificate.capability, certificate.issuer)?;
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

/// What every device certificate of one personal database binds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersonalContext {
    pub database: DatabaseContext,
    pub owner: PublicKey,
    /// The root's trusted grant to the owner.
    pub grant: Commitment,
    /// The genesis descriptor (`SignedManifest::descriptor`).
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
    /// The pinned `owner`, which holds the root's trusted grant this chain
    /// starts with, certifies `node` through `signer` independently of its
    /// other devices; the node signs its possession of the key.
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
        let binding = PersonalBinding::sign(database, owner, node, (grant.id()?, descriptor))?;
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
                ..crate::Extended::default()
            })?,
        };
        self.0.push(SignedCapability::sign(leaf, signer)?);
        Ok(self)
    }
    /// A personal leaf binds the root's grant this chain starts with.
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
    /// Whether a permanent statement ends `node` of `owner`: the owner's
    /// unlink or a root statement naming it. A tombstone dominates every
    /// certificate, however often or freshly replayed.
    pub fn tombstones(&self, owner: PublicKey, node: PublicKey) -> bool {
        self.identity_unlinks(owner, PeerId::of(node)) || self.root_names(node)
    }
    /// The owner's live devices: personal certificates no tombstone, reset
    /// or other held statement ends. A device a reset signed out signs in
    /// again as a new node.
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
    /// Admits one owner device after checking its binding, its tombstone
    /// and the local device limit. Refusals change nothing; an existing
    /// device is never removed. Returns whether the certificate is new.
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
