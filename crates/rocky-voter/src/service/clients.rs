use super::*;
use tokio::sync::OwnedSemaphorePermit;

const PRIVATE_SERVICE_CLIENT_LANES: usize = 16;

const UNADMITTED_CLIENT_STREAMS_PER_CLIENT_LANE: usize = 16;

const ADMINISTRATIVE_LANES: usize = 2;

const CLIENT_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

const CLIENT_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub(super) struct Admission {
    unadmitted_client_streams: Arc<Semaphore>,
    client_lanes: Semaphore,
    tenant_lanes: std::sync::Mutex<HashMap<Option<PublicKey>, Arc<Semaphore>>>,
    administrative_lanes: Semaphore,
}
pub(super) struct Unadmitted {
    _slot: OwnedSemaphorePermit,
    deadline: tokio::time::Instant,
}
impl Admission {
    pub(super) fn new(genesis: &Genesis) -> Self {
        let client_lanes = match &genesis.public {
            Some(_) => CLIENT_LANES as usize,
            None => PRIVATE_SERVICE_CLIENT_LANES,
        };
        Self {
            unadmitted_client_streams: Arc::new(Semaphore::new(
                client_lanes * UNADMITTED_CLIENT_STREAMS_PER_CLIENT_LANE,
            )),
            client_lanes: Semaphore::new(client_lanes),
            tenant_lanes: Default::default(),
            administrative_lanes: Semaphore::new(ADMINISTRATIVE_LANES),
        }
    }
}

#[derive(Default)]
pub(crate) struct Faults {
    lose_next_claim_reply: AtomicBool,
    unanswered: std::sync::Mutex<Option<Commitment>>,
}
impl Faults {
    pub(crate) fn lose_next_claim_reply(&self) {
        self.lose_next_claim_reply.store(true, Ordering::SeqCst);
    }
    fn loses(&self, request: Commitment, committed_claim: bool) -> bool {
        let mut unanswered = self
            .unanswered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *unanswered == Some(request) {
            return true;
        }
        if committed_claim && self.lose_next_claim_reply.swap(false, Ordering::SeqCst) {
            *unanswered = Some(request);
            return true;
        }
        false
    }
}

impl VoterService {
    pub async fn serve_clients(self: Arc<Self>, transport: Arc<dyn Transport>) {
        self.serve_clients_with(transport, None).await;
    }
    pub(crate) async fn serve_clients_with(
        self: Arc<Self>,
        transport: Arc<dyn Transport>,
        faults: Option<Arc<Faults>>,
    ) {
        if transport.listen(None, true).is_err() {
            return;
        }
        log_lifecycle(self.node, "serving clients");
        while let Some(incoming) = transport.accept().await {
            let rocky_core::net::Incoming::Stream(Protocol::Coordinate, stream) = incoming else {
                continue;
            };
            let Some(unadmitted) = self
                .counters
                .connection_slot(&self.admission.unadmitted_client_streams)
            else {
                continue;
            };
            tokio::spawn(
                self.clone()
                    .serve_client(stream, unadmitted, faults.clone()),
            );
        }
        log_lifecycle(self.node, "client transport closed");
    }
    async fn serve_client(
        self: Arc<Self>,
        mut stream: rocky_core::net::Stream,
        slot: OwnedSemaphorePermit,
        faults: Option<Arc<Faults>>,
    ) {
        let unadmitted = Unadmitted {
            _slot: slot,
            deadline: tokio::time::Instant::now() + CLIENT_ADMISSION_TIMEOUT,
        };
        let _ = tokio::time::timeout(CLIENT_REQUEST_TIMEOUT, async {
            let genesis = self.genesis.id()?;
            let envelope = tokio::time::timeout_at(
                unadmitted.deadline,
                read_bounded(&mut stream, MAX_CLIENT_FRAME),
            )
            .await
            .map_err(|_| CoordinationError::Unavailable)??;
            let reply = match envelope.client_request(genesis) {
                Ok(request) => {
                    let (id, claim) = (
                        request.id,
                        matches!(request.operation, Operation::Claim { .. }),
                    );
                    let reply = self.admit(request, Some(unadmitted)).await;
                    if faults
                        .as_ref()
                        .is_some_and(|faults| faults.loses(id, claim && reply.result.is_ok()))
                    {
                        return Ok(());
                    }
                    reply
                }
                Err(error) => self.refuse_unadmitted(error),
            };
            let response = crate::transport::Envelope::from_body(
                genesis,
                envelope.nonce,
                encode(&reply)?,
                &self.key,
            )?;
            write(&mut stream, &response).await
        })
        .await;
    }
    pub(super) async fn admit(&self, request: Request, unadmitted: Option<Unadmitted>) -> Reply {
        if self.fenced.load(Ordering::SeqCst) {
            return self.refuse(CoordinationError::Unavailable);
        }
        let resolving = self.tenant_lane(&request);
        let resolved = match &unadmitted {
            Some(unadmitted) => tokio::time::timeout_at(unadmitted.deadline, resolving).await,
            None => Ok(resolving.await),
        };
        let lane = match resolved {
            Ok(Ok(lane)) => lane,
            Ok(Err(error)) => return self.refuse(error),
            Err(_) => {
                self.counters.busy.fetch_add(1, Ordering::Relaxed);
                return self.refuse(CoordinationError::Backpressure);
            }
        };
        let tenant = lane.as_ref().map(|lane| lane.try_acquire()).transpose();
        let (Ok(_client_lane), Ok(_tenant_lane)) =
            (self.admission.client_lanes.try_acquire(), tenant)
        else {
            self.counters.busy.fetch_add(1, Ordering::Relaxed);
            return self.refuse(CoordinationError::Backpressure);
        };
        drop(unadmitted);
        self.answer(request).await
    }
    pub(super) async fn administer(&self, request: Request) -> Reply {
        if self.fenced.load(Ordering::SeqCst) {
            return self.refuse(CoordinationError::Unavailable);
        }
        let Ok(_administrative_lane) = self.admission.administrative_lanes.try_acquire() else {
            self.counters.busy.fetch_add(1, Ordering::Relaxed);
            return self.refuse(CoordinationError::Backpressure);
        };
        self.answer(request).await
    }
    async fn tenant_lane(
        &self,
        request: &Request,
    ) -> Result<Option<Arc<Semaphore>>, CoordinationError> {
        let Some(public) = &self.genesis.genesis.public else {
            return Ok(None);
        };
        let tenant = self.store.lane(request).await;
        let mut lanes = self
            .admission
            .tenant_lanes
            .lock()
            .map_err(|_| CoordinationError::Corrupt)?;
        if lanes.len() > public.tenants as usize {
            let permits_per_lane = public.tenant_lanes as usize;
            lanes.retain(|_, lane| lane.available_permits() < permits_per_lane);
        }
        Ok(Some(
            lanes
                .entry(tenant)
                .or_insert_with(|| Arc::new(Semaphore::new(public.tenant_lanes as usize)))
                .clone(),
        ))
    }
}
#[cfg(test)]
mod tests;
