pub use crate::raft_store::{Backup, Fence};
use crate::{raft_store::*, transport::*};
use openraft::{
    BasicNode, LogId, RaftMetrics, SnapshotPolicy,
    error::{ClientWriteError, Fatal, RaftError},
    raft::ClientWriteResponse,
};
use p2panda_core::{PrivateKey, PublicKey};
use rocky_coordinator::{CoordinationError, Refusal};
use rocky_coordinator::{
    certified::*,
    protocol::{Reply, Request},
    service::*,
};
use rocky_core::{
    net::{Protocol, Transport},
    *,
};
use std::{
    collections::{BTreeSet, HashMap},
    convert::Infallible,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
    sync::Semaphore,
};

const READ_WITHOUT_LOG_MAX_AUTHORITY_AGE_SECONDS: u64 = 30;

const PAUSE_AFTER_FAILED_ACCEPT: std::time::Duration = std::time::Duration::from_millis(50);

const VOTER_CONNECTION_SLOTS: usize = 64;

const VOTER_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

const PROPOSAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

const HEARTBEAT_INTERVAL_MILLIS: u64 = 100;

const ELECTION_TIMEOUT_MIN_MILLIS: u64 = 350;

const ELECTION_TIMEOUT_MAX_MILLIS: u64 = 700;

const OPENRAFT_TICK_MILLIS: u64 = HEARTBEAT_INTERVAL_MILLIS * 3 / 2;

const SLOW_HEARTBEAT_ANSWER_MILLIS: u64 = 2_500;

const QUORUM_ACK_LEASE_MILLIS: u64 = OPENRAFT_TICK_MILLIS + SLOW_HEARTBEAT_ANSWER_MILLIS;

const _: () = assert!(
    SLOW_HEARTBEAT_ANSWER_MILLIS > ELECTION_TIMEOUT_MAX_MILLIS,
    "a heartbeat answer pays for a new connection and a signed round trip, so a leader its followers still follow refuses no writes"
);

fn quorum_acknowledged(metrics: &RaftMetrics<u64, BasicNode>) -> bool {
    metrics
        .millis_since_quorum_ack
        .is_some_and(|millis| millis < QUORUM_ACK_LEASE_MILLIS)
}

#[derive(Debug)]
pub enum ServeError {
    Coordination(CoordinationError),
    ConsensusHalted(Box<Fatal<u64>>),
}
impl From<CoordinationError> for ServeError {
    fn from(error: CoordinationError) -> Self {
        Self::Coordination(error)
    }
}
impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Coordination(error) => write!(f, "{error}"),
            Self::ConsensusHalted(fatal) => write!(f, "consensus halted: {fatal}"),
        }
    }
}
impl std::error::Error for ServeError {}

