//! The network port and the transports the SDK supplies besides iroh:
//! TCP, an in-process hub with fault controls and the chunk adapter for
//! transports defined outside Rust. Enrollment, renewal and sync carry
//! their own authentication and encryption; a transport only connects
//! peers. See `docs/runtime/sdk-transports.md`.
pub use rocky_core::net::{
    BoxFuture, Connector, Failure, Protocol, SessionStream, Stream, Transport,
};
use serde::Deserialize;
use std::sync::Arc;

pub mod adapter;
pub mod memory;
mod tcp;

/// The starter's transport, as native options name it.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransportOptions {
    /// Plain TCP. A node listens on `listen`; the domain creator listens
    /// on an ephemeral loopback port without it.
    Tcp {
        #[serde(default)]
        listen: Option<String>,
        #[serde(default)]
        advertised: Option<String>,
    },
    /// QUIC through an iroh endpoint the starter owns, dialed by endpoint
    /// key. Needs the `iroh` feature.
    Iroh {
        /// `default`, `staging`, `disabled` or a relay URL.
        #[serde(default)]
        relay: Option<String>,
        /// Publish and resolve endpoint addresses through n0's DNS service.
        #[serde(default = "enabled")]
        discovery: bool,
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
            discovery: true,
            bind: None,
        }
    }
}
impl Connector for TransportOptions {
    fn start(
        &self,
        accepts: bool,
        secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Arc<dyn Transport>, Failure>> {
        Box::pin(async move {
            match self {
                Self::Tcp { listen, advertised } => {
                    let listen = match listen.as_deref() {
                        Some(listen) => Some(listen),
                        None => accepts.then_some(tcp::DEFAULT_LISTEN),
                    };
                    let transport = tcp::Tcp::start(listen, advertised.as_deref()).await?;
                    Ok(Arc::new(transport) as Arc<dyn Transport>)
                }
                #[cfg(feature = "iroh")]
                Self::Iroh {
                    relay,
                    discovery,
                    bind,
                } => {
                    let options = rocky_iroh::Options {
                        relay: relay.clone(),
                        discovery: *discovery,
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
