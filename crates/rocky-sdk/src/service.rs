use crate::{
    Database, DatabaseStatus, Failure, PublicKey, Route, SyncReport,
    request::Request,
    transport::{Connected, Connector, Transport},
};
use std::sync::Arc;

#[derive(Clone)]
pub enum Network {
    Owned(Arc<dyn Connector>),
    Borrowed(Arc<dyn Transport>),
    Attached(Connected),
}
impl Network {
    pub(crate) async fn start(
        self,
        accepts: bool,
        route_secret: [u8; 32],
    ) -> Result<(Connected, bool), Failure> {
        Ok(match self {
            Self::Owned(connector) => (connector.start(accepts, route_secret).await?, true),
            Self::Borrowed(transport) => (Connected::new(transport), false),
            Self::Attached(connected) => (connected, false),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceError {
    NetworkRequired(&'static str),
    NetworkAttached,
    NoRoute { peer: &'static str, network: String },
    Unreachable(String),
    TimedOut(String),
    CredentialsExpired,
    Closed(&'static str),
}
impl ServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NetworkRequired(_) | Self::CredentialsExpired => "network_required",
            Self::NetworkAttached | Self::NoRoute { .. } => "rejected",
            Self::Unreachable(_) | Self::TimedOut(_) => "unreachable",
            Self::Closed(_) => "closed",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Unreachable(_) | Self::TimedOut(_))
    }
}
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkRequired(operation) => {
                write!(f, "{operation} needs an attached network service")
            }
            Self::NetworkAttached => f.write_str("a network service is already attached"),
            Self::NoRoute { peer, network } => write!(
                f,
                "{peer} has no {network} route; attach its transport or add a current contact"
            ),
            Self::Unreachable(cause) => write!(f, "peer unreachable: {cause}"),
            Self::TimedOut(cause) => write!(f, "dial timed out: {cause}"),
            Self::CredentialsExpired => {
                f.write_str("credentials expired; open with a network that reaches an issuer")
            }
            Self::Closed(handle) => write!(f, "{handle} closed"),
        }
    }
}
impl std::error::Error for ServiceError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Off,
    Manual,
    Live,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContactInfo {
    pub uri: String,
    pub node: PublicKey,
    pub routes: Vec<Route>,
}

pub struct StatusChanges {
    database: Database,
    seen: Option<String>,
}
impl StatusChanges {
    pub async fn next(&mut self) -> Result<DatabaseStatus, crate::Error> {
        let status = match self.seen.clone() {
            None => self.database.status().await?,
            Some(seen) => {
                self.database
                    .send(|reply| Request::StatusAfter(seen, reply))
                    .await?
            }
        };
        self.seen = Some(status.token.clone());
        Ok(status)
    }
}

impl Database {
    pub async fn contact(&self) -> Result<ContactInfo, crate::Error> {
        self.send(Request::Contact).await
    }
    pub async fn add_peer(&self, contact: &str) -> Result<PublicKey, crate::Error> {
        self.send(|reply| Request::AddPeer(contact.into(), reply))
            .await
    }
    pub async fn remove_peer(&self, node: PublicKey) -> Result<(), crate::Error> {
        self.send(|reply| Request::RemovePeer(node, reply)).await
    }
    pub async fn sync_now(&self) -> Result<SyncReport, crate::Error> {
        self.send(Request::Sync).await
    }
    pub async fn set_sync(&self, mode: SyncMode) -> Result<(), crate::Error> {
        self.send(|reply| Request::SetSync(mode, reply)).await
    }
    pub async fn status(&self) -> Result<DatabaseStatus, crate::Error> {
        self.send(Request::Status).await
    }
    pub fn status_changes(&self) -> StatusChanges {
        StatusChanges {
            database: self.clone(),
            seen: None,
        }
    }
}
