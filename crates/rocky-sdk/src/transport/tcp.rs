//! TCP: a five-byte preamble names the protocol of each connection.
use super::{BoxFuture, Protocol, Stream, Transport};
use crate::Failure;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, mpsc},
    task::JoinHandle,
};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:0";
const MAGIC: &[u8; 4] = b"RKY1";
const TIMEOUT: Duration = Duration::from_secs(10);
/// Connections whose preamble is still awaited.
const PENDING: usize = 32;

fn tag(protocol: Protocol) -> u8 {
    match protocol {
        Protocol::Pair => 1,
        Protocol::Sync => 2,
        Protocol::Link => 7,
        Protocol::Renew => 3,
        Protocol::Command => 6,
        Protocol::Coordinate => 5,
    }
}

pub struct Tcp {
    advertised: Option<String>,
    incoming: Mutex<mpsc::Receiver<(Protocol, Stream)>>,
    listener: std::sync::Mutex<Option<JoinHandle<()>>>,
    listening: Arc<AtomicBool>,
}
impl Tcp {
    /// Listens on `listen` when given. Peers dial `advertised`, or the
    /// bound address without it.
    pub async fn start(listen: Option<&str>, advertised: Option<&str>) -> Result<Self, Failure> {
        let (sender, receiver) = mpsc::channel(PENDING);
        let listening = Arc::new(AtomicBool::new(false));
        let (listener, advertised) = match listen {
            Some(listen) => {
                let listener = TcpListener::bind(listen).await?;
                let address = match advertised {
                    Some(advertised) => advertised.to_owned(),
                    None => listener.local_addr()?.to_string(),
                };
                let task = tokio::spawn(accept(listener, sender, listening.clone()));
                (Some(task), Some(address))
            }
            None => (None, None),
        };
        Ok(Self {
            advertised,
            incoming: Mutex::new(receiver),
            listener: std::sync::Mutex::new(listener),
            listening,
        })
    }
}
async fn accept(
    listener: TcpListener,
    sender: mpsc::Sender<(Protocol, Stream)>,
    listening: Arc<AtomicBool>,
) {
    let pending = Arc::new(tokio::sync::Semaphore::new(PENDING));
    while let Ok((mut stream, _)) = listener.accept().await {
        // Without a service, or over the pending bound, the dialer is dropped.
        if !listening.load(Ordering::SeqCst) {
            continue;
        }
        let Ok(permit) = pending.clone().try_acquire_owned() else {
            continue;
        };
        let sender = sender.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut preamble = [0; 5];
            let read = tokio::time::timeout(TIMEOUT, stream.read_exact(&mut preamble)).await;
            let protocol = Protocol::ALL
                .into_iter()
                .find(|protocol| preamble[4] == tag(*protocol));
            if let (Ok(Ok(_)), true, Some(protocol)) = (read, &preamble[..4] == MAGIC, protocol) {
                let _ = stream.set_nodelay(true);
                let _ = sender.send((protocol, Box::new(stream) as Stream)).await;
            }
        });
    }
}
impl Transport for Tcp {
    fn kind(&self) -> &str {
        "tcp"
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async {
            self.advertised
                .clone()
                .ok_or_else(|| "this node does not accept connections".into())
        })
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            let mut stream = tokio::time::timeout(TIMEOUT, TcpStream::connect(address)).await??;
            stream.set_nodelay(true)?;
            let mut preamble = *b"RKY1\0";
            preamble[4] = tag(protocol);
            stream.write_all(&preamble).await?;
            Ok(Box::new(stream) as Stream)
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<(Protocol, Stream)>> {
        Box::pin(async { self.incoming.lock().await.recv().await })
    }
    fn listen(&self, listening: bool) {
        self.listening.store(listening, Ordering::SeqCst);
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            let task = self
                .listener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(task) = task {
                task.abort();
                let _ = task.await;
            }
            self.incoming.lock().await.close();
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportOptions;
    use rocky_core::net::Connector;

    #[tokio::test]
    async fn default_issuers_listen_concurrently_on_distinct_ports() {
        let options = TransportOptions::Tcp {
            listen: None,
            advertised: None,
        };
        let a = options.start(true, [1; 32]).await.unwrap();
        let b = options.start(true, [2; 32]).await.unwrap();
        let aa: std::net::SocketAddr = a.address().await.unwrap().parse().unwrap();
        let bb: std::net::SocketAddr = b.address().await.unwrap().parse().unwrap();
        assert_ne!(aa.port(), 0);
        assert_ne!(aa, bb);
        a.listen(true);
        let mut outgoing = b.connect(&aa.to_string(), Protocol::Pair).await.unwrap();
        let (protocol, mut incoming) = a.accept().await.unwrap();
        assert_eq!(protocol, Protocol::Pair);
        outgoing.write_all(b"ok").await.unwrap();
        let mut bytes = [0; 2];
        incoming.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ok");
        a.close().await;
        b.close().await;
    }
}
