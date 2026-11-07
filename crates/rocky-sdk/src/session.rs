use crate::{Failure, GrantProfile, Session};
use serde_json::json;

/// An enrollment invitation. Treat its URI as a secret.
#[derive(Clone)]
pub struct Invitation {
    pub uri: String,
    pub expires_at_epoch_seconds: u64,
    /// What the invited device is admitted as on arrival; `None` when an
    /// issuer admits each requesting device.
    pub profile: Option<GrantProfile>,
    pub id: String,
}
/// A device that answered an approval invitation and waits for `admit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
    /// The requesting node's key, hexadecimal.
    pub node: String,
    pub nonce: String,
    /// The digest of the invitation this request answered.
    pub invitation: String,
    /// What the device calls itself; nobody verified it.
    pub name: String,
    pub received_epoch_seconds: u64,
    /// When the request ends with its invitation.
    pub expires_epoch_seconds: u64,
    /// The code the requesting device shows; `admit` needs it.
    pub code: String,
}
/// The devices this node knows of without having enrolled them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovered {
    /// Join requests waiting on this issuer node, oldest first; empty on
    /// any other node.
    pub pending: Vec<JoinRequest>,
}
/// An issuer's admission of a pending join request.
#[derive(Clone)]
pub struct Admit {
    /// The requesting node's key, hexadecimal.
    pub node: String,
    /// The code the requesting device shows, as the user confirmed it.
    pub code: String,
    pub profile: GrantProfile,
    /// The registry label of the admitted identity.
    pub label: Option<Vec<u8>>,
}
impl Session {
    /// Unlinks one node of this identity. The signer is used only for this call.
    /// Issuers rotate the reader scopes exposed to that node as they learn the unlink.
    pub async fn unlink(
        &self,
        node: &str,
        identity: std::sync::Arc<dyn crate::IdentityProvider>,
    ) -> Result<(), Failure> {
        self.request_with_identity(json!({"type":"unlink", "node":node}), identity)
            .await?;
        Ok(())
    }
    /// Creates an approval invitation, replacing the issuer's previous one
    /// and ending its requests. A device answering it waits until an issuer
    /// admits it with [`Self::admit`].
    pub async fn invitation(&self) -> Result<Invitation, Failure> {
        self.invite(json!({"type":"invitation"})).await
    }
    /// Creates an invitation whose device is admitted on arrival as
    /// `profile`, replacing the issuer's previous invitation. Grant scopes
    /// name schema tables; there is deliberately no default grant.
    pub async fn invitation_with(&self, profile: GrantProfile) -> Result<Invitation, Failure> {
        self.invite(json!({"type":"invitation", "profile": profile}))
            .await
    }
    async fn invite(&self, request: serde_json::Value) -> Result<Invitation, Failure> {
        let value = self.request(request).await?;
        Ok(Invitation {
            uri: value["uri"]
                .as_str()
                .ok_or("missing invitation URI")?
                .into(),
            expires_at_epoch_seconds: value["expires"]
                .as_str()
                .ok_or("missing expiration")?
                .parse()?,
            profile: serde_json::from_value(value["profile"].clone())?,
            id: value["id"].as_str().ok_or("missing invitation ID")?.into(),
        })
    }
    /// The devices this node knows of without having enrolled them.
    pub async fn discovered(&self) -> Result<Discovered, Failure> {
        let value = self.request(json!({"type":"discovered"})).await?;
        let pending = value["pending"]
            .as_array()
            .ok_or("missing pending join requests")?
            .iter()
            .map(|request| {
                let text = |field: &str| {
                    request[field]
                        .as_str()
                        .map(String::from)
                        .ok_or_else(|| format!("missing join request {field}"))
                };
                Ok::<_, Failure>(JoinRequest {
                    node: text("node")?,
                    nonce: text("nonce")?,
                    invitation: text("invitation")?,
                    name: text("name")?,
                    received_epoch_seconds: text("received")?.parse()?,
                    expires_epoch_seconds: text("expires")?.parse()?,
                    code: text("code")?,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Discovered { pending })
    }
    /// Admits a pending join request once `admit.code` equals the code its
    /// device shows; a mismatch is refused. The device enrolls at its next
    /// `join`.
    pub async fn admit(&self, admit: Admit) -> Result<(), Failure> {
        self.request(json!({
            "type": "admit",
            "node": admit.node,
            "code": admit.code,
            "profile": admit.profile,
            "label": admit.label,
        }))
        .await
        .map(drop)
    }
    /// Refuses a pending join request; its device's next `join` fails with
    /// [`crate::JoinRefused`].
    pub async fn refuse(&self, node: &str) -> Result<(), Failure> {
        self.request(json!({"type":"refuse", "node": node}))
            .await
            .map(drop)
    }
    /// A signed contact URI that another device can pass to `add_peer`.
    pub async fn contact(&self) -> Result<String, Failure> {
        let value = self.request(json!({"type":"contact"})).await?;
        Ok(value["uri"].as_str().ok_or("missing contact URI")?.into())
    }
    /// Saves and validates a contact, then wakes sync. Returns the peer's public key.
    pub async fn add_peer(&self, contact: &str) -> Result<String, Failure> {
        let value = self
            .request(json!({"type":"peer_add", "contact":contact}))
            .await?;
        Ok(value["node"].as_str().ok_or("missing peer key")?.into())
    }
    /// Removes a saved peer; credentials and grants are unaffected.
    pub async fn remove_peer(&self, node: &str) -> Result<(), Failure> {
        self.request(json!({"type":"peer_remove", "node":node}))
            .await
            .map(drop)
    }
}
