use crate::{
    CoordinationError, Refusal,
    tenancy::{SignedCoordinationGrant, SignedTenantAdmission},
};
use crate::{
    certified::*,
    machine::Request,
    service::{Operation, Outcome, Registration, Reply},
    transport::*,
};
use p2panda_core::{PrivateKey, PublicKey};
use rocky_core::{
    net::{Protocol, Transport},
    *,
};
use rocky_credentials::{RevocationSet, SignedCapability};
use rocky_platform::time::{Duration, sleep, timeout};
use std::sync::{Arc, Mutex, PoisonError};
pub struct NetworkCoordinator {
    genesis: CertifiedGenesis,
    key: PrivateKey,
    certificate: Mutex<Option<SignedCapability>>,
    public_client_lane: Option<Arc<dyn Transport>>,
    preferred: Mutex<(Option<u64>, Option<u64>)>,
    observer: Mutex<Option<Observer>>,
}
pub type Observer = Arc<dyn Fn(Result<(), &CoordinationError>) + Send + Sync>;
pub const CALL_DEADLINE: Duration = Duration::from_secs(12);
const ATTEMPT_DEADLINE: Duration = Duration::from_secs(3);
const PAUSE_BETWEEN_ROUNDS: Duration = Duration::from_millis(100);
#[cfg(target_family = "wasm")]
fn unrouted_browser_client() -> Result<Reply, CoordinationError> {
    Err(CoordinationError::Configuration)
}
impl NetworkCoordinator {
    #[cfg(not(target_family = "wasm"))]
    pub fn open(
        genesis: CertifiedGenesis,
        root: PublicKey,
        mesh: MeshId,
        key: PrivateKey,
    ) -> Result<Self, CoordinationError> {
        Self::new(genesis, root, mesh, key, None)
    }
    fn new(
        genesis: CertifiedGenesis,
        root: PublicKey,
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
    pub fn observe(&self, observer: Observer) {
        *self.observer.lock().unwrap_or_else(PoisonError::into_inner) = Some(observer);
    }
    pub fn certify(&self, certificate: SignedCapability) {
        *self
            .certificate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(certificate);
    }
    #[cfg(feature = "database")]
    pub(crate) fn certified_genesis(&self) -> &CertifiedGenesis {
        &self.genesis
    }
    pub fn genesis(&self) -> Result<Commitment, CoordinationError> {
        self.genesis.id()
    }
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
    async fn attempt_leader_first(
        &self,
        request: &Request,
        attempt_deadline: Duration,
    ) -> Vec<Result<Reply, CoordinationError>> {
        let genesis = match self.genesis.id() {
            Ok(id) => id,
            Err(error) => return vec![Err(error)],
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
        let mut replies = Vec::new();
        while !queue.is_empty() {
            let id = queue.remove(0);
            if visited.contains(&id) {
                continue;
            }
            visited.push(id);
            let Ok(voter) = self.voter(id) else {
                continue;
            };
            let message = || Message::Client(Box::new(request.clone()));
            let reply = match (&self.public_client_lane, &self.genesis.genesis.public) {
                (Some(transport), Some(public)) => {
                    let mut reply: Result<Reply, CoordinationError> =
                        Err(CoordinationError::Unavailable);
                    for endpoint in public
                        .endpoints
                        .iter()
                        .filter(|e| e.voter == id && e.kind == transport.kind())
                    {
                        reply = timeout(attempt_deadline, async {
                            let mut stream = transport
                                .connect(&endpoint.address, Protocol::Coordinate, None)
                                .await
                                .map_err(|_| CoordinationError::Unavailable)?;
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
                _ => exchange(voter, genesis, &self.key, message()).await,
                #[cfg(target_family = "wasm")]
                _ => unrouted_browser_client(),
            };
            let done = if let Ok(reply) = &reply {
                *self
                    .preferred
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = (Some(id), reply.leader);
                if let Some(hint) = reply.leader.filter(|hint| !visited.contains(hint)) {
                    queue.retain(|id| *id != hint);
                    queue.insert(0, hint);
                }
                reply.result != Err(CoordinationError::Unavailable)
            } else {
                false
            };
            replies.push(reply);
            if done {
                break;
            }
        }
        replies
    }
    pub async fn call(
        &self,
        id: Commitment,
        operation: Operation,
    ) -> Result<Outcome, CoordinationError> {
        self.submit(&self.request(id, operation)?).await
    }
    pub async fn submit(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        self.submit_within(request, CALL_DEADLINE).await
    }
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
        timeout(deadline, async {
            for _ in 0..3 {
                for response in self.attempt_leader_first(request, attempt_deadline).await {
                    match response {
                        Ok(reply) if reply.result != Err(CoordinationError::Unavailable) => {
                            return reply.result;
                        }
                        _ => continue,
                    }
                }
                sleep(PAUSE_BETWEEN_ROUNDS).await;
            }
            Err(CoordinationError::Unavailable)
        })
        .await
        .map_err(|_| CoordinationError::Unavailable)?
    }
    pub async fn register(
        &self,
        grant: SignedCoordinationGrant,
        admission: Option<SignedTenantAdmission>,
        certificate: ControlCertificate,
        id: Commitment,
    ) -> Result<Commitment, CoordinationError> {
        match self
            .call(
                id,
                Operation::Register(Box::new(Registration {
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
    pub async fn claim(
        &self,
        scope: Scope,
        counter: crate::counters::Name,
        claim: crate::counters::Claim,
    ) -> Result<u64, CoordinationError> {
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
            Outcome::Claimed(value) => Ok(value),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    pub async fn counter(
        &self,
        scope: Scope,
        counter: crate::counters::Name,
        claim: Option<crate::counters::Claim>,
    ) -> Result<crate::counters::Reading, CoordinationError> {
        match self
            .call(
                nonce(),
                Operation::Counter {
                    scope,
                    counter,
                    claim,
                },
            )
            .await?
        {
            Outcome::Counter(reading) => Ok(reading),
            _ => Err(CoordinationError::Corrupt),
        }
    }
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
    pub async fn usage(&self) -> Result<crate::service::Usage, CoordinationError> {
        match self.call(nonce(), Operation::Usage).await? {
            Outcome::Usage(usage) => Ok(usage),
            _ => Err(CoordinationError::Corrupt),
        }
    }
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
    fn voter(&self, id: u64) -> Result<&Voter, CoordinationError> {
        self.genesis
            .genesis
            .voters
            .iter()
            .find(|v| v.id == id)
            .ok_or(CoordinationError::Denied(Refusal::UnknownVoter))
    }
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
