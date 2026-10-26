//! An iroh endpoint the foreign application owns: its own protocols and
//! Rocky's share one router, and only the application shuts it down. An
//! endpoint created by another library cannot be attached; the router of
//! this one is the only one Rocky's handlers are registered on.
use crate::NativeNetwork;
use crate::{ByteStream, NativeError, NativeSecretStore, NativeSession, failure};
use rocky_sdk::iroh::{
    Attachment, Options,
    iroh::{
        Endpoint, EndpointAddr,
        endpoint::{Connection, RecvStream, SendStream},
        protocol::{AcceptError, Router},
    },
};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::{
    io::AsyncWriteExt,
    runtime::{Handle, Runtime},
    sync::{Mutex, Semaphore},
};

/// Inbound streams of one application protocol handled at once.
const STREAMS: usize = 64;
const MAX_READ: u32 = 1024 * 1024;

fn message(detail: impl std::fmt::Display) -> NativeError {
    NativeError::Failure {
        detail: detail.to_string(),
    }
}

/// An application protocol the foreign application serves.
#[uniffi::export(with_foreign)]
#[async_trait::async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// One inbound stream from the endpoint `remote`. The stream is
    /// closed when this returns.
    async fn accept(&self, remote: String, stream: Arc<dyn ByteStream>);
}
#[derive(uniffi::Record)]
pub struct AppProtocol {
    /// The protocol's ALPN, such as `example/echo/1`.
    pub name: String,
    pub handler: Arc<dyn ProtocolHandler>,
}

/// One QUIC stream. Its work runs on the endpoint's runtime.
struct Quic {
    send: Arc<Mutex<Option<SendStream>>>,
    recv: Arc<Mutex<Option<RecvStream>>>,
    /// Keeps the connection open while the stream is used.
    connection: StdMutex<Option<Connection>>,
    runtime: Handle,
}
impl Quic {
    fn stream(
        connection: Connection,
        (send, recv): (SendStream, RecvStream),
        runtime: Handle,
    ) -> Arc<dyn ByteStream> {
        Arc::new(Self {
            send: Arc::new(Mutex::new(Some(send))),
            recv: Arc::new(Mutex::new(Some(recv))),
            connection: StdMutex::new(Some(connection)),
            runtime,
        })
    }
    async fn run<T: Send + 'static>(
        &self,
        work: impl Future<Output = Result<T, NativeError>> + Send + 'static,
    ) -> Result<T, NativeError> {
        self.runtime.spawn(work).await.map_err(message)?
    }
}
#[async_trait::async_trait]
impl ByteStream for Quic {
    async fn read(&self, max: u32) -> Result<Vec<u8>, NativeError> {
        let recv = self.recv.clone();
        self.run(async move {
            let mut guard = recv.lock().await;
            let stream = guard.as_mut().ok_or_else(|| message("stream closed"))?;
            let mut bytes = vec![0; max.clamp(1, MAX_READ) as usize];
            let read = stream.read(&mut bytes).await.map_err(message)?;
            bytes.truncate(read.unwrap_or(0));
            Ok(bytes)
        })
        .await
    }
    async fn write(&self, bytes: Vec<u8>) -> Result<(), NativeError> {
        let send = self.send.clone();
        self.run(async move {
            let mut guard = send.lock().await;
            let stream = guard.as_mut().ok_or_else(|| message("stream finished"))?;
            stream.write_all(&bytes).await.map_err(message)
        })
        .await
    }
    async fn finish(&self) -> Result<(), NativeError> {
        let send = self.send.clone();
        self.run(async move {
            if let Some(mut stream) = send.lock().await.take() {
                stream.shutdown().await.map_err(message)?;
            }
            Ok(())
        })
        .await
    }
    async fn abandon(&self) {
        let (send, recv) = (self.send.clone(), self.recv.clone());
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let _ = self
            .run(async move {
                // A reader waiting on the peer is not waited for.
                send.lock().await.take();
                if let Ok(mut recv) = recv.try_lock() {
                    recv.take();
                }
                drop(connection);
                Ok(())
            })
            .await;
    }
}

/// Serves one application protocol with a foreign handler.
struct App {
    handler: Arc<dyn ProtocolHandler>,
    streams: Arc<Semaphore>,
    runtime: Handle,
}
impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("rocky_uniffi::App")
    }
}
impl rocky_sdk::iroh::iroh::protocol::ProtocolHandler for App {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let remote = connection.remote_id().to_string();
        while let Ok(pair) = connection.accept_bi().await {
            // Over the bound, the stream is reset.
            let Ok(permit) = self.streams.clone().try_acquire_owned() else {
                continue;
            };
            let stream = Quic::stream(connection.clone(), pair, self.runtime.clone());
            let (handler, remote) = (self.handler.clone(), remote.clone());
            tokio::spawn(async move {
                let _permit = permit;
                handler.accept(remote, stream.clone()).await;
                stream.abandon().await;
            });
        }
        Ok(())
    }
}

