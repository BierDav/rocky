use crate::{Command, Error, Replica};
use rocky_core::*;
use tokio::sync::oneshot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedPublication {
    pub package: StrongPackage,
    pub receipt: StrongReceipt,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandState {
    Queued,
    OutcomeUnknown,
    ConfirmedLocally,
    PeerApplied,
    /// Released from holding checkpoints at its cut. This replica no longer
    /// executes it; a replica that kept the cut still may.
    Released,
    /// The coordinator retired its identity: it never executes again, and
    /// whether it was confirmed is unavailable there.
    Retired,
    /// Its admission window closed unconfirmed: it was never executed.
    Expired,
}
/// What the coordinator settled about a request it will never execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Settled {
    Retired = 2,
    Expired = 3,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandProgress {
    pub request: StrongRequest,
    pub state: CommandState,
    pub peer_received: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrongStatus {
    pub scope: Scope,
    pub configuration: Option<Configuration>,
    pub sequence: u64,
    pub head: Option<Commitment>,
    pub commands: Vec<CommandProgress>,
    pub received: Vec<(Commitment, bool)>,
    pub verification: Vec<(Commitment, VerificationState)>,
    pub peer_acknowledgments: Vec<(NodeId, Commitment, bool)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationState {
    Pending,
    Verified,
    Rejected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandHandle(pub Commitment);
pub struct CommandReceipt {
    handle: CommandHandle,
    result: oneshot::Receiver<Result<Response, Error>>,
}
impl CommandReceipt {
    pub fn handle(&self) -> CommandHandle {
        self.handle
    }
    pub async fn committed(self) -> Result<StrongRequest, Error> {
        match self.result.await.map_err(|_| Error::Closed)?? {
            Response::Request(request) => Ok(request),
            _ => Err(Error::Corrupt("command receipt".into())),
        }
    }
}
#[derive(Clone, Debug)]
pub struct SealedInputs {
    pub frontier: AppliedFrontier,
    pub digest: Commitment,
}
#[derive(Clone, Debug)]
pub struct CommandInvocation {
    pub method: String,
    pub version: u32,
    pub arguments: Vec<u8>,
    pub policy: Commitment,
    /// None seals the current applied local cut on durable acceptance.
    pub inputs: Option<SealedInputs>,
}
pub(crate) enum Request {
    Publish(CommandHandle, CommandInvocation),
    LookupCommand(CommandHandle),
    StoreCommand(Vec<u8>),
    Status,
    /// Up to this many unreleased requests without an applied result, the
    /// ones `execute_pending` works on, without decoding the ledger.
    Unresolved(usize),
    /// What reconciliation inside this process needs of the ledger.
    Ledger,
    Bind(StrongHead),
    Prepare(Box<StrongRequest>, StrongHead),
    Attempts(RequestId),
    /// Package, receipt and what the coordinator said about the receipt.
    StoreResult(Vec<u8>, Vec<u8>, Verdict),
    Verify(Commitment),
    Reject(Commitment),
    Apply,
    Acknowledge(NodeId, Commitment, bool),
    Results,
    Publications,
    ResultPage(Option<Commitment>),
    RequestsPage(Option<RequestId>),
    Release(RequestId),
    /// Records what the coordinator settled; the request is never executed.
    Settle(RequestId, Settled),
    /// The highest own sequence through which every request is settled.
    Acknowledgeable,
}
impl Request {
    /// Steps that change the strong ledger; the others only read it.
    pub(crate) fn writes(&self) -> bool {
        matches!(
            self,
            Self::Publish(..)
                | Self::StoreCommand(_)
                | Self::Bind(_)
                | Self::Prepare(..)
                | Self::StoreResult(..)
                | Self::Verify(_)
                | Self::Reject(_)
                | Self::Apply
                | Self::Acknowledge(..)
                | Self::Release(_)
                | Self::Settle(..)
        )
    }
    /// Replies that reach the coordinator or peers: published requests and
    /// their sealed frontier, packages and the confirmation sent after
    /// `Prepare`, and every page or list of requests and results. Storing what
    /// others sent, verification, application and peer acknowledgments stay
    /// local until one of these exposes them. `Ledger` is not an exposure:
    /// reconciliation keeps what it reads, except receipts it shows to the
    /// coordinator that issued them.
    pub(crate) fn exposes(&self) -> bool {
        matches!(
            self,
            Self::Publish(..)
                | Self::LookupCommand(_)
                | Self::Status
                | Self::Unresolved(_)
                | Self::Prepare(..)
                | Self::Attempts(_)
                | Self::Results
                | Self::Publications
                | Self::ResultPage(_)
                | Self::RequestsPage(_)
        )
    }
}
/// The coordinator's answer for a receipt, obtained before the result is
/// stored so that storing, marking and applying share one commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Not asked or unreachable: store the receipt and verify it later.
    Deferred,
    /// Store, mark verified and apply what is ready.
    Verified,
    /// Store as rejection evidence only.
    Rejected,
}
pub(crate) enum Response {
    Unit,
    Request(StrongRequest),
    OptionalRequest(Option<StrongRequest>),
    Status(StrongStatus),
    Requests(Vec<StrongRequest>),
    /// Whether the domain has a coordinator binding or requests, and the
    /// received results that are not verified yet.
    Ledger {
        bound: bool,
        unverified: Vec<ConfirmedPublication>,
    },
    Package(StrongPackage),
    Packages(Vec<StrongPackage>),
    Results(Vec<ConfirmedPublication>),
    ResultPage(crate::Page<ConfirmedPublication, Commitment>),
    RequestsPage(crate::Page<StrongRequest, RequestId>),
    Acknowledgeable {
        node: NodeId,
        through: u64,
    },
}
impl<A> Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    pub(crate) async fn strong_request(&self, request: Request) -> Result<Response, Error> {
        let (reply, result) = oneshot::channel();
        self.inner
            .commands
            .send(Command::Strong(Box::new(request), reply))
            .await?;
        result.await.map_err(|_| Error::Closed)?
    }
    pub async fn request_command(&self, method: &str) -> Result<StrongRequest, Error> {
        let next = self
            .inner
            .submissions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut bytes = self.inner.nonce.to_vec();
        bytes.extend(next.to_le_bytes());
        self.submit_command(
            CommandHandle(commitment("rocky.command.handle", &bytes)),
            method,
        )?
        .committed()
        .await
    }
    pub fn submit_command(
        &self,
        handle: CommandHandle,
        method: &str,
    ) -> Result<CommandReceipt, Error> {
        self.submit_command_with_arguments(handle, method, Vec::new(), [0; 32])
    }
    pub fn submit_command_with_arguments(
        &self,
        handle: CommandHandle,
        method: &str,
        arguments: Vec<u8>,
        policy: Commitment,
    ) -> Result<CommandReceipt, Error> {
        self.invoke_command(
            handle,
            CommandInvocation {
                method: method.into(),
                version: 1,
                arguments,
                policy,
                inputs: None,
            },
        )
    }
    pub fn invoke_command(
        &self,
        handle: CommandHandle,
        invocation: CommandInvocation,
    ) -> Result<CommandReceipt, Error> {
        let (reply, result) = oneshot::channel();
        self.inner.commands.try_send(Command::Strong(
            Box::new(Request::Publish(handle, invocation)),
            reply,
        ))?;
        Ok(CommandReceipt { handle, result })
    }
    pub async fn lookup_command(
        &self,
        handle: CommandHandle,
    ) -> Result<Option<StrongRequest>, Error> {
        match self.strong_request(Request::LookupCommand(handle)).await? {
            Response::OptionalRequest(request) => Ok(request),
            _ => Err(Error::Corrupt("command lookup".into())),
        }
    }
    pub async fn receive_request(&self, bytes: Vec<u8>) -> Result<(), Error> {
        self.strong_request(Request::StoreCommand(bytes))
            .await
            .map(|_| ())
    }
    /// Stops an unresolved request from holding checkpoints at its sealed
    /// cut. Once a checkpoint passes the cut this replica cannot execute it or
    /// verify its result; see `docs/runtime/checkpoints.md`. Refused while a
    /// submitted attempt has an unknown outcome.
    pub async fn release_command(&self, id: RequestId) -> Result<(), Error> {
        self.strong_request(Request::Release(id)).await.map(|_| ())
    }
    pub async fn strong_status(&self) -> Result<StrongStatus, Error> {
        match self.strong_request(Request::Status).await? {
            Response::Status(s) => Ok(s),
            _ => Err(Error::Corrupt("strong status response".into())),
        }
    }
    /// Explicit history query, separate from the current row snapshot.
    pub async fn confirmed_publications(&self) -> Result<Vec<ConfirmedPublication>, Error> {
        match self.strong_request(Request::Publications).await? {
            Response::Results(results) => Ok(results),
            _ => Err(Error::Corrupt("publication response".into())),
        }
    }
    pub async fn result_page(
        &self,
        after: Option<Commitment>,
    ) -> Result<crate::Page<ConfirmedPublication, Commitment>, Error> {
        match self.strong_request(Request::ResultPage(after)).await? {
            Response::ResultPage(page) => Ok(page),
            _ => Err(Error::Corrupt("result page".into())),
        }
    }
    pub async fn request_page(
        &self,
        after: Option<RequestId>,
    ) -> Result<crate::Page<StrongRequest, RequestId>, Error> {
        match self.strong_request(Request::RequestsPage(after)).await? {
            Response::RequestsPage(page) => Ok(page),
            _ => Err(Error::Corrupt("request page".into())),
        }
    }
    pub async fn strong_results(&self) -> Result<Vec<ConfirmedPublication>, Error> {
        match self.strong_request(Request::Results).await? {
            Response::Results(p) => Ok(p),
            _ => Err(Error::Corrupt("strong result response".into())),
        }
    }
    pub async fn acknowledge_result(
        &self,
        peer: NodeId,
        result: Commitment,
        applied: bool,
    ) -> Result<(), Error> {
        self.strong_request(Request::Acknowledge(peer, result, applied))
            .await
            .map(|_| ())
    }
    /// Verifies the receipt with the coordinator, then stores the result,
    /// records the verdict and applies what is ready in one commit. A receipt
    /// the coordinator could not be asked about is stored unverified; reopen
    /// and `reconcile_strong` resume from the retained package and receipt.
    pub async fn receive_result(
        &self,
        package: Vec<u8>,
        receipt: Vec<u8>,
        coordinator: &dyn Coordinator,
    ) -> Result<(), Error> {
        let _verification = self.inner.verification.lock().await;
        // An undecodable receipt is stored as rejection evidence by the step.
        let (verdict, refusal) = match StrongReceipt::decode(&receipt) {
            Ok(decoded) => match coordinator.verify(&decoded).await {
                Ok(()) => (Verdict::Verified, None),
                // A retired identity without retained evidence stays unverified.
                Err(CoordinationError::Unavailable | CoordinationError::Retired) => {
                    (Verdict::Deferred, None)
                }
                Err(error) => (Verdict::Rejected, Some(error)),
            },
            Err(_) => (Verdict::Deferred, None),
        };
        self.strong_request(Request::StoreResult(package, receipt, verdict))
            .await?;
        if let Some(error) = refusal {
            return Err(error.into());
        }
        self.reconcile_locked(coordinator).await
    }
    pub async fn reconcile_strong(&self, coordinator: &dyn Coordinator) -> Result<(), Error> {
        let _verification = self.inner.verification.lock().await;
        self.reconcile_locked(coordinator).await
    }
    async fn reconcile_locked(&self, coordinator: &dyn Coordinator) -> Result<(), Error> {
        let (bound, unverified) = match self.strong_request(Request::Ledger).await? {
            Response::Ledger { bound, unverified } => (bound, unverified),
            _ => return Err(Error::Corrupt("strong ledger response".into())),
        };
        if bound {
            match coordinator.head(self.inner.scope).await {
                Ok(head) => {
                    self.strong_request(Request::Bind(head)).await?;
                }
                Err(CoordinationError::Unavailable) => {}
                Err(e) => return Err(e.into()),
            }
        }
        // A validated receipt remains usable during later coordinator outages,
        // so only received results that are not verified yet are asked about.
        for result in unverified {
            let hash = result.receipt.confirmation.result;
            // Verify is intentionally external even for an already applied result;
            // unavailable validation is harmless after trusted evidence was stored.
            match coordinator.verify(&result.receipt).await {
                Ok(()) => {
                    self.strong_request(Request::Verify(hash)).await?;
                }
                Err(CoordinationError::Unavailable | CoordinationError::Retired) => {}
                Err(CoordinationError::InvalidCommitment) => {
                    self.strong_request(Request::Reject(hash)).await?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        self.strong_request(Request::Apply).await?;
        self.acknowledge_settled(coordinator).await
    }
    /// Tells the coordinator that every own request through the settled
    /// prefix is resolved here, so it may retire those identities.
    async fn acknowledge_settled(&self, coordinator: &dyn Coordinator) -> Result<(), Error> {
        let (node, through) = match self.strong_request(Request::Acknowledgeable).await? {
            Response::Acknowledgeable { node, through } => (node, through),
            _ => return Err(Error::Corrupt("acknowledgment response".into())),
        };
        let acknowledged = &self.inner.acknowledged;
        if through <= acknowledged.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        self.flush().await?;
        match coordinator.retire(self.inner.scope, node, through).await {
            Ok(()) => {
                acknowledged.fetch_max(through, std::sync::atomic::Ordering::AcqRel);
                Ok(())
            }
            Err(CoordinationError::Unavailable) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    /// One bounded executor pass. Reconciliation always precedes new attempts.
    /// The caller retries pending work after transport/coordinator recovery.
    pub async fn execute_pending(&self, coordinator: &dyn Coordinator) -> Result<(), Error> {
        let _execution = self.inner.strong_execution.lock().await;
        self.reconcile_strong(coordinator).await?;
        let requests = match self.strong_request(Request::Unresolved(32)).await? {
            Response::Requests(requests) => requests,
            _ => return Err(Error::Corrupt("unresolved requests response".into())),
        };
        for request in requests {
            let established = match coordinator.lookup(request.scope, request.id).await {
                Ok(established) => established,
                // Never prepared again, and never replaced by a new identity.
                Err(CoordinationError::Retired) => {
                    self.settle(request.id, Settled::Retired).await?;
                    continue;
                }
                Err(CoordinationError::Expired) => {
                    self.settle(request.id, Settled::Expired).await?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let attempts = match self.strong_request(Request::Attempts(request.id)).await? {
                Response::Packages(p) => p,
                _ => return Err(Error::Corrupt("attempt response".into())),
            };
            if let Some(receipt) = established {
                let package = attempts
                    .into_iter()
                    .find(|p| p.confirmation().is_ok_and(|c| c == receipt.confirmation));
                let Some(package) = package else {
                    return Err(Error::StrongPending(
                        crate::PendingReason::MissingConfirmedPackage,
                    ));
                };
                self.receive_result(package.encode()?, receipt.encode(), coordinator)
                    .await?;
                continue;
            }
            let id = request.id;
            let head = coordinator.head(request.scope).await?;
            // Binds the head and records the attempt as outcome unknown.
            let package = match self
                .strong_request(Request::Prepare(Box::new(request), head))
                .await?
            {
                Response::Package(p) => p,
                _ => return Err(Error::Corrupt("preparation response".into())),
            };
            match coordinator.confirm(package.confirmation()?).await {
                Ok(receipt) => {
                    self.receive_result(package.encode()?, receipt.encode(), coordinator)
                        .await?
                }
                Err(CoordinationError::StaleHead | CoordinationError::StaleEpoch) => {
                    return Err(Error::StrongPending(crate::PendingReason::StaleFence));
                }
                Err(CoordinationError::Retired) => self.settle(id, Settled::Retired).await?,
                Err(CoordinationError::Expired) => self.settle(id, Settled::Expired).await?,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    async fn settle(&self, id: RequestId, settled: Settled) -> Result<(), Error> {
        self.strong_request(Request::Settle(id, settled)).await?;
        self.flush().await
    }
}
