//! Bounded native request channels inheriting the authenticated link admission.
use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
};
use tokio::sync::mpsc;

const FRAGMENT: usize = 8 * 1024;
const QUEUE: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Packet {
    Open {
        generation: u64,
    },
    Data {
        generation: u64,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
        last: bool,
    },
    End {
        generation: u64,
    },
}
impl Packet {
    pub(super) fn validate(&self) -> Result<(), Error> {
        if let Self::Data { bytes, .. } = self
            && (bytes.is_empty() || bytes.len() > FRAGMENT)
        {
            return Err(denied("link fragment quota"));
        }
        Ok(())
    }
    fn generation(&self) -> u64 {
        match self {
            Self::Open { generation }
            | Self::Data { generation, .. }
            | Self::End { generation } => *generation,
        }
    }
}
#[derive(Clone)]
struct Admission {
    local: PrivateKey,
    peer: PublicKey,
    credentials: CredentialChain,
    peer_chain: crate::wire::Chain,
    binding: Commitment,
    proved: std::collections::BTreeSet<ResourceScope>,
    offered: std::collections::BTreeSet<ResourceScope>,
    contact_key: Commitment,
}
struct Slot {
    generation: u64,
    receive: Option<mpsc::Sender<Vec<u8>>>,
    bytes: Vec<u8>,
    total: usize,
    closed: bool,
}
#[derive(Default)]
struct State {
    next: u64,
    remote: u64,
    slots: BTreeMap<(bool, u16), Slot>,
}
#[derive(Clone)]
pub struct Handle {
    admission: Admission,
    direction: bool,
    state: Arc<Mutex<State>>,
    send: mpsc::Sender<Frame>,
    events: tokio::sync::watch::Sender<()>,
}
pub struct Channels {
    pub handle: Handle,
    pub outgoing: mpsc::Receiver<Frame>,
}
pub(crate) struct Io {
    handle: Handle,
    key: (bool, u16),
    generation: u64,
    receive: mpsc::Receiver<Vec<u8>>,
    closed: bool,
}
impl Channels {
    pub(super) fn new(session: &NativeSession, direction: bool) -> Self {
        let (send, outgoing) = mpsc::channel(QUEUE);
        Self {
            handle: Handle {
                admission: Admission {
                    local: session.local.clone(),
                    peer: session.peer,
                    credentials: session.credentials.clone(),
                    peer_chain: session.peer_chain.clone(),
                    binding: session.binding,
                    proved: session.proved.clone(),
                    offered: session.offered.clone(),
                    contact_key: session.contact_key,
                },
                direction,
                state: Default::default(),
                send,
                events: tokio::sync::watch::channel(()).0,
            },
            outgoing,
        }
    }
    /// Dispatches a data frame without awaiting a slow request handler.
    pub fn accept(&self, frame: Frame) -> Result<Option<NativeSession>, Error> {
        let Message::Channel(packet) = frame.message else {
            return Err(denied("link channel message"));
        };
        let key = (frame.direction, frame.channel);
        let generation = packet.generation();
        let mut state = self
            .handle
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Packet::Open { .. } = packet {
            if frame.direction == self.handle.direction
                || state.slots.contains_key(&key)
                || generation < state.remote
            {
                return Err(denied("link channel open quota/sequence"));
            }
            state.remote = generation
                .checked_add(1)
                .ok_or_else(|| denied("link channel generation exhausted"))?;
            let (send, receive) = mpsc::channel(QUEUE);
            state.slots.insert(
                key,
                Slot {
                    generation,
                    receive: Some(send),
                    bytes: Vec::new(),
                    total: 0,
                    closed: false,
                },
            );
            return Ok(Some(self.handle.session(key, generation, receive)?));
        }
        let slot = state
            .slots
            .get_mut(&key)
            .ok_or_else(|| denied("unknown link channel"))?;
        if slot.generation != generation {
            return Err(denied("stale link channel"));
        }
        match packet {
            Packet::Data { bytes, last, .. } => {
                slot.total = slot
                    .total
                    .checked_add(bytes.len())
                    .filter(|total| *total <= MAX_SECURE_BYTES)
                    .ok_or_else(|| denied("link channel byte quota"))?;
                if slot.bytes.len() + bytes.len() > MAX_SECURE_FRAME {
                    return Err(denied("link channel message quota"));
                }
                slot.bytes.extend(bytes);
                if last {
                    slot.receive
                        .as_ref()
                        .ok_or_else(|| denied("closed link channel"))?
                        .try_send(std::mem::take(&mut slot.bytes))
                        .map_err(|_| denied("link channel queue quota"))?;
                }
            }
            Packet::End { .. } => {
                if !slot.bytes.is_empty() {
                    return Err(denied("unfinished link channel message"));
                }
                slot.receive.take();
                if slot.closed {
                    state.slots.remove(&key);
                    self.handle.events.send_replace(());
                }
            }
            Packet::Open { .. } => return Err(denied("duplicate link channel")),
        }
        Ok(None)
    }
}
impl Handle {
    /// Captures one bounded page after a committed cursor, without querying the peer.
    pub async fn push_committed<A: RowApplication>(
        &self,
        replica: &EncryptedReplica<A>,
        cursor: &ScopeFrontier,
    ) -> Result<bool, Error> {
        if !self.admission.proved.contains(&cursor.scope) {
            return Err(denied("unproved link push scope"));
        }
        let page = match replica
            .range_page(
                self.admission.peer,
                &self.admission.credentials,
                cursor,
                None,
                FRAME_BYTES / 8,
            )
            .await
        {
            Err(Error::Rejected(reason)) if reason == "byte quota smaller than one frame" => {
                return Ok(false);
            }
            other => other?,
        };
        if page.behind {
            return Ok(false);
        }
        if page.frames.is_empty() {
            return Ok(true);
        }
        let frame = Frame::new(
            self.direction,
            Message::Push {
                scope: cursor.scope,
                frames: page.frames,
            },
        );
        let bytes = encode(&frame)?;
        if encode(&SignedMessage {
            sequence: u64::MAX,
            signature: self.admission.local.sign(b"push bound"),
            bytes,
        })?
        .len()
            > FRAME_BYTES
        {
            return Ok(false);
        }
        timeout(TIMEOUT, self.send.send(frame))
            .await
            .map_err(|_| denied("link push write timeout"))?
            .map_err(|_| denied("link ended"))?;
        Ok(page.next.is_none())
    }
    /// Applies self-contained frames using the same authorization and receive path as a catch-up page.
    pub async fn receive_push<A: RowApplication>(
        &self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        frames: Vec<EncryptedFrame>,
    ) -> Result<bool, Error> {
        if !self.admission.offered.contains(&scope)
            || frames.is_empty()
            || frames.len() > rows::MAX_SECURE_PAGE
        {
            return Err(denied("unproved or overfull link push"));
        }
        replica
            .authorize_scope_peer(self.admission.peer, &self.admission.credentials, scope)
            .await?;
        if replica
            .horizon_peer_behind(self.admission.peer, &self.admission.credentials, scope)
            .await?
        {
            return Err(denied("phase-3 rebootstrap required before sync or write"));
        }
        for frame in &frames {
            if frame.scope()? != scope {
                return Err(denied("pushed frame outside its scope"));
            }
            frame.chain()?;
        }
        let mut complete = true;
        for status in replica.receive_all(frames).await {
            complete &= !matches!(
                status?,
                ReceiveStatus::PendingDependencies | ReceiveStatus::MissingKey
            );
        }
        Ok(complete)
    }

