use crate::request::Request;
use crate::{Access, Database, PublicKey};
use std::sync::Arc;

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

    pub async fn unlink(
        &self,
        node: crate::PeerId,
        identity: Arc<dyn crate::IdentityProvider>,
    ) -> Result<(), crate::Error> {
        self.send_signed(Some(identity), |reply| Request::Unlink(node, reply))
            .await
    }
    pub async fn device_revocation_receipt(
        &self,
        node: crate::PeerId,
    ) -> Result<Option<DeviceRevocationReceipt>, crate::Error> {
        self.send(|reply| Request::DeviceRevocationReceipt(node, reply))
            .await
    }
    pub async fn provision(
        &self,
        access: Access,
        label: Option<Vec<u8>>,
    ) -> Result<Provisioning, crate::Error> {
        self.send(|reply| Request::Provision(access, label, reply))
            .await
    }
    pub async fn invite(
        &self,
        identity: PublicKey,
        access: Access,
        terms: crate::InviteTerms,
    ) -> Result<crate::Invitation, crate::Error> {
        self.send(|reply| Request::Invite(identity, access, terms, reply))
            .await
    }
    pub async fn withdraw_invitation(&self, id: crate::InvitationId) -> Result<(), crate::Error> {
        self.send(|reply| Request::WithdrawInvitation(id, reply))
            .await
    }
    pub async fn open_invitations(&self) -> Result<Vec<crate::OpenInvitation>, crate::Error> {
        self.send(Request::OpenInvitations).await
    }
    pub async fn renew(&self) -> Result<(), crate::Error> {
        self.send(Request::Renew).await
    }
    pub async fn discovered(&self) -> Result<Discovered, crate::Error> {
        self.send(Request::Discovered).await
    }
    pub async fn confirm_link(&self, node: PublicKey, code: &str) -> Result<(), crate::Error> {
        self.send(|reply| Request::ConfirmLink(node, code.into(), reply))
            .await
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
