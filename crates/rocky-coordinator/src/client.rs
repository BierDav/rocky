use crate::{
    CoordinationError, Refusal,
    certified::*,
    request::{Operation, Outcome, Reply, Request},
    transport::*,
};
use p2panda_core::PrivateKey;
use rocky_core::{
    net::{Protocol, Transport},
    *,
};
use rocky_credentials::{RevocationSet, SignedCapability};
use rocky_platform::time::{Duration, sleep, timeout};
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};
/// A low-level coordinator client that sends signed requests to the voters, leader first.
pub struct NetworkCoordinator {
    genesis: CertifiedGenesis,
    key: PrivateKey,
    certificate: Mutex<Option<SignedCapability>>,
    public_client_lane: Option<Arc<dyn Transport>>,
    preferred: Mutex<(Option<u64>, Option<u64>)>,
    observer: Mutex<Option<Observer>>,
}
/// Hears the outcome of every call, for health reporting.
pub type Observer = Arc<dyn Fn(Result<(), &CoordinationError>) + Send + Sync>;
/// How long a call waits by default before it is `Unavailable` or `OutcomeUnknown`.
pub const CALL_DEADLINE: Duration = Duration::from_secs(12);
const ATTEMPT_DEADLINE: Duration = Duration::from_secs(3);
const ROUNDS: usize = 3;
const PAUSE_BETWEEN_ROUNDS: Duration = Duration::from_millis(100);
#[cfg(target_family = "wasm")]
fn unrouted_browser_client() -> Result<Reply, CoordinationError> {
    Err(CoordinationError::Configuration)
}
fn undecided(result: &Result<Outcome, CoordinationError>) -> bool {
    matches!(
        result,
        Err(CoordinationError::Unavailable | CoordinationError::OutcomeUnknown)
    )
}
impl NetworkCoordinator {
    fn new(
        genesis: CertifiedGenesis,
        root: p2panda_core::PublicKey,
        mesh: MeshId,
        key: PrivateKey,
        public_client_lane: Option<Arc<dyn Transport>>,
    ) -> Result<Self, CoordinationError> {
        genesis.verify(root, mesh)?;
        Ok(Self {
            genesis,
            key,
            certificate: Mutex::new(None),
            public_client_lane,
            preferred: Mutex::new((None, None)),
            observer: Mutex::new(None),
        })
    }
    #[cfg(feature = "database")]
    pub(crate) fn certified_genesis(&self) -> &CertifiedGenesis {
        &self.genesis
    }
    /// The genesis id requests name.
    pub fn genesis(&self) -> Result<Commitment, CoordinationError> {
        self.genesis.id()
    }
    /// A client that reaches the voters a trust bundle names over `transport`.
    pub fn connect(
        bundle: &TrustBundle,
        key: PrivateKey,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, CoordinationError> {
        let genesis = bundle.genesis()?;
        let public = genesis
            .genesis
            .public
            .as_ref()
            .ok_or(CoordinationError::Configuration)?;
        if !public.endpoints.iter().any(|e| e.kind == transport.kind()) {
            return Err(CoordinationError::Configuration);
        }
        Self::new(genesis, bundle.root, bundle.mesh, key, Some(transport))
    }
    async fn converse_with(
        &self,
        voter: &Voter,
        genesis: Commitment,
        request: &Request,
        attempt_deadline: Duration,
        sent: &AtomicBool,
    ) -> Result<Reply, CoordinationError> {
        let message = || Message::Client(Box::new(request.clone()));
        let conversed = AtomicBool::new(false);
        let reply: Result<Reply, CoordinationError> =
            match (&self.public_client_lane, &self.genesis.genesis.public) {
                (Some(transport), Some(public)) => {
                    let mut reply = Err(CoordinationError::Unavailable);
                    for endpoint in public
                        .endpoints
                        .iter()
                        .filter(|e| e.voter == voter.id && e.kind == transport.kind())
                    {
                        reply = timeout(attempt_deadline, async {
                            let mut stream = transport
                                .connect(&endpoint.address, Protocol::Coordinate, None)
                                .await
                                .map_err(|_| CoordinationError::Unavailable)?;
                            conversed.store(true, Ordering::Relaxed);
                            converse(&mut stream, voter.key, genesis, &self.key, message()).await
                        })
                        .await
                        .unwrap_or(Err(CoordinationError::Unavailable));
                        if reply.is_ok() {
                            break;
                        }
                    }
                    reply
                }
                #[cfg(not(target_family = "wasm"))]
                _ => timeout(attempt_deadline, async {
                    let mut stream = tokio::net::TcpStream::connect(&voter.address)
                        .await
                        .map_err(|_| CoordinationError::Unavailable)?;
                    conversed.store(true, Ordering::Relaxed);
                    converse(&mut stream, voter.key, genesis, &self.key, message()).await
                })
                .await
                .unwrap_or(Err(CoordinationError::Unavailable)),
                #[cfg(target_family = "wasm")]
                _ => unrouted_browser_client(),
            };
        let unanswered = match &reply {
            Ok(reply) => reply.result == Err(CoordinationError::OutcomeUnknown),
            Err(_) => conversed.load(Ordering::Relaxed),
        };
        if unanswered {
            sent.store(true, Ordering::Relaxed);
        }
        reply
    }
    async fn attempt_leader_first(
        &self,
        request: &Request,
        attempt_deadline: Duration,
        sent: &AtomicBool,
    ) -> Option<Result<Outcome, CoordinationError>> {
        let genesis = match self.genesis.id() {
            Ok(id) => id,
            Err(error) => return Some(Err(error)),
        };
        let (last, leader) = *self
            .preferred
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut queue = Vec::new();
        for id in leader
            .into_iter()
            .chain(last)
            .chain(self.genesis.genesis.voters.iter().map(|v| v.id))
        {
            if !queue.contains(&id) {
                queue.push(id);
            }
        }
        let mut visited = Vec::new();
        while !queue.is_empty() {
            let id = queue.remove(0);
            if visited.contains(&id) {
                continue;
            }
            visited.push(id);
            let Ok(voter) = self.voter(id) else {
                continue;
            };
            let Ok(reply) = self
                .converse_with(voter, genesis, request, attempt_deadline, sent)
                .await
            else {
                continue;
            };
            *self
                .preferred
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = (Some(id), reply.leader);
            if let Some(hint) = reply.leader.filter(|hint| !visited.contains(hint)) {
                queue.retain(|id| *id != hint);
                queue.insert(0, hint);
            }
            if !undecided(&reply.result) {
                return Some(reply.result);
            }
        }
        None
    }
    /// Sends an operation under a request id, which makes a retry apply once.
    pub async fn call(
        &self,
        id: Commitment,
        operation: Operation,
    ) -> Result<Outcome, CoordinationError> {
        self.submit(&self.request(id, operation)?).await
    }
    /// Sends a prepared request within `CALL_DEADLINE`.
    pub async fn submit(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        self.submit_within(request, CALL_DEADLINE).await
    }
    /// Sends a prepared request within `deadline`; a change that may have reached a voter without
    /// an answer is `OutcomeUnknown`, never `Unavailable`.
    pub async fn submit_within(
        &self,
        request: &Request,
        deadline: Duration,
    ) -> Result<Outcome, CoordinationError> {
        request.within_client_limit()?;
        let result = self.send(request, deadline).await;
        let observer = self
            .observer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(observer) = observer {
            observer(result.as_ref().map(drop));
        }
        result
    }
    fn request(&self, id: Commitment, operation: Operation) -> Result<Request, CoordinationError> {
        let certificate = self
            .certificate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Request::certified(
            self.genesis.id()?,
            id,
            operation,
            (&self.key, certificate),
            now()?,
        )
    }
    async fn send(
        &self,
        request: &Request,
        deadline: Duration,
    ) -> Result<Outcome, CoordinationError> {
        let attempt_deadline = ATTEMPT_DEADLINE.min(deadline);
        let sent = AtomicBool::new(false);
        let answered = timeout(deadline, async {
            for _ in 0..ROUNDS {
                if let Some(result) = self
                    .attempt_leader_first(request, attempt_deadline, &sent)
                    .await
                {
                    return Some(result);
                }
                sleep(PAUSE_BETWEEN_ROUNDS).await;
            }
            None
        })
        .await;
        match answered {
            Ok(Some(result)) => result,
            _ if sent.load(Ordering::Relaxed) && !request.read_only() => {
                Err(CoordinationError::OutcomeUnknown)
            }
            _ => Err(CoordinationError::Unavailable),
        }
    }
    /// Pushes a database's revocations.
    pub async fn revoke(
        &self,
        scope: Scope,
        revocations: RevocationSet,
    ) -> Result<Commitment, CoordinationError> {
        match self
            .call(
                nonce(),
                Operation::Revoke {
                    scope,
                    revocations: Box::new(revocations),
                },
            )
            .await?
        {
            Outcome::Revoked(digest) => Ok(digest),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    fn voter(&self, id: u64) -> Result<&Voter, CoordinationError> {
        self.genesis
            .genesis
            .voters
            .iter()
            .find(|v| v.id == id)
            .ok_or(CoordinationError::Denied(Refusal::UnknownVoter))
    }
}
#[cfg(feature = "voter")]
impl NetworkCoordinator {
    /// A client that dials voters directly over TCP, for operators and voters.
    #[cfg(not(target_family = "wasm"))]
    pub fn open(
        genesis: CertifiedGenesis,
        root: p2panda_core::PublicKey,
        mesh: MeshId,
        key: PrivateKey,
    ) -> Result<Self, CoordinationError> {
        Self::new(genesis, root, mesh, key, None)
    }
    /// Sets the one observer of call outcomes.
    pub fn observe(&self, observer: Observer) {
        *self.observer.lock().unwrap_or_else(PoisonError::into_inner) = Some(observer);
    }
    /// Attaches the capability that certifies this client's key for later requests.
    pub fn certify(&self, certificate: SignedCapability) {
        *self
            .certificate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(certificate);
    }
    /// Registers a database through a public service.
    pub async fn register(
        &self,
        grant: crate::tenancy::SignedCoordinationGrant,
        admission: Option<crate::tenancy::SignedTenantAdmission>,
        certificate: ControlCertificate,
        id: Commitment,
    ) -> Result<Commitment, CoordinationError> {
        match self
            .call(
                id,
                Operation::Register(Box::new(crate::request::Registration {
                    grant,
                    admission,
                    certificate,
                })),
            )
            .await?
        {
            Outcome::Controlled(reference) => Ok(reference),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// Publishes a control certificate.
    pub async fn control(
        &self,
        certificate: ControlCertificate,
        id: Commitment,
    ) -> Result<Commitment, CoordinationError> {
        match self
            .call(id, Operation::Control(Box::new(certificate)))
            .await?
        {
            Outcome::Controlled(reference) => Ok(reference),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// Takes a counter's number for `claim`; a retry of the same claim answers what it found.
    pub async fn claim(
        &self,
        scope: Scope,
        counter: crate::counters::Name,
        claim: crate::counters::Claim,
    ) -> Result<crate::counters::Claimed, CoordinationError> {
        match self
            .call(
                nonce(),
                Operation::Claim {
                    scope,
                    counter,
                    claim,
                },
            )
            .await?
        {
            Outcome::Claimed(claimed) => Ok(claimed),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// Reads a counter's last number.
    pub async fn counter(
        &self,
        scope: Scope,
        counter: crate::counters::Name,
    ) -> Result<u64, CoordinationError> {
        match self
            .call(nonce(), Operation::Counter { scope, counter })
            .await?
        {
            Outcome::Counter(value) => Ok(value),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// What this client's tenant has stored.
    pub async fn usage(&self) -> Result<crate::request::Usage, CoordinationError> {
        match self.call(nonce(), Operation::Usage).await? {
            Outcome::Usage(usage) => Ok(usage),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// Closes a database's registration.
    pub async fn close_domain(
        &self,
        scope: Scope,
        id: Commitment,
    ) -> Result<(), CoordinationError> {
        match self.call(id, Operation::Close { scope }).await? {
            Outcome::Closed => Ok(()),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// One voter's health and metrics.
    #[cfg(not(target_family = "wasm"))]
    pub async fn status(&self, voter: u64) -> Result<crate::service::Status, CoordinationError> {
        exchange(
            self.voter(voter)?,
            self.genesis.id()?,
            &self.key,
            Message::Status,
        )
        .await
    }
    /// Starts the Raft group from one voter.
    #[cfg(not(target_family = "wasm"))]
    pub async fn initialize(&self, voter: u64) -> Result<(), CoordinationError> {
        exchange(
            self.voter(voter)?,
            self.genesis.id()?,
            &self.key,
            Message::Initialize,
        )
        .await?
    }
    /// Asks one voter to snapshot its state now.
    #[cfg(not(target_family = "wasm"))]
    pub async fn snapshot(&self, voter: u64) -> Result<(), CoordinationError> {
        exchange(
            self.voter(voter)?,
            self.genesis.id()?,
            &self.key,
            Message::SnapshotNow,
        )
        .await?
    }
}
#[cfg(test)]
pub(crate) mod tests;
