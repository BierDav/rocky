pub use crate::raft_store::{Backup, Fence};
use crate::{raft_store::*, transport::*};
use openraft::{BasicNode, LogId, SnapshotPolicy};
use p2panda_core::{PrivateKey, PublicKey};
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
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{net::TcpListener, sync::Semaphore};

const READ_WITHOUT_LOG_MAX_AUTHORITY_AGE_SECONDS: u64 = 30;

const PAUSE_AFTER_FAILED_ACCEPT: std::time::Duration = std::time::Duration::from_millis(50);

const VOTER_CONNECTION_SLOTS: usize = 64;

const VOTER_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
            Err(CoordinationError::Denied) => self.denied.fetch_add(1, Ordering::Relaxed),
            Err(CoordinationError::Unavailable) => self.unavailable.fetch_add(1, Ordering::Relaxed),
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
    eprintln!("rocky-voter {node}: {event}");
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
    acknowledgments: tokio::sync::Mutex<HashMap<TipLog, VolatileAcknowledgments>>,
}
mod clients;
mod coordinated;
use clients::Admission;
use coordinated::VolatileAcknowledgments;
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
            return Err(CoordinationError::Denied);
        }
        let store = Store::open(path, genesis.clone(), provision, genesis.limits())?;
        store.set_node(node);
        let fence = store.fence().await;
        let fenced_after_restore = fence.is_some();
        let config = openraft::Config {
            cluster_name: format!("{:?}", genesis.id()?),
            heartbeat_interval: 100,
            election_timeout_min: 350,
            election_timeout_max: 700,
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
            acknowledgments: Default::default(),
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
        Self::watch_log_deadlines(&service);
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
    fn refuse(&self, error: CoordinationError) -> Reply {
        Reply {
            leader: self.raft.metrics().borrow().current_leader,
            result: Err(error),
        }
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
            if let Operation::Coordinated {
                log,
                action: rocky_coordinator::coordinated::Action::Acknowledge { pending },
            } = &request.operation
            {
                let outcome = self.acknowledge_log(&request, *log, pending).await?;
                let log = self
                    .raft
                    .metrics()
                    .borrow()
                    .last_applied
                    .ok_or(CoordinationError::Unavailable)?;
                return Ok((Ok(outcome), log));
            }
            if let Operation::Coordinated {
                log,
                action: rocky_coordinator::coordinated::Action::Status { position, hash },
            } = &request.operation
            {
                let outcome = self
                    .pending_status(&request, *log, *position, *hash)
                    .await?;
                let applied = self
                    .raft
                    .metrics()
                    .borrow()
                    .last_applied
                    .ok_or(CoordinationError::Unavailable)?;
                return Ok((Ok(Outcome::CoordinatedStatus(outcome)), applied));
            }
            if let Some(read) = self.read(&request).await {
                return read;
            }
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                self.raft.client_write(Proposal {
                    time: now()?,
                    request,
                }),
            )
            .await
            .map_err(|_| CoordinationError::Unavailable)?
            .map_err(|_| CoordinationError::Unavailable)?;
            if let Ok(Outcome::Membership(voters)) = &response.data {
                self.complete_membership(voters).await?;
            }
            Ok((response.data, response.log_id))
        }
        .await;
        match result {
            Err(error) => self.refuse(error),
            Ok((data, _)) => {
                crash("reply.before_send");
                Reply {
                    leader: self.raft.metrics().borrow().current_leader,
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
    pub async fn serve(self: Arc<Self>) -> Result<(), CoordinationError> {
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
    async fn serve_voters(self: Arc<Self>, listener: TcpListener) -> Result<(), CoordinationError> {
        let slots = Arc::new(Semaphore::new(VOTER_CONNECTION_SLOTS));
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                tokio::time::sleep(PAUSE_AFTER_FAILED_ACCEPT).await;
                continue;
            };
            let Some(slot) = self.counters.connection_slot(&slots) else {
                continue;
            };
            let service = self.clone();
            tokio::spawn(async move {
                let _slot = slot;
                let _ = tokio::time::timeout(VOTER_REQUEST_TIMEOUT, async {
                    let envelope = read(&mut stream).await?;
                    envelope.authenticate(service.genesis.id()?)?;
                    let message: Message = decode(&envelope.body)?;
                    if !matches!(message, Message::Client(_)) {
                        envelope.verify(service.genesis.id()?)?;
                    }
                    let is_voter = service
                        .genesis
                        .genesis
                        .voters
                        .iter()
                        .any(|v| v.key == envelope.key);
                    let sender = service
                        .genesis
                        .genesis
                        .voters
                        .iter()
                        .find(|v| v.key == envelope.key)
                        .map(|v| v.id);
                    let membership = service.raft.metrics().borrow().membership_config.clone();
                    let peer = is_voter
                        && (membership.nodes().next().is_none()
                            || sender
                                .is_some_and(|id| membership.membership().get_node(&id).is_some()));
                    let administrative =
                        envelope.key == service.genesis.genesis.membership_authority;
                    let tenants_use_client_lane = service.genesis.genesis.public.is_some();
                    let body = match message {
                        Message::Client(request) if !tenants_use_client_lane || administrative => {
                            if request.key != envelope.key {
                                return Err(CoordinationError::Denied);
                            }
                            let reply = if administrative {
                                service.administer(*request).await
                            } else {
                                service.admit(*request, None).await
                            };
                            encode(&reply)?
                        }
                        Message::Append(rpc)
                            if peer && sender == Some(rpc.vote.leader_id.node_id) =>
                        {
                            encode(&service.raft.append_entries(rpc).await)?
                        }
                        Message::Vote(rpc)
                            if peer
                                && sender == Some(rpc.vote.leader_id.node_id)
                                && !service.fenced.load(Ordering::SeqCst) =>
                        {
                            encode(&service.raft.vote(rpc).await)?
                        }
                        Message::Snapshot(rpc)
                            if peer && sender == Some(rpc.vote.leader_id.node_id) =>
                        {
                            if rpc.offset.saturating_add(rpc.data.len() as u64) > MAX_STORE as u64 {
                                return Err(CoordinationError::Backpressure);
                            }
                            encode(&service.raft.install_snapshot(rpc).await)?
                        }
                        Message::Status if administrative || is_voter => {
                            let metrics = service.raft.metrics().borrow().clone();
                            let footprint = service.store.footprint().await?;
                            encode(&Status {
                                logs: footprint.logs,
                                snapshots: footprint.snapshots,
                                disk_bytes: footprint.disk_bytes,
                                allocated_bytes: footprint.allocated_bytes,
                                state_bytes: footprint.state_bytes,
                                retry_records: footprint.retry_records,
                                tenants: service.store.tenants().await,
                                fenced: service.fenced.load(Ordering::SeqCst),
                                metrics: service.counters.snapshot(),
                                node: service.node,
                                leader: metrics.current_leader,
                                applied: metrics.last_applied.map(|id| id.index),
                                voters: metrics.membership_config.voter_ids().collect(),
                                snapshot: metrics.snapshot.map(|id| id.index),
                                health: format!("{:?}: {:?}", metrics.state, metrics.running_state),
                            })?
                        }
                        Message::SnapshotNow if administrative => encode(
                            &service
                                .raft
                                .trigger()
                                .snapshot()
                                .await
                                .map_err(|_| CoordinationError::Unavailable),
                        )?,
                        Message::Initialize if administrative => {
                            let members: std::collections::BTreeMap<_, _> = service
                                .genesis
                                .genesis
                                .voters
                                .iter()
                                .filter(|v| service.genesis.genesis.initial_voters.contains(&v.id))
                                .map(|v| (v.id, BasicNode::new(v.address.clone())))
                                .collect();
                            encode(
                                &service
                                    .raft
                                    .initialize(members)
                                    .await
                                    .map_err(|_| CoordinationError::Configuration),
                            )?
                        }
                        _ => return Err(CoordinationError::Denied),
                    };
                    let response = crate::transport::Envelope::from_body(
                        service.genesis.id()?,
                        envelope.nonce,
                        body,
                        &service.key,
                    )?;
                    write(&mut stream, &response).await
                })
                .await;
            });
        }
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
