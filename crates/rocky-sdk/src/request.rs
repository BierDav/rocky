use rocky_credentials::PublicKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Domain,
    Generation,
    Identities,
    IdentityLabel,
    HeldChanges,
    DropHeld,
    Retention,
    RetentionLimits,
    TrustedStaleAfter,
    Activity,
    ActivityNext,
    PartitionGrant,
    PartitionAddress,
    PartitionComplete,
    PartitionCreate,
    PartitionInstall,
    PartitionStatus,
    PartitionRegister,
    PartitionSubscribe,
    PartitionUnsubscribe,
    PartitionEvict,
    DiscoveryTopics,
    ContactRequest,
    Network,
    Peers,
    Status,
    StatusNext,
    Sync,
    NetworkingControl,
    SyncStart,
    NetworkForeground,
    NetworkBackground,
    Logout,
    #[cfg(any(test, feature = "test-support"))]
    Tips,
    Trust,
    Provision,
    Invitations,
    Renew,
    Discovered,
    ConfirmLink,
    Contact,
    PeerAdd,
    PeerRemove,
    Unlink,
    Revoke,
    Session,
    Grant,
    Install,
    PartitionRole,
    PartitionNext,
    PartitionShare,
    Extension,
}
impl Kind {
    pub(crate) fn waits(self) -> bool {
        matches!(self, Self::StatusNext | Self::ActivityNext)
    }
    pub(crate) fn allowed_after_logout(self) -> bool {
        matches!(
            self,
            Self::Domain
                | Self::Status
                | Self::Activity
                | Self::ActivityNext
                | Self::StatusNext
                | Self::Network
                | Self::Peers
        )
    }
}

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
    DiscoveryTopics(Reply<crate::DiscoveryTopics>),
    ResolveContact(rocky_core::NodeId, Reply<Option<PublicKey>>),
    IssuePartitions(
        crate::NodeKeys,
        Vec<rocky_credentials::PartitionGrant>,
        u8,
        bool,
        Reply<crate::Supplied>,
    ),
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

    Domain(Reply<crate::DomainInfo>),
    Generation(Reply<crate::GenerationStatus>),
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
    Network(Reply<crate::NetworkStatus>),
    Peers(Reply<Vec<crate::Peer>>),
    Status(Reply<crate::DatabaseStatus>),
    StatusAfter(String, Reply<crate::DatabaseStatus>),
    Sync(Reply<crate::ReconciliationReport>),
    NetworkingEnabled(bool, Reply<()>),
    AutomaticSync(bool, Reply<()>),
    Foreground(Reply<()>),
    Background(Reply<()>),
    Logout(bool, Reply<()>),
    #[cfg(any(test, feature = "test-support"))]
    Tips(crate::testing::Probe),
    InstallTrust(String, Reply<()>),
    Trust(
        rocky_credentials::PublicKey,
        crate::node::Root,
        Reply<crate::TrustGrant>,
    ),
    Provision(
        crate::GrantProfile,
        Option<Vec<u8>>,
        Reply<crate::Provisioning>,
    ),
    Invite(
        rocky_credentials::PublicKey,
        crate::GrantProfile,
        crate::InviteTerms,
        Reply<crate::Invitation>,
    ),
    WithdrawInvitation(crate::InvitationId, Reply<()>),
    OpenInvitations(Reply<Vec<crate::OpenInvitation>>),
    Renew(Reply<()>),
    Discovered(Reply<crate::Discovered>),
    ConfirmLink(rocky_credentials::PublicKey, String, Reply<()>),
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
    Info(Reply<crate::SessionInfo>),
    Grant(
        crate::NodeKeys,
        crate::GrantProfile,
        crate::GrantTerms,
        bool,
        Reply<crate::Supplied>,
    ),
    Install(Box<crate::Supplied>, Reply<()>),
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
    pub(crate) fn kind(&self) -> Kind {
        match self {
            Self::WithIdentity(_, request) => request.kind(),
            Self::Domain(..) => Kind::Domain,
            Self::Generation(..) => Kind::Generation,
            Self::Identities(..) => Kind::Identities,
            Self::IdentityLabel(..) => Kind::IdentityLabel,
            Self::HeldChanges(..) => Kind::HeldChanges,
            Self::DropHeld(..) => Kind::DropHeld,
            Self::Retention(..) => Kind::Retention,
            Self::RetentionLimits(..) => Kind::RetentionLimits,
            Self::TrustedStaleAfter(..) => Kind::TrustedStaleAfter,
            Self::Activity(..) => Kind::Activity,
            Self::ActivityNext(..) => Kind::ActivityNext,
            Self::IssuePartitions(..) => Kind::PartitionGrant,
            Self::PartitionAddress(..) => Kind::PartitionAddress,
            Self::PartitionComplete(..) => Kind::PartitionComplete,
            Self::CreatePartition(..) => Kind::PartitionCreate,
            Self::InstallPartition(..) => Kind::PartitionInstall,
            Self::PartitionStatus(..) => Kind::PartitionStatus,
            Self::RegisterPartition(..) => Kind::PartitionRegister,
            Self::HoldPartition(..) => Kind::PartitionSubscribe,
            Self::ReleasePartition(..) => Kind::PartitionUnsubscribe,
            Self::EvictPartition(..) => Kind::PartitionEvict,
            Self::DiscoveryTopics(..) => Kind::DiscoveryTopics,
            Self::ResolveContact(..) => Kind::ContactRequest,
            Self::Network(..) => Kind::Network,
            Self::Peers(..) => Kind::Peers,
            Self::Status(..) => Kind::Status,
            Self::StatusAfter(..) => Kind::StatusNext,
            Self::Sync(..) => Kind::Sync,
            Self::NetworkingEnabled(..) => Kind::NetworkingControl,
            Self::AutomaticSync(..) => Kind::SyncStart,
            Self::Foreground(..) => Kind::NetworkForeground,
            Self::Background(..) => Kind::NetworkBackground,
            Self::Logout(..) => Kind::Logout,
            #[cfg(any(test, feature = "test-support"))]
            Self::Tips(..) => Kind::Tips,
            Self::Trust(..) | Self::InstallTrust(..) => Kind::Trust,
            Self::Provision(..) => Kind::Provision,
            Self::Invite(..) | Self::WithdrawInvitation(..) | Self::OpenInvitations(..) => {
                Kind::Invitations
            }
            Self::Renew(..) => Kind::Renew,
            Self::Discovered(..) => Kind::Discovered,
            Self::ConfirmLink(..) => Kind::ConfirmLink,
            Self::Contact(..) => Kind::Contact,
            Self::AddPeer(..) => Kind::PeerAdd,
            Self::RemovePeer(..) => Kind::PeerRemove,
            Self::DeviceRevocationReceipt(..) => Kind::Status,
            Self::Unlink(..) => Kind::Unlink,
            Self::RevokeIdentity(..) => Kind::Revoke,
            Self::Info(..) => Kind::Session,
            Self::Grant(..) => Kind::Grant,
            Self::Install(..) => Kind::Install,
            Self::ReplicaRole(..) | Self::PartitionCustody(..) => Kind::PartitionRole,
            Self::NextPartition(..) => Kind::PartitionNext,
            Self::PartitionRecipients(..) => Kind::PartitionStatus,
            Self::SharePartition(..) => Kind::PartitionShare,
            Self::IdentityForNode(..) => Kind::Identities,
            Self::DeviceCredential(..) | Self::SignExtension(..) => Kind::Extension,
        }
    }
    pub(crate) fn waits(&self) -> bool {
        self.kind().waits()
    }
    pub(crate) fn fail(self, error: crate::Failure) {
        match self {
            Self::WithIdentity(_, request) => request.fail(error),
            Self::Domain(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Generation(reply) => {
                let _ = reply.send(Err(error));
            }
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
            Self::IssuePartitions(_, _, _, _, reply) => {
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
            Self::DiscoveryTopics(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::ResolveContact(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Network(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Peers(reply) => {
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
            Self::NetworkingEnabled(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::AutomaticSync(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Foreground(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Background(reply) => {
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
            Self::Provision(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Invite(_, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::WithdrawInvitation(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::OpenInvitations(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Renew(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Discovered(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::ConfirmLink(_, _, reply) => {
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
            Self::Grant(_, _, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Install(_, reply) => {
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
