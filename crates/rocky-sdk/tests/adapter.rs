//! A transport defined only by the chunk contract, as a host language
//! supplies one: enrollment and sync run over it, and it stays the
//! application's. See `docs/runtime/sdk-transports.md`.
use common::{Device, Failure};
use rocky_sdk::{
    Network,
    transport::{
        BoxFuture, Transport,
        adapter::{ChunkStream, ChunkTransport, adapt},
    },
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::mpsc;

mod common;

type Inbound = (String, Arc<dyn ChunkStream>);
/// One direction pair of a stream: two small queues of chunks.
struct Half {
    incoming: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
    outgoing: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    rest: Mutex<Vec<u8>>,
    closes: Arc<AtomicUsize>,
}
impl ChunkStream for Half {
    fn read(&self, max: u32) -> BoxFuture<'_, Result<Vec<u8>, Failure>> {
        Box::pin(async move {
            let mut rest = std::mem::take(&mut *self.rest.lock().map_err(|_| "poisoned")?);
            if rest.is_empty() {
                rest = self.incoming.lock().await.recv().await.unwrap_or_default();
            }
            let tail = rest.split_off(rest.len().min(max as usize));
            *self.rest.lock().map_err(|_| "poisoned")? = tail;
            Ok(rest)
        })
    }
    fn write(&self, bytes: Vec<u8>) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move {
            let sender = self.outgoing.lock().map_err(|_| "poisoned")?.clone();
            Ok(sender.ok_or("finished")?.send(bytes).await?)
        })
    }
    fn finish(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move {
            self.outgoing.lock().map_err(|_| "poisoned")?.take();
            Ok(())
        })
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.closes.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut outgoing) = self.outgoing.lock() {
                outgoing.take();
            }
            self.incoming.lock().await.close();
        })
    }
}
#[derive(Default)]
struct Wires {
    listeners: Mutex<HashMap<String, mpsc::Sender<Inbound>>>,
    opened: AtomicUsize,
    closes: Arc<AtomicUsize>,
}
struct Wire {
    wires: Arc<Wires>,
    name: String,
    inbound: tokio::sync::Mutex<mpsc::Receiver<Inbound>>,
}
fn wire(wires: &Arc<Wires>, name: &str) -> Arc<dyn Transport> {
    let (sender, receiver) = mpsc::channel(8);
    wires
        .listeners
        .lock()
        .expect("wires")
        .insert(name.into(), sender);
    adapt(Arc::new(Wire {
        wires: wires.clone(),
        name: name.into(),
        inbound: tokio::sync::Mutex::new(receiver),
    }))
}
impl ChunkTransport for Wire {
    fn kind(&self) -> String {
        "wire".into()
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async { Ok(self.name.clone()) })
    }
    fn connect(
        &self,
        address: String,
        protocol: String,
    ) -> BoxFuture<'_, Result<Arc<dyn ChunkStream>, Failure>> {
        Box::pin(async move {
            let listener = self
                .wires
                .listeners
                .lock()
                .map_err(|_| "poisoned")?
                .get(&address)
                .cloned()
                .ok_or("unreachable")?;
            // Two chunks in flight per direction: writers wait for readers.
            let (near_out, far_in) = mpsc::channel(2);
            let (far_out, near_in) = mpsc::channel(2);
            let half = |incoming, outgoing| {
                Arc::new(Half {
                    incoming: tokio::sync::Mutex::new(incoming),
                    outgoing: Mutex::new(Some(outgoing)),
                    rest: Mutex::default(),
                    closes: self.wires.closes.clone(),
                }) as Arc<dyn ChunkStream>
            };
            self.wires.opened.fetch_add(2, Ordering::SeqCst);
            listener
                .send((protocol, half(far_in, far_out)))
                .await
                .map_err(|_| "unreachable")?;
            Ok(half(near_in, near_out))
        })
    }
    fn accept(&self) -> BoxFuture<'_, Result<Option<Inbound>, Failure>> {
        Box::pin(async { Ok(self.inbound.lock().await.recv().await) })
    }
}

#[tokio::test]
async fn a_chunk_transport_carries_enrollment_and_sync() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let wires = Arc::new(Wires::default());
    let (first, second) = (wire(&wires, "a"), wire(&wires, "b"));
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Borrowed(first.clone())),
    )?;
    assert_eq!(a.ok(json!({"type": "domain"})).await["transport"], "wire");
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        Network::Borrowed(second.clone()),
    )?;
    for row in 0..20 {
        a.add(row + 1, &format!("a{row:02}")).await;
        b.add(row + 101, &format!("b{row:02}")).await;
    }
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(a.items().await.len(), 40);
    assert_eq!(a.items().await, b.items().await);
    a.close().await;
    b.close().await;

    // Every stream was closed exactly once; the transports are still the
    // application's and carry the reopened databases.
    for _ in 0..1_000 {
        if wires.closes.load(Ordering::SeqCst) == wires.opened.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert_eq!(
        wires.closes.load(Ordering::SeqCst),
        wires.opened.load(Ordering::SeqCst)
    );
    let a = Device::open(&root.path().join("a"), Some(Network::Borrowed(first)))?;
    let b = Device::open(&root.path().join("b"), Some(Network::Borrowed(second)))?;
    a.add(200, "late").await;
    b.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(b.items().await.len(), 41);
    a.close().await;
    b.close().await;
    Ok(())
}
