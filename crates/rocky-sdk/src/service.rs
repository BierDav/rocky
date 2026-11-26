use crate::{
    Database, DatabaseStatus, Failure, PublicKey, Route, SyncReport,
    request::Request,
    transport::{Connected, Connector, Transport},
};
use std::sync::Arc;

/// How a database reaches peers.
#[derive(Clone)]
pub enum Network {
    /// A connector Rocky starts at open and closes at close, such as `rocky_tcp::Options` or
    /// `rocky_iroh::Options`.
    Owned(Arc<dyn Connector>),
    /// A transport the application owns and closes, such as one from `Attachment::transport`.
    Borrowed(Arc<dyn Transport>),
    /// A started transport with optional discovery and network-change events, which the application
    /// owns.
    Attached(Connected),
}
impl<C: Connector + 'static> From<C> for Network {
    fn from(connector: C) -> Self {
        Self::Owned(Arc::new(connector))
    }
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

/// Why the network refused or failed a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceError {
    /// The call needs an attached network.
    NetworkRequired(&'static str),
    /// A network is already attached.
    NetworkAttached,
    /// No route to the peer has a scheme the attached transport claims; attach its transport or
    /// add a current contact.
    NoRoute {
        /// The kind of peer.
        peer: &'static str,
        /// The route schemes the attached transport claims.
        schemes: Vec<String>,
    },
    /// The peer did not answer, and `std::error::Error::source` yields the transport's error, such as
    /// a refused connection or an untrusted TLS certificate; retry.
    Unreachable(Cause),
    /// The dial timed out, and `std::error::Error::source` yields the transport's error; retry.
    TimedOut(Cause),
    /// This device's credentials expired and it needs a network to renew them with an admin device.
    CredentialsExpired,
    /// The database or network is closed.
    Closed(&'static str),
    /// Sync is `Off`.
    SyncOff,
    /// A sync pass reconciled with no peer, as when every peer is offline or an owner has no other
    /// device yet; the pass's report says why each known peer failed.
    NoPeerReached(Box<SyncReport>),
}
/// The error a transport returned for a failed dial; `ServiceError`'s `source` yields it.
#[derive(Clone)]
pub struct Cause(Arc<dyn std::error::Error + Send + Sync>);
impl Cause {
    pub(crate) fn new(error: Failure) -> Self {
        Self(Arc::from(error))
    }
}
impl PartialEq for Cause {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.to_string() == other.0.to_string()
    }
}
impl Eq for Cause {}
impl std::fmt::Debug for Cause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}
impl std::fmt::Display for Cause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl ServiceError {
    pub(crate) fn dial_failed(error: Failure) -> Failure {
        if error.is::<rocky_core::net::ScopeRefusal>() || error.is::<Self>() {
            return error;
        }
        let timed_out = error.is::<rocky_platform::time::error::Elapsed>()
            || error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::TimedOut);
        let cause = Cause::new(error);
        Box::new(if timed_out {
            Self::TimedOut(cause)
        } else {
            Self::Unreachable(cause)
        })
    }
    pub(crate) fn no_route(peer: &'static str, transport: &dyn Transport) -> Self {
        Self::NoRoute {
            peer,
            schemes: transport
                .schemes()
                .iter()
                .map(|scheme| (*scheme).to_owned())
                .collect(),
        }
    }
    /// A stable snake-case code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NetworkRequired(_) | Self::CredentialsExpired => "network_required",
            Self::NetworkAttached | Self::NoRoute { .. } => "rejected",
            Self::Unreachable(_) | Self::TimedOut(_) | Self::NoPeerReached(_) => "unreachable",
            Self::Closed(_) => "closed",
            Self::SyncOff => "sync_off",
        }
    }
    /// Whether a retry may succeed; a pass that reached no peer may succeed later unless every
    /// known peer refused for a reason a retry does not clear.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Unreachable(_) | Self::TimedOut(_) => true,
            Self::NoPeerReached(report) => {
                report.peers.is_empty() || report.peers.iter().any(|peer| peer.retryable())
            }
            _ => false,
        }
    }
}
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkRequired(operation) => {
                write!(f, "{operation} needs an attached network service")
            }
            Self::NetworkAttached => f.write_str("a network service is already attached"),
            Self::NoRoute { peer, schemes } => write!(
                f,
                "{peer} has no {} route; attach its transport or add a current contact",
                schemes.join(", ")
            ),
            Self::Unreachable(cause) => write!(f, "peer unreachable: {cause}"),
            Self::TimedOut(cause) => write!(f, "dial timed out: {cause}"),
            Self::CredentialsExpired => {
                f.write_str("credentials expired; open with a network that reaches an issuer")
            }
            Self::Closed(handle) => write!(f, "{handle} closed"),
            Self::SyncOff => f.write_str("sync is off; set the sync mode to Manual or Live"),
            Self::NoPeerReached(report) if report.peers.is_empty() => {
                f.write_str("no peer is known to sync with")
            }
            Self::NoPeerReached(report) => {
                write!(
                    f,
                    "none of the {} known peers reconciled",
                    report.peers.len()
                )?;
                for peer in &report.peers {
                    if let Some(failure) = peer.failure() {
                        write!(f, "; {}: {}", peer.node, failure.detail)?;
                    }
                }
                Ok(())
            }
        }
    }
}
impl std::error::Error for ServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreachable(cause) | Self::TimedOut(cause) => Some(&*cause.0),
            _ => None,
        }
    }
}

