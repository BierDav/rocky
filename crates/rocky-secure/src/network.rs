use super::*;
use p2panda_core::Signature;
use rocky_core::{Commitment, ResourceScope, RowApplication};
use rocky_platform::time::timeout;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(test)]
mod budgets;
mod chains;
mod checkpoints;
pub use checkpoints::SealPull;
mod contacts;
pub mod link;
pub use contacts::{ContactFuture, ContactHandler, ContactHint, ContactRequest, ContactResponse};
mod delivery;
mod escrow;
mod frontiers;
mod held;
use held::HeldMessage;
pub(crate) mod holdings;
mod placements;
pub(crate) mod publication;
mod revocations;
mod rows;
use publication::PublicationMessage;
pub use publication::{PublicationOutcome, PublicationReceipt, PublicationRepairReason};
mod tips;
pub(crate) mod trace;
use chains::{ChainMessage, ChainTable};
use checkpoints::CheckpointMessage;
use contacts::ContactMessage;
use delivery::DeliveryMessage;
pub use delivery::{DELIVERY_PAGE, Delivered, Deliveries, DeliveryFuture, Held, Outgoing};
use escrow::{EscrowMessage, EscrowTable};
pub use frontiers::FrontierExchange;
use frontiers::FrontierMessage;
use holdings::HoldingMessage;
use placements::PlacementMessage;
use revocations::RevocationMessage;
use rows::RowMessage;
pub use rows::{CatchUp, Pushed, ScopeSync};
use tips::TipMessage;

pub use rocky_core::net::SessionStream;

const TIMEOUT: Duration = Duration::from_secs(10);
const WORKING_INTERVAL: Duration = Duration::from_secs(4);
const REQUEST_LIMIT: Duration = Duration::from_secs(120);
const IO_CHUNK: usize = 8 * 1024;
pub(crate) const MAX_PAGE_BYTES: usize = MAX_SECURE_FRAME / 2;
pub const PEER_REVOKED: &str = "the peer is revoked";
pub const REVOKED: &str = "this device is revoked";
#[derive(Debug)]
pub struct Revoked(pub Box<SignedRevocation>);
impl std::fmt::Display for Revoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REVOKED)
    }
}
impl std::error::Error for Revoked {}
const AUTHENTICATION_FAILED: &str = "link authentication failed";

