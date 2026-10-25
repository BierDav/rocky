use crate::{
    certified::*,
    machine::Request,
    service::{Operation, Outcome, Registration, Reply, Status},
    transport::*,
};
use p2panda_core::{PrivateKey, PublicKey};
use rocky_core::{
    net::{Protocol, Transport},
    *,
};
use rocky_credentials::{SignedCapability, SignedCoordinationGrant, SignedTenantAdmission};
use std::sync::{Arc, Mutex, PoisonError};
pub struct NetworkCoordinator {
    genesis: CertifiedGenesis,
    key: PrivateKey,
    /// The node certificate requests present, which a trusted identity
    /// signed; replaced when it is renewed.
    certificate: Mutex<Option<SignedCapability>>,
    /// The client lane of a public service; voter TCP when absent.
    route: Option<Arc<dyn Transport>>,
}
impl NetworkCoordinator {
    pub fn open(
        genesis: CertifiedGenesis,
        root: PublicKey,
        mesh: MeshId,
        key: PrivateKey,
    ) -> Result<Self, CoordinationError> {
        genesis.verify(root, mesh)?;
        Ok(Self {
            genesis,
            key,
            certificate: Mutex::new(None),
            route: None,
        })
    }
    /// Presents `certificate`, this node's under a trusted identity, with
    /// every later request.
    pub fn certify(&self, certificate: SignedCapability) {
        *self
            .certificate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(certificate);
    }
    /// A client of the public service `bundle` pins, reached through
    /// `transport` at the certified client endpoints of its kind. Requests
    /// are signed with `key`; the transport stays its owner's.
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
        let mut client = Self::open(genesis, bundle.root, bundle.mesh, key)?;
        client.route = Some(transport);
        Ok(client)
    }
    /// One attempt at each reachable voter, in certified order.
    async fn attempt(&self, request: &Request) -> Vec<Result<Reply, CoordinationError>> {
        let genesis = match self.genesis.id() {
            Ok(id) => id,
            Err(error) => return vec![Err(error)],
        };
        let message = || Message::Client(Box::new(request.clone()));
        let mut replies = Vec::new();
        match (&self.route, &self.genesis.genesis.public) {
            (Some(transport), Some(public)) => {
                for endpoint in public
                    .endpoints
                    .iter()
                    .filter(|e| e.kind == transport.kind())
                {
                    let Ok(voter) = self.voter(endpoint.voter) else {
                        continue;
                    };
                    let reply = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                        let mut stream = transport
                            .connect(&endpoint.address, Protocol::Coordinate)
                            .await
                            .map_err(|_| CoordinationError::Unavailable)?;
                        converse(&mut stream, voter.key, genesis, &self.key, message()).await
                    })
                    .await
                    .unwrap_or(Err(CoordinationError::Unavailable));
                    if reply.is_err() {
                        transport.forget(&endpoint.address);
                    }
                    let done = reply
                        .as_ref()
                        .is_ok_and(|r: &Reply| r.result != Err(CoordinationError::Unavailable));
                    replies.push(reply);
                    if done {
                        break;
                    }
                }
            }
            _ => {
                for voter in &self.genesis.genesis.voters {
                    let reply = exchange(voter, genesis, &self.key, message()).await;
                    let done = reply
                        .as_ref()
                        .is_ok_and(|r: &Reply| r.result != Err(CoordinationError::Unavailable));
                    replies.push(reply);
                    if done {
                        break;
                    }
                }
            }
        }
        replies
    }
    pub async fn call(
        &self,
        id: Commitment,
        operation: Operation,
    ) -> Result<Outcome, CoordinationError> {
        let certificate = self
            .certificate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let request = Request::certified(
            self.genesis.id()?,
            id,
            operation,
            (&self.key, certificate),
            now()?,
        )?;
        tokio::time::timeout(std::time::Duration::from_secs(12), async {
            for _ in 0..3 {
                for response in self.attempt(&request).await {
                    match response {
                        Ok(reply) if reply.result != Err(CoordinationError::Unavailable) => {
                            return reply.result;
                        }
                        _ => continue,
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(CoordinationError::Unavailable)
        })
        .await
        .map_err(|_| CoordinationError::Unavailable)?
    }
    /// Metadata-only log call. Join/append sequences must be durably increasing per log/node.
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
    /// Commits a terminal entry: a seal, or the owner's recovery.
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
    /// The authority's default minimum in-sync size for `scope`'s tips,
    /// before any node joined one.
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
    /// Registers the domain of `certificate` on a public service under this
    /// client's key, which `grant` names as the application's tenant.
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
    /// A new control revision: trusted nodes or validity. A trusted node
    /// still in the log's in-sync set cannot be removed.
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
    /// This tenant's use of its budgets.
    pub async fn usage(&self) -> Result<crate::service::Usage, CoordinationError> {
        match self.call(nonce(), Operation::Usage).await? {
            Outcome::Usage(usage) => Ok(usage),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// Owner-initiated closure of `scope`; a closed identity never reopens.
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
    pub async fn status(&self, voter: u64) -> Result<Status, CoordinationError> {
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
    pub async fn initialize(&self, voter: u64) -> Result<(), CoordinationError> {
        exchange(
            self.voter(voter)?,
            self.genesis.id()?,
            &self.key,
            Message::Initialize,
        )
        .await?
    }
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
    fn node(&self) -> NodeKey {
        *self.key.public_key().as_bytes()
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
                deadline: append.deadline,
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
