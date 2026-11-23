use crate::request::Request;
use crate::{Access, Failure, PublicKey, Session};

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Provisioning {
    pub uri: String,
    pub node: PublicKey,
    pub identity: PublicKey,
    pub access: Access,
    pub sequence: u64,
}
impl std::fmt::Debug for Provisioning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provisioning")
            .field("uri", &"<redacted>")
            .field("node", &self.node)
            .field("identity", &self.identity)
            .field("access", &self.access)
            .field("sequence", &self.sequence)
            .finish()
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Discovered {
    pub seen: Vec<SeenDevice>,
    pub links: Vec<LinkRequest>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeenDevice {
    pub node: PublicKey,
    pub identity: PublicKey,
    pub last_seen: u64,
    pub registry_known: bool,
    pub unknown_identity: bool,
    pub revoked: bool,
    pub unlinked: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LinkRequest {
    pub node: PublicKey,
    pub code: String,
    pub received: u64,
    pub confirmed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContactInfo {
    pub uri: String,
    pub node: PublicKey,
    pub routes: Vec<crate::Route>,
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
pub struct SessionInfo {
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

impl Session {
    pub async fn info(&self) -> Result<SessionInfo, Failure> {
        self.send(Request::Info).await
    }

    pub async fn unlink(
        &self,
        node: crate::PeerId,
        identity: std::sync::Arc<dyn crate::IdentityProvider>,
    ) -> Result<(), Failure> {
        self.send_signed(Some(identity), |reply| Request::Unlink(node, reply))
            .await
    }
    pub async fn device_revocation_receipt(
        &self,
        node: crate::PeerId,
    ) -> Result<Option<DeviceRevocationReceipt>, Failure> {
        self.send(|reply| Request::DeviceRevocationReceipt(node, reply))
            .await
    }
    pub async fn provision(
        &self,
        access: Access,
        label: Option<Vec<u8>>,
    ) -> Result<Provisioning, Failure> {
        self.send(|reply| Request::Provision(access, label, reply))
            .await
    }
    pub async fn invite(
        &self,
        identity: PublicKey,
        access: Access,
        terms: crate::InviteTerms,
    ) -> Result<crate::Invitation, Failure> {
        self.send(|reply| Request::Invite(identity, access, terms, reply))
            .await
    }
    pub async fn withdraw_invitation(&self, id: crate::InvitationId) -> Result<(), Failure> {
        self.send(|reply| Request::WithdrawInvitation(id, reply))
            .await
    }
    pub async fn open_invitations(&self) -> Result<Vec<crate::OpenInvitation>, Failure> {
        self.send(Request::OpenInvitations).await
    }
    pub async fn renew(&self) -> Result<(), Failure> {
        self.send(Request::Renew).await
    }
    pub async fn discovered(&self) -> Result<Discovered, Failure> {
        self.send(Request::Discovered).await
    }
    pub async fn confirm_link(&self, node: PublicKey, code: &str) -> Result<(), Failure> {
        self.send(|reply| Request::ConfirmLink(node, code.into(), reply))
            .await
    }
    pub async fn contact(&self) -> Result<String, Failure> {
        self.contact_info().await.map(|contact| contact.uri)
    }
    pub async fn contact_info(&self) -> Result<ContactInfo, Failure> {
        self.send(Request::Contact).await
    }
    pub async fn add_peer(&self, contact: &str) -> Result<PublicKey, Failure> {
        self.send(|reply| Request::AddPeer(contact.into(), reply))
            .await
    }
    pub async fn remove_peer(&self, node: PublicKey) -> Result<(), Failure> {
        self.send(|reply| Request::RemovePeer(node, reply)).await
    }
}
