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
    IdentityRequired,
    NetworkRequired,
    Unreachable,
    NotApproved,
    CodeMismatch,
    UnknownRequest,
    UnknownDevice,
    AdminRequired,
    Removed,
}
impl LinkError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::IdentityRequired => "identity_required",
            Self::NetworkRequired => "network_required",
            Self::Unreachable => "unreachable",
            Self::NotApproved => "link_not_approved",
            Self::CodeMismatch => "link_code_mismatch",
            Self::UnknownRequest => "link_request_unknown",
            Self::UnknownDevice => "device_unknown",
            Self::AdminRequired => "admin_required",
            Self::Removed => "removed",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Unreachable | Self::NotApproved)
    }
    const CARRIED: [Self; 4] = [
        Self::NotApproved,
        Self::CodeMismatch,
        Self::UnknownRequest,
        Self::Removed,
    ];
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
            Self::IdentityRequired => {
                "linking a device needs the person's identity; configure it with Configuration::identity"
            }
            Self::NetworkRequired => "linking a device needs an attached network",
            Self::Unreachable => "the other device is not reachable",
            Self::NotApproved => "the other device has not approved this link yet",
            Self::CodeMismatch => "the link code does not match",
            Self::UnknownRequest => "no such link request; it expired or never arrived",
            Self::UnknownDevice => "no such device of a person in this database",
            Self::AdminRequired => "only an admin unlinks another person's device",
            Self::Removed => "the person or device was removed",
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevocationResult {
    pub statement: String,
    pub revocations: crate::RevocationStatus,
    pub cut_dropped_stages: Vec<DroppedStage>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRevocationReceipt {
    pub node: crate::PeerId,
    pub statement: rocky_core::Commitment,
    pub local: bool,
    pub delivered: std::collections::BTreeSet<crate::PeerId>,
    pub pending: std::collections::BTreeSet<crate::PeerId>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DroppedStage {
    pub node: PublicKey,
    pub sequence: u64,
    pub head: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustGrant {
    pub identity: String,
    pub capability: String,
    pub grant: String,
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub principal: rocky_credentials::PublicKey,
    pub node: rocky_credentials::PublicKey,
    pub revocations: crate::RevocationStatus,
    pub expires: u64,
    pub issuer: bool,
    pub access: Access,
    pub transport: String,
    pub readable: usize,
    pub writable: usize,
    pub writer: crate::WriterId,
}

impl Database {
    pub async fn info(&self) -> Result<DeviceInfo, crate::Error> {
        self.send(Request::Info).await
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
        self.send_signed(self.identity(), |reply| {
            Request::Unlink(crate::PeerId::of(device.0), reply)
        })
        .await
    }
    pub async fn device_revocation_receipt(
        &self,
        node: crate::PeerId,
    ) -> Result<Option<DeviceRevocationReceipt>, crate::Error> {
        self.send(|reply| Request::DeviceRevocationReceipt(node, reply))
            .await
    }
    pub async fn renew(&self) -> Result<(), crate::Error> {
        self.send(Request::Renew).await
    }
    pub async fn sync_now_with_identity(
        &self,
        identity: Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::SyncReport, crate::Error> {
        self.send_signed(Some(identity), Request::Sync).await
    }
    pub async fn logout(&self, purge_on_close: bool) -> Result<(), crate::Error> {
        self.send(|reply| Request::Logout(purge_on_close, reply))
            .await
    }
    pub async fn trust(
        &self,
        identity: PublicKey,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<TrustGrant, crate::Error> {
        self.send_rooted(Some(root), |root, reply| {
            Request::Trust(identity, root, reply)
        })
        .await
    }
    pub async fn revoke_trusted(
        &self,
        identity: PublicKey,
        mode: crate::RevocationMode,
        root: Arc<dyn crate::IdentityProvider>,
    ) -> Result<RevocationResult, crate::Error> {
        self.revoke_identity_with_root(identity, mode, Some(root), true)
            .await
    }
    pub async fn revoke_identity(
        &self,
        identity: PublicKey,
    ) -> Result<RevocationResult, crate::Error> {
        self.revoke_identity_with_root(identity, crate::RevocationMode::Lost, None, true)
            .await
    }
    pub async fn revoke_identity_with_root(
        &self,
        identity: PublicKey,
        mode: crate::RevocationMode,
        root: Option<Arc<dyn crate::IdentityProvider>>,
        keep: bool,
    ) -> Result<RevocationResult, crate::Error> {
        self.send_rooted(root, |root, reply| {
            Request::RevokeIdentity(identity, mode, root, keep, reply)
        })
        .await
    }
    pub async fn install_trust(&self, grant: &str) -> Result<(), crate::Error> {
        self.send(|reply| Request::InstallTrust(grant.into(), reply))
            .await
    }
}
