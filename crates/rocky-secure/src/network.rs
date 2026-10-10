//! Mutually authenticated native sessions with per-frame and per-session
//! byte limits, over any reliable ordered byte stream: TCP, a QUIC stream or
//! an in-memory pipe. A peer is admitted by its credential chain and the
//! revocation set, which the handshake exchanges; see
//! `docs/decisions/0051-roster-free-admission.md`.
//!
//! Each message family has its module, with its requests, the client calls
//! that send them and the answers that serve them.
use super::*;
use p2panda_core::Signature;
use rand::{RngCore, rngs::OsRng};
use rocky_core::{Commitment, ResourceScope, RowApplication, net::NATIVE_SESSION_VERSION};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};

mod checkpoints;
mod contacts;
pub use contacts::{ContactFuture, ContactHandler, ContactHint, ContactRequest, ContactResponse};
mod commands;
mod delivery;
mod existence;
mod frontiers;
pub(crate) mod holdings;
mod horizon;
mod revocations;
mod rows;
use checkpoints::CheckpointMessage;
use commands::CommandMessage;
use contacts::ContactMessage;
use delivery::DeliveryMessage;
pub use delivery::{DELIVERY_PAGE, Delivered, Deliveries, DeliveryFuture, Held, Outgoing};
use existence::ExistenceMessage;
pub use frontiers::FrontierExchange;
use frontiers::FrontierMessage;
use holdings::HoldingMessage;
use horizon::HorizonMessage;
use revocations::RevocationMessage;
use rows::RowMessage;
pub use rows::{CatchUp, Pulled, Pushed, ScopeSync};

/// The stream a session runs over. The transport adds no security the
/// session relies on; see `docs/runtime/permissions-and-sync.md`.
pub use rocky_core::net::SessionStream;

const TIMEOUT: Duration = Duration::from_secs(10);
/// The refusal of a peer the merged revocation set revokes.
pub const PEER_REVOKED: &str = "the peer is revoked";
/// The refusal of a side the merged revocation set revokes: it ends the
/// session. See `Revoked`.
pub const REVOKED: &str = "this device is revoked";
/// The handshake's end on a side a statement revokes, carried by
/// `Error::Application`: the statement, which the peer sent or the replica
/// held. The replica does not keep a statement that revokes itself; its
/// owner records it.
#[derive(Debug)]
pub struct Revoked(pub Box<SignedRevocation>);
impl std::fmt::Display for Revoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REVOKED)
    }
}
impl std::error::Error for Revoked {}
/// Every failed proof of the peer's key, another session version included.
const AUTHENTICATION_FAILED: &str = "native session authentication failed";

#[derive(Serialize, Deserialize)]
struct Hello {
    version: u32,
    node: PublicKey,
    challenge: [u8; 32],
    credentials: CredentialChain,
    exchange: [u8; 32],
}
/// The label both sides sign the two hellos under.
fn session_label(version: u32) -> String {
    format!("rocky.native-session.v{version}")
}
/// What a session signs of each message is
/// `signing_bytes("rocky.native-frame.v1", &(binding, sequence, bytes))`.
/// A `Vec<u8>` encodes as an array of integers, which the generic encoder
/// writes one element at a time, and the binding holds both hellos with
/// their credential chains: encoding it for every message cost more than
/// the signature. The session encodes it once, up to the sequence.
fn frame_signing_head(binding: &Vec<u8>) -> Result<Vec<u8>, Error> {
    let mut head = signing_bytes("rocky.native-frame.v1", &(binding, 0u64, &Vec::<u8>::new()))?;
    // Sequence 0 and the empty message are the last two bytes.
    if head.split_off(head.len().saturating_sub(2)) != [0x00, 0x80] {
        return Err(Error::Corrupt("native frame signing head".into()));
    }
    Ok(head)
}
/// The signed bytes of one message, byte for byte as `signing_bytes`
/// encodes them after `frame_signing_head`.
fn frame_signing_bytes(head: &[u8], sequence: u64, bytes: &[u8]) -> Vec<u8> {
    let mut signed = Vec::with_capacity(head.len() + 18 + 2 * bytes.len());
    signed.extend_from_slice(head);
    cbor_head(&mut signed, 0, sequence);
    cbor_head(&mut signed, 4, bytes.len() as u64);
    for byte in bytes {
        if *byte < 24 {
            signed.push(*byte);
        } else {
            signed.extend_from_slice(&[24, *byte]);
        }
    }
    signed
}
/// A CBOR head in its shortest form, as the canonical encoder writes it.
fn cbor_head(out: &mut Vec<u8>, major: u8, value: u64) {
    let major = major << 5;
    if let Ok(value) = u8::try_from(value) {
        if value < 24 {
            out.push(major | value);
        } else {
            out.extend([major | 24, value]);
        }
    } else if let Ok(value) = u16::try_from(value) {
        out.push(major | 25);
        out.extend(value.to_be_bytes());
    } else if let Ok(value) = u32::try_from(value) {
        out.push(major | 26);
        out.extend(value.to_be_bytes());
    } else {
        out.push(major | 27);
        out.extend(value.to_be_bytes());
    }
}

