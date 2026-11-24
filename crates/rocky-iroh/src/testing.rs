use crate::discovery::{entries, rendezvous_key};
use iroh::EndpointId;
use iroh_dns::pkarr::SignedPacket;
use rocky_core::net::Failure;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

pub use iroh::test_utils::run_relay_server;

type Packets = Arc<Mutex<HashMap<EndpointId, SignedPacket>>>;
type Stored = Arc<tokio::sync::watch::Sender<u64>>;
/// A local pkarr lookup server for tests, stopped on drop.
pub struct LookupServer {
    /// The pkarr URL for `Options::lookup`.
    pub url: String,
    packets: Packets,
    stored: Stored,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for LookupServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl LookupServer {
    /// Starts the server on a free loopback port.
    pub async fn start() -> Result<Self, Failure> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/pkarr", listener.local_addr()?);
        let packets = Packets::default();
        let stored = Stored::new(tokio::sync::watch::channel(0).0);
        let (served, storing) = (packets.clone(), stored.clone());
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (packets, stored) = (served.clone(), storing.clone());
                tokio::spawn(async move {
                    let _ = serve(stream, packets, stored).await;
                });
            }
        });
        Ok(Self {
            url,
            packets,
            stored,
            task,
        })
    }
    /// Waits until a topic's rendezvous record lists at least `contacts` contacts.
    pub async fn wait_for_contacts(&self, topic: &[u8; 32], contacts: usize) {
        let mut stored = self.stored.subscribe();
        while self.contacts(topic) < contacts {
            if stored.changed().await.is_err() {
                return;
            }
        }
    }
    /// The keys of every published record.
    pub fn keys(&self) -> Vec<EndpointId> {
        self.packets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }
    /// Contacts a topic's rendezvous record lists now.
    pub fn contacts(&self, topic: &[u8; 32]) -> usize {
        self.packets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&rendezvous_key(topic).public())
            .map_or(0, |packet| entries(topic, packet).len())
    }
    /// The raw bytes of every stored record, to check what the server can read.
    pub fn stored(&self) -> Vec<u8> {
        self.packets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .flat_map(|packet| packet.as_bytes().to_vec())
            .collect()
    }
}
async fn serve(stream: TcpStream, packets: Packets, stored: Stored) -> Result<(), Failure> {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    stream.read_line(&mut line).await?;
    let mut request = line.split_whitespace();
    let (method, path) = (
        request.next().unwrap_or_default().to_owned(),
        request.next().unwrap_or_default().to_owned(),
    );
    let mut length = 0;
    loop {
        line.clear();
        stream.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse()?;
        }
    }
    if length > SignedPacket::MAX_BYTES {
        return Err("oversized".into());
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    let key = path
        .strip_prefix("/pkarr/")
        .and_then(|key| EndpointId::from_z32(key).ok());
    let (status, body) = match (method.as_str(), key) {
        ("PUT", Some(key)) => match SignedPacket::from_relay_payload(&key, &body) {
            Ok(packet) => {
                let mut packets = packets.lock().unwrap_or_else(PoisonError::into_inner);
                if packets
                    .get(&key)
                    .is_none_or(|held| packet.more_recent_than(held))
                {
                    packets.insert(key, packet);
                    stored.send_modify(|count| *count += 1);
                }
                ("204 No Content", Vec::new())
            }
            Err(_) => ("400 Bad Request", Vec::new()),
        },
        ("GET", Some(key)) => {
            let packets = packets.lock().unwrap_or_else(PoisonError::into_inner);
            match packets.get(&key) {
                Some(packet) => ("200 OK", packet.to_relay_payload()),
                None => ("404 Not Found", Vec::new()),
            }
        }
        _ => ("400 Bad Request", Vec::new()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let stream = stream.get_mut();
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;
    Ok(())
}
