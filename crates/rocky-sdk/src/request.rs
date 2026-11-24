use rocky_credentials::PublicKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Active,
    LoggedOut { purge_on_close: bool },
}
impl Lifecycle {
    pub(crate) fn is_logged_out(self) -> bool {
        matches!(self, Self::LoggedOut { .. })
    }
    pub(crate) fn purge_on_close(self) -> bool {
        matches!(
            self,
            Self::LoggedOut {
                purge_on_close: true
            }
        )
    }
}

use tokio::sync::oneshot;

type Reply<T> = oneshot::Sender<Result<T, crate::Failure>>;

pub(crate) enum Request {
    PartitionAddress(String, Vec<crate::SqlValue>, Reply<crate::PartitionAddress>),
    PartitionComplete(String, Reply<bool>),
    CreatePartition(crate::PartitionTarget, Reply<crate::PreparedPartition>),
    InstallPartition(
        crate::PartitionTarget,
        Box<rocky_credentials::GroupSecretEnvelope>,
        rocky_credentials::CredentialChain,
        Reply<()>,
    ),
    PartitionStatus(crate::PartitionTarget, Reply<crate::PartitionStatus>),
    RegisterPartition(crate::PartitionTarget, crate::PartitionSelection, Reply<()>),
    HoldPartition(
        crate::PartitionTarget,
        crate::PartitionSelection,
        bool,
        Reply<()>,
    ),
    ReleasePartition(
        crate::PartitionTarget,
        std::collections::BTreeSet<String>,
        Reply<()>,
    ),
    EvictPartition(
        crate::PartitionTarget,
        std::collections::BTreeSet<String>,
        Reply<crate::PartitionEviction>,
    ),

    Identities(Reply<Vec<crate::IdentityInfo>>),
    IdentityLabel(PublicKey, Vec<u8>, Reply<()>),
    HeldChanges(Reply<Vec<crate::HeldChange>>),
    DropHeld(rocky_core::Commitment, Reply<()>),
    Retention(
        String,
        Option<crate::RetentionPolicy>,
        bool,
        Reply<crate::RetentionStatus>,
    ),
    RetentionLimits(
        Option<crate::RetentionLimits>,
        Reply<crate::RetentionLimits>,
    ),
    TrustedStaleAfter(u64, Reply<()>),
    Activity(u64, Reply<crate::ActivityPage>),
    ActivityNext(u64, Reply<crate::ActivityPage>),

    WithIdentity(crate::identities::Loan, Box<Request>),
    Status(Reply<crate::DatabaseStatus>),
    StatusAfter(String, Reply<crate::DatabaseStatus>),
    Sync(Reply<crate::SyncReport>),
    SetSync(crate::SyncMode, Reply<()>),
    Logout(bool, Reply<()>),
    #[cfg(any(test, feature = "test-support"))]
    Tips(crate::testing::Probe),
    InstallTrust(String, Reply<()>),
    Trust(
        rocky_credentials::PublicKey,
        crate::node::Root,
        Reply<crate::TrustGrant>,
    ),
    Invite(Box<crate::Invite>, Reply<crate::Invitation>),
    CancelInvitation(crate::InvitationId, Reply<()>),
    Invitations(Reply<Vec<crate::PendingInvitation>>),
    Renew(Reply<()>),
    LinkRequests(Reply<Vec<crate::LinkRequest>>),
    ApproveLink(rocky_credentials::PublicKey, crate::LinkCode, Reply<()>),
    Contact(Reply<crate::ContactInfo>),
    AddPeer(String, Reply<PublicKey>),
    RemovePeer(rocky_credentials::PublicKey, Reply<()>),
    DeviceRevocationReceipt(
        rocky_credentials::PeerId,
        Reply<Option<crate::DeviceRevocationReceipt>>,
    ),
    Unlink(rocky_credentials::PeerId, Reply<()>),
    RevokeIdentity(
        rocky_credentials::PublicKey,
        rocky_credentials::RevocationMode,
        crate::node::Root,
        bool,
        Reply<crate::RevocationResult>,
    ),
    Info(Reply<crate::DeviceInfo>),
    ReplicaRole(crate::ReplicaRoleConfig, Reply<()>),
    PartitionCustody(
        rocky_core::ResourceScope,
        crate::PartitionCustody,
        Reply<()>,
    ),
    NextPartition(String, Reply<crate::CreatedPartition>),
    PartitionRecipients(
        crate::PartitionTarget,
        Reply<Vec<rocky_credentials::PublicKey>>,
    ),
    GrantPartition(
        rocky_credentials::PublicKey,
        crate::PartitionTarget,
        Reply<crate::PartitionShare>,
    ),
    SharePartition(
        crate::PartitionTarget,
        std::collections::BTreeSet<rocky_credentials::PublicKey>,
        Option<rocky_core::KeyEpoch>,
        Reply<crate::PartitionShare>,
    ),
    IdentityForNode(
        rocky_credentials::PublicKey,
        Reply<Option<rocky_credentials::PublicKey>>,
    ),
    DeviceCredential(Reply<crate::DeviceCredential>),
    SignExtension(Vec<u8>, Reply<rocky_credentials::Signature>),
}
impl Request {
    pub(crate) fn allowed_after_logout(&self) -> bool {
        match self {
            Self::WithIdentity(_, request) => request.allowed_after_logout(),
            Self::Status(..)
            | Self::StatusAfter(..)
            | Self::DeviceRevocationReceipt(..)
            | Self::Activity(..)
            | Self::ActivityNext(..) => true,
            _ => false,
        }
    }
    pub(crate) fn fail(self, error: crate::Failure) {
        match self {
            Self::WithIdentity(_, request) => request.fail(error),
            Self::Identities(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::IdentityLabel(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::HeldChanges(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::DropHeld(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Retention(_, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::RetentionLimits(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::TrustedStaleAfter(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Activity(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::ActivityNext(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::PartitionAddress(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::PartitionComplete(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::CreatePartition(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::InstallPartition(_, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::PartitionStatus(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::RegisterPartition(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::HoldPartition(_, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::ReleasePartition(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::EvictPartition(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Status(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::StatusAfter(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Sync(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::SetSync(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Logout(_, reply) => {
                let _ = reply.send(Err(error));
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::Tips(probe) => probe.fail(error),
            Self::InstallTrust(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Trust(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Invite(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::CancelInvitation(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Invitations(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Renew(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::LinkRequests(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::ApproveLink(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Contact(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::AddPeer(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::RemovePeer(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::DeviceRevocationReceipt(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Unlink(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::RevokeIdentity(_, _, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Info(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::ReplicaRole(_, reply) | Self::PartitionCustody(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::NextPartition(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::PartitionRecipients(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::GrantPartition(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::SharePartition(_, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::IdentityForNode(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::DeviceCredential(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::SignExtension(_, reply) => {
                let _ = reply.send(Err(error));
            }
        }
    }
}
