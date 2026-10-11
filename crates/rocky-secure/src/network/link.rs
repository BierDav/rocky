//! Authenticated live link framing. See `docs/runtime/live-links.md`.
use super::*;
use tokio::io::{ReadHalf, WriteHalf};

// A distinct handshake identity prevents a native session being used as a link.
pub(super) const HANDSHAKE_VERSION: u32 = 0x10001;
pub const CHANNELS: u16 = 2;
pub const FRAME_BYTES: usize = 64 * 1024;
pub const SIGNAL_SCOPES: usize = 64;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum CloseReason {
    Authority,
    Duplicate,
    Offline,
    Detached,
    Busy,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Message {
    Changed {
        scopes: Vec<ResourceScope>,
        token: [u8; 32],
    },
    Ping,
    Pong,
    Close {
        reason: CloseReason,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub channel: u16,
    /// The request lane: false for the dialer, true for the acceptor.
    pub direction: bool,
    pub message: Message,
}
impl Frame {
    pub fn new(direction: bool, message: Message) -> Self {
        Self {
            channel: u16::from(matches!(message, Message::Changed { .. })),
            direction,
            message,
        }
    }
    fn validate(&self) -> Result<(), Error> {
        let channel = u16::from(matches!(self.message, Message::Changed { .. }));
        if self.channel >= CHANNELS || self.channel != channel {
            return Err(denied("link channel quota"));
        }
        if let Message::Changed { scopes, .. } = &self.message
            && (scopes.is_empty()
                || scopes.len() > SIGNAL_SCOPES
                || scopes
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != scopes.len())
        {
            return Err(denied("link signal scope quota"));
        }
        Ok(())
    }
}

pub struct Link {
    pub peer: PublicKey,
    pub held: Vec<ResourceScope>,
    pub offered: Vec<ResourceScope>,
    pub direction: bool,
    pub expires: u64,
    pub states: std::collections::BTreeMap<ResourceScope, Commitment>,
    pub reader: Reader,
    pub writer: Writer,
}
pub struct Reader {
    stream: ReadHalf<Box<dyn SessionStream>>,
    peer: PublicKey,
    signing: Vec<u8>,
    sequence: u64,
}
pub struct Writer {
    stream: WriteHalf<Box<dyn SessionStream>>,
    local: PrivateKey,
    signing: Vec<u8>,
    sequence: u64,
}
impl Link {
    pub async fn open<A: RowApplication>(
        stream: impl SessionStream + 'static,
        replica: &EncryptedReplica<A>,
        local: PrivateKey,
        credentials: CredentialChain,
        expected: Option<PublicKey>,
    ) -> Result<Self, Error> {
        let side = if expected.is_some() {
            Side::Initiator
        } else {
            Side::Acceptor
        };
        let mut session = NativeSession::handshake(
            Box::new(stream),
            replica,
            (local, credentials.clone()),
            (side, expected),
            MAX_SECURE_BYTES,
            HANDSHAKE_VERSION,
        )
        .await?
        .ok_or_else(|| denied("link handshake required"))?;
        let mut scopes = Vec::new();
        let mut after = None;
        loop {
            let (page, next) = replica
                .held_scopes(session.local.public_key(), &credentials, after)
                .await?;
            scopes.extend(page);
            if next.is_none() {
                break;
            }
            after = next;
        }
        {
            let inner = replica.inner.lock().await;
            for grant in inner.local_grants(now()?)?.iter() {
                if grant.rows == rocky_core::RowSelection::All
                    && (grant.rights.contains(&rocky_core::Right::Read)
                        || grant.rights.contains(&rocky_core::Right::Custody))
                    && inner.holds(grant.scope).await?
                {
                    scopes.push(grant.scope);
                }
            }
        }
        scopes.sort();
        scopes.dedup();
        // Exchange proofs in handshake order so bounded transports cannot deadlock.
        if side == Side::Initiator {
            session.offer_link_holdings(replica, &scopes).await?;
            session.accept_link_holdings(replica).await?;
        } else {
            session.accept_link_holdings(replica).await?;
            session.offer_link_holdings(replica, &scopes).await?;
        }
        let states = if side == Side::Initiator {
            session.offer_link_states(replica).await?;
            session.accept_link_states().await?
        } else {
            let states = session.accept_link_states().await?;
            session.offer_link_states(replica).await?;
            states
        };
        let held = session.proved.iter().copied().collect();
        let (read, write) = tokio::io::split(session.stream);
        Ok(Self {
            peer: session.peer,
            states,
            held,
            offered: session.offered.iter().copied().collect(),
            direction: side == Side::Acceptor,
            expires: credentials.expires().min(session.credentials.expires()),
            reader: Reader {
                stream: read,
                peer: session.peer,
                signing: session.signing.clone(),
                sequence: session.received,
            },
            writer: Writer {
                stream: write,
                local: session.local,
                signing: session.signing,
                sequence: session.sent,
            },
        })
    }
}
/// A scope's committed receipt state, flushed before its token can leave.
pub async fn scope_token<A: RowApplication>(
    replica: &EncryptedReplica<A>,
    scope: ResourceScope,
) -> Result<Commitment, Error> {
    let (bytes, flusher, commit) = {
        let inner = replica.inner.lock().await;
        let frontier = inner.frontier(scope).await?;
        let logs: Vec<_> = frontier
            .logs
            .into_iter()
            .map(|log| (log.author, log.held))
            .collect();
        let existence: Vec<_> = inner.existence_frontier(scope).await?.into_iter().collect();
        let mut head = inner
            .conn
            .query(
                "SELECT COALESCE(MAX(sequence), 0) FROM secure_results WHERE domain=?",
                [encode(&scope.domain)?],
            )
            .await?;
        let row = head
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("link result head".into()))?;
        let head = u64::try_from(row.get::<i64>(0)?)
            .map_err(|_| Error::Corrupt("link result head".into()))?;
        let bytes = encode(&(
            logs,
            existence,
            head,
            inner.strong_base(scope).await?.sequence(),
            inner.mergeable_horizon(scope).await?,
        ))?;
        (bytes, inner.flusher.clone(), inner.commit)
    };
    flusher.flush(commit)?;
    Ok(rocky_core::commitment("rocky.link-scope.v1", &bytes))
}
impl NativeSession {
    async fn offer_link_states<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        let scopes: Vec<_> = self.proved.iter().copied().collect();
        for batch in scopes.chunks(SIGNAL_SCOPES) {
            let mut states = Vec::new();
            for scope in batch {
                states.push((*scope, scope_token(replica, *scope).await?));
            }
            self.send(super::Message::LinkState(states)).await?;
            self.acknowledged("link state acknowledgement").await?;
        }
        self.send(super::Message::Acknowledged).await
    }
    async fn accept_link_states(
        &mut self,
    ) -> Result<std::collections::BTreeMap<ResourceScope, Commitment>, Error> {
        let mut states = std::collections::BTreeMap::new();
        loop {
            match self.receive().await? {
                super::Message::Acknowledged => {
                    if states.len() != self.offered.len() {
                        return Err(denied("incomplete link state"));
                    }
                    return Ok(states);
                }
                super::Message::LinkState(batch)
                    if !batch.is_empty() && batch.len() <= SIGNAL_SCOPES =>
                {
                    for (scope, token) in batch {
                        if !self.offered.contains(&scope) || states.insert(scope, token).is_some() {
                            return Err(denied("unproved link state"));
                        }
                    }
                    self.send(super::Message::Acknowledged).await?;
                }
                _ => return Err(denied("link state response")),
            }
        }
    }
    async fn offer_link_holdings<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: &[ResourceScope],
    ) -> Result<(), Error> {
        for batch in scopes.chunks(SIGNAL_SCOPES) {
            let allowed = replica
                .peer_holds(self.peer, &self.credentials, batch)
                .await?;
            if allowed.is_empty() {
                continue;
            }
            let proofs = replica.holding_proofs(self.binding, &allowed).await?;
            let partitions = replica
                .tuple_proofs(self.peer, &self.credentials, &allowed)
                .await?;
            self.send(HoldingMessage::Prove {
                scopes: allowed,
                proofs,
                partitions,
            })
            .await?;
            let HoldingMessage::Proved { scopes: accepted } =
                self.reply::<HoldingMessage>().await?
            else {
                return Err(denied("link holding response"));
            };
            if accepted.iter().any(|scope| !batch.contains(scope)) {
                return Err(denied("link unoffered holding"));
            }
            self.offered.extend(accepted);
        }
        self.send(super::Message::Acknowledged).await
    }
    async fn accept_link_holdings<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        loop {
            match self.receive().await? {
                super::Message::Acknowledged => return Ok(()),
                super::Message::Holdings(HoldingMessage::Prove {
                    scopes,
                    proofs,
                    partitions,
                }) => {
                    self.accept_proofs(replica, &scopes, &proofs, &partitions)
                        .await?;
                    let mut accepted = Vec::new();
                    for scope in scopes {
                        if self.require_holding(replica, scope).await.is_ok()
                            && replica
                                .authorize_scope_peer(self.peer, &self.credentials, scope)
                                .await
                                .is_ok()
                        {
                            self.proved.insert(scope);
                            accepted.push(scope);
                        }
                    }
                    self.send(HoldingMessage::Proved { scopes: accepted })
                        .await?;
                }
                _ => return Err(denied("link holding request")),
            }
        }
    }
}
impl Writer {
    /// Finishes queued frames before a transport resets an unfinished stream.
    pub async fn finish(&mut self) -> Result<(), Error> {
        timeout(TIMEOUT, self.stream.shutdown())
            .await
            .map_err(|_| denied("link finish timeout"))?
            .map_err(Error::Io)
    }
    pub async fn send(&mut self, frame: Frame) -> Result<(), Error> {
        frame.validate()?;
        let bytes = encode(&frame)?;
        let frame = encode(&SignedMessage {
            sequence: self.sequence,
            signature: self
                .local
                .sign(&frame_signing_bytes(&self.signing, self.sequence, &bytes)),
            bytes,
        })?;
        if frame.len() > FRAME_BYTES {
            return Err(denied("link frame quota"));
        }
        timeout(TIMEOUT, async {
            self.stream.write_u32(frame.len() as u32).await?;
            self.stream.write_all(&frame).await?;
            self.stream.flush().await
        })
        .await
        .map_err(|_| denied("link write timeout"))?
        .map_err(Error::Io)?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| denied("link sequence exhausted"))?;
        Ok(())
    }
}
impl Reader {
    /// Idle reads have no deadline. Once a header starts, the frame has ten seconds.
    pub async fn receive(&mut self) -> Result<Frame, Error> {
        let first = self.stream.read_u8().await.map_err(Error::Io)?;
        timeout(TIMEOUT, async {
            let mut header = [first, 0, 0, 0];
            self.stream
                .read_exact(&mut header[1..])
                .await
                .map_err(Error::Io)?;
            let length = u32::from_be_bytes(header) as usize;
            if length == 0 || length > FRAME_BYTES {
                return Err(denied("link frame quota"));
            }
            let mut bytes = vec![0; length];
            self.stream
                .read_exact(&mut bytes)
                .await
                .map_err(Error::Io)?;
            let signed: SignedMessage = decode(&bytes)?;
            if signed.sequence != self.sequence
                || !self.peer.verify(
                    &frame_signing_bytes(&self.signing, signed.sequence, &signed.bytes),
                    &signed.signature,
                )
            {
                return Err(denied("link signature/sequence"));
            }
            let frame: Frame = decode(&signed.bytes)?;
            frame.validate()?;
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or_else(|| denied("link sequence exhausted"))?;
            Ok(frame)
        })
        .await
        .map_err(|_| denied("link read timeout"))?
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn channels_are_bounded_and_control_has_its_own_channel() {
        assert!(Frame::new(false, Message::Ping).validate().is_ok());
        assert!(
            Frame {
                channel: CHANNELS,
                direction: false,
                message: Message::Ping
            }
            .validate()
            .is_err()
        );
        assert!(
            Frame {
                channel: 1,
                direction: false,
                message: Message::Pong
            }
            .validate()
            .is_err()
        );
        assert!(
            Frame::new(
                false,
                Message::Changed {
                    scopes: vec![],
                    token: [0; 32]
                }
            )
            .validate()
            .is_err()
        );
    }
    #[test]
    fn scope_bounds_and_maximal_frame_fit_the_byte_quota() {
        let scope = ResourceScope {
            domain: rocky_core::Scope {
                mesh: rocky_core::MeshId([255; 16]),
                app: rocky_core::AppId([255; 16]),
                domain: rocky_core::DomainId([255; 16]),
            },
            table: [255; 32],
            encryption: [255; 32],
        };
        let scopes: Vec<_> = (0..SIGNAL_SCOPES)
            .map(|index| ResourceScope {
                table: {
                    let mut table = scope.table;
                    table[0] = index as u8;
                    table
                },
                ..scope
            })
            .collect();
        let frame = Frame::new(
            true,
            Message::Changed {
                scopes: scopes.clone(),
                token: [255; 32],
            },
        );
        frame.validate().unwrap();
        let bytes = encode(&frame).unwrap();
        let signed = SignedMessage {
            sequence: u64::MAX,
            bytes,
            signature: PrivateKey::new().sign(b"bounds"),
        };
        assert!(encode(&signed).unwrap().len() <= FRAME_BYTES);
        assert!(
            Frame::new(
                false,
                Message::Changed {
                    scopes: vec![scope; 2],
                    token: [0; 32]
                }
            )
            .validate()
            .is_err()
        );
        let mut too_many = scopes;
        too_many.push(ResourceScope {
            table: [254; 32],
            ..scope
        });
        assert!(
            Frame::new(
                false,
                Message::Changed {
                    scopes: too_many,
                    token: [0; 32]
                }
            )
            .validate()
            .is_err()
        );
    }
    #[test]
    fn only_current_link_name_and_distinct_handshake_identity() {
        assert_eq!(
            rocky_core::net::Protocol::named(b"rocky/link/1"),
            Some(rocky_core::net::Protocol::Link)
        );
        assert_eq!(rocky_core::net::Protocol::named(b"rocky/link/2"), None);
        assert_ne!(
            session_label(HANDSHAKE_VERSION),
            session_label(NATIVE_SESSION_VERSION)
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn oversized_and_empty_headers_are_refused_before_body_read() {
        for length in [0, FRAME_BYTES as u32 + 1] {
            let (near, mut far) = tokio::io::duplex(16);
            let (stream, _) = tokio::io::split(Box::new(near) as Box<dyn SessionStream>);
            let mut reader = Reader {
                stream,
                peer: PrivateKey::new().public_key(),
                signing: vec![],
                sequence: 0,
            };
            far.write_u32(length).await.unwrap();
            assert!(
                reader
                    .receive()
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("link frame quota")
            );
        }
    }
}
