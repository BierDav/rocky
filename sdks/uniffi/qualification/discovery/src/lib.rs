//! Physical-device gate for discovery and authenticated direct QUIC, without a relay or lookup.
use rocky_core::net::{Failure, Transport};
use rocky_iroh::{
    Attachment, Options,
    iroh::{
        EndpointAddr, SecretKey,
        endpoint::Connection,
        protocol::{AcceptError, ProtocolHandler, Router},
    },
};
use std::sync::{Arc, Mutex};
use tokio::runtime::{Handle, Runtime};

uniffi::setup_scaffolding!();
const ALPN: &[u8] = b"rocky/discovery-check/1";
const TOPIC: [u8; 32] = [0x74; 32];

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ProbeError {
    #[error("{detail}")]
    Failure { detail: String },
}
fn error(value: impl std::fmt::Display) -> ProbeError {
    ProbeError::Failure {
        detail: value.to_string(),
    }
}

#[derive(Debug)]
struct Echo;
impl ProtocolHandler for Echo {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        while let Ok((mut send, mut recv)) = connection.accept_bi().await {
            let mut nonce = [0; 32];
            if tokio::time::timeout(
                std::time::Duration::from_secs(3),
                recv.read_exact(&mut nonce),
            )
            .await
            .is_ok_and(|result| result.is_ok())
            {
                let _ = send.write_all(&nonce).await;
                let _ = send.finish();
            }
        }
        Ok(())
    }
}

#[derive(uniffi::Object)]
pub struct Probe {
    runtime: Mutex<Option<Runtime>>,
    handle: Handle,
    router: Router,
    transport: Arc<dyn Transport>,
    state: Arc<Mutex<serde_json::Value>>,
}
#[uniffi::export]
impl Probe {
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<Self>, ProbeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(error)?;
        let handle = runtime.handle().clone();
        let state = Arc::new(Mutex::new(serde_json::json!({"stage":"starting"})));
        let (router, transport) = runtime.block_on(async {
            let options = Options { local: true, ..Options::isolated("0.0.0.0:0") };
            let secret = SecretKey::generate();
            let endpoint = options.bind(secret.to_bytes()).await?;
            let attachment = Attachment::discovering(endpoint.clone(), options.discover());
            let transport = attachment.transport();
            let discovery = transport.discovery().ok_or("no discovery")?;
            discovery.announce(*endpoint.id().as_bytes(), &[TOPIC]).await?;
            let router = attachment.register(Router::builder(endpoint.clone())).accept(ALPN, Echo).spawn();
            *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = serde_json::json!({"stage":"announcing", "endpoint":endpoint.id().to_string(), "echoes":0});
            let state = state.clone();
            tokio::spawn(async move {
                let mut echoes = 0u64;
                loop {
                    let Some(found) = discovery.found().await else { break; };
                    {
                        let mut status = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        status["candidate"] = found.address.clone().into();
                    }
                    let dial = async {
                        let address: EndpointAddr = serde_json::from_str(&found.address)?;
                        let connection = endpoint.connect(address, ALPN).await?;
                        let (mut send, mut recv) = connection.open_bi().await?;
                        let nonce = SecretKey::generate().to_bytes();
                        send.write_all(&nonce).await?;
                        send.finish()?;
                        let mut reply = [0; 32];
                        recv.read_exact(&mut reply).await?;
                        if reply != nonce { return Err("echo mismatch".into()); }
                        Ok::<_, Failure>(connection.remote_id().to_string())
                    };
                    let result = tokio::time::timeout(std::time::Duration::from_secs(5), dial).await;
                    let mut status = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    match result {
                        Ok(Ok(peer)) => {
                            echoes += 1;
                            status["stage"] = "passed".into(); status["peer"] = peer.into();
                            status["address"] = found.address.into(); status["local"] = found.local.into();
                            status["echoes"] = echoes.into();
                        }
                        other => { status["error"] = format!("{other:?}").into(); }
                    }
                }
            });
            Ok::<_, Failure>((router, transport))
        }).map_err(error)?;
        Ok(Arc::new(Self {
            runtime: Mutex::new(Some(runtime)),
            handle,
            router,
            transport,
            state,
        }))
    }
    pub fn status(&self) -> String {
        let mut status = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        status["paths"] = format!(
            "{:?}",
            self.transport.discovery().map(|finder| finder.paths())
        )
        .into();
        status.to_string()
    }
    pub fn foreground(&self, foreground: bool) -> Result<(), ProbeError> {
        let transport = self.transport.clone();
        self.handle.block_on(async move {
            transport
                .discovery()
                .ok_or_else(|| error("no discovery"))?
                .foreground(foreground);
            Ok(())
        })
    }
    pub fn shutdown(&self) -> Result<(), ProbeError> {
        let transport = self.transport.clone();
        self.handle
            .block_on(async {
                transport.close().await;
                self.router.shutdown().await
            })
            .map_err(error)?;
        if let Some(runtime) = self
            .runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            runtime.shutdown_background();
        }
        Ok(())
    }
}