#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    reads: AtomicU64,
    busy: AtomicU64,
    denied: AtomicU64,
    unavailable: AtomicU64,
}
impl Counters {
    fn record(&self, result: &Result<Outcome, CoordinationError>) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        match result {
            Err(CoordinationError::Denied(_)) => self.denied.fetch_add(1, Ordering::Relaxed),
            Err(CoordinationError::Unavailable | CoordinationError::OutcomeUnknown) => {
                self.unavailable.fetch_add(1, Ordering::Relaxed)
            }
            _ => 0,
        };
    }
    fn connection_slot(&self, slots: &Arc<Semaphore>) -> Option<tokio::sync::OwnedSemaphorePermit> {
        match slots.clone().try_acquire_owned() {
            Ok(slot) => Some(slot),
            Err(_) => {
                self.busy.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }
    fn snapshot(&self) -> Metrics {
        Metrics {
            requests: self.requests.load(Ordering::Relaxed),
            reads: self.reads.load(Ordering::Relaxed),
            busy: self.busy.load(Ordering::Relaxed),
            denied: self.denied.load(Ordering::Relaxed),
            unavailable: self.unavailable.load(Ordering::Relaxed),
        }
    }
}
pub(crate) fn log_lifecycle(node: u64, event: &str) {
    tracing::info!(node, event, "rocky-voter lifecycle");
}

pub struct VoterService {
    raft: Consensus,
    store: Store,
    genesis: CertifiedGenesis,
    key: PrivateKey,
    node: u64,
    counters: Counters,
    admission: Admission,
    fenced: AtomicBool,
}
mod clients;
use clients::Admission;
impl VoterService {
    pub async fn open(
        path: &Path,
        genesis: CertifiedGenesis,
        root: PublicKey,
        mesh: MeshId,
        node: u64,
        key: PrivateKey,
        provision: bool,
    ) -> Result<Arc<Self>, CoordinationError> {
        genesis.verify(root, mesh)?;
        if !genesis
            .genesis
            .voters
            .iter()
            .any(|v| v.id == node && v.key == key.public_key())
        {
            return Err(CoordinationError::Denied(Refusal::UnknownVoter));
        }
        let store = Store::open(path, genesis.clone(), provision, genesis.limits())?;
        Self::start(store, genesis, node, key, Route::Tcp).await
    }
    async fn start(
        store: Store,
        genesis: CertifiedGenesis,
        node: u64,
        key: PrivateKey,
        route: Route,
    ) -> Result<Arc<Self>, CoordinationError> {
        store.set_node(node);
        let fence = store.fence().await;
        let fenced_after_restore = fence.is_some();
        let config = openraft::Config {
            cluster_name: format!("{:?}", genesis.id()?),
            heartbeat_interval: HEARTBEAT_INTERVAL_MILLIS,
            election_timeout_min: ELECTION_TIMEOUT_MIN_MILLIS,
            election_timeout_max: ELECTION_TIMEOUT_MAX_MILLIS,
            snapshot_policy: SnapshotPolicy::LogsSinceLast(1024),
            max_in_snapshot_log_to_keep: 8,
            snapshot_max_chunk_size: 64 * 1024,
            max_payload_entries: 8,
            enable_elect: !fenced_after_restore,
            ..Default::default()
        }
        .validate()
        .map_err(|_| CoordinationError::Configuration)?;
        let raft = Consensus::new(
            node,
            Arc::new(config),
            Network {
                genesis: genesis.clone(),
                key: key.clone(),
                route,
            },
            store.clone(),
            store.clone(),
        )
        .await
        .map_err(|_| CoordinationError::Corrupt)?;
        let service = Arc::new(Self {
            raft,
            admission: Admission::new(&genesis.genesis),
            store,
            genesis,
            key,
            node,
            counters: Counters::default(),
            fenced: AtomicBool::new(fenced_after_restore),
        });
        log_lifecycle(
            node,
            if fenced_after_restore {
                "opened, fenced after restore"
            } else {
                "opened"
            },
        );
        if fenced_after_restore {
            tokio::spawn(service.clone().await_readmission());
        }
        Ok(service)
    }
    async fn await_readmission(self: Arc<Self>) {
        let mut metrics = self.raft.metrics();
        loop {
            let readmitted = {
                let m = metrics.borrow();
                let membership = &m.membership_config;
                membership.voter_ids().any(|id| id == self.node)
                    && m.last_applied.map(|id| id.index) >= membership.log_id().map(|id| id.index)
            };
            if readmitted
                && let Some(fence) = self.store.fence().await
                && fence.learner
                && self
                    .raft
                    .metrics()
                    .borrow()
                    .membership_config
                    .log_id()
                    .is_some_and(|id| id.index > fence.index)
                && self.store.lift_fence().await.is_ok()
            {
                self.fenced.store(false, Ordering::SeqCst);
                self.raft.runtime_config().elect(true);
                log_lifecycle(self.node, "re-added after restore; fence lifted");
                return;
            }
            if metrics.changed().await.is_err() {
                return;
            }
        }
    }
    pub async fn leads_a_quorum(&self) -> bool {
        let node = self.node;
        self.raft
            .metrics()
            .wait_for(|metrics| {
                metrics.current_leader == Some(node) && quorum_acknowledged(metrics)
            })
            .await
            .is_ok()
    }
    pub async fn shutdown(&self) {
        if self.raft.shutdown().await.is_err() {
            log_lifecycle(self.node, "consensus had already stopped");
        }
    }
    fn leader_hint(&self) -> Option<u64> {
        let metrics = self.raft.metrics();
        let metrics = metrics.borrow();
        metrics
            .current_leader
            .filter(|leader| *leader != self.node || quorum_acknowledged(&metrics))
    }
    fn refuse_unadmitted(&self, error: CoordinationError) -> Reply {
        let reply = self.refuse(error);
        self.counters.record(&reply.result);
        reply
    }
    fn refuse(&self, error: CoordinationError) -> Reply {
        Reply {
            leader: self.leader_hint(),
            result: Err(error),
        }
    }
    async fn propose(
        &self,
        request: Request,
    ) -> Result<ClientWriteResponse<Types>, CoordinationError> {
        if !quorum_acknowledged(&self.raft.metrics().borrow()) {
            return Err(CoordinationError::Unavailable);
        }
        let proposal = Proposal {
            time: now()?,
            request,
        };
        match tokio::time::timeout(PROPOSAL_TIMEOUT, self.raft.client_write(proposal)).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(RaftError::APIError(ClientWriteError::ForwardToLeader(_)))) => {
                Err(CoordinationError::Unavailable)
            }
            Ok(Err(_)) | Err(_) => Err(CoordinationError::OutcomeUnknown),
        }
    }
    async fn consensus_halted(&self) -> Fatal<u64> {
        let mut metrics = self.raft.metrics();
        metrics
            .wait_for(|metrics| metrics.running_state.is_err())
            .await
            .ok()
            .and_then(|metrics| metrics.running_state.clone().err())
            .unwrap_or(Fatal::Stopped)
    }
    async fn answer(&self, request: Request) -> Reply {
        let reply = self.client(request).await;
        self.counters.record(&reply.result);
        reply
    }
    async fn read(
        &self,
        request: &Request,
    ) -> Option<Result<(Result<Outcome, CoordinationError>, LogId<u64>), CoordinationError>> {
        if !request.read_only()
            || now()
                .ok()?
                .saturating_sub(self.store.authority_time().await)
                > READ_WITHOUT_LOG_MAX_AUTHORITY_AGE_SECONDS
        {
            return None;
        }
        let barrier = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.raft.ensure_linearizable(),
        )
        .await;
        let Ok(Ok(Some(log))) = barrier else {
            return Some(Err(CoordinationError::Unavailable));
        };
        self.counters.reads.fetch_add(1, Ordering::Relaxed);
        Some(Ok((self.store.read(request).await, log)))
    }
    async fn client(&self, request: Request) -> Reply {
        let result = async {
            request.authenticate(self.genesis.id()?)?;
            request.fresh()?;
            request.within_client_limit()?;
            self.store.preflight(&request).await?;
            if let Some(read) = self.read(&request).await {
                return read;
            }
            let response = self.propose(request).await?;
            if let Ok(Outcome::Membership(voters)) = &response.data {
                self.complete_membership(voters)
                    .await
                    .map_err(|_| CoordinationError::OutcomeUnknown)?;
            }
            Ok((response.data, response.log_id))
        }
        .await;
        match result {
            Err(error) => self.refuse(error),
            Ok((data, _)) => {
                crash("reply.before_send");
                Reply {
                    leader: self.leader_hint(),
                    result: data,
                }
            }
        }
    }
    async fn complete_membership(&self, voters: &BTreeSet<u64>) -> Result<(), CoordinationError> {
        let is_durable_target = &self.store.desired_voters().await == voters;
        if !is_durable_target {
            return Ok(());
        }
        let membership = self.raft.metrics().borrow().membership_config.clone();
        let already_applied = membership.membership().get_joint_config().len() == 1
            && membership.voter_ids().collect::<BTreeSet<_>>() == *voters;
        if already_applied {
            return Ok(());
        }
        for id in voters {
            let voter = self
                .genesis
                .genesis
                .voters
                .iter()
                .find(|v| v.id == *id)
                .ok_or(CoordinationError::Configuration)?;
            self.raft
                .add_learner(*id, BasicNode::new(voter.address.clone()), true)
                .await
                .map_err(|_| CoordinationError::Unavailable)?;
        }
        self.raft
            .change_membership(voters.clone(), false)
            .await
            .map_err(|_| CoordinationError::Unavailable)?;
        Ok(())
    }
    pub async fn serve(self: Arc<Self>) -> Result<Infallible, ServeError> {
        let voter = self
            .genesis
            .genesis
            .voters
            .iter()
            .find(|v| v.id == self.node)
            .ok_or(CoordinationError::Configuration)?;
        let listener = TcpListener::bind(&voter.address).await.map_err(|error| {
            log_lifecycle(
                self.node,
                &format!("bind voters at {}: {error}", voter.address),
            );
            CoordinationError::Unavailable
        })?;
        log_lifecycle(self.node, &format!("serving voters at {}", voter.address));
        self.serve_voters(listener).await
    }
    pub async fn serve_voters(
        self: Arc<Self>,
        listener: TcpListener,
    ) -> Result<Infallible, ServeError> {
        let slots = Arc::new(Semaphore::new(VOTER_CONNECTION_SLOTS));
        let halted = self.consensus_halted();
        tokio::pin!(halted);
        loop {
            let accepted = tokio::select! {
                fatal = &mut halted => {
                    log_lifecycle(self.node, &format!("consensus halted: {fatal}"));
                    return Err(ServeError::ConsensusHalted(Box::new(fatal)));
                }
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, _)) = accepted else {
                tokio::time::sleep(PAUSE_AFTER_FAILED_ACCEPT).await;
                continue;
            };
            let Some(slot) = self.counters.connection_slot(&slots) else {
                continue;
            };
            let service = self.clone();
            tokio::spawn(async move {
                let _slot = slot;
                let _ =
                    tokio::time::timeout(VOTER_REQUEST_TIMEOUT, service.answer_voter(stream)).await;
            });
        }
    }
    pub(crate) async fn answer_voter(
        self: Arc<Self>,
        mut stream: impl AsyncRead + AsyncWrite + Unpin,
    ) -> Result<(), CoordinationError> {
        let envelope = read(&mut stream).await?;
        envelope.authenticate(self.genesis.id()?)?;
        let message: Message = decode(&envelope.body)?;
        if !matches!(message, Message::Client(_)) {
            envelope.verify(self.genesis.id()?)?;
        }
        let is_voter = self
            .genesis
            .genesis
            .voters
            .iter()
            .any(|v| v.key == envelope.key);
        let sender = self
            .genesis
            .genesis
            .voters
            .iter()
            .find(|v| v.key == envelope.key)
            .map(|v| v.id);
        let membership = self.raft.metrics().borrow().membership_config.clone();
        let peer = is_voter
            && (membership.nodes().next().is_none()
                || sender.is_some_and(|id| membership.membership().get_node(&id).is_some()));
        let administrative = envelope.key == self.genesis.genesis.membership_authority;
        let tenants_use_client_lane = self.genesis.genesis.public.is_some();
        let body = match message {
            Message::Client(request) if !tenants_use_client_lane || administrative => {
                let reply = if administrative {
                    self.administer(*request).await
                } else {
                    self.admit(*request, None).await
                };
                encode(&reply)?
            }
            Message::Append(rpc) if peer && sender == Some(rpc.vote.leader_id.node_id) => {
                encode(&self.raft.append_entries(rpc).await)?
            }
            Message::Vote(rpc)
                if peer
                    && sender == Some(rpc.vote.leader_id.node_id)
                    && !self.fenced.load(Ordering::SeqCst) =>
            {
                encode(&self.raft.vote(rpc).await)?
            }
            Message::Snapshot(rpc) if peer && sender == Some(rpc.vote.leader_id.node_id) => {
                if rpc.offset.saturating_add(rpc.data.len() as u64) > MAX_STORE as u64 {
                    return Err(CoordinationError::Backpressure);
                }
                encode(&self.raft.install_snapshot(rpc).await)?
            }
            Message::Status if administrative || is_voter => {
                let metrics = self.raft.metrics().borrow().clone();
                let footprint = self.store.footprint().await?;
                encode(&Status {
                    logs: footprint.logs,
                    snapshots: footprint.snapshots,
                    disk_bytes: footprint.disk_bytes,
                    allocated_bytes: footprint.allocated_bytes,
                    state_bytes: footprint.state_bytes,
                    retry_records: footprint.retry_records,
                    tenants: self.store.tenants().await,
                    fenced: self.fenced.load(Ordering::SeqCst),
                    metrics: self.counters.snapshot(),
                    node: self.node,
                    leader: metrics.current_leader,
                    applied: metrics.last_applied.map(|id| id.index),
                    voters: metrics.membership_config.voter_ids().collect(),
                    snapshot: metrics.snapshot.map(|id| id.index),
                    health: format!("{:?}: {:?}", metrics.state, metrics.running_state),
                })?
            }
            Message::SnapshotNow if administrative => encode(
                &self
                    .raft
                    .trigger()
                    .snapshot()
                    .await
                    .map_err(|_| CoordinationError::Unavailable),
            )?,
            Message::Initialize if administrative => {
                let members: std::collections::BTreeMap<_, _> = self
                    .genesis
                    .genesis
                    .voters
                    .iter()
                    .filter(|v| self.genesis.genesis.initial_voters.contains(&v.id))
                    .map(|v| (v.id, BasicNode::new(v.address.clone())))
                    .collect();
                encode(
                    &self
                        .raft
                        .initialize(members)
                        .await
                        .map_err(|_| CoordinationError::Configuration),
                )?
            }
            Message::Client(_) => encode(
                &self.refuse_unadmitted(CoordinationError::Denied(Refusal::UnexpectedMessage)),
            )?,
            _ => return Err(CoordinationError::Denied(Refusal::UnexpectedMessage)),
        };
        let response = crate::transport::Envelope::from_body(
            self.genesis.id()?,
            envelope.nonce,
            body,
            &self.key,
        )?;
        write(&mut stream, &response).await
    }
}
pub fn backup(
    path: &Path,
    genesis: CertifiedGenesis,
    node: u64,
    out: &Path,
) -> Result<Backup, CoordinationError> {
    Store::backup(path, genesis, node, out)
}
pub fn restore(
    backup: &Path,
    genesis: &CertifiedGenesis,
    node: u64,
    path: &Path,
) -> Result<Backup, CoordinationError> {
    Store::restore(backup, genesis, node, path)
}

#[cfg(test)]
mod tests;
