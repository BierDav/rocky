use crate::request::Request;
use crate::{Access, Configuration, Database, PublicKey, Timestamp};
use std::sync::Arc;

#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
/// A person, identified by their person key.
pub struct PersonId(PublicKey);
impl PersonId {
    /// The person with this key.
    pub fn new(key: PublicKey) -> Self {
        Self(key)
    }
    /// The person a person key signs for, as `Invite::for_identity` and `remove` take it; fails
    /// when the provider cannot name its key.
    pub fn of(provider: &dyn crate::IdentityProvider) -> Result<Self, crate::Error> {
        provider
            .public_key()
            .map(Self)
            .map_err(|error| crate::Error::failed(Box::new(error)))
    }
    /// The person key.
    pub fn key(&self) -> PublicKey {
        self.0
    }
}
impl From<PublicKey> for PersonId {
    fn from(key: PublicKey) -> Self {
        Self(key)
    }
}
impl std::fmt::Display for PersonId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.to_hex())
    }
}
impl std::fmt::Debug for PersonId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PersonId({self})")
    }
}

#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
/// A device, identified by its device key.
pub struct DeviceId(PublicKey);
impl DeviceId {
    /// The device with this key.
    pub fn new(key: PublicKey) -> Self {
        Self(key)
    }
    /// The device key.
    pub fn key(&self) -> PublicKey {
        self.0
    }
}
impl From<PublicKey> for DeviceId {
    fn from(key: PublicKey) -> Self {
        Self(key)
    }
}
impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.to_hex())
    }
}
impl std::fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceId({self})")
    }
}

/// A short code both devices show while linking; the person approves only when they match.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct LinkCode(pub(crate) String);
impl std::fmt::Display for LinkCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::fmt::Debug for LinkCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LinkCode({})", self.0)
    }
}
impl std::str::FromStr for LinkCode {
    type Err = LinkError;
    fn from_str(code: &str) -> Result<Self, LinkError> {
        let code = code.trim();
        if code.is_empty() || code.len() > 32 || !code.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(LinkError::CodeMismatch);
        }
        Ok(Self(code.to_owned()))
    }
}

/// A request from a new device to link to this device's person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRequest {
    /// The new device.
    pub device: DeviceId,
    /// The code it shows.
    pub code: LinkCode,
    /// When the request arrived.
    pub received: Timestamp,
    /// Whether this person already approved it.
    pub approved: bool,
}

/// Why linking a device failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// Linking needs an attached network.
    NetworkRequired,
    /// The other device has not approved the link yet; retry after it does.
    NotApproved,
    /// The approved code is not the one the new device shows.
    CodeMismatch,
    /// No such link request; it expired or never arrived.
    UnknownRequest,
    /// No device of a person in this database has this id.
    UnknownDevice,
}
impl LinkError {
    /// A stable snake-case code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NetworkRequired => "network_required",
            Self::NotApproved => "link_not_approved",
            Self::CodeMismatch => "link_code_mismatch",
            Self::UnknownRequest => "link_request_unknown",
            Self::UnknownDevice => "device_unknown",
        }
    }
    /// Whether a retry may succeed.
    pub fn retryable(&self) -> bool {
        matches!(self, Self::NotApproved)
    }
    const CARRIED: [Self; 3] = [Self::NotApproved, Self::CodeMismatch, Self::UnknownRequest];
    pub(crate) fn carried(error: crate::Failure) -> crate::Failure {
        let refusal = error
            .downcast_ref::<crate::peer::PeerError>()
            .and_then(|peer| {
                Self::CARRIED
                    .into_iter()
                    .find(|refusal| refusal.code() == peer.code)
            });
        match refusal {
            Some(refusal) => Box::new(refusal),
            None => error,
        }
    }
}
impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NetworkRequired => "linking a device needs an attached network",
            Self::NotApproved => "the other device has not approved this link yet",
            Self::CodeMismatch => "the link code does not match",
            Self::UnknownRequest => "no such link request; it expired or never arrived",
            Self::UnknownDevice => "no such device of a person in this database",
        })
    }
}
impl std::error::Error for LinkError {}

/// A database that joined or linked, with the person key it minted, if any.
pub struct Opened {
    /// The open database.
    pub database: Database,
    /// The person key a default invitation minted, returned once for the app to store; `None` when
    /// the person brought their own.
    pub identity: Option<crate::IdentityKey>,
}

/// A link waiting for approval on another device of the person.
pub struct PendingLink {
    pub(crate) configuration: Configuration,
    pub(crate) contact: String,
    pub(crate) awaiting: Arc<crate::self_enrollment::AwaitingLink>,
}
impl PendingLink {
    /// The code to compare with the one `Database::link_requests` shows on the other device.
    pub fn code(&self) -> &LinkCode {
        &self.awaiting.code
    }
    /// This new device's id.
    pub fn device(&self) -> DeviceId {
        DeviceId(self.awaiting.node)
    }
    /// Finishes the link after the other device approved it and opens the database.
    pub async fn confirm(&self) -> Result<Opened, crate::Error> {
        let install = crate::database::Install::Identity {
            contact: self.contact.clone(),
            confirmed: Some(self.awaiting.clone()),
        };
        let database = Database::launch(&self.configuration, install, None).await?;
        Ok(Opened {
            database,
            identity: None,
        })
    }
}
impl std::fmt::Debug for PendingLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingLink")
            .field("code", self.code())
            .field("device", &self.device())
            .finish_non_exhaustive()
    }
}