#[derive(Serialize, Deserialize)]
struct SignedMessage {
    sequence: u64,
    bytes: Vec<u8>,
    signature: Signature,
}
/// Every message of the session, by family.
#[derive(Serialize, Deserialize)]
enum Message {
    Acknowledged,
    Rows(RowMessage),
    Checkpoints(CheckpointMessage),
    Horizon(HorizonMessage),
    Frontiers(FrontierMessage),
    Existence(ExistenceMessage),
    Commands(CommandMessage),
    Revocations(RevocationMessage),
    Holdings(HoldingMessage),
    Contacts(ContactMessage),
    Delivery(DeliveryMessage),
}
macro_rules! families {
    ($($variant:ident($family:ident)),* $(,)?) => {$(
        impl From<$family> for Message {
            fn from(message: $family) -> Self {
                Self::$variant(message)
            }
        }
        impl TryFrom<Message> for $family {
            type Error = Error;
            fn try_from(message: Message) -> Result<Self, Error> {
                match message {
                    Message::$variant(message) => Ok(message),
                    _ => Err(denied("unexpected native response")),
                }
            }
        }
    )*};
}
families!(
    Rows(RowMessage),
    Checkpoints(CheckpointMessage),
    Horizon(HorizonMessage),
    Frontiers(FrontierMessage),
    Existence(ExistenceMessage),
    Commands(CommandMessage),
    Revocations(RevocationMessage),
    Holdings(HoldingMessage),
    Contacts(ContactMessage),
    Delivery(DeliveryMessage),
);

/// Which side of the handshake this is: the dialer sends its hello first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Initiator,
    Acceptor,
}

