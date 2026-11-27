use crate::{Configuration, Failure, LIST_GROUP, TodoError, Todos};
use rocky_sdk::{Access, IdentityKey, RoleName, RowIdentity, params};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Admin,
    Full,
    Partial,
}
pub const PARTIAL: RoleName = RoleName::from_static("partial");

impl Role {
    pub const ALL: [Self; 3] = [Self::Admin, Self::Full, Self::Partial];
    pub const INVITABLE: [Self; 2] = [Self::Full, Self::Partial];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Full => "full",
            Self::Partial => "partial",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == text)
    }
    fn access(self) -> Result<Access, TodoError> {
        match self {
            Self::Admin => Err(TodoError::NotInvitable),
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
    todos.register(Role::Admin, name).await?;
    Ok(todos)
}

pub async fn join(configuration: &Configuration, code: &str, name: &str) -> Result<Todos, Failure> {
    let (role, uri) = parse_invitation(code)?;
    let person = Arc::new(IdentityKey::generate());
    let todos = Todos::new(
        configuration
            .clone()
            .identity(person)
            .join(&uri.parse()?)
            .await?,
    );
    todos.register(role, name).await?;
    Ok(todos)
}

impl Todos {
    pub async fn node(&self) -> Result<String, Failure> {
        Ok(self.database.me().await?.device.to_string())
    }
    pub async fn invite(&self, role: Role) -> Result<String, Failure> {
        let invitation = self
            .database
            .invite(rocky_sdk::Invite::new(role.access()?))
            .await?;
        Ok(format!("{role}:{}", invitation.code))
    }
    pub async fn register(&self, role: Role, name: &str) -> Result<(), Failure> {
        let node = self.node().await?;
        let name = crate::title(name)?;
        self.database
            .write(async |transaction| {
                transaction
                    .execute(
                        "INSERT INTO devices(node,role,name) VALUES(?,?,?) \
                         ON CONFLICT(node) DO UPDATE SET role=excluded.role,name=excluded.name",
                        params![node, role.as_str(), name],
                    )
                    .await
            })
            .await?;
        Ok(())
    }
    pub async fn remove_device(&self, node: &str) -> Result<(), Failure> {
        if node == self.node().await? {
            return Err("a device cannot remove itself; reset it instead".into());
        }
        let node_key = node.parse()?;
        let person = self.person_of(node_key).await?;
        self.database.remove(person).await?;
        self.database
            .write(async |transaction| {
                transaction
                    .execute("DELETE FROM devices WHERE node=?", params![node])
                    .await
            })
            .await?;
        let _ = self
            .database
            .remove_peer(rocky_sdk::DeviceId::new(node_key))
            .await;
        Ok(())
    }
    pub async fn role(&self) -> Result<Role, Failure> {
        let me = self.database.me().await?;
        Ok(if me.admin {
            Role::Admin
        } else if me.access == Access::Role(PARTIAL) {
            Role::Partial
        } else {
            Role::Full
        })
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
    async fn person_of(&self, node: rocky_sdk::PublicKey) -> Result<rocky_sdk::PersonId, Failure> {
        let device = rocky_sdk::DeviceId::new(node);
        self.database
            .people()
            .await?
            .into_iter()
            .find(|person| person.devices.iter().any(|held| held.id == device))
            .map(|person| person.id)
            .ok_or_else(|| "the node is not linked to a recorded person".into())
    }
    pub async fn shared_with(&self, list: RowIdentity) -> Result<BTreeSet<String>, Failure> {
        let partition = list_partition(list);
        Ok(self
            .database
            .people()
            .await?
            .into_iter()
            .filter(|person| person.partitions.contains(&partition))
            .flat_map(|person| person.devices)
            .map(|device| device.id.key().to_hex())
            .collect())
    }
    pub async fn share(&self, list: RowIdentity, nodes: &[String]) -> Result<(), Failure> {
        for node in nodes {
            let person = self.person_of(node.parse()?).await?;
            self.database
                .grant_partition(person, &list_partition(list))
                .await?;
        }
        Ok(())
    }
}

fn list_partition(list: RowIdentity) -> rocky_sdk::Partition {
    rocky_sdk::Partition::new(LIST_GROUP, vec![list.sql()])
}