/// What this device may do to one table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TableRights {
    /// Read rows.
    pub read: bool,
    /// Insert rows.
    pub insert: bool,
    /// Update rows.
    pub update: bool,
    /// Delete rows.
    pub delete: bool,
}

/// Who this device belongs to and what it may do.
#[derive(Clone, Debug, PartialEq)]
pub struct Me {
    /// The person.
    pub person: PersonId,
    /// This device.
    pub device: DeviceId,
    /// The person's access.
    pub access: Access,
    /// Whether the person is an admin.
    pub admin: bool,
    /// Whether the person is the owner, from the root-signed owner grant.
    pub owner: bool,
    /// The rights per table.
    pub tables: std::collections::BTreeMap<String, TableRights>,
    /// When this device's credentials expire; a sync with an admin device renews them.
    pub expires: Timestamp,
}

/// One device of a person.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// The device.
    pub id: DeviceId,
    /// The label the invitation that admitted it gave.
    pub label: Option<String>,
    /// When it joined or linked.
    pub linked: Timestamp,
    /// When this device last heard from it, or `None` if never.
    pub last_seen: Option<Timestamp>,
    /// The schema version it last reported, or `None` if it has not reported since this device
    /// opened.
    pub schema: Option<ReportedSchema>,
}

/// The application schema version a device reported in its signed health report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportedSchema {
    /// The version.
    pub version: u32,
    /// When it was reported.
    pub reported: Timestamp,
}

/// A member of the database and their devices.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    /// The person.
    pub id: PersonId,
    /// Their access.
    pub access: Access,
    /// Whether they are an admin.
    pub admin: bool,
    /// Whether they are the owner.
    pub owner: bool,
    /// The label of their first invitation.
    pub label: Option<String>,
    /// When they joined.
    pub joined: Timestamp,
    /// Who invited them, or `None` for the owner.
    pub invited_by: Option<PersonId>,
    /// The invitation they joined with.
    pub invitation: Option<crate::InvitationId>,
    /// The partitions granted to them.
    pub partitions: Vec<crate::Partition>,
    /// Their devices.
    pub devices: Vec<Device>,
}

/// Why `revoke_admin` ends an admin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    /// They left or lost their devices; what they issued before stays valid.
    Departed,
    /// Their keys may be in other hands, so the revocation also removes the people they admitted
    /// and ends every credential they issued.
    Compromised,
}

/// What a logout does with the device's data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logout {
    /// Keeps the directory.
    KeepData,
    /// Deletes Rocky's files when the database closes, and the directory unless it holds others.
    PurgeOnClose,
}

/// Why an access change was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessError {
    /// No such person in this database.
    UnknownPerson,
    /// The schema declares no such role.
    UnknownRole(crate::RoleName),
    /// An admin's rights change only through the owner's `make_admin` and `revoke_admin`.
    TargetIsAdmin,
    /// The person is not an admin.
    NotAdmin,
    /// The person's role does not hold partitions of this group.
    NotPerPartition {
        /// The role.
        role: crate::RoleName,
        /// The group.
        group: String,
    },
    /// Whole access covers every partition; revoke the person's partitions first.
    PartitionsHeld,
    /// The person's role does not read every table of the partition group.
    PartialGroup {
        /// The group.
        group: String,
    },
    /// The person holds as many partitions as allowed.
    TooManyPartitions {
        /// The limit.
        limit: usize,
    },
}
impl AccessError {
    /// A stable snake-case code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownPerson => "person_unknown",
            Self::UnknownRole(_) => "role_unknown",
            Self::TargetIsAdmin => "target_is_admin",
            Self::NotAdmin => "not_admin",
            Self::NotPerPartition { .. } => "role_not_per_partition",
            Self::PartitionsHeld => "partitions_held",
            Self::PartialGroup { .. } => "role_reads_part_of_group",
            Self::TooManyPartitions { .. } => "too_many_partitions",
        }
    }
    pub(crate) fn of(
        error: &crate::Failure,
        access: &Access,
        partitions: &[crate::Partition],
    ) -> Self {
        if let Some(unknown) = error.downcast_ref::<crate::access::UnknownRole>() {
            return Self::UnknownRole(unknown.0.clone());
        }
        match (access, partitions.first()) {
            (Access::Role(role), Some(partition)) => Self::NotPerPartition {
                role: role.clone(),
                group: partition.group.clone(),
            },
            _ => Self::PartitionsHeld,
        }
    }
}
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPerson => f.write_str("no such person in this database"),
            Self::UnknownRole(role) => write!(f, "the schema declares no role {role}"),
            Self::TargetIsAdmin => f.write_str("an admin's rights change only through the owner"),
            Self::NotAdmin => f.write_str("the person is not an admin"),
            Self::NotPerPartition { role, group } => {
                write!(f, "role {role} does not hold partitions of group {group}")
            }
            Self::PartitionsHeld => f.write_str(
                "whole access holds every partition; revoke the person's partitions first",
            ),
            Self::PartialGroup { group } => write!(
                f,
                "the person's role does not read every table of partition group {group}"
            ),
            Self::TooManyPartitions { limit } => {
                write!(f, "a person holds at most {limit} partitions")
            }
        }
    }
}
impl std::error::Error for AccessError {}

