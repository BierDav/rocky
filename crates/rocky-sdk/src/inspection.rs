use crate::{Failure, Session, request::Request};
use rocky_core::Commitment;
use rocky_credentials::PublicKey;

#[derive(Clone, Debug)]
pub struct DomainInfo {
    pub scope: crate::Scope,
    pub issuer: bool,
    pub access: crate::Access,
    pub collections: Vec<String>,
    pub transport: String,
}
#[derive(Clone, Debug)]
pub struct IdentityInfo {
    pub identity: PublicKey,
    pub kind: crate::IdentityOrigin,
    pub issuer: PublicKey,
    pub origin: Option<PublicKey>,
    pub enrolled: u64,
    pub invitation: Option<crate::InvitationId>,
    pub single_use: bool,
    pub own: bool,
    pub revoked: bool,
    pub access: Option<crate::Access>,
    pub capability: Option<Commitment>,
    pub expires: Option<u64>,
    pub label: Option<Vec<u8>>,
    pub partitions: Vec<crate::RecordedPartitionGrant>,
    pub trusted: Option<TrustedIdentity>,
    pub nodes: Vec<IdentityNode>,
}
#[derive(Clone, Debug)]
pub struct IdentityNode {
    pub node: PublicKey,
    pub linked: u64,
    pub last_seen: Option<u64>,
    pub revoked: bool,
}
#[derive(Clone, Debug)]
pub struct TrustedIdentity {
    pub capability: Commitment,
    pub granted: u64,
    pub live: bool,
}
#[derive(Clone, Debug)]
pub struct RetentionStatus {
    pub policy: Option<crate::RetentionPolicy>,
    pub checkpoint: crate::CheckpointStatus,
}
impl Session {
    pub async fn domain(&self) -> Result<DomainInfo, Failure> {
        self.send(Request::Domain).await
    }
    pub async fn generation(&self) -> Result<crate::GenerationStatus, Failure> {
        self.send(Request::Generation).await
    }
    pub async fn identities(&self) -> Result<Vec<IdentityInfo>, Failure> {
        self.send(Request::Identities).await
    }
    pub async fn set_identity_label(
        &self,
        identity: PublicKey,
        label: Vec<u8>,
    ) -> Result<(), Failure> {
        self.send(|reply| Request::IdentityLabel(identity, label, reply))
            .await
    }
    pub async fn held_changes(&self) -> Result<Vec<crate::HeldChange>, Failure> {
        self.send(Request::HeldChanges).await
    }
    pub async fn drop_held(&self, id: Commitment) -> Result<(), Failure> {
        self.send(|reply| Request::DropHeld(id, reply)).await
    }
    pub async fn retention(&self, collection: &str) -> Result<RetentionStatus, Failure> {
        self.send(|reply| Request::Retention(collection.into(), None, false, reply))
            .await
    }
    pub async fn set_retention(
        &self,
        collection: &str,
        policy: crate::RetentionPolicy,
    ) -> Result<RetentionStatus, Failure> {
        self.send(|reply| Request::Retention(collection.into(), Some(policy), false, reply))
            .await
    }
    pub async fn maintain_retention(&self, collection: &str) -> Result<RetentionStatus, Failure> {
        self.send(|reply| Request::Retention(collection.into(), None, true, reply))
            .await
    }
    pub async fn retention_limits(&self) -> Result<crate::RetentionLimits, Failure> {
        self.send(|reply| Request::RetentionLimits(None, reply))
            .await
    }
    pub async fn set_retention_limits(
        &self,
        limits: crate::RetentionLimits,
    ) -> Result<crate::RetentionLimits, Failure> {
        self.send(|reply| Request::RetentionLimits(Some(limits), reply))
            .await
    }
    pub async fn set_trusted_stale_after(&self, seconds: u64) -> Result<(), Failure> {
        self.send(|reply| Request::TrustedStaleAfter(seconds, reply))
            .await
    }
    pub async fn activity(&self, after: u64) -> Result<crate::ActivityPage, Failure> {
        self.send(|reply| Request::Activity(after, reply)).await
    }
    pub async fn next_activity(&self, after: u64) -> Result<crate::ActivityPage, Failure> {
        self.send(|reply| Request::ActivityNext(after, reply)).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscoveryTopics {
    pub current: [u8; 32],
    pub previous: Option<[u8; 32]>,
}
impl Session {
    pub async fn discovery_topics(&self) -> Result<DiscoveryTopics, Failure> {
        self.send(Request::DiscoveryTopics).await
    }
    pub async fn request_contact(
        &self,
        node: rocky_core::NodeId,
    ) -> Result<Option<PublicKey>, Failure> {
        self.send(|reply| Request::ResolveContact(node, reply))
            .await
    }
}
