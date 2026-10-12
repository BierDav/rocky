//! The network port and the starter's options. iroh is Rocky's only
//! network; the in-process hub with fault controls serves tests.
//! Enrollment, renewal and sync carry their own authentication and
//! encryption; a transport only connects peers. See
//! `docs/runtime/sdk-transports.md`.
pub use rocky_core::net::{
    BoxFuture, Connector, Discovery, Failure, Found, LOCAL_PATH, NetworkPath, PathSource,
    PathState, Protocol, SessionStream, Stream, TOPIC_PATH, Transport,
};
use serde::Deserialize;
use std::sync::Arc;

pub mod memory;

/// The starter's transport, as native options name it.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransportOptions {
    /// QUIC through an iroh endpoint the starter owns, dialed by endpoint
    /// key. Needs the `iroh` feature.
    Iroh {
        /// `default`, `staging`, `disabled` or a relay URL.
        #[serde(default)]
        relay: Option<String>,
        /// Where endpoint addresses are published and resolved: `default`
        /// for n0's DNS service, `disabled`, or the URL of a pkarr relay.
        #[serde(default)]
        lookup: Option<String>,
        /// Find domain members through the topic's rendezvous record.
        #[serde(default = "enabled")]
        topic: bool,
        /// Find domain members on the local network over mDNS.
        #[serde(default = "enabled")]
        local: bool,
        /// A local socket address to bind, such as a fixed port.
        #[serde(default)]
        bind: Option<String>,
    },
}
fn enabled() -> bool {
    true
}
impl Default for TransportOptions {
    fn default() -> Self {
        Self::Iroh {
            relay: None,
            lookup: None,
            topic: true,
            local: true,
            bind: None,
        }
    }
}
impl TransportOptions {
    /// iroh on `bind` with no relay, lookup service or discovery: peers
    /// are reached at their contacts' addresses only.
    pub fn isolated_iroh(bind: &str) -> Self {
        Self::Iroh {
            relay: Some("disabled".into()),
            lookup: Some("disabled".into()),
            topic: false,
            local: false,
            bind: Some(bind.into()),
        }
    }
}
impl Connector for TransportOptions {
    fn start(
        &self,
        _accepts: bool,
        secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Arc<dyn Transport>, Failure>> {
        Box::pin(async move {
            match self {
                #[cfg(feature = "iroh")]
                Self::Iroh {
                    relay,
                    lookup,
                    topic,
                    local,
                    bind,
                } => {
                    let options = rocky_iroh::Options {
                        relay: relay.clone(),
                        lookup: lookup.clone(),
                        topic: *topic,
                        local: *local,
                        bind: bind.clone(),
                    };
                    Ok(options.start(secret).await?.transport())
                }
                #[cfg(not(feature = "iroh"))]
                Self::Iroh { .. } => {
                    let _ = secret;
                    Err("this build has no iroh transport".into())
                }
            }
        })
    }
}
