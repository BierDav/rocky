//! Device roles. Rocky separates authority (issuer or writer grants) from
//! what a replica holds (full or partial); the app combines them into three
//! roles and records each device's role in the replicated `devices` table,
//! so every list holder knows whom to share new lists with.
//!
//! - **Trusted**: an issuer. Invites devices, seals and shares lists.
//! - **Full**: a writer holding every list; trusted and full devices share
//!   each list with it.
//! - **Partial**: a writer holding only the lists shared with it.
use crate::{Configuration, DEVICES, LIST_GROUP, LISTS, TODOS, TodoError, Todos};
use rocky_sdk::{Failure, GrantProfile, params};
use serde_json::json;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Trusted,
    Full,
    Partial,
}
impl Role {
    pub const ALL: [Self; 3] = [Self::Trusted, Self::Full, Self::Partial];
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
    /// Receives every list.
    pub fn holds_all(self) -> bool {
        self != Self::Partial
    }
    fn profile(self) -> GrantProfile {
        match self {
            Self::Trusted => GrantProfile::Issuer,
            Self::Full | Self::Partial => GrantProfile::Writer {
                scopes: [LISTS, TODOS, DEVICES].map(String::from).into(),
            },
        }
    }
}
impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A device as the `devices` table records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub node: String,
    pub role: Role,
    pub name: String,
}

/// An invitation code is the role, a colon and Rocky's invitation URI.
/// Treat it as a secret.
pub fn parse_invitation(code: &str) -> Result<(Role, &str), Failure> {
    let code = code.trim();
    let (role, uri) = code
        .split_once(':')
        .and_then(|(role, uri)| Some((Role::parse(role)?, uri)))
        .ok_or("an invitation starts with trusted:, full: or partial:")?;
    Ok((role, uri))
}

/// Creates the first device, which is trusted.
pub async fn create(configuration: &Configuration, name: &str) -> Result<Todos, Failure> {
    let todos = Todos::new(configuration.create().await?.database);
    todos.register(Role::Trusted, name).await?;
    Ok(todos)
}

/// Joins with an invitation code and records this device's role.
pub async fn join(configuration: &Configuration, code: &str, name: &str) -> Result<Todos, Failure> {
    let (role, uri) = parse_invitation(code)?;
    let todos = Todos::new(configuration.join(uri).await?.database);
    if role == Role::Partial {
        todos
            .call(json!({"type": "partition_role", "role": {"role": "Partial", "full_nodes": [], "custody": 0}}))
            .await?;
    }
    todos.register(role, name).await?;
    Ok(todos)
}

impl Todos {
    /// This device's node key.
    pub async fn node(&self) -> Result<String, Failure> {
        Ok(self.database.session().status().await?.authority.node)
    }
    /// An invitation code for a new device with `role`; only trusted
    /// devices invite.
    pub async fn invite(&self, role: Role) -> Result<String, Failure> {
        let invitation = self.database.session().pairing(role.profile()).await?;
        Ok(format!("{role}:{}", invitation.uri))
    }
    /// Records this device's role and name.
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
    /// Revokes another device and forgets it: its credentials stop
    /// working on every device that learns the revocation, and lists are
    /// no longer shared with it. Only trusted devices revoke.
    pub async fn remove_device(&self, node: &str) -> Result<(), Failure> {
        if node == self.node().await? {
            return Err("a device cannot remove itself; reset it instead".into());
        }
        self.call(json!({"type": "revoke", "node": node})).await?;
        let mut transaction = self.database.transaction().await?;
        transaction
            .execute("DELETE FROM devices WHERE node=?", params![node])
            .await?;
        transaction.commit().await?;
        // Not every removed device was a configured peer.
        let _ = self.database.session().remove_peer(node).await;
        Ok(())
    }
    /// This device's recorded role.
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
                    role: Role::parse(&role).ok_or(TodoError::Reply("a known role"))?,
                    name: row.get_at(2)?,
                })
            })
            .collect()
    }
    /// Devices this device shared `list` with.
    pub async fn shared_with(&self, list: i64) -> Result<BTreeSet<String>, Failure> {
        let status = self
            .call(json!({"type": "partition_status", "group": LIST_GROUP, "values": list_values(list)}))
            .await?;
        Ok(status["recipients"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|recipient| recipient["node"].as_str().map(str::to_owned))
            .collect())
    }
    /// Delivers `list`'s key to `nodes`; they hold it after their next sync.
    pub async fn share(&self, list: i64, nodes: &[String]) -> Result<(), Failure> {
        if nodes.is_empty() {
            return Ok(());
        }
        self.call(json!({"type": "partition_share", "group": LIST_GROUP, "values": list_values(list), "recipients": nodes}))
            .await
            .map(drop)
    }
    /// Shares `list` with every trusted and full device not yet sent it
    /// from here.
    pub async fn share_with_members(&self, list: i64) -> Result<(), Failure> {
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
    /// Shares every list held here with the trusted and full devices; run
    /// when a device joins. Partial devices hold no list's whole group, so
    /// they share nothing.
    pub async fn share_lists_with_members(&self) -> Result<(), Failure> {
        if !self.role().await?.is_some_and(Role::holds_all) {
            return Ok(());
        }
        for list in self.lists().await? {
            self.share_with_members(list.id).await?;
        }
        Ok(())
    }
}

fn list_values(list: i64) -> serde_json::Value {
    json!([{"type": "integer", "value": list.to_string()}])
}
