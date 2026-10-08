//! Configuration-only custody peer. See `docs/runtime/always-on-peer.md`.
use crate::{
    Database, Failure, Network, Open, Opened,
    enrollment::{Invite, Settings},
    transport::TransportOptions,
};
use rocky_core::Right;
use rocky_credentials::Profile;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    directory: String,
    schema: Value,
    transport: TransportOptions,
    #[serde(default)]
    invitation: Option<String>,
    #[serde(default)]
    peers: Vec<String>,
    #[serde(default)]
    enrollment_upstream: Option<String>,
}

/// A keyless, non-issuing replica, opened entirely from JSON configuration.
pub struct CustodyPeer {
    database: Arc<Database>,
}
impl CustodyPeer {
    /// Enrolls once, then resumes the persisted custody identity. Refuses
    /// reader/writer/issuer identities before opening their runtime.
    pub fn open(configuration: &str) -> Result<Self, Failure> {
        if configuration.len() > 256 * 1024 {
            return Err("custody configuration quota".into());
        }
        let config: Configuration = serde_json::from_str(configuration)?;
        if config.peers.len() > crate::enrollment::MAX_PEERS {
            return Err("custody peer quota".into());
        }
        if matches!(
            &config.transport,
            TransportOptions::Tcp { listen: None, .. }
        ) {
            return Err("a custody TCP peer needs a listening address".into());
        }
        let directory = Path::new(&config.directory);
        let identity = directory.join("identity.json");
        let existing = identity.exists();
        if existing {
            let settings = Settings::load(&identity)?;
            if !matches!(settings.profile, Profile::Custody { .. })
                || settings.authority.is_some()
                || !settings.keys.is_empty()
                || settings.credentials.0.iter().any(|capability| {
                    capability.capability.grants.iter().any(|grant| {
                        grant.rights != [Right::Custody].into() || !grant.fields.is_empty()
                    })
                })
            {
                return Err(
                    "custody peer requires only custody grants and no reader or issuer keys".into(),
                );
            }
        } else {
            let invitation = config
                .invitation
                .as_deref()
                .ok_or("custody enrollment needs an invitation")?;
            if !matches!(Invite::parse(invitation)?.profile, Profile::Custody { .. }) {
                return Err("custody enrollment requires a custody invitation".into());
            }
        }
        let schema = config.schema.to_string();
        let open = Open {
            directory,
            schema: &schema,
            secrets: None,
            network: Some(Network::Owned(Arc::new(config.transport))),
            commands: None,
        };
        let Opened { database, .. } = if existing {
            Database::open(open)?
        } else {
            Database::join(
                open,
                config.invitation.as_deref().ok_or("missing invitation")?,
            )?
        };
        let peer = Self { database };
        // Opening is synchronous, outside the caller's async runtime.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let configured = runtime.block_on(async {
            for contact in config.peers {
                peer.call(json!({"type": "peer_add", "contact": contact}))
                    .await?;
            }
            peer.call(
                json!({"type": "enrollment_upstream", "contact": config.enrollment_upstream}),
            )
            .await?;
            peer.call(json!({"type": "sync_start"})).await?;
            Ok::<_, Failure>(())
        });
        if let Err(error) = configured {
            let _ = runtime.block_on(peer.close());
            return Err(error);
        }
        Ok(peer)
    }
    async fn call(&self, request: Value) -> Result<Value, Failure> {
        let session = self.database.session();
        let response = session.call(json!({"version": 1,
            "scope": serde_json::from_str::<Value>(&self.database.scope())?, "request": request
        }).to_string()).await;
        session.close().await;
        let response: Value = serde_json::from_str(&response?)?;
        if response["status"] != "ok" {
            return Err(response["error"]["message"]
                .as_str()
                .unwrap_or("custody call failed")
                .to_owned()
                .into());
        }
        Ok(response["value"].clone())
    }
    /// The contact to configure on devices. This grants no authority.
    pub async fn contact(&self) -> Result<String, Failure> {
        Ok(self.call(json!({"type": "contact"})).await?["uri"]
            .as_str()
            .ok_or("missing contact")?
            .to_owned())
    }
    /// Creates the SDK's consistent backup archive without reader keys.
    pub async fn backup(&self, target: &Path) -> Result<String, Failure> {
        self.database.backup(target).await
    }
    /// Drains networking, flushes custody and closes the database.
    pub async fn close(&self) -> Result<(), Failure> {
        self.database.close().await
    }
}
