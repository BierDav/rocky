#![cfg(not(target_family = "wasm"))]

use rocky_core::{
    Scope,
    net::{
        BoxFuture, Connected, Connector, Failure, Incoming, Protocol, ScopeRefusal, Stream,
        Transport, framing,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, watch},
};

pub const KIND: &str = "tcp";
pub const TIMEOUT: Duration = Duration::from_secs(15);
pub const PENDING: usize = 32;
pub const RPC_REQUESTS: usize = 32;
const VERSION: u32 = 1;
const MAX_HEADER: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Wire {
    Rpc,
    Link,
    Coordinate,
}
impl From<Protocol> for Wire {
    fn from(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Rpc => Self::Rpc,
            Protocol::Link => Self::Link,
            Protocol::Coordinate => Self::Coordinate,
        }
    }
}
impl From<Wire> for Protocol {
    fn from(wire: Wire) -> Self {
        match wire {
            Wire::Rpc => Self::Rpc,
            Wire::Link => Self::Link,
            Wire::Coordinate => Self::Coordinate,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    version: u32,
    protocol: Wire,
    scope: Option<Scope>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    version: u32,
    result: Result<(), ScopeRefusal>,
}

struct Route {
    scope: std::sync::Mutex<Option<Scope>>,
    sender: mpsc::Sender<Incoming>,
    incoming: Mutex<mpsc::Receiver<Incoming>>,
    listening: AtomicBool,
    closed: watch::Sender<bool>,
}
impl Route {
    fn new() -> Self {
        let (sender, incoming) = mpsc::channel(PENDING);
        Self {
            scope: Default::default(),
            sender,
            incoming: Mutex::new(incoming),
            listening: AtomicBool::new(false),
            closed: watch::channel(false).0,
        }
    }
}

struct Shared {
    local: Option<SocketAddr>,
    advertise: Option<String>,
    owned: bool,
    routes: std::sync::Mutex<HashMap<Option<Scope>, Weak<Route>>>,
    rpc: Arc<Semaphore>,
    refused: AtomicU64,
    closed: watch::Sender<bool>,
}

#[derive(Clone, Debug)]
pub struct Options {
    bind: SocketAddr,
    advertise: Option<String>,
}
impl Options {
    pub fn new(bind: SocketAddr) -> Self {
        Self {
            bind,
            advertise: None,
        }
    }
    pub fn advertise(mut self, address: impl Into<String>) -> Self {
        self.advertise = Some(address.into());
        self
    }
    pub async fn start(&self) -> Result<Attachment, Failure> {
        self.serve(false).await
    }
    pub fn dialer() -> Attachment {
        Attachment::build(None, None, false)
    }
    async fn serve(&self, owned: bool) -> Result<Attachment, Failure> {
        let listener = TcpListener::bind(self.bind).await?;
        let attachment =
            Attachment::build(Some(listener.local_addr()?), self.advertise.clone(), owned);
        let shared = Arc::downgrade(&attachment.0);
        let mut closed = attachment.0.closed.subscribe();
        rocky_platform::task::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    biased;
                    _ = closed.wait_for(|closed| *closed) => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                let Some(shared) = shared.upgrade() else {
                    break;
                };
                rocky_platform::task::spawn(async move {
                    let _ = admit(shared, stream).await;
                });
            }
        });
        Ok(attachment)
    }
}
impl Connector for Options {
    fn start(
        &self,
        _accepts: bool,
        _secret: [u8; 32],
    ) -> BoxFuture<'_, Result<Connected, Failure>> {
        Box::pin(async move { Ok(self.serve(true).await?.connected()) })
    }
}

