//! Mutually authenticated sessions with per-frame and per-session byte
//! limits. The [live link](link) handshake runs over any reliable ordered
//! byte stream; every request runs on one of its channels. A peer is admitted
//! by its credential chain and the revocation set, which the handshake
//! exchanges; see `docs/decisions/0051-roster-free-admission.md`.
//!
//! Each message family has its module, with its requests, the client calls
//! that send them and the answers that serve them.
use super::*;
use p2panda_core::Signature;
use rand::{RngCore, rngs::OsRng};
use rocky_core::{Commitment, ResourceScope, RowApplication};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};

mod chains;
mod checkpoints;
mod contacts;
pub mod link;
pub use contacts::{ContactFuture, ContactHandler, ContactHint, ContactRequest, ContactResponse};
mod delivery;
mod entries;
mod existence;
mod frontiers;
mod held;
use held::HeldMessage;
pub(crate) mod holdings;
mod horizon;
mod horizon_relay;
use horizon_relay::HorizonRelayMessage;
mod revocations;
mod rows;
mod tips;
use chains::{ChainMessage, ChainTable};
use checkpoints::CheckpointMessage;
use contacts::ContactMessage;
use delivery::DeliveryMessage;
pub use delivery::{DELIVERY_PAGE, Delivered, Deliveries, DeliveryFuture, Held, Outgoing};
use entries::EntryMessage;
use existence::ExistenceMessage;
pub use frontiers::FrontierExchange;
use frontiers::FrontierMessage;
use holdings::HoldingMessage;
use horizon::HorizonMessage;
use revocations::RevocationMessage;
use rows::RowMessage;
pub use rows::{CatchUp, Pulled, Pushed, ScopeSync};
use tips::TipMessage;

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
/// Every failed proof of the peer's key, another handshake version included.
const AUTHENTICATION_FAILED: &str = "link authentication failed";
/// The version of a pinned contact reference's proof.
const RESOLUTION_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Hello {
    version: u32,
    node: PublicKey,
    challenge: [u8; 32],
    /// The tip the sender follows; its chain names none.
    tip: rocky_core::TipId,
    credentials: CredentialChain,
    exchange: [u8; 32],
}
/// The label both sides sign the two hellos under.
fn session_label(version: u32) -> String {
    format!("rocky.link.v{version}")
}
/// What a session signs of each message: its binding, which holds both
/// hellos with their credential chains, the sequence and the message, each
/// by digest. The signature hashes 32 bytes however large the message.
fn frame_signing_bytes(
    binding: &Commitment,
    sequence: u64,
    bytes: &[u8],
) -> Result<Vec<u8>, Error> {
    let body = rocky_core::commitment("rocky.native-frame-body.v2", bytes);
    signing_bytes(
        "rocky.native-frame.v2",
        &(
            serde_bytes::Bytes::new(binding),
            sequence,
            serde_bytes::Bytes::new(&body),
        ),
    )
}
/// The session binding as each message's signature names it.
fn frame_binding(binding: &[u8]) -> Commitment {
    rocky_core::commitment("rocky.native-frame-binding.v2", binding)
}
#[derive(Serialize, Deserialize)]
struct SignedMessage {
    sequence: u64,
    #[serde(with = "serde_bytes")]
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
    HorizonRelay(HorizonRelayMessage),
    Frontiers(FrontierMessage),
    Existence(ExistenceMessage),
    Revocations(RevocationMessage),
    Holdings(HoldingMessage),
    Contacts(ContactMessage),
    Delivery(DeliveryMessage),
    Chains(ChainMessage),
    LinkState(Vec<(ResourceScope, Commitment)>),
    Entries(EntryMessage),
    HeldChanges(HeldMessage),
    Tips(TipMessage),
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
    HorizonRelay(HorizonRelayMessage),
    Frontiers(FrontierMessage),
    Existence(ExistenceMessage),
    Revocations(RevocationMessage),
    Holdings(HoldingMessage),
    Contacts(ContactMessage),
    Delivery(DeliveryMessage),
    Chains(ChainMessage),
    Entries(EntryMessage),
    HeldChanges(HeldMessage),
    Tips(TipMessage),
);

