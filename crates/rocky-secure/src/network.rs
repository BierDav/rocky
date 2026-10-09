//! Mutually authenticated native sessions with per-frame and per-session
//! byte limits, over any reliable ordered byte stream: TCP, a QUIC stream or
//! an in-memory pipe.
use super::*;
use p2panda_core::Signature;
use rand::{RngCore, rngs::OsRng};
use rocky_core::{Commitment, ResourceScope, RowApplication};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::replica::roles::FrontierState;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};

mod frontiers;

/// The stream a session runs over. The transport adds no security the
/// session relies on; see `docs/runtime/permissions-and-sync.md`.
pub use rocky_core::net::SessionStream;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Existence records per exchanged page and signers per frontier.
const EXISTENCE_PAGE: usize = 128;
const MAX_EXISTENCE_SIGNERS: usize = 4096;
#[derive(Serialize, Deserialize)]
struct Hello {
    node: PublicKey,
    challenge: [u8; 32],
    credentials: CredentialChain,
}
#[derive(Serialize, Deserialize)]
struct SignedMessage {
    sequence: u64,
    bytes: Vec<u8>,
    signature: Signature,
}
#[derive(Serialize, Deserialize)]
enum Message {
    Pull {
        scopes: Vec<ResourceScope>,
        after: Option<Hash>,
        quota: usize,
    },
    Page {
        frames: Vec<EncryptedFrame>,
        next: Option<Hash>,
        /// Subscribed scopes whose history is pruned at the sender.
        pruned: Vec<ResourceScope>,
    },
    MergeableCut {
        scope: ResourceScope,
    },
    MergeableState {
        signed: Option<Box<rocky_credentials::SignedHorizon>>,
        behind: bool,
    },
    HorizonRejoined {
        scope: ResourceScope,
        horizon: Commitment,
    },
    HorizonAck {
        scope: ResourceScope,
        ack: rocky_credentials::HorizonAck,
    },
    Horizon {
        scope: ResourceScope,
        after: Option<PublicKey>,
    },
    HorizonLogs {
        page: HorizonPage,
    },
    CheckpointOffer {
        scope: ResourceScope,
    },
    Checkpoint {
        signed: Option<Box<SignedCheckpoint>>,
    },
    CheckpointPull {
        scope: ResourceScope,
        checkpoint: rocky_core::Commitment,
        after: Option<rocky_core::Commitment>,
        quota: usize,
    },
    CheckpointRows {
        page: CheckpointPage,
    },
    Ack {
        id: Hash,
        applied: bool,
    },
    Acknowledged,
    Execute {
        command: EncryptedCommand,
        scopes: Vec<ResourceScope>,
    },
    ResultPull {
        domain: rocky_core::Scope,
        sequence: u64,
        scopes: Vec<ResourceScope>,
    },
    ResultPush {
        result: ConfirmedCommand,
    },
    Result {
        result: Option<ConfirmedCommand>,
        /// The sender's signed receipt evidence: the result stays verifiable
        /// after the coordinator retired its identity.
        evidence: Option<Vec<u8>>,
    },
    ResultAck {
        states: Vec<(ResourceScope, bool)>,
    },
    Range {
        frontier: ScopeFrontier,
        after: Option<(PublicKey, u64)>,
        quota: usize,
    },
    RangeFrames {
        page: RangePage,
    },
    /// The puller's frontier after a page, as acknowledgment.
    Held {
        frontier: ScopeFrontier,
    },
    FrontiersQuery {
        frontiers: Vec<ScopeFrontier>,
    },
    Frontiers {
        states: Vec<FrontierState>,
    },
    FrontierQuery {
        scope: ResourceScope,
    },
    Frontier {
        frontier: ScopeFrontier,
    },
    Push {
        scope: ResourceScope,
        frames: Vec<EncryptedFrame>,
    },
    ScopesQuery {
        after: Option<ResourceScope>,
    },
    Scopes {
        scopes: Vec<ResourceScope>,
        next: Option<ResourceScope>,
    },
    Read {
        scope: ResourceScope,
        after: Option<Commitment>,
        max_rows: usize,
    },
    ReadRows {
        package: EncryptedPackage,
    },
    ExistenceBaseOffer {
        scope: ResourceScope,
        expected: Option<Commitment>,
    },
    ExistenceBase {
        attachment: Option<Box<ExistenceBaseAttachment>>,
    },
    ExistenceBaseBegin {
        attachment: Box<ExistenceBaseAttachment>,
    },
    ExistenceBaseCursor {
        after: Option<i64>,
        complete: bool,
    },
    ExistenceBaseStage {
        scope: ResourceScope,
        page: ExistenceBasePage,
    },
    ExistenceBaseFinish {
        scope: ResourceScope,
    },
    ExistenceBasePull {
        scope: ResourceScope,
        base: Commitment,
        after: Option<i64>,
    },
    ExistenceBaseRows {
        page: ExistenceBasePage,
    },
    /// Existence logs (`docs/runtime/existence-logs.md`): any device of the domain may ask.
    ExistenceFrontier {
        scope: ResourceScope,
    },
    ExistenceHeld {
        frontier: BTreeMap<PublicKey, u64>,
    },
    ExistencePull {
        scope: ResourceScope,
        known: BTreeMap<PublicKey, u64>,
        after: Option<(PublicKey, u64)>,
    },
    ExistenceRecords {
        page: ExistencePage,
    },
    ExistencePush {
        scope: ResourceScope,
        records: Vec<ExistenceRecord>,
    },
}
/// The outcome of one pull.
#[derive(Clone, Debug)]
pub struct Pulled {
    pub next: Option<Hash>,
    /// Scopes whose pruned history at the peer this replica does not hold.
    pub rebootstrap: Vec<ResourceScope>,
}
/// What one catch-up pass of a scope did.
#[derive(Clone, Debug, Default)]
pub struct CatchUp {
    /// Frames received, including duplicates.
    pub frames: usize,
    pub pages: usize,
    /// This replica is behind the peer's horizon and must rebootstrap.
    pub behind: bool,
    /// Encoded bytes of the frames received.
    pub bytes: usize,
    /// Applied operations the peer reported that this replica still lacks.
    pub lag: u64,
    /// The session's byte budget ran low; continue on a new session.
    pub exhausted: bool,
}
/// What one push of a scope did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pushed {
    pub frames: usize,
    /// The peer is behind this replica's horizon and must rebootstrap from it.
    pub behind: bool,
    /// The session's byte budget ran low; continue on a new session.
    pub exhausted: bool,
}
/// One sync of a scope: rebootstrap if needed, then catch up.
#[derive(Clone, Debug)]
pub struct ScopeSync {
    pub rebootstrap: Option<RebootstrapReport>,
    pub catch_up: CatchUp,
}
/// Frames in one transfer page.
const MAX_SECURE_PAGE: usize = 32;
/// Session signatures bind fresh mutual challenges, credentials and frame sequence.
/// Confidentiality is provided by encrypted payloads; routing and credentials are visible.
pub struct NativeSession {
    stream: Box<dyn SessionStream>,
    local: PrivateKey,
    peer: PublicKey,
    credentials: CredentialChain,
    binding: Vec<u8>,
    sent: u64,
    received: u64,
    remaining: usize,
}
impl NativeSession {
    /// Authenticates `expected_peer` over `stream`.
    pub async fn authenticate(
        stream: impl SessionStream + 'static,
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
            local,
            credentials,
            |peer| *peer == expected_peer,
            byte_budget,
        )
        .await
    }
    /// Authenticates whichever peer `admit` accepts, for a node that learns
    /// its peer only from the handshake. Every request is still authorized
    /// by the peer's credentials.
    pub async fn accept(
        stream: impl SessionStream + 'static,
        local: PrivateKey,
        credentials: CredentialChain,
        admit: impl FnOnce(&PublicKey) -> bool,
        byte_budget: usize,
    ) -> Result<Self, Error> {
        let own = local.public_key();
        Self::handshake(
            Box::new(stream),
            local,
            credentials,
            |peer| *peer != own && admit(peer),
            byte_budget,
        )
        .await
    }
    async fn handshake(
        mut stream: Box<dyn SessionStream>,
        local: PrivateKey,
        credentials: CredentialChain,
        admit: impl FnOnce(&PublicKey) -> bool,
        byte_budget: usize,
    ) -> Result<Self, Error> {
        if byte_budget == 0 || byte_budget > MAX_SECURE_BYTES {
            return Err(denied("native session peer/budget"));
        }
        let mut challenge = [0; 32];
        OsRng.fill_bytes(&mut challenge);
        let hello = Hello {
            node: local.public_key(),
            challenge,
            credentials,
        };
        let local_bytes = encode(&hello)?;
        write_frame(&mut stream, &local_bytes).await?;
        let peer_bytes = read_frame(&mut stream).await?;
        let peer: Hello = decode(&peer_bytes)?;
        if !admit(&peer.node) {
            return Err(denied("unexpected native peer"));
        }
        let mut parts = [(hello.node, local_bytes), (peer.node, peer_bytes)];
        parts.sort_by_key(|(key, _)| *key);
        // The current wire label refuses every other session format.
        let binding = signing_bytes("rocky.native-session.v5", &parts)?;
        write_frame(&mut stream, &encode(&local.sign(&binding))?).await?;
        let signature: Signature = decode(&read_frame(&mut stream).await?)?;
        if !peer.node.verify(&binding, &signature) {
            return Err(denied("native session authentication failed"));
        }
        Ok(Self {
            stream,
            local,
            peer: peer.node,
            credentials: peer.credentials,
            binding,
            sent: 0,
            received: 0,
            remaining: byte_budget,
        })
    }
    pub fn peer(&self) -> PublicKey {
        self.peer
    }
    /// Ends the session: the peer reads the end of the stream after the
    /// last message. Transports that reset unfinished streams need this.
    pub async fn close(mut self) -> Result<(), Error> {
        timeout(TIMEOUT, self.stream.shutdown())
            .await
            .map_err(|_| denied("native write timeout"))?
            .map_err(Error::Io)
    }
    async fn send(&mut self, message: &Message) -> Result<(), Error> {
        let bytes = encode(message)?;
        let signature = self.local.sign(&signing_bytes(
            "rocky.native-frame.v1",
            &(&self.binding, self.sent, &bytes),
        )?);
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
                &signing_bytes(
                    "rocky.native-frame.v1",
                    &(&self.binding, message.sequence, &message.bytes),
                )?,
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
    /// Each pull supplies current subscriptions. Leaving a scope removes it from later pulls.
    /// A scope whose history the peer pruned past what this replica holds
    /// fails the pull; `pull_page` reports such scopes instead.
    pub async fn pull<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: Vec<ResourceScope>,
        after: Option<Hash>,
        quota: usize,
    ) -> Result<Option<Hash>, Error> {
        let pulled = self.pull_page(replica, scopes, after, quota).await?;
        if !pulled.rebootstrap.is_empty() {
            return Err(denied(
                "behind the peer's pruning horizon; rebootstrap the scope",
            ));
        }
        Ok(pulled.next)
    }
    /// One pull. Frames of a scope this replica is behind the peer's horizon
    /// for are not received; the scope is returned in `rebootstrap`.
    pub async fn pull_page<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: Vec<ResourceScope>,
        after: Option<Hash>,
        quota: usize,
    ) -> Result<Pulled, Error> {
        self.send(&Message::Pull {
            scopes: scopes.clone(),
            after,
            quota,
        })
        .await?;
        let Message::Page {
            frames,
            next,
            pruned,
        } = self.receive().await?
        else {
            return Err(denied("unexpected native response"));
        };
        let mut rebootstrap = Vec::new();
        for scope in pruned {
            if !scopes.contains(&scope) {
                return Err(denied("native response outside subscription"));
            }
            let mut after = None;
            loop {
                self.send(&Message::Horizon { scope, after }).await?;
                let Message::HorizonLogs { page } = self.receive().await? else {
                    return Err(denied("unexpected horizon response"));
                };
                if page.scope != scope || page.logs.len() > MAX_SECURE_PAGE * 8 {
                    return Err(denied("horizon outside subscription"));
                }
                if replica.behind_horizon(&page).await? {
                    rebootstrap.push(scope);
                    break;
                }
                after = page.next;
                if after.is_none() {
                    break;
                }
            }
        }
        let mut held = Vec::with_capacity(frames.len());
        for frame in frames {
            if !rebootstrap.contains(&frame.scope()?) {
                held.push(frame);
            }
        }
        let frames = held;
        if frames.len() > MAX_SECURE_PAGE
            || frames.iter().try_fold(0usize, |sum, frame| {
                Ok::<_, Error>(sum.saturating_add(frame.to_bytes()?.len()))
            })? > quota
        {
            return Err(denied("native page quota"));
        }
        let mut ids = Vec::with_capacity(frames.len());
        for frame in &frames {
            if !scopes.contains(&frame.scope()?) {
                return Err(denied("native response outside subscription"));
            }
            ids.push(frame.id()?);
        }
        // The page is received in shared transactions; acknowledgments follow the commit.
        for (id, status) in ids.into_iter().zip(replica.receive_all(frames).await) {
            let status = status?;
            let applied = status == ReceiveStatus::Applied
                || (status == ReceiveStatus::Duplicate
                    && replica.receipt_status(id).await? == ReceiveStatus::Applied);
            if status != ReceiveStatus::Rejected {
                self.send(&Message::Ack { id, applied }).await?;
                if !matches!(self.receive().await?, Message::Acknowledged) {
                    return Err(denied("unexpected acknowledgment response"));
                }
            }
        }
        Ok(Pulled { next, rebootstrap })
    }
    /// Imports the peer's checkpoint of a scope and switches to it. An
    /// interrupted import of the same checkpoint continues where it stopped.
    pub async fn rebootstrap<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<RebootstrapReport, Error> {
        self.rebootstrap_within(replica, scope, quota)
            .await?
            .ok_or_else(|| {
                denied("native session byte budget exhausted; continue on a new session")
            })
    }
    /// Like `rebootstrap`, but returns `None` once the session's budget runs
    /// low; the staged pages remain and a new session continues.
    async fn rebootstrap_within<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<Option<RebootstrapReport>, Error> {
        let signed = self
            .checkpoint_offer(scope)
            .await?
            .ok_or_else(|| denied("the peer offers no checkpoint"))?;
        self.import_within(replica, signed, quota).await
    }
    /// The peer's current checkpoint of `scope`, if it offers one.
    async fn checkpoint_offer(
        &mut self,
        scope: ResourceScope,
    ) -> Result<Option<Box<SignedCheckpoint>>, Error> {
        self.send(&Message::CheckpointOffer { scope }).await?;
        let Message::Checkpoint { signed } = self.receive().await? else {
            return Err(denied("unexpected checkpoint offer response"));
        };
        if signed
            .as_ref()
            .is_some_and(|signed| signed.statement.scope != scope)
        {
            return Err(denied("checkpoint outside subscription"));
        }
        Ok(signed)
    }
    async fn import_within<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        signed: Box<SignedCheckpoint>,
        quota: usize,
    ) -> Result<Option<RebootstrapReport>, Error> {
        let scope = signed.statement.scope;
        let checkpoint = signed.id()?;
        let finalized_checkpoint = signed.statement.finalized.is_some();
        if let Some(finalized) = &signed.statement.finalized {
            for base in &finalized.statement.bases {
                let expected = ExistenceBaseAttachment {
                    horizon: finalized.clone(),
                    scope: base.scope,
                };
                if !self
                    .pull_existence_base(replica, base.scope, Some(expected.id()?))
                    .await?
                {
                    return Ok(None);
                }
            }
        }
        let (mut after, mut complete) = match replica.rebootstrap_progress(scope).await? {
            Some((staged, cursor, complete)) if staged == checkpoint => (cursor, complete),
            _ => {
                replica.begin_rebootstrap(*signed).await?;
                (None, false)
            }
        };
        while !complete {
            if self.low(quota) {
                return Ok(None);
            }
            self.send(&Message::CheckpointPull {
                scope,
                checkpoint,
                after,
                quota,
            })
            .await?;
            let Message::CheckpointRows { page } = self.receive().await? else {
                return Err(denied("unexpected checkpoint response"));
            };
            if page.scope != scope || page.checkpoint != checkpoint || page.after != after {
                return Err(denied("checkpoint page outside request"));
            }
            (after, complete) = (page.next, page.next.is_none());
            replica.stage_checkpoint_page(page).await?;
        }
        let report = replica.finish_rebootstrap(scope).await?;
        if finalized_checkpoint && let Some(signed) = replica.admitted_horizon(scope).await? {
            self.send(&Message::HorizonRejoined {
                scope,
                horizon: signed.id().map_err(credential)?,
            })
            .await?;
            if !matches!(self.receive().await?, Message::Acknowledged) {
                return Err(denied("unexpected horizon rebootstrap acknowledgment"));
            }
        }
        Ok(Some(report))
    }
    /// Pulls every frame of `scope` this replica lacks in bounded pages and
    /// acknowledges its frontier after each page. Stops at once when this
    /// replica is behind the peer's horizon.
    pub async fn catch_up<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<CatchUp, Error> {
        self.send(&Message::MergeableCut { scope }).await?;
        let Message::MergeableState { signed, behind } = self.receive().await? else {
            return Err(denied("unexpected mergeable horizon response"));
        };
        if signed
            .as_ref()
            .is_some_and(|signed| signed.statement.scope != scope)
        {
            return Err(denied("mergeable horizon outside requested scope"));
        }
        if behind || replica.horizon_rebootstrap_required(scope).await? {
            if let Some(signed) = signed {
                replica.require_horizon_rebootstrap(*signed).await?;
            }
            return Ok(CatchUp {
                behind: true,
                ..Default::default()
            });
        }
        if let Some(signed) = signed
            && replica.receive_horizon((*signed).clone()).await.is_err()
        {
            replica.require_horizon_rebootstrap(*signed).await?;
            return Ok(CatchUp {
                behind: true,
                ..Default::default()
            });
        }
        let mut report = CatchUp::default();
        let mut after = None;
        loop {
            if self.low(quota) {
                report.exhausted = true;
                return Ok(report);
            }
            let frontier = replica.frontier(scope).await?;
            self.send(&Message::Range {
                frontier,
                after,
                quota,
            })
            .await?;
            let Message::RangeFrames { page } = self.receive().await? else {
                return Err(denied("unexpected range response"));
            };
            if page.scope != scope
                || page.frames.len() > MAX_SECURE_PAGE
                || page.frontier.scope != scope
                || (page.behind && !page.frames.is_empty())
            {
                return Err(denied("range page outside request"));
            }
            let mut bytes = 0usize;
            for frame in &page.frames {
                bytes = bytes.saturating_add(frame.to_bytes()?.len());
                report.bytes = report.bytes.saturating_add(frame.to_bytes()?.len());
                if frame.scope()? != scope {
                    return Err(denied("native response outside subscription"));
                }
            }
            if bytes > quota {
                return Err(denied("native page quota"));
            }
            report.pages += 1;
            if page.behind {
                report.behind = true;
                return Ok(report);
            }
            report.frames += page.frames.len();
            // The page is received in shared transactions and one flush.
            for status in replica.receive_all(page.frames).await {
                status?;
            }
            let held = replica.frontier(scope).await?;
            report.lag = held.lag_behind(&page.frontier);
            self.send(&Message::Held { frontier: held }).await?;
            if !matches!(self.receive().await?, Message::Acknowledged) {
                return Err(denied("unexpected acknowledgment response"));
            }
            // The sender's own frontier is its acknowledgment of what it holds.
            replica
                .acknowledge_frontier(self.peer, &self.credentials, &page.frontier)
                .await?;
            after = page.next;
            if after.is_none() {
                return Ok(report);
            }
        }
    }
    /// Starts from a verified checkpoint even when the peer has retained history,
    /// then catches up. Exhausted sessions leave durable import progress.
    /// `None` when the peer offers no checkpoint, as a peer without
    /// whole-scope read does not.
    pub async fn bootstrap_scope<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<Option<ScopeSync>, Error> {
        if self.low(MAX_SECURE_FRAME) {
            return Ok(Some(ScopeSync {
                rebootstrap: None,
                catch_up: CatchUp {
                    exhausted: true,
                    ..Default::default()
                },
            }));
        }
        let rows = quota.min(MAX_SECURE_FRAME / 2 - 4096);
        let Some(signed) = self.checkpoint_offer(scope).await? else {
            return Ok(None);
        };
        let rebootstrap = self.import_within(replica, signed, rows).await?;
        let catch_up = if rebootstrap.is_some() {
            self.catch_up(replica, scope, quota).await?
        } else {
            CatchUp {
                exhausted: true,
                ..Default::default()
            }
        };
        if catch_up.behind {
            return Err(denied("still behind the peer's horizon after bootstrap"));
        }
        Ok(Some(ScopeSync {
            rebootstrap,
            catch_up,
        }))
    }
    /// Rebootstraps `scope` from the peer if this replica is behind its
    /// horizon, then catches up.
    pub async fn sync_scope<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<ScopeSync, Error> {
        let mut catch_up = self.catch_up(replica, scope, quota).await?;
        let mut rebootstrap = None;
        if catch_up.behind && self.low(MAX_SECURE_FRAME) {
            catch_up.exhausted = true;
        } else if catch_up.behind {
            // Checkpoint pages leave room for their row encoding.
            let rows = quota.min(MAX_SECURE_FRAME / 2 - 4096);
            rebootstrap = self.rebootstrap_within(replica, scope, rows).await?;
            if rebootstrap.is_none() {
                catch_up.exhausted = true;
                return Ok(ScopeSync {
                    rebootstrap,
                    catch_up,
                });
            }
            catch_up = self.catch_up(replica, scope, quota).await?;
            if catch_up.behind {
                return Err(denied("still behind the peer's horizon after rebootstrap"));
            }
        }
        Ok(ScopeSync {
            rebootstrap,
            catch_up,
        })
    }
    /// Sends the peer every frame of `scope` it lacks, in bounded pages; the
    /// peer answers each page with its frontier, recorded as acknowledgment.
    pub async fn push<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<Pushed, Error> {
        if replica.horizon_rebootstrap_required(scope).await? {
            return Err(denied("phase-3 rebootstrap required before sync or write"));
        }
        let mut frontier = self.peer_frontier(replica, scope).await?;
        let mut after = None;
        let mut pushed = Pushed::default();
        loop {
            if self.low(quota) {
                pushed.exhausted = true;
                return Ok(pushed);
            }
            let page = replica
                .range_page(self.peer, &self.credentials, &frontier, after, quota)
                .await?;
            if page.behind {
                pushed.behind = true;
                return Ok(pushed);
            }
            if page.frames.is_empty() {
                return Ok(pushed);
            }
            pushed.frames += page.frames.len();
            self.send(&Message::Push {
                scope,
                frames: page.frames,
            })
            .await?;
            let Message::Frontier { frontier: theirs } = self.receive().await? else {
                return Err(denied("unexpected push response"));
            };
            if theirs.scope != scope {
                return Err(denied("frontier outside request"));
            }
            replica
                .acknowledge_frontier(self.peer, &self.credentials, &theirs)
                .await?;
            frontier = theirs;
            after = page.next;
            if after.is_none() {
                return Ok(pushed);
            }
        }
    }
    /// The peer's frontier of `scope`, recorded as its acknowledgment.
    pub async fn peer_frontier<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<ScopeFrontier, Error> {
        self.send(&Message::FrontierQuery { scope }).await?;
        let Message::Frontier { frontier } = self.receive().await? else {
            return Err(denied("unexpected frontier response"));
        };
        if frontier.scope != scope {
            return Err(denied("frontier outside request"));
        }
        replica
            .acknowledge_frontier(self.peer, &self.credentials, &frontier)
            .await?;
        Ok(frontier)
    }
    async fn pull_existence_base<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        expected: Option<Commitment>,
    ) -> Result<bool, Error> {
        self.send(&Message::ExistenceBaseOffer { scope, expected })
            .await?;
        let Message::ExistenceBase { attachment } = self.receive().await? else {
            return Err(denied("unexpected existence base response"));
        };
        let Some(attachment) = attachment else {
            return if expected.is_some() {
                Err(denied("checkpoint existence attachment is unavailable"))
            } else {
                Ok(true)
            };
        };
        if attachment.scope != scope || expected.is_some_and(|id| attachment.id().ok() != Some(id))
        {
            return Err(denied("existence attachment differs from checkpoint"));
        }
        if let Some(held) = replica.installed_existence_base(scope).await? {
            if held.id()? == attachment.id()? || (expected.is_some() && held.covers(&attachment)?) {
                return Ok(true);
            }
            if expected.is_none()
                && attachment.cut()?.logs.iter().all(|next| {
                    held.cut().is_ok_and(|cut| {
                        cut.logs
                            .iter()
                            .find(|log| log.signer == next.signer)
                            .is_some_and(|log| next.sequence <= log.sequence)
                    })
                })
            {
                return Ok(true);
            }
        }
        let base = attachment.id()?;
        let (mut after, mut complete) = replica.begin_existence_base(*attachment).await?;
        while !complete {
            if self.low(MAX_SECURE_FRAME) {
                return Ok(false);
            }
            self.send(&Message::ExistenceBasePull { scope, base, after })
                .await?;
            let Message::ExistenceBaseRows { page } = self.receive().await? else {
                return Err(denied("unexpected existence base page"));
            };
            if page.base != base {
                return Err(denied("existence base changed during transfer"));
            }
            after = page.next;
            complete = after.is_none();
            replica.stage_existence_base(scope, page).await?;
        }
        replica.finish_existence_base(scope).await?;
        Ok(true)
    }
    async fn push_existence_base<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let Some(base) = replica
            .existence_base_offer(self.peer, &self.credentials, scope)
            .await?
        else {
            return Ok(true);
        };
        self.send(&Message::ExistenceBaseOffer {
            scope,
            expected: None,
        })
        .await?;
        let Message::ExistenceBase { attachment: held } = self.receive().await? else {
            return Err(denied("unexpected existence base response"));
        };
        if let Some(held) = held {
            if held.id()? == base.id()? {
                return Ok(true);
            }
            if !held.cut()?.logs.iter().all(|previous| {
                base.cut().is_ok_and(|cut| {
                    cut.logs.iter().any(|next| {
                        next.signer == previous.signer
                            && next.sequence >= previous.sequence
                            && (next.sequence != previous.sequence || next.head == previous.head)
                    })
                })
            }) {
                return Err(denied(
                    "existence bases have incomparable cuts; obtain an issuer successor",
                ));
            }
        }
        let id = base.id()?;
        self.send(&Message::ExistenceBaseBegin {
            attachment: Box::new(base),
        })
        .await?;
        let Message::ExistenceBaseCursor {
            mut after,
            mut complete,
        } = self.receive().await?
        else {
            return Err(denied("unexpected existence cursor"));
        };
        while !complete {
            if self.low(MAX_SECURE_FRAME) {
                return Ok(false);
            }
            let page = replica
                .existence_base_page(
                    self.peer,
                    &self.credentials,
                    scope,
                    id,
                    after,
                    EXISTENCE_PAGE,
                )
                .await?;
            after = page.next;
            complete = after.is_none();
            self.send(&Message::ExistenceBaseStage { scope, page })
                .await?;
            if !matches!(self.receive().await?, Message::Acknowledged) {
                return Err(denied("unexpected existence stage acknowledgment"));
            }
        }
        self.send(&Message::ExistenceBaseFinish { scope }).await?;
        if !matches!(self.receive().await?, Message::Acknowledged) {
            return Err(denied("unexpected existence base acknowledgment"));
        }
        Ok(true)
    }
    pub async fn acknowledge_mergeable_horizon<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let Some(signed) = replica.admitted_horizon(scope).await? else {
            return Ok(false);
        };
        if !signed
            .statement
            .peers
            .iter()
            .any(|peer| peer.node == self.local.public_key())
        {
            return Ok(false);
        }
        let Ok(ack) = replica.acknowledge_horizon(scope).await else {
            return Ok(false);
        };
        self.send(&Message::HorizonAck { scope, ack }).await?;
        if !matches!(self.receive().await?, Message::Acknowledged) {
            return Err(denied("unexpected horizon acknowledgment"));
        }
        Ok(true)
    }
    /// Exchanges the existence log of `scope` with the peer: pulls the
    /// records this replica lacks, then pushes those the peer lacks.
    /// Returns how many records were received and sent.
    pub async fn sync_existence<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<(usize, usize), Error> {
        if !self.pull_existence_base(replica, scope, None).await? {
            return Err(denied(
                "existence base session exhausted; resume on the next session",
            ));
        }
        if !self.push_existence_base(replica, scope).await? {
            return Err(denied(
                "existence base session exhausted; resume on the next session",
            ));
        }
        let mut received = 0;
        let mut after = None;
        loop {
            let known = replica.existence_frontier(scope).await?;
            self.send(&Message::ExistencePull {
                scope,
                known,
                after,
            })
            .await?;
            let Message::ExistenceRecords { page } = self.receive().await? else {
                return Err(denied("unexpected existence response"));
            };
            if page.records.len() > EXISTENCE_PAGE
                || page.records.iter().any(|record| record.scope() != scope)
            {
                return Err(denied("existence page outside request"));
            }
            received += page.records.len();
            replica.receive_existence_all(page.records).await?;
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        self.send(&Message::ExistenceFrontier { scope }).await?;
        let Message::ExistenceHeld { mut frontier } = self.receive().await? else {
            return Err(denied("unexpected existence response"));
        };
        let mut sent = 0;
        let mut after = None;
        loop {
            let page = replica
                .existence_page(
                    self.peer,
                    &self.credentials,
                    scope,
                    &frontier,
                    after,
                    EXISTENCE_PAGE,
                )
                .await?;
            if !page.records.is_empty() {
                sent += page.records.len();
                self.send(&Message::ExistencePush {
                    scope,
                    records: page.records,
                })
                .await?;
                let Message::ExistenceHeld { frontier: theirs } = self.receive().await? else {
                    return Err(denied("unexpected existence response"));
                };
                frontier = theirs;
            }
            after = page.next;
            if after.is_none() {
                return Ok((received, sent));
            }
        }
    }
    /// Scopes the peer holds and this replica may hold too, 64 per page.
    pub async fn peer_scopes(
        &mut self,
        after: Option<ResourceScope>,
    ) -> Result<(Vec<ResourceScope>, Option<ResourceScope>), Error> {
        self.send(&Message::ScopesQuery { after }).await?;
        let Message::Scopes { scopes, next } = self.receive().await? else {
            return Err(denied("unexpected scope list response"));
        };
        if scopes.len() > 64
            || scopes.windows(2).any(|pair| pair[0] >= pair[1])
            || scopes
                .first()
                .is_some_and(|first| after.is_some_and(|after| *first <= after))
        {
            return Err(denied("scope list outside request"));
        }
        Ok((scopes, next))
    }
    /// Reads rows of a scope this replica does not hold from the peer. The
    /// page states the peer's revision, frontier and completeness and is
    /// not stored.
    pub async fn read_remote<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        after: Option<Commitment>,
        max_rows: usize,
    ) -> Result<RemotePage, Error> {
        self.send(&Message::Read {
            scope,
            after,
            max_rows,
        })
        .await?;
        let Message::ReadRows { package } = self.receive().await? else {
            return Err(denied("unexpected remote read response"));
        };
        let page = replica
            .open_remote_read(self.peer, scope, after, &package)
            .await?;
        if page.rows.len() > max_rows {
            return Err(denied("remote read outside request"));
        }
        Ok(page)
    }
    pub async fn execute_command<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        command: EncryptedCommand,
        scopes: Vec<ResourceScope>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<ConfirmedCommand, Error> {
        let expected_request = command.request()?.hash()?;
        self.send(&Message::Execute {
            command,
            scopes: scopes.clone(),
        })
        .await?;
        let Message::Result {
            result: Some(result),
            evidence,
        } = self.receive().await?
        else {
            return Err(denied("missing native command result"));
        };
        if result.strong_package()?.request.hash()? != expected_request
            || result
                .parts
                .iter()
                .any(|part| !scopes.contains(&part.scope))
        {
            return Err(denied("result outside subscription"));
        }
        if let Some(evidence) = evidence {
            coordinator.import_evidence(&evidence).await?;
        }
        replica
            .receive_encrypted_result(result.clone(), coordinator)
            .await?;
        self.send_result_ack(replica, &result).await?;
        Ok(result)
    }
    pub async fn push_result<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        result: ConfirmedCommand,
    ) -> Result<(), Error> {
        self.send(&Message::ResultPush {
            result: result.clone(),
        })
        .await?;
        let Message::ResultAck { states } = self.receive().await? else {
            return Err(denied("missing result custody acknowledgment"));
        };
        replica
            .acknowledge_result(self.peer, &self.credentials, &result, &states)
            .await
    }
    async fn send_result_ack<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        result: &ConfirmedCommand,
    ) -> Result<(), Error> {
        self.send(&Message::ResultAck {
            states: replica.result_ack(result).await?,
        })
        .await?;
        if !matches!(self.receive().await?, Message::Acknowledged) {
            return Err(denied("missing result acknowledgment response"));
        }
        Ok(())
    }
    async fn send_result<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        result: Option<ConfirmedCommand>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<(), Error> {
        let evidence = match &result {
            Some(result) => coordinator.export_evidence(&result.receipt()?).await?,
            None => None,
        };
        self.send(&Message::Result {
            result: result.clone(),
            evidence,
        })
        .await?;
        if let Some(result) = result {
            let Message::ResultAck { states } = self.receive().await? else {
                return Err(denied("missing named result acknowledgment"));
            };
            replica
                .acknowledge_result(self.peer, &self.credentials, &result, &states)
                .await?;
            self.send(&Message::Acknowledged).await?;
        }
        Ok(())
    }
    pub async fn pull_result<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        domain: rocky_core::Scope,
        sequence: u64,
        scopes: Vec<ResourceScope>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<bool, Error> {
        self.send(&Message::ResultPull {
            domain,
            sequence,
            scopes: scopes.clone(),
        })
        .await?;
        let Message::Result { result, evidence } = self.receive().await? else {
            return Err(denied("unexpected native result response"));
        };
        let Some(result) = result else {
            return Ok(false);
        };
        let package = result.strong_package()?;
        if package.request.scope != domain
            || package.sequence != sequence
            || result
                .parts
                .iter()
                .any(|part| !scopes.contains(&part.scope))
        {
            return Err(denied("result outside subscription/sequence"));
        }
        if let Some(evidence) = evidence {
            coordinator.import_evidence(&evidence).await?;
        }
        replica
            .receive_encrypted_result(result.clone(), coordinator)
            .await?;
        self.send_result_ack(replica, &result).await?;
        Ok(true)
    }
    /// Serve a strong invocation or result transfer; the coordinator sees metadata only.
    pub async fn serve_command_once<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<(), Error> {
        let message = self.receive().await?;
        self.answer_command(replica, coordinator, message).await
    }
    /// Serves row sync and command transfers until the peer ends the
    /// session. An executed command passes the admission of a local one.
    pub async fn serve_with_commands<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<(), Error> {
        loop {
            let message = match self.receive().await {
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                other => other?,
            };
            if matches!(
                message,
                Message::Execute { .. } | Message::ResultPull { .. } | Message::ResultPush { .. }
            ) {
                self.answer_command(replica, coordinator, message).await?;
            } else {
                self.answer(replica, message).await?;
            }
        }
    }
    async fn answer_command<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        coordinator: &dyn rocky_core::Coordinator,
        message: Message,
    ) -> Result<(), Error> {
        match message {
            Message::Execute { command, scopes } => {
                replica
                    .authorize_command_transfer(self.peer, &self.credentials, &scopes)
                    .await?;
                let result = replica.execute_encrypted(command, coordinator).await?;
                let package = result.strong_package()?;
                let result = replica
                    .encrypted_result_for(
                        self.peer,
                        &self.credentials,
                        package.request.scope,
                        package.sequence,
                        &scopes,
                    )
                    .await?;
                self.send_result(replica, result, coordinator).await
            }
            Message::ResultPull {
                domain,
                sequence,
                scopes,
            } => {
                let result = replica
                    .encrypted_result_for(self.peer, &self.credentials, domain, sequence, &scopes)
                    .await?;
                self.send_result(replica, result, coordinator).await
            }
            Message::ResultPush { result } => {
                let scopes: Vec<_> = result
                    .parts
                    .iter()
                    .map(|part| part.scope)
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                replica
                    .authorize_command_transfer(self.peer, &self.credentials, &scopes)
                    .await?;
                replica
                    .receive_encrypted_result(result.clone(), coordinator)
                    .await?;
                self.send(&Message::ResultAck {
                    states: replica.result_ack(&result).await?,
                })
                .await
            }
            _ => Err(denied("unexpected native command request")),
        }
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
        match message {
            Message::Pull {
                scopes,
                after,
                quota,
            } => {
                // Leave room for canonical envelope and session signature overhead.
                if quota > MAX_SECURE_FRAME / 2 {
                    return Err(denied("native page quota"));
                }
                for scope in &scopes {
                    if replica.horizon_rebootstrap_required(*scope).await? {
                        return Err(denied(
                            "phase-3 rebootstrap required before serving row sync",
                        ));
                    }
                    if replica
                        .horizon_peer_behind(self.peer, &self.credentials, *scope)
                        .await?
                    {
                        return Err(denied("phase-3 rebootstrap required before sync or write"));
                    }
                }
                let page = replica
                    .transfer_page(self.peer, &self.credentials, &scopes, after, quota)
                    .await?;
                self.send(&Message::Page {
                    frames: page.frames,
                    next: page.next,
                    pruned: page.pruned,
                })
                .await
            }
            Message::MergeableCut { scope } => {
                let behind = replica
                    .horizon_peer_behind(self.peer, &self.credentials, scope)
                    .await?;
                let signed = replica.admitted_horizon(scope).await?;
                self.send(&Message::MergeableState { signed, behind }).await
            }
            Message::HorizonRejoined { scope, horizon } => {
                replica
                    .acknowledge_horizon_rebootstrap(self.peer, &self.credentials, scope, horizon)
                    .await?;
                self.send(&Message::Acknowledged).await
            }
            Message::HorizonAck { scope, ack } => {
                if ack.node != self.peer {
                    return Err(denied("horizon acknowledgment differs from session peer"));
                }
                replica.receive_horizon_ack(scope, ack).await?;
                self.send(&Message::Acknowledged).await
            }
            Message::Horizon { scope, after } => {
                let page = replica
                    .horizon_page(self.peer, &self.credentials, scope, after)
                    .await?;
                self.send(&Message::HorizonLogs { page }).await
            }
            Message::CheckpointOffer { scope } => {
                let signed = replica
                    .checkpoint_offer(self.peer, &self.credentials, scope)
                    .await?;
                self.send(&Message::Checkpoint {
                    signed: signed.map(Box::new),
                })
                .await
            }
            Message::CheckpointPull {
                scope,
                checkpoint,
                after,
                quota,
            } => {
                // Leave room for canonical envelope and session signature overhead.
                if quota > MAX_SECURE_FRAME / 2 - 4096 {
                    return Err(denied("native page quota"));
                }
                let page = replica
                    .checkpoint_page(
                        self.peer,
                        &self.credentials,
                        scope,
                        checkpoint,
                        after,
                        quota,
                    )
                    .await?;
                self.send(&Message::CheckpointRows { page }).await
            }
            Message::Ack { id, applied } => {
                replica
                    .acknowledge(self.peer, &self.credentials, id, applied)
                    .await?;
                self.send(&Message::Acknowledged).await
            }
            Message::Range {
                frontier,
                after,
                quota,
            } => {
                if quota > MAX_SECURE_FRAME / 2 {
                    return Err(denied("native page quota"));
                }
                if replica.horizon_rebootstrap_required(frontier.scope).await? {
                    return Err(denied(
                        "phase-3 rebootstrap required before serving row sync",
                    ));
                }
                if replica
                    .horizon_peer_behind(self.peer, &self.credentials, frontier.scope)
                    .await?
                {
                    return Err(denied("phase-3 rebootstrap required before sync or write"));
                }
                let page = replica
                    .range_page(self.peer, &self.credentials, &frontier, after, quota)
                    .await?;
                self.send(&Message::RangeFrames { page }).await
            }
            Message::Held { frontier } => {
                replica
                    .acknowledge_frontier(self.peer, &self.credentials, &frontier)
                    .await?;
                self.send(&Message::Acknowledged).await
            }
            Message::FrontiersQuery { frontiers } => {
                let states = replica
                    .exchange_frontiers(self.peer, &self.credentials, &frontiers)
                    .await?;
                self.send(&Message::Frontiers { states }).await
            }
            Message::FrontierQuery { scope } => {
                replica
                    .authorize_scope_peer(self.peer, &self.credentials, scope)
                    .await?;
                let frontier = replica.frontier(scope).await?;
                self.send(&Message::Frontier { frontier }).await
            }
            Message::Push { scope, frames } => {
                if replica
                    .horizon_peer_behind(self.peer, &self.credentials, scope)
                    .await?
                {
                    return Err(denied("phase-3 rebootstrap required before sync or write"));
                }
                replica
                    .authorize_scope_peer(self.peer, &self.credentials, scope)
                    .await?;
                if frames.len() > MAX_SECURE_PAGE {
                    return Err(denied("native page quota"));
                }
                for frame in &frames {
                    if frame.scope()? != scope {
                        return Err(denied("pushed frame outside its scope"));
                    }
                }
                for status in replica.receive_all(frames).await {
                    status?;
                }
                let frontier = replica.frontier(scope).await?;
                self.send(&Message::Frontier { frontier }).await
            }
            Message::ScopesQuery { after } => {
                let (scopes, next) = replica
                    .held_scopes(self.peer, &self.credentials, after)
                    .await?;
                self.send(&Message::Scopes { scopes, next }).await
            }
            Message::Read {
                scope,
                after,
                max_rows,
            } => {
                let package = replica
                    .remote_read(self.peer, &self.credentials, scope, after, max_rows)
                    .await?;
                self.send(&Message::ReadRows { package }).await
            }
            Message::ExistenceBaseOffer { scope, expected } => {
                let attachment = replica
                    .existence_base_at(self.peer, &self.credentials, scope, expected)
                    .await?
                    .map(Box::new);
                self.send(&Message::ExistenceBase { attachment }).await
            }
            Message::ExistenceBaseBegin { attachment } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, attachment.scope)
                    .await?;
                let (after, complete) = replica.begin_existence_base(*attachment).await?;
                self.send(&Message::ExistenceBaseCursor { after, complete })
                    .await
            }
            Message::ExistenceBaseStage { scope, page } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, scope)
                    .await?;
                replica.stage_existence_base(scope, page).await?;
                self.send(&Message::Acknowledged).await
            }
            Message::ExistenceBaseFinish { scope } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, scope)
                    .await?;
                replica.finish_existence_base(scope).await?;
                self.send(&Message::Acknowledged).await
            }
            Message::ExistenceBasePull { scope, base, after } => {
                let page = replica
                    .existence_base_page(
                        self.peer,
                        &self.credentials,
                        scope,
                        base,
                        after,
                        EXISTENCE_PAGE,
                    )
                    .await?;
                self.send(&Message::ExistenceBaseRows { page }).await
            }
            Message::ExistenceFrontier { scope } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, scope)
                    .await?;
                let frontier = replica.existence_frontier(scope).await?;
                self.send(&Message::ExistenceHeld { frontier }).await
            }
            Message::ExistencePull {
                scope,
                known,
                after,
            } => {
                if known.len() > MAX_EXISTENCE_SIGNERS {
                    return Err(denied("existence frontier quota"));
                }
                let page = replica
                    .existence_page(
                        self.peer,
                        &self.credentials,
                        scope,
                        &known,
                        after,
                        EXISTENCE_PAGE,
                    )
                    .await?;
                self.send(&Message::ExistenceRecords { page }).await
            }
            Message::ExistencePush { scope, records } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, scope)
                    .await?;
                if records.len() > EXISTENCE_PAGE
                    || records.iter().any(|record| record.scope() != scope)
                {
                    return Err(denied("existence push outside its log or quota"));
                }
                replica.receive_existence_all(records).await?;
                let frontier = replica.existence_frontier(scope).await?;
                self.send(&Message::ExistenceHeld { frontier }).await
            }
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