/// Session signatures bind fresh mutual challenges, credentials and frame sequence.
/// Confidentiality is provided by encrypted payloads; routing and credentials are visible.
pub struct NativeSession {
    stream: Box<dyn SessionStream>,
    local: PrivateKey,
    peer: PublicKey,
    credentials: CredentialChain,
    /// The encoded start of every message's signed bytes.
    signing: Vec<u8>,
    binding: Commitment,
    proved: std::collections::BTreeSet<ResourceScope>,
    offered: std::collections::BTreeSet<ResourceScope>,
    served_frames: std::collections::BTreeMap<Hash, ResourceScope>,
    contact_requests: usize,
    contact_handed: usize,
    contact_received: usize,
    contact_key: Commitment,
    sent: u64,
    received: u64,
    remaining: usize,
}
impl NativeSession {
    /// Authenticates `expected_peer` over `stream`, admits it by its chain
    /// and the merged revocation set, and leaves `replica` holding the
    /// statements the peer sent.
    pub async fn authenticate<A: RowApplication>(
        stream: impl SessionStream + 'static,
        replica: &EncryptedReplica<A>,
        local: PrivateKey,
        credentials: CredentialChain,
        expected_peer: PublicKey,
        byte_budget: usize,
    ) -> Result<Self, Error> {
        if expected_peer == local.public_key() {
            return Err(denied("native session peer/budget"));
        }
        Self::handshake(
            Box::new(stream),
            replica,
            (local, credentials),
            (Side::Initiator, Some(expected_peer)),
            byte_budget,
            NATIVE_SESSION_VERSION,
        )
        .await
    }
    /// Authenticates whichever peer dialed, for a node that learns its peer
    /// only from the handshake. The peer is admitted when its chain verifies
    /// for this domain and no statement of the merged set revokes it; every
    /// request is still authorized by its grants.
    pub async fn accept<A: RowApplication>(
        stream: impl SessionStream + 'static,
        replica: &EncryptedReplica<A>,
        local: PrivateKey,
        credentials: CredentialChain,
        byte_budget: usize,
    ) -> Result<Self, Error> {
        Self::handshake(
            Box::new(stream),
            replica,
            (local, credentials),
            (Side::Acceptor, None),
            byte_budget,
            NATIVE_SESSION_VERSION,
        )
        .await
    }
    /// The initiator's hello, then the acceptor's once it verified the
    /// initiator's chain, then both signatures; the initiator then verifies
    /// the acceptor's chain. Both exchange the revocation statements the
    /// other lacks and check the merged set last.
    async fn handshake<A: RowApplication>(
        mut stream: Box<dyn SessionStream>,
        replica: &EncryptedReplica<A>,
        (local, credentials): (PrivateKey, CredentialChain),
        (side, expected): (Side, Option<PublicKey>),
        byte_budget: usize,
        version: u32,
    ) -> Result<Self, Error> {
        if byte_budget == 0 || byte_budget > MAX_SECURE_BYTES {
            return Err(denied("native session peer/budget"));
        }
        let own = local.public_key();
        let mut challenge = [0; 32];
        OsRng.fill_bytes(&mut challenge);
        let (exchange_secret, exchange) = rocky_credentials::Exchange::new();
        let hello = Hello {
            version,
            node: own,
            challenge,
            credentials,
            exchange,
        };
        let local_bytes = encode(&hello)?;
        let peer_bytes = match side {
            Side::Initiator => {
                write_frame(&mut stream, &local_bytes).await?;
                read_frame(&mut stream).await?
            }
            Side::Acceptor => read_frame(&mut stream).await?,
        };
        let peer: Hello = decode(&peer_bytes).map_err(|_| denied(AUTHENTICATION_FAILED))?;
        if peer.version != version {
            return Err(denied(AUTHENTICATION_FAILED));
        }
        if peer.node == own || expected.is_some_and(|expected| peer.node != expected) {
            return Err(denied("unexpected native peer"));
        }
        if side == Side::Acceptor {
            // An outsider is shown no chain: the acceptor speaks only once
            // the initiator's chain verified.
            replica.admit_chain(peer.node, &peer.credentials).await?;
            write_frame(&mut stream, &local_bytes).await?;
        }
        let mut parts = [(hello.node, local_bytes), (peer.node, peer_bytes)];
        parts.sort_by_key(|(key, _)| *key);
        // The label carries the version: every other session is refused.
        let binding = signing_bytes(&session_label(version), &parts)?;
        write_frame(&mut stream, &encode(&local.sign(&binding))?).await?;
        let signature: Signature = decode(&read_frame(&mut stream).await?)?;
        if !peer.node.verify(&binding, &signature) {
            return Err(denied(AUTHENTICATION_FAILED));
        }
        if side == Side::Initiator {
            replica.admit_chain(peer.node, &peer.credentials).await?;
        }
        let shared = exchange_secret.shared(&peer.exchange).map_err(credential)?;
        let contact_key = rocky_core::commitment(
            "rocky.discovery-session-key.v1",
            &encode(&(shared, &binding))?,
        );
        let mut session = Self {
            stream,
            local,
            peer: peer.node,
            credentials: peer.credentials,
            signing: frame_signing_head(&binding)?,
            binding: rocky_core::commitment("rocky.holding-session.v1", &binding),
            proved: Default::default(),
            offered: Default::default(),
            served_frames: Default::default(),
            contact_requests: 0,
            contact_handed: 0,
            contact_received: 0,
            contact_key,
            sent: 0,
            received: 0,
            remaining: byte_budget,
        };
        let sent = session
            .exchange_revocations(replica, &hello.credentials, side == Side::Initiator)
            .await?;
        let held = replica.revocations().await;
        let own = match sent {
            Some(statement) => Some(statement),
            None => held
                .revoking(&hello.credentials)
                .map_err(credential)?
                .cloned(),
        };
        if let Some(statement) = own {
            return Err(Error::Application(Box::new(Revoked(Box::new(statement)))));
        }
        if held
            .revoking(&session.credentials)
            .map_err(credential)?
            .is_some()
        {
            return Err(denied(PEER_REVOKED));
        }
        Ok(session)
    }
    /// A handshake under another session version, as another build runs.
    #[cfg(test)]
    pub(crate) async fn authenticate_as_version<A: RowApplication>(
        stream: impl SessionStream + 'static,
        replica: &EncryptedReplica<A>,
        (local, credentials): (PrivateKey, CredentialChain),
        expected_peer: PublicKey,
        version: u32,
    ) -> Result<Self, Error> {
        Self::handshake(
            Box::new(stream),
            replica,
            (local, credentials),
            (Side::Initiator, Some(expected_peer)),
            MAX_SECURE_BYTES,
            version,
        )
        .await
    }
    pub fn peer(&self) -> PublicKey {
        self.peer
    }
    /// What remains of the byte budget.
    #[cfg(test)]
    pub(crate) fn remaining(&self) -> usize {
        self.remaining
    }
    /// Ends the session: the peer reads the end of the stream after the
    /// last message. Transports that reset unfinished streams need this.
    pub async fn close(mut self) -> Result<(), Error> {
        timeout(TIMEOUT, self.stream.shutdown())
            .await
            .map_err(|_| denied("native write timeout"))?
            .map_err(Error::Io)
    }
    async fn send(&mut self, message: impl Into<Message>) -> Result<(), Error> {
        let bytes = encode(&message.into())?;
        let signature = self
            .local
            .sign(&frame_signing_bytes(&self.signing, self.sent, &bytes));
        let frame = encode(&SignedMessage {
            sequence: self.sent,
            bytes,
            signature,
        })?;
        self.spend(frame.len())?;
        write_frame(&mut self.stream, &frame).await?;
        self.sent = self
            .sent
            .checked_add(1)
            .ok_or_else(|| denied("session sequence exhausted"))?;
        Ok(())
    }
    async fn receive(&mut self) -> Result<Message, Error> {
        let bytes = read_frame(&mut self.stream).await?;
        self.spend(bytes.len())?;
        let message: SignedMessage = decode(&bytes)?;
        if message.sequence != self.received
            || !self.peer.verify(
                &frame_signing_bytes(&self.signing, message.sequence, &message.bytes),
                &message.signature,
            )
        {
            return Err(denied("native frame signature/sequence"));
        }
        self.received = self
            .received
            .checked_add(1)
            .ok_or_else(|| denied("session sequence exhausted"))?;
        decode(&message.bytes)
    }
    /// The next message, which must belong to family `T`.
    async fn reply<T: TryFrom<Message, Error = Error>>(&mut self) -> Result<T, Error> {
        self.receive().await?.try_into()
    }
    /// Fails with `refusal` unless the peer acknowledges.
    async fn acknowledged(&mut self, refusal: &str) -> Result<(), Error> {
        match self.receive().await? {
            Message::Acknowledged => Ok(()),
            _ => Err(denied(refusal)),
        }
    }
    /// Whether the budget may not cover another exchange of `quota` bytes
    /// each way. Both peers spend what crosses the session.
    fn low(&self, quota: usize) -> bool {
        self.remaining < 2 * quota.min(MAX_SECURE_FRAME) + 64 * 1024
    }
    fn spend(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| denied("native session byte budget exhausted"))?;
        Ok(())
    }
    /// Serves requests until the peer ends the session. A peer that closes
    /// the stream between requests ends it cleanly.
    pub async fn serve<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        loop {
            match self.serve_once(replica).await {
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                other => other?,
            }
        }
    }
    /// Serve one authenticated pull or acknowledgment; callers own accept-loop shutdown.
    pub async fn serve_once<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        let message = self.receive().await?;
        self.answer(replica, message).await
    }
    async fn answer<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: Message,
    ) -> Result<(), Error> {
        replica
            .record_horizon_credentials(self.peer, &self.credentials)
            .await?;
        self.guard_message(replica, &message).await?;
        match message {
            Message::Holdings(message) => self.answer_holdings(replica, message).await,
            Message::Rows(message) => self.answer_rows(replica, message).await,
            Message::Checkpoints(message) => self.answer_checkpoints(replica, message).await,
            Message::Horizon(message) => self.answer_horizon(replica, message).await,
            Message::Frontiers(message) => self.answer_frontiers(replica, message).await,
            Message::Existence(message) => self.answer_existence(replica, message).await,
            Message::Delivery(_) => Err(denied("this peer serves no envelope delivery")),
            _ => Err(denied("unexpected native request")),
        }
    }
}
async fn write_frame(stream: &mut Box<dyn SessionStream>, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_SECURE_FRAME {
        return Err(denied("native frame quota"));
    }
    timeout(TIMEOUT, async {
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(bytes).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| denied("native write timeout"))?
    .map_err(Error::Io)
}
async fn read_frame(stream: &mut Box<dyn SessionStream>) -> Result<Vec<u8>, Error> {
    timeout(TIMEOUT, async {
        let length = stream.read_u32().await.map_err(Error::Io)? as usize;
        if length == 0 || length > MAX_SECURE_FRAME {
            return Err(denied("native frame quota"));
        }
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).await.map_err(Error::Io)?;
        Ok(bytes)
    })
    .await
    .map_err(|_| denied("native read timeout"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_signing_bytes_equal_the_generic_encoding() {
        let binding = (0..4096).map(|index| (index % 251) as u8).collect();
        let head = frame_signing_head(&binding).unwrap();
        for length in [0, 1, 23, 24, 255, 256, 65_535, 65_536, 70_000] {
            let bytes: Vec<u8> = (0..length).map(|index| (index * 31 % 256) as u8).collect();
            for sequence in [0, 23, 24, 255, 256, 65_536, 1 << 40] {
                assert_eq!(
                    frame_signing_bytes(&head, sequence, &bytes),
                    signing_bytes("rocky.native-frame.v1", &(&binding, sequence, &bytes)).unwrap(),
                    "{length} bytes at {sequence}"
                );
            }
        }
    }

    #[test]
    fn the_version_names_the_transport_and_the_label() {
        let name = rocky_core::net::Protocol::Sync.name();
        assert_eq!(
            name,
            format!("rocky/native-session/{NATIVE_SESSION_VERSION}").as_bytes()
        );
        assert_eq!(
            session_label(NATIVE_SESSION_VERSION),
            format!("rocky.native-session.v{NATIVE_SESSION_VERSION}")
        );
    }
}
