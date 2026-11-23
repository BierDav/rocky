use crate::{Configuration, DEVICES, LIST_GROUP, TodoError, Todos};
use rocky_sdk::{
    Access, Failure, PartitionTarget, ReplicaRole, ReplicaRoleConfig, RoleName, RowIdentity, params,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Trusted,
    Full,
    Partial,
}
pub const PARTIAL: RoleName = RoleName::from_static("partial");

impl Role {
    pub const ALL: [Self; 3] = [Self::Trusted, Self::Full, Self::Partial];
    pub const INVITABLE: [Self; 2] = [Self::Full, Self::Partial];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Full => "full",
            Self::Partial => "partial",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == text)
    }
    pub fn holds_all(self) -> bool {
        self != Self::Partial
    }
    fn access(self) -> Result<Access, TodoError> {
        match self {
            Self::Trusted => Err(TodoError::NotInvitable),
            Self::Full => Ok(Access::Full),
            Self::Partial => Ok(Access::Role(PARTIAL)),
        }
    }
}
impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub node: String,
    pub role: Role,
    pub name: String,
}

pub fn parse_invitation(code: &str) -> Result<(Role, &str), Failure> {
    let code = code.trim();
    let (role, uri) = code
        .split_once(':')
        .and_then(|(role, uri)| Some((Role::parse(role)?, uri)))
        .filter(|(role, _)| Role::INVITABLE.contains(role))
        .ok_or("an invitation starts with full: or partial:")?;
    Ok((role, uri))
}

pub async fn create(configuration: &Configuration, name: &str) -> Result<Todos, Failure> {
    let todos = Todos::new(configuration.create().await?.database);
    todos.register(Role::Trusted, name).await?;
    Ok(todos)
}

pub async fn join(configuration: &Configuration, code: &str, name: &str) -> Result<Todos, Failure> {
    let (role, uri) = parse_invitation(code)?;
    let todos = Todos::new(configuration.join(uri).await?);
    if role == Role::Partial {
        todos
            .database
            .set_replica_role(ReplicaRoleConfig {
                role: ReplicaRole::Partial,
            })
            .await?;
    }
    todos.register(role, name).await?;
    Ok(todos)
}

impl Todos {
    pub async fn node(&self) -> Result<String, Failure> {
        Ok(self.database.status().await?.authority.node)
    }
    pub async fn invite(&self, role: Role) -> Result<String, Failure> {
        let provisioning = self.database.provision(role.access()?, None).await?;
        Ok(format!("{role}:{}", provisioning.uri))
    }
    pub async fn register(&self, role: Role, name: &str) -> Result<(), Failure> {
        let node = self.node().await?;
        let name = crate::title(name)?;
        let mut transaction = self.database.transaction().await?;
        let known = transaction
            .query("SELECT 1 FROM devices WHERE node=?", params![node.as_str()])
            .await?
            .collect()
            .await?;
        if known.is_empty() {
            let id = transaction.next_id(DEVICES, params![]).await?;
            transaction
                .execute(
                    "INSERT INTO devices(id,role,name,node) VALUES(?,?,?,?)",
                    params![id, role.as_str(), name, node],
                )
                .await?;
        } else {
            transaction
                .execute(
                    "UPDATE devices SET role=?,name=? WHERE node=?",
                    params![role.as_str(), name, node],
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }
    pub async fn remove_device(&self, node: &str) -> Result<(), Failure> {
        if node == self.node().await? {
            return Err("a device cannot remove itself; reset it instead".into());
        }
        let node_key = node.parse()?;
        let identity = self
            .database
            .identity_for_node(node_key)
            .await?
            .ok_or("the node is not linked to a recorded identity")?;
        self.database.revoke_identity(identity).await?;
        let mut transaction = self.database.transaction().await?;
        transaction
            .execute("DELETE FROM devices WHERE node=?", params![node])
            .await?;
        transaction.commit().await?;
        let _ = self.database.remove_peer(node_key).await;
        Ok(())
    }
    pub async fn role(&self) -> Result<Option<Role>, Failure> {
        let node = self.node().await?;
        Ok(self
            .members()
            .await?
            .into_iter()
            .find(|member| member.node == node)
            .map(|member| member.role))
    }
    pub async fn members(&self) -> Result<Vec<Member>, Failure> {
        let rows = self
            .database
            .query(
                "SELECT node,role,name FROM devices ORDER BY name,node",
                params![],
            )
            .await?
            .collect()
            .await?;
        rows.iter()
            .map(|row| {
                let role: String = row.get_at(1)?;
                Ok(Member {
                    node: row.get_at(0)?,
                    role: Role::parse(&role).ok_or(TodoError::UnknownRole)?,
                    name: row.get_at(2)?,
                })
            })
            .collect()
    }
    pub async fn shared_with(&self, list: RowIdentity) -> Result<BTreeSet<String>, Failure> {
        Ok(self
            .database
            .partition_recipients(list_target(list))
            .await?
            .iter()
            .map(|node| node.to_hex())
            .collect())
    }
    pub async fn share(&self, list: RowIdentity, nodes: &[String]) -> Result<(), Failure> {
        if nodes.is_empty() {
            return Ok(());
        }
        let partial: BTreeSet<String> = self
            .members()
            .await?
            .into_iter()
            .filter(|member| member.role == Role::Partial)
            .map(|member| member.node)
            .collect();
        let mut full = BTreeSet::new();
        for node in nodes {
            let key = node.parse()?;
            if partial.contains(node) {
                let identity = self
                    .database
                    .identity_for_node(key)
                    .await?
                    .ok_or("the node is not linked to a recorded identity")?;
                self.database
                    .grant_partition(identity, list_target(list))
                    .await?;
            } else {
                full.insert(key);
            }
        }
        if !full.is_empty() {
            self.database
                .share_partition(list_target(list), full, None)
                .await?;
        }
        Ok(())
    }

    pub async fn share_with_members(&self, list: RowIdentity) -> Result<(), Failure> {
        let own = self.node().await?;
        let shared = self.shared_with(list).await?;
        let missing: Vec<String> = self
            .members()
            .await?
            .into_iter()
            .filter(|member| {
                member.role.holds_all() && member.node != own && !shared.contains(&member.node)
            })
            .map(|member| member.node)
            .collect();
        self.share(list, &missing).await
    }
    pub async fn share_lists_with_members(&self) -> Result<(), Failure> {
        if self.role().await? != Some(Role::Trusted) {
            return Ok(());
        }
        for list in self.lists().await? {
            self.share_with_members(list.id).await?;
        }
        Ok(())
    }
}

fn list_target(list: RowIdentity) -> PartitionTarget {
    PartitionTarget::Group {
        group: LIST_GROUP.into(),
        values: vec![list.sql()],
    }
}