/// An iroh endpoint and router the application owns. Sessions opened on
/// it borrow it; `shutdown` is the application's last step.
#[derive(uniffi::Object)]
pub struct NativeEndpoint {
    endpoint: Endpoint,
    attachment: Attachment,
    router: Router,
    runtime: StdMutex<Option<Runtime>>,
    handle: Handle,
}
#[uniffi::export]
impl NativeEndpoint {
    /// Binds an endpoint whose identity is `secret` (32 bytes the
    /// application keeps) and routes `protocols` next to Rocky's.
    /// `options` are the starter's: `relay`, `lookup`, `topic`, `local`
    /// and `bind`.
    #[uniffi::constructor]
    pub fn bind(
        options: String,
        secret: Vec<u8>,
        protocols: Vec<AppProtocol>,
    ) -> Result<Arc<Self>, NativeError> {
        let options: Options = serde_json::from_str(&options).map_err(message)?;
        let secret: [u8; 32] = secret
            .try_into()
            .map_err(|_| message("the endpoint secret must be 32 bytes"))?;
        if protocols.len() > 32
            || protocols.iter().any(|protocol| {
                protocol.name.is_empty()
                    || Attachment::alpns().any(|alpn| alpn == protocol.name.as_bytes())
            })
        {
            return Err(message("invalid application protocols"));
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rocky-endpoint")
            .enable_all()
            .build()
            .map_err(message)?;
        let handle = runtime.handle().clone();
        let (endpoint, attachment, router) = runtime
            .block_on(async {
                let endpoint = options.bind(secret).await?;
                // Discovery only as the options ask; the endpoint's own lookup
                // services stay as bound.
                let attachment = Attachment::discovering(endpoint.clone(), options.discover());
                let mut router = attachment.register(Router::builder(endpoint.clone()));
                for protocol in protocols {
                    let app = App {
                        handler: protocol.handler,
                        streams: Arc::new(Semaphore::new(STREAMS)),
                        runtime: handle.clone(),
                    };
                    router = router.accept(protocol.name.as_bytes(), app);
                }
                Ok::<_, rocky_sdk::Failure>((endpoint, attachment, router.spawn()))
            })
            .map_err(failure)?;
        Ok(Arc::new(Self {
            endpoint,
            attachment,
            router,
            runtime: StdMutex::new(Some(runtime)),
            handle,
        }))
    }
    /// The endpoint's public key.
    pub fn id(&self) -> String {
        self.endpoint.id().to_string()
    }
    /// The address other endpoints dial, as `connect` takes it.
    pub fn address(&self) -> Result<String, NativeError> {
        serde_json::to_string(&self.endpoint.addr()).map_err(message)
    }
    /// A stream of the application protocol `protocol` to `address`.
    pub async fn connect(
        &self,
        address: String,
        protocol: String,
    ) -> Result<Arc<dyn ByteStream>, NativeError> {
        let address: EndpointAddr = serde_json::from_str(&address).map_err(message)?;
        let (endpoint, handle) = (self.endpoint.clone(), self.handle.clone());
        self.handle
            .spawn(async move {
                let dial = async {
                    let connection = endpoint.connect(address, protocol.as_bytes()).await?;
                    let pair = connection.open_bi().await?;
                    Ok::<_, rocky_sdk::Failure>(Quic::stream(connection, pair, handle))
                };
                tokio::time::timeout(rocky_sdk::iroh::TIMEOUT, dial)
                    .await
                    .map_err(message)?
                    .map_err(failure)
            })
            .await
            .map_err(message)?
    }
    /// Closes the router and the endpoint. Sessions still open on it
    /// lose their network and nothing else.
    pub async fn shutdown(&self) -> Result<(), NativeError> {
        let router = self.router.clone();
        let stopped = self.handle.spawn(async move { router.shutdown().await });
        let result = stopped.await.map_err(message)?.map_err(message);
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(runtime) = runtime {
            runtime.shutdown_background();
        }
        result
    }
}

#[uniffi::export]
impl NativeSession {
    /// Opens on `endpoint`, which the application keeps and shuts down.
    #[uniffi::constructor]
    pub fn open_on_endpoint(
        options: String,
        schema: String,
        secrets: Option<Arc<dyn NativeSecretStore>>,
        endpoint: Arc<NativeEndpoint>,
        coordinator: Option<String>,
    ) -> Result<Arc<Self>, NativeError> {
        let transport = endpoint.attachment.transport();
        Self::open_on(options, schema, secrets, transport, coordinator)
    }
}

#[uniffi::export]
impl NativeNetwork {
    /// `endpoint`, which the application keeps and shuts down.
    #[uniffi::constructor]
    pub fn endpoint(endpoint: Arc<NativeEndpoint>) -> Arc<Self> {
        Self::new(rocky_sdk::Network::Borrowed(
            endpoint.attachment.transport(),
        ))
    }
}
