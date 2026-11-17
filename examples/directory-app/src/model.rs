use rocky_core::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const QUOTA: usize = 1024;
#[derive(Clone)]
pub struct DirectoryApp {
    pub publisher: Commitment,
    pub domain: DomainId,
    pub bootstrap_registrar: PrincipalId,
    pub initial_ownership: Vec<Ownership>,
}
#[derive(Clone)]
pub struct Policy {
    pub scope: Scope,
    pub caller: PrincipalId,
    pub reference: Commitment,
    pub permitted_callers: Vec<PrincipalId>,
}
#[derive(Debug)]
pub struct Invalid;
impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "directory transition denied or malformed")
    }
}
impl std::error::Error for Invalid {}
pub fn principal(key: Commitment) -> PrincipalId {
    let hash = commitment("rocky.secure-principal.v1", &key);
    let mut id = [0; 16];
    id.copy_from_slice(&hash[..16]);
    PrincipalId(id)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub key: Commitment,
    pub principal: PrincipalId,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Deployment {
    pub key: Commitment,
    pub owner: PrincipalId,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    pub key: Commitment,
    pub owner: PrincipalId,
    pub deployment: Option<Commitment>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ownership {
    pub scope: Scope,
    pub owner: PrincipalId,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum DirectoryRight {
    RegisterDeployment,
    RegisterNode,
    Delegate,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Delegation {
    pub id: Commitment,
    pub owner: PrincipalId,
    pub subject: PrincipalId,
    pub rights: BTreeSet<DirectoryRight>,
    pub depth: u8,
    pub not_before: u64,
    pub expires: u64,
    pub parent: Option<Commitment>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Disposition {
    Transfer(PrincipalId),
    Retire,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifacts {
    pub owner: PrincipalId,
    pub app_grants: Vec<Commitment>,
    pub keys: Vec<ResourceScope>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Deletion {
    pub user: PrincipalId,
    pub revoked_nodes: Vec<Commitment>,
    pub revoked_grants: Vec<Commitment>,
    pub rotate_keys: Vec<ResourceScope>,
    pub disposition: Vec<(Scope, Disposition)>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Directory {
    pub users: Vec<User>,
    pub deployments: Vec<Deployment>,
    pub nodes: Vec<Node>,
    pub delegations: Vec<Delegation>,
    pub ownership: Vec<Ownership>,
    pub artifacts: Vec<Artifacts>,
    pub deleted_keys: BTreeSet<Commitment>,
    pub deletions: Vec<Deletion>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Transition {
    RegisterUser {
        key: Commitment,
    },
    RegisterDeployment {
        key: Commitment,
        owner: PrincipalId,
        time: u64,
    },
    RegisterNode {
        key: Commitment,
        owner: PrincipalId,
        deployment: Option<Commitment>,
        time: u64,
    },
    Delegate {
        grant: Delegation,
        time: u64,
    },
    RevokeDelegation {
        id: Commitment,
    },
    TrackArtifacts(Artifacts),
    DeleteUser {
        disposition: Vec<(Scope, Disposition)>,
    },
}