#[derive(Clone)]
pub struct Attachment(Arc<Shared>);
impl Attachment {
    fn build(local: Option<SocketAddr>, advertise: Option<String>, owned: bool) -> Self {
        Self(Arc::new(Shared {
            local,
            advertise,
            owned,
            routes: Default::default(),
            rpc: Arc::new(Semaphore::new(RPC_REQUESTS)),
            refused: AtomicU64::new(0),
            closed: watch::channel(false).0,
        }))
    }
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.0.local
    }
    pub fn refused(&self) -> u64 {
        self.0.refused.load(Ordering::Relaxed)
    }
    pub fn transport(&self) -> Arc<dyn Transport> {
        Arc::new(Port {
            attachment: self.clone(),
            route: Arc::new(Route::new()),
        })
    }
    pub fn connected(&self) -> Connected {
        Connected::new(self.transport())
    }
    pub fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.0.closed.send_replace(true);
            let routes: Vec<_> = self
                .0
                .routes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .drain()
                .filter_map(|(_, route)| route.upgrade())
                .collect();
            for route in routes {
                route.closed.send_replace(true);
                route.incoming.lock().await.close();
            }
        })
    }
    async fn dial(
        &self,
        address: &str,
        protocol: Protocol,
        scope: Option<Scope>,
    ) -> Result<Stream, Failure> {
        if *self.0.closed.borrow() {
            return Err("the TCP attachment is closed".into());
        }
        let scope = match protocol {
            Protocol::Coordinate => None,
            Protocol::Rpc | Protocol::Link => Some(scope.ok_or(ScopeRefusal::UnknownScope)?),
        };
        let mut stream =
            rocky_platform::time::timeout(TIMEOUT, TcpStream::connect(address)).await??;
        stream.set_nodelay(true)?;
        rocky_platform::time::timeout(TIMEOUT, async {
            framing::write(
                &mut stream,
                &rocky_credentials::encode(&Hello {
                    version: VERSION,
                    protocol: protocol.into(),
                    scope,
                })?,
            )
            .await?;
            let reply: Accepted = rocky_credentials::decode_bounded(
                &framing::read_bounded(&mut stream, MAX_HEADER).await?,
                MAX_HEADER,
            )?;
            if reply.version != VERSION {
                return Err::<(), Failure>(ScopeRefusal::UnsupportedVersion.into());
            }
            reply.result.map_err(Into::into)
        })
        .await??;
        Ok(Stream::new(stream))
    }
}

enum Admitted {
    Rpc(Arc<Route>, OwnedSemaphorePermit),
    Stream(Arc<Route>, Protocol),
}

fn route_for(shared: &Shared, hello: &Hello) -> Result<Admitted, ScopeRefusal> {
    if hello.version != VERSION {
        return Err(ScopeRefusal::UnsupportedVersion);
    }
    let key = match hello.protocol {
        Wire::Coordinate => None,
        Wire::Rpc | Wire::Link => Some(hello.scope.ok_or(ScopeRefusal::UnknownScope)?),
    };
    let route = shared
        .routes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .and_then(Weak::upgrade)
        .ok_or(ScopeRefusal::UnknownScope)?;
    if !route.listening.load(Ordering::SeqCst) || *route.closed.borrow() {
        return Err(ScopeRefusal::NotListening);
    }
    if route.sender.capacity() == 0 {
        return Err(ScopeRefusal::Busy);
    }
    match hello.protocol {
        Wire::Rpc => {
            let permit = shared
                .rpc
                .clone()
                .try_acquire_owned()
                .map_err(|_| ScopeRefusal::Busy)?;
            Ok(Admitted::Rpc(route, permit))
        }
        Wire::Link | Wire::Coordinate => Ok(Admitted::Stream(route, hello.protocol.into())),
    }
}