impl Database {
    /// Renews this device's credentials with an admin device now.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn renew(&self) -> Result<(), crate::Error> {
        self.send(Request::Renew).await
    }
    /// Who this device belongs to and what it may do.
    pub async fn me(&self) -> Result<Me, crate::Error> {
        self.send(Request::Me).await
    }
    /// Every member and their devices; admins only.
    pub async fn people(&self) -> Result<Vec<Person>, crate::Error> {
        self.send(Request::People).await
    }
    /// Changes a member's access; narrowing it revokes their credentials and rotates the keys of
    /// what they could read.
    pub async fn set_access(&self, person: PersonId, access: Access) -> Result<(), crate::Error> {
        self.send(|reply| Request::SetAccess(person, access, reply))
            .await
    }
    /// Grants a partition to every device of a person, including devices that join or link later.
    pub async fn grant_partition(
        &self,
        person: PersonId,
        partition: &crate::Partition,
    ) -> Result<(), crate::Error> {
        let partition = partition.clone();
        self.send(|reply| Request::GrantPartition(person, partition, reply))
            .await
    }
    /// Takes a partition back and rotates its key.
    pub async fn revoke_partition(
        &self,
        person: PersonId,
        partition: &crate::Partition,
    ) -> Result<(), crate::Error> {
        let partition = partition.clone();
        self.send(|reply| Request::RevokePartition(person, partition, reply))
            .await
    }
    /// Creates a partition's key so writes to it can start; admin devices only.
    pub async fn create_partition(
        &self,
        partition: &crate::Partition,
    ) -> Result<crate::Partition, crate::Error> {
        let partition = partition.clone();
        self.send(|reply| Request::CreatePartition(partition, reply))
            .await
    }
    /// Removes a member; their devices stop reading and writing at their next sync, and what they
    /// wrote before stays valid. An admin is `OwnerRequired`: end them with `revoke_admin`.
    pub async fn remove(&self, person: PersonId) -> Result<(), crate::Error> {
        self.send(|reply| Request::Remove(person, reply)).await
    }
    /// Makes a member an admin; needs the root key.
    pub async fn make_admin(
        &self,
        person: PersonId,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<(), crate::Error> {
        self.send_rooted(Some(root), |root, reply| {
            Request::MakeAdmin(person, root, reply)
        })
        .await
    }
    /// Ends an admin; needs the root key, and an admin who should stay a member is invited again.
    pub async fn revoke_admin(
        &self,
        person: PersonId,
        removal: Removal,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<(), crate::Error> {
        self.send_rooted(Some(root), |root, reply| {
            Request::RevokeAdmin(person, removal, root, reply)
        })
        .await
    }
    /// Issues an invitation; admins only, and the joiner needs this device online to redeem it.
    pub async fn invite(&self, invite: crate::Invite) -> Result<crate::Invitation, crate::Error> {
        self.send(|reply| Request::Invite(Box::new(invite), reply))
            .await
    }
    /// Open and redeemed invitations this database knows.
    pub async fn invitations(&self) -> Result<Vec<crate::PendingInvitation>, crate::Error> {
        self.send(Request::Invitations).await
    }
    /// Cancels an unused invitation.
    pub async fn cancel_invitation(&self, id: crate::InvitationId) -> Result<(), crate::Error> {
        self.send(|reply| Request::CancelInvitation(id, reply))
            .await
    }
    /// Requests from new devices to link to this person.
    pub async fn link_requests(&self) -> Result<Vec<LinkRequest>, crate::Error> {
        self.send(Request::LinkRequests).await
    }
    /// Approves a link request whose code the person compared with the new device's.
    pub async fn approve_link(
        &self,
        device: DeviceId,
        code: &LinkCode,
    ) -> Result<(), crate::Error> {
        self.send(|reply| Request::ApproveLink(device.0, code.clone(), reply))
            .await
    }
    /// Ends one device: one of the person's own, or as an admin a member's lost device.
    pub async fn unlink(&self, device: DeviceId) -> Result<(), crate::Error> {
        self.send(|reply| Request::Unlink(rocky_credentials::PeerId::of(device.0), reply))
            .await
    }
    /// Ends this device's session; `PurgeOnClose` deletes its data at close.
    pub async fn logout(&self, logout: Logout) -> Result<(), crate::Error> {
        self.send(|reply| Request::Logout(logout, reply)).await
    }
}
