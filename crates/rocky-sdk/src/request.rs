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
    #[cfg(any(test, feature = "test-support"))]
    PartitionStatus(crate::PartitionTarget, Reply<crate::PartitionStatus>),

    #[cfg(any(test, feature = "test-support"))]
    HeldChanges(Reply<Vec<crate::HeldChange>>),
    #[cfg(any(test, feature = "test-support"))]
    DropHeld(rocky_core::Commitment, Reply<()>),
    #[cfg(any(test, feature = "test-support"))]
    Retention(
        String,
        Option<crate::RetentionPolicy>,
        bool,
        Reply<crate::RetentionStatus>,
    ),
    #[cfg(any(test, feature = "test-support"))]
    RetentionLimits(
        Option<crate::RetentionLimits>,
        Reply<crate::RetentionLimits>,
    ),
    #[cfg(any(test, feature = "test-support"))]
    Activity(u64, Reply<crate::activity::ActivityPage>),
    #[cfg(any(test, feature = "test-support"))]
    ActivityNext(u64, Reply<crate::activity::ActivityPage>),

    Status(Reply<crate::DatabaseStatus>),
    StatusAfter(String, Reply<crate::DatabaseStatus>),
    Sync(Reply<crate::SyncReport>),
    FirstPull(Reply<()>),
    SetSync(crate::SyncMode, Reply<()>),
    Logout(crate::Logout, Reply<()>),
    #[cfg(any(test, feature = "test-support"))]
    Tips(crate::testing::Probe),
    Invite(Box<crate::Invite>, Reply<crate::Invitation>),
    Me(Reply<crate::Me>),
    People(Reply<Vec<crate::Person>>),
    SetAccess(crate::PersonId, crate::Access, Reply<()>),
    GrantPartition(crate::PersonId, crate::Partition, Reply<()>),
    RevokePartition(crate::PersonId, crate::Partition, Reply<()>),
    CreatePartition(crate::Partition, Reply<crate::Partition>),
    Remove(crate::PersonId, Reply<()>),
    MakeAdmin(crate::PersonId, crate::node::Root, Reply<()>),
    RevokeAdmin(
        crate::PersonId,
        crate::Removal,
        crate::node::Root,
        Reply<()>,
    ),
    CancelInvitation(crate::InvitationId, Reply<()>),
    Invitations(Reply<Vec<crate::PendingInvitation>>),
    #[cfg(any(test, feature = "test-support"))]
    Renew(Reply<()>),
    LinkRequests(Reply<Vec<crate::LinkRequest>>),
    ApproveLink(rocky_credentials::PublicKey, crate::LinkCode, Reply<()>),
    Contact(Reply<crate::ContactInfo>),
    AddPeer(String, Reply<PublicKey>),
    RemovePeer(rocky_credentials::PublicKey, Reply<()>),
    Unlink(rocky_credentials::PeerId, Reply<()>),
    NextPartition(String, Reply<crate::CreatedPartition>),
    DeviceCredential(Reply<crate::DeviceCredential>),
    SignExtension(Vec<u8>, Reply<rocky_credentials::Signature>),
}
impl Request {
    pub(crate) fn allowed_after_logout(&self) -> bool {
        match self {
            Self::Status(..) | Self::StatusAfter(..) => true,
            #[cfg(any(test, feature = "test-support"))]
            Self::Activity(..) | Self::ActivityNext(..) => true,
            _ => false,
        }
    }
    pub(crate) fn fail(self, error: crate::Failure) {
        match self {
            #[cfg(any(test, feature = "test-support"))]
            Self::HeldChanges(reply) => {
                let _ = reply.send(Err(error));
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::DropHeld(_, reply) => {
                let _ = reply.send(Err(error));
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::Retention(_, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::RetentionLimits(_, reply) => {
                let _ = reply.send(Err(error));
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::Activity(_, reply) => {
                let _ = reply.send(Err(error));
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::ActivityNext(_, reply) => {
                let _ = reply.send(Err(error));
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::PartitionStatus(_, reply) => {
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
            Self::FirstPull(reply) => {
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
            Self::Invite(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::CancelInvitation(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Invitations(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Me(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::People(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::SetAccess(_, _, reply)
            | Self::GrantPartition(_, _, reply)
            | Self::RevokePartition(_, _, reply)
            | Self::MakeAdmin(_, _, reply)
            | Self::RevokeAdmin(_, _, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::CreatePartition(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Remove(_, reply) => {
                let _ = reply.send(Err(error));
            }
            #[cfg(any(test, feature = "test-support"))]
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
            Self::Unlink(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::NextPartition(_, reply) => {
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
