use crate::request::Request;
use crate::{Failure, GrantProfile, Session};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Invitation {
    pub uri: String,
    pub expires_at_epoch_seconds: u64,
    pub profile: Option<GrantProfile>,
    pub id: String,
    pub sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JoinRequest {
    pub node: String,
    pub nonce: String,
    pub invitation: String,
    pub name: String,
    pub received_epoch_seconds: u64,
    pub expires_epoch_seconds: u64,
    pub code: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Discovered {
    pub pending: Vec<JoinRequest>,
    pub seen: Vec<SeenDevice>,
    pub links: Vec<LinkRequest>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeenDevice {
    pub node: String,
    pub identity: String,
    pub last_seen: u64,
    pub registry_known: bool,
    pub unknown_identity: bool,
    pub revoked: bool,
    pub unlinked: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LinkRequest {
    pub node: String,
    pub code: String,
    pub received: u64,
    pub confirmed: bool,
}
#[derive(Clone)]
pub struct Admit {
    pub node: String,
    pub code: String,
    pub profile: GrantProfile,
    pub label: Option<Vec<u8>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvitationState {
    pub id: String,
    pub expires_at_epoch_seconds: u64,
    pub open: bool,
    pub enrolled: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContactInfo {
    pub uri: String,
    pub node: String,
    pub routes: Vec<crate::Route>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevocationResult {
    pub statement: String,
    pub revocations: crate::RevocationStatus,
    pub cut_dropped_stages: Vec<DroppedStage>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DroppedStage {
    pub node: String,
    pub sequence: u64,
    pub head: String,
}

#[derive(Clone, Debug)]
pub struct ReadQuery {
    pub sql: String,
    pub values: Vec<crate::SqlValue>,
    pub max_rows: usize,
    pub max_bytes: Option<usize>,
    pub cursor: Option<String>,
}
impl ReadQuery {
    pub fn new(sql: impl Into<String>, max_rows: usize) -> Self {
        Self {
            sql: sql.into(),
            values: Vec::new(),
            max_rows,
            max_bytes: None,
            cursor: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ReadPage {
    pub scope: crate::Scope,
    pub revision: u64,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<crate::SqlValue>>,
    pub next: Option<String>,
}
#[derive(Clone, Debug)]
pub struct SessionRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<crate::SqlValue>>,
    pub changes: u64,
    pub committed: Option<CommitSummary>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitSummary {
    pub handle: rocky_core::Commitment,
    pub frames: usize,
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub principal: crate::credentials::PublicKey,
    pub node: crate::credentials::PublicKey,
    pub revocations: crate::RevocationStatus,
    pub expires: u64,
    pub issuer: bool,
    pub profile: GrantProfile,
    pub transport: String,
    pub readable: usize,
    pub writable: usize,
    pub writer: crate::WriterId,
}

impl Session {
    pub async fn sign_coordination_grant(
        &self,
        tenant: crate::credentials::PublicKey,
        lifetime: u64,
        root: std::sync::Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::credentials::SignedCoordinationGrant, Failure> {
        self.send(|reply| {
            Request::SignCoordinationGrant(tenant, lifetime, crate::node::Root(Some(root)), reply)
        })
        .await
    }

    pub async fn follow_with_identity(
        &self,
        name: &str,
        identity: std::sync::Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::TipStatus, Failure> {
        self.change_tip_with_identity(
            crate::request::TipChange::Follow { name: name.into() },
            identity,
        )
        .await
    }
    pub async fn move_bookmark_with_identity(
        &self,
        name: &str,
        tip: &str,
        identity: std::sync::Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::TipStatus, Failure> {
        self.change_tip_with_identity(
            crate::request::TipChange::Move {
                name: name.into(),
                tip: tip.into(),
            },
            identity,
        )
        .await
    }
    pub async fn follow_when_ready_with_identity(
        &self,
        name: &str,
        identity: std::sync::Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::TipStatus, Failure> {
        self.change_tip_with_identity(
            crate::request::TipChange::FollowWhenReady { name: name.into() },
            identity,
        )
        .await
    }
    async fn change_tip_with_identity(
        &self,
        change: crate::request::TipChange,
        identity: std::sync::Arc<dyn crate::IdentityProvider>,
    ) -> Result<crate::TipStatus, Failure> {
        self.send_signed(Some(identity), |reply| {
            Request::ChangeTip(change, crate::node::Root::default(), reply)
        })
        .await
    }
    pub async fn info(&self) -> Result<SessionInfo, Failure> {
        self.send(Request::Info).await
    }
    pub async fn grant(
        &self,
        device: crate::NodeKeys,
        profile: GrantProfile,
        terms: crate::GrantTerms,
    ) -> Result<crate::Supplied, Failure> {
        self.send(|reply| Request::Grant(device, profile, terms, false, reply))
            .await
    }
    pub async fn delegate(
        &self,
        device: crate::NodeKeys,
        profile: GrantProfile,
        terms: crate::GrantTerms,
    ) -> Result<crate::Supplied, Failure> {
        self.send(|reply| Request::Grant(device, profile, terms, true, reply))
            .await
    }
    pub async fn install_credentials(&self, supplied: crate::Supplied) -> Result<(), Failure> {
        self.send(|reply| Request::Install(Box::new(supplied), reply))
            .await
    }

    pub async fn execute(
        &self,
        sql: &str,
        values: Vec<crate::SqlValue>,
    ) -> Result<SessionRows, Failure> {
        self.send(|reply| Request::Execute(sql.into(), values, None, reply))
            .await
    }
    pub async fn commit(&self, handle: rocky_core::Commitment) -> Result<SessionRows, Failure> {
        self.send(|reply| Request::Execute("COMMIT".into(), Vec::new(), Some(handle), reply))
            .await
    }
    pub async fn next_id(&self, table: &str) -> Result<rocky_core::RowIdentity, Failure> {
        self.send(|reply| Request::NextId(table.into(), None, reply))
            .await
    }
    pub async fn next_partition_id(
        &self,
        table: &str,
        values: Vec<crate::SqlValue>,
    ) -> Result<rocky_core::RowIdentity, Failure> {
        self.send(|reply| Request::NextId(table.into(), Some(values), reply))
            .await
    }
    pub async fn mutation_committed(
        &self,
        handle: rocky_core::Commitment,
    ) -> Result<bool, Failure> {
        self.send(|reply| Request::MutationCommitted(handle, reply))
            .await
    }
    pub async fn read(&self, query: ReadQuery) -> Result<ReadPage, Failure> {
        self.send(|reply| Request::Read(query, reply)).await
    }
    pub async fn subscribe(&self, query: ReadQuery) -> Result<u64, Failure> {
        self.send(|reply| Request::Subscribe(query, reply)).await
    }
    pub async fn next(&self, subscription: u64) -> Result<ReadPage, Failure> {
        self.send(|reply| Request::Next(subscription, reply)).await
    }
    pub async fn unsubscribe(&self, subscription: u64) -> Result<(), Failure> {
        self.send(|reply| Request::Unsubscribe(subscription, reply))
            .await
    }

    pub async fn unlink(
        &self,
        node: crate::PeerId,
        identity: std::sync::Arc<dyn crate::IdentityProvider>,
    ) -> Result<(), Failure> {
        self.send(|reply| Request::Unlink(node, identity, reply))
            .await
    }
    pub async fn invitation(&self) -> Result<Invitation, Failure> {
        self.invite(None, None).await
    }
    pub async fn invitation_with(&self, profile: GrantProfile) -> Result<Invitation, Failure> {
        self.invite(Some(profile), None).await
    }
    pub async fn invite(
        &self,
        profile: Option<GrantProfile>,
        label: Option<Vec<u8>>,
    ) -> Result<Invitation, Failure> {
        self.send(|reply| Request::Invitation(profile, label, reply))
            .await
    }
    pub async fn invitation_state(&self, uri: &str) -> Result<InvitationState, Failure> {
        self.send(|reply| Request::InvitationState(uri.into(), reply))
            .await
    }
    pub async fn renew(&self) -> Result<(), Failure> {
        self.send(Request::Renew).await
    }
    pub async fn discovered(&self) -> Result<Discovered, Failure> {
        self.send(Request::Discovered).await
    }
    pub async fn confirm_link(&self, node: &str, code: &str) -> Result<(), Failure> {
        let node = node.parse()?;
        self.send(|reply| Request::ConfirmLink(node, code.into(), reply))
            .await
    }
    pub async fn admit(&self, admit: Admit) -> Result<(), Failure> {
        self.send(|reply| Request::Admit(admit, reply)).await
    }
    pub async fn refuse(&self, node: &str) -> Result<(), Failure> {
        let node = node.parse()?;
        self.send(|reply| Request::Refuse(node, reply)).await
    }
    pub async fn contact(&self) -> Result<String, Failure> {
        self.contact_info().await.map(|contact| contact.uri)
    }
    pub async fn contact_info(&self) -> Result<ContactInfo, Failure> {
        self.send(Request::Contact).await
    }
    pub async fn add_peer(&self, contact: &str) -> Result<String, Failure> {
        self.send(|reply| Request::AddPeer(contact.into(), reply))
            .await
    }
    pub async fn remove_peer(&self, node: &str) -> Result<(), Failure> {
        let node = node.parse()?;
        self.send(|reply| Request::RemovePeer(node, reply)).await
    }
    pub async fn set_enrollment_upstream(&self, contact: Option<&str>) -> Result<(), Failure> {
        self.send(|reply| Request::EnrollmentUpstream(contact.map(str::to_owned), reply))
            .await
    }
}