#[derive(Serialize, Deserialize)]
struct Hello {
    version: u32,
    node: PublicKey,
    challenge: [u8; 32],
    credentials: CredentialChain,
    exchange: [u8; 32],
    clock: u64,
}
fn session_label(version: u32) -> String {
    format!("rocky.link.v{version}")
}
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointPageUnavailable;
impl std::fmt::Display for CheckpointPageUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("checkpoint page unavailable")
    }
}
impl std::error::Error for CheckpointPageUnavailable {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointSuperseded(pub &'static str);
impl std::fmt::Display for CheckpointSuperseded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for CheckpointSuperseded {}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RequestRefusal {
    NotPermitted,
    Unavailable,
    OverBudget,
    Malformed,
    WrongTip,
}
impl std::fmt::Display for RequestRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotPermitted => "request not permitted",
            Self::Unavailable => "request unavailable",
            Self::OverBudget => "request over budget",
            Self::Malformed => "malformed request",
            Self::WrongTip => "request on wrong tip",
        })
    }
}
impl std::error::Error for RequestRefusal {}
impl RequestRefusal {
    pub(crate) fn of(error: &Error) -> Self {
        match error {
            Error::Application(cause) => {
                if let Some(refusal) = cause.downcast_ref::<Self>() {
                    *refusal
                } else {
                    Self::Unavailable
                }
            }
            Error::Quota { .. } | Error::Backpressure => Self::OverBudget,
            Error::Conflict | Error::Incompatible(_) => Self::WrongTip,
            Error::Protocol(_) | Error::Corrupt(_) => Self::Malformed,
            Error::Rejected(reason) if reason.contains("quota") || reason.contains("budget") => {
                Self::OverBudget
            }
            Error::Rejected(reason)
                if reason.contains("unexpected")
                    || reason.contains("invalid")
                    || reason.contains("outside")
                    || reason.contains("incomplete") =>
            {
                Self::Malformed
            }
            Error::Rejected(reason) if reason.contains("tip") => Self::WrongTip,
            Error::Rejected(_) => Self::NotPermitted,
            _ => Self::Unavailable,
        }
    }
}
#[derive(Serialize, Deserialize)]
enum Message {
    Refused { reason: RequestRefusal },
    Acknowledged,
    Rows(RowMessage),
    Checkpoints(CheckpointMessage),
    Frontiers(FrontierMessage),
    Revocations(RevocationMessage),
    Holdings(HoldingMessage),
    Contacts(ContactMessage),
    Delivery(DeliveryMessage),
    Chains(ChainMessage),
    Escrow(EscrowMessage),
    LinkState(Vec<(ResourceScope, Commitment)>),
    HeldChanges(HeldMessage),
    Tips(TipMessage),
    Placements(PlacementMessage),
    Publication(PublicationMessage),
}
impl Message {
    fn family(&self) -> &'static str {
        match self {
            Self::Refused { .. } => "refusal",
            Self::Acknowledged => "acknowledgment",
            Self::Rows(_) => "rows",
            Self::Checkpoints(_) => "checkpoints",
            Self::Frontiers(_) => "frontiers",
            Self::Revocations(_) => "revocations",
            Self::Holdings(_) => "holdings",
            Self::Contacts(_) => "contacts",
            Self::Delivery(_) => "delivery",
            Self::Chains(_) => "chains",
            Self::Escrow(_) => "escrow",
            Self::LinkState(_) => "link state",
            Self::HeldChanges(_) => "held changes",
            Self::Tips(_) => "tips",
            Self::Placements(_) => "placements",
            Self::Publication(_) => "publication",
        }
    }
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
    Frontiers(FrontierMessage),
    Revocations(RevocationMessage),
    Holdings(HoldingMessage),
    Contacts(ContactMessage),
    Delivery(DeliveryMessage),
    Chains(ChainMessage),
    Escrow(EscrowMessage),
    HeldChanges(HeldMessage),
    Tips(TipMessage),
    Placements(PlacementMessage),
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Initiator,
    Acceptor,
}

