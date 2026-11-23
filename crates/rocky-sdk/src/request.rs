use rocky_credentials::PublicKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Domain,
    Generation,
    Identities,
    IdentityLabel,
    HeldChanges,
    DropHeld,
    Coordinated,
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
    Tips,
    Claims,
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
    CoordinationGrant,
    PartitionRole,
    PartitionNext,
    PartitionShare,
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

pub(crate) enum TipChange {
    Status,
    Seal { close: bool },
    Recover,
    Pull { seal: String },
    GenesisMinimum { minimum_in_sync: u32 },
    DeclareCoordinated,
    Create { name: String, tip: Option<String> },
    Move { name: String, tip: String },
    Delete { name: String },
    FollowWhenReady { name: String },
    Follow { name: String },
    Drop { tip: String },
    InstallTrust { grant: String },
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
    Coordinated(Reply<crate::CoordinatedLogStatus>),
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
    ChangeTip(TipChange, crate::node::Root, Reply<crate::TipStatus>),
    InspectTips(Reply<crate::TipInspection>),
    KeptSeals(Reply<crate::KeptSeals>),
    SetKeptSeals(crate::KeptSeals, Reply<crate::KeptSeals>),
    Compaction(Reply<crate::TipCompaction>),
    Compact(usize, Reply<crate::TipCompaction>),
    OpenTip(rocky_core::Commitment, u32, Reply<String>),
    Trust(
        rocky_credentials::PublicKey,
        crate::node::Root,
        Reply<crate::TrustGrant>,
    ),
    GrantClaims(
        rocky_credentials::PublicKey,
        Option<std::collections::BTreeSet<Vec<u8>>>,
        u8,
        Reply<crate::ClaimGrant>,
    ),
    InstallClaims(crate::ClaimGrant, Reply<()>),
    HeldClaims(Reply<Option<crate::ClaimGrant>>),
    RevokeClaims(rocky_credentials::PublicKey, Reply<()>),
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
    SignCoordinationGrant(
        rocky_credentials::PublicKey,
        u64,
        crate::node::Root,
        Reply<rocky_credentials::SignedCoordinationGrant>,
    ),
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
            Self::Coordinated(..) => Kind::Coordinated,
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
            Self::ChangeTip(..) => Kind::Tips,
            Self::InspectTips(..) => Kind::Tips,
            Self::KeptSeals(..) => Kind::Tips,
            Self::SetKeptSeals(..) => Kind::Tips,
            Self::Compaction(..) => Kind::Tips,
            Self::Compact(..) => Kind::Tips,
            Self::OpenTip(..) => Kind::Tips,
            Self::Trust(..) => Kind::Tips,
            Self::GrantClaims(..) => Kind::Claims,
            Self::InstallClaims(..) => Kind::Claims,
            Self::HeldClaims(..) => Kind::Claims,
            Self::RevokeClaims(..) => Kind::Claims,
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
            Self::SignCoordinationGrant(..) => Kind::CoordinationGrant,
            Self::ReplicaRole(..) | Self::PartitionCustody(..) => Kind::PartitionRole,
            Self::NextPartition(..) => Kind::PartitionNext,
            Self::PartitionRecipients(..) => Kind::PartitionStatus,
            Self::SharePartition(..) => Kind::PartitionShare,
            Self::IdentityForNode(..) => Kind::Identities,
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
            Self::Coordinated(reply) => {
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
            Self::ChangeTip(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::InspectTips(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::KeptSeals(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::SetKeptSeals(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Compaction(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Compact(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::OpenTip(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Trust(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::GrantClaims(_, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::InstallClaims(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::HeldClaims(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::RevokeClaims(_, reply) => {
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
            Self::SignCoordinationGrant(_, _, _, reply) => {
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
        }
    }
}
