use rocky_iroh::{
    Attachment, Discover, Options, Topic,
    iroh::{
        Endpoint, RelayMode, SecretKey,
        address_lookup::{PkarrPublisher, PkarrResolver},
        endpoint::presets,
        protocol::Router,
    },
};
use rocky_sdk::{
    Failure,
    transport::{BoxFuture, Connected, Connector},
};
use serde::Serialize;
use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

const ONLINE: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Path {
    Default,
    Bound(String),
    RelayOnly,
}
impl Path {
    pub fn parse(text: &str) -> Result<Self, Failure> {
        Ok(match text {
            "default" => Self::Default,
            "relay" => Self::RelayOnly,
            bound if bound.starts_with("bind:") => Self::Bound(bound["bind:".len()..].into()),
            other => return Err(format!("unknown path {other}").into()),
        })
    }
}

struct Held {
    attachment: Attachment,
    router: Option<Router>,
}

pub struct BenchConnector {
    path: Path,
    held: Mutex<Option<Held>>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Traffic {
    pub direct_sent: u64,
    pub direct_received: u64,
    pub relay_sent: u64,
    pub relay_received: u64,
    pub holepunch_attempts: u64,
    pub relay_rate_limited: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PathReport {
    pub selected: bool,
    pub kind: String,
    pub remote: String,
    pub rtt_us: u128,
    pub sent: u64,
    pub received: u64,
    pub lost_packets: u64,
}

impl BenchConnector {
    pub fn new(path: Path) -> Arc<Self> {
        Arc::new(Self {
            path,
            held: Mutex::new(None),
        })
    }
    fn attachment(&self) -> Option<Attachment> {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|held| held.attachment.clone())
    }
    pub fn address(&self) -> Option<String> {
        let endpoint = self.attachment()?.endpoint();
        serde_json::to_string(&endpoint.addr()).ok()
    }
    pub fn traffic(&self) -> Traffic {
        let Some(attachment) = self.attachment() else {
            return Traffic::default();
        };
        let endpoint = attachment.endpoint();
        let socket = &endpoint.metrics().socket;
        Traffic {
            direct_sent: socket.send_ipv4.get() + socket.send_ipv6.get(),
            direct_received: socket.recv_data_ipv4.get() + socket.recv_data_ipv6.get(),
            relay_sent: socket.send_relay.get(),
            relay_received: socket.recv_data_relay.get(),
            holepunch_attempts: socket.holepunch_attempts.get(),
            relay_rate_limited: socket.relay_conns_ratelimited.get(),
        }
    }
    pub async fn paths(&self, address: &str) -> Result<Vec<PathReport>, Failure> {
        let Some(attachment) = self.attachment() else {
            return Ok(Vec::new());
        };
        Ok(attachment
            .paths(address)
            .await?
            .into_iter()
            .map(|path| PathReport {
                selected: path.selected,
                kind: path.kind.into(),
                remote: path.remote,
                rtt_us: path.rtt.as_micros(),
                sent: path.sent_bytes,
                received: path.received_bytes,
                lost_packets: path.lost_packets,
            })
            .collect())
    }
    pub fn release(&self) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
    async fn relay_only(route_secret: [u8; 32]) -> Result<Held, Failure> {
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&route_secret))
            .relay_mode(RelayMode::Default)
            .clear_ip_transports()
            .address_lookup(PkarrPublisher::n0_dns())
            .address_lookup(PkarrResolver::n0_dns())
            .bind()
            .await?;
        let attachment = Attachment::discovering(
            endpoint.clone(),
            Discover {
                topic: Topic::Off,
                local: false,
            },
        );
        let router = attachment.register(Router::builder(endpoint)).spawn();
        Ok(Held {
            attachment,
            router: Some(router),
        })
    }
}

impl Connector for BenchConnector {
    fn start(
        &self,
        _accepts: bool,
        route_secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Connected, Failure>> {
        Box::pin(async move {
            let held = match &self.path {
                Path::Default => Held {
                    attachment: Options::default().start(route_secret).await?,
                    router: None,
                },
                Path::Bound(bind) => Held {
                    attachment: Options {
                        bind: Some(bind.clone()),
                        ..Options::default()
                    }
                    .start(route_secret)
                    .await?,
                    router: None,
                },
                Path::RelayOnly => Self::relay_only(route_secret).await?,
            };
            let endpoint = held.attachment.endpoint();
            if tokio::time::timeout(ONLINE, endpoint.online())
                .await
                .is_err()
            {
                return Err(format!("the endpoint found no home relay within {ONLINE:?}").into());
            }
            let connected = held.attachment.connected();
            if let Some(previous) = self
                .held
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .replace(held)
                && let Some(router) = previous.router
            {
                drop(router);
            }
            Ok(connected)
        })
    }
}
