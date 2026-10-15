//! The starter composition: one database, one session and a network
//! service the engine owns, selected by JSON options. It adds no behaviour
//! to the services it composes. See `docs/runtime/sdk-composition.md`.
use crate::{
    Failure,
    database::{Database, NetworkService, Open, Opened, Session},
    secrets::SecretStore,
    service::Network,
    transport::TransportOptions,
};
use serde::Deserialize;
use std::{path::Path, sync::Arc};

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum Mode {
    Create {
        #[serde(default)]
        credential_ttl: Option<u64>,
    },
    Join {
        invitation: String,
    },
    Resume,
}
#[derive(Deserialize)]
struct Options {
    directory: String,
    #[serde(default)]
    transport: TransportOptions,
    /// The trust bundle of a coordination service, public or self-hosted.
    #[serde(default)]
    coordinator: Option<serde_json::Value>,
    #[serde(flatten)]
    mode: Mode,
}

/// A database with the network it starts and owns.
pub struct Engine {
    database: Arc<Database>,
    session: Session,
    network: Option<NetworkService>,
}
impl Engine {
    /// Opens a database. Without `secrets`, keys are owner-only files in
    /// the data directory.
    pub fn open(
        options: &str,
        schema: &str,
        secrets: Option<Arc<dyn SecretStore>>,
    ) -> Result<Arc<Self>, Failure> {
        Self::open_with(options, schema, secrets, None)
    }
    /// Opens a database over `network` instead of the transport `options`
    /// name. A borrowed network stays open when the engine closes.
    pub fn open_with(
        options: &str,
        schema: &str,
        secrets: Option<Arc<dyn SecretStore>>,
        network: Option<Network>,
    ) -> Result<Arc<Self>, Failure> {
        Self::open_composed(options, schema, secrets, network, None)
    }
    /// Opens with an application-owned metadata log or service.
    pub fn open_composed(
        options: &str,
        schema: &str,
        secrets: Option<Arc<dyn SecretStore>>,
        network: Option<Network>,
        coordination: Option<crate::Coordination>,
    ) -> Result<Arc<Self>, Failure> {
        if options.len() > 16 * 1024 {
            return Err("native options quota".into());
        }
        let options: Options = serde_json::from_str(options)?;
        let coordination = match (options.coordinator.clone(), coordination) {
            (None, coordination) => coordination,
            (Some(_), Some(_)) => return Err("two coordinators supplied".into()),
            #[cfg(feature = "iroh")]
            (Some(bundle), None) => Some(crate::Coordination::Service(Box::new(
                serde_json::from_value(bundle)?,
            ))),
            #[cfg(not(feature = "iroh"))]
            (Some(_), None) => return Err("a coordination service needs iroh".into()),
        };
        let network =
            network.unwrap_or_else(|| Network::Owned(Arc::new(options.transport.clone())));
        let open = Open {
            directory: Path::new(&options.directory),
            schema,
            secrets,
            network: Some(network),
            coordination,
        };
        let Opened { database, network } = match &options.mode {
            Mode::Create { credential_ttl } => Database::create(open, *credential_ttl)?,
            Mode::Join { invitation } => Database::join(open, invitation)?,
            Mode::Resume => Database::open(open)?,
        };
        Ok(Arc::new(Self {
            session: database.session(),
            database,
            network,
        }))
    }
    pub fn scope(&self) -> String {
        self.database.scope()
    }
    pub async fn call(&self, request: String) -> Result<String, Failure> {
        self.session.call(request).await
    }
    pub async fn close(&self) -> Result<(), Failure> {
        if let Some(network) = &self.network {
            network.close().await;
        }
        self.database.close().await
    }
}