/// Which side of the handshake this is: the dialer sends its hello first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Initiator,
    Acceptor,
}

/// Session signatures bind fresh mutual challenges, credentials and frame sequence.
/// Confidentiality is provided by encrypted payloads; routing and credentials are visible.
enum SessionIo {
    Native(Box<dyn SessionStream>),
    Channel(Box<link::channels::Io>),
}
pub struct NativeSession {
    stream: SessionIo,
    local: PrivateKey,
    peer: PublicKey,
    credentials: CredentialChain,
    /// `credentials` with its digest, as the replica records it.
    peer_chain: crate::wire::Chain,
    chains: ChainTable,
    /// The encoded start of every message's signed bytes.
    signing: Commitment,
    binding: Commitment,
    proved: std::collections::BTreeSet<ResourceScope>,
    offered: std::collections::BTreeSet<ResourceScope>,
    served_frames: std::collections::BTreeMap<Hash, ResourceScope>,
    /// A coordinated entry the peer is sending page by page.
    entry: Option<entries::Receiving>,
    tip_incoming: Vec<crate::TipStatement>,
    contact_requests: usize,
    contact_handed: usize,
    contact_received: usize,
    contact_key: Commitment,
    sent: u64,
    received: u64,
    remaining: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvePeer {
    version: u32,
    challenge: Commitment,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedPeer {
    version: u32,
    challenge: Commitment,
    node: PublicKey,
    signature: Signature,
}
/// A challenge for the node behind a pinned contact reference, and the
/// request carrying it. The answer proves the key without opening a session.
fn peer_resolution_request() -> Result<(Vec<u8>, Commitment), Error> {
    let mut challenge = [0; 32];
    OsRng.fill_bytes(&mut challenge);
    let request = encode(&ResolvePeer {
        version: RESOLUTION_VERSION,
        challenge,
    })?;
    Ok((request, challenge))
}
/// The node key a resolution answer proves for `expected`.
fn resolved_peer(
    reply: &[u8],
    challenge: Commitment,
    expected: rocky_credentials::PeerId,
) -> Result<PublicKey, Error> {
    let proof: ResolvedPeer = decode(reply)?;
    let bytes = signing_bytes(
        "rocky.peer-resolution.v1",
        &(proof.version, proof.challenge, proof.node),
    )?;
    if proof.version != RESOLUTION_VERSION
        || proof.challenge != challenge
        || rocky_credentials::PeerId::of(proof.node) != expected
        || !proof.node.verify(&bytes, &proof.signature)
    {
        return Err(denied(
            "peer resolution does not prove the pinned reference",
        ));
    }
    Ok(proof.node)
}
impl NativeSession {
    /// Proves which node answers `stream`, a link dialed to a pinned contact
    /// reference, without opening the link or revealing credentials.
    pub async fn resolve_peer(
        mut stream: rocky_core::net::Stream,
        expected: rocky_credentials::PeerId,
    ) -> Result<PublicKey, Error> {
        let (request, challenge) = peer_resolution_request()?;
        write_frame(&mut stream, &request).await?;
        let node = resolved_peer(&read_frame(&mut stream).await?, challenge, expected)?;
        stream.shutdown().await.map_err(Error::Io)?;
        Ok(node)
    }
    /// Answers a node-reference challenge without opening a session or revealing credentials.
    pub fn peer_resolution_reply(
        bytes: &[u8],
        local: &PrivateKey,
    ) -> Result<Option<Vec<u8>>, Error> {
        let Ok(request) = decode::<ResolvePeer>(bytes) else {
            return Ok(None);
        };
        if request.version != RESOLUTION_VERSION {
            return Err(denied("unsupported peer resolution version"));
        }
        let node = local.public_key();
        let bytes = signing_bytes(
            "rocky.peer-resolution.v1",
            &(request.version, request.challenge, node),
        )?;
        Ok(Some(encode(&ResolvedPeer {
            version: request.version,
            challenge: request.challenge,
            node,
            signature: local.sign(&bytes),
        })?))
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
        (version, resolve): (u32, bool),
    ) -> Result<Option<Self>, Error> {
        if byte_budget == 0 || byte_budget > MAX_SECURE_BYTES {
            return Err(denied("session peer/budget"));
        }
        let own = local.public_key();
        let mut challenge = [0; 32];
        OsRng.fill_bytes(&mut challenge);
        let (exchange_secret, exchange) = rocky_credentials::Exchange::new();
        let hello = Hello {
            version,
            node: own,
            challenge,
            tip: replica.followed_tip().await,
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
        if side == Side::Acceptor
            && resolve
            && let Some(proof) = Self::peer_resolution_reply(&peer_bytes, &local)?
        {
            write_frame(&mut stream, &proof).await?;
            stream.shutdown().await.map_err(Error::Io)?;
            return Ok(None);
        }
        let peer: Hello = decode(&peer_bytes).map_err(|_| denied(AUTHENTICATION_FAILED))?;
        if peer.version != version {
            return Err(denied(AUTHENTICATION_FAILED));
        }
        if peer.node == own || expected.is_some_and(|expected| peer.node != expected) {
            return Err(denied("unexpected link peer"));
        }
        if side == Side::Acceptor {
            // An outsider is shown no chain: the acceptor speaks only once
            // the initiator's chain verified.
            replica
                .admit_peer(peer.node, &peer.credentials, peer.tip)
                .await?;
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
            replica
                .admit_peer(peer.node, &peer.credentials, peer.tip)
                .await?;
        }
        let shared = exchange_secret.shared(&peer.exchange).map_err(credential)?;
        let contact_key = rocky_core::commitment(
            "rocky.discovery-session-key.v1",
            &encode(&(shared, &binding))?,
        );
        let mut session = Self {
            stream: SessionIo::Native(stream),
            local,
            peer: peer.node,
            peer_chain: crate::wire::Chain::new(peer.credentials.clone())?,
            chains: Default::default(),
            credentials: peer.credentials,
            signing: frame_binding(&binding),
            binding: rocky_core::commitment("rocky.holding-session.v1", &binding),
            proved: Default::default(),
            offered: Default::default(),
            served_frames: Default::default(),
            entry: None,
            tip_incoming: Vec::new(),
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
                .revoking_holder(&hello.credentials)
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
        session
            .exchange_tips(replica, side == Side::Initiator)
            .await?;
        Ok(Some(session))
    }
    pub fn peer(&self) -> PublicKey {
        self.peer
    }
    /// Serves until the peer ends the channel, then ends this side too, so
    /// a peer awaiting `close` returns.
    #[cfg(test)]
    pub(crate) async fn serve_to_end<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        let served = self.serve(replica).await;
        if let SessionIo::Channel(channel) = &mut self.stream {
            channel.close().await?;
        }
        served
    }
    /// The same session with a smaller byte budget.
    #[cfg(test)]
    pub(crate) fn with_budget(mut self, bytes: usize) -> Self {
        self.remaining = self.remaining.min(bytes);
        self
    }
    /// Ends the session: the peer reads the end of the stream after the
    /// last message. Transports that reset unfinished streams need this.
    pub async fn close(mut self) -> Result<(), Error> {
        match &mut self.stream {
            SessionIo::Channel(channel) => channel.close().await,
            SessionIo::Native(stream) => timeout(TIMEOUT, stream.shutdown())
                .await
                .map_err(|_| denied("native write timeout"))?
                .map_err(Error::Io),
        }
    }
    async fn send(&mut self, message: impl Into<Message>) -> Result<(), Error> {
        let bytes = encode(&message.into())?;
        let signature = self
            .local
            .sign(&frame_signing_bytes(&self.signing, self.sent, &bytes)?);
        let frame = encode(&SignedMessage {
            sequence: self.sent,
            bytes,
            signature,
        })?;
        self.spend(frame.len())?;
        match &mut self.stream {
            SessionIo::Channel(channel) => channel.send(frame).await?,
            SessionIo::Native(stream) => write_frame(stream, &frame).await?,
        }
        self.sent = self
            .sent
            .checked_add(1)
            .ok_or_else(|| denied("session sequence exhausted"))?;
        Ok(())
    }
    async fn receive(&mut self) -> Result<Message, Error> {
        let bytes = match &mut self.stream {
            SessionIo::Channel(channel) => channel.receive().await?,
            SessionIo::Native(stream) => read_frame(stream).await?,
        };
        self.spend(bytes.len())?;
        let message: SignedMessage = decode(&bytes)?;
        if message.sequence != self.received
            || !self.peer.verify(
                &frame_signing_bytes(&self.signing, message.sequence, &message.bytes)?,
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
    /// The peer's answer; the chains it carries first join the table.
    async fn answered(&mut self) -> Result<Message, Error> {
        loop {
            match self.receive().await? {
                Message::Chains(message) => self.learn_chains(message),
                message => return Ok(message),
            }
        }
    }
    /// The next message, which must belong to family `T`.
    async fn reply<T: TryFrom<Message, Error = Error>>(&mut self) -> Result<T, Error> {
        self.answered().await?.try_into()
    }
    /// Fails with `refusal` unless the peer acknowledges.
    async fn acknowledged(&mut self, refusal: &str) -> Result<(), Error> {
        match self.answered().await? {
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
        // Chains a push carries first are not a request of their own.
        let message = self.answered().await?;
        self.answer(replica, message).await
    }
    async fn answer<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: Message,
    ) -> Result<(), Error> {
        replica
            .record_horizon_chain(self.peer, &self.peer_chain)
            .await?;
        self.guard_message(replica, &message).await?;
        match message {
            Message::Holdings(message) => self.answer_holdings(replica, message).await,
            Message::Rows(message) => self.answer_rows(replica, message).await,
            Message::Checkpoints(message) => self.answer_checkpoints(replica, message).await,
            Message::Horizon(message) => self.answer_horizon(replica, message).await,
            Message::HorizonRelay(message) => self.answer_horizon_relay(replica, message).await,
            Message::Frontiers(message) => self.answer_frontiers(replica, message).await,
            Message::Existence(message) => self.answer_existence(replica, message).await,
            Message::Entries(message) => self.answer_entries(replica, message).await,
            Message::HeldChanges(message) => self.answer_held(replica, message).await,
            Message::Tips(message) => self.answer_tips(replica, message).await,
            Message::Delivery(_) => Err(denied("this peer serves no envelope delivery")),
            _ => Err(denied("unexpected native request")),
        }
    }
}
async fn write_frame(stream: &mut impl SessionStream, bytes: &[u8]) -> Result<(), Error> {
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
async fn read_frame(stream: &mut impl SessionStream) -> Result<Vec<u8>, Error> {
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
    fn a_message_signature_binds_session_sequence_and_body_and_the_body_travels_as_bytes() {
        let binding = frame_binding(&[7; 4096]);
        let body = vec![200u8; 70_000];
        let signed = frame_signing_bytes(&binding, 5, &body).unwrap();
        assert!(signed.len() < 128);
        let mut other = body.clone();
        other[69_999] = 0;
        for changed in [
            frame_signing_bytes(&frame_binding(&[8; 4096]), 5, &body),
            frame_signing_bytes(&binding, 6, &body),
            frame_signing_bytes(&binding, 5, &other),
        ] {
            assert_ne!(changed.unwrap(), signed);
        }
        let key = PrivateKey::new();
        let frame = encode(&SignedMessage {
            sequence: 5,
            signature: key.sign(&signed),
            bytes: body,
        })
        .unwrap();
        assert!(frame.len() < 70_200, "{}", frame.len());
    }
}
