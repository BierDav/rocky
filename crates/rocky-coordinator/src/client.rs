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
use rocky_credentials::{
    CredentialChain, RevocationSet, SignedCapability, SignedCoordinationGrant,
    SignedTenantAdmission,
};
use rocky_platform::time::{Duration, sleep, timeout};
use std::sync::{Arc, Mutex, PoisonError};
pub struct NetworkCoordinator {
    genesis: CertifiedGenesis,
    key: PrivateKey,
    certificate: Mutex<Option<SignedCapability>>,
    claims: Mutex<Option<CredentialChain>>,
    public_client_lane: Option<Arc<dyn Transport>>,
    preferred: Mutex<(Option<u64>, Option<u64>)>,
    observer: Mutex<Option<Observer>>,
    identity: CoordinatorIdentity,
}
pub use rocky_credentials::CoordinationObserver as Observer;
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
        let identity = CoordinatorIdentity {
            root: *root.as_bytes(),
            mesh,
            genesis: genesis.id()?,
        };
        Ok(Self {
            identity,
            genesis,
            key,
            certificate: Mutex::new(None),
            claims: Mutex::new(None),
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
    pub fn present_claims(&self, claims: Option<CredentialChain>) {
        *self.claims.lock().unwrap_or_else(PoisonError::into_inner) = claims;
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
                        reply = timeout(Duration::from_secs(3), async {
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
        let result = self.request(id, operation).await;
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
    async fn request(
        &self,
        id: Commitment,
        operation: Operation,
    ) -> Result<Outcome, CoordinationError> {
        let certificate = self
            .certificate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let claims = match operation {
            Operation::Claim { .. } | Operation::Counter { .. } => self
                .claims
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            _ => None,
        };
        let mut request = Request::certified(
            self.genesis.id()?,
            id,
            operation,
            (&self.key, certificate),
            now()?,
        )?;
        if let Some(claims) = claims {
            request = request.with_claims(claims, &self.key)?;
        }
        timeout(Duration::from_secs(12), async {
            for _ in 0..3 {
                for response in self.attempt_leader_first(&request).await {
                    match response {
                        Ok(reply) if reply.result != Err(CoordinationError::Unavailable) => {
                            return reply.result;
                        }
                        _ => continue,
                    }
                }
                sleep(Duration::from_millis(100)).await;
            }
            Err(CoordinationError::Unavailable)
        })
        .await
        .map_err(|_| CoordinationError::Unavailable)?
    }
    pub async fn coordinated(
        &self,
        log: TipLog,
        action: crate::coordinated::Action,
    ) -> Result<Outcome, CoordinationError> {
        use tracing::Instrument;
        let span = tracing::debug_span!(
            "coordinated",
            action = action.kind(),
            outcome = tracing::field::Empty
        );
        let result = self
            .call(
                crate::transport::nonce(),
                Operation::Coordinated { log, action },
            )
            .instrument(span.clone())
            .await;
        span.record(
            "outcome",
            match &result {
                Ok(_) => "ok",
                Err(error) => error.category(),
            },
        );
        result
    }
    fn terminal(
        &self,
        log: TipLog,
        action: crate::coordinated::Action,
    ) -> CoordinationFuture<'_, LogResult> {
        Box::pin(async move {
            match self.coordinated(log, action).await? {
                Outcome::Coordinated(crate::coordinated::Result::Committed {
                    position,
                    running_hash,
                }) => Ok(LogResult::Committed {
                    position,
                    running_hash,
                }),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    pub async fn coordinated_head(
        &self,
        log: TipLog,
    ) -> Result<crate::coordinated::Head, CoordinationError> {
        match self
            .coordinated(log, crate::coordinated::Action::Head)
            .await?
        {
            Outcome::CoordinatedHead(head) => Ok(*head),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    pub async fn configure(
        &self,
        scope: Scope,
        minimum: usize,
        id: Commitment,
    ) -> Result<(), CoordinationError> {
        match self
            .call(id, Operation::Configure { scope, minimum })
            .await?
        {
            Outcome::Coordinated(crate::coordinated::Result::Configured) => Ok(()),
            _ => Err(CoordinationError::Corrupt),
        }
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
        match self.call(id, Operation::Control(certificate)).await? {
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
            .ok_or(CoordinationError::Denied)
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
impl DomainLog for NetworkCoordinator {
    fn pending_status(
        &self,
        log: TipLog,
        position: u64,
        hash: Commitment,
    ) -> CoordinationFuture<'_, LogStatus> {
        Box::pin(async move {
            match self
                .coordinated(log, crate::coordinated::Action::Status { position, hash })
                .await?
            {
                Outcome::CoordinatedStatus(status) => Ok(status),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn node(&self) -> NodeKey {
        *self.key.public_key().as_bytes()
    }
    fn identity(&self) -> CoordinatorIdentity {
        self.identity
    }
    fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead> {
        Box::pin(async move { Ok(self.coordinated_head(log).await?.port()) })
    }
    fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()> {
        Box::pin(async move {
            match self
                .coordinated(log, crate::coordinated::Action::Configure { minimum })
                .await?
            {
                Outcome::Coordinated(crate::coordinated::Result::Configured) => Ok(()),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn join(
        &self,
        log: TipLog,
        position: u64,
        running_hash: Commitment,
        sequence: u64,
    ) -> CoordinationFuture<'_, u64> {
        Box::pin(async move {
            let action = crate::coordinated::Action::Join {
                position,
                running_hash,
                sequence,
            };
            match self.coordinated(log, action).await? {
                Outcome::Coordinated(crate::coordinated::Result::Joined { epoch }) => Ok(epoch),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn append(&self, log: TipLog, append: Append) -> CoordinationFuture<'_, LogPending> {
        Box::pin(async move {
            let action = crate::coordinated::Action::Append {
                expected: append.expected,
                epoch: append.epoch,
                hash: append.hash,
                duration: append.duration,
                sequence: append.sequence,
            };
            match self.coordinated(log, action).await? {
                Outcome::Coordinated(crate::coordinated::Result::Assigned(pending)) => {
                    Ok(pending.port())
                }
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn acknowledge(&self, log: TipLog, pending: LogPending) -> CoordinationFuture<'_, LogResult> {
        Box::pin(async move {
            let pending = crate::coordinated::Pending::from_port(&pending)?;
            match self
                .coordinated(log, crate::coordinated::Action::Acknowledge { pending })
                .await?
            {
                Outcome::Coordinated(crate::coordinated::Result::Acknowledged) => {
                    Ok(LogResult::Acknowledged)
                }
                Outcome::Coordinated(crate::coordinated::Result::Committed {
                    position,
                    running_hash,
                }) => Ok(LogResult::Committed {
                    position,
                    running_hash,
                }),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn seal(&self, log: TipLog, terminal: LogTerminal) -> CoordinationFuture<'_, LogResult> {
        self.terminal(log, crate::coordinated::Action::Seal { terminal })
    }
    fn recover(&self, log: TipLog, seal: Vec<u8>) -> CoordinationFuture<'_, LogResult> {
        self.terminal(log, crate::coordinated::Action::Recover { seal })
    }
    fn acknowledge_terminal(
        &self,
        log: TipLog,
        acknowledgment: Vec<u8>,
    ) -> CoordinationFuture<'_, ()> {
        Box::pin(async move {
            let action = crate::coordinated::Action::AcknowledgeTerminal { acknowledgment };
            match self.coordinated(log, action).await? {
                Outcome::Coordinated(crate::coordinated::Result::TerminalAcknowledged) => Ok(()),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
}
#[cfg(test)]
mod tests;

impl rocky_credentials::Coordinator for NetworkCoordinator {
    fn bundle(&self) -> Result<rocky_credentials::CoordinatorBundle, CoordinationError> {
        Ok(rocky_credentials::CoordinatorBundle {
            identity: self.identity,
            certified: self.genesis.to_bytes()?,
            url: None,
        })
    }
    fn bind(
        &self,
        key: PrivateKey,
        certificate: SignedCapability,
    ) -> Result<Arc<dyn rocky_credentials::Coordinator>, CoordinationError> {
        if certificate.capability.subject != key.public_key() {
            return Err(CoordinationError::Denied);
        }
        let client = Self::new(
            self.genesis.clone(),
            self.genesis.root,
            self.genesis.genesis.mesh,
            key,
            self.public_client_lane.clone(),
        )?;
        client.certify(certificate);
        Ok(Arc::new(client))
    }
    fn certify(&self, certificate: SignedCapability) {
        self.certify(certificate);
    }
    fn observe(&self, observer: Observer) {
        self.observe(observer);
    }
    fn present_claims(&self, claims: Option<CredentialChain>) {
        self.present_claims(claims);
    }
    fn claim(
        &self,
        scope: Scope,
        counter: crate::counters::Name,
        claim: crate::counters::Claim,
    ) -> CoordinationFuture<'_, u64> {
        Box::pin(self.claim(scope, counter, claim))
    }
    fn counter(
        &self,
        scope: Scope,
        counter: crate::counters::Name,
        claim: Option<crate::counters::Claim>,
    ) -> CoordinationFuture<'_, crate::counters::Reading> {
        Box::pin(self.counter(scope, counter, claim))
    }
    fn revoke(
        &self,
        scope: Scope,
        revocations: RevocationSet,
    ) -> CoordinationFuture<'_, Commitment> {
        Box::pin(self.revoke(scope, revocations))
    }
}
