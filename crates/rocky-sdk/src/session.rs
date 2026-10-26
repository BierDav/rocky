use crate::{Failure, GrantProfile, Session};
use serde_json::json;

/// A single-device enrollment invitation. Treat its URI as a secret.
#[derive(Clone)]
pub struct Invitation {
    pub uri: String,
    pub expires_at_epoch_seconds: u64,
    pub profile: GrantProfile,
    pub id: String,
}
impl Session {
    /// Creates a fresh invitation, replacing the issuer's previous invitation.
    /// Grant scopes name schema tables; there is deliberately no default grant.
    pub async fn pairing(&self, profile: GrantProfile) -> Result<Invitation, Failure> {
        let value = self
            .request(json!({"type":"pairing", "profile": profile}))
            .await?;
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