    pub async fn open(&self) -> Result<NativeSession, Error> {
        let mut events = self.events.subscribe();
        let (key, generation, receive) = timeout(TIMEOUT, async {
            loop {
                events.borrow_and_update();
                let reserved = {
                    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(channel) = (2..CHANNELS)
                        .find(|channel| !state.slots.contains_key(&(self.direction, *channel)))
                    {
                        let generation = state.next;
                        state.next = generation
                            .checked_add(1)
                            .ok_or_else(|| denied("link channel generation exhausted"))?;
                        let key = (self.direction, channel);
                        let (send, receive) = mpsc::channel(QUEUE);
                        state.slots.insert(
                            key,
                            Slot {
                                generation,
                                receive: Some(send),
                                bytes: Vec::new(),
                                total: 0,
                                closed: false,
                            },
                        );
                        Some((key, generation, receive))
                    } else {
                        None
                    }
                };
                if let Some(reserved) = reserved {
                    return Ok::<_, Error>(reserved);
                }
                events.changed().await.map_err(|_| denied("link ended"))?;
            }
        })
        .await
        .map_err(|_| denied("link channel quota timeout"))??;
        let session = self.session(key, generation, receive)?;
        self.send_frame(key, Packet::Open { generation }).await?;
        Ok(session)
    }
    fn session(
        &self,
        key: (bool, u16),
        generation: u64,
        receive: mpsc::Receiver<Vec<u8>>,
    ) -> Result<NativeSession, Error> {
        let admission = &self.admission;
        let binding = signing_bytes(
            "rocky.link-channel.v1",
            &(admission.binding, key, generation),
        )?;
        Ok(NativeSession {
            stream: Box::new(tokio::io::duplex(1).0),
            channel: Some(Io {
                handle: self.clone(),
                key,
                generation,
                receive,
                closed: false,
            }),
            local: admission.local.clone(),
            peer: admission.peer,
            credentials: admission.credentials.clone(),
            peer_chain: admission.peer_chain.clone(),
            chains: Default::default(),
            signing: frame_signing_head(&binding)?,
            binding: admission.binding,
            proved: admission.proved.clone(),
            offered: admission.offered.clone(),
            served_frames: Default::default(),
            contact_requests: 0,
            contact_handed: 0,
            contact_received: 0,
            contact_key: admission.contact_key,
            sent: 0,
            received: 0,
            remaining: MAX_SECURE_BYTES,
        })
    }
    async fn send_frame(
        &self,
        (direction, channel): (bool, u16),
        packet: Packet,
    ) -> Result<(), Error> {
        timeout(
            TIMEOUT,
            self.send.send(Frame {
                channel,
                direction,
                message: Message::Channel(packet),
            }),
        )
        .await
        .map_err(|_| denied("link channel write timeout"))?
        .map_err(|_| denied("link ended"))
    }
}
impl Io {
    pub async fn send(&mut self, bytes: Vec<u8>) -> Result<(), Error> {
        if bytes.is_empty() || bytes.len() > MAX_SECURE_FRAME {
            return Err(denied("link channel message quota"));
        }
        let count = bytes.len().div_ceil(FRAGMENT);
        for (index, bytes) in bytes.chunks(FRAGMENT).enumerate() {
            self.handle
                .send_frame(
                    self.key,
                    Packet::Data {
                        generation: self.generation,
                        bytes: bytes.to_vec(),
                        last: index + 1 == count,
                    },
                )
                .await?;
        }
        Ok(())
    }
    pub async fn receive(&mut self) -> Result<Vec<u8>, Error> {
        timeout(TIMEOUT, self.receive.recv())
            .await
            .map_err(|_| denied("link channel read timeout"))?
            .ok_or_else(|| Error::Io(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)))
    }
    pub async fn close(&mut self) -> Result<(), Error> {
        if !self.closed {
            self.handle
                .send_frame(
                    self.key,
                    Packet::End {
                        generation: self.generation,
                    },
                )
                .await?;
            self.mark_closed();
        }
        Ok(())
    }
    fn mark_closed(&mut self) {
        self.closed = true;
        let mut state = self
            .handle
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = state
            .slots
            .get_mut(&self.key)
            .filter(|slot| slot.generation == self.generation)
        {
            slot.closed = true;
            if slot.receive.is_none() {
                state.slots.remove(&self.key);
                self.handle.events.send_replace(());
            }
        }
    }
}
impl Drop for Io {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.handle.send.try_send(Frame {
                channel: self.key.1,
                direction: self.key.0,
                message: Message::Channel(Packet::End {
                    generation: self.generation,
                }),
            });
            self.mark_closed();
        }
    }
}

impl Drop for Channels {
    fn drop(&mut self) {
        self.handle
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slots
            .clear();
        self.handle.events.send_replace(());
    }
}
