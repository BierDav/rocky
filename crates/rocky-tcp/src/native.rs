use super::*;
use rocky_core::net::{SessionStream, scheme};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::OwnedSemaphorePermit,
};

#[derive(Clone, Debug)]
pub(super) struct Listener {
    bind: SocketAddr,
    advertise: Option<String>,
    websocket: bool,
}

impl Options {
    /// Listens on `bind`, a loopback, LAN or mesh address; port 0 picks a free port.
    pub fn new(bind: SocketAddr) -> Self {
        Self {
            listener: Some(Listener {
                bind,
                advertise: None,
                websocket: false,
            }),
            roots: Vec::new(),
        }
    }
    /// Options that only dial out: the device listens nowhere, names no route and syncs over
    /// the connections it opens itself.
    pub fn dialer() -> Self {
        Self {
            listener: None,
            roots: Vec::new(),
        }
    }
    /// The `tcp://`, `ws://` or `wss://` URL peers dial, as when the bound address is not reachable
    /// from them; without it, peers dial the bound address. A dial-only attachment ignores it.
    pub fn advertise(mut self, route: impl Into<String>) -> Self {
        if let Some(listener) = &mut self.listener {
            listener.advertise = Some(route.into());
        }
        self
    }
    /// Also accepts WebSocket handshakes on the listener, advertising `ws://` with the bound
    /// address unless `advertise` names another route.
    #[cfg(feature = "websocket")]
    pub fn websocket(mut self) -> Self {
        if let Some(listener) = &mut self.listener {
            listener.websocket = true;
        }
        self
    }
    /// DER-encoded root certificates a `wss://` dial trusts besides the public web roots, as for a
    /// server whose certificate a private CA issued; `start` fails on one that does not parse.
    #[cfg(feature = "websocket")]
    pub fn root_certificates(mut self, der: impl IntoIterator<Item = Vec<u8>>) -> Self {
        self.roots.extend(der);
        self
    }
    pub(super) async fn serve(&self, owned: bool) -> Result<Attachment, Failure> {
        let dialer = Dialer::new(&self.roots)?;
        let Some(options) = &self.listener else {
            return Ok(Attachment::build(None, None, owned, dialer));
        };
        if let Some(advertise) = &options.advertise
            && !scheme(advertise).is_some_and(|scheme| [TCP, WS, WSS].contains(&scheme))
        {
            return Err(format!("advertise a tcp://, ws:// or wss:// URL, not {advertise}").into());
        }
        let listener = TcpListener::bind(options.bind).await?;
        let local = listener.local_addr()?;
        let advertise = options.advertise.clone().unwrap_or_else(|| {
            let scheme = if options.websocket { WS } else { TCP };
            format!("{scheme}://{local}")
        });
        let attachment = Attachment::build(Some(local), Some(advertise), owned, dialer);
        let websocket = options.websocket;
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
                    let _ = accepted_tcp(shared, stream, websocket).await;
                });
            }
        });
        Ok(attachment)
    }
}

impl Attachment {
    /// An attachment with no listener of its own, for connections an application's HTTP server
    /// upgrades and hands to `admit_websocket`; `advertise` is the public `ws://` or `wss://` URL.
    #[cfg(feature = "websocket")]
    pub fn external(advertise: impl Into<String>) -> Self {
        Self::build(None, Some(advertise.into()), false, Dialer::default())
    }
    /// Serves a connection the application's HTTP server already upgraded to WebSocket.
    #[cfg(feature = "websocket")]
    pub async fn admit_websocket(
        &self,
        upgraded: impl SessionStream + 'static,
    ) -> Result<(), Failure> {
        if *self.0.closed.borrow() {
            return Err("the TCP attachment is closed".into());
        }
        admit(self.0.clone(), websocket::stream(upgraded).await).await
    }
}

#[derive(Default)]
pub(super) struct Dialer {
    #[cfg(feature = "websocket")]
    tls: websocket::Tls,
}
impl Dialer {
    #[cfg(feature = "websocket")]
    fn new(roots: &[Vec<u8>]) -> Result<Self, Failure> {
        Ok(Self {
            tls: websocket::Tls::new(roots)?,
        })
    }
    #[cfg(not(feature = "websocket"))]
    fn new(_roots: &[Vec<u8>]) -> Result<Self, Failure> {
        Ok(Self {})
    }
    pub(super) async fn open(&self, address: &str) -> Result<Box<dyn SessionStream>, Failure> {
        match scheme(address) {
            Some(TCP) => {
                let host = address
                    .strip_prefix("tcp://")
                    .ok_or_else(|| unsupported(address))?;
                let stream = TcpStream::connect(host.trim_end_matches('/')).await?;
                stream.set_nodelay(true)?;
                Ok(Box::new(stream))
            }
            #[cfg(feature = "websocket")]
            Some(WS | WSS) => websocket::dial(address, &self.tls).await,
            _ => Err(unsupported(address)),
        }
    }
}

enum Admitted {
    Rpc(Arc<Inbox>, OwnedSemaphorePermit),
    Stream(Arc<Inbox>, Protocol),
}

fn inbox_for(shared: &Shared, hello: &Hello) -> Result<Admitted, ScopeRefusal> {
    if hello.version != VERSION {
        return Err(ScopeRefusal::UnsupportedVersion);
    }
    let key = match hello.protocol {
        Wire::Coordinate => None,
        Wire::Rpc | Wire::Link => Some(hello.scope.ok_or(ScopeRefusal::UnknownScope)?),
    };
    let inbox = shared
        .inboxes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .and_then(Weak::upgrade)
        .ok_or(ScopeRefusal::UnknownScope)?;
    if !inbox.listening.load(Ordering::SeqCst) || *inbox.closed.borrow() {
        return Err(ScopeRefusal::NotListening);
    }
    if inbox.sender.capacity() == 0 {
        return Err(ScopeRefusal::Busy);
    }
    match hello.protocol {
        Wire::Rpc => {
            let permit = shared
                .rpc
                .clone()
                .try_acquire_owned()
                .map_err(|_| ScopeRefusal::Busy)?;
            Ok(Admitted::Rpc(inbox, permit))
        }
        Wire::Link | Wire::Coordinate => Ok(Admitted::Stream(inbox, hello.protocol.into())),
    }
}

async fn accepted_tcp(
    shared: Arc<Shared>,
    stream: TcpStream,
    websocket: bool,
) -> Result<(), Failure> {
    stream.set_nodelay(true)?;
    if websocket {
        #[cfg(feature = "websocket")]
        {
            let stream =
                rocky_platform::time::timeout(TIMEOUT, websocket::accept(stream)).await??;
            return admit(shared, stream).await;
        }
    }
    admit(shared, stream).await
}

pub(super) async fn admit(
    shared: Arc<Shared>,
    mut stream: impl SessionStream + 'static,
) -> Result<(), Failure> {
    let hello: Hello = rocky_platform::time::timeout(TIMEOUT, async {
        rocky_credentials::decode_bounded(
            &framing::read_bounded(&mut stream, MAX_HEADER).await?,
            MAX_HEADER,
        )
        .map_err(Failure::from)
    })
    .await??;
    let admitted = inbox_for(&shared, &hello);
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
        Admitted::Rpc(inbox, permit) => {
            let _permit = permit;
            let mut closed = inbox.closed.subscribe();
            tokio::select! {
                served = rocky_platform::time::timeout(2 * TIMEOUT, framing::serve(Stream::new(stream), &inbox.sender)) => served??,
                _ = closed.wait_for(|closed| *closed) => {},
            }
        }
        Admitted::Stream(inbox, protocol) => {
            if inbox
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