enum SessionIo {
    Native(Box<dyn SessionStream>),
    Channel(Box<link::channels::Io>),
}
#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct SentCounts {
    pub statements: usize,
    pub ranges: usize,
    pub acknowledgments: usize,
    pub checkpoint_requests: usize,
    pub checkpoint_pages: usize,
    pub held_frames: usize,
}
pub struct NativeSession {
    #[cfg(test)]
    pub(crate) sent_counts: SentCounts,
    stream: SessionIo,
    local: PrivateKey,
    peer: PublicKey,
    credentials: CredentialChain,
    peer_chain: crate::wire::Chain,
    chains: ChainTable,
    escrow: EscrowTable,
    signing: Commitment,
    binding: Commitment,
    proved: std::collections::BTreeSet<ResourceScope>,
    offered: std::collections::BTreeSet<ResourceScope>,
    tip_incoming: Vec<crate::TipStatement>,
    contact_requests: usize,
    contact_handed: usize,
    contact_received: usize,
    contact_key: Commitment,
    sent: u64,
    received: u64,
    remaining: usize,
    span: tracing::Span,
    request: Option<trace::Request>,
    serving: bool,
}
impl NativeSession {
    async fn handshake<A: RowApplication>(
        mut stream: Box<dyn SessionStream>,
        replica: &EncryptedReplica<A>,
        (local, credentials): (PrivateKey, CredentialChain),
        (side, expected): (Side, Option<PublicKey>),
        byte_budget: usize,
        version: u32,
    ) -> Result<(Self, i64), Error> {
        if replica.private_schema().await? {
            return Err(denied("schema publication is private"));
        }
        if byte_budget == 0 || byte_budget > MAX_SECURE_BYTES {
            return Err(denied("session peer/budget"));
        }
        let own = local.public_key();
        let mut challenge = [0; 32];
        rocky_platform::entropy::fill(&mut challenge);
        let (exchange_secret, exchange) = rocky_credentials::Exchange::new();
        let hello = Hello {
            version,
            node: own,
            challenge,
            credentials,
            exchange,
            clock: micros()?,
        };
        let local_bytes = encode(&hello)?;
        let peer_bytes = match side {
            Side::Initiator => {
                write_frame(&mut stream, &local_bytes).await?;
                read_frame(&mut stream).await?
            }
            Side::Acceptor => read_frame(&mut stream).await?,
        };
        let received = micros()?;
        let peer: Hello = decode(&peer_bytes).map_err(|_| denied(AUTHENTICATION_FAILED))?;
        if peer.version != version {
            return Err(denied(AUTHENTICATION_FAILED));
        }
        if peer.node == own || expected.is_some_and(|expected| peer.node != expected) {
            return Err(denied("unexpected link peer"));
        }
        if side == Side::Acceptor {
            replica.admit_chain(peer.node, &peer.credentials).await?;
            write_frame(&mut stream, &local_bytes).await?;
        }
        let mut parts = [(hello.node, local_bytes), (peer.node, peer_bytes)];
        parts.sort_by_key(|(key, _)| *key);
        let binding = signing_bytes(&session_label(version), &parts)?;
        write_frame(&mut stream, &encode(&local.sign(&binding))?).await?;
        let signature: Signature = decode(&read_frame(&mut stream).await?)?;
        if !peer.node.verify(&binding, &signature) {
            return Err(denied(AUTHENTICATION_FAILED));
        }
        let offset = clock_offset(peer.clock, received)?;
        if side == Side::Initiator {
            replica.admit_chain(peer.node, &peer.credentials).await?;
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
            escrow: Default::default(),
            credentials: peer.credentials,
            signing: frame_binding(&binding),
            binding: rocky_core::commitment("rocky.holding-session.v1", &binding),
            proved: Default::default(),
            offered: Default::default(),
            tip_incoming: Vec::new(),
            #[cfg(test)]
            sent_counts: SentCounts::default(),
            contact_requests: 0,
            contact_handed: 0,
            contact_received: 0,
            contact_key,
            sent: 0,
            received: 0,
            remaining: byte_budget,
            span: trace::session(0),
            request: None,
            serving: false,
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
        if held.ends_chain(&hello.credentials).map_err(credential)?
            || held.ends_chain(&session.credentials).map_err(credential)?
        {
            return Err(denied("membership certificate is void"));
        }
        session
            .exchange_tips(replica, side == Side::Initiator)
            .await?;
        Ok((session, offset))
    }
    pub fn peer(&self) -> PublicKey {
        self.peer
    }
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
    #[cfg(test)]
    pub(crate) fn with_budget(mut self, bytes: usize) -> Self {
        self.remaining = self.remaining.min(bytes);
        self
    }
    pub async fn close(mut self) -> Result<(), Error> {
        match &mut self.stream {
            SessionIo::Channel(channel) => channel.close().await,
            SessionIo::Native(stream) => timeout(TIMEOUT, stream.shutdown())
                .await
                .map_err(|_| link::timed_out("native write timeout"))?
                .map_err(Error::Io),
        }
    }
    async fn send(&mut self, message: impl Into<Message>) -> Result<(), Error> {
        let message = message.into();
        #[cfg(test)]
        match &message {
            Message::Tips(
                TipMessage::Statements { statements, .. }
                | TipMessage::Push { statements, .. }
                | TipMessage::CandidatePush { statements, .. },
            ) => self.sent_counts.statements += statements.len(),
            Message::Rows(RowMessage::Range { .. }) => self.sent_counts.ranges += 1,
            Message::Rows(RowMessage::Held { .. }) => self.sent_counts.acknowledgments += 1,
            Message::Checkpoints(CheckpointMessage::CheckpointPull { .. }) => {
                self.sent_counts.checkpoint_requests += 1
            }
            Message::Checkpoints(CheckpointMessage::CheckpointRows { .. }) => {
                self.sent_counts.checkpoint_pages += 1
            }
            Message::HeldChanges(
                HeldMessage::Page { frames, .. } | HeldMessage::Push { frames, .. },
            ) => self.sent_counts.held_frames += frames.len(),
            _ => {}
        }
        let bytes = encode(&message)?;
        let signature = self
            .local
            .sign(&frame_signing_bytes(&self.signing, self.sent, &bytes)?);
        let frame = encode(&SignedMessage {
            sequence: self.sent,
            bytes,
            signature,
        })?;
        let family = message.family();
        if frame.len() > MAX_SECURE_FRAME {
            return Err(denied(&format!(
                "native {family} frame quota: {} exceeds {MAX_SECURE_FRAME} bytes",
                frame.len()
            )));
        }
        let length = frame.len();
        if let SessionIo::Channel(io) = &self.stream
            && io.publication.is_some()
        {
            let part = match &message {
                Message::Publication(message) => message.part_name(),
                _ => family,
            };
            tracing::debug!(parent: &self.span, family, part, signed_native_bytes = length, sequence = self.sent, "publication_message");
        }
        self.spend(length)?;
        if !self.serving {
            let channel = self.channel();
            self.request
                .get_or_insert_with(|| trace::Request::start(&self.span, channel, family))
                .sent(length);
        }
        let deadline = self.request.as_ref().map_or_else(
            || rocky_platform::time::Instant::now() + REQUEST_LIMIT,
            trace::Request::deadline,
        );
        let written = rocky_platform::time::timeout_at(deadline, async {
            match &mut self.stream {
                SessionIo::Channel(channel) => channel.send(frame).await,
                SessionIo::Native(stream) => write_frame(stream, &frame).await,
            }
        })
        .await;
        match written {
            Ok(result) => result?,
            Err(_) => {
                if let SessionIo::Channel(channel) = &mut self.stream {
                    channel.retire();
                }
                return Err(link::timed_out("request lifetime timeout"));
            }
        }
        #[cfg(any(test, feature = "qualification"))]
        if let SessionIo::Channel(io) = &self.stream {
            let part = io.publication.map(|_| match &message {
                Message::Publication(message) => message.part_name(),
                _ => family,
            });
            let receipt = match &message {
                Message::Publication(PublicationMessage::Receipt(receipt)) => Some(receipt),
                _ => None,
            };
            io.record_sent(family, part, length, receipt);
        }
        self.sent = self
            .sent
            .checked_add(1)
            .ok_or_else(|| denied("session sequence exhausted"))?;
        Ok(())
    }
    fn channel(&self) -> u16 {
        match &self.stream {
            SessionIo::Channel(channel) => channel.channel(),
            SessionIo::Native(_) => 0,
        }
    }
    async fn receive(&mut self) -> Result<Message, Error> {
        self.receive_with_idle(false).await
    }
    async fn receive_with_idle(&mut self, idle: bool) -> Result<Message, Error> {
        let bytes = match &mut self.stream {
            SessionIo::Channel(channel) if idle => channel.receive_with_idle(true).await,
            SessionIo::Channel(channel) => channel.receive().await,
            SessionIo::Native(stream) => read_frame(stream).await,
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) => {
                if let Some(request) = self.request.take() {
                    request.finish(error.category());
                }
                return Err(error);
            }
        };
        if let Some(request) = &mut self.request {
            request.received(bytes.len());
        }
        self.spend(bytes.len())?;
        if let SessionIo::Channel(io) = &mut self.stream
            && io.publication.is_some()
        {
            publication::charge(&bytes, &mut io.decoded)?;
        }
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
        if let SessionIo::Channel(io) = &mut self.stream
            && io.publication.is_some()
        {
            publication::charge(&message.bytes, &mut io.decoded)?;
        }
        let message = decode(&message.bytes)?;
        if let SessionIo::Channel(io) = &mut self.stream
            && io.publication.is_some()
        {
            publication::charge_metadata(&message, &mut io.decoded)?;
            publication::charge_references(&message, &self.chains, &mut io.decoded)?;
        }
        Ok(message)
    }
    async fn answered(&mut self) -> Result<Message, Error> {
        let deadline = self.request.as_ref().map_or_else(
            || rocky_platform::time::Instant::now() + REQUEST_LIMIT,
            trace::Request::deadline,
        );
        match rocky_platform::time::timeout_at(deadline, self.answered_with_idle(false)).await {
            Ok(result) => result,
            Err(_) => {
                if let SessionIo::Channel(channel) = &mut self.stream {
                    channel.retire();
                }
                if let Some(request) = self.request.take() {
                    request.finish("timeout");
                }
                Err(link::timed_out("request lifetime timeout"))
            }
        }
    }
    async fn answered_request(&mut self) -> Result<Message, Error> {
        self.answered_with_idle(true).await
    }
    async fn answered_with_idle(&mut self, idle: bool) -> Result<Message, Error> {
        loop {
            match self.receive_with_idle(idle).await? {
                Message::Chains(message) => self.learn_chains(message),
                Message::Escrow(message) => self.learn_escrow(message)?,
                Message::Refused { reason } => {
                    if let Some(request) = self.request.take() {
                        request.finish("refused");
                    }
                    return Err(Error::Application(Box::new(reason)));
                }
                message => {
                    if let Some(request) = self.request.take() {
                        request.finish("ok");
                    }
                    return Ok(message);
                }
            }
        }
    }
    async fn reply<T: TryFrom<Message, Error = Error>>(&mut self) -> Result<T, Error> {
        self.answered().await?.try_into()
    }
    async fn acknowledged(&mut self, refusal: &str) -> Result<(), Error> {
        match self.answered().await? {
            Message::Acknowledged => Ok(()),
            _ => Err(denied(refusal)),
        }
    }
    fn budget_low(&self, quota: usize) -> bool {
        self.remaining < 2 * quota.min(MAX_SECURE_FRAME) + 64 * 1024
    }
    fn spend(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| denied("native session byte budget exhausted"))?;
        Ok(())
    }
    pub async fn serve<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        if self.is_publication() {
            return self.receive_publication(replica).await.map(|_| ());
        }
        loop {
            match self.serve_once(replica).await {
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                other => other?,
            }
        }
    }
    pub async fn serve_once<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        let message = self.answered_request().await?;
        self.respond(replica, message, None, None).await
    }
    async fn respond<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: Message,
        deliveries: Option<&dyn Deliveries>,
        handler: Option<&dyn ContactHandler>,
    ) -> Result<(), Error> {
        let span = trace::serve(&self.span, self.channel(), message.family());
        let progress = match &self.stream {
            SessionIo::Channel(channel) => Some(channel.progress()),
            SessionIo::Native(_) => None,
        };
        self.serving = true;
        let result = {
            let answer = async {
                match (message, deliveries, handler) {
                    (Message::Contacts(message), _, Some(handler)) => {
                        self.answer_contact(replica, handler, message).await
                    }
                    (Message::Delivery(message), Some(deliveries), _) => {
                        self.answer_delivery(deliveries, message).await
                    }
                    (message, _, _) => self.answer(replica, message).await,
                }
            };
            let work = async {
                match progress {
                    Some(progress) => progress.run().await,
                    None => std::future::pending().await,
                }
            };
            let result = rocky_platform::time::timeout(REQUEST_LIMIT, async {
                let answer = tracing::Instrument::instrument(answer, span.clone());
                tokio::pin!(answer, work);
                tokio::select! {
                    biased;
                    result = &mut answer => result,
                    result = &mut work => result,
                }
            })
            .await
            .unwrap_or_else(|_| Err(link::timed_out("request lifetime timeout")));
            trace::outcome(&span, &result);
            result
        };
        let result = match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.tip_incoming.clear();
                self.send(Message::Refused {
                    reason: RequestRefusal::of(&error),
                })
                .await
            }
        };
        self.serving = false;
        result
    }
    async fn answer<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: Message,
    ) -> Result<(), Error> {
        self.guard_message(replica, &message).await?;
        match message {
            Message::Revocations(RevocationMessage::Exchange) => {
                self.send(Message::Acknowledged).await?;
                self.refresh_authority(replica, false).await
            }
            Message::Holdings(message) => self.answer_holdings(replica, message).await,
            Message::Rows(message) => self.answer_rows(replica, message).await,
            Message::Checkpoints(message) => self.answer_checkpoints(replica, message).await,
            Message::Frontiers(message) => self.answer_frontiers(replica, message).await,
            Message::HeldChanges(message) => self.answer_held(replica, message).await,
            Message::Tips(message) => self.answer_tips(replica, message).await,
            Message::Placements(message) => self.answer_placements(replica, message).await,
            Message::Delivery(_) => Err(denied("this peer serves no envelope delivery")),
            _ => Err(denied("unexpected native request")),
        }
    }
}
async fn read_progress(
    stream: &mut (impl AsyncRead + Unpin),
    bytes: &mut [u8],
    expired: fn() -> Error,
) -> Result<(), Error> {
    for chunk in bytes.chunks_mut(IO_CHUNK) {
        timeout(TIMEOUT, stream.read_exact(chunk))
            .await
            .map_err(|_| expired())?
            .map_err(Error::Io)?;
    }
    Ok(())
}
fn clock_offset(peer_clock: u64, received: u64) -> Result<i64, Error> {
    i64::try_from(i128::from(peer_clock) - i128::from(received))
        .map_err(|_| denied("link peer clock"))
}
async fn write_progress(
    stream: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
    expired: fn() -> Error,
) -> Result<(), Error> {
    for chunk in bytes.chunks(IO_CHUNK) {
        timeout(TIMEOUT, stream.write_all(chunk))
            .await
            .map_err(|_| expired())?
            .map_err(Error::Io)?;
    }
    Ok(())
}
async fn write_frame(stream: &mut impl SessionStream, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_SECURE_FRAME {
        return Err(denied("native frame quota"));
    }
    let expired = || link::timed_out("native write timeout");
    write_progress(stream, &(bytes.len() as u32).to_be_bytes(), expired).await?;
    write_progress(stream, bytes, expired).await?;
    timeout(TIMEOUT, stream.flush())
        .await
        .map_err(|_| expired())?
        .map_err(Error::Io)
}
async fn read_frame(stream: &mut impl SessionStream) -> Result<Vec<u8>, Error> {
    let expired = || link::timed_out("native read timeout");
    let mut header = [0; 4];
    stream
        .read_exact(&mut header[..1])
        .await
        .map_err(Error::Io)?;
    read_progress(stream, &mut header[1..], expired).await?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_SECURE_FRAME {
        return Err(denied("native frame quota"));
    }
    let mut bytes = vec![0; length];
    read_progress(stream, &mut bytes, expired).await?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(start_paused = true)]
    async fn admission_waits_while_the_peer_works_before_its_next_frame() {
        let (mut near, mut far) = tokio::io::duplex(64);
        let deliver = async {
            rocky_platform::time::sleep(TIMEOUT * 2).await;
            write_frame(&mut far, &[7]).await.unwrap();
        };
        let (received, ()) = tokio::join!(read_frame(&mut near), deliver);
        assert_eq!(received.unwrap(), vec![7]);
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(start_paused = true)]
    async fn a_started_admission_header_must_keep_making_progress() {
        let (mut near, mut far) = tokio::io::duplex(64);
        let start = rocky_platform::time::Instant::now();
        let deliver = async {
            rocky_platform::time::sleep(TIMEOUT * 2).await;
            far.write_all(&[0]).await.unwrap();
            std::future::pending::<()>().await;
        };
        tokio::pin!(deliver);
        let received = tokio::select! {
            received = read_frame(&mut near) => received,
            () = &mut deliver => unreachable!(),
        };
        let error = received.unwrap_err();
        assert!(error.to_string().contains("native read timeout"));
        assert!(
            matches!(&error, Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut),
            "a stalled peer is a timeout, not a rejection: {error:?}"
        );
        assert_eq!(
            RequestRefusal::of(&error),
            RequestRefusal::Unavailable,
            "a peer whose own link timed out does not refuse as not permitted"
        );
        assert_eq!(start.elapsed(), TIMEOUT * 3);
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn missing_checkpoint_page_is_unavailable_not_not_permitted() {
        let error = Error::Application(Box::new(CheckpointPageUnavailable));
        assert_eq!(
            RequestRefusal::of(&error),
            RequestRefusal::Unavailable,
            "a missing checkpoint page must not surface as a permission refusal"
        );
        assert_ne!(RequestRefusal::of(&error), RequestRefusal::NotPermitted);
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(start_paused = true)]
    async fn native_frame_reads_follow_bounded_chunk_progress() {
        let (mut near, mut far) = tokio::io::duplex(IO_CHUNK * 2);
        let deliver = async {
            far.write_u32((IO_CHUNK * 3) as u32).await.unwrap();
            for value in 0..3 {
                rocky_platform::time::sleep(TIMEOUT * 3 / 5).await;
                far.write_all(&vec![value; IO_CHUNK]).await.unwrap();
            }
        };
        let (received, ()) = tokio::join!(read_frame(&mut near), deliver);
        let expected: Vec<_> = (0..3).flat_map(|value| vec![value; IO_CHUNK]).collect();
        assert_eq!(received.unwrap(), expected);
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(start_paused = true)]
    async fn native_frame_writes_follow_a_slow_reader() {
        let (mut near, mut far) = tokio::io::duplex(IO_CHUNK);
        let send = async move { write_frame(&mut near, &vec![7; IO_CHUNK * 3]).await };
        let receive = async {
            assert_eq!(far.read_u32().await.unwrap() as usize, IO_CHUNK * 3);
            let mut bytes = vec![0; IO_CHUNK * 3];
            for chunk in bytes.chunks_mut(IO_CHUNK) {
                rocky_platform::time::sleep(TIMEOUT * 3 / 5).await;
                far.read_exact(chunk).await.unwrap();
            }
            bytes
        };
        let (sent, received) = tokio::join!(send, receive);
        sent.unwrap();
        assert_eq!(received, vec![7; IO_CHUNK * 3]);
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(start_paused = true)]
    async fn native_frame_trickles_still_time_out() {
        let (mut near, mut far) = tokio::io::duplex(IO_CHUNK);
        let deliver = async {
            far.write_u32(IO_CHUNK as u32).await.unwrap();
            for _ in 0..4 {
                rocky_platform::time::sleep(TIMEOUT * 3 / 10).await;
                far.write_all(&[1]).await.unwrap();
            }
        };
        let (received, ()) = tokio::join!(read_frame(&mut near), deliver);
        assert!(
            received
                .unwrap_err()
                .to_string()
                .contains("native read timeout")
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
