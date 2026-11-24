use crate::request::Request;
use crate::{Access, Configuration, Database, PublicKey, Timestamp};
use std::sync::Arc;

#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct PersonId(PublicKey);
impl PersonId {
    pub fn new(key: PublicKey) -> Self {
        Self(key)
    }
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
pub struct DeviceId(PublicKey);
impl DeviceId {
    pub fn new(key: PublicKey) -> Self {
        Self(key)
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRequest {
    pub device: DeviceId,
    pub code: LinkCode,
    pub received: Timestamp,
    pub approved: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    NetworkRequired,
    Unreachable,
    NotApproved,
    CodeMismatch,
    UnknownRequest,
    UnknownDevice,
}
impl LinkError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NetworkRequired => "network_required",
            Self::Unreachable => "unreachable",
            Self::NotApproved => "link_not_approved",
            Self::CodeMismatch => "link_code_mismatch",
            Self::UnknownRequest => "link_request_unknown",
            Self::UnknownDevice => "device_unknown",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Unreachable | Self::NotApproved)
    }
    const CARRIED: [Self; 3] = [Self::NotApproved, Self::CodeMismatch, Self::UnknownRequest];
    pub(crate) fn carried(error: crate::Failure) -> crate::Failure {
        if let Some(code) = error
            .downcast_ref::<crate::peer::PeerError>()
            .map(|peer| peer.code.clone())
        {
            return match Self::CARRIED
                .into_iter()
                .find(|refusal| refusal.code() == code)
            {
                Some(refusal) => Box::new(refusal),
                None => error,
            };
        }
        match error.downcast_ref::<crate::ServiceError>() {
            Some(
                crate::ServiceError::Unreachable(_)
                | crate::ServiceError::TimedOut(_)
                | crate::ServiceError::NoRoute { .. },
            ) => Box::new(Self::Unreachable),
            _ => error,
        }
    }
}
impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NetworkRequired => "linking a device needs an attached network",
            Self::Unreachable => "the other device is not reachable",
            Self::NotApproved => "the other device has not approved this link yet",
            Self::CodeMismatch => "the link code does not match",
            Self::UnknownRequest => "no such link request; it expired or never arrived",
            Self::UnknownDevice => "no such device of a person in this database",
        })
    }
}
impl std::error::Error for LinkError {}

pub struct Opened {
    pub database: Database,
    pub identity: Option<crate::IdentityKey>,
}

pub struct PendingLink {
    pub(crate) configuration: Configuration,
    pub(crate) contact: String,
    pub(crate) awaiting: Arc<crate::self_enrollment::AwaitingLink>,
}
impl PendingLink {
    pub fn code(&self) -> &LinkCode {
        &self.awaiting.code
    }
    pub fn device(&self) -> DeviceId {
        DeviceId(self.awaiting.node)
    }
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TableRights {
    pub read: bool,
    pub insert: bool,
    pub update: bool,
    pub delete: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Me {
    pub person: PersonId,
    pub device: DeviceId,
    pub access: Access,
    pub admin: bool,
    pub owner: bool,
    pub tables: std::collections::BTreeMap<String, TableRights>,
    pub expires: Timestamp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: DeviceId,
    pub label: Option<String>,
    pub linked: Timestamp,
    pub last_seen: Option<Timestamp>,
    pub schema: Option<ReportedSchema>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportedSchema {
    pub version: u32,
    pub reported: Timestamp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub id: PersonId,
    pub access: Access,
    pub admin: bool,
    pub owner: bool,
    pub label: Option<String>,
    pub joined: Timestamp,
    pub invited_by: Option<PersonId>,
    pub invitation: Option<crate::InvitationId>,
    pub partitions: Vec<crate::Partition>,
    pub devices: Vec<Device>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    Departed,
    Compromised,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logout {
    KeepData,
    PurgeOnClose,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessError {
    UnknownPerson,
    UnknownRole(crate::RoleName),
    TargetIsAdmin,
    NotAdmin,
    NotPerPartition {
        role: crate::RoleName,
        group: String,
    },
    PartitionsHeld,
    PartialGroup {
        group: String,
    },
    TooManyPartitions {
        limit: usize,
    },
}
impl AccessError {
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
    #[cfg(any(test, feature = "test-support"))]
    pub async fn renew(&self) -> Result<(), crate::Error> {
        self.send(Request::Renew).await
    }
    pub async fn me(&self) -> Result<Me, crate::Error> {
        self.send(Request::Me).await
    }
    pub async fn people(&self) -> Result<Vec<Person>, crate::Error> {
        self.send(Request::People).await
    }
    pub async fn set_access(&self, person: PersonId, access: Access) -> Result<(), crate::Error> {
        self.send(|reply| Request::SetAccess(person, access, reply))
            .await
    }
    pub async fn grant_partition(
        &self,
        person: PersonId,
        partition: &crate::Partition,
    ) -> Result<(), crate::Error> {
        let partition = partition.clone();
        self.send(|reply| Request::GrantPartition(person, partition, reply))
            .await
    }
    pub async fn revoke_partition(
        &self,
        person: PersonId,
        partition: &crate::Partition,
    ) -> Result<(), crate::Error> {
        let partition = partition.clone();
        self.send(|reply| Request::RevokePartition(person, partition, reply))
            .await
    }
    pub async fn create_partition(
        &self,
        partition: &crate::Partition,
    ) -> Result<crate::Partition, crate::Error> {
        let partition = partition.clone();
        self.send(|reply| Request::CreatePartition(partition, reply))
            .await
    }
    pub async fn remove(&self, person: PersonId, removal: Removal) -> Result<(), crate::Error> {
        self.send_rooted(None, |root, reply| {
            Request::Remove(person, removal, root, reply)
        })
        .await
    }
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
    pub async fn invite(&self, invite: crate::Invite) -> Result<crate::Invitation, crate::Error> {
        self.send(|reply| Request::Invite(Box::new(invite), reply))
            .await
    }
    pub async fn invitations(&self) -> Result<Vec<crate::PendingInvitation>, crate::Error> {
        self.send(Request::Invitations).await
    }
    pub async fn cancel_invitation(&self, id: crate::InvitationId) -> Result<(), crate::Error> {
        self.send(|reply| Request::CancelInvitation(id, reply))
            .await
    }
    pub async fn link_requests(&self) -> Result<Vec<LinkRequest>, crate::Error> {
        self.send(Request::LinkRequests).await
    }
    pub async fn approve_link(
        &self,
        device: DeviceId,
        code: &LinkCode,
    ) -> Result<(), crate::Error> {
        self.send(|reply| Request::ApproveLink(device.0, code.clone(), reply))
            .await
    }
    pub async fn unlink(&self, device: DeviceId) -> Result<(), crate::Error> {
        self.send(|reply| Request::Unlink(crate::PeerId::of(device.0), reply))
            .await
    }
    pub async fn logout(&self, logout: Logout) -> Result<(), crate::Error> {
        self.send(|reply| Request::Logout(logout, reply)).await
    }
}