async fn admit(shared: Arc<Shared>, mut stream: TcpStream) -> Result<(), Failure> {
    stream.set_nodelay(true)?;
    let hello: Hello = rocky_platform::time::timeout(TIMEOUT, async {
        rocky_credentials::decode_bounded(
            &framing::read_bounded(&mut stream, MAX_HEADER).await?,
            MAX_HEADER,
        )
        .map_err(Failure::from)
    })
    .await??;
    let admitted = route_for(&shared, &hello);
    framing::write(
        &mut stream,
        &rocky_credentials::encode(&Accepted {
            version: VERSION,
            result: admitted.as_ref().map(|_| ()).map_err(Clone::clone),
        })?,
    )
    .await?;
    let admitted = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => {
            shared.refused.fetch_add(1, Ordering::Relaxed);
            stream.shutdown().await?;
            return Err(refusal.into());
        }
    };
    match admitted {
        Admitted::Rpc(route, permit) => {
            let _permit = permit;
            let mut closed = route.closed.subscribe();
            tokio::select! {
                served = rocky_platform::time::timeout(2 * TIMEOUT, framing::serve(Stream::new(stream), &route.sender)) => served??,
                _ = closed.wait_for(|closed| *closed) => {},
            }
        }
        Admitted::Stream(route, protocol) => {
            if route
                .sender
                .try_send(Incoming::Stream(protocol, Stream::new(stream)))
                .is_err()
            {
                shared.refused.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    Ok(())
}

struct Port {
    attachment: Attachment,
    route: Arc<Route>,
}
impl Transport for Port {
    fn kind(&self) -> &str {
        KIND
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async {
            let shared = &self.attachment.0;
            shared
                .advertise
                .clone()
                .or_else(|| shared.local.map(|local| local.to_string()))
                .ok_or_else(|| "this TCP attachment does not accept connections".into())
        })
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
        scope: Option<Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(self.attachment.dial(address, protocol, scope))
    }
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        Box::pin(async move {
            let mut failure: Failure = "no route".into();
            for address in addresses {
                let result = rocky_platform::time::timeout(3 * TIMEOUT, async {
                    let mut stream = self
                        .attachment
                        .dial(address, Protocol::Rpc, Some(scope))
                        .await?;
                    framing::write(&mut stream, request).await?;
                    let reply = framing::read(&mut stream).await?;
                    let _ = stream.shutdown().await;
                    Ok::<_, Failure>(reply)
                })
                .await;
                match result {
                    Ok(Ok(reply)) => return Ok(reply),
                    Ok(Err(error)) => failure = error,
                    Err(error) => failure = error.into(),
                }
            }
            Err(failure)
        })
    }
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            let mut failure: Failure = "no route".into();
            for address in addresses {
                match rocky_platform::time::timeout(
                    3 * TIMEOUT,
                    self.attachment.dial(address, Protocol::Link, Some(scope)),
                )
                .await
                {
                    Ok(Ok(stream)) => return Ok(stream),
                    Ok(Err(error)) => failure = error,
                    Err(error) => failure = error.into(),
                }
            }
            Err(failure)
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<Incoming>> {
        Box::pin(async {
            let mut closed = self.route.closed.subscribe();
            let mut incoming = tokio::select! {
                biased;
                _ = closed.wait_for(|closed| *closed) => return None,
                incoming = self.route.incoming.lock() => incoming,
            };
            tokio::select! {
                biased;
                _ = closed.wait_for(|closed| *closed) => None,
                accepted = incoming.recv() => accepted,
            }
        })
    }
    fn listen(&self, scope: Option<Scope>, listening: bool) -> Result<(), Failure> {
        let shared = &self.attachment.0;
        let mut routes = shared.routes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut bound = self
            .route
            .scope
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        routes.retain(|_, route| route.strong_count() > 0);
        if listening {
            if shared.local.is_none() {
                return Err("this TCP attachment does not accept connections".into());
            }
            if *self.route.closed.borrow() || *shared.closed.borrow() {
                return Err(ScopeRefusal::NotListening.into());
            }
            if self.route.listening.load(Ordering::SeqCst) && *bound != scope {
                return Err(ScopeRefusal::AlreadyAttached.into());
            }
            if routes
                .get(&scope)
                .and_then(Weak::upgrade)
                .is_some_and(|route| !Arc::ptr_eq(&route, &self.route))
            {
                return Err(ScopeRefusal::AlreadyAttached.into());
            }
            *bound = scope;
            routes.insert(scope, Arc::downgrade(&self.route));
        } else if routes
            .get(&scope)
            .and_then(Weak::upgrade)
            .is_some_and(|route| Arc::ptr_eq(&route, &self.route))
        {
            routes.remove(&scope);
        }
        self.route.listening.store(listening, Ordering::SeqCst);
        Ok(())
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let scope = *self
                .route
                .scope
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let _ = self.listen(scope, false);
            self.route.closed.send_replace(true);
            self.route.incoming.lock().await.close();
            if self.attachment.0.owned {
                self.attachment.close().await;
            }
        })
    }
}

#[cfg(test)]
mod tests;