/// How a database syncs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// Neither serves peers nor reaches out.
    Off,
    /// Serves peers but reaches out only on `sync_now`.
    Manual,
    /// The default: serves peers, syncs on its own and pushes each commit to connected peers at
    /// once.
    Live,
}

/// How other devices reach this one.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContactInfo {
    /// The contact to pass to `add_peer` or `Configuration::link` on another device.
    pub uri: String,
    /// This device's key.
    pub node: PublicKey,
    /// Its routes.
    pub routes: Vec<Route>,
}

/// Status snapshots, each returned once the status changed since the last.
pub struct StatusChanges {
    database: Database,
    seen: Option<String>,
}
impl StatusChanges {
    /// Waits for the status to change and returns it; the first call returns at once.
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
    /// How other devices reach this one.
    pub async fn contact(&self) -> Result<ContactInfo, crate::Error> {
        self.send(Request::Contact).await
    }
    /// Adds a peer from its contact and returns its device; it undoes an earlier `remove_peer`.
    pub async fn add_peer(&self, contact: &str) -> Result<crate::DeviceId, crate::Error> {
        self.send(|reply| Request::AddPeer(contact.into(), reply))
            .await
            .map(crate::DeviceId::new)
    }
    /// Forgets a peer however this device learned it and keeps it from being learned again, also
    /// across restarts, until `add_peer` adds it; removing an unknown peer succeeds.
    pub async fn remove_peer(&self, device: crate::DeviceId) -> Result<(), crate::Error> {
        self.send(|reply| Request::RemovePeer(device.key(), reply))
            .await
    }
    /// Runs one sync pass; fails with `NoPeerReached` when no peer reconciled and with `SyncOff`
    /// when sync is off.
    pub async fn sync_now(&self) -> Result<SyncReport, crate::Error> {
        self.send(Request::Sync).await
    }
    /// Changes the sync mode.
    pub async fn set_sync(&self, mode: SyncMode) -> Result<(), crate::Error> {
        self.send(|reply| Request::SetSync(mode, reply)).await
    }
    /// Who this device is, where its data stands and what the network does.
    pub async fn status(&self) -> Result<DatabaseStatus, crate::Error> {
        self.send(Request::Status).await
    }
    /// A stream of status snapshots as the status changes.
    pub fn status_changes(&self) -> StatusChanges {
        StatusChanges {
            database: self.clone(),
            seen: None,
        }
    }
}
