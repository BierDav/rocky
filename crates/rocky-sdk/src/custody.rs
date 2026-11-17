//! Configuration-only custody peer.
use crate::{
    Database, Failure, Network, Open, Opened,
    enrollment::{Invite, Settings},
};
use rocky_core::Right;
use rocky_credentials::Profile;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub struct CustodyConfiguration {
    pub directory: PathBuf,
    pub schema: crate::Schema,
    pub transport: Arc<dyn rocky_core::net::Connector>,
    pub coordination: Option<Arc<dyn rocky_credentials::Coordinator>>,
    pub invitation: Option<String>,
    pub peers: Vec<String>,
    pub enrollment_upstream: Option<String>,
}

/// A keyless, non-issuing replica, opened from typed configuration.
pub struct CustodyPeer {
    database: Arc<Database>,
}
impl CustodyPeer {
    /// Enrolls once, then resumes the persisted custody identity. Refuses
    /// reader/writer/issuer identities before opening their runtime.
    pub fn open(config: CustodyConfiguration) -> Result<Self, Failure> {
        if config.peers.len() > crate::enrollment::MAX_CONTACTS {
            return Err("custody peer quota".into());
        }
        let directory = Path::new(&config.directory);
        let identity = directory.join("identity.json");
        let existing = rocky_platform::fs::exists(&identity);
        if existing {
            let settings = Settings::load(&identity)?;
            if !matches!(settings.profile, Profile::Custody { .. })
                || settings.authority.is_some()
                || !settings.keys.is_empty()
                || settings.credentials.0.iter().any(|capability| {
                    capability.capability.grants.iter().any(|grant| {
                        grant.rights != [Right::Custody].into() || !grant.fields.is_none()
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
            if !matches!(
                Invite::parse(invitation)?.profile,
                Some(Profile::Custody { .. })
            ) {
                return Err("custody enrollment requires a custody invitation".into());
            }
        }
        let schema = serde_json::to_string(&config.schema)?;
        let open = Open {
            identity: None,
            directory,
            schema: &schema,
            secrets: None,
            network: Some(Network::Owned(config.transport)),
            coordination: config.coordination,
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
        let synchronous_open_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let configured = synchronous_open_runtime.block_on(async {
            for contact in config.peers {
                peer.database.session().add_peer(&contact).await?;
            }
            let session = peer.database.session();
            session
                .set_enrollment_upstream(config.enrollment_upstream.as_deref())
                .await?;
            session.set_automatic_sync(true).await?;
            session.close().await;
            Ok::<_, Failure>(())
        });
        if let Err(error) = configured {
            let _ = synchronous_open_runtime.block_on(peer.close());
            return Err(error);
        }
        Ok(peer)
    }
    /// The contact to configure on devices. This grants no authority.
    pub async fn contact(&self) -> Result<String, Failure> {
        let session = self.database.session();
        let contact = session.contact().await;
        session.close().await;
        contact
    }
    /// Creates the SDK's consistent backup archive without reader keys.
    pub async fn backup(&self, target: &Path) -> Result<crate::BackupInventory, Failure> {
        self.database.backup(target).await
    }
    /// Drains networking, flushes custody and closes the database.
    pub async fn close(&self) -> Result<(), Failure> {
        self.database.close().await
    }
}
